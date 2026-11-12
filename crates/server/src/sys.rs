//! The `sys` schema: `sys.jobs` and the statements answered through it (research.md D2, D6).

use std::sync::Arc;

use micro_dsql_policy::StatementPlan;
use pgwire::error::PgWireResult;
use turso_core::{ConnectOpts, Database};
use turso_pg::PgConnection;

use crate::internal::exec;
use crate::Outcome;

/// Creates `sys` and `sys.jobs` when the database does not have them yet.
pub(crate) fn bootstrap(db: &Arc<Database>) -> anyhow::Result<()> {
    let conn = PgConnection::connect_with(db, ConnectOpts::new())?;
    let statements = [
        "CREATE SCHEMA IF NOT EXISTS sys",
        "CREATE TABLE IF NOT EXISTS sys.jobs (job_id text PRIMARY KEY, status text NOT NULL, \
         details text, job_type text NOT NULL, class_id oid NOT NULL, object_id oid NOT NULL, \
         object_name text, start_time timestamptz NOT NULL, update_time timestamptz)",
    ];
    for sql in statements {
        exec(&conn, sql).map_err(|e| anyhow::anyhow!("sys bootstrap: {sql}: {e}"))?;
    }
    conn.close()?;
    Ok(())
}

/// Runs a statement the plan answers through `sys.jobs`.
pub(crate) fn run_job_statement(
    _conn: &PgConnection,
    plan: &StatementPlan,
    run_engine: impl FnOnce(&str) -> PgWireResult<(Outcome, u64)>,
) -> PgWireResult<(Outcome, u64)> {
    run_engine(&plan.engine_sql)
}
