//! Constraint work the engine's PostgreSQL frontend lacks (research.md D7, D8).
//!
//! The only module that builds engine ASTs (`turso_parser`) and prepares them through
//! `PgConnection::inner()`.

use micro_dsql_policy::jobs::FkAction;
use micro_dsql_policy::{errors, NotValidKind, PolicyError, TxnStatus};
use pgwire::error::PgWireResult;
use turso_parser::ast;
use turso_pg::PgConnection;

use crate::dsql::txn_status;
use crate::internal::{exec, ident as quote, literal, query, user_error};
use crate::sys::Job;

/// Refuses `ALTER TABLE ... DROP COLUMN` of a primary-key column (`drop_pk_column`): the
/// statement alone does not say which columns are the key, so the catalog is asked.
pub(crate) fn check_drop_column(
    conn: &PgConnection,
    schema: Option<&str>,
    table: &str,
    columns: &[String],
) -> PgWireResult<Option<PolicyError>> {
    let definitions = query(
        conn,
        &format!(
            "SELECT pg_get_constraintdef(co.oid) FROM pg_constraint co \
             JOIN pg_class c ON c.oid = co.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE co.contype = 'p' AND c.relname = {} AND n.nspname = {}",
            literal(table),
            literal(schema.unwrap_or("public"))
        ),
    )?;
    let key: Vec<String> = definitions
        .iter()
        .filter_map(|row| row[0].as_deref())
        .flat_map(primary_key_columns)
        .collect();
    Ok(columns
        .iter()
        .find(|column| key.contains(column))
        .map(|column| errors::drop_pk_column(column)))
}

/// The columns of `PRIMARY KEY (a, "b c")`.
fn primary_key_columns(definition: &str) -> Vec<String> {
    let Some((_, rest)) = definition.split_once('(') else {
        return Vec::new();
    };
    let list = rest.split(')').next().unwrap_or_default();
    list.split(',')
        .map(|name| name.trim().trim_matches('"').replace("\"\"", "\""))
        .filter(|name| !name.is_empty())
        .collect()
}

/// The schema a table is looked up in when the statement does not say.
const DEFAULT_SCHEMA: &str = "public";

/// Restores the connection's foreign-key enforcement and CHECK evaluation, whatever happens.
struct EnforcementOff<'a>(&'a PgConnection);

impl<'a> EnforcementOff<'a> {
    /// Switches both off for statements prepared from now on (core API, per connection).
    fn new(conn: &'a PgConnection) -> Self {
        conn.inner().set_foreign_keys_enabled(false);
        conn.inner().set_check_constraints_ignored(true);
        Self(conn)
    }
}

impl Drop for EnforcementOff<'_> {
    fn drop(&mut self) {
        // Sessions always enforce foreign keys and evaluate CHECK (see `serve_connection`).
        self.0.inner().set_foreign_keys_enabled(true);
        self.0.inner().set_check_constraints_ignored(false);
    }
}

/// Runs `steps` atomically on the session's connection.
///
/// Inside a client transaction the steps are part of it (a failure aborts the block like any
/// failed DDL). When the session is idle, an internal `BEGIN` ... `COMMIT` wraps them
/// (`ROLLBACK` on any error) and the session is idle again afterwards. This is the one place
/// where micro-dsql opens a transaction for a client statement: nothing else runs inside it, it
/// starts and ends within the execution of that one statement, and the policy sees `Idle` before
/// and after. If the server's way of starting transactions changes (M4), this is the helper to
/// route through it.
pub(crate) fn in_internal_transaction<T>(
    conn: &PgConnection,
    steps: impl FnOnce() -> PgWireResult<T>,
) -> PgWireResult<T> {
    if txn_status(conn) != TxnStatus::Idle {
        return steps().inspect_err(|_| conn.abort_transaction_block_on_error());
    }
    exec(conn, "BEGIN")?;
    match steps() {
        Ok(value) => {
            if let Err(error) = exec(conn, "COMMIT") {
                let _ = exec(conn, "ROLLBACK");
                return Err(error);
            }
            Ok(value)
        }
        Err(error) => {
            let _ = exec(conn, "ROLLBACK");
            Err(error)
        }
    }
}

/// How an `ADD CONSTRAINT ... NOT VALID` is stated to the engine.
fn added_constraint(
    conn: &PgConnection,
    schema: &str,
    name: &str,
    kind: &NotValidKind,
) -> PgWireResult<(String, Vec<ast::NamedColumnConstraint>)> {
    let (column, constraint) = match kind {
        NotValidKind::Check { column, expr_sql } => (
            column,
            format!("CONSTRAINT {} CHECK ({expr_sql})", quote(name)),
        ),
        NotValidKind::ForeignKey {
            column,
            ref_schema,
            ref_table,
            ref_column,
            on_delete,
            on_update,
        } => {
            let target = ref_schema.as_deref().unwrap_or(schema);
            if target != schema {
                return Err(user_error(
                    conn,
                    "0A000",
                    "a NOT VALID foreign key to a table of another schema is not supported",
                ));
            }
            let current = conn.inner().current_schema();
            let parent = current
                .namespace_named(target)
                .and_then(|ns| current.get_btree_table(ns, ref_table))
                .ok_or_else(|| {
                    user_error(
                        conn,
                        "42P01",
                        &format!("relation \"{ref_table}\" does not exist"),
                    )
                })?;
            let parent_column = ref_column
                .clone()
                .or_else(|| parent.primary_key_columns.first().map(|(c, _)| c.clone()))
                .ok_or_else(|| {
                    user_error(
                        conn,
                        "42830",
                        "there is no primary key for the referenced table",
                    )
                })?;
            let action = |a: &FkAction| match a {
                FkAction::NoAction => "NO ACTION",
                FkAction::Restrict => "RESTRICT",
                FkAction::Cascade => "CASCADE",
                FkAction::SetNull => "SET NULL",
                FkAction::SetDefault => "SET DEFAULT",
            };
            (
                column,
                format!(
                    "CONSTRAINT {} REFERENCES {} ({}) ON DELETE {} ON UPDATE {}",
                    quote(name),
                    quote(ref_table),
                    quote(&parent_column),
                    action(on_delete),
                    action(on_update)
                ),
            )
        }
    };
    // Translated by the engine's own PostgreSQL translator, from a synthetic CREATE TABLE.
    let synthetic = format!("CREATE TABLE x ({} int {constraint})", quote(column));
    let parsed = turso_pg_parser::parse(&synthetic)
        .map_err(|e| user_error(conn, "42601", &e.to_string()))?;
    let translated = turso_pg_parser::translator::PostgreSQLTranslator::new()
        .translate(&parsed)
        .map_err(|e| user_error(conn, "42601", &e.to_string()))?;
    let ast::Stmt::CreateTable {
        body: ast::CreateTableBody::ColumnsAndConstraints { columns, .. },
        ..
    } = translated
    else {
        return Err(user_error(
            conn,
            "XX000",
            "unexpected translation of a constraint",
        ));
    };
    let added = columns
        .into_iter()
        .next()
        .map(|c| c.constraints)
        .unwrap_or_default();
    Ok((column.clone(), added))
}

/// Gives every constraint without a name the one PostgreSQL would derive, so the rebuilt table
/// keeps the names `pg_constraint` shows today (the engine derives them from the table's name).
fn name_constraints(
    table: &str,
    columns: &mut [ast::ColumnDefinition],
    constraints: &mut [ast::NamedTableConstraint],
) {
    for column in columns {
        let col = column.col_name.as_str().to_owned();
        for named in &mut column.constraints {
            if named.name.is_some() {
                continue;
            }
            let suffix = match named.constraint {
                ast::ColumnConstraint::PrimaryKey { .. } => "pkey".to_owned(),
                ast::ColumnConstraint::Unique(_) => format!("{col}_key"),
                ast::ColumnConstraint::Check { .. } => format!("{col}_check"),
                ast::ColumnConstraint::ForeignKey { .. } => format!("{col}_fkey"),
                _ => continue,
            };
            let name = if suffix == "pkey" {
                format!("{table}_pkey")
            } else {
                format!("{table}_{suffix}")
            };
            named.name = Some(ast::Name::exact(name));
        }
    }
    for named in constraints {
        if named.name.is_some() {
            continue;
        }
        let names = |cols: Vec<String>| cols.join("_");
        let name = match &named.constraint {
            ast::TableConstraint::PrimaryKey { .. } => format!("{table}_pkey"),
            ast::TableConstraint::Unique { columns, .. } => format!(
                "{table}_{}_key",
                names(columns.iter().map(|c| c.expr.to_string()).collect())
            ),
            ast::TableConstraint::Check { .. } => format!("{table}_check"),
            ast::TableConstraint::ForeignKey { columns, .. } => format!(
                "{table}_{}_fkey",
                names(
                    columns
                        .iter()
                        .map(|c| c.col_name.as_str().to_owned())
                        .collect()
                )
            ),
        };
        named.name = Some(ast::Name::exact(name));
    }
}

/// `ALTER TABLE t ADD CONSTRAINT n FOREIGN KEY ... / CHECK ... NOT VALID` (research.md D13).
///
/// The engine can neither add a foreign key to an existing table nor add a CHECK without
/// looking at the existing rows (spike T032), so the table is rebuilt the way SQLite alters a
/// table: a new table with the same columns and constraints plus the added one, the rows copied
/// without checking them, the old table dropped, the new one renamed, the indexes recreated.
/// Foreign-key enforcement and CHECK evaluation are off on the connection while it happens
/// (restored on every exit path), and the whole thing is atomic ([`in_internal_transaction`]).
/// The table gets a new oid; the `sys.jobs` rows that name it or its indexes follow it.
pub(crate) fn rebuild_with_constraint(
    conn: &PgConnection,
    schema: Option<&str>,
    table: &str,
    name: &str,
    kind: &NotValidKind,
) -> PgWireResult<()> {
    let schema_name = schema.unwrap_or(DEFAULT_SCHEMA);
    let current = conn.inner().current_schema();
    let btree = current
        .namespace_named(schema_name)
        .and_then(|ns| current.get_btree_table(ns, table))
        .ok_or_else(|| {
            user_error(
                conn,
                "42P01",
                &format!("relation \"{table}\" does not exist"),
            )
        })?;
    let sql = btree.to_sql();
    let mut parser = turso_parser::parser::Parser::new(sql.as_bytes());
    let Some(Ok(ast::Cmd::Stmt(ast::Stmt::CreateTable {
        body:
            ast::CreateTableBody::ColumnsAndConstraints {
                mut columns,
                mut constraints,
                options: _,
            },
        ..
    }))) = parser.next()
    else {
        return Err(user_error(
            conn,
            "XX000",
            "cannot read the table definition",
        ));
    };
    name_constraints(table, &mut columns, &mut constraints);
    let (column, added) = added_constraint(conn, schema_name, name, kind)?;
    // PostgreSQL text is what a PostgreSQL table is stored and executed as: render the engine
    // AST back to text (without the engine's STRICT) and let the connection create it.
    let options = ast::TableOptions {
        without_rowid_text: None,
        strict_text: None,
    };
    let Some(target) = columns.iter_mut().find(|c| c.col_name.as_str() == column) else {
        return Err(user_error(
            conn,
            "42703",
            &format!("column \"{column}\" of relation \"{table}\" does not exist"),
        ));
    };
    target.constraints.extend(added);
    let column_list = btree
        .columns()
        .iter()
        .filter_map(|c| c.name.as_deref())
        .map(quote)
        .collect::<Vec<_>>()
        .join(", ");
    let rebuilt = format!("{table}__rebuild");
    let qualified = format!("{}.{}", quote(schema_name), quote(table));
    let qualified_new = format!("{}.{}", quote(schema_name), quote(&rebuilt));
    let create = ast::Stmt::CreateTable {
        temporary: false,
        if_not_exists: false,
        tbl_name: ast::QualifiedName {
            namespace: Some(ast::Name::exact(schema_name.to_owned())),
            name: ast::Name::exact(rebuilt.clone()),
            alias: None,
        },
        body: ast::CreateTableBody::ColumnsAndConstraints {
            columns,
            constraints,
            options,
        },
    };
    let create_text = create.to_string();
    // The table's own indexes (not the ones its constraints make), as PostgreSQL text.
    let indexes = query(
        conn,
        &format!(
            "SELECT indexname, indexdef FROM pg_indexes WHERE schemaname = {} AND tablename = {} \
             AND indexname NOT LIKE '%turso_internal%' AND indexname NOT LIKE 'sqlite_autoindex%'",
            literal(schema_name),
            literal(table)
        ),
    )?;
    in_internal_transaction(conn, || {
        let _enforcement = EnforcementOff::new(conn);
        let step = |sql: &str| -> PgWireResult<()> {
            exec(conn, sql)?;
            failpoint(conn)
        };
        step(&create_text)?;
        step(&format!(
            "INSERT INTO {qualified_new} ({column_list}) SELECT {column_list} FROM {qualified}"
        ))?;
        step(&format!("DROP TABLE {qualified}"))?;
        step(&format!(
            "ALTER TABLE {qualified_new} RENAME TO {}",
            quote(table)
        ))?;
        for row in &indexes {
            if let Some(definition) = &row[1] {
                step(definition)?;
            }
        }
        refresh_job_objects(conn, schema_name, table, &indexes)?;
        failpoint(conn)
    })
}

#[cfg(test)]
thread_local! {
    /// Tests: fail the rebuild after this many more steps.
    static FAIL_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Test hook: an injected failure after a step of the rebuild.
#[cfg(test)]
fn failpoint(conn: &PgConnection) -> PgWireResult<()> {
    FAIL_AFTER.with(|left| match left.get() {
        Some(0) => {
            left.set(None);
            Err(user_error(conn, "XX000", "injected failure"))
        }
        Some(n) => {
            left.set(Some(n - 1));
            Ok(())
        }
        None => Ok(()),
    })
}

#[cfg(not(test))]
fn failpoint(_: &PgConnection) -> PgWireResult<()> {
    Ok(())
}

/// The table and its indexes have new oids after a rebuild: the `sys.jobs` rows that named them
/// are pointed at the new ones.
fn refresh_job_objects(
    conn: &PgConnection,
    schema: &str,
    table: &str,
    indexes: &[Vec<Option<String>>],
) -> PgWireResult<()> {
    let mut objects = vec![(table.to_owned(), "VALIDATE_CONSTRAINT")];
    objects.extend(
        indexes
            .iter()
            .filter_map(|row| row[0].clone())
            .map(|index| (index, "INDEX_BUILD")),
    );
    for (name, job_type) in objects {
        let oid = query(
            conn,
            &format!(
                "SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relname = {} AND n.nspname = {}",
                literal(&name),
                literal(schema)
            ),
        )?;
        if let Some(oid) = oid.first().and_then(|r| r[0].as_deref()) {
            exec(
                conn,
                &format!(
                    "UPDATE sys.jobs SET object_id = {oid} WHERE job_type = '{job_type}' \
                     AND object_name = {}",
                    literal(&format!("{schema}.{name}"))
                ),
            )?;
        }
    }
    Ok(())
}

/// `ALTER TABLE ASYNC t VALIDATE CONSTRAINT c` (research.md D8): the check the constraint stands
/// for runs over all rows; the result is a completed or failed job, never a statement error
/// (except for a constraint the table does not have).
pub(crate) fn validate(
    conn: &PgConnection,
    schema: Option<&str>,
    table: &str,
    constraint: &str,
) -> PgWireResult<Job> {
    let schema_name = schema.unwrap_or(DEFAULT_SCHEMA);
    let found = query(
        conn,
        &format!(
            "SELECT co.contype, pg_get_constraintdef(co.oid), c.oid FROM pg_constraint co \
             JOIN pg_class c ON c.oid = co.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE co.conname = {} AND c.relname = {} AND n.nspname = {}",
            literal(constraint),
            literal(table),
            literal(schema_name)
        ),
    )?;
    let Some(row) = found.first() else {
        return Err(user_error(
            conn,
            "42704",
            &format!("constraint \"{constraint}\" of relation \"{table}\" does not exist"),
        ));
    };
    let kind = row[0].as_deref().unwrap_or_default();
    let definition = row[1].as_deref().unwrap_or_default();
    let table_oid: i64 = row[2].as_deref().and_then(|v| v.parse().ok()).unwrap_or(0);
    let qualified = format!("{}.{}", quote(schema_name), quote(table));
    let violation = match kind {
        "c" => check_violation(conn, &qualified, table, constraint, definition)?,
        "f" => foreign_key_violation(conn, &qualified, table, constraint, definition)?,
        // Primary keys and unique constraints were enforced when they were made.
        _ => None,
    };
    Ok(Job {
        status: if violation.is_some() {
            "failed"
        } else {
            "completed"
        },
        details: violation,
        job_type: "VALIDATE_CONSTRAINT",
        object_id: table_oid,
        object_name: format!("{schema_name}.{table}"),
    })
}

/// The expression of `CHECK (expr)` and the first row for which it is false.
fn check_violation(
    conn: &PgConnection,
    qualified: &str,
    table: &str,
    constraint: &str,
    definition: &str,
) -> PgWireResult<Option<String>> {
    let (Some(open), Some(close)) = (definition.find('('), definition.rfind(')')) else {
        return Ok(None);
    };
    let expression = &definition[open + 1..close];
    let violating = query(
        conn,
        &format!("SELECT count(*) FROM {qualified} WHERE NOT COALESCE(({expression}), true)"),
    )?;
    let count: i64 = violating
        .first()
        .and_then(|r| r[0].as_deref())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Ok((count > 0).then(|| {
        format!("check constraint \"{constraint}\" of relation \"{table}\" is violated by some row")
    }))
}

/// `FOREIGN KEY (a, b) REFERENCES [schema.]parent(x, y)`: the first child row with a non-null key
/// and no parent.
fn foreign_key_violation(
    conn: &PgConnection,
    qualified: &str,
    table: &str,
    constraint: &str,
    definition: &str,
) -> PgWireResult<Option<String>> {
    let Some((child_columns, parent, parent_columns)) = parse_foreign_key(definition) else {
        return Ok(None);
    };
    let parent = parent
        .split('.')
        .map(|part| quote(part.trim_matches('"')))
        .collect::<Vec<_>>()
        .join(".");
    let not_null = child_columns
        .iter()
        .map(|c| format!("c.{} IS NOT NULL", quote(c)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let matches = child_columns
        .iter()
        .zip(&parent_columns)
        .map(|(c, p)| format!("p.{} = c.{}", quote(p), quote(c)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let select_list = child_columns
        .iter()
        .map(|c| format!("c.{}", quote(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = query(
        conn,
        &format!(
            "SELECT {select_list} FROM {qualified} c WHERE {not_null} \
             AND NOT EXISTS (SELECT 1 FROM {parent} p WHERE {matches}) LIMIT 1"
        ),
    )?;
    Ok(rows.first().map(|values| {
        format!(
            "insert or update on table \"{table}\" violates foreign key constraint \"{constraint}\". \
             Key ({})=({}) is not present in the referenced table.",
            child_columns.join(", "),
            values
                .iter()
                .map(|v| v.clone().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }))
}

/// The child columns, the parent (as written) and the parent columns of a constraint definition.
fn parse_foreign_key(definition: &str) -> Option<(Vec<String>, String, Vec<String>)> {
    let rest = definition.strip_prefix("FOREIGN KEY")?.trim_start();
    let (children, rest) = rest.strip_prefix('(')?.split_once(')')?;
    let rest = rest.trim_start().strip_prefix("REFERENCES")?.trim_start();
    let (parent, rest) = rest.split_once('(')?;
    let (parents, _) = rest.split_once(')')?;
    let names = |list: &str| -> Vec<String> {
        list.split(',')
            .map(|n| n.trim().trim_matches('"').to_owned())
            .collect()
    };
    Some((names(children), parent.trim().to_owned(), names(parents)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_key_definitions() {
        assert_eq!(primary_key_columns("PRIMARY KEY (id)"), ["id"]);
        assert_eq!(
            primary_key_columns("PRIMARY KEY (a, \"B c\")"),
            ["a", "B c"]
        );
        assert!(primary_key_columns("").is_empty());
    }

    #[test]
    fn foreign_key_definitions() {
        assert_eq!(
            parse_foreign_key("FOREIGN KEY (parent) REFERENCES p(id) ON DELETE CASCADE"),
            Some((
                vec!["parent".to_owned()],
                "p".to_owned(),
                vec!["id".to_owned()]
            ))
        );
        assert_eq!(
            parse_foreign_key("FOREIGN KEY (a, b) REFERENCES s.\"P q\" (x, y)"),
            Some((
                vec!["a".to_owned(), "b".to_owned()],
                "s.\"P q\"".to_owned(),
                vec!["x".to_owned(), "y".to_owned()]
            ))
        );
        assert_eq!(parse_foreign_key("CHECK (a > 0)"), None);
    }

    /// A foreign key with an orphan row fails its VALIDATE job (the orphan is made with foreign
    /// keys switched off on the connection, which is how a NOT VALID key leaves old rows).
    #[test]
    fn validate_reports_orphans_of_a_foreign_key() {
        use crate::internal::exec;
        use crate::{open_database, Storage};
        use turso_core::ConnectOpts;
        let db = open_database(&Storage::Memory).unwrap();
        let conn = PgConnection::connect_with(&db, ConnectOpts::new()).unwrap();
        exec(&conn, "CREATE TABLE vp (id bigint PRIMARY KEY)").unwrap();
        exec(
            &conn,
            "CREATE TABLE vc (id int PRIMARY KEY, parent bigint CONSTRAINT vc_fk REFERENCES vp (id), \
             w int CONSTRAINT vc_w CHECK (w > 0))",
        )
        .unwrap();
        exec(&conn, "INSERT INTO vp VALUES (1)").unwrap();
        exec(&conn, "INSERT INTO vc VALUES (1, 1, 5), (2, NULL, 5)").unwrap();
        let ok = validate(&conn, None, "vc", "vc_fk").unwrap();
        assert_eq!((ok.status, ok.details), ("completed", None));
        assert_eq!(ok.job_type, "VALIDATE_CONSTRAINT");
        assert_eq!(ok.object_name, "public.vc");
        assert_eq!(
            validate(&conn, None, "vc", "vc_w").unwrap().status,
            "completed"
        );
        conn.inner().set_foreign_keys_enabled(false);
        exec(&conn, "INSERT INTO vc VALUES (3, 99, 5)").unwrap();
        conn.inner().set_foreign_keys_enabled(true);
        let failed = validate(&conn, None, "vc", "vc_fk").unwrap();
        assert_eq!(failed.status, "failed");
        let details = failed.details.unwrap();
        assert!(
            details.contains("vc_fk") && details.contains("(parent)=(99)"),
            "{details}"
        );
        assert!(validate(&conn, None, "vc", "nope").is_err());
        assert!(validate(&conn, None, "vp", "vc_fk").is_err());
    }

    /// Research.md D13 (e): a failure after any step leaves the table intact, the session where it
    /// was (idle, or inside the client's aborted block), and enforcement switched back on.
    #[test]
    fn a_failure_after_any_step_leaves_the_table_intact() {
        use crate::internal::exec;
        use crate::{open_database, Storage};
        use micro_dsql_policy::NotValidKind;
        use turso_core::ConnectOpts;
        for in_client_transaction in [false, true] {
            for fail_after in 0..6 {
                let db = open_database(&Storage::Memory).unwrap();
                let conn = PgConnection::connect_with(&db, ConnectOpts::new()).unwrap();
                conn.inner().set_foreign_keys_enabled(true);
                for sql in [
                    "CREATE TABLE p (id bigint PRIMARY KEY)",
                    "CREATE TABLE c (id int PRIMARY KEY, w int, o bigint REFERENCES p (id))",
                    "CREATE INDEX ASYNC c_w ON c (w)",
                    "INSERT INTO p VALUES (1)",
                    "INSERT INTO c VALUES (1, 5, 1)",
                ] {
                    // Not through the policy: `ASYNC` is blanked by hand.
                    exec(&conn, &sql.replace(" ASYNC", "")).unwrap();
                }
                if in_client_transaction {
                    exec(&conn, "BEGIN").unwrap();
                }
                FAIL_AFTER.with(|left| left.set(Some(fail_after)));
                let kind = NotValidKind::Check {
                    column: "w".into(),
                    expr_sql: "w > 0".into(),
                };
                let result = rebuild_with_constraint(&conn, None, "c", "c_chk", &kind);
                FAIL_AFTER.with(|left| left.set(None));
                assert!(result.is_err(), "{fail_after} {in_client_transaction}");
                let expected = if in_client_transaction {
                    TxnStatus::Aborted
                } else {
                    TxnStatus::Idle
                };
                assert_eq!(txn_status(&conn), expected, "{fail_after}");
                if in_client_transaction {
                    exec(&conn, "ROLLBACK").unwrap();
                }
                assert_eq!(txn_status(&conn), TxnStatus::Idle);
                let rows = query(&conn, "SELECT id, w, o FROM c").unwrap();
                assert_eq!(rows.len(), 1, "{fail_after}");
                let names = query(
                    &conn,
                    "SELECT conname FROM pg_constraint WHERE conname LIKE 'c_%'",
                )
                .unwrap();
                assert_eq!(
                    names
                        .iter()
                        .filter(|r| r[0].as_deref() == Some("c_chk"))
                        .count(),
                    0
                );
                assert_eq!(
                    query(
                        &conn,
                        "SELECT count(*) FROM pg_indexes WHERE indexname = 'c_w'"
                    )
                    .unwrap()[0][0]
                        .as_deref(),
                    Some("1"),
                    "{fail_after}"
                );
                assert!(conn.inner().foreign_keys_enabled());
                assert!(!conn.inner().check_constraints_ignored());
                // The foreign key still works, and no half-built table is left.
                assert!(
                    exec(&conn, "INSERT INTO c VALUES (2, 5, 77)").is_err(),
                    "{fail_after}"
                );
                assert_eq!(
                    query(
                        &conn,
                        "SELECT count(*) FROM pg_class WHERE relname = 'c__rebuild'"
                    )
                    .unwrap()[0][0]
                        .as_deref(),
                    Some("0")
                );
            }
        }
    }
}
