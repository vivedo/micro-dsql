//! Optimistic concurrency the way Aurora DSQL has it (M4): autocommit statements as
//! single-statement transactions, conflicts reported at `COMMIT`, a snapshot taken at `BEGIN`
//! and `SELECT ... FOR UPDATE` as a write-set touch.
//!
//! Every statement the policy plans goes through [`run`], which hands a wrapped engine closure
//! to `dsql::run_planned` (006's dispatch is unchanged). The engine statements this module
//! issues on its own (`BEGIN`, `COMMIT`, `ROLLBACK`, the pin read, touches) never pass through
//! the policy and are never shown to the client.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use micro_dsql_policy::{
    EngineFailure, SessionPolicy, StatementClass, StatementPlan, TouchPlan, TxnControl, TxnStatus,
};
use pgwire::api::portal::Format;
use pgwire::error::{PgWireError, PgWireResult};
use tracing::debug;
use turso_pg::PgConnection;

use crate::internal::{exec, ident};
use crate::{dsql, Outcome};

/// Switches of the OCC emulation (`ServerOptions::occ`).
#[derive(Debug, Clone, Copy)]
pub struct OccOptions {
    /// Report a write conflict inside an explicit transaction at `COMMIT` (the doomed
    /// transaction), as DSQL does, instead of at the statement.
    pub defer_conflicts: bool,
    /// Fix the snapshot at `BEGIN` instead of at the first statement.
    pub snapshot_at_begin: bool,
}

impl Default for OccOptions {
    fn default() -> Self {
        Self {
            defer_conflicts: true,
            snapshot_at_begin: true,
        }
    }
}

/// Runs statements of the client on the engine; implemented by the simple and the extended
/// protocol (the extended one binds the portal's parameters).
pub(crate) trait Engine {
    /// Runs `sql` and returns its outcome and change count.
    fn run(&mut self, sql: &str) -> PgWireResult<(Outcome, u64)>;
    /// The outcome of `sql` with no rows and a 0 count (describe only, no execution).
    fn empty(&mut self, sql: &str) -> PgWireResult<(Outcome, u64)>;
}

/// The read that fixes a transaction's snapshot: any table read does, this one returns nothing.
pub(crate) const PIN_SQL: &str = "SELECT 1 FROM pg_catalog.pg_class LIMIT 0";

/// How long contention for the engine's exclusive lock is retried.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BusyRetry {
    pub attempts: u32,
    pub budget: Duration,
}

pub(crate) const BUSY_RETRY: BusyRetry = BusyRetry {
    attempts: 10,
    budget: Duration::from_millis(200),
};

/// The engine failure behind a wire error, if it came from the engine.
pub(crate) fn failure_of(error: &PgWireError) -> Option<EngineFailure> {
    match error {
        PgWireError::ApiError(inner) => inner
            .downcast_ref::<turso_pg::Error>()
            .map(dsql::engine_failure),
        _ => None,
    }
}

fn lock(policy: &Mutex<SessionPolicy>) -> MutexGuard<'_, SessionPolicy> {
    policy.lock().unwrap_or_else(|e| e.into_inner())
}

/// Ends whatever engine transaction is open; errors are ignored (nothing is open after a
/// failed commit, and a rollback of nothing is not an error).
fn rollback(conn: &PgConnection) {
    let _ = exec(conn, "ROLLBACK");
}

/// Runs `attempt` again while it fails with lock contention, for at most [`BUSY_RETRY`]. Every
/// attempt must leave nothing applied when it fails.
fn with_busy_retry<T>(mut attempt: impl FnMut() -> PgWireResult<T>) -> PgWireResult<T> {
    let started = Instant::now();
    let mut tries = 0;
    loop {
        let error = match attempt() {
            Err(error) if failure_of(&error) == Some(EngineFailure::LockBusy) => error,
            other => return other,
        };
        tries += 1;
        if tries >= BUSY_RETRY.attempts || started.elapsed() >= BUSY_RETRY.budget {
            debug!(tries, "residual contention");
            return Err(error);
        }
        if tries == 1 {
            debug!("busy retry");
        }
        std::thread::sleep(Duration::from_millis(2 * u64::from(tries)));
    }
}

/// Idle only: unpinned `BEGIN`, the steps, `COMMIT`; `ROLLBACK` on any error; the whole thing
/// is retried on lock contention within [`BUSY_RETRY`].
pub(crate) fn internal_transaction<T>(
    conn: &PgConnection,
    mut steps: impl FnMut() -> PgWireResult<T>,
) -> PgWireResult<T> {
    with_busy_retry(|| {
        exec(conn, "BEGIN")?;
        let result = steps().and_then(|value| exec(conn, "COMMIT").map(|()| value));
        if result.is_err() {
            rollback(conn);
        }
        result
    })
}

/// An autocommit DML statement as a single-statement transaction: `BEGIN`, the statement, the
/// row limit, `COMMIT`; any failure rolls back and is reported; contention retries the whole
/// attempt (it applied nothing). A `FOR UPDATE` read touches its rows first.
fn implicit_transaction(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    engine: &mut dyn Engine,
    touch: Option<&str>,
    sql: &str,
) -> PgWireResult<(Outcome, u64)> {
    with_busy_retry(|| {
        exec(conn, "BEGIN")?;
        let result = (|| {
            if let Some(touch) = touch {
                engine.run(touch)?;
            }
            let (outcome, rows) = engine.run(sql)?;
            lock(policy)
                .check_implicit_rows(rows)
                .map_err(dsql::policy_error)?;
            exec(conn, "COMMIT")?;
            Ok((outcome, rows))
        })();
        if result.is_err() {
            rollback(conn);
        }
        result
    })
}

/// Fixes the snapshot of the open transaction: the engine takes it at the first table read, and
/// any table read does, so this one (which returns no rows) makes `BEGIN` the point in time.
/// A failing pin is ignored: the transaction then behaves as it did without it.
fn pin(conn: &PgConnection, policy: &Mutex<SessionPolicy>) {
    match exec(conn, PIN_SQL) {
        Ok(()) => lock(policy).mark_pinned(),
        Err(error) => debug!(?error, "snapshot pin failed"),
    }
}

/// Re-opens the engine transaction of the client after a rollback: its own `BEGIN` text (so
/// options such as `READ ONLY` survive), and, when the snapshot is taken at `BEGIN`, the pin.
fn reopen(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    opts: &OccOptions,
) -> PgWireResult<()> {
    rollback(conn);
    let begin = lock(policy).begin_sql().unwrap_or("BEGIN").to_owned();
    exec(conn, &begin)?;
    if opts.snapshot_at_begin {
        pin(conn, policy);
    }
    Ok(())
}

/// A statement of an explicit transaction that hit a write conflict (DSQL reports it at
/// `COMMIT`): roll back, re-open the transaction on a fresh snapshot, flag it doomed, and run
/// the statement once more. If it conflicts again (the other writer is still open) the
/// statement answers with no rows. Anything else is the statement's own error.
fn defer_conflict(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    opts: &OccOptions,
    engine: &mut dyn Engine,
    sql: &str,
    conflict: PgWireError,
) -> PgWireResult<(Outcome, u64)> {
    debug!("deferred conflict");
    if reopen(conn, policy, opts).is_err() {
        return Err(conflict);
    }
    lock(policy).mark_doomed();
    match engine.run(sql) {
        Err(second) if failure_of(&second) == Some(EngineFailure::DataConflict) => {
            reopen(conn, policy, opts)?;
            engine.empty(sql)
        }
        other => other,
    }
}

/// The column a touch rewrites to itself: the first one that is not part of the primary key
/// (so no key is rewritten), else the first. `None` when the table cannot be found: the read
/// then reports the usual error.
fn touch_column(conn: &PgConnection, touch: &TouchPlan) -> Option<String> {
    let schema = conn.inner().current_schema();
    let table = schema
        .namespace_named(touch.schema().unwrap_or("public"))
        .and_then(|namespace| schema.get_btree_table(namespace, touch.relation()));
    if let Some(table) = table {
        let names: Vec<&str> = table
            .columns()
            .iter()
            .filter_map(|column| column.name.as_deref())
            .collect();
        let in_key = |name: &str| {
            table
                .primary_key_columns
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case(name))
        };
        let chosen = names.iter().find(|name| !in_key(name)).or(names.first());
        return chosen.map(|name| (*name).to_owned());
    }
    // The schema lookup failed: ask the engine for the table's first column.
    let qualified = match touch.schema() {
        Some(schema) => format!("{}.{}", ident(schema), ident(touch.relation())),
        None => ident(touch.relation()),
    };
    let stmt = conn
        .prepare(format!("SELECT * FROM {qualified} LIMIT 0"))
        .ok()?;
    (stmt.num_columns() > 0).then(|| stmt.get_column_name(0).into_owned())
}

/// Runs a statement of an explicit transaction. With deferral on, a write conflict does not
/// fail it (DSQL reports conflicts at `COMMIT`): see [`defer_conflict`].
fn run_in_transaction(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    opts: &OccOptions,
    engine: &mut dyn Engine,
    sql: &str,
) -> PgWireResult<(Outcome, u64)> {
    match engine.run(sql) {
        Err(error)
            if opts.defer_conflicts
                && failure_of(&error) == Some(EngineFailure::DataConflict)
                && !lock(policy).is_implicit() =>
        {
            defer_conflict(conn, policy, opts, engine, sql, error)
        }
        other => other,
    }
}

/// The engine closure `run_planned` calls for the client's statement: the statement itself,
/// inside whatever wrapper its situation calls for.
fn run_wrapped(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    plan: &StatementPlan,
    opts: &OccOptions,
    engine: &mut dyn Engine,
    sql: &str,
) -> PgWireResult<(Outcome, u64)> {
    let status = dsql::txn_status(conn);
    // The touch of a `FOR UPDATE`; in a failed block the engine answers the read with 25P02.
    let touch = plan
        .locking
        .as_ref()
        .and_then(|locking| locking.touch.as_ref())
        .filter(|_| status != TxnStatus::Aborted)
        .and_then(|touch| touch_column(conn, touch).map(|column| touch.render(&column)));
    match (plan.class, status) {
        (StatementClass::Dml, TxnStatus::Idle) => {
            implicit_transaction(conn, policy, engine, None, sql)
        }
        (_, TxnStatus::Idle) if touch.is_some() => {
            implicit_transaction(conn, policy, engine, touch.as_deref(), sql)
        }
        (StatementClass::Ddl, TxnStatus::Idle) => with_busy_retry(|| engine.run(sql)),
        (StatementClass::Dml, TxnStatus::Active) => {
            run_in_transaction(conn, policy, opts, engine, sql)
        }
        (_, TxnStatus::Active) if touch.is_some() => {
            if let Some(touch) = &touch {
                run_in_transaction(conn, policy, opts, engine, touch)?;
            }
            run_in_transaction(conn, policy, opts, engine, sql)
        }
        _ => engine.run(sql),
    }
}

/// Runs one planned statement: the entry point of every client statement.
pub(crate) fn run(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    plan: &StatementPlan,
    format: &Format,
    opts: &OccOptions,
    engine: &mut dyn Engine,
) -> PgWireResult<Outcome> {
    // A DDL statement as the first statement of a pinned transaction runs on a fresh,
    // unpinned snapshot: the engine refuses DDL on a snapshot that is out of date (E-P4).
    // Nothing but the pin has run, so nothing is lost by re-opening.
    if opts.snapshot_at_begin
        && plan.refusal.is_none()
        && dsql::txn_status(conn) == TxnStatus::Active
        && lock(policy).needs_fresh_snapshot(plan)
    {
        debug!("snapshot re-opened for DDL");
        let begin = lock(policy).begin_sql().unwrap_or("BEGIN").to_owned();
        rollback(conn);
        exec(conn, &begin)?;
    }
    let result = dsql::run_planned(conn, policy, plan, format, |sql| {
        run_wrapped(conn, policy, plan, opts, engine, sql)
    });
    // The snapshot of a transaction is fixed when it begins.
    if result.is_ok()
        && opts.snapshot_at_begin
        && plan.class == StatementClass::Txn(TxnControl::Begin)
        && dsql::txn_status(conn) == TxnStatus::Active
        && lock(policy).begin_sql().is_some()
    {
        pin(conn, policy);
    }
    result
}

/// What a multi-statement string produced: the outcomes of the statements that ran, then the
/// error that stopped it (if any).
pub(crate) struct BatchResult {
    pub outcomes: Vec<Outcome>,
    pub error: Option<PgWireError>,
}

/// Whether a simple-query string is run as one implicit transaction (FR-006): the session is
/// idle, there are at least two statements, none is transaction control, and at least one is
/// DML or DDL.
pub(crate) fn is_batch(conn: &PgConnection, plans: &[StatementPlan]) -> bool {
    plans.len() >= 2
        && dsql::txn_status(conn) == TxnStatus::Idle
        && !plans
            .iter()
            .any(|plan| matches!(plan.class, StatementClass::Txn(_)))
        && plans
            .iter()
            .any(|plan| matches!(plan.class, StatementClass::Dml | StatementClass::Ddl))
}

/// Runs the statements of one string as one transaction: `BEGIN`, each statement through
/// `each` (which calls [`run`]), `COMMIT`; the first error rolls everything back.
pub(crate) fn run_batch(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    plans: &[StatementPlan],
    mut each: impl FnMut(&StatementPlan) -> PgWireResult<Outcome>,
) -> BatchResult {
    let mut outcomes = Vec::new();
    // Nothing is applied by a failed BEGIN, so contention can be retried here.
    if let Err(error) = with_busy_retry(|| exec(conn, "BEGIN")) {
        return BatchResult {
            outcomes,
            error: Some(error),
        };
    }
    lock(policy).begin_implicit(Instant::now());
    let mut result = Ok(());
    for plan in plans {
        match each(plan) {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => {
                result = Err(error);
                break;
            }
        }
    }
    if result.is_ok() {
        result = exec(conn, "COMMIT");
    }
    if result.is_err() {
        rollback(conn);
    }
    lock(policy).failed(TxnStatus::Idle);
    BatchResult {
        outcomes,
        error: result.err(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turso_core::LimboError;

    fn wire(error: LimboError) -> PgWireError {
        PgWireError::ApiError(Box::new(turso_pg::Error::from(error)))
    }

    #[test]
    fn failure_of_classifies_engine_errors() {
        assert_eq!(
            failure_of(&wire(LimboError::WriteWriteConflict)),
            Some(EngineFailure::DataConflict)
        );
        assert_eq!(
            failure_of(&wire(LimboError::Busy)),
            Some(EngineFailure::LockBusy)
        );
        assert_eq!(
            failure_of(&wire(LimboError::SchemaConflict)),
            Some(EngineFailure::SchemaConflict)
        );
        assert_eq!(
            failure_of(&wire(LimboError::DivisionByZero)),
            Some(EngineFailure::Other)
        );
    }

    #[test]
    fn failure_of_ignores_errors_that_are_not_the_engine_s() {
        assert_eq!(failure_of(&PgWireError::NotReadyForQuery), None);
        let policy = PgWireError::ApiError(Box::new(micro_dsql_policy::oc000()));
        assert_eq!(failure_of(&policy), None);
    }

    // --- internal_transaction (T074) ---

    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use turso_core::ConnectOpts;

    use crate::internal::{exec, query};
    use crate::{open_database, Storage};

    fn two_connections() -> (PgConnection, PgConnection) {
        let db = open_database(&Storage::Memory).unwrap();
        let connect = || PgConnection::connect_with(&db, ConnectOpts::new()).unwrap();
        (connect(), connect())
    }

    #[test]
    fn internal_transaction_retries_while_another_ddl_transaction_is_open() {
        let (a, b) = two_connections();
        exec(&a, "CREATE TABLE seed (id int PRIMARY KEY)").unwrap();
        exec(&b, "BEGIN").unwrap();
        exec(&b, "CREATE TABLE holder (id int)").unwrap();
        let release = {
            let b = b.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                exec(&b, "COMMIT").unwrap();
            })
        };
        let attempts = Arc::new(AtomicU32::new(0));
        let counted = attempts.clone();
        internal_transaction(&a, || {
            counted.fetch_add(1, Ordering::SeqCst);
            exec(&a, "CREATE TABLE made (id int)")
        })
        .unwrap();
        release.join().unwrap();
        assert!(attempts.load(Ordering::SeqCst) > 1, "the steps ran once");
        assert_eq!(dsql::txn_status(&a), micro_dsql_policy::TxnStatus::Idle);
        let rows = query(&a, "SELECT count(*) FROM made").unwrap();
        assert_eq!(rows[0][0].as_deref(), Some("0"));
    }

    #[test]
    fn internal_transaction_gives_up_with_lock_busy_and_leaves_nothing() {
        let (a, b) = two_connections();
        exec(&a, "CREATE TABLE seed (id int PRIMARY KEY)").unwrap();
        exec(&b, "BEGIN").unwrap();
        exec(&b, "CREATE TABLE holder (id int)").unwrap();
        let error = internal_transaction(&a, || exec(&a, "CREATE TABLE made (id int)"))
            .expect_err("the lock is never released");
        assert_eq!(failure_of(&error), Some(EngineFailure::LockBusy));
        assert_eq!(dsql::txn_status(&a), micro_dsql_policy::TxnStatus::Idle);
        exec(&b, "COMMIT").unwrap();
        // Nothing the steps wrote exists.
        assert!(query(&a, "SELECT count(*) FROM made").is_err());
    }

    #[test]
    fn internal_transaction_rolls_back_when_the_steps_fail() {
        let (a, _b) = two_connections();
        exec(&a, "CREATE TABLE seed (id int PRIMARY KEY)").unwrap();
        let error = internal_transaction(&a, || {
            exec(&a, "INSERT INTO seed VALUES (1)")?;
            exec(&a, "INSERT INTO seed VALUES (1)")
        })
        .expect_err("duplicate key");
        assert_eq!(failure_of(&error), Some(EngineFailure::Other));
        assert_eq!(dsql::txn_status(&a), micro_dsql_policy::TxnStatus::Idle);
        let rows = query(&a, "SELECT count(*) FROM seed").unwrap();
        assert_eq!(rows[0][0].as_deref(), Some("0"));
    }
}
