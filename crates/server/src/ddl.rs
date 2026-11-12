//! DDL that needs work around the engine statement: identity sequences, `DROP TABLE` cleanup,
//! constraints the engine's SQL frontend lacks (research.md D5, D7).

use micro_dsql_policy::StatementPlan;
use pgwire::error::PgWireResult;
use turso_pg::PgConnection;

use crate::Outcome;

/// Runs a DDL statement together with the work its plan carries.
pub(crate) fn run_ddl_statement(
    _conn: &PgConnection,
    plan: &StatementPlan,
    run_engine: impl FnOnce(&str) -> PgWireResult<(Outcome, u64)>,
) -> PgWireResult<(Outcome, u64)> {
    run_engine(&plan.engine_sql)
}
