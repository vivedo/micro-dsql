//! Autocommit statements behave like single-statement DSQL transactions (US1): no lock
//! contention reaches the client, the row limit applies, multi-statement strings are atomic.

use std::sync::Arc;
use std::time::Duration;

use micro_dsql_policy::PolicyLimits;
use micro_dsql_server::{OccOptions, ServerOptions};

use crate::common::{DbErr, TestServer};
use crate::{oc000, oc001, Session};

fn small_limit(rows: u64) -> ServerOptions {
    ServerOptions {
        limits: PolicyLimits {
            max_rows_per_txn: rows,
            ..PolicyLimits::default()
        },
        occ: OccOptions::default(),
    }
}

/// SC-001, research E-P2: 8 sessions x 150 autocommit statements (distinct-row INSERT, hot-row
/// UPDATE). Plain engine autocommit gives ~390 `55P03` here; as single-statement transactions
/// inserts never fail and every failure is OC000.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn autocommit_storm_never_fails_an_insert_and_only_reports_oc000() {
    let server = Arc::new(TestServer::start());
    let setup = Session::new(&server).await;
    setup
        .ok("CREATE TABLE storm (id int PRIMARY KEY, v int)")
        .await;
    setup.ok("INSERT INTO storm VALUES (0, 0)").await;
    let mut tasks = Vec::new();
    for worker in 0..8 {
        let server = server.clone();
        tasks.push(tokio::spawn(async move {
            let s = Session::new(&server).await;
            let (mut inserted, mut updated) = (0u64, 0u64);
            let mut insert_errors = Vec::new();
            let mut update_errors = Vec::new();
            for i in 0..150 {
                if i % 2 == 0 {
                    let sql = format!("INSERT INTO storm VALUES ({}, 0)", 1 + worker * 1000 + i);
                    match s.try_ok(&sql).await {
                        Ok(()) => inserted += 1,
                        Err(e) => insert_errors.push(e),
                    }
                } else {
                    match s.try_ok("UPDATE storm SET v = v + 1 WHERE id = 0").await {
                        Ok(()) => updated += 1,
                        Err(e) => update_errors.push(e),
                    }
                }
            }
            (inserted, updated, insert_errors, update_errors)
        }));
    }
    let (mut inserted, mut updated) = (0, 0);
    for task in tasks {
        let (i, u, insert_errors, update_errors) = task.await.unwrap();
        inserted += i;
        updated += u;
        assert!(
            insert_errors.is_empty(),
            "inserts must not fail: {insert_errors:?}"
        );
        for e in update_errors {
            assert_eq!(e, oc000(), "{e:?}");
        }
    }
    assert_eq!(
        setup.value("SELECT count(*) FROM storm").await,
        Some((inserted + 1).to_string())
    );
    assert_eq!(
        setup.value("SELECT v FROM storm WHERE id = 0").await,
        Some(updated.to_string())
    );
}

/// FR-005: the 3,000-row limit applies to every autocommit DML statement, and nothing of a
/// refused statement is applied.
#[tokio::test]
async fn autocommit_row_limit_refuses_and_applies_nothing() {
    let server = TestServer::start();
    let s = Session::new(&server).await;
    s.ok("CREATE TABLE big (id int PRIMARY KEY, v int)").await;
    let limit = DbErr::new("54000", "transaction row limit exceeded");

    // INSERT ... SELECT over the limit.
    assert_eq!(
        s.err("INSERT INTO big SELECT g, 0 FROM generate_series(1, 3001) g")
            .await,
        limit
    );
    assert_eq!(
        s.value("SELECT count(*) FROM big").await.as_deref(),
        Some("0")
    );

    // Exactly the limit succeeds.
    s.ok("INSERT INTO big SELECT g, 0 FROM generate_series(1, 3000) g")
        .await;
    assert_eq!(
        s.value("SELECT count(*) FROM big").await.as_deref(),
        Some("3000")
    );

    // An UPDATE of 3,001 rows: one more row, then update all.
    s.ok("INSERT INTO big VALUES (3001, 0)").await;
    assert_eq!(s.err("UPDATE big SET v = 1").await, limit);
    assert_eq!(
        s.value("SELECT count(*) FROM big WHERE v = 1")
            .await
            .as_deref(),
        Some("0")
    );

    // A DELETE of 3,001 rows.
    assert_eq!(s.err("DELETE FROM big").await, limit);
    assert_eq!(
        s.value("SELECT count(*) FROM big").await.as_deref(),
        Some("3001")
    );
    // The session is usable.
    assert_eq!(s.value("SELECT 1").await.as_deref(), Some("1"));
}

/// The same with a small limit, for the cases where the size is not the point.
#[tokio::test]
async fn autocommit_row_limit_is_configurable() {
    let server = TestServer::start_with(small_limit(5));
    let s = Session::new(&server).await;
    s.ok("CREATE TABLE small (id int PRIMARY KEY, v int)").await;
    s.ok("INSERT INTO small SELECT g, 0 FROM generate_series(1, 5) g")
        .await;
    s.ok("UPDATE small SET v = 1 WHERE id <= 5").await;
    s.ok("INSERT INTO small VALUES (6, 0)").await;
    let e = s.err("UPDATE small SET v = 2").await;
    assert_eq!(e.code, "54000");
    assert_eq!(
        s.value("SELECT count(*) FROM small WHERE v = 2")
            .await
            .as_deref(),
        Some("0")
    );
    // RETURNING statements are counted by the rows they changed.
    let e = s.err("DELETE FROM small RETURNING id").await;
    assert_eq!(e.code, "54000");
    assert_eq!(
        s.value("SELECT count(*) FROM small").await.as_deref(),
        Some("6")
    );
}

/// FR-006: a simple-protocol string with several statements is one implicit transaction.
#[tokio::test]
async fn multi_statement_strings_are_atomic() {
    let server = TestServer::start();
    let s = Session::new(&server).await;
    s.ok("CREATE TABLE ms (id int PRIMARY KEY)").await;

    // A duplicate key in the second statement undoes the first.
    let e = s
        .err("INSERT INTO ms VALUES (30); INSERT INTO ms VALUES (30)")
        .await;
    assert_eq!(e.code, "23505");
    assert_eq!(
        s.value("SELECT count(*) FROM ms WHERE id = 30")
            .await
            .as_deref(),
        Some("0")
    );
    // ReadyForQuery status is idle: a following BEGIN works and the session is not in a block.
    s.ok("BEGIN").await;
    s.ok("COMMIT").await;

    // DDL then a failing statement: the table is not created.
    let e = s
        .err("CREATE TABLE a (id int PRIMARY KEY); SELECT * FROM missing")
        .await;
    assert_eq!(e.code, "42P01");
    let e = s.err("SELECT * FROM a").await;
    assert_eq!(e.code, "42P01");

    // Success applies both.
    s.ok("INSERT INTO ms VALUES (1); INSERT INTO ms VALUES (2)")
        .await;
    assert_eq!(
        s.value("SELECT count(*) FROM ms").await.as_deref(),
        Some("2")
    );

    // A string with explicit transaction control keeps its per-statement behaviour.
    let e = s
        .err("INSERT INTO ms VALUES (40); BEGIN; INSERT INTO ms VALUES (40); COMMIT")
        .await;
    assert_eq!(e.code, "23505");
    // The first INSERT was applied by itself; the block the failure left is aborted.
    let _ = s.client.simple_query("ROLLBACK").await;
    assert_eq!(
        s.value("SELECT count(*) FROM ms WHERE id = 40")
            .await
            .as_deref(),
        Some("1")
    );

    // Only reads and session statements: not wrapped, results as before.
    let rows = s.rows("SELECT 1; SELECT 2").await;
    assert_eq!(rows.len(), 2);
}

/// Statements before the failing one answer normally, then the error; the row counter of the
/// implicit transaction covers the whole string.
#[tokio::test]
async fn multi_statement_strings_count_rows_as_one_transaction() {
    let server = TestServer::start_with(small_limit(3));
    let s = Session::new(&server).await;
    s.ok("CREATE TABLE r (id int PRIMARY KEY)").await;
    let e = s
        .err("INSERT INTO r VALUES (1), (2); INSERT INTO r VALUES (3), (4)")
        .await;
    assert_eq!(e.code, "54000");
    assert_eq!(
        s.value("SELECT count(*) FROM r").await.as_deref(),
        Some("0")
    );
}

/// E-P1: an autocommit UPDATE of a row another session wrote (uncommitted) fails with OC000 and
/// leaves the session idle; other rows are fine.
#[tokio::test]
async fn autocommit_update_of_a_row_in_another_open_transaction_is_oc000() {
    let server = TestServer::start();
    let a = Session::new(&server).await;
    let b = Session::new(&server).await;
    a.ok("CREATE TABLE t (id int PRIMARY KEY, v int)").await;
    a.ok("INSERT INTO t VALUES (1, 0), (2, 0)").await;
    a.ok("BEGIN").await;
    a.ok("UPDATE t SET v = 1 WHERE id = 1").await;
    assert_eq!(b.err("UPDATE t SET v = 2 WHERE id = 1").await, oc000());
    assert_eq!(b.value("SELECT 1").await.as_deref(), Some("1"));
    b.ok("UPDATE t SET v = 2 WHERE id = 2").await;
    a.ok("COMMIT").await;
    assert_eq!(
        a.value("SELECT v FROM t WHERE id = 1").await.as_deref(),
        Some("1")
    );
    assert_eq!(
        a.value("SELECT v FROM t WHERE id = 2").await.as_deref(),
        Some("2")
    );
}

/// Commits `a`'s open transaction after `after` on its own task.
fn commit_later(a: &Arc<Session>, after: Duration) -> tokio::task::JoinHandle<()> {
    let a = a.clone();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        a.ok("COMMIT").await;
    })
}

/// E-P4b, E-P9: while another session holds an open DDL transaction, no autocommit statement
/// sees `55P03`. An autocommit DDL statement is retried until the other DDL commits (or fails
/// with OC001 when it is held past the budget). Autocommit DML is rejected by the engine as a
/// write conflict at its COMMIT (the engine does not tell it from a real one, so it is not
/// retried): OC000, the documented B11 difference, and DSQL clients retry it.
#[tokio::test]
async fn autocommit_against_an_open_ddl_transaction_never_leaks_lock_errors() {
    let server = TestServer::start();
    let a = Arc::new(Session::new(&server).await);
    let b = Session::new(&server).await;
    a.ok("CREATE TABLE t (id int PRIMARY KEY, v int)").await;

    a.ok("BEGIN").await;
    a.ok("CREATE TABLE ddl_open (id int)").await;
    let dml = b.try_ok("INSERT INTO t VALUES (10, 0)").await;
    assert_eq!(dml, Err(oc000()));
    // Nothing was applied; the session is idle and usable.
    assert_eq!(
        b.value("SELECT count(*) FROM t").await.as_deref(),
        Some("0")
    );
    a.ok("COMMIT").await;
    // Once the DDL has committed the same statement goes through.
    b.ok("INSERT INTO t VALUES (10, 0)").await;

    a.ok("BEGIN").await;
    a.ok("CREATE TABLE ddl_open2 (id int)").await;
    let release = commit_later(&a, Duration::from_millis(20));
    let ddl = b.try_ok("CREATE TABLE b_made (id int)").await;
    release.await.unwrap();
    assert_eq!(
        ddl,
        Ok(()),
        "the statement is retried until the lock is free"
    );
    assert_eq!(
        b.value("SELECT count(*) FROM b_made").await.as_deref(),
        Some("0")
    );

    // Held past the retry budget: OC001, and nothing applied.
    a.ok("BEGIN").await;
    a.ok("CREATE TABLE ddl_open3 (id int)").await;
    assert_eq!(b.err("CREATE TABLE b_never (id int)").await, oc001());
    a.ok("COMMIT").await;
    assert_eq!(b.err("SELECT * FROM b_never").await.code, "42P01");
}

/// FR-008: the extended protocol takes the same path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_protocol_autocommit_storm_and_errors() {
    let server = Arc::new(TestServer::start());
    let setup = Session::new(&server).await;
    setup
        .ok("CREATE TABLE ex (id int PRIMARY KEY, v int)")
        .await;
    setup.ok("INSERT INTO ex VALUES (0, 0)").await;
    let mut tasks = Vec::new();
    for worker in 0..4i32 {
        let server = server.clone();
        tasks.push(tokio::spawn(async move {
            let client = server.connect().await;
            let insert = client
                .prepare("INSERT INTO ex VALUES ($1, 0)")
                .await
                .unwrap();
            let update = client
                .prepare("UPDATE ex SET v = v + 1 WHERE id = $1")
                .await
                .unwrap();
            let mut errors = Vec::new();
            for i in 0..60i32 {
                let result = if i % 2 == 0 {
                    client.execute(&insert, &[&(1 + worker * 1000 + i)]).await
                } else {
                    client.execute(&update, &[&0i32]).await
                };
                if let Err(e) = result {
                    errors.push((i % 2 == 0, DbErr::of(&e)));
                }
            }
            errors
        }));
    }
    for task in tasks {
        for (is_insert, e) in task.await.unwrap() {
            assert!(!is_insert, "insert failed: {e:?}");
            assert_eq!(e, oc000(), "{e:?}");
        }
    }

    let c = server.connect().await;
    let insert = c.prepare("INSERT INTO ex VALUES ($1, 0)").await.unwrap();
    // A duplicate key: 23505, session idle.
    let e = c.execute(&insert, &[&0i32]).await.unwrap_err();
    assert_eq!(DbErr::of(&e).code, "23505");
    assert!(!c.is_closed());
    assert_eq!(
        crate::common::value(&c, "SELECT 1").await.as_deref(),
        Some("1")
    );
    // The row limit, with the table unchanged.
    let big = c
        .prepare("INSERT INTO ex SELECT g + 100000, 0 FROM generate_series(1, $1::int) g")
        .await
        .unwrap();
    let before = crate::common::value(&c, "SELECT count(*) FROM ex").await;
    let e = c.execute(&big, &[&3001i32]).await.unwrap_err();
    assert_eq!(
        DbErr::of(&e),
        DbErr::new("54000", "transaction row limit exceeded")
    );
    assert_eq!(
        crate::common::value(&c, "SELECT count(*) FROM ex").await,
        before
    );
}

/// 006's server-side steps under contention: an `ASYNC` index build and a NOT VALID rebuild
/// retried while another session's DDL transaction commits; one job row per job statement; OC001
/// (and no change) when the lock is held past the budget.
#[tokio::test]
async fn autocommit_ddl_with_server_steps_under_contention() {
    let server = TestServer::start();
    let a = Arc::new(Session::new(&server).await);
    let b = Session::new(&server).await;
    b.ok("CREATE TABLE parent (id int PRIMARY KEY)").await;
    b.ok("CREATE TABLE child (id int PRIMARY KEY, parent_id int, v int)")
        .await;
    b.ok("CREATE TABLE t (id int PRIMARY KEY, v int)").await;
    b.ok("INSERT INTO parent VALUES (1)").await;
    b.ok("INSERT INTO child VALUES (1, 1, 0)").await;

    a.ok("BEGIN").await;
    a.ok("CREATE TABLE hold (id int PRIMARY KEY)").await;
    let release = commit_later(&a, Duration::from_millis(50));
    let index = b.try_ok("CREATE INDEX ASYNC t_v ON t (v)").await;
    release.await.unwrap();
    assert!(index.is_ok(), "{index:?}");

    a.ok("BEGIN").await;
    a.ok("CREATE TABLE hold2 (id int PRIMARY KEY)").await;
    let release = commit_later(&a, Duration::from_millis(50));
    let fk = b
        .try_ok(
            "ALTER TABLE child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) \
             REFERENCES parent (id) NOT VALID",
        )
        .await;
    release.await.unwrap();
    assert!(fk.is_ok(), "{fk:?}");

    let all_jobs = b.value("SELECT count(*) FROM sys.jobs").await;
    assert_eq!(
        all_jobs.as_deref(),
        Some("1"),
        "one job row per job statement"
    );

    // Lock held past the budget: OC001, state unchanged.
    a.ok("BEGIN").await;
    a.ok("CREATE TABLE hold3 (id int PRIMARY KEY)").await;
    let e = b.err("CREATE INDEX ASYNC t_id2 ON t (id, v)").await;
    assert_eq!(e, oc001());
    let e = b
        .err("ALTER TABLE child ADD CONSTRAINT child_v_chk CHECK (v >= 0) NOT VALID")
        .await;
    assert_eq!(e, oc001());
    a.ok("COMMIT").await;
    assert_eq!(
        b.value("SELECT count(*) FROM sys.jobs").await.as_deref(),
        Some("1")
    );
    // The check constraint was not added: a negative value is accepted.
    b.ok("UPDATE child SET v = -1 WHERE id = 1").await;
}
