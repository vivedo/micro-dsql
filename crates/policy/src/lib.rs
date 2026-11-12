//! DSQL policy for micro-dsql.
//!
//! Backend-agnostic by design (constitution Principle III): everything here works on the
//! `pg_query` AST and on plain values. Nothing in this crate knows about the engine; the
//! server passes in engine facts as [`TxnStatus`], a changed-row count, an `Instant` and an
//! [`EngineFailure`].
//!
//! - [`statement`]: [`StatementPlan`], the pipeline run once per statement text;
//! - [`classify`]: DDL / DML / read / transaction control / session command;
//! - [`session`]: [`SessionPolicy`], the per-connection transaction discipline and limits;
//! - [`errors`]: the one place where DSQL's error texts live, and the PostgreSQL-style shaping
//!   of ordinary engine errors;
//! - [`rewrite`]: text rewrites of what the engine runs (pre-parse stage and AST edits);
//! - [`server_params`], [`intercept`]: the DSQL-shaped server parameters and the statements
//!   answered without the engine;
//! - [`params`]: `$n` parameter counting and type hints, and result-column type hints.

pub mod classify;
pub mod errors;
pub mod intercept;
pub mod params;
pub mod rewrite;
pub mod server_params;
pub mod session;
pub mod statement;

pub use classify::{classify, StatementClass, TxnControl};
pub use errors::{
    age_limit, oc000, oc001, row_limit, shape_engine_error, shape_error, EngineFailure,
    PolicyError, ShapedError,
};
pub use intercept::{intercept, Intercept};
pub use params::{analyze, ParamHint, ResultHint, StatementShape};
pub use session::{Admission, PolicyLimits, Reply, SessionPolicy, TxnStatus};
pub use statement::StatementPlan;
