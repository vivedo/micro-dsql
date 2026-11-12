//! DDL that needs work around the engine statement: identity sequences, `DROP TABLE` cleanup,
//! constraints the engine's SQL frontend lacks (research.md D5, D7).

use micro_dsql_policy::identity::{default_sequence_name, nextval_target};
use micro_dsql_policy::{DdlAction, StatementPlan, TableRef, TxnStatus};
use pgwire::error::PgWireResult;
use turso_pg::PgConnection;

use crate::dsql::txn_status;
use crate::internal::{exec, ident, literal, query};
use crate::Outcome;

/// Runs a DDL statement together with the work its plan carries.
///
/// The prelude (identity sequences) runs first, in whatever transaction the session is in. If
/// the statement then fails outside a transaction, the sequences made for it are dropped again
/// (never inside a transaction: that is rolled back, and this module never opens one).
pub(crate) fn run_ddl_statement(
    conn: &PgConnection,
    plan: &StatementPlan,
    run_engine: impl FnOnce(&str) -> PgWireResult<(Outcome, u64)>,
) -> PgWireResult<(Outcome, u64)> {
    // The sequences of the identity columns of the tables a DROP TABLE removes.
    let to_drop = match &plan.ddl {
        Some(DdlAction::DropTable { tables }) => identity_sequences(conn, tables)?,
        _ => Vec::new(),
    };
    let mut made = 0;
    for statement in &plan.prelude {
        if let Err(error) = exec(conn, statement) {
            compensate(conn, plan, made);
            return Err(error);
        }
        made += 1;
    }
    match run_engine(&plan.engine_sql) {
        Ok(result) => {
            for (schema, sequence) in &to_drop {
                exec(
                    conn,
                    &format!(
                        "DROP SEQUENCE IF EXISTS {}.{}",
                        ident(schema),
                        ident(sequence)
                    ),
                )?;
            }
            Ok(result)
        }
        Err(error) => {
            compensate(conn, plan, made);
            Err(error)
        }
    }
}

/// Undoes the first `made` prelude statements when no transaction will roll them back.
fn compensate(conn: &PgConnection, plan: &StatementPlan, made: usize) {
    if txn_status(conn) != TxnStatus::Idle {
        return;
    }
    for statement in plan.cleanup_on_failure.iter().take(made) {
        let _ = exec(conn, statement);
    }
}

/// `(schema, sequence)` of every column default of the tables that is
/// `nextval('<schema>.<table>_<column>_seq')`: the sequences a desugared identity column made.
fn identity_sequences(
    conn: &PgConnection,
    tables: &[TableRef],
) -> PgWireResult<Vec<(String, String)>> {
    let mut found = Vec::new();
    for table in tables {
        let schema = table.schema.as_deref().unwrap_or("public");
        let defaults = query(
            conn,
            &format!(
                "SELECT a.attname, pg_get_expr(d.adbin, d.adrelid) FROM pg_attrdef d \
                 JOIN pg_class c ON c.oid = d.adrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = d.adrelid AND a.attnum = d.adnum \
                 WHERE c.relname = {} AND n.nspname = {}",
                literal(&table.name),
                literal(schema)
            ),
        )?;
        for row in defaults {
            let (Some(column), Some(expression)) = (&row[0], &row[1]) else {
                continue;
            };
            let sequence = default_sequence_name(&table.name, column);
            if nextval_literal(expression).as_deref() == Some(&nextval_target(schema, &sequence)) {
                found.push((schema.to_owned(), sequence));
            }
        }
    }
    Ok(found)
}

/// The text of the string literal inside `nextval('...')`.
fn nextval_literal(expression: &str) -> Option<String> {
    let rest = &expression[expression.find("nextval")?..];
    let start = rest.find('\'')? + 1;
    let end = start + rest[start..].find('\'')?;
    Some(rest[start..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nextval_literals() {
        assert_eq!(
            nextval_literal("nextval ('public.t_id_seq')").as_deref(),
            Some("public.t_id_seq")
        );
        assert_eq!(
            nextval_literal("(nextval('s.\"My T_id_seq\"'::regclass))").as_deref(),
            Some("s.\"My T_id_seq\"")
        );
        assert_eq!(nextval_literal("now()"), None);
        assert_eq!(nextval_literal("nextval(x)"), None);
    }
}
