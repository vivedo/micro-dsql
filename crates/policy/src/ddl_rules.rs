//! The M3 deny rules for DDL (contracts/rule-catalogue-m3.md): index DDL, identity and
//! sequences, `ALTER TABLE` forms and the `sys` schema. Stateless, on the AST and the
//! pre-parse result; evaluated after 005's rules, first match wins.

use pg_query::protobuf::{RangeVar, Token};
use pg_query::NodeEnum;

use crate::errors::{self, PolicyError};
use crate::jobs;
use crate::rewrite::AsyncKind;
use crate::rules::{tokens, RuleContext};

/// One M3 rule: sees the statement, its (blanked) text and the pre-parse context.
pub struct DdlRule {
    pub id: &'static str,
    pub check: fn(&NodeEnum, &str, &RuleContext) -> Option<PolicyError>,
}

/// The first M3 rule that matches.
pub fn check(node: &NodeEnum, sql: &str, ctx: &RuleContext) -> Option<PolicyError> {
    RULES.iter().find_map(|rule| (rule.check)(node, sql, ctx))
}

/// The M3 rules in evaluation order.
pub const RULES: &[DdlRule] = &[
    DdlRule {
        id: "index_concurrently",
        check: |node, _, _| {
            index(node)
                .filter(|i| i.concurrent)
                .map(|_| errors::index_concurrently())
        },
    },
    DdlRule {
        id: "index_sync",
        check: |node, _, ctx| {
            index(node)
                .filter(|_| ctx.async_kind != Some(AsyncKind::Index))
                .map(|_| errors::index_sync())
        },
    },
    DdlRule {
        id: "index_method",
        check: |node, _, _| {
            index(node)
                .filter(|i| {
                    !i.access_method.is_empty() && !i.access_method.eq_ignore_ascii_case("btree")
                })
                .map(|_| errors::index_method())
        },
    },
    DdlRule {
        id: "index_key_columns",
        check: |node, _, _| {
            index(node)
                .filter(|i| i.index_params.len() > MAX_INDEX_KEYS)
                .map(|_| errors::index_key_columns())
        },
    },
    DdlRule {
        id: "wait_for_job_select",
        check: |node, sql, _| {
            (!matches!(node, NodeEnum::CallStmt(_)) && has_sys_call(sql, "wait_for_job"))
                .then(errors::wait_for_job_select)
        },
    },
    DdlRule {
        id: "wait_for_job_subquery",
        check: |node, sql, _| {
            (is_wait_for_job_call(node) && tokens(sql).iter().any(|(t, _)| *t == Token::Select))
                .then(errors::wait_for_job_subquery)
        },
    },
    DdlRule {
        id: "wait_for_job_id",
        check: |node, _, _| {
            let NodeEnum::CallStmt(call) = node else {
                return None;
            };
            let call = call
                .funccall
                .as_ref()
                .filter(|_| is_wait_for_job_call(node))?;
            let id = const_string(call.args.first()?)?;
            (!jobs::is_job_id(&id)).then(errors::wait_for_job_id)
        },
    },
    DdlRule {
        id: "sys_write",
        check: |node, _, _| {
            let relation = match node {
                NodeEnum::InsertStmt(s) => s.relation.as_ref(),
                NodeEnum::UpdateStmt(s) => s.relation.as_ref(),
                NodeEnum::DeleteStmt(s) => s.relation.as_ref(),
                _ => None,
            }?;
            is_sys(relation).then(|| errors::sys_write(&relation.relname))
        },
    },
    DdlRule {
        id: "sys_ddl",
        check: |node, _, _| targets_sys_schema(node).then(errors::sys_ddl),
    },
];

/// DSQL's limit of key columns in one index.
const MAX_INDEX_KEYS: usize = 8;

fn index(node: &NodeEnum) -> Option<&pg_query::protobuf::IndexStmt> {
    match node {
        NodeEnum::IndexStmt(index) => Some(index),
        _ => None,
    }
}

fn is_sys(relation: &RangeVar) -> bool {
    relation.schemaname == "sys"
}

/// The string of a name node.
pub(crate) fn name_part(node: &pg_query::protobuf::Node) -> Option<&str> {
    match node.node.as_ref()? {
        NodeEnum::String(s) => Some(&s.sval),
        _ => None,
    }
}

/// `sys.<name>(` in the token stream: a call of a `sys` function anywhere in the statement.
fn has_sys_call(sql: &str, name: &str) -> bool {
    let toks = tokens(sql);
    let unquoted = |text: &str| text.trim_matches('"').to_owned();
    toks.windows(4).any(|w| {
        w[0].0 == Token::Ident
            && unquoted(w[0].1) == "sys"
            && w[1].1 == "."
            && w[2].0 == Token::Ident
            && unquoted(w[2].1) == name
            && w[3].1 == "("
    })
}

/// `CALL sys.wait_for_job(...)`.
pub(crate) fn is_wait_for_job_call(node: &NodeEnum) -> bool {
    let NodeEnum::CallStmt(call) = node else {
        return false;
    };
    let Some(func) = &call.funccall else {
        return false;
    };
    matches!(
        func.funcname
            .iter()
            .map(name_part)
            .collect::<Vec<_>>()
            .as_slice(),
        [Some("sys"), Some("wait_for_job")]
    )
}

/// The string of a constant argument (looking through `::type` casts).
pub(crate) fn const_string(node: &pg_query::protobuf::Node) -> Option<String> {
    match node.node.as_ref()? {
        NodeEnum::AConst(c) => match c.val.as_ref()? {
            pg_query::protobuf::a_const::Val::Sval(s) => Some(s.sval.clone()),
            _ => None,
        },
        NodeEnum::TypeCast(cast) => const_string(cast.arg.as_deref()?),
        _ => None,
    }
}

/// DDL whose target schema or relation is in `sys`.
fn targets_sys_schema(node: &NodeEnum) -> bool {
    let relation_in_sys = |r: &Option<RangeVar>| r.as_ref().is_some_and(is_sys);
    match node {
        NodeEnum::CreateSchemaStmt(s) => s.schemaname == "sys",
        NodeEnum::CreateStmt(s) => relation_in_sys(&s.relation),
        NodeEnum::IndexStmt(s) => relation_in_sys(&s.relation),
        NodeEnum::AlterTableStmt(s) => relation_in_sys(&s.relation),
        NodeEnum::CreateSeqStmt(s) => relation_in_sys(&s.sequence),
        NodeEnum::DropStmt(drop) => drop
            .objects
            .iter()
            .any(|object| match object.node.as_ref() {
                Some(NodeEnum::String(s)) => s.sval == "sys",
                Some(NodeEnum::List(list)) => {
                    list.items.len() >= 2 && list.items.first().and_then(name_part) == Some("sys")
                }
                _ => false,
            }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::statement::StatementPlan;

    /// (sql, rule id, SQLSTATE, message)
    pub(super) fn refusal(sql: &str) -> Option<(&'static str, &'static str, String)> {
        StatementPlan::new(sql)
            .refusal
            .map(|e| (e.rule, e.sqlstate, e.message))
    }

    pub(super) fn assert_refused(cases: &[(&str, &str, &str, &str)]) {
        for (sql, rule, state, message) in cases {
            assert_eq!(
                refusal(sql),
                Some((*rule, *state, (*message).to_owned())),
                "{sql}"
            );
        }
    }

    pub(super) fn assert_passes(sqls: &[&str]) {
        for sql in sqls {
            assert_eq!(refusal(sql), None, "{sql}");
        }
    }

    const EIGHT: &str = "a, b, c, d, e, f, g, h";

    #[test]
    fn index_rules() {
        assert_refused(&[
            (
                "CREATE INDEX CONCURRENTLY i ON t (c)",
                "index_concurrently",
                "0A000",
                "CONCURRENTLY not supported for CREATE INDEX",
            ),
            (
                "CREATE INDEX i ON t (c)",
                "index_sync",
                "0A000",
                "unsupported mode. please use CREATE INDEX ASYNC.",
            ),
            (
                "CREATE UNIQUE INDEX i ON t (c)",
                "index_sync",
                "0A000",
                "unsupported mode. please use CREATE INDEX ASYNC.",
            ),
            (
                "CREATE INDEX ON t (c)",
                "index_sync",
                "0A000",
                "unsupported mode. please use CREATE INDEX ASYNC.",
            ),
            (
                "CREATE INDEX ASYNC i ON t USING hash (c)",
                "index_method",
                "0A000",
                "USING not supported for CREATE INDEX",
            ),
            (
                "CREATE INDEX ASYNC i ON t USING gin (c)",
                "index_method",
                "0A000",
                "USING not supported for CREATE INDEX",
            ),
            (
                "CREATE INDEX ASYNC i ON t (a, b, c, d, e, f, g, h, i)",
                "index_key_columns",
                "54011",
                "more than 8 column keys in an index are not supported",
            ),
        ]);
    }

    #[test]
    fn index_statements_that_pass() {
        assert_passes(&[
            "CREATE INDEX ASYNC i ON t (c)",
            "CREATE UNIQUE INDEX ASYNC i ON t (c)",
            "CREATE INDEX ASYNC ON t (c)",
            "CREATE INDEX ASYNC IF NOT EXISTS i ON t (c)",
            "CREATE INDEX ASYNC i ON t USING btree (c)",
            "CREATE INDEX ASYNC i ON t USING BTREE (c)",
            &format!("CREATE INDEX ASYNC i ON t ({EIGHT})"),
            "CREATE INDEX ASYNC i ON t (c) INCLUDE (d) WHERE c > 0",
            "CREATE INDEX ASYNC i ON t ((lower(c)))",
        ]);
    }

    #[test]
    fn sys_rules() {
        assert_refused(&[
            (
                "SELECT sys.wait_for_job('x')",
                "wait_for_job_select",
                "42809",
                "sys.wait_for_job(text) is a procedure",
            ),
            (
                "SELECT * FROM t WHERE x = sys.wait_for_job($1)",
                "wait_for_job_select",
                "42809",
                "sys.wait_for_job(text) is a procedure",
            ),
            (
                "CALL sys.wait_for_job((SELECT job_id FROM sys.jobs LIMIT 1))",
                "wait_for_job_subquery",
                "0A000",
                "cannot use subquery in CALL argument",
            ),
            (
                "CALL sys.wait_for_job('no-such-job')",
                "wait_for_job_id",
                "22P02",
                "Unable to convert text to UUID",
            ),
            (
                "INSERT INTO sys.jobs (job_id) VALUES ('x')",
                "sys_write",
                "42501",
                "permission denied for table jobs",
            ),
            (
                "UPDATE sys.jobs SET status = 'x'",
                "sys_write",
                "42501",
                "permission denied for table jobs",
            ),
            (
                "DELETE FROM sys.jobs",
                "sys_write",
                "42501",
                "permission denied for table jobs",
            ),
            (
                "CREATE SCHEMA sys",
                "sys_ddl",
                "42501",
                "permission denied for schema sys",
            ),
            (
                "CREATE SCHEMA IF NOT EXISTS sys",
                "sys_ddl",
                "42501",
                "permission denied for schema sys",
            ),
            (
                "DROP SCHEMA sys",
                "sys_ddl",
                "42501",
                "permission denied for schema sys",
            ),
            (
                "DROP SCHEMA sys CASCADE",
                "sys_ddl",
                "42501",
                "permission denied for schema sys",
            ),
            (
                "CREATE TABLE sys.x (id int)",
                "sys_ddl",
                "42501",
                "permission denied for schema sys",
            ),
            (
                "DROP TABLE sys.jobs",
                "sys_ddl",
                "42501",
                "permission denied for schema sys",
            ),
            (
                "ALTER TABLE sys.jobs ADD COLUMN x int",
                "sys_ddl",
                "42501",
                "permission denied for schema sys",
            ),
            (
                "CREATE INDEX ASYNC i ON sys.jobs (status)",
                "sys_ddl",
                "42501",
                "permission denied for schema sys",
            ),
        ]);
        assert_passes(&[
            "SELECT * FROM sys.jobs",
            "CALL sys.wait_for_job('tpqrncdmjja4tdl3zxo2qqvh4y')",
            "CALL sys.wait_for_job($1)",
            "SELECT sys.dsql_major_version()",
            "CREATE SCHEMA other",
            "INSERT INTO jobs VALUES (1)",
            "SELECT 'sys.wait_for_job(x)'",
        ]);
    }
}
