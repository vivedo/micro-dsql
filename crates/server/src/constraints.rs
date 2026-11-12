//! Constraint work the engine's PostgreSQL frontend lacks (research.md D7, D8).

use micro_dsql_policy::{errors, PolicyError};
use pgwire::error::PgWireResult;
use turso_pg::PgConnection;

use crate::internal::{literal, query};

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
}
