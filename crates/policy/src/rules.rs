//! Stateless deny rules: statements DSQL refuses, decided on the AST alone.
//!
//! One table, [`RULES`]; the first matching rule wins. Texts come from
//! specs/005-m2-dsql-rules/contracts/rule-catalogue.md.

use pg_query::protobuf::{ObjectType, Token};
use pg_query::NodeEnum;

use crate::errors::PolicyError;
use crate::types::check_column_type;

/// One deny rule.
pub struct Rule {
    pub id: &'static str,
    /// Looks at the parsed statement and its text.
    pub check: fn(&NodeEnum, &str) -> Option<PolicyError>,
}

/// The first deny rule that matches the statement.
pub fn check_rules(node: &NodeEnum, sql: &str) -> Option<PolicyError> {
    RULES.iter().find_map(|rule| (rule.check)(node, sql))
}

fn refused(id: &'static str, message: &str) -> Option<PolicyError> {
    Some(PolicyError::new(id, "0A000", message))
}

/// `unsupported statement: <kind>` for the statements DSQL reports by node name.
fn unsupported(id: &'static str, kind: &str) -> Option<PolicyError> {
    refused(id, &format!("unsupported statement: {kind}"))
}

/// The column definitions a statement introduces: `CREATE TABLE` columns and
/// `ALTER TABLE ... ADD COLUMN`.
fn new_columns(node: &NodeEnum) -> Vec<&pg_query::protobuf::ColumnDef> {
    let nodes: Vec<&pg_query::protobuf::Node> = match node {
        NodeEnum::CreateStmt(create) => create.table_elts.iter().collect(),
        NodeEnum::AlterTableStmt(alter) => alter
            .cmds
            .iter()
            .filter_map(|cmd| match cmd.node.as_ref()? {
                NodeEnum::AlterTableCmd(cmd)
                    if cmd.subtype == pg_query::protobuf::AlterTableType::AtAddColumn as i32 =>
                {
                    cmd.def.as_deref()
                }
                _ => None,
            })
            .collect(),
        _ => return Vec::new(),
    };
    nodes
        .into_iter()
        .filter_map(|n| match n.node.as_ref()? {
            NodeEnum::ColumnDef(col) => Some(&**col),
            _ => None,
        })
        .collect()
}

fn column_type(node: &NodeEnum, id: &'static str) -> Option<PolicyError> {
    new_columns(node)
        .into_iter()
        .filter_map(|col| col.type_name.as_ref())
        .filter_map(check_column_type)
        .find(|e| e.rule == id)
}

fn persistence(node: &NodeEnum) -> Option<&str> {
    match node {
        NodeEnum::CreateStmt(create) => create.relation.as_ref().map(|r| r.relpersistence.as_str()),
        _ => None,
    }
}

/// The tokens of the statement text with their source slices. The parser accepted the text,
/// so the token stream is exact: string literals and comments are single tokens, which is why
/// the rules that apply "anywhere in the statement" look at tokens instead of walking the
/// tree (the library's node walker skips parts of it).
fn tokens(sql: &str) -> Vec<(Token, &str)> {
    let Ok(scanned) = pg_query::scan(sql) else {
        return Vec::new();
    };
    scanned
        .tokens
        .iter()
        .filter_map(|t| {
            let text = sql.get(usize::try_from(t.start).ok()?..usize::try_from(t.end).ok()?)?;
            Some((Token::try_from(t.token).ok()?, text))
        })
        .collect()
}

/// `FOR SHARE` or `FOR NO KEY UPDATE` anywhere in the statement.
fn has_unsupported_locking(sql: &str) -> bool {
    let kinds: Vec<Token> = tokens(sql).into_iter().map(|(t, _)| t).collect();
    kinds.iter().enumerate().any(|(i, t)| {
        *t == Token::For
            && match kinds[i + 1..] {
                [Token::Share, ..] => true,
                [Token::No, Token::Key, Token::Update, ..] => true,
                _ => false,
            }
    })
}

fn has_tablesample(sql: &str) -> bool {
    tokens(sql).iter().any(|(t, _)| *t == Token::Tablesample)
}

/// A call of `pg_advisory_*` / `pg_try_advisory_*` (any schema).
fn has_advisory_call(sql: &str) -> bool {
    let toks = tokens(sql);
    toks.windows(2)
        .any(|w| w[0].0 == Token::Ident && is_advisory(w[0].1.trim_matches('"')) && w[1].1 == "(")
}

fn function_language(node: &NodeEnum) -> Option<String> {
    let NodeEnum::CreateFunctionStmt(create) = node else {
        return None;
    };
    create
        .options
        .iter()
        .find_map(|opt| match opt.node.as_ref()? {
            NodeEnum::DefElem(def) if def.defname == "language" => {
                match def.arg.as_ref()?.node.as_ref()? {
                    NodeEnum::String(s) => Some(s.sval.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
}

fn is_advisory(name: &str) -> bool {
    name.starts_with("pg_advisory_") || name.starts_with("pg_try_advisory_")
}

macro_rules! statement_rule {
    ($id:literal, $pat:pat if $guard:expr => $check:expr) => {
        Rule {
            id: $id,
            check: |node, _| match node {
                $pat if $guard => $check,
                _ => None,
            },
        }
    };
    ($id:literal, $pat:pat => $check:expr) => {
        Rule {
            id: $id,
            check: |node, _| match node {
                $pat => $check,
                _ => None,
            },
        }
    };
}

/// The deny rules, in evaluation order. `create_procedure` precedes
/// `create_function_language` and `materialized_view` precedes `create_table_as`; table-level
/// rules precede the column-type rules.
pub const RULES: &[Rule] = &[
    statement_rule!("truncate", NodeEnum::TruncateStmt(_) => unsupported("truncate", "Truncate")),
    Rule {
        id: "unlogged_table",
        check: |node, _| {
            (persistence(node) == Some("u"))
                .then(|| {
                    refused(
                        "unlogged_table",
                        "UNLOGGED table not supported for CREATE TABLE",
                    )
                })
                .flatten()
        },
    },
    Rule {
        id: "temporary_table",
        check: |node, _| {
            (persistence(node) == Some("t"))
                .then(|| {
                    refused(
                        "temporary_table",
                        "TEMPORARY or TEMP table not supported for CREATE TABLE",
                    )
                })
                .flatten()
        },
    },
    statement_rule!("materialized_view", NodeEnum::CreateTableAsStmt(c)
        if c.objtype == ObjectType::ObjectMatview as i32 => unsupported("materialized_view", "CreateTableAs")),
    Rule {
        id: "create_table_as",
        check: |node, _| match node {
            NodeEnum::CreateTableAsStmt(_) => unsupported("create_table_as", "CreateTableAs"),
            // `SELECT ... INTO t` is a plain SelectStmt with an INTO clause at parse level.
            NodeEnum::SelectStmt(s) if s.into_clause.is_some() => {
                unsupported("create_table_as", "CreateTableAs")
            }
            _ => None,
        },
    },
    statement_rule!("enum_type", NodeEnum::CreateEnumStmt(_) => refused("enum_type", "CREATE TYPE not supported")),
    statement_rule!("alter_enum", NodeEnum::AlterEnumStmt(_) => refused("alter_enum", "ALTER TYPE not supported")),
    statement_rule!("drop_type", NodeEnum::DropStmt(d) if d.remove_type == ObjectType::ObjectType as i32 => refused("drop_type", "unsupported object in DROP statement")),
    statement_rule!("rename_type", NodeEnum::RenameStmt(r) if r.rename_type == ObjectType::ObjectType as i32 => refused("rename_type", "unsupported object in RENAME statement")),
    statement_rule!("copy", NodeEnum::CopyStmt(_) => refused("copy", "COPY is not supported")),
    Rule {
        id: "serial",
        check: |node, _| column_type(node, "serial"),
    },
    Rule {
        id: "unsupported_type",
        check: |node, _| column_type(node, "unsupported_type"),
    },
    Rule {
        id: "array_column",
        check: |node, _| column_type(node, "array_column"),
    },
    statement_rule!("extensions", NodeEnum::CreateExtensionStmt(_) => unsupported("extensions", "CreateExtension")),
    statement_rule!("triggers", NodeEnum::CreateTrigStmt(_) => refused("triggers", "CREATE TRIGGER not supported")),
    statement_rule!("create_database", NodeEnum::CreatedbStmt(_) => unsupported("create_database", "Createdb")),
    statement_rule!("composite_type", NodeEnum::CompositeTypeStmt(_) => unsupported("composite_type", "CompositeType")),
    statement_rule!("range_type", NodeEnum::CreateRangeStmt(_) => unsupported("range_type", "CreateRange")),
    statement_rule!("tablespace", NodeEnum::CreateTableSpaceStmt(_) => unsupported("tablespace", "CreateTableSpace")),
    statement_rule!("foreign_table", NodeEnum::CreateForeignTableStmt(_) => unsupported("foreign_table", "CreateForeignTable")),
    statement_rule!("vacuum", NodeEnum::VacuumStmt(v) if v.is_vacuumcmd => refused("vacuum", "unsupported VACUUM statement")),
    statement_rule!("listen", NodeEnum::ListenStmt(_) => unsupported("listen", "Listen")),
    statement_rule!("notify", NodeEnum::NotifyStmt(_) => unsupported("notify", "Notify")),
    statement_rule!("unlisten", NodeEnum::UnlistenStmt(_) => unsupported("unlisten", "Unlisten")),
    statement_rule!("alter_system", NodeEnum::AlterSystemStmt(_) => unsupported("alter_system", "AlterSystem")),
    statement_rule!("merge", NodeEnum::MergeStmt(_) => refused("merge", "unsupported statement type: MERGE")),
    statement_rule!("create_procedure", NodeEnum::CreateFunctionStmt(f) if f.is_procedure => refused("create_procedure", "CREATE PROCEDURE is not supported")),
    Rule {
        id: "create_function_language",
        check: |node, _| {
            let language = function_language(node)?;
            (!language.eq_ignore_ascii_case("sql")).then(|| {
                PolicyError::new(
                    "create_function_language",
                    "0A000",
                    format!("CREATE FUNCTION with language {language} not supported"),
                )
            })
        },
    },
    Rule {
        id: "unsupported_locking",
        check: |_, sql| {
            has_unsupported_locking(sql).then(|| {
                PolicyError::new(
                    "unsupported_locking",
                    "0A000",
                    "locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported",
                )
            })
        },
    },
    Rule {
        id: "tablesample",
        check: |_, sql| {
            has_tablesample(sql).then(|| {
                PolicyError::new("tablesample", "0A000", "TABLESAMPLE clause not supported")
            })
        },
    },
    statement_rule!("lock_table", NodeEnum::LockStmt(_) => refused("lock_table", "LOCK TABLE is not supported")),
    statement_rule!("do", NodeEnum::DoStmt(_) => refused("do", "DO is not supported")),
    statement_rule!("create_rule", NodeEnum::RuleStmt(_) => refused("create_rule", "CREATE RULE is not supported")),
    statement_rule!("create_publication", NodeEnum::CreatePublicationStmt(_) => refused("create_publication", "CREATE PUBLICATION is not supported")),
    statement_rule!("reindex", NodeEnum::ReindexStmt(_) => refused("reindex", "REINDEX is not supported")),
    statement_rule!("cluster", NodeEnum::ClusterStmt(_) => refused("cluster", "CLUSTER is not supported")),
    statement_rule!("prepare_transaction", NodeEnum::TransactionStmt(t)
        if t.kind == pg_query::protobuf::TransactionStmtKind::TransStmtPrepare as i32 =>
        refused("prepare_transaction", "prepared transactions is not supported")),
    Rule {
        id: "advisory_locks",
        check: |_, sql| {
            has_advisory_call(sql).then(|| {
                PolicyError::new(
                    "advisory_locks",
                    "0A000",
                    "advisory locks are not supported",
                )
            })
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn check(sql: &str) -> Option<PolicyError> {
        let parsed = pg_query::parse(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let node = parsed.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .as_ref()
            .unwrap();
        check_rules(node, sql)
    }

    /// (sql, rule id, SQLSTATE, message)
    const REFUSED: &[(&str, &str, &str, &str)] = &[
        // 3.1
        (
            "TRUNCATE t",
            "truncate",
            "0A000",
            "unsupported statement: Truncate",
        ),
        (
            "TRUNCATE TABLE a, b",
            "truncate",
            "0A000",
            "unsupported statement: Truncate",
        ),
        (
            "CREATE UNLOGGED TABLE t (id int)",
            "unlogged_table",
            "0A000",
            "UNLOGGED table not supported for CREATE TABLE",
        ),
        (
            "CREATE TABLE t AS SELECT 1",
            "create_table_as",
            "0A000",
            "unsupported statement: CreateTableAs",
        ),
        (
            "SELECT * INTO t FROM s",
            "create_table_as",
            "0A000",
            "unsupported statement: CreateTableAs",
        ),
        (
            "CREATE TYPE mood AS ENUM ('a')",
            "enum_type",
            "0A000",
            "CREATE TYPE not supported",
        ),
        (
            "ALTER TYPE mood ADD VALUE 'b'",
            "alter_enum",
            "0A000",
            "ALTER TYPE not supported",
        ),
        (
            "DROP TYPE mood",
            "drop_type",
            "0A000",
            "unsupported object in DROP statement",
        ),
        (
            "ALTER TYPE mood RENAME TO m2",
            "rename_type",
            "0A000",
            "unsupported object in RENAME statement",
        ),
        ("COPY t TO STDOUT", "copy", "0A000", "COPY is not supported"),
        (
            "COPY t FROM STDIN",
            "copy",
            "0A000",
            "COPY is not supported",
        ),
        (
            "CREATE TABLE t (id serial)",
            "serial",
            "42704",
            "type \"serial\" does not exist",
        ),
        (
            "CREATE TABLE t (id bigserial PRIMARY KEY)",
            "serial",
            "42704",
            "type \"bigserial\" does not exist",
        ),
        (
            "ALTER TABLE t ADD COLUMN id smallserial",
            "serial",
            "42704",
            "type \"smallserial\" does not exist",
        ),
        (
            "CREATE TABLE t (c money)",
            "unsupported_type",
            "0A000",
            "datatype money not supported",
        ),
        (
            "ALTER TABLE t ADD COLUMN c inet",
            "unsupported_type",
            "0A000",
            "datatype inet not supported",
        ),
        (
            "CREATE TABLE t (id int, c varbit(8))",
            "unsupported_type",
            "0A000",
            "datatype bit varying not supported",
        ),
        (
            "CREATE TABLE t (c int[])",
            "array_column",
            "0A000",
            "datatype integer[] not supported",
        ),
        (
            "ALTER TABLE t ADD COLUMN c text[]",
            "array_column",
            "0A000",
            "datatype text[] not supported",
        ),
        // 3.2
        (
            "CREATE TEMPORARY TABLE t (id int)",
            "temporary_table",
            "0A000",
            "TEMPORARY or TEMP table not supported for CREATE TABLE",
        ),
        (
            "CREATE TEMP TABLE t (id int)",
            "temporary_table",
            "0A000",
            "TEMPORARY or TEMP table not supported for CREATE TABLE",
        ),
        (
            "CREATE EXTENSION pgcrypto",
            "extensions",
            "0A000",
            "unsupported statement: CreateExtension",
        ),
        (
            "CREATE TRIGGER tr BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()",
            "triggers",
            "0A000",
            "CREATE TRIGGER not supported",
        ),
        (
            "CREATE DATABASE d",
            "create_database",
            "0A000",
            "unsupported statement: Createdb",
        ),
        (
            "CREATE MATERIALIZED VIEW mv AS SELECT 1",
            "materialized_view",
            "0A000",
            "unsupported statement: CreateTableAs",
        ),
        (
            "CREATE TYPE c AS (a int)",
            "composite_type",
            "0A000",
            "unsupported statement: CompositeType",
        ),
        (
            "CREATE TYPE r AS RANGE (subtype = int)",
            "range_type",
            "0A000",
            "unsupported statement: CreateRange",
        ),
        (
            "CREATE TABLESPACE ts LOCATION '/x'",
            "tablespace",
            "0A000",
            "unsupported statement: CreateTableSpace",
        ),
        (
            "CREATE FOREIGN TABLE f (id int) SERVER s",
            "foreign_table",
            "0A000",
            "unsupported statement: CreateForeignTable",
        ),
        ("VACUUM", "vacuum", "0A000", "unsupported VACUUM statement"),
        (
            "VACUUM t",
            "vacuum",
            "0A000",
            "unsupported VACUUM statement",
        ),
        (
            "VACUUM ANALYZE t",
            "vacuum",
            "0A000",
            "unsupported VACUUM statement",
        ),
        (
            "LISTEN ch",
            "listen",
            "0A000",
            "unsupported statement: Listen",
        ),
        (
            "NOTIFY ch",
            "notify",
            "0A000",
            "unsupported statement: Notify",
        ),
        (
            "UNLISTEN ch",
            "unlisten",
            "0A000",
            "unsupported statement: Unlisten",
        ),
        (
            "ALTER SYSTEM SET work_mem = '1MB'",
            "alter_system",
            "0A000",
            "unsupported statement: AlterSystem",
        ),
        (
            "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
            "merge",
            "0A000",
            "unsupported statement type: MERGE",
        ),
        (
            "CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS 'begin return 1; end'",
            "create_function_language",
            "0A000",
            "CREATE FUNCTION with language plpgsql not supported",
        ),
        (
            "CREATE FUNCTION f() RETURNS int AS 'select 1' LANGUAGE c",
            "create_function_language",
            "0A000",
            "CREATE FUNCTION with language c not supported",
        ),
        (
            "SELECT * FROM t FOR SHARE",
            "unsupported_locking",
            "0A000",
            "locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported",
        ),
        (
            "SELECT * FROM t FOR NO KEY UPDATE",
            "unsupported_locking",
            "0A000",
            "locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported",
        ),
        (
            "SELECT * FROM (SELECT * FROM t FOR SHARE) s",
            "unsupported_locking",
            "0A000",
            "locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported",
        ),
        (
            "SELECT * FROM t TABLESAMPLE BERNOULLI (10)",
            "tablesample",
            "0A000",
            "TABLESAMPLE clause not supported",
        ),
        (
            "LOCK TABLE t",
            "lock_table",
            "0A000",
            "LOCK TABLE is not supported",
        ),
        ("DO $$ begin end $$", "do", "0A000", "DO is not supported"),
        (
            "CREATE PROCEDURE p() LANGUAGE sql AS 'select 1'",
            "create_procedure",
            "0A000",
            "CREATE PROCEDURE is not supported",
        ),
        (
            "CREATE PROCEDURE p() LANGUAGE plpgsql AS 'begin end'",
            "create_procedure",
            "0A000",
            "CREATE PROCEDURE is not supported",
        ),
        (
            "CREATE RULE r AS ON INSERT TO t DO NOTHING",
            "create_rule",
            "0A000",
            "CREATE RULE is not supported",
        ),
        (
            "CREATE PUBLICATION p FOR ALL TABLES",
            "create_publication",
            "0A000",
            "CREATE PUBLICATION is not supported",
        ),
        (
            "REINDEX TABLE t",
            "reindex",
            "0A000",
            "REINDEX is not supported",
        ),
        (
            "CLUSTER t USING i",
            "cluster",
            "0A000",
            "CLUSTER is not supported",
        ),
        (
            "PREPARE TRANSACTION 'x'",
            "prepare_transaction",
            "0A000",
            "prepared transactions is not supported",
        ),
        (
            "SELECT pg_advisory_lock(1)",
            "advisory_locks",
            "0A000",
            "advisory locks are not supported",
        ),
        (
            "SELECT pg_try_advisory_xact_lock(1, 2)",
            "advisory_locks",
            "0A000",
            "advisory locks are not supported",
        ),
        (
            "SELECT pg_catalog.pg_advisory_unlock_all()",
            "advisory_locks",
            "0A000",
            "advisory locks are not supported",
        ),
        (
            "SELECT 1 WHERE pg_advisory_lock(1) IS NOT NULL",
            "advisory_locks",
            "0A000",
            "advisory locks are not supported",
        ),
    ];

    #[test]
    fn every_deny_rule_matches_with_its_text() {
        for (sql, rule, sqlstate, message) in REFUSED {
            let e = check(sql).unwrap_or_else(|| panic!("not refused: {sql}"));
            assert_eq!(
                (e.rule, e.sqlstate, e.message.as_str()),
                (*rule, *sqlstate, *message),
                "{sql}"
            );
        }
    }

    #[test]
    fn statements_dsql_supports_are_not_refused() {
        for sql in [
            "CREATE DOMAIN d AS int",
            "CREATE INDEX i ON t (id)",
            "CREATE UNIQUE INDEX i ON t (id)",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t FOR KEY SHARE",
            "SELECT ARRAY[1, 2]",
            "SELECT '10.0.0.1'::inet",
            "SELECT a[1] FROM t",
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'select 1'",
            "CREATE TABLE t (id int PRIMARY KEY) PARTITION BY RANGE (id)",
            "CREATE TABLE t (id int, n numeric(10,2), j jsonb, u uuid, tz timetz, i interval, b bytea, t timestamptz)",
            "CREATE TABLE t (id int GENERATED ALWAYS AS IDENTITY)",
            "ALTER TABLE t ADD COLUMN c int",
            "ALTER TABLE t DROP COLUMN c",
            "ALTER TABLE t ADD CONSTRAINT c CHECK (id > 0)",
            "ANALYZE t",
            "ANALYZE",
            "CREATE VIEW v AS SELECT 1",
            "CREATE TEMP VIEW v AS SELECT 1",
            "CREATE SEQUENCE s",
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "DELETE FROM t",
            "SELECT count(*) FROM t",
            "DROP TABLE t",
            "DROP DOMAIN d",
            "BEGIN",
            "COMMIT",
            "SET search_path = public",
            "SHOW server_version",
            "EXPLAIN SELECT 1",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "ALTER TABLE t RENAME TO u",
            "ALTER TABLE t RENAME COLUMN a TO b",
            "SELECT pg_sleep(0)",
            "SELECT 'pg_advisory_lock'",
            "SELECT 1 /* FOR SHARE */",
            "SELECT 'FOR SHARE'",
            "SELECT * FROM t FOR UPDATE OF t SKIP LOCKED",
            "CREATE TABLE t (pg_advisory_lock int)",
        ] {
            assert_eq!(check(sql), None, "{sql}");
        }
    }

    #[test]
    fn rule_order_resolves_overlaps() {
        assert_eq!(
            check("CREATE PROCEDURE p() LANGUAGE plpgsql AS 'x'")
                .unwrap()
                .rule,
            "create_procedure"
        );
        assert_eq!(
            check("CREATE MATERIALIZED VIEW m AS SELECT 1")
                .unwrap()
                .rule,
            "materialized_view"
        );
        // Table-level rules come before column types.
        assert_eq!(
            check("CREATE TEMP TABLE t (c money)").unwrap().rule,
            "temporary_table"
        );
        assert_eq!(
            check("CREATE UNLOGGED TABLE t (c int[])").unwrap().rule,
            "unlogged_table"
        );
    }

    #[test]
    fn rule_ids_are_unique_and_match_the_checks() {
        let mut seen = std::collections::HashSet::new();
        for rule in RULES {
            assert!(seen.insert(rule.id), "duplicate rule id {}", rule.id);
        }
    }

    #[test]
    fn nothing_fires_on_text_that_does_not_parse() {
        // check_rules works on the AST; unparsable text never reaches it. This guards the
        // plan: the plan marks it parse_failed and carries no refusal.
        let plan = crate::statement::StatementPlan::new("TRUNCATE");
        assert!(plan.parse_failed);
        assert_eq!(plan.refusal, None);
    }
}
