//! The snapshot is taken at BEGIN (US3): a transaction does not see commits made after its
//! BEGIN, whatever its first statement is.

use micro_dsql_server::{OccOptions, ServerOptions};
use tokio_postgres::SimpleQueryMessage;

use crate::common::TestServer;
use crate::{oc000, Session};

async fn setup(options: ServerOptions) -> (TestServer, Session, Session) {
    let server = TestServer::start_with(options);
    let a = Session::new(&server).await;
    let b = Session::new(&server).await;
    a.ok("CREATE TABLE t (id int PRIMARY KEY, v int)").await;
    a.ok("INSERT INTO t VALUES (1, 0), (2, 0)").await;
    (server, a, b)
}

/// S1d: B BEGIN; A commits r2 = 555; B reads r2 and sees the old value; B's write of r2 ends in
/// OC000 at COMMIT. Every spelling of BEGIN.
#[tokio::test]
async fn the_snapshot_is_taken_at_begin() {
    for begin in [
        "BEGIN",
        "BEGIN ISOLATION LEVEL REPEATABLE READ",
        "START TRANSACTION",
        "BEGIN READ ONLY",
    ] {
        let (_server, a, b) = setup(ServerOptions::default()).await;
        b.ok(begin).await;
        a.ok("UPDATE t SET v = 555 WHERE id = 2").await;
        assert_eq!(
            b.value("SELECT v FROM t WHERE id = 2").await.as_deref(),
            Some("0"),
            "{begin}: the snapshot predates A's commit"
        );
        if begin.ends_with("READ ONLY") {
            b.ok("COMMIT").await;
            continue;
        }
        b.ok("UPDATE t SET v = 7 WHERE id = 2").await;
        assert_eq!(b.err("COMMIT").await, oc000(), "{begin}");
        assert_eq!(
            a.value("SELECT v FROM t WHERE id = 2").await.as_deref(),
            Some("555")
        );
    }
}

/// FR-021, E-P4 / E-P10b: DDL as the first statement of a transaction whose snapshot was pinned
/// works even after a concurrent commit; a read-only transaction keeps the engine's refusal.
#[tokio::test]
async fn ddl_first_in_a_transaction_after_a_concurrent_commit() {
    let (_server, a, b) = setup(ServerOptions::default()).await;
    b.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    b.ok("CREATE TABLE made (id int PRIMARY KEY)").await;
    b.ok("COMMIT").await;
    assert_eq!(
        b.value("SELECT count(*) FROM made").await.as_deref(),
        Some("0")
    );

    b.ok("BEGIN READ ONLY").await;
    let e = b.err("CREATE TABLE ro (id int)").await;
    assert_eq!(e.code, "25006", "{e:?}");
    b.ok("ROLLBACK").await;

    // After another statement the transaction keeps its snapshot (no re-open): a DDL after a
    // read follows the usual one-DDL rules.
    b.ok("BEGIN").await;
    b.ok("SELECT * FROM t").await;
    b.ok("CREATE TABLE made2 (id int PRIMARY KEY)").await;
    b.ok("COMMIT").await;
}

/// The pin is invisible: BEGIN answers its own tag and no rows, the DDL/DML discipline and the
/// aborted-block behaviour are as before.
#[tokio::test]
async fn the_pin_is_invisible() {
    let (_server, a, _b) = setup(ServerOptions::default()).await;
    let messages = a.client.simple_query("BEGIN").await.unwrap();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(matches!(
        messages[0],
        SimpleQueryMessage::CommandComplete(0)
    ));
    // The discipline counters are untouched: one DDL, then a second one is refused as before.
    a.ok("CREATE TABLE d1 (id int)").await;
    assert_eq!(a.err("CREATE TABLE d2 (id int)").await.code, "0A000");
    a.ok("ROLLBACK").await;

    a.ok("BEGIN").await;
    a.ok("INSERT INTO t VALUES (3, 0)").await;
    assert_eq!(a.err("CREATE TABLE d3 (id int)").await.code, "0A000");
    a.ok("ROLLBACK").await;

    a.ok("BEGIN").await;
    assert_eq!(a.err("INSERT INTO t VALUES (1, 0)").await.code, "23505");
    assert_eq!(a.err("SELECT 1").await.code, "25P02");
    a.ok("ROLLBACK").await;
    assert_eq!(
        a.value("SELECT count(*) FROM t").await.as_deref(),
        Some("2")
    );
}

/// FR-023: with `snapshot_at_begin` off the snapshot is taken by the first statement, as before.
#[tokio::test]
async fn without_the_pin_the_snapshot_is_taken_by_the_first_statement() {
    let options = ServerOptions {
        occ: OccOptions {
            snapshot_at_begin: false,
            ..OccOptions::default()
        },
        ..ServerOptions::default()
    };
    let (_server, a, b) = setup(options).await;
    b.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 555 WHERE id = 2").await;
    assert_eq!(
        b.value("SELECT v FROM t WHERE id = 2").await.as_deref(),
        Some("555")
    );
    b.ok("COMMIT").await;
}
