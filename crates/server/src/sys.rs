//! The `sys` schema: `sys.jobs` and the statements answered through it (research.md D2, D6).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use micro_dsql_policy::jobs::{encode_job_id, is_job_id, JobIdSource};
use micro_dsql_policy::{errors, ColumnType, JobAction, ReplyColumn, StatementPlan, TxnStatus};
use pgwire::api::portal::Format;
use pgwire::error::{PgWireError, PgWireResult};
use turso_core::{ConnectOpts, Database};
use turso_pg::PgConnection;

use crate::constraints;
use crate::dsql::{policy_error, txn_status};
use crate::internal::{exec, ident, literal, query};
use crate::{reply_rows, Outcome};

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

/// How long a finished job stays in `sys.jobs` (DSQL's rule).
const JOB_RETENTION: Duration = Duration::from_secs(30 * 60);

/// The `pg_class` oid of `pg_class` itself, recorded as the job's class.
const PG_CLASS_OID: i64 = 1259;

/// What a recorded job looks like.
pub(crate) struct Job {
    pub(crate) status: &'static str,
    pub(crate) details: Option<String>,
    pub(crate) job_type: &'static str,
    pub(crate) object_id: i64,
    pub(crate) object_name: String,
}

/// `YYYY-MM-DD HH:MM:SS.ffffff` in UTC, without an offset: the engine does not do offset or
/// interval arithmetic (known gap B7), so timestamps are written as plain UTC literals.
pub(crate) fn utc_literal(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = since.as_secs() as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:06}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        since.subsec_micros()
    )
}

/// Removes finished jobs older than the retention (FR-008); `now` is a parameter so tests can
/// move the clock.
pub(crate) fn purge_finished(conn: &PgConnection, now: SystemTime) -> PgWireResult<()> {
    let cutoff = utc_literal(now.checked_sub(JOB_RETENTION).unwrap_or(UNIX_EPOCH));
    exec(
        conn,
        &format!(
            "DELETE FROM sys.jobs WHERE status IN ('completed', 'failed') \
             AND update_time < {}",
            literal(&cutoff)
        ),
    )
}

/// Records a job (after purging old ones) and returns its id. Runs in the session's current
/// transaction, so a rollback removes the job with the statement that made it.
pub(crate) fn record_job(
    conn: &PgConnection,
    now: SystemTime,
    random: u128,
    job: &Job,
) -> PgWireResult<String> {
    purge_finished(conn, now)?;
    let id = encode_job_id(random);
    let stamp = literal(&utc_literal(now));
    let details = job.details.as_deref().map_or("NULL".to_owned(), literal);
    exec(
        conn,
        &format!(
            "INSERT INTO sys.jobs (job_id, status, details, job_type, class_id, object_id, \
             object_name, start_time, update_time) VALUES ({}, {}, {details}, {}, {PG_CLASS_OID}, \
             {}, {}, {stamp}, {stamp})",
            literal(&id),
            literal(job.status),
            literal(job.job_type),
            job.object_id,
            literal(&job.object_name),
        ),
    )?;
    Ok(id)
}

fn random_bits() -> u128 {
    uuid::Uuid::new_v4().as_u128()
}

fn job_rows(tag: &str, id: String, format: &Format) -> PgWireResult<(Outcome, u64)> {
    let columns = [ReplyColumn {
        name: "job_id".to_owned(),
        ty: ColumnType::Text,
    }];
    // Job statements report no changed rows to the policy: they are DDL, and the job row they
    // write must not count against the transaction row limit.
    reply_rows(tag, &columns, &[vec![Some(id)]], format).map(|outcome| (outcome, 0))
}

/// Runs a statement the plan answers through `sys.jobs`.
pub(crate) fn run_job_statement(
    conn: &PgConnection,
    plan: &StatementPlan,
    format: &Format,
    run_engine: impl FnOnce(&str) -> PgWireResult<(Outcome, u64)>,
) -> PgWireResult<(Outcome, u64)> {
    match plan.job.as_ref() {
        Some(JobAction::IndexBuild {
            schema,
            index,
            table,
            unique,
            ..
        }) => {
            let schema = schema.as_deref().unwrap_or("public");
            match run_engine(&plan.engine_sql) {
                Ok(_) => {
                    let (object_id, name) = built_index(conn, schema, table, index.as_deref())?;
                    let job = Job {
                        status: "completed",
                        details: None,
                        job_type: "INDEX_BUILD",
                        object_id,
                        object_name: format!("{schema}.{name}"),
                    };
                    let id = record_job(conn, SystemTime::now(), random_bits(), &job)?;
                    job_rows("CREATE INDEX", id, format)
                }
                // A unique build over duplicates fails as a job, not as the statement.
                Err(error)
                    if *unique
                        && engine_sqlstate(&error) == Some("23505")
                        && txn_status(conn) == TxnStatus::Idle =>
                {
                    let name = index.clone().unwrap_or_else(|| format!("{table}_idx"));
                    let job = Job {
                        status: "failed",
                        details: Some(duplicate_details(conn, schema, table, &name, &error)),
                        job_type: "INDEX_BUILD",
                        object_id: 0,
                        object_name: format!("{schema}.{name}"),
                    };
                    let id = record_job(conn, SystemTime::now(), random_bits(), &job)?;
                    job_rows("CREATE INDEX", id, format)
                }
                Err(error) => Err(error),
            }
        }
        Some(JobAction::WaitForJob { id }) => {
            let id = match id {
                JobIdSource::Const(id) => id.clone(),
                JobIdSource::Param(n) => {
                    let (outcome, _) = run_engine(&format!("SELECT ${n}::text"))?;
                    first_text(&outcome).unwrap_or_default()
                }
            };
            if !is_job_id(&id) {
                return Err(policy_error(errors::wait_for_job_id()));
            }
            let status = query(
                conn,
                &format!(
                    "SELECT status FROM sys.jobs WHERE job_id = {}",
                    literal(&id)
                ),
            )?;
            let Some(status) = status.first().and_then(|row| row[0].clone()) else {
                return Err(policy_error(errors::wait_for_job_unknown(&id)));
            };
            let columns = [ReplyColumn {
                name: "succeeded".to_owned(),
                ty: ColumnType::Bool,
            }];
            let value = if status == "completed" { "t" } else { "f" };
            reply_rows("CALL", &columns, &[vec![Some(value.to_owned())]], format)
                .map(|outcome| (outcome, 0))
        }
        Some(JobAction::Validate {
            schema,
            table,
            constraint,
        }) => {
            let job = constraints::validate(conn, schema.as_deref(), table, constraint)?;
            let id = record_job(conn, SystemTime::now(), random_bits(), &job)?;
            job_rows("ALTER TABLE", id, format)
        }
        // `MajorVersion` is answered by the policy and never reaches the server.
        _ => run_engine(&plan.engine_sql),
    }
}

/// The oid and name of the index a `CREATE INDEX` made (or found, with `IF NOT EXISTS`): by
/// name, else the newest index of the table.
fn built_index(
    conn: &PgConnection,
    schema: &str,
    table: &str,
    index: Option<&str>,
) -> PgWireResult<(i64, String)> {
    let by_table = format!(
        "SELECT c.oid, c.relname FROM pg_index i \
         JOIN pg_class c ON c.oid = i.indexrelid JOIN pg_class t ON t.oid = i.indrelid \
         JOIN pg_namespace n ON n.oid = t.relnamespace \
         WHERE t.relname = {} AND n.nspname = {} AND c.relname NOT LIKE '__turso_internal%' \
         ORDER BY c.oid DESC LIMIT 1",
        literal(table),
        literal(schema)
    );
    let by_name = index.map(|name| {
        format!(
            "SELECT c.oid, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relname = {} AND n.nspname = {}",
            literal(name),
            literal(schema)
        )
    });
    let found = match by_name {
        Some(sql) => query(conn, &sql)?,
        None => query(conn, &by_table)?,
    };
    Ok(found
        .first()
        .and_then(|row| Some((row[0].as_ref()?.parse().ok()?, row[1].clone()?)))
        .unwrap_or_else(|| (0, index.unwrap_or(table).to_owned())))
}

fn engine_sqlstate(error: &PgWireError) -> Option<&str> {
    match error {
        PgWireError::ApiError(error) => error
            .downcast_ref::<turso_pg::Error>()
            .map(|error| error.sqlstate),
        _ => None,
    }
}

/// PostgreSQL-style details of a failed unique build: names the first duplicated key when the
/// engine's message says which columns were involved.
fn duplicate_details(
    conn: &PgConnection,
    schema: &str,
    table: &str,
    index: &str,
    error: &PgWireError,
) -> String {
    let message = match error {
        PgWireError::ApiError(error) => error
            .downcast_ref::<turso_pg::Error>()
            .map(|e| e.message.clone()),
        _ => None,
    }
    .unwrap_or_default();
    let head = format!("could not create unique index \"{index}\"");
    // "UNIQUE constraint failed: t.a, t.b"
    let columns: Vec<String> = message
        .split_once(": ")
        .map(|(_, list)| {
            list.split(", ")
                .filter_map(|c| c.rsplit_once('.').map(|(_, name)| name.trim().to_owned()))
                .collect()
        })
        .unwrap_or_default();
    if columns.is_empty() {
        return format!("{head}: {message}");
    }
    let list = columns
        .iter()
        .map(|c| ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {list} FROM {}.{} GROUP BY {list} HAVING count(*) > 1 LIMIT 1",
        ident(schema),
        ident(table)
    );
    match query(conn, &sql)
        .ok()
        .and_then(|rows| rows.into_iter().next())
    {
        Some(values) => format!(
            "{head}. Key ({})=({}) is duplicated.",
            columns.join(", "),
            values
                .iter()
                .map(|v| v.clone().unwrap_or_else(|| "null".to_owned()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => format!("{head}: {message}"),
    }
}

/// The first field of the first row of a text result.
fn first_text(outcome: &Outcome) -> Option<String> {
    let Outcome::Rows { rows, .. } = outcome else {
        return None;
    };
    let row = rows.first()?.as_ref().ok()?;
    // DataRow wire format: a 4-byte length (-1 for NULL), then the bytes.
    let data = row.data.as_ref();
    let len = i32::from_be_bytes(data.get(..4)?.try_into().ok()?);
    let len = usize::try_from(len).ok()?;
    String::from_utf8(data.get(4..4 + len)?.to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(status: &'static str) -> Job {
        Job {
            status,
            details: None,
            job_type: "INDEX_BUILD",
            object_id: 1,
            object_name: "public.i".to_owned(),
        }
    }

    /// FR-008: finished jobs older than 30 minutes disappear when a new job is recorded. The
    /// clock is the `now` argument of `record_job`, so the test moves it instead of waiting.
    #[test]
    fn finished_jobs_older_than_thirty_minutes_are_purged_on_the_next_job() {
        use crate::{open_database, Storage};
        let db = open_database(&Storage::Memory).unwrap();
        let conn = PgConnection::connect_with(&db, ConnectOpts::new()).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_794_480_000);
        let ago = |minutes: u64| now - Duration::from_secs(minutes * 60);
        let old_completed = record_job(&conn, ago(45), 1, &job("completed")).unwrap();
        let old_failed = record_job(&conn, ago(31), 2, &job("failed")).unwrap();
        // Just inside the retention, and an unfinished job of any age.
        let recent = record_job(&conn, ago(29), 3, &job("completed")).unwrap();
        let old_running = record_job(&conn, ago(90), 4, &job("processing")).unwrap();
        // Recording at `ago(45)` purged nothing that was younger; now the clock is at `now`.
        let new = record_job(&conn, now, 5, &job("completed")).unwrap();
        let ids: Vec<String> = query(&conn, "SELECT job_id FROM sys.jobs ORDER BY job_id")
            .unwrap()
            .into_iter()
            .filter_map(|row| row[0].clone())
            .collect();
        for gone in [&old_completed, &old_failed] {
            assert!(
                !ids.contains(gone),
                "{gone} should have been purged: {ids:?}"
            );
        }
        for kept in [&recent, &old_running, &new] {
            assert!(ids.contains(kept), "{kept} should be kept: {ids:?}");
        }
    }

    #[test]
    fn utc_literals() {
        let at = |secs: u64, micros: u32| UNIX_EPOCH + Duration::new(secs, micros * 1000);
        assert_eq!(utc_literal(at(0, 0)), "1970-01-01 00:00:00.000000");
        assert_eq!(
            utc_literal(at(951_782_400, 5)),
            "2000-02-29 00:00:00.000005"
        );
        assert_eq!(
            utc_literal(at(1_794_479_999, 999_999)),
            "2026-11-12 10:39:59.999999"
        );
        assert_eq!(
            utc_literal(at(1_709_251_199, 0)),
            "2024-02-29 23:59:59.000000"
        );
    }
}
