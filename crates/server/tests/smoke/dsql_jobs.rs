//! DSQL jobs and the `sys` schema through a real client (spec US1): `CREATE INDEX ASYNC`,
//! `sys.jobs`, `CALL sys.wait_for_job`, `sys.dsql_major_version()`.

use crate::common::{rows, TestServer};

const JOB_COLUMNS: [&str; 9] = [
    "job_id",
    "status",
    "details",
    "job_type",
    "class_id",
    "object_id",
    "object_name",
    "start_time",
    "update_time",
];

#[tokio::test]
async fn sys_jobs_is_empty_with_the_nine_dsql_columns() {
    let server = TestServer::start();
    let c = server.connect().await;
    assert!(rows(&c, "SELECT * FROM sys.jobs").await.is_empty());
    let statement = c.prepare("SELECT * FROM sys.jobs").await.unwrap();
    let names: Vec<&str> = statement.columns().iter().map(|c| c.name()).collect();
    assert_eq!(names, JOB_COLUMNS);
}

use tokio_postgres::Client;

use crate::common::{simple_err, DbErr};
use crate::dsql_rules::{e, Proto, PROTOS};

/// Runs a statement that returns exactly one row of text columns, on either protocol.
async fn one_row(proto: Proto, c: &Client, sql: &str) -> Vec<Option<String>> {
    match proto {
        Proto::Simple => {
            let mut rows = rows(c, sql).await;
            assert_eq!(rows.len(), 1, "{sql}: {rows:?}");
            rows.remove(0)
        }
        Proto::Extended => {
            let rows = c
                .query(sql, &[])
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
            assert_eq!(rows.len(), 1, "{sql}");
            (0..rows[0].len())
                .map(|i| {
                    let ty = rows[0].columns()[i].type_().clone();
                    if ty == tokio_postgres::types::Type::BOOL {
                        rows[0]
                            .get::<_, Option<bool>>(i)
                            .map(|b| if b { "t" } else { "f" }.to_owned())
                    } else if ty == tokio_postgres::types::Type::INT4 {
                        rows[0].get::<_, Option<i32>>(i).map(|n| n.to_string())
                    } else {
                        rows[0].get::<_, Option<String>>(i)
                    }
                })
                .collect()
        }
    }
}

fn is_job_id(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
}

/// Runs a job statement and returns its `job_id`.
async fn job(proto: Proto, c: &Client, sql: &str) -> String {
    let row = one_row(proto, c, sql).await;
    assert_eq!(row.len(), 1, "{sql}");
    let id = row[0].clone().expect("job id");
    assert!(is_job_id(&id), "{sql}: {id}");
    id
}

async fn succeeded(proto: Proto, c: &Client, id: &str) -> String {
    one_row(proto, c, &format!("CALL sys.wait_for_job('{id}')"))
        .await
        .remove(0)
        .expect("succeeded")
}

async fn make_table(proto: Proto, c: &Client, name: &str) {
    proto
        .ok(
            c,
            &format!("CREATE TABLE {name} (id int PRIMARY KEY, v text, w int)"),
        )
        .await;
}

#[tokio::test]
async fn create_index_async_returns_a_job_and_the_index_exists() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        let t = format!("ix_{proto:?}").to_lowercase();
        make_table(proto, &c, &t).await;
        proto
            .ok(
                &c,
                &format!("INSERT INTO {t} VALUES (1, 'a', 1), (2, 'b', 2)"),
            )
            .await;
        let id = job(proto, &c, &format!("CREATE INDEX ASYNC {t}_v ON {t} (v)")).await;
        assert_eq!(succeeded(proto, &c, &id).await, "t", "{proto:?}");
        let job_row = one_row(
            proto,
            &c,
            &format!(
                "SELECT status, job_type, object_name, class_id::text, \
                 CASE WHEN start_time IS NOT NULL THEN 'set' END, \
                 CASE WHEN update_time IS NOT NULL THEN 'set' END, details \
                 FROM sys.jobs WHERE job_id = '{id}'"
            ),
        )
        .await;
        assert_eq!(
            job_row,
            vec![
                s("completed"),
                s("INDEX_BUILD"),
                Some(format!("public.{t}_v")),
                s("1259"),
                s("set"),
                s("set"),
                None
            ],
            "{proto:?}"
        );
        assert_eq!(
            value(
                &c,
                &format!("SELECT count(*) FROM pg_indexes WHERE indexname = '{t}_v'")
            )
            .await,
            s("1")
        );
        // The variants of the index definition.
        for (name, ddl) in [
            ("u", format!("CREATE UNIQUE INDEX ASYNC {t}_u ON {t} (v)")),
            ("n", format!("CREATE INDEX ASYNC ON {t} (w)")),
            (
                "p",
                format!("CREATE INDEX ASYNC {t}_p ON {t} (w) WHERE w > 1"),
            ),
            ("e", format!("CREATE INDEX ASYNC {t}_e ON {t} ((lower(v)))")),
            (
                "i",
                format!("CREATE INDEX ASYNC {t}_i ON {t} (w) INCLUDE (v)"),
            ),
            (
                "nd",
                format!("CREATE UNIQUE INDEX ASYNC {t}_nd ON {t} (v) NULLS NOT DISTINCT"),
            ),
            (
                "bt",
                format!("CREATE INDEX ASYNC {t}_bt ON {t} USING btree (w)"),
            ),
        ] {
            let id = job(proto, &c, &ddl).await;
            assert_eq!(succeeded(proto, &c, &id).await, "t", "{proto:?} {name}");
        }
    }
}

use crate::common::{s, value};

#[tokio::test]
async fn create_index_async_edge_cases() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        let t = format!("ie_{proto:?}").to_lowercase();
        make_table(proto, &c, &t).await;
        // IF NOT EXISTS on an existing index still answers with a job id.
        job(proto, &c, &format!("CREATE INDEX ASYNC {t}_i ON {t} (v)")).await;
        job(
            proto,
            &c,
            &format!("CREATE INDEX ASYNC IF NOT EXISTS {t}_i ON {t} (v)"),
        )
        .await;
        // A schema-qualified index name is a syntax error, as on DSQL.
        assert_eq!(
            proto
                .err(&c, &format!("CREATE INDEX ASYNC public.{t}_q ON {t} (v)"))
                .await,
            e("42601", "syntax error at or near \".\""),
            "{proto:?}"
        );
        // Without ASYNC the statement is refused.
        assert_eq!(
            proto
                .err(&c, &format!("CREATE INDEX {t}_s ON {t} (v)"))
                .await,
            e("0A000", "unsupported mode. please use CREATE INDEX ASYNC."),
            "{proto:?}"
        );
        // An engine error is returned as is and no job is recorded for it.
        let before = value(&c, "SELECT count(*) FROM sys.jobs").await;
        assert_eq!(
            proto
                .err(&c, "CREATE INDEX ASYNC nx ON no_such_table (v)")
                .await
                .code,
            "42P01",
            "{proto:?}"
        );
        assert_eq!(value(&c, "SELECT count(*) FROM sys.jobs").await, before);
    }
}

#[tokio::test]
async fn a_failed_unique_build_is_a_failed_job() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        let t = format!("uq_{proto:?}").to_lowercase();
        make_table(proto, &c, &t).await;
        proto
            .ok(
                &c,
                &format!("INSERT INTO {t} VALUES (1, 'dup', 1), (2, 'dup', 2)"),
            )
            .await;
        let id = job(
            proto,
            &c,
            &format!("CREATE UNIQUE INDEX ASYNC {t}_u ON {t} (v)"),
        )
        .await;
        assert_eq!(succeeded(proto, &c, &id).await, "f", "{proto:?}");
        let row = one_row(
            proto,
            &c,
            &format!(
                "SELECT status, job_type, object_name, details FROM sys.jobs WHERE job_id = '{id}'"
            ),
        )
        .await;
        assert_eq!(row[0], s("failed"));
        assert_eq!(row[1], s("INDEX_BUILD"));
        assert_eq!(row[2], Some(format!("public.{t}_u")));
        let details = row[3].clone().expect("details name the duplicate");
        assert!(details.contains("dup"), "{details}");
        // No index was left behind, so the duplicates are still insertable.
        assert_eq!(
            value(
                &c,
                &format!("SELECT count(*) FROM pg_indexes WHERE indexname = '{t}_u'")
            )
            .await,
            s("0")
        );
        proto
            .ok(&c, &format!("INSERT INTO {t} VALUES (3, 'dup', 3)"))
            .await;
    }
}

#[tokio::test]
async fn wait_for_job_and_major_version_forms() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        let t = format!("wf_{proto:?}").to_lowercase();
        make_table(proto, &c, &t).await;
        let id = job(proto, &c, &format!("CREATE INDEX ASYNC {t}_i ON {t} (v)")).await;
        assert_eq!(
            proto
                .err(&c, &format!("SELECT sys.wait_for_job('{id}')"))
                .await,
            e("42809", "sys.wait_for_job(text) is a procedure"),
            "{proto:?}"
        );
        assert_eq!(
            proto.err(&c, "CALL sys.wait_for_job('no-such-job')").await,
            e("22P02", "Unable to convert text to UUID"),
            "{proto:?}"
        );
        assert_eq!(
            proto
                .err(
                    &c,
                    "CALL sys.wait_for_job((SELECT job_id FROM sys.jobs LIMIT 1))"
                )
                .await,
            e("0A000", "cannot use subquery in CALL argument"),
            "{proto:?}"
        );
        let unknown = "a".repeat(26);
        assert_eq!(
            proto
                .err(&c, &format!("CALL sys.wait_for_job('{unknown}')"))
                .await,
            e("22023", &format!("job {unknown} not found")),
            "{proto:?}"
        );
        for sql in [
            "SELECT * FROM sys.dsql_major_version()",
            "SELECT sys.dsql_major_version()",
        ] {
            assert_eq!(
                one_row(proto, &c, sql).await,
                vec![s("1")],
                "{proto:?} {sql}"
            );
        }
        assert_eq!(
            one_row(proto, &c, "SELECT sys.dsql_major_version() AS v").await,
            vec![s("1")]
        );
    }
    let names = |sql: &'static str| {
        let c = &c;
        async move {
            c.prepare(sql)
                .await
                .unwrap()
                .columns()
                .iter()
                .map(|col| (col.name().to_owned(), col.type_().name().to_owned()))
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        names("SELECT sys.dsql_major_version()").await,
        vec![("dsql_major_version".to_owned(), "int4".to_owned())]
    );
    assert_eq!(
        names("SELECT sys.dsql_major_version() AS v").await,
        vec![("v".to_owned(), "int4".to_owned())]
    );
}

#[tokio::test]
async fn describe_and_parameters_of_job_statements() {
    let server = TestServer::start();
    let c = server.connect().await;
    make_table(Proto::Simple, &c, "dj").await;
    let statement = c
        .prepare("CREATE INDEX ASYNC dj_i ON dj (v)")
        .await
        .unwrap();
    let columns: Vec<_> = statement
        .columns()
        .iter()
        .map(|col| (col.name().to_owned(), col.type_().name().to_owned()))
        .collect();
    assert_eq!(columns, vec![("job_id".to_owned(), "text".to_owned())]);
    let rows = c.query(&statement, &[]).await.unwrap();
    let id: String = rows[0].get(0);
    assert!(is_job_id(&id));
    let wait = c.prepare("CALL sys.wait_for_job($1)").await.unwrap();
    let columns: Vec<_> = wait
        .columns()
        .iter()
        .map(|col| (col.name().to_owned(), col.type_().name().to_owned()))
        .collect();
    assert_eq!(columns, vec![("succeeded".to_owned(), "bool".to_owned())]);
    let rows = c.query(&wait, &[&id]).await.unwrap();
    assert!(rows[0].get::<_, bool>(0));
    let bad = c.query(&wait, &[&"short"]).await.unwrap_err();
    assert_eq!(
        DbErr::of(&bad),
        e("22P02", "Unable to convert text to UUID")
    );
}

#[tokio::test]
async fn jobs_in_transactions_and_query_strings() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        let t = format!("tx_{proto:?}").to_lowercase();
        make_table(proto, &c, &t).await;
        // Rolled back: neither the index nor the job survive.
        let before = value(&c, "SELECT count(*) FROM sys.jobs").await;
        proto.ok(&c, "BEGIN").await;
        job(proto, &c, &format!("CREATE INDEX ASYNC {t}_r ON {t} (v)")).await;
        proto.ok(&c, "ROLLBACK").await;
        assert_eq!(
            value(&c, "SELECT count(*) FROM sys.jobs").await,
            before,
            "{proto:?}"
        );
        assert_eq!(
            value(
                &c,
                &format!("SELECT count(*) FROM pg_indexes WHERE indexname = '{t}_r'")
            )
            .await,
            s("0"),
            "{proto:?}"
        );
        // Committed: both are there, and it was the transaction's one DDL.
        proto.ok(&c, "BEGIN").await;
        let id = job(proto, &c, &format!("CREATE INDEX ASYNC {t}_c ON {t} (v)")).await;
        assert_eq!(
            proto
                .err(&c, &format!("CREATE INDEX ASYNC {t}_c2 ON {t} (w)"))
                .await
                .code,
            "0A000"
        );
        proto.ok(&c, "ROLLBACK").await;
        proto.ok(&c, "BEGIN").await;
        let id2 = job(proto, &c, &format!("CREATE INDEX ASYNC {t}_c ON {t} (v)")).await;
        proto.ok(&c, "COMMIT").await;
        assert_ne!(id, id2);
        assert_eq!(
            value(
                &c,
                &format!("SELECT status FROM sys.jobs WHERE job_id = '{id2}'")
            )
            .await,
            s("completed"),
            "{proto:?}"
        );
    }
    // One query string: BEGIN; CREATE INDEX ASYNC; COMMIT gives the job row.
    make_table(Proto::Simple, &c, "qs").await;
    let messages = c
        .simple_query("BEGIN; CREATE INDEX ASYNC qs_i ON qs (v); COMMIT")
        .await
        .unwrap();
    let job_rows: Vec<_> = messages
        .iter()
        .filter_map(|m| match m {
            tokio_postgres::SimpleQueryMessage::Row(r) => r.get(0).map(str::to_owned),
            _ => None,
        })
        .collect();
    assert_eq!(job_rows.len(), 1);
    assert!(is_job_id(&job_rows[0]));
    // DDL followed by an async index in one string: two DDLs.
    assert_eq!(
        simple_err(
            &c,
            "CREATE TABLE qs2 (id int); CREATE INDEX ASYNC qs2_i ON qs2 (id)"
        )
        .await,
        e(
            "0A000",
            "multiple ddl statements not supported in a transaction"
        )
    );
}

#[tokio::test]
async fn two_sessions_get_distinct_job_ids_and_see_committed_jobs() {
    let server = TestServer::start();
    let a = server.connect().await;
    let b = server.connect().await;
    make_table(Proto::Simple, &a, "shared").await;
    let ida = job(Proto::Simple, &a, "CREATE INDEX ASYNC sh_a ON shared (v)").await;
    let idb = job(Proto::Extended, &b, "CREATE INDEX ASYNC sh_b ON shared (w)").await;
    assert_ne!(ida, idb);
    for id in [&ida, &idb] {
        for c in [&a, &b] {
            assert_eq!(
                value(
                    c,
                    &format!("SELECT status FROM sys.jobs WHERE job_id = '{id}'")
                )
                .await,
                s("completed")
            );
        }
        assert_eq!(succeeded(Proto::Simple, &b, id).await, "t");
    }
}

#[tokio::test]
async fn sys_is_read_only_for_clients() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        let denied_table = e("42501", "permission denied for table jobs");
        let denied_schema = e("42501", "permission denied for schema sys");
        for sql in [
            "INSERT INTO sys.jobs (job_id, status, job_type, class_id, object_id, start_time) \
             VALUES ('x', 'completed', 'INDEX_BUILD', 1, 1, now())",
            "UPDATE sys.jobs SET status = 'failed'",
            "DELETE FROM sys.jobs",
        ] {
            assert_eq!(proto.err(&c, sql).await, denied_table, "{proto:?}: {sql}");
        }
        // The edge case of the spec: CREATE SCHEMA sys is refused like the other DDL on sys
        // (rule catalogue `sys_ddl`), not answered with "already exists".
        for sql in [
            "CREATE SCHEMA sys",
            "CREATE SCHEMA IF NOT EXISTS sys",
            "DROP SCHEMA sys",
            "CREATE TABLE sys.mine (id int)",
            "DROP TABLE sys.jobs",
        ] {
            assert_eq!(proto.err(&c, sql).await, denied_schema, "{proto:?}: {sql}");
        }
        assert!(rows(&c, "SELECT * FROM sys.jobs LIMIT 1").await.len() <= 1);
    }
}
