//! `SELECT ... FOR UPDATE` takes part in conflict detection (US4): it enters its rows into the
//! write set, so a concurrent write of one of them conflicts exactly like a write would.
//! `FOR KEY SHARE` is a plain read (documented difference).

use micro_dsql_policy::PolicyLimits;
use micro_dsql_server::ServerOptions;

use crate::common::TestServer;
use crate::{oc000, Session};

async fn setup() -> (TestServer, Session, Session) {
    let server = TestServer::start();
    let a = Session::new(&server).await;
    let b = Session::new(&server).await;
    a.ok("CREATE TABLE t (id int PRIMARY KEY, v int, u int UNIQUE)")
        .await;
    a.ok("INSERT INTO t VALUES (1, 0, 1), (2, 0, 2)").await;
    (server, a, b)
}

async fn v(s: &Session, id: i32) -> String {
    s.value(&format!("SELECT v FROM t WHERE id = {id}"))
        .await
        .unwrap()
}

/// M5, order 1: A locks r1; B's UPDATE of r1 conflicts; B's COMMIT fails with OC000.
#[tokio::test]
async fn m5_for_update_then_update() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("SELECT v FROM t WHERE id = 1 FOR UPDATE").await;
    assert_eq!(b.affected("UPDATE t SET v = 9 WHERE id = 1").await, 0);
    a.ok("COMMIT").await;
    assert_eq!(b.err("COMMIT").await, oc000());
    assert_eq!(v(&a, 1).await, "0");
}

/// M5, order 2: B writes r1 first; A's FOR UPDATE of r1 is the later toucher and loses at COMMIT
/// (the read still answers).
#[tokio::test]
async fn m5_update_then_for_update() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    b.ok("UPDATE t SET v = 9 WHERE id = 1").await;
    let rows = a.rows("SELECT id FROM t WHERE id = 1 FOR UPDATE").await;
    assert_eq!(rows.len(), 1);
    b.ok("COMMIT").await;
    assert_eq!(a.err("COMMIT").await, oc000());
    assert_eq!(v(&b, 1).await, "9");
}

/// M5 with the snapshot taken at BEGIN: A's snapshot is older than B's commit.
#[tokio::test]
async fn m5_for_update_of_a_row_changed_since_begin() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("UPDATE t SET v = 9 WHERE id = 1").await;
    a.ok("SELECT * FROM t WHERE id = 1 FOR UPDATE").await;
    assert_eq!(a.err("COMMIT").await, oc000());
}

/// M6: two FOR UPDATE of one row.
#[tokio::test]
async fn m6_for_update_against_for_update() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("SELECT * FROM t WHERE id = 1 FOR UPDATE").await;
    b.ok("SELECT * FROM t WHERE id = 1 FOR UPDATE").await;
    a.ok("COMMIT").await;
    assert_eq!(b.err("COMMIT").await, oc000());
}

/// M7: FOR UPDATE of r1 against other rows: no conflict.
#[tokio::test]
async fn m7_for_update_against_other_rows() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("SELECT * FROM t WHERE id = 1 FOR UPDATE").await;
    b.ok("UPDATE t SET v = 9 WHERE id = 2").await;
    b.ok("INSERT INTO t VALUES (3, 0, 3)").await;
    b.ok("SELECT * FROM t WHERE id = 2 FOR UPDATE").await;
    a.ok("COMMIT").await;
    b.ok("COMMIT").await;
    assert_eq!(v(&a, 2).await, "9");
}

/// M9: FOR KEY SHARE is a plain read; it conflicts with nothing (non-key UPDATE).
#[tokio::test]
async fn m9_for_key_share_against_a_non_key_update() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    let rows = a.rows("SELECT id FROM t WHERE id = 1 FOR KEY SHARE").await;
    assert_eq!(rows.len(), 1);
    b.ok("UPDATE t SET v = 9 WHERE id = 1").await;
    a.ok("COMMIT").await;
    b.ok("COMMIT").await;
    assert_eq!(v(&a, 1).await, "9");
}

/// FOR UPDATE answers with the rows of the plain SELECT (columns, order, LIMIT, alias, `OF`,
/// `NOWAIT`, `SKIP LOCKED`) and changes nothing.
#[tokio::test]
async fn for_update_returns_the_plain_rows_and_changes_nothing() {
    let (_server, a, _b) = setup().await;
    let plain = a.rows("SELECT id, v FROM t ORDER BY id DESC LIMIT 1").await;
    for sql in [
        "SELECT id, v FROM t ORDER BY id DESC LIMIT 1 FOR UPDATE",
        "SELECT x.id, x.v FROM t AS x ORDER BY x.id DESC LIMIT 1 FOR UPDATE OF x",
        "SELECT id, v FROM t ORDER BY id DESC LIMIT 1 FOR UPDATE NOWAIT",
        "SELECT id, v FROM t ORDER BY id DESC LIMIT 1 FOR UPDATE SKIP LOCKED",
        "SELECT id, v FROM t ORDER BY id DESC LIMIT 1 FOR KEY SHARE",
    ] {
        // In a transaction and in autocommit.
        a.ok("BEGIN").await;
        assert_eq!(a.rows(sql).await, plain, "{sql}");
        a.ok("COMMIT").await;
        assert_eq!(a.rows(sql).await, plain, "{sql}");
    }
    assert_eq!(
        a.rows("SELECT id, v, u FROM t ORDER BY id").await,
        vec![
            vec![Some("1".into()), Some("0".into()), Some("1".into())],
            vec![Some("2".into()), Some("0".into()), Some("2".into())]
        ]
    );
    // A table whose only column is its primary key (the touch rewrites the key to itself).
    a.ok("CREATE TABLE only_key (id int PRIMARY KEY)").await;
    a.ok("INSERT INTO only_key VALUES (1)").await;
    assert_eq!(a.rows("SELECT id FROM only_key FOR UPDATE").await.len(), 1);
    // A missing table is the usual error.
    assert_eq!(
        a.err("SELECT * FROM missing FOR UPDATE").await.code,
        "42P01"
    );
}

/// Unsupported forms are refused with 0A000 and abort the block.
#[tokio::test]
async fn unsupported_locking_forms_are_refused() {
    let (_server, a, _b) = setup().await;
    a.ok("CREATE TABLE s (id int PRIMARY KEY)").await;
    let e = a
        .err("SELECT * FROM t JOIN s ON s.id = t.id FOR UPDATE")
        .await;
    assert_eq!(e.code, "0A000");
    assert_eq!(
        e.message,
        "locking clause is only supported on a single-table SELECT"
    );
    assert_eq!(a.err("SELECT * FROM t FOR SHARE").await.code, "0A000");
}

/// FOR UPDATE rows are not counted toward the row limit and FOR UPDATE does not count as DML
/// for the DDL/DML rule.
#[tokio::test]
async fn for_update_is_a_read_for_the_transaction_rules() {
    let server = TestServer::start_with(ServerOptions {
        limits: PolicyLimits {
            max_rows_per_txn: 2,
            ..PolicyLimits::default()
        },
        ..ServerOptions::default()
    });
    let a = Session::new(&server).await;
    a.ok("CREATE TABLE r (id int PRIMARY KEY, v int)").await;
    a.ok("INSERT INTO r VALUES (1, 0), (2, 0)").await;
    a.ok("BEGIN").await;
    assert_eq!(a.rows("SELECT * FROM r FOR UPDATE").await.len(), 2);
    a.ok("INSERT INTO r VALUES (3, 0)").await;
    a.ok("INSERT INTO r VALUES (4, 0)").await;
    a.ok("COMMIT").await;

    // A read-only use followed by one DDL is allowed.
    a.ok("BEGIN").await;
    a.ok("SELECT * FROM r WHERE id = 1 FOR UPDATE").await;
    a.ok("CREATE TABLE made (id int)").await;
    a.ok("COMMIT").await;
}

/// Autocommit FOR UPDATE runs as a single-statement transaction: it conflicts with another
/// session's open write of the row (OC000) and leaves the session idle.
#[tokio::test]
async fn autocommit_for_update_conflicts_with_an_open_write() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    assert_eq!(
        b.err("SELECT * FROM t WHERE id = 1 FOR UPDATE").await,
        oc000()
    );
    assert_eq!(
        b.rows("SELECT * FROM t WHERE id = 2 FOR UPDATE")
            .await
            .len(),
        1
    );
    assert_eq!(b.value("SELECT 1").await.as_deref(), Some("1"));
    a.ok("COMMIT").await;
}
