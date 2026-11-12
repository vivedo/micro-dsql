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
            &format!("CREATE INDEX ASYNC {t}_i ON mix (id)"),
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
        ..ServerOptions::default()
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
            "INSERT INTO multi VALUES (1); CREATE INDEX ASYNC multi_i ON multi (id)"
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
    c.simple_query("BEGIN; CREATE INDEX ASYNC multi_ok ON multi (id); COMMIT")
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

// ---------------------------------------------------------------------------------------------
// Statement deny rules (contracts/rule-catalogue.md section 3)
// ---------------------------------------------------------------------------------------------

/// (statement, SQLSTATE, message): every deny rule once.
const DENIED: &[(&str, &str, &str)] = &[
    // 3.1: statements the engine accepts today
    (
        "TRUNCATE rules_t",
        "0A000",
        "unsupported statement: Truncate",
    ),
    (
        "CREATE UNLOGGED TABLE rules_u (id int)",
        "0A000",
        "UNLOGGED table not supported for CREATE TABLE",
    ),
    (
        "CREATE TABLE rules_ctas AS SELECT 1 AS a",
        "0A000",
        "unsupported statement: CreateTableAs",
    ),
    (
        "SELECT * INTO rules_into FROM rules_t",
        "0A000",
        "unsupported statement: CreateTableAs",
    ),
    (
        "CREATE TYPE mood AS ENUM ('a', 'b')",
        "0A000",
        "CREATE TYPE not supported",
    ),
    (
        "ALTER TYPE mood ADD VALUE 'c'",
        "0A000",
        "ALTER TYPE not supported",
    ),
    (
        "DROP TYPE mood",
        "0A000",
        "unsupported object in DROP statement",
    ),
    (
        "ALTER TYPE mood RENAME TO mood2",
        "0A000",
        "unsupported object in RENAME statement",
    ),
    ("COPY rules_t TO STDOUT", "0A000", "COPY is not supported"),
    ("COPY rules_t FROM STDIN", "0A000", "COPY is not supported"),
    (
        "CREATE TABLE rules_s (id serial PRIMARY KEY)",
        "42704",
        "type \"serial\" does not exist",
    ),
    (
        "CREATE TABLE rules_s (id bigserial)",
        "42704",
        "type \"bigserial\" does not exist",
    ),
    (
        "CREATE TABLE rules_s (id smallserial)",
        "42704",
        "type \"smallserial\" does not exist",
    ),
    (
        "ALTER TABLE rules_t ADD COLUMN s serial",
        "42704",
        "type \"serial\" does not exist",
    ),
    (
        "CREATE TABLE rules_m (c money)",
        "0A000",
        "datatype money not supported",
    ),
    (
        "CREATE TABLE rules_m (c xml)",
        "0A000",
        "datatype xml not supported",
    ),
    (
        "CREATE TABLE rules_m (c inet)",
        "0A000",
        "datatype inet not supported",
    ),
    (
        "CREATE TABLE rules_m (c cidr)",
        "0A000",
        "datatype cidr not supported",
    ),
    (
        "CREATE TABLE rules_m (c point)",
        "0A000",
        "datatype point not supported",
    ),
    (
        "CREATE TABLE rules_m (c tsvector)",
        "0A000",
        "datatype tsvector not supported",
    ),
    (
        "CREATE TABLE rules_m (c int4range)",
        "0A000",
        "datatype int4range not supported",
    ),
    (
        "CREATE TABLE rules_m (c daterange)",
        "0A000",
        "datatype daterange not supported",
    ),
    (
        "CREATE TABLE rules_m (c bit(8))",
        "0A000",
        "datatype bit not supported",
    ),
    (
        "CREATE TABLE rules_m (c varbit(8))",
        "0A000",
        "datatype bit varying not supported",
    ),
    (
        "CREATE TABLE rules_m (c pg_lsn)",
        "0A000",
        "datatype pg_lsn not supported",
    ),
    (
        "ALTER TABLE rules_t ADD COLUMN m money",
        "0A000",
        "datatype money not supported",
    ),
    (
        "CREATE TABLE rules_a (c int[])",
        "0A000",
        "datatype integer[] not supported",
    ),
    (
        "CREATE TABLE rules_a (c text[])",
        "0A000",
        "datatype text[] not supported",
    ),
    (
        "ALTER TABLE rules_t ADD COLUMN a int[]",
        "0A000",
        "datatype integer[] not supported",
    ),
    // 3.2: statements the engine refuses with another SQLSTATE or text
    (
        "CREATE TEMPORARY TABLE rules_tmp (id int)",
        "0A000",
        "TEMPORARY or TEMP table not supported for CREATE TABLE",
    ),
    (
        "CREATE TEMP TABLE rules_tmp (id int)",
        "0A000",
        "TEMPORARY or TEMP table not supported for CREATE TABLE",
    ),
    (
        "CREATE EXTENSION pgcrypto",
        "0A000",
        "unsupported statement: CreateExtension",
    ),
    (
        "CREATE TRIGGER tr BEFORE INSERT ON rules_t FOR EACH ROW EXECUTE FUNCTION f()",
        "0A000",
        "CREATE TRIGGER not supported",
    ),
    (
        "CREATE DATABASE other",
        "0A000",
        "unsupported statement: Createdb",
    ),
    (
        "CREATE MATERIALIZED VIEW rules_mv AS SELECT 1",
        "0A000",
        "unsupported statement: CreateTableAs",
    ),
    (
        "CREATE TYPE comp AS (a int)",
        "0A000",
        "unsupported statement: CompositeType",
    ),
    (
        "CREATE TYPE rng AS RANGE (subtype = int)",
        "0A000",
        "unsupported statement: CreateRange",
    ),
    (
        "CREATE TABLESPACE ts LOCATION '/tmp/x'",
        "0A000",
        "unsupported statement: CreateTableSpace",
    ),
    (
        "CREATE FOREIGN TABLE rules_f (id int) SERVER s",
        "0A000",
        "unsupported statement: CreateForeignTable",
    ),
    ("VACUUM", "0A000", "unsupported VACUUM statement"),
    ("VACUUM rules_t", "0A000", "unsupported VACUUM statement"),
    ("LISTEN ch", "0A000", "unsupported statement: Listen"),
    ("NOTIFY ch", "0A000", "unsupported statement: Notify"),
    ("UNLISTEN ch", "0A000", "unsupported statement: Unlisten"),
    (
        "ALTER SYSTEM SET work_mem = '1MB'",
        "0A000",
        "unsupported statement: AlterSystem",
    ),
    (
        "MERGE INTO rules_t USING rules_t s ON true WHEN MATCHED THEN DELETE",
        "0A000",
        "unsupported statement type: MERGE",
    ),
    (
        "CREATE FUNCTION rf() RETURNS int LANGUAGE plpgsql AS 'begin return 1; end'",
        "0A000",
        "CREATE FUNCTION with language plpgsql not supported",
    ),
    (
        "SELECT * FROM rules_t FOR SHARE",
        "0A000",
        "locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported",
    ),
    (
        "SELECT * FROM rules_t FOR NO KEY UPDATE",
        "0A000",
        "locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported",
    ),
    (
        "SELECT * FROM rules_t TABLESAMPLE BERNOULLI (50)",
        "0A000",
        "TABLESAMPLE clause not supported",
    ),
    ("LOCK TABLE rules_t", "0A000", "LOCK TABLE is not supported"),
    ("DO $$ begin end $$", "0A000", "DO is not supported"),
    (
        "CREATE PROCEDURE rp() LANGUAGE sql AS 'select 1'",
        "0A000",
        "CREATE PROCEDURE is not supported",
    ),
    (
        "CREATE RULE rr AS ON INSERT TO rules_t DO NOTHING",
        "0A000",
        "CREATE RULE is not supported",
    ),
    (
        "CREATE PUBLICATION pub FOR ALL TABLES",
        "0A000",
        "CREATE PUBLICATION is not supported",
    ),
    ("REINDEX TABLE rules_t", "0A000", "REINDEX is not supported"),
    ("CLUSTER rules_t", "0A000", "CLUSTER is not supported"),
    (
        "PREPARE TRANSACTION 'x'",
        "0A000",
        "prepared transactions is not supported",
    ),
    (
        "SELECT pg_advisory_lock(1)",
        "0A000",
        "advisory locks are not supported",
    ),
    (
        "SELECT pg_try_advisory_xact_lock(1)",
        "0A000",
        "advisory locks are not supported",
    ),
];

async fn rules_server() -> (TestServer, Client) {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE rules_t (id int PRIMARY KEY, v int)").await;
    rows(&c, "INSERT INTO rules_t VALUES (1, 10), (2, 20)").await;
    (server, c)
}

#[tokio::test]
async fn every_deny_rule_refuses_with_its_sqlstate_and_text() {
    let (_server, c) = rules_server().await;
    for proto in PROTOS {
        for (sql, code, message) in DENIED {
            assert_eq!(
                proto.err(&c, sql).await,
                e(code, message),
                "{proto:?}: {sql}"
            );
            // Outside a transaction a refusal has no lasting effect: the session still works.
            proto.ok(&c, "SELECT 1").await;
        }
    }
}

#[tokio::test]
async fn a_refusal_inside_a_transaction_aborts_it() {
    let (_server, c) = rules_server().await;
    for proto in PROTOS {
        for (sql, code, message) in [
            DENIED[0], DENIED[1], DENIED[9], DENIED[10], DENIED[30], DENIED[51],
        ] {
            refused_in_txn(proto, &c, &["SELECT 1"], sql, e(code, message)).await;
        }
    }
}

#[tokio::test]
async fn a_refused_statement_has_no_effect() {
    let (_server, c) = rules_server().await;
    for proto in PROTOS {
        proto.err(&c, "TRUNCATE rules_t").await;
        assert_eq!(
            value(&c, "SELECT count(*) FROM rules_t").await.as_deref(),
            Some("2")
        );
        proto.err(&c, "CREATE TABLE rules_gone (id serial)").await;
        proto.err(&c, "CREATE TABLE rules_gone (c money)").await;
        proto.err(&c, "CREATE TEMP TABLE rules_gone (id int)").await;
        assert_eq!(
            simple_err(&c, "SELECT * FROM rules_gone").await.code,
            "42P01"
        );
        proto
            .err(&c, "ALTER TABLE rules_t ADD COLUMN m money")
            .await;
        proto
            .err(&c, "ALTER TABLE rules_t ADD COLUMN a int[]")
            .await;
        proto
            .err(&c, "ALTER TABLE rules_t ADD COLUMN s serial")
            .await;
        assert_eq!(simple_err(&c, "SELECT m FROM rules_t").await.code, "42703");
        assert_eq!(simple_err(&c, "SELECT a FROM rules_t").await.code, "42703");
        assert_eq!(simple_err(&c, "SELECT s FROM rules_t").await.code, "42703");
    }
}

#[tokio::test]
async fn an_aborted_transaction_answers_25p02_before_any_rule() {
    let (_server, c) = rules_server().await;
    for proto in PROTOS {
        proto.ok(&c, "BEGIN").await;
        assert_eq!(proto.err(&c, "SELECT 1/0").await.code, "22012");
        for sql in [
            "TRUNCATE rules_t",
            "SAVEPOINT s",
            "CREATE TABLE x (id serial)",
            "COPY rules_t FROM STDIN",
        ] {
            assert_eq!(
                proto.err(&c, sql).await,
                e("25P02", ABORTED),
                "{proto:?}: {sql}"
            );
        }
        proto.ok(&c, "ROLLBACK").await;
    }
}

#[tokio::test]
async fn a_statement_that_does_not_parse_is_not_refused_by_a_rule() {
    let (_server, c) = rules_server().await;
    for proto in PROTOS {
        let err = proto.err(&c, "TRUNCATE TABLE").await;
        assert_eq!(err.code, "42601", "{proto:?}: {err:?}");
    }
}

#[tokio::test]
async fn copy_is_refused_before_it_enters_copy_mode() {
    let (_server, c) = rules_server().await;
    for sql in [
        "COPY rules_t TO STDOUT",
        "COPY rules_t FROM STDIN",
        "COPY (SELECT 1) TO STDOUT",
    ] {
        assert_eq!(
            simple_err(&c, sql).await,
            e("0A000", "COPY is not supported"),
            "{sql}"
        );
        // The connection is still in normal mode.
        assert_eq!(
            value(&c, "SELECT count(*) FROM rules_t").await.as_deref(),
            Some("2")
        );
    }
}

/// FR-024: statements DSQL supports must not be refused.
#[tokio::test]
async fn supported_statements_are_not_refused() {
    let (_server, c) = rules_server().await;
    for proto in PROTOS {
        let n = format!("{proto:?}").to_lowercase();
        for sql in [
            format!("CREATE DOMAIN dom_{n} AS int"),
            format!("CREATE INDEX ASYNC idx_{n} ON rules_t (v)"),
            format!("CREATE UNIQUE INDEX ASYNC uidx_{n} ON rules_t (id, v)"),
            "SELECT ARRAY[1, 2]".to_owned(),
            "SELECT '10.0.0.1'::inet".to_owned(),
            format!("CREATE TABLE sup_{n} (id int PRIMARY KEY, n numeric(10,2), j jsonb, u uuid, tz timetz, i interval, b bytea, t timestamptz, txt text)"),
            format!("ALTER TABLE sup_{n} ADD COLUMN extra int"),
            format!("CREATE VIEW view_{n} AS SELECT 1 AS a"),
            format!("CREATE SEQUENCE seq_{n} CACHE 1"),
        ] {
            if let Err(err) = proto.run(&c, &sql).await {
                // The engine may have its own opinion; a rule of ours must not.
                panic!("{proto:?}: {sql}: refused: {err:?}");
            }
        }
    }
}

/// DSQL supports these, the engine does not (its own error, recorded in docs/known-gaps.md);
/// the policy must not refuse them, so the assertion is that no rule of ours fired.
#[tokio::test]
async fn statements_the_engine_limits_are_not_refused_by_a_rule() {
    let (_server, c) = rules_server().await;
    let ours = |message: &str| {
        DENIED.iter().any(|(_, _, m)| *m == message)
            || message.starts_with("CREATE FUNCTION with language")
    };
    for proto in PROTOS {
        for sql in [
            "SELECT * FROM rules_t FOR UPDATE",
            "SELECT * FROM rules_t FOR KEY SHARE",
            "ANALYZE rules_t",
            "CREATE FUNCTION fn_sql() RETURNS int LANGUAGE sql AS 'select 1'",
        ] {
            if let Err(err) = proto.run(&c, sql).await {
                assert!(!ours(&err.message), "{proto:?}: {sql}: {err:?}");
            }
        }
    }
}

#[tokio::test]
async fn rollback_to_a_savepoint_in_the_aborted_block_reads_like_postgres() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        proto.ok(&c, "BEGIN").await;
        proto.err(&c, "SAVEPOINT sp1").await;
        // The block is aborted: the engine answers, with PostgreSQL's text.
        assert_eq!(
            proto.err(&c, "ROLLBACK TO SAVEPOINT sp1").await,
            e("3B001", "savepoint \"sp1\" does not exist"),
            "{proto:?}"
        );
        proto.ok(&c, "ROLLBACK").await;
    }
}
