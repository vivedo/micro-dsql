//! DSQL policy for micro-dsql.
//!
//! Backend-agnostic by design (constitution Principle III): everything here works on the
//! `pg_query` AST and on plain values. Nothing in this crate knows about the engine.
//!
//! M1 content:
//! - [`server_params`]: the DSQL-shaped parameters a client sees at startup and through `SHOW`;
//! - [`intercept`]: statements the server answers itself (`SHOW <dsql parameter>`,
//!   `SET timezone` to UTC);
//! - [`errors`]: engine error messages reshaped like PostgreSQL's (position, schema);
//! - [`params`]: `$n` parameter counting and type hints, and result-column type hints.
//!
//! The DSQL statement rules, transaction limits and error shaping arrive in M2.

pub mod errors;
pub mod intercept;
pub mod params;
pub mod server_params;

pub use errors::{shape_error, ShapedError};
pub use intercept::{intercept, Intercept};
pub use params::{analyze, ParamHint, ResultHint, StatementShape};
