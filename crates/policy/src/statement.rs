//! Everything known about one statement text without session state.

use pg_query::NodeEnum;

use crate::classify::{classify, StatementClass};
use crate::errors::PolicyError;
use crate::identity::desugar;
use crate::intercept::{intercept_node, Intercept};
use crate::jobs::{ddl_action, job_action, DdlAction, JobAction};
use crate::locking::{plan_locking, LockingRead};
use crate::rewrite::{apply_edits, ast_rewrites, pre_parse, AsyncKind};
use crate::rules::{check_rules, RuleContext};
use crate::session::{ColumnType, ReplyColumn};
use crate::settings::{check_set_config, SettingCommand};

/// The result of running the policy pipeline on one statement text (contracts/policy-api.md §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementPlan {
    /// The text the client sent; error positions refer to it.
    pub original: String,
    /// The text the engine runs: `original` with rewrites applied.
    pub engine_sql: String,
    /// libpg_query could not parse the text; no rule applies and the engine reports the error.
    pub parse_failed: bool,
    pub class: StatementClass,
    /// The first stateless deny rule that matches.
    pub refusal: Option<PolicyError>,
    /// A SET / RESET / SHOW the policy handles (remembered settings, no-ops).
    pub setting: Option<SettingCommand>,
    /// A statement the server answers itself (fixed `SHOW`, `DEALLOCATE`, `SET timezone`).
    pub intercept: Option<Intercept>,
    /// Row count of an `INSERT ... VALUES` list.
    pub values_rows: Option<u64>,
    /// The DSQL `ASYNC` keyword the pre-parse blanked, and where.
    pub async_kind: Option<AsyncKind>,
    /// Engine statements run before `engine_sql`, in the same transaction (identity sequences).
    pub prelude: Vec<String>,
    /// Engine statements run when `engine_sql` fails while no transaction is open.
    pub cleanup_on_failure: Vec<String>,
    /// A statement the server answers through `sys.jobs`.
    pub job: Option<JobAction>,
    /// DDL the server runs with extra work.
    pub ddl: Option<DdlAction>,
    /// A `SELECT ... FOR UPDATE / FOR KEY SHARE` split into touch and read (`engine_sql` is the
    /// read).
    pub locking: Option<LockingRead>,
}

impl StatementPlan {
    /// Plans one statement. Text with several statements (or none) is left to the engine.
    pub fn new(sql: &str) -> Self {
        let pre = pre_parse(sql);
        let (text, async_kind) = (pre.text, pre.async_kind);
        let plan = |engine_sql: String, parse_failed: bool| Self {
            original: sql.to_owned(),
            engine_sql,
            parse_failed,
            class: StatementClass::Other,
            refusal: None,
            setting: None,
            intercept: None,
            values_rows: None,
            async_kind,
            prelude: Vec::new(),
            cleanup_on_failure: Vec::new(),
            job: None,
            ddl: None,
            locking: None,
        };
        let Ok(parsed) = pg_query::parse(&text) else {
            // The engine reports the syntax error, on the text with `ASYNC` blanked.
            return plan(text.into_owned(), true);
        };
        let [raw] = parsed.protobuf.stmts.as_slice() else {
            return plan(text.into_owned(), false);
        };
        let Some(node) = raw.stmt.as_ref().and_then(|n| n.node.as_ref()) else {
            return plan(text.into_owned(), false);
        };
        let mut edits = ast_rewrites(node, &text);
        let identity = desugar(node, &text);
        edits.extend(identity.edits);
        // Stateless refusals: deny rules first, then the settings whitelist.
        let (setting, setting_refusal) = match SettingCommand::from_ast(node) {
            Ok(setting) => (setting, check_set_config(&text)),
            Err(error) => (None, Some(error)),
        };
        let (locking, locking_refusal) = match plan_locking(node) {
            Some(Ok(locking)) => (Some(locking), None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, None),
        };
        let refusal = check_rules(node, &text, &RuleContext { async_kind })
            .or(setting_refusal)
            .or(locking_refusal);
        let engine_sql = match &locking {
            Some(locking) => locking.read_sql.clone(),
            None => apply_edits(&text, &edits).into_owned(),
        };
        let mut planned = Self {
            engine_sql,
            locking,
            class: classify(node),
            refusal,
            setting,
            intercept: intercept_node(node),
            values_rows: values_rows(node),
            job: job_action(node, async_kind),
            ddl: ddl_action(node, &text),
            prelude: identity.prelude,
            cleanup_on_failure: identity.cleanup,
            ..plan(String::new(), false)
        };
        // A refused statement never reaches the server's dispatch.
        if planned.refusal.is_some() {
            planned.prelude.clear();
            planned.cleanup_on_failure.clear();
            planned.job = None;
            planned.ddl = None;
            planned.locking = None;
        }
        planned
    }
}

impl StatementPlan {
    /// The result columns when the policy or the server (not the engine) answers this statement
    /// with rows: needed to describe the statement in the extended protocol.
    pub fn reply_columns(&self) -> Option<Vec<ReplyColumn>> {
        let column = |name: &str, ty| ReplyColumn {
            name: name.to_owned(),
            ty,
        };
        match &self.job {
            Some(JobAction::IndexBuild { .. } | JobAction::Validate { .. }) => {
                return Some(vec![column("job_id", ColumnType::Text)])
            }
            Some(JobAction::WaitForJob { .. }) => {
                return Some(vec![column("succeeded", ColumnType::Bool)])
            }
            Some(JobAction::MajorVersion { column: name }) => {
                return Some(vec![column(name, ColumnType::Int4)])
            }
            None => {}
        }
        self.show_column()
            .map(|name| vec![column(name, ColumnType::Text)])
    }

    /// The column name when the policy answers this statement with a one-row `SHOW` result
    /// (needed to describe the statement in the extended protocol).
    pub fn show_column(&self) -> Option<&str> {
        match (&self.setting, &self.intercept) {
            (Some(SettingCommand::Show { name }), _) => Some(crate::settings::show_column(name)),
            (_, Some(Intercept::Show { column, .. })) => Some(column),
            _ => None,
        }
    }
}

/// The number of rows of an `INSERT ... VALUES (...), (...)` list.
fn values_rows(node: &NodeEnum) -> Option<u64> {
    let NodeEnum::InsertStmt(insert) = node else {
        return None;
    };
    let Some(NodeEnum::SelectStmt(select)) = insert.select_stmt.as_ref()?.node.as_ref() else {
        return None;
    };
    (!select.values_lists.is_empty()).then_some(select.values_lists.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::TxnControl;

    #[test]
    fn plain_statement() {
        let plan = StatementPlan::new("SELECT 1");
        assert!(!plan.parse_failed);
        assert_eq!(plan.class, StatementClass::Read);
        assert_eq!(plan.original, "SELECT 1");
        assert_eq!(plan.engine_sql, "SELECT 1");
        assert_eq!(plan.refusal, None);
        assert_eq!(plan.intercept, None);
        assert_eq!(plan.values_rows, None);
    }

    #[test]
    fn unparsable_text_is_left_to_the_engine() {
        let plan = StatementPlan::new("CREATE INDEX ASYNC public.i ON t (c)");
        assert!(plan.parse_failed);
        assert_eq!(plan.engine_sql, "CREATE INDEX       public.i ON t (c)");
        let plan = StatementPlan::new("SELEC 1");
        assert!(plan.parse_failed);
        assert_eq!(plan.engine_sql, "SELEC 1");
        assert_eq!(plan.class, StatementClass::Other);
        assert_eq!(plan.refusal, None);
    }

    #[test]
    fn several_statements_are_not_planned() {
        let plan = StatementPlan::new("SELECT 1; SELECT 2");
        assert!(!plan.parse_failed);
        assert_eq!(plan.class, StatementClass::Other);
        assert_eq!(plan.engine_sql, "SELECT 1; SELECT 2");
        let plan = StatementPlan::new("");
        assert!(!plan.parse_failed);
        assert_eq!(plan.class, StatementClass::Other);
    }

    #[test]
    fn classes_and_intercepts() {
        assert_eq!(
            StatementPlan::new("BEGIN").class,
            StatementClass::Txn(TxnControl::Begin)
        );
        assert_eq!(
            StatementPlan::new("DEALLOCATE ALL").intercept,
            Some(Intercept::Deallocate { name: None })
        );
        assert!(matches!(
            StatementPlan::new("SHOW server_version").intercept,
            Some(Intercept::Show { .. })
        ));
    }

    #[test]
    fn engine_sql_carries_rewrites_and_original_stays_untouched() {
        let plan = StatementPlan::new("CREATE TABLE t (n numeric, m int)");
        assert_eq!(plan.original, "CREATE TABLE t (n numeric, m int)");
        assert_eq!(plan.engine_sql, "CREATE TABLE t (n numeric(18,6), m int)");
    }

    #[test]
    fn refusals_are_recorded_in_the_plan() {
        let plan = StatementPlan::new("TRUNCATE t");
        assert_eq!(plan.refusal.as_ref().map(|e| e.rule), Some("truncate"));
        assert_eq!(StatementPlan::new("SELECT 1").refusal, None);
    }

    #[test]
    fn values_rows_counts_value_lists() {
        assert_eq!(
            StatementPlan::new("INSERT INTO t VALUES (1)").values_rows,
            Some(1)
        );
        let rows = (0..3001)
            .map(|i| format!("({i})"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            StatementPlan::new(&format!("INSERT INTO t VALUES {rows}")).values_rows,
            Some(3001)
        );
        assert_eq!(
            StatementPlan::new("INSERT INTO t SELECT * FROM s").values_rows,
            None
        );
        assert_eq!(StatementPlan::new("UPDATE t SET a = 1").values_rows, None);
        assert_eq!(
            StatementPlan::new("SELECT * FROM (VALUES (1),(2)) v").values_rows,
            None
        );
    }

    #[test]
    fn async_kind_is_recorded_and_the_engine_text_is_blanked() {
        let plan = StatementPlan::new("CREATE INDEX ASYNC i ON t (c)");
        assert_eq!(plan.async_kind, Some(AsyncKind::Index));
        assert_eq!(plan.original, "CREATE INDEX ASYNC i ON t (c)");
        assert!(!plan.parse_failed);
        assert_eq!(StatementPlan::new("SELECT 1").async_kind, None);
    }

    #[test]
    fn reply_columns_describe_policy_answers() {
        let columns = |sql: &str| StatementPlan::new(sql).reply_columns();
        assert_eq!(
            columns("SHOW server_version"),
            Some(vec![ReplyColumn {
                name: "server_version".into(),
                ty: ColumnType::Text
            }])
        );
        assert_eq!(columns("SELECT 1"), None);
        assert_eq!(columns("SET work_mem = 1"), None);
    }

    /// A refused plan never carries work for the server's dispatch.
    #[test]
    fn a_refused_plan_has_no_job_ddl_or_prelude() {
        for sql in [
            "TRUNCATE t",
            "CREATE UNLOGGED TABLE t (id int)",
            "CREATE TABLE t (id serial)",
        ] {
            let plan = StatementPlan::new(sql);
            assert!(plan.refusal.is_some(), "{sql}");
            assert_eq!(plan.job, None, "{sql}");
            assert_eq!(plan.ddl, None, "{sql}");
            assert!(plan.prelude.is_empty(), "{sql}");
            assert!(plan.cleanup_on_failure.is_empty(), "{sql}");
        }
    }

    #[test]
    fn a_locking_read_is_planned_as_touch_and_read() {
        let plan = StatementPlan::new("SELECT s FROM t WHERE id = 1 FOR UPDATE");
        assert_eq!(plan.refusal, None);
        assert_eq!(plan.class, StatementClass::Read);
        assert_eq!(plan.original, "SELECT s FROM t WHERE id = 1 FOR UPDATE");
        assert_eq!(plan.engine_sql, "SELECT s FROM t WHERE id = 1");
        let locking = plan.locking.expect("planned");
        assert_eq!(
            locking.touch.unwrap().render("v"),
            "UPDATE t SET v = v WHERE id = 1"
        );
        let plan = StatementPlan::new("SELECT s FROM t FOR KEY SHARE");
        assert_eq!(plan.engine_sql, "SELECT s FROM t");
        assert!(plan.locking.is_some_and(|l| l.touch.is_none()));
        assert_eq!(StatementPlan::new("SELECT 1").locking, None);
    }

    #[test]
    fn unsupported_locking_forms_are_refused_and_other_strengths_keep_their_rule() {
        let plan = StatementPlan::new("SELECT * FROM a JOIN b ON a.id = b.id FOR UPDATE");
        assert_eq!(plan.refusal.map(|e| e.rule), Some("locking_read_form"));
        assert_eq!(plan.locking, None);
        let plan = StatementPlan::new("SELECT * FROM t FOR SHARE");
        assert_eq!(plan.refusal.map(|e| e.rule), Some("unsupported_locking"));
        assert_eq!(plan.locking, None);
    }
}
