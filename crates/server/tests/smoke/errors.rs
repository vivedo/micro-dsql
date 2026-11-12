//! User story 3: errors come back with PostgreSQL SQLSTATEs and the session survives.

use crate::common::{rows, s, sqlstate, value, TestServer};

#[tokio::test]
async fn common_sqlstates() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE errs (id int PRIMARY KEY, v text)").await;
    rows(&c, "INSERT INTO errs VALUES (1, 'first')").await;
    assert_eq!(
        sqlstate(&c, "INSERT INTO errs VALUES (1, 'dup')").await,
        "23505"
    );
    assert_eq!(sqlstate(&c, "SELECT * FROM no_such_table").await, "42P01");
    assert_eq!(sqlstate(&c, "SELEC 1").await, "42601");
    assert_eq!(sqlstate(&c, "SELECT 1/0").await, "22012");
    // The session is still usable.
    assert_eq!(value(&c, "SELECT v FROM errs").await, s("first"));
}

#[tokio::test]
async fn error_messages_read_like_postgres() {
    let server = TestServer::start();
    let c = server.connect().await;
    let err = c
        .simple_query("SELECT * FROM t1_missing")
        .await
        .unwrap_err();
    let db = err.as_db_error().unwrap();
    assert_eq!(db.message(), "relation \"t1_missing\" does not exist");
    assert!(matches!(
        db.position(),
        Some(tokio_postgres::error::ErrorPosition::Original(15))
    ));
    let err = c.simple_query("SELEC 1").await.unwrap_err();
    assert_eq!(
        err.as_db_error().unwrap().message(),
        "syntax error at or near \"SELEC\""
    );
    rows(&c, "CREATE TABLE uniq (id int PRIMARY KEY)").await;
    rows(&c, "INSERT INTO uniq VALUES (1)").await;
    let err = c
        .simple_query("INSERT INTO uniq VALUES (1)")
        .await
        .unwrap_err();
    let db = err.as_db_error().unwrap();
    assert_eq!(db.schema(), Some("public"));
    assert_eq!(db.table(), Some("uniq"));
    assert_eq!(db.constraint(), Some("uniq_pkey"));
}

#[tokio::test]
async fn an_error_aborts_the_transaction_until_rollback() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE aborted (id int PRIMARY KEY)").await;
    rows(&c, "BEGIN").await;
    rows(&c, "INSERT INTO aborted VALUES (1)").await;
    assert_eq!(sqlstate(&c, "SELECT 1/0").await, "22012");
    assert_eq!(sqlstate(&c, "SELECT 1").await, "25P02");
    assert_eq!(
        sqlstate(&c, "INSERT INTO aborted VALUES (2)").await,
        "25P02"
    );
    rows(&c, "ROLLBACK").await;
    assert_eq!(value(&c, "SELECT count(*) FROM aborted").await, s("0"));
}

#[tokio::test]
async fn concurrent_updates_of_one_row_conflict_with_40001() {
    let server = TestServer::start();
    let a = server.connect().await;
    let b = server.connect().await;
    rows(&a, "CREATE TABLE occ (id int PRIMARY KEY, v text)").await;
    rows(&a, "INSERT INTO occ VALUES (1, 'initial')").await;

    rows(&b, "BEGIN ISOLATION LEVEL REPEATABLE READ").await;
    rows(&b, "SELECT v FROM occ WHERE id = 1").await;
    rows(&a, "BEGIN ISOLATION LEVEL REPEATABLE READ").await;
    rows(&a, "UPDATE occ SET v = 'from A' WHERE id = 1").await;
    rows(&a, "COMMIT").await;
    // B fails at its UPDATE (Turso) or at COMMIT (DSQL); either way with 40001.
    let update = crate::common::simple(&b, "UPDATE occ SET v = 'from B' WHERE id = 1").await;
    let code = match update {
        Err(e) => e.code().unwrap().code().to_owned(),
        Ok(_) => sqlstate(&b, "COMMIT").await,
    };
    assert_eq!(code, "40001");
    let _ = b.simple_query("ROLLBACK").await;
    assert_eq!(
        value(&a, "SELECT v FROM occ WHERE id = 1").await,
        s("from A")
    );
}
