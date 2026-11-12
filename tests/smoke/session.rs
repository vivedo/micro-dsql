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
