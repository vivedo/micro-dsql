//! Statements micro-dsql runs on a session's connection for its own purposes (catalog
//! lookups, job rows, sequences): straight to the engine, never through the policy, so they
//! are not counted by the discipline rules and not subject to the DSQL deny rules.

use pgwire::error::{PgWireError, PgWireResult};
use turso_core::Value;
use turso_pg::PgConnection;

use crate::pg_error;

/// Runs a statement and discards its rows.
pub(crate) fn exec(conn: &PgConnection, sql: &str) -> PgWireResult<()> {
    let mut stmt = conn.prepare(sql).map_err(pg_error)?;
    stmt.run_ignore_rows().map_err(pg_error)
}

/// Runs a query and returns its rows as text (`None` for NULL).
pub(crate) fn query(conn: &PgConnection, sql: &str) -> PgWireResult<Vec<Vec<Option<String>>>> {
    let mut stmt = conn.prepare(sql).map_err(pg_error)?;
    let mut rows = Vec::new();
    stmt.run_with_row_callback(|row| {
        rows.push(row.get_values().map(value_text).collect());
        Ok(())
    })
    .map_err(pg_error)?;
    Ok(rows)
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// A string as a SQL literal.
pub(crate) fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// An identifier, quoted.
pub(crate) fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A server-raised error with its own SQLSTATE; like an engine error it aborts an open
/// transaction block.
pub(crate) fn user_error(conn: &PgConnection, sqlstate: &str, message: &str) -> PgWireError {
    conn.abort_transaction_block_on_error();
    PgWireError::UserError(Box::new(crate::error_info_code(sqlstate, message)))
}
