//! The M3 deny rules for DDL (contracts/rule-catalogue-m3.md): index DDL, identity and
//! sequences, `ALTER TABLE` forms and the `sys` schema. Stateless, on the AST and the
//! pre-parse result; evaluated after 005's rules, first match wins.

use pg_query::protobuf::{AlterTableType, ConstrType, Constraint, Node, RangeVar, Token};
use pg_query::NodeEnum;

use crate::errors::{self, PolicyError};
use crate::identity;
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
        id: "identity_type",
        check: |node, _, _| {
            identity_columns(node).find_map(|(column, _)| {
                let ty = column.type_name.as_ref()?;
                let name = identity::type_display(ty);
                (name != "bigint").then(|| errors::identity_type(&name))
            })
        },
    },
    DdlRule {
        id: "identity_cache_missing",
        check: |node, _, _| {
            identity_columns(node)
                .any(|(_, c)| identity::seq_option(&c.options, "cache").is_none())
                .then(errors::identity_cache_missing)
        },
    },
    DdlRule {
        id: "sequence_cache_missing",
        check: |node, _, _| match node {
            NodeEnum::CreateSeqStmt(s) if identity::seq_option(&s.options, "cache").is_none() => {
                Some(errors::sequence_cache_missing())
            }
            _ => None,
        },
    },
    DdlRule {
        id: "cache_value",
        check: |node, _, _| {
            let identity_options: Vec<&[Node]> = identity_columns(node)
                .map(|(_, c)| c.options.as_slice())
                .collect();
            let options: Vec<&[Node]> = match node {
                NodeEnum::CreateSeqStmt(s) => vec![&s.options],
                NodeEnum::AlterSeqStmt(s) => vec![&s.options],
                _ => identity_options,
            };
            options
                .into_iter()
                .filter_map(|o| identity::seq_option(o, "cache"))
                .filter_map(identity::numeric_text)
                .filter_map(|v| v.parse::<i128>().ok())
                .any(|v| v != 1 && v < 65536)
                .then(errors::cache_value)
        },
    },
    DdlRule {
        id: "add_column_constraint",
        check: |node, _, _| {
            alter_cmds(node)
                .filter(|cmd| cmd.subtype == AlterTableType::AtAddColumn as i32)
                .filter_map(|cmd| match cmd.def.as_ref()?.node.as_ref()? {
                    NodeEnum::ColumnDef(column) => Some(&**column),
                    _ => None,
                })
                .any(|column| !column.constraints.is_empty() || column.raw_default.is_some())
                .then(errors::add_column_constraint)
        },
    },
    DdlRule {
        id: "alter_column_type",
        check: |node, _, _| {
            alter_cmds(node)
                .any(|cmd| cmd.subtype == AlterTableType::AtAlterColumnType as i32)
                .then(errors::alter_column_type)
        },
    },
    DdlRule {
        id: "set_not_null",
        check: |node, _, _| {
            alter_cmds(node)
                .any(|cmd| cmd.subtype == AlterTableType::AtSetNotNull as i32)
                .then(errors::set_not_null)
        },
    },
    DdlRule {
        id: "add_constraint",
        check: |node, _, _| {
            alter_cmds(node)
                .filter(|cmd| cmd.subtype == AlterTableType::AtAddConstraint as i32)
                .filter_map(|cmd| constraint(cmd))
                .any(|c| !is_not_valid_fk_or_check(c))
                .then(errors::add_constraint)
        },
    },
    DdlRule {
        id: "add_constraint_form",
        check: |node, _, _| {
            alter_cmds(node)
                .filter(|cmd| cmd.subtype == AlterTableType::AtAddConstraint as i32)
                .filter_map(|cmd| constraint(cmd))
                .find_map(|c| not_valid_form(c).map(errors::add_constraint_form))
        },
    },
    DdlRule {
        id: "validate_sync",
        check: |node, _, ctx| {
            (ctx.async_kind != Some(AsyncKind::AlterTable)
                && alter_cmds(node)
                    .any(|cmd| cmd.subtype == AlterTableType::AtValidateConstraint as i32))
            .then(errors::validate_sync)
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

/// The identity columns of a `CREATE TABLE` with their identity constraint.
fn identity_columns(
    node: &NodeEnum,
) -> impl Iterator<
    Item = (
        &pg_query::protobuf::ColumnDef,
        &pg_query::protobuf::Constraint,
    ),
> {
    let elements: &[Node] = match node {
        NodeEnum::CreateStmt(create) => &create.table_elts,
        _ => &[],
    };
    elements
        .iter()
        .filter_map(|element| match element.node.as_ref()? {
            NodeEnum::ColumnDef(column) => {
                Some((&**column, identity::identity_constraint(column)?))
            }
            _ => None,
        })
}

/// The commands of an `ALTER TABLE`.
pub(crate) fn alter_cmds(
    node: &NodeEnum,
) -> impl Iterator<Item = &pg_query::protobuf::AlterTableCmd> {
    let cmds: &[Node] = match node {
        NodeEnum::AlterTableStmt(alter) => &alter.cmds,
        _ => &[],
    };
    cmds.iter().filter_map(|n| match n.node.as_ref()? {
        NodeEnum::AlterTableCmd(cmd) => Some(&**cmd),
        _ => None,
    })
}

/// The constraint of an `ADD CONSTRAINT` command.
pub(crate) fn constraint(cmd: &pg_query::protobuf::AlterTableCmd) -> Option<&Constraint> {
    match cmd.def.as_ref()?.node.as_ref()? {
        NodeEnum::Constraint(c) => Some(&**c),
        _ => None,
    }
}

/// A foreign key or CHECK declared `NOT VALID`: the only `ADD CONSTRAINT` DSQL has.
fn is_not_valid_fk_or_check(c: &Constraint) -> bool {
    c.skip_validation
        && (c.contype == ConstrType::ConstrForeign as i32
            || c.contype == ConstrType::ConstrCheck as i32)
}

/// The columns a CHECK expression names.
pub(crate) fn check_columns(c: &Constraint) -> Vec<String> {
    let mut columns: Vec<String> = Vec::new();
    if let Some(expr) = c.raw_expr.as_ref().and_then(|e| e.node.as_ref()) {
        for (node, ..) in expr.nodes() {
            if let pg_query::NodeRef::ColumnRef(column) = node {
                let name = column
                    .fields
                    .iter()
                    .filter_map(name_part)
                    .collect::<Vec<_>>()
                    .join(".");
                if !columns.contains(&name) {
                    columns.push(name);
                }
            }
        }
    }
    columns
}

/// The `NOT VALID` forms micro-dsql cannot enforce (`None`: fine, or not a `NOT VALID` form).
fn not_valid_form(c: &Constraint) -> Option<&'static str> {
    if !is_not_valid_fk_or_check(c) {
        return None;
    }
    if c.contype == ConstrType::ConstrForeign as i32 {
        if c.fk_attrs.len() != 1 {
            return Some("multi-column foreign key");
        }
        if c.deferrable || c.initdeferred {
            return Some("deferrable foreign key");
        }
        if c.fk_matchtype == "f" {
            return Some("MATCH FULL foreign key");
        }
        None
    } else if check_columns(c).len() != 1 {
        Some("multi-column check")
    } else {
        None
    }
}

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

    const NEED_CACHE: &str = "please define CACHE greater than or equal to 65536 or equal to 1";

    #[test]
    fn identity_and_sequence_rules() {
        let identity_cache = format!(
            "identity column is not supported without an explicit cache size. {NEED_CACHE}"
        );
        let sequence_cache = format!(
            "CREATE SEQUENCE is not supported without an explicit cache size. {NEED_CACHE}"
        );
        assert_refused(&[
            (
                "CREATE TABLE t (id integer GENERATED BY DEFAULT AS IDENTITY (CACHE 1))",
                "identity_type",
                "0A000",
                "datatype integer not supported, identity column type must be bigint",
            ),
            (
                "CREATE TABLE t (id smallint GENERATED ALWAYS AS IDENTITY (CACHE 65536))",
                "identity_type",
                "0A000",
                "datatype smallint not supported, identity column type must be bigint",
            ),
            (
                "CREATE TABLE t (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY)",
                "identity_type",
                "0A000",
                "datatype integer not supported, identity column type must be bigint",
            ),
            (
                "CREATE TABLE t (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY)",
                "identity_cache_missing",
                "0A000",
                &identity_cache,
            ),
            (
                "CREATE TABLE t (id bigint GENERATED BY DEFAULT AS IDENTITY (START WITH 5))",
                "identity_cache_missing",
                "0A000",
                &identity_cache,
            ),
            (
                "CREATE SEQUENCE s",
                "sequence_cache_missing",
                "0A000",
                &sequence_cache,
            ),
            (
                "CREATE SEQUENCE s INCREMENT BY 2 START 10",
                "sequence_cache_missing",
                "0A000",
                &sequence_cache,
            ),
        ]);
        for cache in ["2", "100", "65535", "0", "-1"] {
            for sql in [
                format!("CREATE SEQUENCE s CACHE {cache}"),
                format!("ALTER SEQUENCE s CACHE {cache}"),
                format!(
                    "CREATE TABLE t (id bigint GENERATED BY DEFAULT AS IDENTITY (CACHE {cache}))"
                ),
            ] {
                assert_eq!(
                    refusal(&sql),
                    Some((
                        "cache_value",
                        "0A000",
                        "CACHE must be 1 or at least 65536".to_owned()
                    )),
                    "{sql}"
                );
            }
        }
        assert_passes(&[
            "CREATE SEQUENCE s CACHE 1",
            "CREATE SEQUENCE s CACHE 65536",
            "CREATE SEQUENCE IF NOT EXISTS s INCREMENT 2 CACHE 1000000",
            "ALTER SEQUENCE s CACHE 65536",
            "ALTER SEQUENCE s CACHE 1",
            "ALTER SEQUENCE s RESTART",
            "ALTER SEQUENCE s INCREMENT BY 3",
            "CREATE TABLE t (id bigint GENERATED BY DEFAULT AS IDENTITY (CACHE 1) PRIMARY KEY)",
            "CREATE TABLE t (id bigint GENERATED ALWAYS AS IDENTITY (CACHE 65536))",
            "CREATE TABLE t (id bigint GENERATED BY DEFAULT AS IDENTITY (sequence name \"t_id_seq\" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 65536) PRIMARY KEY)",
            "CREATE TABLE t (id bigint PRIMARY KEY, n int)",
        ]);
    }

    const ADD_COLUMN: &str = "ALTER TABLE ADD COLUMN with constraint not supported";

    #[test]
    fn alter_table_rules() {
        let mut cases: Vec<(String, &str, &str, String)> = Vec::new();
        for column in [
            "d int DEFAULT 0",
            "d int NOT NULL",
            "d int NULL",
            "d int CHECK (d > 0)",
            "d int UNIQUE",
            "d int PRIMARY KEY",
            "d int REFERENCES p (id)",
            "d bigint GENERATED ALWAYS AS IDENTITY (CACHE 1)",
            "d int GENERATED ALWAYS AS (1) STORED",
            "d text NOT NULL DEFAULT 'x'",
        ] {
            cases.push((
                format!("ALTER TABLE t ADD COLUMN {column}"),
                "add_column_constraint",
                "0A000",
                ADD_COLUMN.to_owned(),
            ));
        }
        cases.push((
            "ALTER TABLE t ADD COLUMN IF NOT EXISTS d int DEFAULT 0".to_owned(),
            "add_column_constraint",
            "0A000",
            ADD_COLUMN.to_owned(),
        ));
        for sql in [
            "ALTER TABLE t ALTER COLUMN w TYPE bigint",
            "ALTER TABLE t ALTER COLUMN w SET DATA TYPE text USING w::text",
        ] {
            cases.push((
                sql.to_owned(),
                "alter_column_type",
                "0A000",
                "unsupported ALTER TABLE ALTER COLUMN ... SET DATA TYPE statement".to_owned(),
            ));
        }
        cases.push((
            "ALTER TABLE t ALTER COLUMN w SET NOT NULL".to_owned(),
            "set_not_null",
            "0A000",
            "SET NOT NULL is not supported (only DROP NOT NULL)".to_owned(),
        ));
        for sql in [
            "ALTER TABLE t ADD CONSTRAINT c FOREIGN KEY (a) REFERENCES p (id)",
            "ALTER TABLE t ADD CONSTRAINT c CHECK (a > 0)",
            "ALTER TABLE t ADD CONSTRAINT c PRIMARY KEY (a)",
            "ALTER TABLE t ADD PRIMARY KEY (a)",
            "ALTER TABLE t ADD CONSTRAINT c UNIQUE (a)",
            "ALTER TABLE t ADD CONSTRAINT c EXCLUDE USING gist (a WITH =)",
            "ALTER TABLE t ADD UNIQUE USING INDEX i",
        ] {
            cases.push((
                sql.to_owned(),
                "add_constraint",
                "0A000",
                "unsupported ALTER TABLE ADD CONSTRAINT statement".to_owned(),
            ));
        }
        for (sql, form) in [
            (
                "ALTER TABLE t ADD CONSTRAINT c FOREIGN KEY (a, b) REFERENCES p (x, y) NOT VALID",
                "multi-column foreign key",
            ),
            (
                "ALTER TABLE t ADD CONSTRAINT c FOREIGN KEY (a) REFERENCES p (id) DEFERRABLE NOT VALID",
                "deferrable foreign key",
            ),
            (
                "ALTER TABLE t ADD CONSTRAINT c FOREIGN KEY (a) REFERENCES p (id) DEFERRABLE INITIALLY DEFERRED NOT VALID",
                "deferrable foreign key",
            ),
            (
                "ALTER TABLE t ADD CONSTRAINT c FOREIGN KEY (a) REFERENCES p (id) MATCH FULL NOT VALID",
                "MATCH FULL foreign key",
            ),
            (
                "ALTER TABLE t ADD CONSTRAINT c CHECK (a > b) NOT VALID",
                "multi-column check",
            ),
            (
                "ALTER TABLE t ADD CONSTRAINT c CHECK (true) NOT VALID",
                "multi-column check",
            ),
        ] {
            cases.push((
                sql.to_owned(),
                "add_constraint_form",
                "0A000",
                format!("unsupported NOT VALID constraint form: {form}"),
            ));
        }
        for sql in [
            "ALTER TABLE t VALIDATE CONSTRAINT c",
            "ALTER TABLE IF EXISTS t VALIDATE CONSTRAINT c",
        ] {
            cases.push((
                sql.to_owned(),
                "validate_sync",
                "0A000",
                "unsupported ALTER TABLE VALIDATE CONSTRAINT statement".to_owned(),
            ));
        }
        for (sql, rule, state, message) in &cases {
            assert_eq!(
                refusal(sql),
                Some((*rule, *state, message.clone())),
                "{sql}"
            );
        }
    }

    #[test]
    fn alter_table_statements_that_pass() {
        assert_passes(&[
            "ALTER TABLE t ADD COLUMN c text",
            "ALTER TABLE t ADD COLUMN IF NOT EXISTS c numeric",
            "ALTER TABLE t ADD COLUMN c numeric(10, 2), ADD COLUMN d jsonb",
            "ALTER TABLE t DROP COLUMN c",
            "ALTER TABLE t RENAME COLUMN a TO b",
            "ALTER TABLE t RENAME TO u",
            "ALTER TABLE t ALTER COLUMN w DROP NOT NULL",
            "ALTER TABLE t ADD CONSTRAINT c FOREIGN KEY (a) REFERENCES p (id) NOT VALID",
            "ALTER TABLE t ADD CONSTRAINT c FOREIGN KEY (a) REFERENCES p (id) ON DELETE CASCADE ON UPDATE no action NOT VALID",
            "ALTER TABLE t ADD CONSTRAINT c CHECK (a > 0) NOT VALID",
            "ALTER TABLE t ADD CONSTRAINT c CHECK (a IS NOT NULL AND a < 10) NOT VALID",
            "ALTER TABLE ASYNC t VALIDATE CONSTRAINT c",
            "ALTER TABLE DROP_ME DROP COLUMN pk_col",
        ]);
    }
}
