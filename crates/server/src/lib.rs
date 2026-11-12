// This file is derived from `postgres/server/lib.rs` of Turso
// (https://github.com/tursodatabase/turso), upstream commit
// d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5.
//
// Copyright 2024 the Turso authors
// Modifications copyright the micro-dsql authors
//
// MIT License
//
// Permission is hereby granted, free of charge, to any person obtaining a copy of
// this software and associated documentation files (the "Software"), to deal in
// the Software without restriction, including without limitation the rights to
// use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software is furnished to do so,
// subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
// FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
// COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
// IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
// CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
//
// The full upstream licence text is in crates/server/LICENSE-turso.
//
// Changes from upstream (micro-dsql M1):
// - our own accept loop and per-connection loop (`conn.rs`): Terminate closes the
//   connection (M0 blocker B4), SSL is refused, graceful shutdown on a future;
// - startup without an authentication exchange and with DSQL-shaped server parameters
//   (`startup.rs`, M0 blocker B9);
// - every engine call runs on the blocking thread pool, never on the async runtime;
// - `SHOW <dsql parameter>` and `SET timezone` to UTC are answered by the server, driven
//   by the backend-agnostic `micro-dsql-policy` crate;
// - the in-memory database is named `postgres`, so `current_database()` says so;
// - foreign keys are enforced in every session (M0 blocker B5);
// - Describe reports the real parameter count with inferred types (B1); binary
//   parameters are decoded (B2); result type OIDs and binary result encodings are fixed
//   for common types (B3, `binary.rs`);
// - parameter decoding errors use PostgreSQL SQLSTATEs (22P02/22P03) instead of XX000;
// - DSQL policy layer (M2): every statement runs through `dsql::run_planned` (rules,
//   transaction discipline and limits, SET whitelist, DDL rewrites from the `micro-dsql-policy`
//   crate); engine conflicts and lock contention are sent as DSQL's 40001 OC000/OC001 errors
//   (`55P03` is mapped to OC000 until autocommit runs as a transaction); errors raised by the
//   policy carry their own SQLSTATE, message and DETAIL;
// - M3 (DDL for ORMs): DSQL's `ASYNC` keyword is blanked before the syntax check and the
//   parameter analysis, job statements (`CREATE INDEX ASYNC`, `CALL sys.wait_for_job`, ...)
//   are answered through `sys.jobs` by `sys.rs`, DDL with prelude statements, cleanup or
//   constraint work runs through `ddl.rs`; `Outcome::TaggedRows` and `reply_rows` carry the
//   typed rows of those answers; `open_database` creates `sys.jobs`;
// - the upstream unit tests (which need Turso's internal test client) are not carried
//   over; crates/server/tests/smoke covers the behaviour.

mod binary;
mod conn;
mod constraints;
mod ddl;
mod dsql;
mod internal;
mod startup;
mod sys;

use std::fmt::Debug;
use std::net::SocketAddr;
use std::num::NonZero;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures::{stream, Sink, SinkExt};
use tokio::net::TcpListener;
use tracing::{debug, error, info};
use turso_core::{ConnectOpts, Database, DatabaseOpts, OpenFlags, Value};
use turso_pg::{PgConnection, StatementResultKind, TransactionState};
use turso_pg_parser::cmdtag::CommandTag;
use turso_pg_parser::copy::{parse_copy_to_stdout, CopyToStdout};
use turso_pg_parser::ParseResult;

use micro_dsql_policy::params::Column;
use micro_dsql_policy::{
    ColumnType, ParamHint, PolicyError, PolicyLimits, ReplyColumn, ResultHint, SessionPolicy,
    SettingCommand, StatementPlan, TxnStatus,
};

use pgwire::api::portal::{Format, Portal};
use pgwire::api::query::{
    send_execution_response, send_query_response, send_ready_for_query, ExtendedQueryHandler,
    SimpleQueryHandler,
};
use pgwire::api::results::{
    DataRowEncoder, DescribePortalResponse, DescribeStatementResponse, FieldFormat, FieldInfo,
    QueryResponse, Response, Tag,
};
use pgwire::api::stmt::{QueryParser, StoredStatement};
use pgwire::api::store::PortalStore;
use pgwire::api::{
    ClientInfo, ClientPortalStore, PgWireConnectionState, PgWireServerHandlers, Type,
};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::copy::{CopyData, CopyDone, CopyOutResponse};
use pgwire::messages::data::DataRow;
use pgwire::messages::extendedquery::{Describe, Execute};
use pgwire::messages::response::{
    EmptyQueryResponse, ErrorResponse, ReadyForQuery, TransactionStatus,
};
use pgwire::messages::simplequery::Query;
use pgwire::messages::PgWireBackendMessage;
use pgwire::types::format::FormatOptions;

/// Where the database lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storage {
    /// A fresh in-memory database named `postgres` (the default).
    Memory,
    /// A database file, created if missing.
    File(PathBuf),
}

impl std::fmt::Display for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Storage::Memory => f.write_str("memory"),
            Storage::File(path) => write!(f, "{}", path.display()),
        }
    }
}

/// The name of the one database a micro-dsql process serves.
pub const DATABASE_NAME: &str = "postgres";

/// Opens the database with the same options `tursopg` uses (so the M0 findings hold), on
/// Turso's Postgres dialect (always MVCC).
pub fn open_database(storage: &Storage) -> anyhow::Result<Arc<Database>> {
    let opts = DatabaseOpts::new()
        .with_views(true)
        .with_custom_types(true)
        .with_encryption(true)
        .with_index_method(true)
        .with_autovacuum(true)
        .with_attach(true);
    let db = match storage {
        // Turso names the database after the path's file stem (`current_database()`,
        // `pg_database`), so an in-memory database opened on a memory IO under a path
        // ending in `/postgres` is called `postgres`, as on DSQL. Nothing touches the
        // file system. The directory part is unique per database because Turso keeps a
        // process-wide registry keyed by path: two servers in one process (tests) must
        // not share or contend for one database.
        Storage::Memory => {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = format!(
                "micro-dsql-memory-{}-{n}/{DATABASE_NAME}",
                std::process::id()
            );
            turso_pg::open_database_with_io(
                Arc::new(turso_core::MemoryIO::new()),
                &path,
                OpenFlags::default(),
                opts,
            )?
        }
        Storage::File(path) => {
            let path = path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("database path is not UTF-8: {}", path.display()))?;
            turso_pg::open_database(path, None, OpenFlags::default(), opts)?.1
        }
    };
    sys::bootstrap(&db)?;
    Ok(db)
}

/// Options of a micro-dsql server.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServerOptions {
    /// Transaction limits (the defaults are DSQL's: 3,000 rows, 300 s). Tests lower them.
    pub limits: PolicyLimits,
}

/// A bound micro-dsql server.
pub struct Server {
    listener: TcpListener,
    db: Arc<Database>,
    options: ServerOptions,
}

impl Server {
    /// Opens the database and binds the listener. With port 0 the OS picks the port; read it
    /// back with [`Server::local_addr`].
    pub async fn bind(address: &str, storage: &Storage) -> anyhow::Result<Self> {
        Self::bind_with(address, storage, ServerOptions::default()).await
    }

    /// Like [`Server::bind`], with explicit options.
    pub async fn bind_with(
        address: &str,
        storage: &Storage,
        options: ServerOptions,
    ) -> anyhow::Result<Self> {
        let storage = storage.clone();
        let db = tokio::task::spawn_blocking(move || open_database(&storage)).await??;
        let listener = TcpListener::bind(address)
            .await
            .map_err(|e| anyhow::anyhow!("cannot listen on {address}: {e}"))?;
        Ok(Self {
            listener,
            db,
            options,
        })
    }

    /// The address the server is listening on.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accepts clients until `shutdown` completes. Each client gets its own session, with its
    /// own transaction and settings. Open connections are closed on shutdown.
    pub async fn serve<F>(self, shutdown: F) -> anyhow::Result<()>
    where
        F: std::future::Future<Output = ()>,
    {
        let mut connections = tokio::task::JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                result = self.listener.accept() => {
                    match result {
                        Ok((socket, addr)) => {
                            debug!("client connected from {addr}");
                            let db = self.db.clone();
                            let limits = self.options.limits;
                            connections.spawn(async move {
                                if let Err(e) = serve_connection(socket, db, limits).await {
                                    error!("error processing connection from {addr}: {e}");
                                }
                                debug!("client {addr} disconnected");
                            });
                        }
                        Err(e) => error!("error accepting connection: {e}"),
                    }
                }
                // Reap finished connection tasks so the set does not grow.
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
                _ = &mut shutdown => {
                    info!("shutting down");
                    break;
                }
            }
        }
        drop(self.listener);
        connections.shutdown().await;
        Ok(())
    }
}

async fn serve_connection(
    socket: tokio::net::TcpStream,
    db: Arc<Database>,
    limits: PolicyLimits,
) -> anyhow::Result<()> {
    let session = tokio::task::spawn_blocking(move || -> turso_pg::Result<PgConnection> {
        let session = PgConnection::connect_with(&db, ConnectOpts::new())?;
        // PostgreSQL always enforces foreign keys; Turso's SQLite heritage keeps them off
        // unless the connection asks (M0 blocker B5).
        session.inner().set_foreign_keys_enabled(true);
        Ok(session)
    })
    .await??;
    let handler = Arc::new(TursoPgHandler {
        conn: session.clone(),
        policy: Arc::new(Mutex::new(SessionPolicy::new(limits))),
        query_parser: Arc::new(TursoQueryParser {
            conn: session.clone(),
        }),
    });
    let factory = TursoPgFactory { handler };
    let result = conn::process_socket(socket, factory).await;
    // Closing rolls back an open transaction.
    tokio::task::spawn_blocking(move || {
        if let Err(e) = session.close() {
            error!("error closing a session: {e}");
        }
    })
    .await?;
    result.map_err(Into::into)
}

/// Runs engine work on the blocking thread pool so a slow statement never stalls the
/// network handling of other connections.
async fn blocking<T, F>(f: F) -> PgWireResult<T>
where
    F: FnOnce() -> PgWireResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| PgWireError::ApiError(Box::new(e)))?
}

/// A prepared statement as stored by the extended protocol.
#[derive(Debug, Clone)]
pub struct PgStatement {
    /// The policy's view of the statement: rules, class, text for the engine.
    plan: Arc<StatementPlan>,
    /// One per parameter: the client-declared type, else the inferred one; `None` when
    /// nothing is known (described as `text`, bound by guessing from the value).
    param_types: Vec<Option<Type>>,
    result_hints: Vec<Option<ResultHint>>,
}

impl PgStatement {
    fn analyze(conn: &PgConnection, sql: String, declared: &[Option<Type>]) -> Self {
        let shape = micro_dsql_policy::analyze(&micro_dsql_policy::rewrite::pre_parse(&sql).text);
        let count = shape.params.len().max(declared.len());
        let param_types = (0..count)
            .map(|i| match declared.get(i) {
                Some(Some(t)) if *t != Type::UNKNOWN => Some(t.clone()),
                _ => shape
                    .params
                    .get(i)
                    .and_then(|hint| resolve_param_hint(conn, hint)),
            })
            .collect();
        Self {
            plan: Arc::new(StatementPlan::new(&sql)),
            result_hints: shape.results,
            param_types,
        }
    }
}

/// Parses statements for the extended protocol: analyses parameters and intercepts once,
/// at Parse time.
pub struct TursoQueryParser {
    conn: PgConnection,
}

#[async_trait]
impl QueryParser for TursoQueryParser {
    type Statement = PgStatement;

    async fn parse_sql<C>(
        &self,
        _client: &C,
        sql: &str,
        types: &[Option<Type>],
    ) -> PgWireResult<Self::Statement>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        let conn = self.conn.clone();
        let sql = sql.to_owned();
        let types = types.to_vec();
        blocking(move || Ok(PgStatement::analyze(&conn, sql, &types))).await
    }
}

struct TursoPgHandler {
    conn: PgConnection,
    /// Policy state of this connection; locked only inside blocking closures.
    policy: Arc<Mutex<SessionPolicy>>,
    query_parser: Arc<TursoQueryParser>,
}

struct TursoPgFactory {
    handler: Arc<TursoPgHandler>,
}

impl PgWireServerHandlers for TursoPgFactory {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.handler.clone()
    }

    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        self.handler.clone()
    }

    fn startup_handler(&self) -> Arc<impl pgwire::api::auth::StartupHandler> {
        Arc::new(startup::DsqlStartupHandler)
    }
}

/// The result of running one statement, computed off the runtime and sent afterwards.
enum Outcome {
    Rows {
        header: Arc<Vec<FieldInfo>>,
        rows: Vec<PgWireResult<DataRow>>,
    },
    Command(Tag),
    /// Rows answered with a command tag of their own (`CREATE INDEX` plus a `job_id` row).
    TaggedRows {
        tag: String,
        header: Arc<Vec<FieldInfo>>,
        rows: Vec<PgWireResult<DataRow>>,
    },
    Copy {
        column_count: i16,
        rows: Vec<Vec<u8>>,
        /// A read error after `rows`: the rows are sent, then the error.
        error: Option<turso_pg::Error>,
    },
}

impl Outcome {
    fn into_response(self) -> Response {
        match self {
            Outcome::Rows { header, rows } => {
                Response::Query(QueryResponse::new(header, stream::iter(rows)))
            }
            Outcome::TaggedRows { tag, header, rows } => {
                let mut response = QueryResponse::new(header, stream::iter(rows));
                response.set_command_tag(&tag);
                Response::Query(response)
            }
            Outcome::Command(tag) => Response::Execution(tag),
            Outcome::Copy { .. } => unreachable!("COPY is only run by the simple protocol"),
        }
    }
}

#[async_trait]
impl SimpleQueryHandler for TursoPgHandler {
    async fn on_query<C>(&self, client: &mut C, query: Query) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let sql = query.query.clone();
        let result = self.run_simple_query(client, query).await;
        let status = wire_transaction_status(&self.conn);
        finish_query(client, result, false, status, Some(&sql)).await
    }

    async fn do_query<C>(&self, _client: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let conn = self.conn.clone();
        let policy = self.policy.clone();
        let query = query.to_owned();
        blocking(move || {
            // Per the PostgreSQL simple query protocol, a query string may contain
            // multiple semicolon-separated statements. Split and execute each one.
            let statements = turso_pg::split_statements(&query).map_err(pg_error)?;
            let plans = plan_statements(&conn, &policy, &statements)?;
            plans
                .iter()
                .map(|plan| run_simple_statement(&conn, &policy, plan).map(Outcome::into_response))
                .collect()
        })
        .await
    }
}

impl TursoPgHandler {
    async fn run_simple_query<C>(&self, client: &mut C, query: Query) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if !matches!(client.state(), PgWireConnectionState::ReadyForQuery) {
            return Err(PgWireError::NotReadyForQuery);
        }
        client.set_state(PgWireConnectionState::QueryInProgress);
        let conn = self.conn.clone();
        let policy = self.policy.clone();
        let text = query.query;
        let plans = blocking(move || {
            // Errors raised here never reach PgConnection::prepare, so report
            // them to the session: any error inside a transaction block aborts
            // the block.
            let parse_error = |error: turso_pg_parser::ParseError| {
                conn.abort_transaction_block_on_error();
                pg_error(turso_core::LimboError::ParseError(error.to_string()).into())
            };
            // DSQL's `ASYNC` keyword is blanked for the syntax check; the statements are
            // split from the client's text and planned (blanked again) one by one.
            let checked = micro_dsql_policy::rewrite::pre_parse(&text);
            turso_pg_parser::parse(&checked.text).map_err(parse_error)?;
            let statements = turso_pg_parser::split_statements(&text).map_err(parse_error)?;
            plan_statements(&conn, &policy, &statements)
        })
        .await?;
        if plans.is_empty() {
            client
                .feed(PgWireBackendMessage::EmptyQueryResponse(
                    EmptyQueryResponse::new(),
                ))
                .await?;
        }
        for plan in plans {
            let conn = self.conn.clone();
            let policy = self.policy.clone();
            let outcome = blocking(move || run_simple_statement(&conn, &policy, &plan)).await?;
            match outcome {
                Outcome::Copy {
                    column_count,
                    rows,
                    error,
                } => send_copy_out(client, column_count, rows, error).await?,
                outcome => match outcome.into_response() {
                    Response::Query(mut result) => {
                        send_query_response(client, &mut result, true).await?
                    }
                    Response::Execution(tag) => send_execution_response(client, tag).await?,
                    _ => unreachable!("simple queries return query or execution responses"),
                },
            }
            client.set_transaction_status(wire_transaction_status(&self.conn));
        }
        client.set_state(PgWireConnectionState::ReadyForQuery);
        send_ready_for_query(client, client.transaction_status()).await
    }
}

/// Plans every statement of a simple query; a string of several statements is checked as one
/// implicit transaction before any of them runs (blocking).
fn plan_statements(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    statements: &[String],
) -> PgWireResult<Vec<StatementPlan>> {
    let plans: Vec<StatementPlan> = statements
        .iter()
        .map(|sql| StatementPlan::new(sql))
        .collect();
    if plans.len() > 1 && dsql::txn_status(conn) == TxnStatus::Idle {
        let result = policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .check_batch(&plans);
        result.map_err(dsql::policy_error)?;
    }
    Ok(plans)
}

/// Runs one statement of a simple query (blocking).
fn run_simple_statement(
    conn: &PgConnection,
    policy: &Mutex<SessionPolicy>,
    plan: &StatementPlan,
) -> PgWireResult<Outcome> {
    dsql::run_planned(conn, policy, plan, &Format::UnifiedText, |sql| {
        let copy = parse_copy_to_stdout(sql).map_err(|error| {
            conn.abort_transaction_block_on_error();
            pg_error(turso_core::LimboError::ParseError(error.to_string()).into())
        })?;
        if let Some(copy) = copy {
            return copy_to_stdout(conn, &copy).map(|outcome| (outcome, 0));
        }
        let mut stmt = conn.prepare(sql).map_err(pg_error)?;
        let parsed = parse_statement(sql)?;
        if stmt.result_kind() != StatementResultKind::Rows {
            execute_non_query(&mut stmt, &parsed)
        } else {
            let hints = micro_dsql_policy::analyze(sql).results;
            let header = Arc::new(build_field_info(&stmt, &Format::UnifiedText, &hints));
            execute_query(&mut stmt, header)
        }
    })
}

fn show_fields(column: &str, format: &Format) -> Vec<FieldInfo> {
    vec![FieldInfo::new(
        column.to_owned(),
        None,
        None,
        Type::TEXT,
        format.format_for(0),
    )]
}

/// The result fields of a statement the policy or the server answers itself.
fn reply_fields(columns: &[ReplyColumn], format: &Format) -> Vec<FieldInfo> {
    columns
        .iter()
        .enumerate()
        .map(|(i, column)| {
            let ty = match column.ty {
                ColumnType::Text => Type::TEXT,
                ColumnType::Bool => Type::BOOL,
                ColumnType::Int4 => Type::INT4,
            };
            FieldInfo::new(column.name.clone(), None, None, ty, format.format_for(i))
        })
        .collect()
}

/// A result set built from text values (`t`/`f` for booleans).
fn reply_rows(
    tag: &str,
    columns: &[ReplyColumn],
    rows: &[Vec<Option<String>>],
    format: &Format,
) -> PgWireResult<Outcome> {
    let header = Arc::new(reply_fields(columns, format));
    let mut encoded = Vec::with_capacity(rows.len());
    for row in rows {
        let mut encoder = DataRowEncoder::new(header.clone());
        for (column, value) in columns.iter().zip(row) {
            match column.ty {
                ColumnType::Text => encoder.encode_field(&value.as_deref())?,
                ColumnType::Bool => encoder.encode_field(&value.as_deref().map(|v| v == "t"))?,
                ColumnType::Int4 => {
                    encoder.encode_field(&value.as_deref().and_then(|v| v.parse::<i32>().ok()))?
                }
            }
        }
        encoded.push(encoder.finish());
    }
    Ok(Outcome::TaggedRows {
        tag: tag.to_owned(),
        header,
        rows: encoded,
    })
}

/// Runs `COPY ... TO STDOUT` (blocking), collecting the encoded rows.
fn copy_to_stdout(conn: &PgConnection, copy: &CopyToStdout) -> PgWireResult<Outcome> {
    let mut stmt = conn.prepare_copy_to_stdout(copy).map_err(pg_error)?;
    let mut has_row = stmt
        .run_one_step_blocking(|| Ok(()), || Ok(()))
        .map_err(pg_error)?
        .is_some();
    let encoder = turso_pg::CopyTextEncoder::new(&stmt);
    let column_count = i16::try_from(stmt.num_columns())
        .map_err(|error| PgWireError::UserError(Box::new(error_info(&error.to_string()))))?;
    let mut rows = Vec::new();
    let mut error = None;
    while has_row {
        let mut data = Vec::new();
        encoder.encode_row(stmt.row().expect("COPY row is present"), &mut data);
        rows.push(data);
        match stmt.run_one_step_blocking(|| Ok(()), || Ok(())) {
            Ok(step) => has_row = step.is_some(),
            Err(e) => {
                error = Some(e);
                break;
            }
        }
    }
    Ok(Outcome::Copy {
        column_count,
        rows,
        error,
    })
}

async fn send_copy_out<C>(
    client: &mut C,
    column_count: i16,
    rows: Vec<Vec<u8>>,
    error: Option<turso_pg::Error>,
) -> PgWireResult<()>
where
    C: Sink<PgWireBackendMessage> + Unpin,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    client
        .send(PgWireBackendMessage::CopyOutResponse(CopyOutResponse::new(
            0,
            column_count,
            vec![0; usize::try_from(column_count).unwrap_or(0)],
        )))
        .await?;
    let row_count = rows.len();
    for data in rows {
        client
            .send(PgWireBackendMessage::CopyData(CopyData::new(data.into())))
            .await?;
    }
    if let Some(error) = error {
        return Err(pg_error(error));
    }
    client
        .send(PgWireBackendMessage::CopyDone(CopyDone::new()))
        .await?;
    send_execution_response(client, Tag::new("COPY").with_rows(row_count)).await
}

#[async_trait]
impl ExtendedQueryHandler for TursoPgHandler {
    type Statement = PgStatement;
    type QueryParser = TursoQueryParser;

    async fn on_execute<C>(&self, client: &mut C, message: Execute) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let sql = client
            .portal_store()
            .get_portal(message.name.as_deref().unwrap_or(pgwire::api::DEFAULT_NAME))
            .map(|portal| portal.statement.statement.plan.original.clone());
        let result = self._on_execute(client, message).await;
        let status = wire_transaction_status(&self.conn);
        finish_query(client, result, true, status, sql.as_deref()).await
    }

    async fn on_describe<C>(&self, client: &mut C, message: Describe) -> PgWireResult<()>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let result = self._on_describe(client, message).await;
        let status = wire_transaction_status(&self.conn);
        finish_query(client, result, true, status, None).await
    }

    fn query_parser(&self) -> Arc<Self::QueryParser> {
        self.query_parser.clone()
    }

    async fn do_query<C>(
        &self,
        _client: &mut C,
        portal: &Portal<Self::Statement>,
        _max_rows: usize,
    ) -> PgWireResult<Response>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let conn = self.conn.clone();
        let policy = self.policy.clone();
        let statement = portal.statement.statement.clone();
        let parameters = portal.parameters.clone();
        let parameter_format = portal.parameter_format.clone();
        let result_format = portal.result_column_format.clone();
        blocking(move || {
            let plan = statement.plan.clone();
            dsql::run_planned(&conn, &policy, &plan, &result_format, |sql| {
                let mut stmt = conn.prepare(sql).map_err(pg_error)?;

                // Bind parameters from the portal
                bind_portal_parameters(&mut stmt, &statement, &parameters, &parameter_format)?;

                let parsed = parse_statement(sql)?;
                if stmt.result_kind() != StatementResultKind::Rows {
                    return execute_non_query(&mut stmt, &parsed);
                }

                let header = Arc::new(build_field_info(
                    &stmt,
                    &result_format,
                    &statement.result_hints,
                ));
                execute_query(&mut stmt, header)
            })
            .map(Outcome::into_response)
            .map(|response| with_transaction_state(&conn, response))
        })
        .await
    }

    async fn do_describe_statement<C>(
        &self,
        _client: &mut C,
        target: &StoredStatement<Self::Statement>,
    ) -> PgWireResult<DescribeStatementResponse>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let conn = self.conn.clone();
        let statement = target.statement.clone();
        blocking(move || {
            let param_types: Vec<Type> = statement
                .param_types
                .iter()
                .map(|t| t.clone().unwrap_or(Type::TEXT))
                .collect();
            let fields = describe(&conn, &statement, &Format::UnifiedText)?;
            Ok(DescribeStatementResponse::new(param_types, fields))
        })
        .await
    }

    async fn do_describe_portal<C>(
        &self,
        _client: &mut C,
        portal: &Portal<Self::Statement>,
    ) -> PgWireResult<DescribePortalResponse>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let conn = self.conn.clone();
        let statement = portal.statement.statement.clone();
        let format = portal.result_column_format.clone();
        blocking(move || describe(&conn, &statement, &format).map(DescribePortalResponse::new))
            .await
    }
}

/// The result fields of a prepared statement (blocking).
fn describe(
    conn: &PgConnection,
    statement: &PgStatement,
    format: &Format,
) -> PgWireResult<Vec<FieldInfo>> {
    // Inside a failed transaction block the engine answers every statement with 25P02.
    if dsql::txn_status(conn) != TxnStatus::Aborted {
        let plan = &statement.plan;
        // A statement the policy refuses has no result; Execute reports the refusal (the engine
        // might not even prepare it, e.g. COPY ... FROM STDIN or DO).
        if plan.refusal.is_some() {
            return Ok(vec![]);
        }
        if let Some(columns) = plan.reply_columns() {
            return Ok(reply_fields(&columns, format));
        }
        // DDL the server runs itself returns no rows, and the engine may not even prepare the
        // text it carries (`ADD CONSTRAINT ... NOT VALID`, a refused `DROP COLUMN`).
        let answered_here = plan.ddl.is_some()
            || match &plan.setting {
                Some(SettingCommand::ResetAll) | None => plan.intercept.is_some(),
                Some(_) => true,
            };
        if answered_here {
            return Ok(vec![]);
        }
    }
    let stmt = conn.prepare(&statement.plan.engine_sql).map_err(pg_error)?;
    Ok(describe_fields(&stmt, format, &statement.result_hints))
}

/// The fields of an ordinary result set, or none for commands and COPY.
fn describe_fields(
    stmt: &turso_pg::Statement,
    format: &Format,
    hints: &[Option<ResultHint>],
) -> Vec<FieldInfo> {
    if stmt.result_kind() != StatementResultKind::Rows {
        return vec![];
    }
    build_field_info(stmt, format, hints)
}

fn parse_statement(sql: &str) -> PgWireResult<ParseResult> {
    turso_pg_parser::parse(sql)
        .map_err(|error| pg_error(turso_core::LimboError::ParseError(error.to_string()).into()))
}

/// Build FieldInfo metadata from a prepared statement's column information.
///
/// `hints` (one per column, from the AST) win over the engine for types PostgreSQL fixes
/// regardless of the operands: `count()` is `bigint`, comparisons are `boolean`.
fn build_field_info(
    stmt: &turso_pg::Statement,
    format: &Format,
    hints: &[Option<ResultHint>],
) -> Vec<FieldInfo> {
    let hints = if hints.len() == stmt.num_columns() {
        hints
    } else {
        &[]
    };
    (0..stmt.num_columns())
        .map(|i| {
            let name = stmt.get_column_name(i).into_owned();
            let pg_type = match hints.get(i).copied().flatten() {
                Some(ResultHint::Int8) => Type::INT8,
                Some(ResultHint::Bool) => Type::BOOL,
                None => resolve_pg_type_for_column(stmt, i).unwrap_or(Type::TEXT),
            };
            FieldInfo::new(name, None, None, pg_type, format.format_for(i))
        })
        .collect()
}

/// Decide the PG wire type for a result column.
///
/// `get_column_type_info` is the single source of truth: it handles direct
/// table-column references (declared name, array depth, custom-type kind,
/// resolved primitive), bare literals (`SELECT 42` -> INTEGER), and typed
/// expressions like CAST. When it returns `Ok(None)` (no determined primitive)
/// or `Err` (custom types not enabled — won't happen in PG mode, but the wire
/// layer shouldn't panic if it does), the type is unknown and the caller picks
/// a default (TEXT); `encode_value` already handles per-value type mismatches.
fn resolve_pg_type_for_column(stmt: &turso_pg::Statement, idx: usize) -> Option<Type> {
    use turso_core::ColumnTypeKind;

    let info = stmt.get_column_type_info(idx).ok().flatten()?;
    // STRUCT and UNION columns live as BLOBs on disk, but exposing them as
    // BYTEA would force clients to deal with raw bytes. Map them to JSONB so
    // libpq/psql/JDBC see structured data they can introspect.
    let mut base = match info.kind {
        ColumnTypeKind::Struct | ColumnTypeKind::Union => Type::JSONB,
        _ => {
            // Prefer the declared name (the user-visible type), then fall
            // back to the resolved base for custom/domain types whose
            // declared name isn't in the lookup table.
            let mapped = sqlite_type_to_pg_type(&info.declared_name);
            if mapped == Type::TEXT {
                info.base_type
                    .as_deref()
                    .map(sqlite_type_to_pg_type)
                    .unwrap_or(Type::TEXT)
            } else {
                mapped
            }
        }
    };
    if info.array_dimensions > 0 {
        base = scalar_pg_type_to_array_type(&base);
    }
    Some(base)
}

/// Resolve a parameter hint from the AST to a wire type (blocking: column hints prepare a
/// probe query against the table).
fn resolve_param_hint(conn: &PgConnection, hint: &ParamHint) -> Option<Type> {
    match hint {
        ParamHint::Unknown => None,
        ParamHint::Int8 => Some(Type::INT8),
        ParamHint::Cast(name) => {
            let (scalar, array) = match name.strip_suffix("[]") {
                Some(scalar) => (scalar, true),
                None => (name.as_str(), false),
            };
            let base = pg_type_by_name(scalar)?;
            Some(if array {
                scalar_pg_type_to_array_type(&base)
            } else {
                base
            })
        }
        ParamHint::Column { tables, column } => tables.iter().find_map(|table| {
            let table_sql = table
                .split('.')
                .map(turso_pg_parser::quote_identifier)
                .collect::<Vec<_>>()
                .join(".");
            let (sql, idx) = match column {
                Column::Name(name) => (
                    format!(
                        "SELECT {} FROM {table_sql} LIMIT 0",
                        turso_pg_parser::quote_identifier(name)
                    ),
                    0,
                ),
                Column::Position(i) => (format!("SELECT * FROM {table_sql} LIMIT 0"), *i),
            };
            let stmt = conn.prepare(&sql).ok()?;
            if idx >= stmt.num_columns() {
                return None;
            }
            resolve_pg_type_for_column(&stmt, idx)
        }),
    }
}

/// Map a type name as written in SQL (`$1::int8`) to a wire type.
fn pg_type_by_name(name: &str) -> Option<Type> {
    let t = match name {
        "int2" | "smallint" => Type::INT2,
        "int4" | "int" | "integer" => Type::INT4,
        "int8" | "bigint" => Type::INT8,
        "float4" | "real" => Type::FLOAT4,
        "float8" | "double precision" => Type::FLOAT8,
        "numeric" | "decimal" => Type::NUMERIC,
        "bool" | "boolean" => Type::BOOL,
        "text" => Type::TEXT,
        "varchar" => Type::VARCHAR,
        "bpchar" | "char" => Type::BPCHAR,
        "name" => Type::NAME,
        "bytea" => Type::BYTEA,
        "uuid" => Type::UUID,
        "json" => Type::JSON,
        "jsonb" => Type::JSONB,
        "date" => Type::DATE,
        "time" => Type::TIME,
        "timestamp" => Type::TIMESTAMP,
        "timestamptz" => Type::TIMESTAMPTZ,
        "interval" => Type::INTERVAL,
        "oid" => Type::OID,
        "regclass" => Type::REGCLASS,
        _ => return None,
    };
    Some(t)
}

/// Map a scalar PG type to its array counterpart.
fn scalar_pg_type_to_array_type(scalar: &Type) -> Type {
    if *scalar == Type::INT2 {
        Type::INT2_ARRAY
    } else if *scalar == Type::INT4 {
        Type::INT4_ARRAY
    } else if *scalar == Type::INT8 {
        Type::INT8_ARRAY
    } else if *scalar == Type::FLOAT8 {
        Type::FLOAT8_ARRAY
    } else if *scalar == Type::BOOL {
        Type::BOOL_ARRAY
    } else if *scalar == Type::TEXT || *scalar == Type::VARCHAR {
        Type::TEXT_ARRAY
    } else if *scalar == Type::UUID {
        Type::UUID_ARRAY
    } else if *scalar == Type::JSON {
        Type::JSON_ARRAY
    } else if *scalar == Type::JSONB {
        Type::JSONB_ARRAY
    } else if *scalar == Type::DATE {
        Type::DATE_ARRAY
    } else if *scalar == Type::TIME {
        Type::TIME_ARRAY
    } else if *scalar == Type::TIMESTAMP {
        Type::TIMESTAMP_ARRAY
    } else if *scalar == Type::TIMESTAMPTZ {
        Type::TIMESTAMPTZ_ARRAY
    } else if *scalar == Type::INET {
        Type::INET_ARRAY
    } else if *scalar == Type::CIDR {
        Type::CIDR_ARRAY
    } else if *scalar == Type::MACADDR {
        Type::MACADDR_ARRAY
    } else if *scalar == Type::MACADDR8 {
        Type::MACADDR8_ARRAY
    } else if *scalar == Type::NUMERIC {
        Type::NUMERIC_ARRAY
    } else if *scalar == Type::BYTEA {
        Type::BYTEA_ARRAY
    } else if *scalar == Type::FLOAT4 {
        Type::FLOAT4_ARRAY
    } else {
        Type::TEXT_ARRAY
    }
}

/// Execute a query that returns rows and collect its rows.
fn execute_query(
    stmt: &mut turso_pg::Statement,
    header: Arc<Vec<FieldInfo>>,
) -> PgWireResult<(Outcome, u64)> {
    let mut rows: Vec<PgWireResult<DataRow>> = Vec::new();
    let header_clone = header.clone();

    stmt.run_with_row_callback(|row| {
        let mut encoder = DataRowEncoder::new(header_clone.clone());
        for (i, val) in row.get_values().enumerate() {
            let (pg_type, format) = header_clone
                .get(i)
                .map(|fi| (fi.datatype().clone(), fi.format()))
                .unwrap_or((Type::TEXT, FieldFormat::Text));
            encode_value(&mut encoder, val, &pg_type, format)?;
        }
        rows.push(encoder.finish());
        Ok(())
    })
    .map_err(pg_error)?;

    let changed = u64::try_from(stmt.n_change()).unwrap_or(0);
    Ok((Outcome::Rows { header, rows }, changed))
}

/// Execute a non-SELECT statement and build its command tag.
fn execute_non_query(
    stmt: &mut turso_pg::Statement,
    parsed: &ParseResult,
) -> PgWireResult<(Outcome, u64)> {
    stmt.run_ignore_rows().map_err(pg_error)?;

    let affected = stmt.n_change() as usize;
    // A COMMIT that ended an aborted block performed a rollback; its tag
    // cannot come from the statement text.
    let tag = match stmt.command_tag_override() {
        Some(tag) => Tag::new(tag),
        None => wire_tag(CommandTag::of(parsed), affected),
    };
    Ok((Outcome::Command(tag), u64::try_from(affected).unwrap_or(0)))
}

fn wire_tag(tag: CommandTag, affected_rows: usize) -> Tag {
    match tag {
        // INSERT also reports the OID of the inserted row, which is always 0
        // because tables have no OIDs.
        CommandTag::Insert => Tag::new(tag.name()).with_oid(0).with_rows(affected_rows),
        _ if tag.has_row_count() => Tag::new(tag.name()).with_rows(affected_rows),
        _ => Tag::new(tag.name()),
    }
}

/// Reflect the session's transaction state onto the wire protocol: pgwire
/// derives the ReadyForQuery status byte from TransactionStart and
/// TransactionEnd responses, so report which side of a transaction block the
/// statement left the session on.
fn with_transaction_state(conn: &PgConnection, response: Response) -> Response {
    let Response::Execution(tag) = response else {
        return response;
    };
    match conn.transaction_state() {
        TransactionState::Idle => Response::TransactionEnd(tag),
        TransactionState::Active | TransactionState::Aborted => Response::TransactionStart(tag),
    }
}

/// The ReadyForQuery status byte for the session's current transaction
/// state: 'I' idle, 'T' in a transaction block, 'E' in a failed one.
fn wire_transaction_status(conn: &PgConnection) -> TransactionStatus {
    match conn.transaction_state() {
        TransactionState::Idle => TransactionStatus::Idle,
        TransactionState::Active => TransactionStatus::Transaction,
        TransactionState::Aborted => TransactionStatus::Error,
    }
}

/// Bind a portal's parameters to a prepared statement.
///
/// PostgreSQL parameters ($1, $2, ...) map to portal parameters 0, 1, ...
/// The bytecode compiler may allocate internal parameter indices in a different
/// order than the $N numbering (e.g. if $2 appears before $1 in the SQL), so we
/// look up each parameter's internal index by name.
fn bind_portal_parameters(
    stmt: &mut turso_pg::Statement,
    statement: &PgStatement,
    parameters: &[Option<Bytes>],
    format: &Format,
) -> PgWireResult<()> {
    for (i, parameter) in parameters.iter().enumerate() {
        let value = match parameter {
            None => Value::Null,
            Some(bytes) => {
                let pg_type = statement.param_types.get(i).cloned().flatten();
                match format.format_for(i) {
                    FieldFormat::Binary => binary::decode_param(bytes, pg_type.as_ref())?,
                    FieldFormat::Text => {
                        pg_bytes_to_value(bytes, pg_type.as_ref().unwrap_or(&Type::UNKNOWN))?
                    }
                }
            }
        };
        // Portal parameter i corresponds to PostgreSQL $N where N = i + 1.
        // Look up the internal index that the bytecode compiler assigned to $N.
        let pg_param_name = format!("${}", i + 1);
        let idx = stmt
            .parameter_index(&pg_param_name)
            .unwrap_or_else(|| NonZero::new(i + 1).expect("parameter index must be non-zero"));
        // Ignore bind errors: parameter index mismatches or value coercion
        // failures surface as wire-protocol errors during the subsequent
        // execute, with a more useful message than a generic Bind failure.
        let _ = stmt.bind_at(idx, value);
    }
    Ok(())
}

/// Convert raw text-format parameter bytes to a turso Value based on the PostgreSQL type.
fn pg_bytes_to_value(bytes: &[u8], pg_type: &Type) -> PgWireResult<Value> {
    let text = std::str::from_utf8(bytes).map_err(|e| {
        PgWireError::UserError(Box::new(error_info_code(
            "22021",
            &format!("invalid UTF-8 in parameter: {e}"),
        )))
    })?;
    let invalid = |type_name: &str| {
        PgWireError::UserError(Box::new(error_info_code(
            "22P02",
            &format!("invalid input syntax for type {type_name}: \"{text}\""),
        )))
    };

    match *pg_type {
        Type::INT2 | Type::INT4 | Type::INT8 => {
            let i: i64 = text
                .trim()
                .parse()
                .map_err(|_| invalid(pg_type_display(pg_type)))?;
            Ok(Value::from_i64(i))
        }
        Type::FLOAT4 | Type::FLOAT8 | Type::NUMERIC => {
            let f: f64 = text
                .trim()
                .parse()
                .map_err(|_| invalid(pg_type_display(pg_type)))?;
            Ok(Value::from_f64(f))
        }
        Type::BOOL => match text.trim().to_ascii_lowercase().as_str() {
            "t" | "true" | "1" | "yes" | "y" | "on" => Ok(Value::from_i64(1)),
            "f" | "false" | "0" | "no" | "n" | "off" => Ok(Value::from_i64(0)),
            _ => Err(invalid("boolean")),
        },
        Type::BYTEA => {
            // PostgreSQL text format for bytea uses \x hex encoding
            if let Some(hex_str) = text.strip_prefix("\\x") {
                let data = decode_hex(hex_str).map_err(|_| invalid("bytea"))?;
                Ok(Value::from_blob(data))
            } else {
                // Raw bytes as-is
                Ok(Value::from_blob(bytes.to_vec()))
            }
        }
        // UNKNOWN: try to infer type from text content (numeric-looking values
        // should be bound as numbers so comparisons with COUNT/SUM etc. work)
        Type::UNKNOWN => {
            if let Ok(i) = text.parse::<i64>() {
                Ok(Value::from_i64(i))
            } else if let Ok(f) = text.parse::<f64>() {
                Ok(Value::from_f64(f))
            } else if text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("t") {
                Ok(Value::from_i64(1))
            } else if text.eq_ignore_ascii_case("false") || text.eq_ignore_ascii_case("f") {
                Ok(Value::from_i64(0))
            } else {
                Ok(Value::from_text(text.to_owned()))
            }
        }
        // TEXT, VARCHAR, and all other types → text
        _ => Ok(Value::from_text(text.to_owned())),
    }
}

fn pg_type_display(pg_type: &Type) -> &str {
    match *pg_type {
        Type::INT2 => "smallint",
        Type::INT4 => "integer",
        Type::INT8 => "bigint",
        Type::FLOAT4 => "real",
        Type::FLOAT8 => "double precision",
        _ => pg_type.name(),
    }
}

/// Decode a hex string into bytes.
fn decode_hex(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err("odd-length hex string".to_owned());
    }
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .map(|(pair_index, pair)| {
            let position = 2 * pair_index;
            let high = decode_hex_digit(pair[0], position)?;
            let low = decode_hex_digit(pair[1], position + 1)?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn decode_hex_digit(byte: u8, position: usize) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("invalid hex at position {position}")),
    }
}

fn encode_value(
    encoder: &mut DataRowEncoder,
    val: &Value,
    pg_type: &Type,
    format: FieldFormat,
) -> turso_core::Result<()> {
    let internal = |e: PgWireError| turso_core::LimboError::InternalError(e.to_string());
    if format == FieldFormat::Binary {
        if let Some(bytes) = binary::encode_result(val, pg_type) {
            return encoder
                .encode_field_with_type_and_format(
                    &bytes.as_slice(),
                    &Type::BYTEA,
                    FieldFormat::Binary,
                    &FormatOptions::default(),
                )
                .map_err(internal);
        }
    }
    match val {
        Value::Null => encoder.encode_field(&None::<i8>).map_err(internal),
        Value::Numeric(n) if format == FieldFormat::Binary => {
            encode_binary_number(encoder, n, pg_type).map_err(internal)
        }
        Value::Numeric(turso_core::Numeric::Integer(i)) => {
            // Boolean columns: encode as true/false instead of 0/1
            if *pg_type == Type::BOOL {
                encoder.encode_field(&(*i != 0)).map_err(internal)
            } else {
                encoder.encode_field(i).map_err(internal)
            }
        }
        Value::Numeric(turso_core::Numeric::Float(f)) => {
            encoder.encode_field(&f64::from(*f)).map_err(internal)
        }
        Value::Text(t) => {
            let text = t.value.as_ref();
            // For TIMESTAMPTZ columns, ensure timezone info is present so clients
            // parse the value correctly (as UTC, not local time).
            // TIMESTAMP (without TZ) should NOT have timezone suffix.
            if *pg_type == Type::TIMESTAMPTZ
                && !text.contains('+')
                && !text.contains('Z')
                && !text.ends_with("-00")
            {
                let with_tz = format!("{text}+00");
                encoder.encode_field(&with_tz.as_str()).map_err(internal)
            } else if pg_type.name().starts_with('_') {
                // Array types: pgwire's to_sql_text quotes strings containing
                // {, }, or commas when the type is Kind::Array. Since we store
                // array values as pre-formatted PG array literals (e.g.
                // "{1,2,3}"), encode with Type::TEXT to bypass the quoting.
                encoder
                    .encode_field_with_type_and_format(
                        &text,
                        &Type::TEXT,
                        FieldFormat::Text,
                        &FormatOptions::default(),
                    )
                    .map_err(internal)
            } else {
                encoder.encode_field(&text).map_err(internal)
            }
        }
        Value::Blob(b) => encoder.encode_field(&b.as_slice()).map_err(internal),
    }
}

/// Binary format writes a number with the width of its Rust type, so the
/// encoding follows the column's type, not the stored value. For a type
/// without a binary number encoding here, the value is sent as its text, the
/// same bytes as in text format.
fn encode_binary_number(
    encoder: &mut DataRowEncoder,
    n: &turso_core::Numeric,
    pg_type: &Type,
) -> PgWireResult<()> {
    use turso_core::Numeric::{Float, Integer};
    match *n {
        Integer(i) if *pg_type == Type::BOOL => encoder.encode_field(&(i != 0)),
        // A value outside the i32 range, stored in an int column, keeps
        // the i64 encoding as before, which binary clients still misread.
        Integer(i) if *pg_type == Type::INT4 => match i32::try_from(i) {
            Ok(i) => encoder.encode_field(&i),
            Err(_) => encoder.encode_field(&i),
        },
        Integer(i) if *pg_type == Type::INT8 => encoder.encode_field(&i),
        Integer(i) if *pg_type == Type::FLOAT8 => encoder.encode_field(&(i as f64)),
        Float(f) if *pg_type == Type::FLOAT8 => encoder.encode_field(&f64::from(f)),
        Integer(i) => encoder.encode_field_with_type_and_format(
            &i,
            pg_type,
            FieldFormat::Text,
            &FormatOptions::default(),
        ),
        Float(f) => encoder.encode_field_with_type_and_format(
            &f64::from(f),
            pg_type,
            FieldFormat::Text,
            &FormatOptions::default(),
        ),
    }
}

fn sqlite_type_to_pg_type(type_str: &str) -> Type {
    let upper = type_str.to_uppercase();
    match upper.as_str() {
        "INTEGER" | "INT" | "INT4" | "SERIAL" => Type::INT4,
        "SMALLINT" | "INT2" | "SMALLSERIAL" => Type::INT2,
        "BIGINT" | "INT8" | "BIGSERIAL" => Type::INT8,
        // The translator stores float8 columns as REAL, so REAL stays float8 (upstream).
        "FLOAT4" => Type::FLOAT4,
        "REAL" | "FLOAT" | "FLOAT8" | "DOUBLE" | "DOUBLE PRECISION" => Type::FLOAT8,
        "NUMERIC" | "DECIMAL" => Type::NUMERIC,
        "TEXT" | "CHARACTER VARYING" | "NAME" => Type::TEXT,
        "VARCHAR" => Type::VARCHAR,
        "CHAR" | "CHARACTER" | "BPCHAR" => Type::BPCHAR,
        "BLOB" | "BYTEA" => Type::BYTEA,
        "BOOLEAN" | "BOOL" => Type::BOOL,
        "UUID" => Type::UUID,
        "JSON" => Type::JSON,
        "JSONB" => Type::JSONB,
        "DATE" => Type::DATE,
        "TIME" | "TIMETZ" => Type::TIME,
        "TIMESTAMP" => Type::TIMESTAMP,
        "TIMESTAMPTZ" => Type::TIMESTAMPTZ,
        "INET" => Type::INET,
        "CIDR" => Type::CIDR,
        "MACADDR" => Type::MACADDR,
        "MACADDR8" => Type::MACADDR8,
        _ => {
            // Handle parameterized types like varchar(50), numeric(10,2)
            if upper.starts_with("VARCHAR") || upper.starts_with("CHAR") {
                Type::VARCHAR
            } else if upper.starts_with("NUMERIC") || upper.starts_with("DECIMAL") {
                Type::NUMERIC
            } else {
                Type::TEXT
            }
        }
    }
}

fn error_info(message: &str) -> ErrorInfo {
    error_info_code("XX000", message)
}

fn error_info_code(sqlstate: &str, message: &str) -> ErrorInfo {
    ErrorInfo::new(
        "ERROR".to_owned(),
        sqlstate.to_owned(),
        error_field_value(message),
    )
}

/// ErrorResponse fields are NUL-terminated strings, so a NUL inside a value
/// would end the field early and the client would read the rest as more
/// fields. PostgreSQL fills these fields from C strings, which also end at
/// the first NUL, so cut the value there.
fn error_field_value(value: &str) -> String {
    match value.find('\0') {
        Some(end) => value[..end].to_owned(),
        None => value.to_owned(),
    }
}

fn pg_error(error: turso_pg::Error) -> PgWireError {
    PgWireError::ApiError(Box::new(error))
}

async fn finish_query<C>(
    client: &mut C,
    result: PgWireResult<()>,
    wait_for_sync: bool,
    status: TransactionStatus,
    sql: Option<&str>,
) -> PgWireResult<()>
where
    C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
    C::Error: Debug,
    PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
{
    let Err(PgWireError::ApiError(error)) = result else {
        return result;
    };
    let fields = match error.downcast::<turso_pg::Error>() {
        Ok(error) => {
            // Engine conflicts become DSQL's errors; the rest is reshaped like PostgreSQL's
            // (micro-dsql additions).
            let shaped = micro_dsql_policy::shape_engine_error(
                dsql::engine_failure(&error),
                error.sqlstate,
                &error.message,
                error.detail.as_deref(),
                error.table_name.is_some(),
                sql.unwrap_or_default(),
            );
            let mut fields = vec![
                (b'S', "ERROR".to_string()),
                (b'C', shaped.sqlstate.clone()),
                (b'M', error_field_value(&shaped.message)),
            ];
            if let (Some(position), Some(_)) = (shaped.position, sql) {
                fields.push((b'P', position.to_string()));
            }
            for (tag, value) in [
                (b'D', &shaped.detail),
                (b's', &shaped.schema),
                (b't', &error.table_name),
                (b'c', &error.column_name),
                (b'n', &error.constraint_name),
            ] {
                if let Some(value) = value {
                    fields.push((tag, error_field_value(value)));
                }
            }
            fields
        }
        Err(error) => match error.downcast::<PolicyError>() {
            Ok(error) => {
                let mut fields = vec![
                    (b'S', "ERROR".to_string()),
                    (b'C', error.sqlstate.to_string()),
                    (b'M', error_field_value(&error.message)),
                ];
                if let Some(detail) = &error.detail {
                    fields.push((b'D', error_field_value(detail)));
                }
                fields
            }
            Err(error) => return Err(PgWireError::ApiError(error)),
        },
    };
    client
        .send(PgWireBackendMessage::ErrorResponse(ErrorResponse::new(
            fields,
        )))
        .await?;
    client.set_transaction_status(status);
    if wait_for_sync {
        client.set_state(PgWireConnectionState::AwaitingSync);
    } else {
        client.set_state(PgWireConnectionState::ReadyForQuery);
        client
            .feed(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
                status,
            )))
            .await?;
    }
    client.flush().await?;
    Ok(())
}
