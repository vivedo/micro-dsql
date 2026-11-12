//! User story 2: the session looks like DSQL (startup parameters, SHOW, current_database,
//! Terminate).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::common::{rows, s, sqlstate, value, TestServer};

/// Reads one backend message: (tag, body).
fn read_message(socket: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut header = [0u8; 5];
    socket.read_exact(&mut header).unwrap();
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    let mut body = vec![0u8; len - 4];
    socket.read_exact(&mut body).unwrap();
    (header[0], body)
}

/// Performs a raw startup and returns the socket and the reported parameters.
fn raw_startup(server: &TestServer) -> (TcpStream, HashMap<String, String>) {
    let mut socket = TcpStream::connect(server.addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut body = Vec::new();
    body.extend_from_slice(&196_608u32.to_be_bytes());
    for (k, v) in [
        ("user", "admin"),
        ("database", "postgres"),
        ("application_name", "smoke"),
    ] {
        body.extend_from_slice(k.as_bytes());
        body.push(0);
        body.extend_from_slice(v.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut message = ((body.len() + 4) as u32).to_be_bytes().to_vec();
    message.extend_from_slice(&body);
    socket.write_all(&message).unwrap();

    let mut params = HashMap::new();
    loop {
        let (tag, body) = read_message(&mut socket);
        match tag {
            b'R' => assert_eq!(&body[..4], &[0, 0, 0, 0], "AuthenticationOk expected"),
            b'S' => {
                let mut parts = body.split(|b| *b == 0);
                let k = String::from_utf8(parts.next().unwrap().to_vec()).unwrap();
                let v = String::from_utf8(parts.next().unwrap().to_vec()).unwrap();
                params.insert(k, v);
            }
            b'Z' => break,
            b'K' => {}
            other => panic!("unexpected message {:?}", other as char),
        }
    }
    (socket, params)
}

#[test]
fn startup_parameters_look_like_dsql() {
    let server = TestServer::start();
    let (_socket, params) = raw_startup(&server);
    let get = |k: &str| params.get(k).map(String::as_str);
    assert!(
        get("server_version").unwrap().starts_with("16."),
        "{params:?}"
    );
    assert!(!get("server_version").unwrap().contains("pgwire"));
    assert_eq!(get("TimeZone"), Some("UTC"));
    assert_eq!(get("is_superuser"), Some("off"));
    assert_eq!(get("server_encoding"), Some("UTF8"));
    assert_eq!(get("client_encoding"), Some("UTF8"));
    assert_eq!(get("DateStyle"), Some("ISO, MDY"));
    assert_eq!(get("standard_conforming_strings"), Some("on"));
    assert_eq!(get("integer_datetimes"), Some("on"));
    assert_eq!(get("application_name"), Some("smoke"));
    assert_eq!(get("session_authorization"), Some("admin"));
}

#[test]
fn terminate_closes_the_connection() {
    let server = TestServer::start();
    let (mut socket, _) = raw_startup(&server);
    socket.write_all(&[b'X', 0, 0, 0, 4]).unwrap();
    let mut buf = [0u8; 16];
    // The server must close its side: read returns 0 (EOF) well before the timeout.
    let n = socket
        .read(&mut buf)
        .expect("server should close, not time out");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn show_reports_dsql_values() {
    let server = TestServer::start();
    let c = server.connect().await;
    let version = value(&c, "SHOW server_version").await.unwrap();
    assert!(version.starts_with("16."), "{version}");
    assert_eq!(value(&c, "SHOW timezone").await, s("UTC"));
    assert_eq!(value(&c, "SHOW TIME ZONE").await, s("UTC"));
    assert_eq!(value(&c, "SHOW is_superuser").await, s("off"));
    assert_eq!(
        value(&c, "SHOW transaction_isolation").await,
        s("repeatable read")
    );
    // Column names follow PostgreSQL.
    let messages = c.simple_query("SHOW timezone").await.unwrap();
    let tokio_postgres::SimpleQueryMessage::Row(row) = &messages[1] else {
        panic!("{messages:?}")
    };
    assert_eq!(row.columns()[0].name(), "TimeZone");
    // Parameters the engine knows still work.
    assert!(value(&c, "SHOW search_path").await.is_some());
    // And through the extended protocol.
    let row = c.query_one("SHOW server_version", &[]).await.unwrap();
    assert!(row.get::<_, String>(0).starts_with("16."));
}

#[tokio::test]
async fn current_database_is_postgres() {
    let server = TestServer::start();
    let c = server.connect().await;
    assert_eq!(value(&c, "SELECT current_database()").await, s("postgres"));
}

#[tokio::test]
async fn set_timezone_accepts_only_utc() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "SET timezone = 'UTC'").await;
    rows(&c, "SET TIME ZONE 'Etc/UTC'").await;
    assert_eq!(value(&c, "SHOW timezone").await, s("UTC"));
    assert_eq!(sqlstate(&c, "SET timezone = 'Europe/Rome'").await, "22023");
}

#[tokio::test]
async fn show_in_a_failed_transaction_gives_25p02() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "BEGIN").await;
    assert_eq!(sqlstate(&c, "SELECT 1/0").await, "22012");
    assert_eq!(sqlstate(&c, "SHOW server_version").await, "25P02");
    rows(&c, "ROLLBACK").await;
    assert!(value(&c, "SHOW server_version").await.is_some());
}

/// Sends one frontend message: tag, length, body.
fn send(socket: &mut TcpStream, tag: u8, body: &[u8]) {
    let mut message = vec![tag];
    message.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
    message.extend_from_slice(body);
    socket.write_all(&message).unwrap();
}

/// Reads messages up to ReadyForQuery; returns command tags and error SQLSTATEs in order.
fn read_until_ready(socket: &mut TcpStream) -> Vec<String> {
    let mut seen = Vec::new();
    loop {
        let (tag, body) = read_message(socket);
        match tag {
            b'C' => seen.push(String::from_utf8_lossy(&body[..body.len() - 1]).into_owned()),
            b'E' => {
                let code = body
                    .split(|b| *b == 0)
                    .find(|field| field.first() == Some(&b'C'))
                    .map(|field| String::from_utf8_lossy(&field[1..]).into_owned())
                    .unwrap_or_default();
                seen.push(format!("ERROR {code}"));
            }
            b'Z' => return seen,
            _ => {}
        }
    }
}

/// psycopg sends `DEALLOCATE ALL` after a DROP/ALTER once it holds server-side prepared
/// statements. micro-dsql answers `DEALLOCATE` with PostgreSQL's command tags; it does not
/// remove the protocol-level statement (KNOWN_GAPS.md "DEALLOCATE").
#[test]
fn deallocate_is_answered_with_its_command_tag() {
    let server = TestServer::start();
    let (mut socket, _) = raw_startup(&server);
    // Parse "p1" (no parameter types), then Sync.
    send(&mut socket, b'P', b"p1\0SELECT 1\0\0\0");
    send(&mut socket, b'S', b"");
    assert_eq!(read_until_ready(&mut socket), Vec::<String>::new());

    send(&mut socket, b'Q', b"DEALLOCATE p1\0");
    assert_eq!(read_until_ready(&mut socket), ["DEALLOCATE"]);
    send(&mut socket, b'Q', b"DEALLOCATE PREPARE ALL\0");
    assert_eq!(read_until_ready(&mut socket), ["DEALLOCATE ALL"]);
    send(&mut socket, b'Q', b"BEGIN; DEALLOCATE ALL; COMMIT\0");
    assert_eq!(
        read_until_ready(&mut socket),
        ["BEGIN", "DEALLOCATE ALL", "COMMIT"]
    );
    // Inside a failed transaction block it is refused like any other statement.
    send(&mut socket, b'Q', b"BEGIN; SELECT 1/0\0");
    assert_eq!(read_until_ready(&mut socket), ["BEGIN", "ERROR 22012"]);
    send(&mut socket, b'Q', b"DEALLOCATE ALL\0");
    assert_eq!(read_until_ready(&mut socket), ["ERROR 25P02"]);
    send(&mut socket, b'Q', b"ROLLBACK\0");
    assert_eq!(read_until_ready(&mut socket), ["ROLLBACK"]);
}

#[tokio::test]
async fn deallocate_all_after_ddl_keeps_the_session_usable() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE dealloc (id int PRIMARY KEY)").await;
    c.execute("INSERT INTO dealloc VALUES ($1)", &[&1i32])
        .await
        .unwrap();
    rows(&c, "DROP TABLE dealloc").await;
    rows(&c, "DEALLOCATE ALL").await;
    c.execute("DEALLOCATE ALL", &[]).await.unwrap();
    assert_eq!(value(&c, "SELECT 1").await, s("1"));
}

/// Parse, Bind and Execute the unnamed statement with no parameters, then Sync.
fn extended(socket: &mut TcpStream, sql: &str) -> Vec<String> {
    let mut parse = b"\0".to_vec();
    parse.extend_from_slice(sql.as_bytes());
    parse.extend_from_slice(b"\0\0\0");
    send(socket, b'P', &parse);
    send(socket, b'B', b"\0\0\0\0\0\0\0\0");
    send(socket, b'E', b"\0\0\0\0\0");
    send(socket, b'S', b"");
    read_until_ready(socket)
}

/// The job statements answer rows under their own command tag, without a row count appended
/// (pgwire adds one to every result set otherwise: `CREATE INDEX 1`).
#[test]
fn job_statements_complete_with_their_own_command_tags() {
    let server = TestServer::start();
    let (mut socket, _) = raw_startup(&server);
    send(
        &mut socket,
        b'Q',
        b"CREATE TABLE tagt (id int PRIMARY KEY, v text, w int)\0",
    );
    assert_eq!(read_until_ready(&mut socket), ["CREATE TABLE"]);
    send(
        &mut socket,
        b'Q',
        b"CREATE INDEX ASYNC tagt_v ON tagt (v)\0",
    );
    assert_eq!(read_until_ready(&mut socket), ["CREATE INDEX"]);
    assert_eq!(
        extended(&mut socket, "CREATE INDEX ASYNC tagt_w ON tagt (id, v)"),
        ["CREATE INDEX"]
    );
    send(
        &mut socket,
        b'Q',
        b"ALTER TABLE tagt ADD CONSTRAINT tagt_c CHECK (w > 0) NOT VALID\0",
    );
    assert_eq!(read_until_ready(&mut socket), ["ALTER TABLE"]);
    assert_eq!(
        extended(
            &mut socket,
            "ALTER TABLE ASYNC tagt VALIDATE CONSTRAINT tagt_c"
        ),
        ["ALTER TABLE"]
    );
    send(
        &mut socket,
        b'Q',
        b"ALTER TABLE ASYNC tagt VALIDATE CONSTRAINT tagt_c\0",
    );
    assert_eq!(read_until_ready(&mut socket), ["ALTER TABLE"]);
    // Ordinary results keep their row counts, and sys.dsql_major_version() is a plain SELECT.
    send(&mut socket, b'Q', b"SELECT sys.dsql_major_version()\0");
    assert_eq!(read_until_ready(&mut socket), ["SELECT 1"]);
    assert_eq!(
        extended(&mut socket, "SELECT sys.dsql_major_version()"),
        ["SELECT 1"]
    );
    send(&mut socket, b'Q', b"SELECT job_id FROM sys.jobs\0");
    assert_eq!(read_until_ready(&mut socket), ["SELECT 4"]);
}
