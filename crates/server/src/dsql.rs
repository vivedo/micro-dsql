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

use crate::{ddl, reply_rows, show_fields, sys, Outcome};

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
        Admission::Run { sql } => match dispatch(conn, plan, sql, format, run_engine) {
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

/// Where a statement the policy admitted runs: `sys` job statements, DDL with extra work, or
/// the engine as is.
fn dispatch(
    conn: &PgConnection,
    plan: &StatementPlan,
    sql: &str,
    format: &Format,
    run_engine: impl FnOnce(&str) -> PgWireResult<(Outcome, u64)>,
) -> PgWireResult<(Outcome, u64)> {
    if plan.job.is_some() {
        sys::run_job_statement(conn, plan, format, run_engine)
    } else if plan.ddl.is_some() || !plan.prelude.is_empty() {
        ddl::run_ddl_statement(conn, plan, run_engine)
    } else {
        run_engine(sql)
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
        Reply::Rows { tag, columns, rows } => reply_rows(&tag, &columns, &rows, format),
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

    // FR-054: each refusal is logged once, at debug level, with the rule id.
    mod refusal_log {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        use micro_dsql_policy::{PolicyLimits, SessionPolicy, StatementPlan};
        use pgwire::api::portal::Format;
        use tracing_subscriber::fmt::MakeWriter;
        use turso_core::ConnectOpts;

        use crate::{open_database, Storage};

        use super::*;

        #[derive(Clone, Default)]
        struct Captured(Arc<Mutex<Vec<u8>>>);

        impl Write for Captured {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for Captured {
            type Writer = Captured;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        fn refuse(level: tracing::Level, sqls: &[&str]) -> String {
            refuse_after(level, &[], sqls)
        }

        /// Like `refuse`, after running `setup` on the connection (outside the policy).
        fn refuse_after(level: tracing::Level, setup: &[&str], sqls: &[&str]) -> String {
            let db = open_database(&Storage::Memory).unwrap();
            let conn = PgConnection::connect_with(&db, ConnectOpts::new()).unwrap();
            for sql in setup {
                crate::internal::exec(&conn, sql).unwrap();
            }
            let policy = Mutex::new(SessionPolicy::new(PolicyLimits::default()));
            let captured = Captured::default();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(level)
                .with_ansi(false)
                .with_writer(captured.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                for sql in sqls {
                    let plan = StatementPlan::new(sql);
                    let result = run_planned(&conn, &policy, &plan, &Format::UnifiedText, |_| {
                        panic!("a refused statement must not reach the engine: {sql}")
                    });
                    assert_eq!(txn_status(&conn), TxnStatus::Idle, "{sql}");
                    assert!(result.is_err(), "{sql}");
                }
            });
            let bytes = captured.0.lock().unwrap().clone();
            String::from_utf8(bytes).unwrap()
        }

        #[test]
        fn a_refusal_is_logged_once_at_debug_level_with_the_rule_id() {
            let log = refuse(tracing::Level::DEBUG, &["TRUNCATE t"]);
            assert_eq!(log.lines().count(), 1, "{log}");
            assert!(log.contains("DEBUG"), "{log}");
            assert!(log.contains("truncate"), "{log}");
            let log = refuse(tracing::Level::DEBUG, &["TRUNCATE t", "SAVEPOINT a"]);
            assert_eq!(log.lines().count(), 2, "{log}");
            assert!(log.contains("savepoint"), "{log}");
        }

        /// FR-033 for the one refusal the server decides (from the catalog): it is logged once
        /// with its rule id, like the stateless ones.
        #[test]
        fn a_primary_key_column_refusal_is_logged_once_with_the_rule_id() {
            let log = refuse_after(
                tracing::Level::DEBUG,
                &["CREATE TABLE pkt (id int PRIMARY KEY, v text)"],
                &["ALTER TABLE pkt DROP COLUMN id"],
            );
            // The catalog lookup the check makes logs at debug level too (the engine's own
            // lines); only the refusal's line counts here.
            let refusals: Vec<&str> = log
                .lines()
                .filter(|line| line.contains("statement refused"))
                .collect();
            assert_eq!(refusals.len(), 1, "{log}");
            assert!(refusals[0].contains("DEBUG"), "{log}");
            assert!(refusals[0].contains("drop_pk_column"), "{log}");
        }

        #[test]
        fn nothing_is_logged_at_info_level() {
            let log = refuse(tracing::Level::INFO, &["TRUNCATE t", "SET work_mem = 1"]);
            assert_eq!(log, "");
        }
    }
}
