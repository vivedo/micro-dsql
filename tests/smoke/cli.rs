//! The `micro-dsql` binary: flags, the logged address, readiness time, persistence and
//! graceful shutdown on SIGTERM/SIGINT.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_micro-dsql");

struct Running {
    child: Child,
    addr: SocketAddr,
    ready_after: Duration,
    log: mpsc::Receiver<String>,
}

/// Starts the binary and waits for its "listening on" log line.
fn start(args: &[&str]) -> Running {
    let started = Instant::now();
    let mut child = Command::new(BIN)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn micro-dsql");
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    loop {
        let Ok(line) = rx.recv_timeout(Duration::from_secs(30)) else {
            let _ = child.kill();
            let _ = child.wait();
            panic!("micro-dsql did not log its address");
        };
        if let Some((_, rest)) = line.split_once("listening on ") {
            let addr = rest.split_whitespace().next().unwrap().parse().unwrap();
            return Running {
                child,
                addr,
                ready_after: started.elapsed(),
                log: rx,
            };
        }
    }
}

fn signal(child: &Child, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success());
}

fn wait_exit(mut running: Running) -> (std::process::ExitStatus, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = running.child.try_wait().unwrap() {
            let lines = running.log.try_iter().collect();
            return (status, lines);
        }
        assert!(Instant::now() < deadline, "micro-dsql did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn client(addr: SocketAddr) -> tokio_postgres::Client {
    let conn = format!(
        "host={} port={} user=admin password=anything dbname=postgres sslmode=disable",
        addr.ip(),
        addr.port()
    );
    let (client, connection) = tokio_postgres::connect(&conn, tokio_postgres::NoTls)
        .await
        .expect("connect");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

#[tokio::test]
async fn starts_fast_on_port_0_and_stops_on_sigterm() {
    // The first launch of a freshly linked binary can be slowed down by the OS (code
    // signing checks, cold page cache); the readiness requirement is about a warm start.
    let warm_up = start(&["--listen", "127.0.0.1:0"]);
    signal(&warm_up.child, "TERM");
    wait_exit(warm_up);

    let running = start(&["--listen", "127.0.0.1:0"]);
    assert_ne!(running.addr.port(), 0);
    assert!(
        running.ready_after < Duration::from_secs(1),
        "ready after {:?}",
        running.ready_after
    );
    let c = client(running.addr).await;
    let row = c.query_one("SELECT current_database()", &[]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), "postgres");
    signal(&running.child, "TERM");
    let (status, log) = wait_exit(running);
    assert!(status.success(), "{status:?} {log:?}");
    assert!(log.iter().any(|l| l.contains("SIGTERM")), "{log:?}");
}

#[tokio::test]
async fn stops_on_sigint() {
    let running = start(&["--listen", "127.0.0.1:0", "--memory"]);
    signal(&running.child, "INT");
    let (status, _) = wait_exit(running);
    assert!(status.success(), "{status:?}");
}

#[tokio::test]
async fn data_file_persists_across_restarts() {
    let path: PathBuf = std::env::temp_dir().join(format!(
        "micro-dsql-cli-test-{}-{}.db",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let path_str = path.to_str().unwrap().to_owned();

    let running = start(&["--listen", "127.0.0.1:0", "--data", &path_str]);
    let c = client(running.addr).await;
    c.batch_execute(
        "CREATE TABLE kept (id int PRIMARY KEY, v text); INSERT INTO kept VALUES (1, 'still here')",
    )
    .await
    .unwrap();
    drop(c);
    signal(&running.child, "TERM");
    let (status, log) = wait_exit(running);
    assert!(status.success(), "{log:?}");

    let running = start(&["--listen", "127.0.0.1:0", "--data", &path_str]);
    let c = client(running.addr).await;
    let row = c
        .query_one("SELECT v FROM kept WHERE id = 1", &[])
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "still here");
    signal(&running.child, "TERM");
    wait_exit(running);

    // The engine keeps side files next to the database (e.g. a log); remove them all.
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
        let name = name.to_string_lossy().into_owned();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            if entry.file_name().to_string_lossy().starts_with(&name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[test]
fn data_and_memory_are_mutually_exclusive() {
    let status = Command::new(BIN)
        .args(["--data", "x.db", "--memory"])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn an_address_in_use_is_an_error() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let output = Command::new(BIN)
        .args(["--listen", &addr])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot listen"));
}
