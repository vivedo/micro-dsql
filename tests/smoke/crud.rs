//! User story 1: start, connect with any credentials, CRUD and transactions.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crate::common::{rows, s, value, TestServer};

#[tokio::test]
async fn any_user_and_any_password_are_accepted() {
    let server = TestServer::start();
    for (user, password) in [
        ("admin", Some("arbitrary")),
        ("someone_else", Some("a-long-iam-token?X-Amz-Signature=abc")),
        ("admin", None),
    ] {
        let client = server.connect_as(user, password).await;
        assert_eq!(value(&client, "SELECT 1").await, s("1"));
    }
}

#[test]
fn ssl_request_is_refused_with_n() {
    let server = TestServer::start();
    let mut socket = std::net::TcpStream::connect(server.addr).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    // SSLRequest: length 8, code 80877103.
    socket.write_all(&[0, 0, 0, 8, 4, 210, 22, 47]).unwrap();
    let mut answer = [0u8; 1];
    socket.read_exact(&mut answer).unwrap();
    assert_eq!(answer[0], b'N');
}

#[tokio::test]
async fn crud_round_trip() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(
        &c,
        "CREATE TABLE crud (id int PRIMARY KEY, name text NOT NULL, qty int)",
    )
    .await;
    let inserted = rows(
        &c,
        "INSERT INTO crud VALUES (1, 'apple', 10), (2, 'banana', 20), (3, 'cherry', 30) \
         RETURNING id, name",
    )
    .await;
    assert_eq!(inserted.len(), 3);
    assert_eq!(inserted[1], vec![s("2"), s("banana")]);
    assert_eq!(
        rows(&c, "SELECT id FROM crud WHERE qty >= 20 ORDER BY id DESC").await,
        vec![vec![s("3")], vec![s("2")]]
    );
    assert_eq!(
        rows(
            &c,
            "UPDATE crud SET qty = qty + 1 WHERE id = 2 RETURNING qty"
        )
        .await,
        vec![vec![s("21")]]
    );
    rows(&c, "DELETE FROM crud WHERE id = 1").await;
    assert_eq!(value(&c, "SELECT count(*) FROM crud").await, s("2"));
    rows(&c, "DROP TABLE crud").await;
}

#[tokio::test]
async fn explicit_transactions_commit_and_roll_back() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE txn (id int PRIMARY KEY)").await;
    rows(&c, "BEGIN; INSERT INTO txn VALUES (1); COMMIT").await;
    rows(&c, "BEGIN").await;
    rows(&c, "INSERT INTO txn VALUES (2)").await;
    assert_eq!(value(&c, "SELECT count(*) FROM txn").await, s("2"));
    rows(&c, "ROLLBACK").await;
    assert_eq!(value(&c, "SELECT count(*) FROM txn").await, s("1"));
    rows(&c, "BEGIN ISOLATION LEVEL REPEATABLE READ").await;
    assert_eq!(value(&c, "SELECT count(*) FROM txn").await, s("1"));
    rows(&c, "COMMIT").await;

    // The same through the extended protocol (tokio-postgres transactions).
    let mut c = c;
    let tx = c.transaction().await.unwrap();
    tx.execute("INSERT INTO txn VALUES (3)", &[]).await.unwrap();
    tx.rollback().await.unwrap();
    let tx = c.transaction().await.unwrap();
    tx.execute("INSERT INTO txn VALUES (4)", &[]).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        rows(&c, "SELECT id FROM txn ORDER BY id").await,
        vec![vec![s("1")], vec![s("4")]]
    );
}

#[tokio::test]
async fn a_dropped_connection_rolls_back_its_transaction() {
    let server = TestServer::start();
    let a = server.connect().await;
    rows(&a, "CREATE TABLE dropped (id int PRIMARY KEY)").await;
    rows(&a, "BEGIN; INSERT INTO dropped VALUES (1)").await;
    drop(a);
    let b = server.connect().await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // Until the server notices the closed socket, the row is invisible anyway
        // (uncommitted); afterwards a new insert of the same key must succeed.
        match crate::common::simple(&b, "INSERT INTO dropped VALUES (1)").await {
            Ok(_) => break,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!(value(&b, "SELECT count(*) FROM dropped").await, s("1"));
}

#[tokio::test]
async fn a_long_statement_does_not_block_other_connections() {
    let server = TestServer::start();
    let slow = server.connect().await;
    let fast = server.connect().await;
    let slow_task = tokio::spawn(async move {
        let started = Instant::now();
        rows(
            &slow,
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000) \
             SELECT count(*) FROM n",
        )
        .await;
        started.elapsed()
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    assert_eq!(value(&fast, "SELECT 1").await, s("1"));
    let fast_took = started.elapsed();
    let slow_took = slow_task.await.unwrap();
    assert!(
        slow_took > Duration::from_millis(300),
        "the slow statement was too fast to prove anything ({slow_took:?})"
    );
    assert!(
        fast_took < slow_took / 2,
        "fast query took {fast_took:?}, slow one {slow_took:?}"
    );
}
