//! The engine-facing side of the DSQL policy: the only place where engine facts (transaction
//! status, error variants, change counts) are turned into the plain values the backend-agnostic
//! `micro-dsql-policy` crate works with.

use std::sync::Mutex;
use std::time::Instant;

use micro_dsql_policy::{
    Admission, EngineFailure, PolicyError, Reply, SessionPolicy, StatementPlan, TxnStatus,
};
use pgwire::api::portal::Format;
use pgwire::api::results::{DataRowEncoder, Tag};
use pgwire::error::{PgWireError, PgWireResult};
use std::sync::Arc;
use tracing::debug;
use turso_core::LimboError;
use turso_pg::{PgConnection, TransactionState};

use crate::{show_fields, Outcome};

/// The session's transaction status, mirrored for the policy.
pub(crate) fn txn_status(conn: &PgConnection) -> TxnStatus {
    match conn.transaction_state() {
        TransactionState::Idle => TxnStatus::Idle,
        TransactionState::Active => TxnStatus::Active,
        TransactionState::Aborted => TxnStatus::Aborted,
    }
}

/// Classifies an engine error for error shaping. This is the only place that maps engine
/// error variants.
pub(crate) fn engine_failure(error: &turso_pg::Error) -> EngineFailure {
    match error.core_error() {
        LimboError::WriteWriteConflict
        | LimboError::BusySnapshot
        | LimboError::CommitDependencyAborted
        | LimboError::Conflict(_) => EngineFailure::DataConflict,
        LimboError::SchemaConflict | LimboError::SchemaUpdated => EngineFailure::SchemaConflict,
        LimboError::Busy | LimboError::TableLocked => EngineFailure::LockBusy,
        _ => EngineFailure::Other,
    }
}

/// An error raised by the policy, as the wire error the server sends.
pub(crate) fn policy_error(error: PolicyError) -> PgWireError {
    debug!(
        rule = error.rule,
        sqlstate = error.sqlstate,
        "statement refused"
    );
    PgWireError::ApiError(Box::new(error))
}

fn lock(policy: &Mutex<SessionPolicy>) -> std::sync::MutexGuard<'_, SessionPolicy> {
    policy.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs one planned statement: admit, engine, complete/failed (blocking).
///
/// `run_engine` runs the given text on the engine and returns its outcome and the number of
/// rows the statement changed (the engine's per-statement change count).
pub(crate) fn run_planned(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    plan: &StatementPlan,
    format: &Format,
    run_engine: impl FnOnce(&str) -> PgWireResult<(Outcome, u64)>,
) -> PgWireResult<Outcome> {
    let admission = lock(policy).admit(plan, txn_status(conn), Instant::now());
    match admission {
        Admission::Run { sql } => match run_engine(sql) {
            Ok((outcome, rows)) => {
                let result = lock(policy).complete(plan, rows, txn_status(conn), Instant::now());
                match result {
                    Ok(()) => Ok(outcome),
                    Err(error) => {
                        conn.abort_transaction_block_on_error();
                        lock(policy).failed(txn_status(conn));
                        Err(policy_error(error))
                    }
                }
            }
            Err(error) => {
                lock(policy).failed(txn_status(conn));
                Err(error)
            }
        },
        Admission::Respond(reply) => respond(reply, format),
        Admission::Refuse(error) => {
            conn.abort_transaction_block_on_error();
            lock(policy).failed(txn_status(conn));
            Err(policy_error(error))
        }
        Admission::RollbackThenRefuse(error) => {
            if let Ok(mut rollback) = conn.prepare("ROLLBACK") {
                let _ = rollback.run_ignore_rows();
            }
            lock(policy).failed(txn_status(conn));
            Err(policy_error(error))
        }
    }
}

fn respond(reply: Reply, format: &Format) -> PgWireResult<Outcome> {
    match reply {
        Reply::Show { column, value } => {
            let header = Arc::new(show_fields(&column, format));
            let mut encoder = DataRowEncoder::new(header.clone());
            encoder.encode_field(&value)?;
            Ok(Outcome::Rows {
                header,
                rows: vec![encoder.finish()],
            })
        }
        Reply::Command { tag } => Ok(Outcome::Command(Tag::new(tag))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(error: LimboError) -> EngineFailure {
        engine_failure(&turso_pg::Error::from(error))
    }

    #[test]
    fn data_conflicts() {
        for error in [
            LimboError::WriteWriteConflict,
            LimboError::BusySnapshot,
            LimboError::CommitDependencyAborted,
            LimboError::Conflict("x".into()),
        ] {
            assert_eq!(failure(error), EngineFailure::DataConflict);
        }
    }

    #[test]
    fn schema_conflicts() {
        assert_eq!(
            failure(LimboError::SchemaConflict),
            EngineFailure::SchemaConflict
        );
        assert_eq!(
            failure(LimboError::SchemaUpdated),
            EngineFailure::SchemaConflict
        );
    }

    #[test]
    fn lock_contention_is_lock_busy() {
        assert_eq!(failure(LimboError::Busy), EngineFailure::LockBusy);
        assert_eq!(failure(LimboError::TableLocked), EngineFailure::LockBusy);
    }

    #[test]
    fn everything_else_is_other() {
        assert_eq!(failure(LimboError::DivisionByZero), EngineFailure::Other);
        assert_eq!(
            failure(LimboError::ParseError("x".into())),
            EngineFailure::Other
        );
    }
}
