//! The emulated cells of the conflict matrix (research/01 section 2): which statement fails
//! when two transactions touch the same data, and with what.
//!
//! The table is `t (id int PRIMARY KEY, v int, u int UNIQUE)` with rows (1, 0, 1) and
//! (2, 0, 2). DSQL reports every conflict at COMMIT of the later committer; micro-dsql reports
//! it at COMMIT of the later *writer* (a write conflict inside an explicit transaction is
//! deferred: the statement answers as if it had worked, the transaction is doomed).

use std::time::Duration;

use micro_dsql_policy::PolicyLimits;
use micro_dsql_server::{OccOptions, ServerOptions};

use crate::common::TestServer;
use crate::{oc000, oc001, Session};

async fn setup() -> (TestServer, Session, Session) {
    setup_with(ServerOptions::default()).await
}

async fn setup_with(options: ServerOptions) -> (TestServer, Session, Session) {
    let server = TestServer::start_with(options);
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

/// M1, S1 (the later writer, A commits before B writes): B's UPDATE answers with its normal
/// row count, B's COMMIT fails with OC000, the session is idle, A's value stays.
#[tokio::test]
async fn m1_update_update_conflict_is_reported_at_the_later_writers_commit() {
    let (_server, a, b) = setup().await;
    b.ok("BEGIN").await;
    b.ok("SELECT v FROM t WHERE id = 1").await;
    a.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    a.ok("COMMIT").await;
    // B's snapshot predates A's commit: the engine conflicts here; the server defers it.
    assert_eq!(b.affected("UPDATE t SET v = 2 WHERE id = 1").await, 1);
    assert_eq!(b.err("COMMIT").await, oc000());
    // Idle again: no open block, the next transaction works.
    assert_eq!(v(&b, 1).await, "1");
    b.ok("BEGIN").await;
    b.ok("UPDATE t SET v = 3 WHERE id = 1").await;
    b.ok("COMMIT").await;
    assert_eq!(v(&a, 1).await, "3");
}

/// S1x: both write while both are open; B's write meets A's uncommitted one.
#[tokio::test]
async fn m1_update_update_while_the_other_writer_is_still_open() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    // The conflict persists after the replay: the statement reports no rows.
    assert_eq!(b.affected("UPDATE t SET v = 2 WHERE id = 1").await, 0);
    a.ok("COMMIT").await;
    assert_eq!(b.err("COMMIT").await, oc000());
    assert_eq!(v(&a, 1).await, "1");
}

/// M3: DELETE against UPDATE of the same row, both orders.
#[tokio::test]
async fn m3_delete_update_conflict() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("DELETE FROM t WHERE id = 2").await;
    assert_eq!(b.affected("UPDATE t SET v = 9 WHERE id = 2").await, 0);
    a.ok("COMMIT").await;
    assert_eq!(b.err("COMMIT").await, oc000());
    assert_eq!(
        a.value("SELECT count(*) FROM t WHERE id = 2")
            .await
            .as_deref(),
        Some("0")
    );

    // The other order: UPDATE first, then DELETE.
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    b.ok("SELECT 1 FROM t WHERE id = 1").await;
    a.ok("UPDATE t SET v = 7 WHERE id = 1").await;
    a.ok("COMMIT").await;
    b.ok("DELETE FROM t WHERE id = 1").await;
    assert_eq!(b.err("COMMIT").await, oc000());
    assert_eq!(v(&a, 1).await, "7");
}

/// M4: an UPDATE of a key (unique) column against an UPDATE of a non-key column of one row.
#[tokio::test]
async fn m4_key_update_against_non_key_update() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    b.ok("SELECT 1 FROM t WHERE id = 1").await;
    a.ok("UPDATE t SET u = 100 WHERE id = 1").await;
    a.ok("COMMIT").await;
    b.ok("UPDATE t SET v = 5 WHERE id = 1").await;
    assert_eq!(b.err("COMMIT").await, oc000());
    assert_eq!(
        a.value("SELECT u FROM t WHERE id = 1").await.as_deref(),
        Some("100")
    );
    assert_eq!(v(&a, 1).await, "0");
}

/// M2: two transactions insert the same primary key / the same UNIQUE value: the second
/// COMMIT fails with OC000 (unchanged path, FR-014).
#[tokio::test]
async fn m2_same_key_inserts_conflict_at_commit() {
    let (_server, a, b) = setup().await;
    for (first, second) in [
        (
            "INSERT INTO t VALUES (5, 0, 50)",
            "INSERT INTO t VALUES (5, 1, 51)",
        ),
        (
            "INSERT INTO t VALUES (6, 0, 60)",
            "INSERT INTO t VALUES (7, 1, 60)",
        ),
    ] {
        a.ok("BEGIN").await;
        b.ok("BEGIN").await;
        a.ok(first).await;
        // Refused at the statement or at COMMIT, always as OC000.
        let at_statement = b.try_ok(second).await;
        a.ok("COMMIT").await;
        match at_statement {
            Ok(()) => assert_eq!(b.err("COMMIT").await, oc000(), "{second}"),
            Err(e) => {
                assert_eq!(e, oc000(), "{second}");
                // A doomed or aborted block: COMMIT does not commit.
                let _ = b.try_ok("COMMIT").await;
            }
        }
    }
    assert_eq!(
        a.value("SELECT count(*) FROM t WHERE id >= 5")
            .await
            .as_deref(),
        Some("2")
    );
}

/// M8: write skew is allowed (snapshot isolation, like DSQL).
#[tokio::test]
async fn m8_write_skew_both_commit() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("SELECT v FROM t WHERE id = 1").await;
    b.ok("SELECT v FROM t WHERE id = 2").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 2").await;
    b.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    a.ok("COMMIT").await;
    b.ok("COMMIT").await;
    assert_eq!((v(&a, 1).await, v(&a, 2).await), ("1".into(), "1".into()));
}

/// S7 (FR-015): a schema conflict is reported at COMMIT as OC001 and never deferred.
#[tokio::test]
async fn schema_conflict_is_reported_at_commit_as_oc001() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    b.ok("ALTER TABLE t ADD COLUMN extra int").await;
    a.ok("SELECT 1").await;
    assert_eq!(a.err("COMMIT").await, oc001());
    assert_eq!(v(&a, 1).await, "0");
}

/// A ROLLBACK of a doomed transaction is silent and applies nothing.
#[tokio::test]
async fn rollback_of_a_doomed_transaction_is_silent() {
    let (_server, a, b) = setup().await;
    b.ok("BEGIN").await;
    b.ok("SELECT 1 FROM t WHERE id = 1").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    b.ok("UPDATE t SET v = 2 WHERE id = 1").await;
    b.ok("ROLLBACK").await;
    assert_eq!(v(&b, 1).await, "1");
}

/// Nothing of a doomed transaction is ever committed (FR-013), including statements that ran
/// after the deferral, and COMMIT keeps failing until the transaction ends.
#[tokio::test]
async fn nothing_of_a_doomed_transaction_is_committed() {
    let (_server, a, b) = setup().await;
    b.ok("BEGIN").await;
    b.ok("INSERT INTO t VALUES (50, 0, 500)").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    b.ok("UPDATE t SET v = 2 WHERE id = 1").await;
    b.ok("INSERT INTO t VALUES (51, 0, 510)").await;
    assert_eq!(b.err("COMMIT").await, oc000());
    assert_eq!(
        a.value("SELECT count(*) FROM t WHERE id >= 50")
            .await
            .as_deref(),
        Some("0")
    );
    assert_eq!(v(&a, 1).await, "1");
}

/// A later error in a doomed transaction aborts the block as usual: 25P02 until ROLLBACK, and
/// COMMIT of the aborted block rolls back (no error).
#[tokio::test]
async fn an_error_in_a_doomed_transaction_aborts_the_block() {
    let (_server, a, b) = setup().await;
    b.ok("BEGIN").await;
    b.ok("SELECT 1 FROM t WHERE id = 1").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    b.ok("UPDATE t SET v = 2 WHERE id = 1").await;
    assert_eq!(b.err("INSERT INTO t VALUES (1, 0, 99)").await.code, "23505");
    assert_eq!(b.err("SELECT 1").await.code, "25P02");
    b.ok("COMMIT").await;
    assert_eq!(v(&b, 1).await, "1");
    assert_eq!(b.value("SELECT 1").await.as_deref(), Some("1"));
}

/// RETURNING of a statement whose replay conflicted again gives no rows.
#[tokio::test]
async fn a_statement_that_conflicts_again_returns_no_rows() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    assert!(b
        .rows("UPDATE t SET v = 2 WHERE id = 1 RETURNING id")
        .await
        .is_empty());
    // Still working (and still doomed) for further statements.
    assert_eq!(b.affected("UPDATE t SET v = 3 WHERE id = 1").await, 0);
    a.ok("COMMIT").await;
    assert_eq!(b.err("COMMIT").await, oc000());
}

/// A doomed transaction past the age limit reports the age error, which wins over OC000.
#[tokio::test]
async fn a_doomed_transaction_past_the_age_limit_reports_the_age_error() {
    let options = ServerOptions {
        limits: PolicyLimits {
            max_txn_age: Duration::from_secs(1),
            ..PolicyLimits::default()
        },
        ..ServerOptions::default()
    };
    let (_server, a, b) = setup_with(options).await;
    b.ok("BEGIN").await;
    b.ok("SELECT 1 FROM t WHERE id = 1").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    b.ok("UPDATE t SET v = 2 WHERE id = 1").await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let e = b.err("COMMIT").await;
    assert_eq!(e.code, "54000");
    assert!(e.message.contains("age limit"), "{e:?}");
}

/// FR-016: with deferral off the conflict is reported at the statement, as the engine finds it.
#[tokio::test]
async fn without_deferral_the_conflict_is_reported_at_the_statement() {
    let options = ServerOptions {
        occ: OccOptions {
            defer_conflicts: false,
            ..OccOptions::default()
        },
        ..ServerOptions::default()
    };
    let (_server, a, b) = setup_with(options).await;
    b.ok("BEGIN").await;
    b.ok("SELECT 1 FROM t WHERE id = 1").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    assert_eq!(b.err("UPDATE t SET v = 2 WHERE id = 1").await, oc000());
    // The block is aborted; COMMIT answers ROLLBACK.
    assert_eq!(b.err("SELECT 1").await.code, "25P02");
    b.ok("COMMIT").await;
    assert_eq!(v(&b, 1).await, "1");
}
