//! DSQL rules through a real client, on both wire protocols: transaction discipline and limits,
//! and (further down) the statement deny rules. Every assertion is on SQLSTATE and exact message.

use std::time::Duration;

use micro_dsql_policy::PolicyLimits;
use micro_dsql_server::ServerOptions;
use tokio_postgres::Client;

use crate::common::{rows, simple_err, value, DbErr, TestServer};

/// The two wire protocols a client can use.
#[derive(Clone, Copy, Debug)]
pub enum Proto {
    Simple,
    Extended,
}

pub const PROTOS: [Proto; 2] = [Proto::Simple, Proto::Extended];

impl Proto {
    /// Runs one statement; `Err` is the server error.
    pub async fn run(self, c: &Client, sql: &str) -> Result<(), DbErr> {
        let result = match self {
            Proto::Simple => c.simple_query(sql).await.map(|_| ()),
            Proto::Extended => c.query(sql, &[]).await.map(|_| ()),
        };
        result.map_err(|e| DbErr::of(&e))
    }

    pub async fn ok(self, c: &Client, sql: &str) {
        if let Err(e) = self.run(c, sql).await {
            panic!("{self:?}: {sql}: {e:?}");
        }
    }

    pub async fn err(self, c: &Client, sql: &str) -> DbErr {
        match self.run(c, sql).await {
            Ok(()) => panic!("{self:?}: {sql}: expected an error"),
            Err(e) => e,
        }
    }
}

pub fn e(code: &str, message: &str) -> DbErr {
    DbErr::new(code, message)
}

pub const ABORTED: &str =
    "current transaction is aborted, commands ignored until end of transaction block";

/// `sql` fails with `expected` inside a transaction that already ran `before`; the transaction is
/// then aborted (`25P02`), and nothing of it survives the ROLLBACK.
async fn refused_in_txn(proto: Proto, c: &Client, before: &[&str], sql: &str, expected: DbErr) {
    proto.ok(c, "BEGIN").await;
    for b in before {
        proto.ok(c, b).await;
    }
    assert_eq!(proto.err(c, sql).await, expected, "{proto:?}: {sql}");
    assert_eq!(
        proto.err(c, "SELECT 1").await,
        e("25P02", ABORTED),
        "{proto:?}"
    );
    proto.ok(c, "ROLLBACK").await;
}

const TWO_DDL: &str = "multiple ddl statements not supported in a transaction";
const DDL_DML: &str = "ddl and dml are not supported in the same transaction";
const ROW_LIMIT: &str = "transaction row limit exceeded";

#[tokio::test]
async fn two_ddl_statements_in_one_transaction() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        let t = format!("two_ddl_{proto:?}");
        refused_in_txn(
            proto,
            &c,
            &[&format!("CREATE TABLE {t}_a (id int)")],
            &format!("CREATE TABLE {t}_b (id int)"),
            e("0A000", TWO_DDL),
        )
        .await;
        // Nothing of the rolled-back transaction exists.
        assert_eq!(
            simple_err(&c, &format!("SELECT * FROM {t}_a")).await.code,
            "42P01"
        );
    }
}

#[tokio::test]
async fn ddl_and_dml_in_one_transaction_in_both_orders() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE mix (id int PRIMARY KEY)").await;
    for proto in PROTOS {
        let t = format!("ddl_dml_{proto:?}");
        refused_in_txn(
            proto,
            &c,
            &[&format!("CREATE TABLE {t} (id int)")],
            &format!("INSERT INTO {t} VALUES (1)"),
            e("0A000", DDL_DML),
        )
        .await;
        assert_eq!(
            simple_err(&c, &format!("SELECT * FROM {t}")).await.code,
            "42P01"
        );
        refused_in_txn(
            proto,
            &c,
            &["INSERT INTO mix VALUES (1)"],
            &format!("CREATE INDEX {t}_i ON mix (id)"),
            e("0A000", DDL_DML),
        )
        .await;
        assert_eq!(
            value(&c, "SELECT count(*) FROM mix").await.as_deref(),
            Some("0")
        );
    }
}

#[tokio::test]
async fn a_read_only_transaction_may_run_one_ddl() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        let t = format!("ro_ddl_{proto:?}");
        proto.ok(&c, "BEGIN").await;
        proto.ok(&c, "SELECT 1").await;
        proto.ok(&c, &format!("CREATE TABLE {t} (id int)")).await;
        proto.ok(&c, "SELECT 2").await;
        proto.ok(&c, "COMMIT").await;
        assert_eq!(
            value(&c, &format!("SELECT count(*) FROM {t}"))
                .await
                .as_deref(),
            Some("0")
        );
    }
}

#[tokio::test]
async fn savepoints_are_refused() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        for (sql, text) in [
            (
                "SAVEPOINT sp",
                "unsupported transaction statement: SAVEPOINT",
            ),
            ("RELEASE sp", "unsupported transaction statement: RELEASE"),
            (
                "RELEASE SAVEPOINT sp",
                "unsupported transaction statement: RELEASE",
            ),
            (
                "ROLLBACK TO sp",
                "unsupported transaction statement: ROLLBACK TO",
            ),
            (
                "ROLLBACK TO SAVEPOINT sp",
                "unsupported transaction statement: ROLLBACK TO",
            ),
        ] {
            // Inside a transaction: refused, the block is aborted.
            refused_in_txn(proto, &c, &[], sql, e("0A000", text)).await;
            // Outside a transaction: refused, no lasting effect.
            assert_eq!(proto.err(&c, sql).await, e("0A000", text));
            proto.ok(&c, "SELECT 1").await;
        }
    }
}

#[tokio::test]
async fn the_row_limit_is_3000_and_cumulative() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE lim (id int PRIMARY KEY)").await;
    let series = |from: i32, to: i32| {
        format!(
            "WITH RECURSIVE s(n) AS (SELECT {from} UNION ALL SELECT n + 1 FROM s WHERE n < {to}) \
             INSERT INTO lim SELECT n FROM s"
        )
    };
    for proto in PROTOS {
        rows(&c, "DELETE FROM lim").await;
        // Exactly 3,000 rows are fine.
        proto.ok(&c, "BEGIN").await;
        proto.ok(&c, &series(1, 3000)).await;
        proto.ok(&c, "COMMIT").await;
        assert_eq!(
            value(&c, "SELECT count(*) FROM lim").await.as_deref(),
            Some("3000")
        );
        rows(&c, "DELETE FROM lim WHERE id > 0").await;
        // 2,000 + 2,000 is not: the second statement fails and aborts the block.
        proto.ok(&c, "BEGIN").await;
        proto.ok(&c, &series(1, 2000)).await;
        assert_eq!(
            proto.err(&c, &series(2001, 4000)).await,
            e("54000", ROW_LIMIT),
            "{proto:?}"
        );
        assert_eq!(proto.err(&c, "SELECT 1").await, e("25P02", ABORTED));
        proto.ok(&c, "ROLLBACK").await;
        assert_eq!(
            value(&c, "SELECT count(*) FROM lim").await.as_deref(),
            Some("0")
        );
    }
}

#[tokio::test]
async fn returning_statements_count_their_rows() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE ret (id int PRIMARY KEY)").await;
    let series = |from: i32, to: i32| {
        format!(
            "WITH RECURSIVE s(n) AS (SELECT {from} UNION ALL SELECT n + 1 FROM s WHERE n < {to}) \
             INSERT INTO ret SELECT n FROM s RETURNING id"
        )
    };
    for proto in PROTOS {
        rows(&c, "DELETE FROM ret").await;
        proto.ok(&c, "BEGIN").await;
        proto.ok(&c, &series(1, 2000)).await;
        assert_eq!(
            proto.err(&c, &series(2001, 3001)).await,
            e("54000", ROW_LIMIT),
            "{proto:?}"
        );
        proto.ok(&c, "ROLLBACK").await;
        assert_eq!(
            value(&c, "SELECT count(*) FROM ret").await.as_deref(),
            Some("0")
        );
        // UPDATE ... RETURNING and DELETE ... RETURNING count too.
        proto.ok(&c, "BEGIN").await;
        proto.ok(&c, &series(1, 1500)).await;
        proto
            .ok(&c, "UPDATE ret SET id = id + 10000 RETURNING id")
            .await;
        assert_eq!(
            proto.err(&c, "DELETE FROM ret RETURNING id").await,
            e("54000", ROW_LIMIT),
            "{proto:?}"
        );
        proto.ok(&c, "ROLLBACK").await;
    }
}

#[tokio::test]
async fn an_autocommit_values_list_over_the_limit_is_refused_before_it_runs() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE big (id int PRIMARY KEY)").await;
    let values = |n: usize| {
        let list = (0..n)
            .map(|i| format!("({i})"))
            .collect::<Vec<_>>()
            .join(",");
        format!("INSERT INTO big VALUES {list}")
    };
    for proto in PROTOS {
        assert_eq!(
            proto.err(&c, &values(3001)).await,
            e("54000", ROW_LIMIT),
            "{proto:?}"
        );
        assert_eq!(
            value(&c, "SELECT count(*) FROM big").await.as_deref(),
            Some("0")
        );
        proto.ok(&c, &values(3000)).await;
        assert_eq!(
            value(&c, "SELECT count(*) FROM big").await.as_deref(),
            Some("3000")
        );
        rows(&c, "DELETE FROM big").await;
    }
}

fn short_age_server() -> TestServer {
    TestServer::start_with(ServerOptions {
        limits: PolicyLimits {
            max_txn_age: Duration::from_secs(1),
            ..PolicyLimits::default()
        },
    })
}

#[tokio::test]
async fn a_statement_after_the_age_limit_fails_and_aborts() {
    let server = short_age_server();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE aged (id int PRIMARY KEY)").await;
    for proto in PROTOS {
        proto.ok(&c, "BEGIN").await;
        proto.ok(&c, "INSERT INTO aged VALUES (1)").await;
        tokio::time::sleep(Duration::from_millis(1300)).await;
        let age = e("54000", "transaction age limit of 1s exceeded");
        assert_eq!(proto.err(&c, "SELECT 1").await, age, "{proto:?}");
        assert_eq!(proto.err(&c, "SELECT 1").await, e("25P02", ABORTED));
        proto.ok(&c, "ROLLBACK").await;
        assert_eq!(
            value(&c, "SELECT count(*) FROM aged").await.as_deref(),
            Some("0")
        );
    }
}

#[tokio::test]
async fn commit_after_the_age_limit_rolls_back_and_leaves_the_session_idle() {
    let server = short_age_server();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE aged2 (id int PRIMARY KEY)").await;
    for proto in PROTOS {
        proto.ok(&c, "BEGIN").await;
        proto.ok(&c, "INSERT INTO aged2 VALUES (1)").await;
        tokio::time::sleep(Duration::from_millis(1300)).await;
        let age = e("54000", "transaction age limit of 1s exceeded");
        assert_eq!(proto.err(&c, "COMMIT").await, age, "{proto:?}");
        // Idle: a new statement works without ROLLBACK, and the write is gone.
        assert_eq!(
            value(&c, "SELECT count(*) FROM aged2").await.as_deref(),
            Some("0")
        );
        proto.ok(&c, "BEGIN").await;
        proto.ok(&c, "COMMIT").await;
    }
}

#[tokio::test]
async fn a_multi_statement_string_is_one_implicit_transaction() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE multi (id int PRIMARY KEY)").await;
    assert_eq!(
        simple_err(
            &c,
            "CREATE TABLE multi_a (id int); CREATE TABLE multi_b (id int)"
        )
        .await,
        e("0A000", TWO_DDL)
    );
    assert_eq!(
        simple_err(
            &c,
            "CREATE TABLE multi_c (id int); INSERT INTO multi_c VALUES (1)"
        )
        .await,
        e("0A000", DDL_DML)
    );
    assert_eq!(
        simple_err(
            &c,
            "INSERT INTO multi VALUES (1); CREATE INDEX multi_i ON multi (id)"
        )
        .await,
        e("0A000", DDL_DML)
    );
    // Nothing ran.
    for t in ["multi_a", "multi_b", "multi_c"] {
        assert_eq!(
            simple_err(&c, &format!("SELECT * FROM {t}")).await.code,
            "42P01"
        );
    }
    assert_eq!(
        value(&c, "SELECT count(*) FROM multi").await.as_deref(),
        Some("0")
    );
    // Allowed strings.
    c.simple_query("BEGIN; CREATE INDEX multi_ok ON multi (id); COMMIT")
        .await
        .unwrap();
    c.simple_query("SELECT 1; SELECT 2").await.unwrap();
    c.simple_query("INSERT INTO multi VALUES (1); INSERT INTO multi VALUES (2)")
        .await
        .unwrap();
    assert_eq!(
        value(&c, "SELECT count(*) FROM multi").await.as_deref(),
        Some("2")
    );
}
