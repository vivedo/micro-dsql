//! Constraint work the engine's PostgreSQL frontend lacks (research.md D7, D8).
//!
//! The only module that builds engine ASTs (`turso_parser`) and prepares them through
//! `PgConnection::inner()`.

use micro_dsql_policy::{errors, PolicyError};
use pgwire::error::{PgWireError, PgWireResult};
use turso_core::NamespaceId;
use turso_parser::ast;
use turso_pg::PgConnection;

use crate::internal::{ident as quote, literal, query, user_error};
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

/// `ALTER TABLE t ADD CONSTRAINT c CHECK (expr) NOT VALID` through the core engine's `ALTER
/// COLUMN` path (research.md D7, spike T032): the column's own definition, read from the schema,
/// plus the CHECK translated by the engine's PostgreSQL translator. New and updated rows are
/// checked from then on. The engine also checks the existing rows when it adds a CHECK, so a
/// table with a row that violates it fails with PostgreSQL's text for a validating ADD (known
/// gap: DSQL accepts the constraint and leaves the old rows alone).
pub(crate) fn add_check_not_valid(
    conn: &PgConnection,
    schema: Option<&str>,
    table: &str,
    name: &str,
    column: &str,
    expr_sql: &str,
    original: &str,
) -> PgWireResult<()> {
    let schema_name = schema.unwrap_or(DEFAULT_SCHEMA);
    let mut definition = column_definition(conn, schema_name, table, column)?;
    // The CHECK, translated from PostgreSQL text by the engine's own translator.
    let synthetic = format!(
        "CREATE TABLE x ({} int CONSTRAINT {} CHECK ({expr_sql}))",
        quote(column),
        quote(name)
    );
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
            "unexpected translation of a CHECK",
        ));
    };
    let Some(added) = columns.into_iter().next() else {
        return Err(user_error(
            conn,
            "XX000",
            "unexpected translation of a CHECK",
        ));
    };
    definition.constraints.extend(added.constraints);
    let statement = ast::Stmt::AlterTable(ast::AlterTable {
        name: ast::QualifiedName {
            namespace: schema.map(|s| ast::Name::exact(s.to_owned())),
            name: ast::Name::exact(table.to_owned()),
            alias: None,
        },
        body: ast::AlterTableBody::AlterColumn {
            old: ast::Name::exact(column.to_owned()),
            new: definition,
        },
    });
    let mut prepared = conn
        .inner()
        .prepare_translated_stmt(statement, original)
        .map_err(|e| engine_error(conn, e, table, name))?;
    prepared
        .run_ignore_rows()
        .map_err(|e| engine_error(conn, e, table, name))
}

fn engine_error(
    conn: &PgConnection,
    error: turso_core::LimboError,
    table: &str,
    constraint: &str,
) -> PgWireError {
    let message = error.to_string();
    if message.contains("CHECK constraint failed") {
        return user_error(
            conn,
            "23514",
            &format!(
                "check constraint \"{constraint}\" of relation \"{table}\" is violated by some row"
            ),
        );
    }
    conn.abort_transaction_block_on_error();
    crate::pg_error(turso_pg::Error::from(error))
}

/// The definition of one column as the engine stores it (parsed from the table's `CREATE TABLE`
/// text, in the engine's own dialect).
fn column_definition(
    conn: &PgConnection,
    schema: &str,
    table: &str,
    column: &str,
) -> PgWireResult<ast::ColumnDefinition> {
    let current = conn.inner().current_schema();
    let btree = current
        .namespace_named(schema)
        .and_then(|namespace: NamespaceId| current.get_btree_table(namespace, table))
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
        body: ast::CreateTableBody::ColumnsAndConstraints { columns, .. },
        ..
    }))) = parser.next()
    else {
        return Err(user_error(
            conn,
            "XX000",
            "cannot read the table definition",
        ));
    };
    columns
        .into_iter()
        .find(|c| c.col_name.as_str() == column)
        .ok_or_else(|| {
            user_error(
                conn,
                "42703",
                &format!("column \"{column}\" of relation \"{table}\" does not exist"),
            )
        })
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
}
