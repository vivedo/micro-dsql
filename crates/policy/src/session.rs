//! Per-connection policy state: the transaction discipline and limits.
//!
//! The server mirrors the engine's transaction status and the per-statement change count into
//! this module ([`SessionPolicy::admit`] before a statement runs, [`SessionPolicy::complete`]
//! or [`SessionPolicy::failed`] after); nothing here knows about the engine (constitution III).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::classify::{StatementClass, TxnControl};
use crate::errors::{self, PolicyError};
use crate::intercept::Intercept;
use crate::settings::{default_value, show_column, SessionSettings, SettingCommand};
use crate::statement::StatementPlan;

/// Limits of a DSQL transaction. The shipped binary uses the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyLimits {
    pub max_rows_per_txn: u64,
    pub max_txn_age: Duration,
}

impl Default for PolicyLimits {
    fn default() -> Self {
        Self {
            max_rows_per_txn: 3000,
            max_txn_age: Duration::from_secs(300),
        }
    }
}

/// The engine's transaction status, mirrored by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnStatus {
    Idle,
    Active,
    Aborted,
}

/// How the server answers a statement the policy answers itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// One row, one text column.
    Show { column: String, value: String },
    /// A command tag.
    Command { tag: &'static str },
}

/// What to do with a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission<'a> {
    /// Send this text to the engine.
    Run { sql: &'a str },
    /// Answered by micro-dsql.
    Respond(Reply),
    /// Error; the server aborts an active transaction block.
    Refuse(PolicyError),
    /// The server rolls the transaction back first (COMMIT past the age limit), then errors.
    RollbackThenRefuse(PolicyError),
}

/// State of one explicit transaction.
#[derive(Debug, Clone)]
struct TxnState {
    /// When the engine status went from idle to active.
    began_at: Instant,
    /// DDL statements that ran successfully.
    ddl_count: u32,
    /// A DML statement ran.
    saw_dml: bool,
    /// Sum of the engine change counts of the DML statements.
    rows_changed: u64,
    /// `SET LOCAL` values; dropped with the transaction.
    local: HashMap<&'static str, String>,
}

impl TxnState {
    fn new(now: Instant) -> Self {
        Self {
            began_at: now,
            ddl_count: 0,
            saw_dml: false,
            rows_changed: 0,
            local: HashMap::new(),
        }
    }
}

/// Policy state of one connection.
#[derive(Debug)]
pub struct SessionPolicy {
    limits: PolicyLimits,
    settings: SessionSettings,
    txn: Option<TxnState>,
}

impl SessionPolicy {
    pub fn new(limits: PolicyLimits) -> Self {
        Self {
            limits,
            settings: SessionSettings::default(),
            txn: None,
        }
    }

    /// Decides what to do with a statement before it runs.
    pub fn admit<'p>(
        &mut self,
        plan: &'p StatementPlan,
        status: TxnStatus,
        now: Instant,
    ) -> Admission<'p> {
        let run = || Admission::Run {
            sql: &plan.engine_sql,
        };
        // The engine answers 25P02 (or ends the block); no rule applies.
        if status == TxnStatus::Aborted || plan.parse_failed {
            return run();
        }
        if status == TxnStatus::Active {
            if let Some(txn) = &self.txn {
                if now.saturating_duration_since(txn.began_at) > self.limits.max_txn_age {
                    let error = errors::age_limit(self.limits.max_txn_age.as_secs());
                    return match plan.class {
                        StatementClass::Txn(TxnControl::Commit) => {
                            Admission::RollbackThenRefuse(error)
                        }
                        // The way out of an expired transaction is never refused.
                        StatementClass::Txn(TxnControl::Rollback) => run(),
                        _ => Admission::Refuse(error),
                    };
                }
            }
        }
        if let Some(error) = &plan.refusal {
            return Admission::Refuse(error.clone());
        }
        if let StatementClass::Txn(control) = plan.class {
            match control {
                TxnControl::Savepoint => return Admission::Refuse(errors::savepoint()),
                TxnControl::Release => return Admission::Refuse(errors::release_savepoint()),
                TxnControl::RollbackTo => return Admission::Refuse(errors::rollback_to_savepoint()),
                _ => {}
            }
        }
        if status == TxnStatus::Active {
            if let Some(txn) = &self.txn {
                if let Some(error) = discipline(plan.class, txn.ddl_count, txn.saw_dml) {
                    return Admission::Refuse(error);
                }
            }
        } else if plan
            .values_rows
            .is_some_and(|rows| rows > self.limits.max_rows_per_txn)
        {
            return Admission::Refuse(errors::row_limit());
        }
        if let Some(setting) = &plan.setting {
            if let Some(admission) = self.admit_setting(setting, status) {
                return admission;
            }
        }
        match &plan.intercept {
            Some(Intercept::Show { column, value }) => Admission::Respond(Reply::Show {
                column: (*column).to_owned(),
                value: (*value).to_owned(),
            }),
            Some(Intercept::SetNoop { tag }) => Admission::Respond(Reply::Command { tag }),
            Some(Intercept::Deallocate { name }) => Admission::Respond(Reply::Command {
                tag: if name.is_some() {
                    "DEALLOCATE"
                } else {
                    "DEALLOCATE ALL"
                },
            }),
            Some(Intercept::Error(error)) => Admission::Refuse(error.clone()),
            None => Admission::Run {
                sql: &plan.engine_sql,
            },
        }
    }

    /// Settings the policy answers itself; `None` sends the statement to the engine.
    fn admit_setting<'p>(
        &mut self,
        setting: &SettingCommand,
        status: TxnStatus,
    ) -> Option<Admission<'p>> {
        let command = |tag| Some(Admission::Respond(Reply::Command { tag }));
        let local_txn = if status == TxnStatus::Active {
            self.txn.as_mut()
        } else {
            None
        };
        match setting {
            // The engine runs it; the remembered values are cleared in `complete`.
            SettingCommand::ResetAll => None,
            SettingCommand::Noop => command("SET"),
            SettingCommand::Show { name } => {
                let local = local_txn.and_then(|t| t.local.get(name).cloned());
                Some(Admission::Respond(Reply::Show {
                    column: show_column(name).to_owned(),
                    value: local.unwrap_or_else(|| self.settings.get(name)),
                }))
            }
            SettingCommand::Set {
                name,
                value,
                local: true,
            } => {
                // Outside a transaction `SET LOCAL` has no effect (PostgreSQL only warns).
                if let Some(txn) = local_txn {
                    txn.local.insert(name, value.clone());
                }
                command("SET")
            }
            SettingCommand::Set {
                name,
                value,
                local: false,
            } => {
                if let Some(txn) = local_txn {
                    txn.local.remove(name);
                }
                self.settings.set(name, value);
                command("SET")
            }
            SettingCommand::SetDefault { name, local: true } => {
                if let Some(txn) = local_txn {
                    txn.local.insert(name, default_value(name));
                }
                command("SET")
            }
            SettingCommand::SetDefault { name, local: false } => {
                if let Some(txn) = local_txn {
                    txn.local.remove(name);
                }
                self.settings.reset(name);
                command("SET")
            }
            SettingCommand::Reset { name } => {
                if let Some(txn) = local_txn {
                    txn.local.remove(name);
                }
                self.settings.reset(name);
                command("RESET")
            }
        }
    }

    /// After the engine ran a statement successfully: transaction boundaries.
    pub fn complete(
        &mut self,
        plan: &StatementPlan,
        rows_changed: u64,
        status_after: TxnStatus,
        now: Instant,
    ) -> Result<(), PolicyError> {
        if plan.setting == Some(SettingCommand::ResetAll) {
            self.settings.reset_all();
        }
        match status_after {
            TxnStatus::Idle => self.txn = None,
            TxnStatus::Active | TxnStatus::Aborted => {
                // The statement that opened the transaction (BEGIN) starts the counters.
                let Some(txn) = self.txn.as_mut() else {
                    self.txn = Some(TxnState::new(now));
                    return Ok(());
                };
                if plan.setting == Some(SettingCommand::ResetAll) {
                    txn.local.clear();
                }
                match plan.class {
                    StatementClass::Ddl => txn.ddl_count += 1,
                    StatementClass::Dml => {
                        txn.saw_dml = true;
                        // Only data-changing statements have a meaningful change count.
                        txn.rows_changed = txn.rows_changed.saturating_add(rows_changed);
                        if txn.rows_changed > self.limits.max_rows_per_txn {
                            return Err(errors::row_limit());
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// After the engine failed: transaction boundary bookkeeping only.
    pub fn failed(&mut self, status_after: TxnStatus) {
        if status_after == TxnStatus::Idle {
            self.txn = None;
        }
    }

    /// Multi-statement simple query while idle: the discipline rules over the whole string.
    pub fn check_batch(&self, plans: &[StatementPlan]) -> Result<(), PolicyError> {
        let (mut ddl_count, mut saw_dml) = (0, false);
        for plan in plans {
            match plan.class {
                StatementClass::Txn(
                    TxnControl::Begin | TxnControl::Commit | TxnControl::Rollback,
                ) => (ddl_count, saw_dml) = (0, false),
                class => {
                    if let Some(error) = discipline(class, ddl_count, saw_dml) {
                        return Err(error);
                    }
                    match class {
                        StatementClass::Ddl => ddl_count += 1,
                        StatementClass::Dml => saw_dml = true,
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether an explicit transaction is being tracked (test and diagnostics aid).
    pub fn in_transaction(&self) -> bool {
        self.txn.is_some()
    }
}

/// The one-DDL and no-DDL-with-DML rules, given what the transaction has done so far.
fn discipline(class: StatementClass, ddl_count: u32, saw_dml: bool) -> Option<PolicyError> {
    match class {
        StatementClass::Ddl if ddl_count > 0 => Some(errors::txn_two_ddl()),
        StatementClass::Ddl if saw_dml => Some(errors::txn_ddl_dml()),
        StatementClass::Dml if ddl_count > 0 => Some(errors::txn_ddl_dml()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_text<'a>(a: &Admission<'a>) -> Option<&'a str> {
        match a {
            Admission::Run { sql } => Some(sql),
            _ => None,
        }
    }

    #[test]
    fn default_limits() {
        let l = PolicyLimits::default();
        assert_eq!(l.max_rows_per_txn, 3000);
        assert_eq!(l.max_txn_age, Duration::from_secs(300));
    }

    #[test]
    fn plain_statements_run() {
        let mut s = SessionPolicy::new(PolicyLimits::default());
        let plan = StatementPlan::new("SELECT 1");
        let a = s.admit(&plan, TxnStatus::Idle, Instant::now());
        assert_eq!(run_text(&a), Some("SELECT 1"));
    }

    #[test]
    fn intercepts_are_answered_or_refused() {
        let mut s = SessionPolicy::new(PolicyLimits::default());
        let now = Instant::now();
        let plan = StatementPlan::new("SHOW server_version");
        assert!(matches!(
            s.admit(&plan, TxnStatus::Idle, now),
            Admission::Respond(Reply::Show { .. })
        ));
        let plan = StatementPlan::new("SET timezone = 'UTC'");
        assert_eq!(
            s.admit(&plan, TxnStatus::Idle, now),
            Admission::Respond(Reply::Command { tag: "SET" })
        );
        let plan = StatementPlan::new("DEALLOCATE ALL");
        assert_eq!(
            s.admit(&plan, TxnStatus::Idle, now),
            Admission::Respond(Reply::Command {
                tag: "DEALLOCATE ALL"
            })
        );
        let plan = StatementPlan::new("SET timezone = 'Europe/Rome'");
        assert!(
            matches!(s.admit(&plan, TxnStatus::Idle, now), Admission::Refuse(e) if e.sqlstate == "22023")
        );
    }

    #[test]
    fn an_aborted_block_always_reaches_the_engine() {
        let mut s = SessionPolicy::new(PolicyLimits::default());
        let now = Instant::now();
        for sql in ["SHOW server_version", "SET timezone = 'Europe/Rome'"] {
            let plan = StatementPlan::new(sql);
            assert_eq!(
                run_text(&s.admit(&plan, TxnStatus::Aborted, now)),
                Some(sql)
            );
        }
    }

    #[test]
    fn transaction_boundaries_follow_the_engine_status() {
        let mut s = SessionPolicy::new(PolicyLimits::default());
        let now = Instant::now();
        assert!(!s.in_transaction());
        let begin = StatementPlan::new("BEGIN");
        s.complete(&begin, 0, TxnStatus::Active, now).unwrap();
        assert!(s.in_transaction());
        let insert = StatementPlan::new("INSERT INTO t VALUES (1)");
        s.complete(&insert, 1, TxnStatus::Active, now).unwrap();
        assert!(s.in_transaction());
        s.complete(&StatementPlan::new("COMMIT"), 0, TxnStatus::Idle, now)
            .unwrap();
        assert!(!s.in_transaction());
        s.complete(&begin, 0, TxnStatus::Active, now).unwrap();
        s.failed(TxnStatus::Aborted);
        assert!(s.in_transaction());
        s.failed(TxnStatus::Idle);
        assert!(!s.in_transaction());
    }

    // --- discipline and limits (US2) ---

    fn plan(sql: &str) -> StatementPlan {
        StatementPlan::new(sql)
    }

    /// Runs `sql` as the next statement of an explicit transaction: admit, then complete.
    fn step(s: &mut SessionPolicy, sql: &str, rows: u64, now: Instant) -> Result<(), PolicyError> {
        let p = plan(sql);
        match s.admit(&p, TxnStatus::Active, now) {
            Admission::Run { .. } => s.complete(&p, rows, TxnStatus::Active, now),
            Admission::Refuse(e) | Admission::RollbackThenRefuse(e) => Err(e),
            Admission::Respond(_) => Ok(()),
        }
    }

    fn begun(limits: PolicyLimits, now: Instant) -> SessionPolicy {
        let mut s = SessionPolicy::new(limits);
        s.complete(&plan("BEGIN"), 0, TxnStatus::Active, now)
            .unwrap();
        s
    }

    fn refusal(r: Result<(), PolicyError>) -> &'static str {
        r.expect_err("expected a refusal").rule
    }

    #[test]
    fn txn_two_ddl() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        step(&mut s, "CREATE TABLE a (id int)", 0, now).unwrap();
        assert_eq!(
            refusal(step(&mut s, "CREATE TABLE b (id int)", 0, now)),
            "txn_two_ddl"
        );
    }

    #[test]
    fn txn_ddl_dml_both_orders() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        step(&mut s, "CREATE TABLE a (id int)", 0, now).unwrap();
        assert_eq!(
            refusal(step(&mut s, "INSERT INTO a VALUES (1)", 1, now)),
            "txn_ddl_dml"
        );
        let mut s = begun(PolicyLimits::default(), now);
        step(&mut s, "INSERT INTO a VALUES (1)", 1, now).unwrap();
        assert_eq!(
            refusal(step(&mut s, "CREATE INDEX i ON a (id)", 0, now)),
            "txn_ddl_dml"
        );
    }

    #[test]
    fn a_read_only_transaction_may_run_one_ddl() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        step(&mut s, "SELECT 1", 0, now).unwrap();
        step(&mut s, "SELECT * FROM a FOR UPDATE", 0, now).unwrap();
        step(&mut s, "CREATE TABLE a (id int)", 0, now).unwrap();
        step(&mut s, "SELECT 2", 0, now).unwrap();
        step(&mut s, "SET search_path = public", 0, now).unwrap();
    }

    #[test]
    fn counters_reset_after_the_transaction_ends() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        step(&mut s, "CREATE TABLE a (id int)", 0, now).unwrap();
        s.complete(&plan("COMMIT"), 0, TxnStatus::Idle, now)
            .unwrap();
        s.complete(&plan("BEGIN"), 0, TxnStatus::Active, now)
            .unwrap();
        step(&mut s, "CREATE TABLE b (id int)", 0, now).unwrap();
        s.complete(&plan("ROLLBACK"), 0, TxnStatus::Idle, now)
            .unwrap();
        s.complete(&plan("BEGIN"), 0, TxnStatus::Active, now)
            .unwrap();
        step(&mut s, "INSERT INTO a VALUES (1)", 1, now).unwrap();
    }

    #[test]
    fn statements_outside_a_transaction_are_not_tracked() {
        let now = Instant::now();
        let mut s = SessionPolicy::new(PolicyLimits::default());
        for sql in [
            "CREATE TABLE a (id int)",
            "INSERT INTO a VALUES (1)",
            "CREATE TABLE b (id int)",
        ] {
            let p = plan(sql);
            assert!(matches!(
                s.admit(&p, TxnStatus::Idle, now),
                Admission::Run { .. }
            ));
            s.complete(&p, 1, TxnStatus::Idle, now).unwrap();
        }
    }

    #[test]
    fn savepoints_are_refused_with_their_texts() {
        let now = Instant::now();
        let mut s = SessionPolicy::new(PolicyLimits::default());
        for (sql, text) in [
            (
                "SAVEPOINT a",
                "unsupported transaction statement: SAVEPOINT",
            ),
            ("RELEASE a", "unsupported transaction statement: RELEASE"),
            (
                "RELEASE SAVEPOINT a",
                "unsupported transaction statement: RELEASE",
            ),
            (
                "ROLLBACK TO a",
                "unsupported transaction statement: ROLLBACK TO",
            ),
            (
                "ROLLBACK TO SAVEPOINT a",
                "unsupported transaction statement: ROLLBACK TO",
            ),
        ] {
            for status in [TxnStatus::Idle, TxnStatus::Active] {
                match s.admit(&plan(sql), status, now) {
                    Admission::Refuse(e) => {
                        assert_eq!((e.sqlstate, e.message.as_str()), ("0A000", text), "{sql}")
                    }
                    other => panic!("{sql}: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn row_limit_is_cumulative_and_inclusive() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        step(&mut s, "INSERT INTO a SELECT 1", 3000, now).unwrap();
        let mut s = begun(PolicyLimits::default(), now);
        step(&mut s, "INSERT INTO a SELECT 1", 2000, now).unwrap();
        let e = step(&mut s, "UPDATE a SET v = 1", 1001, now).unwrap_err();
        assert_eq!(
            (e.sqlstate, e.message.as_str()),
            ("54000", "transaction row limit exceeded")
        );
        let mut s = begun(PolicyLimits::default(), now);
        assert_eq!(
            refusal(step(&mut s, "DELETE FROM a", 3001, now)),
            "txn_row_limit"
        );
    }

    #[test]
    fn only_data_changing_statements_count_rows() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        // A stale change count on a read must not count.
        step(&mut s, "SELECT 1", 5000, now).unwrap();
        step(&mut s, "INSERT INTO a SELECT 1", 3000, now).unwrap();
    }

    #[test]
    fn autocommit_values_over_the_limit_is_refused_before_running() {
        let now = Instant::now();
        let mut s = SessionPolicy::new(PolicyLimits::default());
        let rows = |n: usize| {
            let list = (0..n)
                .map(|i| format!("({i})"))
                .collect::<Vec<_>>()
                .join(",");
            plan(&format!("INSERT INTO t VALUES {list}"))
        };
        assert!(matches!(
            s.admit(&rows(3000), TxnStatus::Idle, now),
            Admission::Run { .. }
        ));
        assert!(
            matches!(s.admit(&rows(3001), TxnStatus::Idle, now), Admission::Refuse(e) if e.rule == "txn_row_limit")
        );
    }

    #[test]
    fn limits_are_configurable() {
        let now = Instant::now();
        let limits = PolicyLimits {
            max_rows_per_txn: 2,
            ..PolicyLimits::default()
        };
        let mut s = begun(limits, now);
        step(&mut s, "INSERT INTO a SELECT 1", 2, now).unwrap();
        assert_eq!(
            refusal(step(&mut s, "INSERT INTO a SELECT 1", 1, now)),
            "txn_row_limit"
        );
    }

    #[test]
    fn age_limit_refuses_statements_and_rolls_back_commit() {
        let t0 = Instant::now();
        let limits = PolicyLimits {
            max_txn_age: Duration::from_secs(300),
            ..PolicyLimits::default()
        };
        let mut s = begun(limits, t0);
        // Exactly at the limit is still fine.
        step(&mut s, "SELECT 1", 0, t0 + Duration::from_secs(300)).unwrap();
        let late = t0 + Duration::from_secs(301);
        match s.admit(&plan("SELECT 1"), TxnStatus::Active, late) {
            Admission::Refuse(e) => assert_eq!(
                (e.sqlstate, e.message.as_str()),
                ("54000", "transaction age limit of 300s exceeded")
            ),
            other => panic!("{other:?}"),
        }
        for sql in ["COMMIT", "END"] {
            assert!(
                matches!(s.admit(&plan(sql), TxnStatus::Active, late), Admission::RollbackThenRefuse(e) if e.rule == "txn_age_limit"),
                "{sql}"
            );
        }
        // ROLLBACK past the limit is how the client leaves; it is never refused.
        assert!(matches!(
            s.admit(&plan("ROLLBACK"), TxnStatus::Active, late),
            Admission::Run { .. }
        ));
        // An aborted block goes to the engine whatever its age.
        assert!(matches!(
            s.admit(&plan("SELECT 1"), TxnStatus::Aborted, late),
            Admission::Run { .. }
        ));
        // Idle sessions have no age.
        s.failed(TxnStatus::Idle);
        assert!(matches!(
            s.admit(&plan("SELECT 1"), TxnStatus::Idle, late),
            Admission::Run { .. }
        ));
    }

    #[test]
    fn age_message_names_the_configured_limit() {
        let t0 = Instant::now();
        let limits = PolicyLimits {
            max_txn_age: Duration::from_secs(1),
            ..PolicyLimits::default()
        };
        let mut s = begun(limits, t0);
        let e = step(&mut s, "SELECT 1", 0, t0 + Duration::from_secs(2)).unwrap_err();
        assert_eq!(e.message, "transaction age limit of 1s exceeded");
    }

    #[test]
    fn an_aborted_block_bypasses_the_rules() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        step(&mut s, "CREATE TABLE a (id int)", 0, now).unwrap();
        s.failed(TxnStatus::Aborted);
        for sql in [
            "CREATE TABLE b (id int)",
            "SAVEPOINT x",
            "INSERT INTO a VALUES (1)",
        ] {
            assert!(matches!(
                s.admit(&plan(sql), TxnStatus::Aborted, now),
                Admission::Run { .. }
            ));
        }
    }

    #[test]
    fn check_batch_applies_the_discipline_to_the_whole_string() {
        let s = SessionPolicy::new(PolicyLimits::default());
        let plans = |sqls: &[&str]| sqls.iter().map(|q| plan(q)).collect::<Vec<_>>();
        let rule = |sqls: &[&str]| s.check_batch(&plans(sqls)).unwrap_err().rule;
        assert_eq!(
            rule(&["CREATE TABLE a (id int)", "CREATE TABLE b (id int)"]),
            "txn_two_ddl"
        );
        assert_eq!(
            rule(&["CREATE TABLE a (id int)", "INSERT INTO a VALUES (1)"]),
            "txn_ddl_dml"
        );
        assert_eq!(
            rule(&["INSERT INTO a VALUES (1)", "CREATE INDEX i ON a (id)"]),
            "txn_ddl_dml"
        );
        assert!(s
            .check_batch(&plans(&["BEGIN", "CREATE INDEX i ON a (id)", "COMMIT"]))
            .is_ok());
        assert!(s.check_batch(&plans(&["SELECT 1", "SELECT 2"])).is_ok());
        assert!(s
            .check_batch(&plans(&[
                "INSERT INTO a VALUES (1)",
                "INSERT INTO a VALUES (2)"
            ]))
            .is_ok());
        assert!(s
            .check_batch(&plans(&[
                "CREATE TABLE a (id int)",
                "COMMIT",
                "CREATE TABLE b (id int)"
            ]))
            .is_ok());
    }

    // --- settings (US4) ---

    fn respond(s: &mut SessionPolicy, sql: &str, status: TxnStatus) -> Admission<'static> {
        // Leak the plan: the admission borrows it, and the test only inspects the reply.
        let p: &'static StatementPlan = Box::leak(Box::new(plan(sql)));
        s.admit(p, status, Instant::now())
    }

    fn show(s: &mut SessionPolicy, name: &str, status: TxnStatus) -> String {
        match respond(s, &format!("SHOW {name}"), status) {
            Admission::Respond(Reply::Show { value, .. }) => value,
            other => panic!("SHOW {name}: {other:?}"),
        }
    }

    fn tag(a: Admission<'_>) -> &'static str {
        match a {
            Admission::Respond(Reply::Command { tag }) => tag,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn remembered_settings_are_set_shown_and_reset() {
        let mut s = SessionPolicy::new(PolicyLimits::default());
        let idle = TxnStatus::Idle;
        assert_eq!(show(&mut s, "enable_seqscan", idle), "on");
        assert_eq!(
            tag(respond(&mut s, "SET enable_seqscan = off", idle)),
            "SET"
        );
        assert_eq!(show(&mut s, "enable_seqscan", idle), "off");
        match respond(&mut s, "SHOW DateStyle", idle) {
            Admission::Respond(Reply::Show { column, value }) => {
                assert_eq!((column.as_str(), value.as_str()), ("DateStyle", "ISO, MDY"))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(tag(respond(&mut s, "RESET enable_seqscan", idle)), "RESET");
        assert_eq!(show(&mut s, "enable_seqscan", idle), "on");
        respond(&mut s, "SET enable_seqscan = off", idle);
        assert_eq!(
            tag(respond(&mut s, "SET enable_seqscan TO DEFAULT", idle)),
            "SET"
        );
        assert_eq!(show(&mut s, "enable_seqscan", idle), "on");
    }

    #[test]
    fn reset_all_reaches_the_engine_and_clears_values_on_completion() {
        let now = Instant::now();
        let mut s = SessionPolicy::new(PolicyLimits::default());
        respond(&mut s, "SET enable_seqscan = off", TxnStatus::Idle);
        respond(&mut s, "SET datestyle = ISO, DMY", TxnStatus::Idle);
        let reset = plan("RESET ALL");
        assert!(matches!(
            s.admit(&reset, TxnStatus::Idle, now),
            Admission::Run { sql: "RESET ALL" }
        ));
        // Nothing is cleared until the engine has run it.
        assert_eq!(show(&mut s, "enable_seqscan", TxnStatus::Idle), "off");
        s.complete(&reset, 0, TxnStatus::Idle, now).unwrap();
        assert_eq!(show(&mut s, "enable_seqscan", TxnStatus::Idle), "on");
        assert_eq!(show(&mut s, "datestyle", TxnStatus::Idle), "ISO, MDY");
        // A failed RESET ALL leaves the values alone.
        respond(&mut s, "SET enable_seqscan = off", TxnStatus::Idle);
        s.failed(TxnStatus::Idle);
        assert_eq!(show(&mut s, "enable_seqscan", TxnStatus::Idle), "off");
    }

    #[test]
    fn set_local_lasts_until_the_transaction_ends() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        let active = TxnStatus::Active;
        assert_eq!(
            tag(respond(&mut s, "SET LOCAL enable_seqscan = off", active)),
            "SET"
        );
        assert_eq!(show(&mut s, "enable_seqscan", active), "off");
        s.complete(&plan("COMMIT"), 0, TxnStatus::Idle, now)
            .unwrap();
        assert_eq!(show(&mut s, "enable_seqscan", TxnStatus::Idle), "on");
        // ROLLBACK ends it too.
        s.complete(&plan("BEGIN"), 0, TxnStatus::Active, now)
            .unwrap();
        respond(&mut s, "SET LOCAL enable_seqscan = off", active);
        s.complete(&plan("ROLLBACK"), 0, TxnStatus::Idle, now)
            .unwrap();
        assert_eq!(show(&mut s, "enable_seqscan", TxnStatus::Idle), "on");
    }

    #[test]
    fn set_local_outside_a_transaction_has_no_effect() {
        let mut s = SessionPolicy::new(PolicyLimits::default());
        assert_eq!(
            tag(respond(
                &mut s,
                "SET LOCAL enable_seqscan = off",
                TxnStatus::Idle
            )),
            "SET"
        );
        assert_eq!(show(&mut s, "enable_seqscan", TxnStatus::Idle), "on");
    }

    #[test]
    fn a_session_set_inside_a_transaction_survives_it() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        respond(&mut s, "SET LOCAL enable_seqscan = off", TxnStatus::Active);
        respond(&mut s, "SET enable_seqscan = off", TxnStatus::Active);
        respond(&mut s, "SET LOCAL enable_hashjoin = off", TxnStatus::Active);
        s.complete(&plan("COMMIT"), 0, TxnStatus::Idle, now)
            .unwrap();
        assert_eq!(show(&mut s, "enable_seqscan", TxnStatus::Idle), "off");
        assert_eq!(show(&mut s, "enable_hashjoin", TxnStatus::Idle), "on");
    }

    #[test]
    fn role_is_remembered_for_the_transaction_only() {
        let now = Instant::now();
        let mut s = begun(PolicyLimits::default(), now);
        assert_eq!(show(&mut s, "role", TxnStatus::Active), "none");
        assert_eq!(
            tag(respond(&mut s, "SET LOCAL role app", TxnStatus::Active)),
            "SET"
        );
        assert_eq!(show(&mut s, "role", TxnStatus::Active), "app");
        s.complete(&plan("COMMIT"), 0, TxnStatus::Idle, now)
            .unwrap();
        assert_eq!(show(&mut s, "role", TxnStatus::Idle), "none");
    }

    #[test]
    fn settings_statements_obey_the_other_admission_rules() {
        let mut s = SessionPolicy::new(PolicyLimits::default());
        // Refusals.
        assert!(
            matches!(respond(&mut s, "SET work_mem = '1MB'", TxnStatus::Idle), Admission::Refuse(e) if e.rule == "set_not_supported")
        );
        // Forwarded names go to the engine.
        assert!(matches!(
            respond(&mut s, "SET search_path = a", TxnStatus::Idle),
            Admission::Run { .. }
        ));
        assert!(matches!(
            respond(&mut s, "SHOW search_path", TxnStatus::Idle),
            Admission::Run { .. }
        ));
        // Accepted no-op.
        assert_eq!(
            tag(respond(
                &mut s,
                "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ",
                TxnStatus::Idle
            )),
            "SET"
        );
        // An aborted block answers 25P02 through the engine, whatever the statement.
        assert!(matches!(
            respond(&mut s, "SET enable_seqscan = off", TxnStatus::Aborted),
            Admission::Run { .. }
        ));
        assert!(matches!(
            respond(&mut s, "SHOW enable_seqscan", TxnStatus::Aborted),
            Admission::Run { .. }
        ));
    }
}
