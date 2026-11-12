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
//! - [`rules`], [`types`]: the stateless deny rules and column type checks;
//! - [`settings`]: the SET whitelist and the remembered values behind `SHOW`;
//! - [`errors`]: the one place where DSQL's error texts live, and the PostgreSQL-style shaping
//!   of ordinary engine errors;
//! - [`rewrite`]: text rewrites of what the engine runs (pre-parse stage and AST edits);
//! - [`server_params`], [`intercept`]: the DSQL-shaped server parameters and the statements
//!   answered without the engine;
//! - [`params`]: `$n` parameter counting and type hints, and result-column type hints.

pub mod classify;
pub mod ddl_rules;
pub mod errors;
pub mod identity;
pub mod intercept;
pub mod jobs;
pub mod params;
pub mod rewrite;
pub mod rules;
pub mod server_params;
pub mod session;
pub mod settings;
pub mod statement;
pub mod types;

pub use classify::{classify, StatementClass, TxnControl};
pub use errors::{
    age_limit, oc000, oc001, row_limit, shape_engine_error, shape_error, EngineFailure,
    PolicyError, ShapedError,
};
pub use intercept::{intercept, Intercept};
pub use params::{analyze, ParamHint, ResultHint, StatementShape};
pub use session::{
    Admission, ColumnType, PolicyLimits, Reply, ReplyColumn, SessionPolicy, TxnState, TxnStatus,
};
pub use settings::{SessionSettings, SettingCommand};
pub use statement::StatementPlan;
