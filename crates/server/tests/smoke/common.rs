//! Test harness: an in-process server on `127.0.0.1:0` and client helpers.

use std::net::SocketAddr;
use std::sync::mpsc;
use std::thread::JoinHandle;

use micro_dsql_server::{Server, ServerOptions, Storage};
use tokio::sync::oneshot;
use tokio_postgres::{Client, NoTls};

/// A server running on its own thread and runtime; stopped on drop.
pub struct TestServer {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl TestServer {
    pub fn start() -> Self {
        Self::start_with(ServerOptions::default())
    }

    pub fn start_with(options: ServerOptions) -> Self {
        let (addr_tx, addr_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let server = Server::bind_with("127.0.0.1:0", &Storage::Memory, options)
                    .await
                    .expect("bind");
                addr_tx.send(server.local_addr().expect("addr")).unwrap();
                server
                    .serve(async {
                        let _ = shutdown_rx.await;
                    })
                    .await
                    .expect("serve");
            });
            runtime.shutdown_timeout(std::time::Duration::from_secs(1));
        });
        let addr = addr_rx.recv().expect("server address");
        Self {
            addr,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        }
    }

    /// Connects as `admin` to `postgres` with an arbitrary password.
    pub async fn connect(&self) -> Client {
        self.connect_as("admin", Some("any-password")).await
    }

    pub async fn connect_as(&self, user: &str, password: Option<&str>) -> Client {
        let mut config = tokio_postgres::Config::new();
        config
            .host(self.addr.ip().to_string())
            .port(self.addr.port())
            .user(user)
            .dbname("postgres")
            .ssl_mode(tokio_postgres::config::SslMode::Disable);
        if let Some(password) = password {
            config.password(password);
        }
        let (client, connection) = config.connect(NoTls).await.expect("connect");
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs a simple query and returns its rows as text (`None` for NULL).
pub async fn rows(client: &Client, sql: &str) -> Vec<Vec<Option<String>>> {
    simple(client, sql)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
}

pub async fn simple(
    client: &Client,
    sql: &str,
) -> Result<Vec<Vec<Option<String>>>, tokio_postgres::Error> {
    let messages = client.simple_query(sql).await?;
    Ok(messages
        .into_iter()
        .filter_map(|m| match m {
            tokio_postgres::SimpleQueryMessage::Row(row) => Some(
                (0..row.len())
                    .map(|i| row.get(i).map(str::to_owned))
                    .collect(),
            ),
            _ => None,
        })
        .collect())
}

/// The single text value of a one-row, one-column simple query.
pub async fn value(client: &Client, sql: &str) -> Option<String> {
    let rows = rows(client, sql).await;
    assert_eq!(rows.len(), 1, "{sql}: {rows:?}");
    rows[0][0].clone()
}

/// The SQLSTATE of a failing simple query.
pub async fn sqlstate(client: &Client, sql: &str) -> String {
    match client.simple_query(sql).await {
        Ok(r) => panic!("{sql}: expected an error, got {} messages", r.len()),
        Err(e) => e
            .code()
            .unwrap_or_else(|| panic!("{sql}: not a server error: {e:?}"))
            .code()
            .to_owned(),
    }
}

pub fn s(v: &str) -> Option<String> {
    Some(v.to_owned())
}

/// The fields of a server error that the DSQL rules define.
#[derive(Debug, PartialEq, Eq)]
pub struct DbErr {
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
}

impl DbErr {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_owned(),
            message: message.to_owned(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: &str) -> Self {
        self.detail = Some(detail.to_owned());
        self
    }

    pub fn of(error: &tokio_postgres::Error) -> Self {
        let db = error
            .as_db_error()
            .unwrap_or_else(|| panic!("not a server error: {error:?}"));
        Self {
            code: db.code().code().to_owned(),
            message: db.message().to_owned(),
            detail: db.detail().map(str::to_owned),
        }
    }
}

/// The error of a failing simple-protocol statement.
pub async fn simple_err(client: &Client, sql: &str) -> DbErr {
    match client.simple_query(sql).await {
        Ok(_) => panic!("{sql}: expected an error"),
        Err(e) => DbErr::of(&e),
    }
}
