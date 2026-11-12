//! The cells where micro-dsql knowingly differs from DSQL (research/01 section 2). Each
//! scenario asserts what micro-dsql does today and names its entry in docs/known-gaps.md; if a
//! difference is ever closed the scenario changes with it.

use micro_dsql_server::ServerOptions;

use crate::common::TestServer;
use crate::{oc000, oc001, Session};

async fn setup() -> (TestServer, Session, Session) {
    let server = TestServer::start_with(ServerOptions::default());
    let a = Session::new(&server).await;
    let b = Session::new(&server).await;
    a.ok("CREATE TABLE t (id int PRIMARY KEY, v int)").await;
    a.ok("INSERT INTO t VALUES (1, 0), (2, 0)").await;
    (server, a, b)
}

/// D1, known-gaps "OCC: FOR KEY SHARE": DSQL's `FOR KEY SHARE` conflicts with a concurrent
/// DELETE (or key UPDATE) of the row; here it conflicts with nothing.
#[tokio::test]
async fn d1_for_key_share_does_not_conflict_with_a_delete() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    assert_eq!(
        a.rows("SELECT id FROM t WHERE id = 1 FOR KEY SHARE")
            .await
            .len(),
        1
    );
    b.ok("DELETE FROM t WHERE id = 1").await;
    a.ok("COMMIT").await;
    b.ok("COMMIT").await;
    assert_eq!(
        a.value("SELECT count(*) FROM t WHERE id = 1")
            .await
            .as_deref(),
        Some("0")
    );
}

/// D2, known-gaps "OCC: foreign keys take no key share": deleting a parent while a child is
/// inserted both commit and leave an orphan (DSQL's implicit KEY SHARE makes one lose).
#[tokio::test]
async fn d2_parent_delete_and_child_insert_both_commit_leaving_an_orphan() {
    let (_server, a, b) = setup().await;
    a.ok("CREATE TABLE parent (id int PRIMARY KEY)").await;
    a.ok("CREATE TABLE child (id int PRIMARY KEY, pid int REFERENCES parent (id))")
        .await;
    a.ok("INSERT INTO parent VALUES (1), (2)").await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("DELETE FROM parent WHERE id = 2").await;
    b.ok("INSERT INTO child VALUES (1, 2)").await;
    a.ok("COMMIT").await;
    b.ok("COMMIT").await;
    assert_eq!(
        a.value("SELECT count(*) FROM child WHERE pid = 2")
            .await
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        a.value("SELECT count(*) FROM parent WHERE id = 2")
            .await
            .as_deref(),
        Some("0")
    );
}

/// D3, known-gaps "OCC: the later writer loses": DSQL fails the later *committer*; here a
/// write that meets another transaction's earlier write dooms the later writer, even when it
/// commits first.
#[tokio::test]
async fn d3_the_earlier_writer_wins_even_when_the_later_one_commits_first() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    b.ok("UPDATE t SET v = 2 WHERE id = 1").await;
    // DSQL: B commits first and wins. Here B is doomed.
    assert_eq!(b.err("COMMIT").await, oc000());
    a.ok("COMMIT").await;
    assert_eq!(
        a.value("SELECT v FROM t WHERE id = 1").await.as_deref(),
        Some("1")
    );
}

/// D4, known-gaps "OCC: a rolled-back writer still dooms": B met A's uncommitted write and is
/// doomed although A rolled back (DSQL: B commits).
#[tokio::test]
async fn d4_a_rolled_back_first_writer_still_dooms_the_second() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    b.ok("UPDATE t SET v = 2 WHERE id = 1").await;
    a.ok("ROLLBACK").await;
    assert_eq!(b.err("COMMIT").await, oc000());
    assert_eq!(
        a.value("SELECT v FROM t WHERE id = 1").await.as_deref(),
        Some("0")
    );
}

/// D4b, known-gaps "OCC: a doomed transaction restarts": the earlier writes of the doomed
/// transaction are discarded when the conflict is deferred and later statements read a fresh
/// snapshot (nothing of it is ever committed).
#[tokio::test]
async fn d4b_a_doomed_transaction_reads_a_fresh_snapshot() {
    let (_server, a, b) = setup().await;
    b.ok("BEGIN").await;
    b.ok("UPDATE t SET v = 5 WHERE id = 2").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    assert_eq!(
        b.value("SELECT v FROM t WHERE id = 1").await.as_deref(),
        Some("0")
    );
    // The conflict on r1 is deferred: the transaction restarts, its write of r2 is gone and
    // the snapshot is now A's commit.
    b.ok("UPDATE t SET v = 6 WHERE id = 1").await;
    assert_eq!(
        b.value("SELECT v FROM t WHERE id = 2").await.as_deref(),
        Some("0")
    );
    assert_eq!(
        b.value("SELECT v FROM t WHERE id = 1").await.as_deref(),
        Some("6")
    );
    assert_eq!(b.err("COMMIT").await, oc000());
}

/// D5, known-gaps B11: an unrelated DDL fails a writing transaction at COMMIT with OC001, and
/// an unrelated write fails with OC000 while another session's DDL transaction is open.
#[tokio::test]
async fn d5_unrelated_ddl_conflicts_with_writers() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    b.ok("CREATE TABLE unrelated (id int)").await;
    assert_eq!(a.err("COMMIT").await, oc001());

    b.ok("BEGIN").await;
    b.ok("CREATE TABLE unrelated2 (id int)").await;
    assert_eq!(a.err("INSERT INTO t VALUES (9, 0)").await, oc000());
    b.ok("COMMIT").await;
}

/// known-gaps "OCC: tables created after BEGIN": the snapshot is taken at BEGIN, so a table
/// another session creates afterwards does not exist for the transaction.
#[tokio::test]
async fn tables_created_after_begin_are_invisible() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("CREATE TABLE fresh (id int)").await;
    assert_eq!(a.err("SELECT * FROM fresh").await.code, "42P01");
    a.ok("ROLLBACK").await;
    assert_eq!(
        a.value("SELECT count(*) FROM fresh").await.as_deref(),
        Some("0")
    );
}

/// known-gaps "OCC: lock contention in an explicit transaction": DDL that meets another
/// session's open DDL transaction cannot be retried inside a transaction and is reported as
/// OC001, aborting the block.
#[tokio::test]
async fn residual_contention_inside_an_explicit_transaction_is_oc001() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    a.ok("CREATE TABLE held (id int)").await;
    b.ok("BEGIN").await;
    assert_eq!(b.err("CREATE TABLE other (id int)").await, oc001());
    assert_eq!(b.err("SELECT 1").await.code, "25P02");
    b.ok("ROLLBACK").await;
    a.ok("COMMIT").await;
}

/// known-gaps "OCC: NOWAIT / SKIP LOCKED": accepted and ignored; there are no row locks to wait
/// for, so the second locker proceeds and the conflict shows at its COMMIT.
#[tokio::test]
async fn nowait_and_skip_locked_are_no_ops() {
    let (_server, a, b) = setup().await;
    a.ok("BEGIN").await;
    b.ok("BEGIN").await;
    a.ok("SELECT * FROM t WHERE id = 1 FOR UPDATE").await;
    for clause in ["NOWAIT", "SKIP LOCKED"] {
        let rows = b
            .rows(&format!(
                "SELECT id FROM t WHERE id = 1 FOR UPDATE {clause}"
            ))
            .await;
        assert_eq!(rows.len(), 1, "{clause}: the row is returned, not skipped");
    }
    a.ok("COMMIT").await;
    assert_eq!(b.err("COMMIT").await, oc000());
}
