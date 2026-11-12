//! Everything known about one statement text without session state.

use pg_query::NodeEnum;

use crate::classify::{classify, StatementClass};
use crate::errors::PolicyError;
use crate::intercept::{intercept_node, Intercept};
use crate::rewrite::{apply_edits, ast_rewrites, pre_parse};

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
    /// A statement the server answers itself (fixed `SHOW`, `DEALLOCATE`, `SET timezone`).
    pub intercept: Option<Intercept>,
    /// Row count of an `INSERT ... VALUES` list.
    pub values_rows: Option<u64>,
}

impl StatementPlan {
    /// Plans one statement. Text with several statements (or none) is left to the engine.
    pub fn new(sql: &str) -> Self {
        let text = pre_parse(sql);
        let plan = |engine_sql: String, parse_failed: bool| Self {
            original: sql.to_owned(),
            engine_sql,
            parse_failed,
            class: StatementClass::Other,
            refusal: None,
            intercept: None,
            values_rows: None,
        };
        let Ok(parsed) = pg_query::parse(&text) else {
            return plan(sql.to_owned(), true);
        };
        let [raw] = parsed.protobuf.stmts.as_slice() else {
            return plan(text.into_owned(), false);
        };
        let Some(node) = raw.stmt.as_ref().and_then(|n| n.node.as_ref()) else {
            return plan(text.into_owned(), false);
        };
        let edits = ast_rewrites(node, &text);
        Self {
            engine_sql: apply_edits(&text, &edits).into_owned(),
            class: classify(node),
            intercept: intercept_node(node),
            values_rows: values_rows(node),
            ..plan(String::new(), false)
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
}
