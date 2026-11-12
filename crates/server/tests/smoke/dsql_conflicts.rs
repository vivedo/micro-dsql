//! Conflict errors look like DSQL's: SQLSTATE 40001 with the OC000 / OC001 texts, and engine
//! lock contention (55P03) never reaches the client.

use std::sync::Arc;

use crate::common::{rows, simple_err, value, DbErr, TestServer};

const OC000: &str = "change conflicts with another transaction (OC000)";
const OC001: &str = "schema has been updated by another transaction (OC001)";

fn oc000() -> DbErr {
    DbErr::new("40001", OC000)
}

/// Runs the statements; returns the first error (statement or COMMIT), if any.
async fn first_error(client: &tokio_postgres::Client, statements: &[&str]) -> Option<DbErr> {
    for sql in statements {
        if let Err(e) = client.simple_query(sql).await {
            return Some(DbErr::of(&e));
        }
    }
    None
}

#[tokio::test]
async fn same_row_update_in_two_transactions_conflicts_with_oc000() {
    let server = TestServer::start();
    let a = server.connect().await;
    let b = server.connect().await;
    rows(&a, "CREATE TABLE hot (id int PRIMARY KEY, v int)").await;
    rows(&a, "INSERT INTO hot VALUES (1, 0)").await;
    rows(&a, "BEGIN").await;
    rows(&b, "BEGIN").await;
    rows(&b, "SELECT v FROM hot").await;
    rows(&a, "UPDATE hot SET v = 1 WHERE id = 1").await;
    let a_commit = first_error(&a, &["COMMIT"]).await;
    let b_error = first_error(&b, &["UPDATE hot SET v = 2 WHERE id = 1", "COMMIT"]).await;
    // Exactly one of the two loses; DSQL fails the later committer, the engine may fail the
    // later writer: both are the OC000 error.
    let errors: Vec<DbErr> = [a_commit, b_error].into_iter().flatten().collect();
    assert_eq!(errors, vec![oc000()], "{errors:?}");
    // The connection stays usable after the failure.
    let _ = b.simple_query("ROLLBACK").await;
    assert_eq!(value(&b, "SELECT 1").await.as_deref(), Some("1"));
    assert_eq!(value(&b, "SELECT v FROM hot").await.as_deref(), Some("1"));
}

#[tokio::test]
async fn same_primary_key_insert_conflicts_with_oc000() {
    let server = TestServer::start();
    let a = server.connect().await;
    let b = server.connect().await;
    rows(&a, "CREATE TABLE pk (id int PRIMARY KEY)").await;
    rows(&a, "BEGIN").await;
    rows(&b, "BEGIN").await;
    rows(&a, "INSERT INTO pk VALUES (1)").await;
    let b_insert = first_error(&b, &["INSERT INTO pk VALUES (1)"]).await;
    let a_commit = first_error(&a, &["COMMIT"]).await;
    let b_commit = first_error(&b, &["COMMIT"]).await;
    // B's INSERT is refused at once or at COMMIT, never with a lock error.
    let errors: Vec<DbErr> = [b_insert, a_commit, b_commit]
        .into_iter()
        .flatten()
        .collect();
    assert!(!errors.is_empty(), "no conflict reported");
    for e in &errors {
        assert!(*e == oc000() || e.code == "23505", "{e:?}");
        assert_ne!(e.code, "55P03");
    }
    let _ = b.simple_query("ROLLBACK").await;
    assert_eq!(
        value(&a, "SELECT count(*) FROM pk").await.as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn schema_change_under_a_writing_transaction_conflicts_with_oc001() {
    let server = TestServer::start();
    let a = server.connect().await;
    let b = server.connect().await;
    rows(&a, "CREATE TABLE sch (id int PRIMARY KEY, v int)").await;
    rows(&a, "INSERT INTO sch VALUES (1, 0)").await;
    rows(&a, "BEGIN").await;
    rows(&a, "UPDATE sch SET v = 1 WHERE id = 1").await;
    rows(&b, "ALTER TABLE sch ADD COLUMN extra int").await;
    let error = simple_err(&a, "COMMIT").await;
    assert_eq!(error, DbErr::new("40001", OC001));
    assert_eq!(value(&a, "SELECT 1").await.as_deref(), Some("1"));
}

#[tokio::test]
async fn autocommit_storm_reports_only_oc000() {
    let server = TestServer::start();
    let setup = server.connect().await;
    rows(&setup, "CREATE TABLE storm (id int PRIMARY KEY, v int)").await;
    rows(&setup, "INSERT INTO storm VALUES (0, 0)").await;
    let server = Arc::new(server);
    let mut tasks = Vec::new();
    for worker in 0..6 {
        let server = server.clone();
        tasks.push(tokio::spawn(async move {
            let c = server.connect().await;
            let mut errors = Vec::new();
            for i in 0..40 {
                let sql = if i % 2 == 0 {
                    format!("INSERT INTO storm VALUES ({}, 0)", 1 + worker * 100 + i)
                } else {
                    "UPDATE storm SET v = v + 1 WHERE id = 0".to_owned()
                };
                if let Err(e) = c.simple_query(&sql).await {
                    errors.push(DbErr::of(&e));
                }
            }
            errors
        }));
    }
    for task in tasks {
        for e in task.await.unwrap() {
            assert_ne!(e.code, "55P03", "lock contention leaked: {e:?}");
            assert_eq!(e, oc000(), "{e:?}");
        }
    }
}
