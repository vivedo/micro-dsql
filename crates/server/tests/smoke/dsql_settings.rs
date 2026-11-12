//! The SET whitelist, remembered values for SHOW, and the numeric rewrite, through a real
//! client on both wire protocols.

use tokio_postgres::Client;

use crate::common::{rows, simple_err, value, DbErr, TestServer};
use crate::dsql_rules::{e, Proto, ABORTED, PROTOS};

/// The single text value of `SHOW <name>` on the given protocol.
async fn show(proto: Proto, c: &Client, name: &str) -> String {
    let sql = format!("SHOW {name}");
    match proto {
        Proto::Simple => value(c, &sql).await.expect("a value"),
        Proto::Extended => {
            let rows = c
                .query(sql.as_str(), &[])
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
            assert_eq!(rows.len(), 1, "{sql}");
            rows[0].get::<_, String>(0)
        }
    }
}

async fn show_column(proto: Proto, c: &Client, name: &str) -> String {
    let sql = format!("SHOW {name}");
    match proto {
        Proto::Simple => {
            let messages = c.simple_query(&sql).await.unwrap();
            for m in messages {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = m {
                    return row.columns()[0].name().to_owned();
                }
            }
            panic!("{sql}: no row");
        }
        Proto::Extended => c.query(sql.as_str(), &[]).await.unwrap()[0].columns()[0]
            .name()
            .to_owned(),
    }
}

fn not_supported(name: &str) -> DbErr {
    e(
        "0A000",
        &format!("setting configuration parameter \"{name}\" not supported"),
    )
}

#[tokio::test]
async fn parameters_off_the_whitelist_are_refused() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        for sql in [
            "SET work_mem = '64MB'",
            "SET SESSION work_mem = '64MB'",
            "SET LOCAL work_mem = '64MB'",
            "RESET work_mem",
            "SET statement_timeout = 1000",
        ] {
            let name = if sql.contains("statement_timeout") {
                "statement_timeout"
            } else {
                "work_mem"
            };
            assert_eq!(
                proto.err(&c, sql).await,
                not_supported(name),
                "{proto:?}: {sql}"
            );
        }
        // `role` is settable only with SET LOCAL.
        for sql in ["SET role = x", "SET SESSION role x", "RESET role"] {
            assert_eq!(
                proto.err(&c, sql).await,
                not_supported("role"),
                "{proto:?}: {sql}"
            );
        }
        // Inside a transaction a refusal aborts it.
        proto.ok(&c, "BEGIN").await;
        assert_eq!(
            proto.err(&c, "SET work_mem = '1MB'").await,
            not_supported("work_mem")
        );
        assert_eq!(proto.err(&c, "SELECT 1").await, e("25P02", ABORTED));
        proto.ok(&c, "ROLLBACK").await;
        // In an aborted block 25P02 wins.
        proto.ok(&c, "BEGIN").await;
        assert_eq!(proto.err(&c, "SELECT 1/0").await.code, "22012");
        assert_eq!(
            proto.err(&c, "SET work_mem = '1MB'").await,
            e("25P02", ABORTED)
        );
        proto.ok(&c, "ROLLBACK").await;
    }
}

#[tokio::test]
async fn set_config_of_other_parameters_is_refused() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        assert_eq!(
            proto
                .err(&c, "SELECT set_config('work_mem', '1MB', false)")
                .await,
            not_supported("work_mem"),
            "{proto:?}"
        );
        assert_eq!(
            proto
                .err(&c, "SELECT set_config('dsql.nope', '1', false)")
                .await,
            e(
                "42602",
                "invalid configuration parameter name \"dsql.nope\""
            )
            .with_detail("\"dsql\" is a reserved prefix."),
            "{proto:?}"
        );
    }
}

#[tokio::test]
async fn set_transaction_is_refused() {
    let server = TestServer::start();
    let c = server.connect().await;
    let msg = "setting configuration parameter \"TRANSACTION\" not supported";
    for proto in PROTOS {
        for sql in [
            "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "SET TRANSACTION READ ONLY",
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ",
        ] {
            assert_eq!(
                proto.err(&c, sql).await,
                e("0A000", msg),
                "{proto:?}: {sql}"
            );
            // Inside a transaction too (and it aborts the block).
            proto.ok(&c, "BEGIN").await;
            assert_eq!(proto.err(&c, sql).await, e("0A000", msg));
            assert_eq!(proto.err(&c, "SELECT 1").await.code, "25P02");
            proto.ok(&c, "ROLLBACK").await;
        }
    }
}

#[tokio::test]
async fn session_characteristics() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        for sql in [
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ WRITE",
        ] {
            proto.ok(&c, sql).await;
        }
        for (mode, level) in [
            ("ISOLATION LEVEL SERIALIZABLE", "SERIALIZABLE"),
            ("ISOLATION LEVEL READ COMMITTED", "READ COMMITTED"),
            ("ISOLATION LEVEL READ UNCOMMITTED", "READ UNCOMMITTED"),
        ] {
            assert_eq!(
                proto
                    .err(
                        &c,
                        &format!("SET SESSION CHARACTERISTICS AS TRANSACTION {mode}")
                    )
                    .await,
                e("0A000", &format!("Unsupported isolation level: {level}")),
                "{proto:?}: {mode}"
            );
        }
        for mode in ["READ ONLY", "DEFERRABLE", "NOT DEFERRABLE"] {
            assert_eq!(
                proto
                    .err(
                        &c,
                        &format!("SET SESSION CHARACTERISTICS AS TRANSACTION {mode}")
                    )
                    .await,
                e(
                    "0A000",
                    "setting configuration parameter \"TRANSACTION\" not supported"
                ),
                "{proto:?}: {mode}"
            );
        }
    }
}

#[tokio::test]
async fn unknown_dsql_names_are_reserved() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        for sql in [
            "SET dsql.nope = 1",
            "RESET dsql.nope",
            "SET LOCAL dsql.nope = 'x'",
        ] {
            assert_eq!(
                proto.err(&c, sql).await,
                e(
                    "42602",
                    "invalid configuration parameter name \"dsql.nope\""
                )
                .with_detail("\"dsql\" is a reserved prefix."),
                "{proto:?}: {sql}"
            );
        }
        // The one DSQL parameter that exists is fine.
        proto.ok(&c, "SET dsql.enable_batched_nestloop = off").await;
        proto.ok(&c, "RESET dsql.enable_batched_nestloop").await;
    }
}

#[tokio::test]
async fn booleans_and_value_lists_are_validated() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        assert_eq!(
            proto.err(&c, "SET enable_seqscan = banana").await,
            e(
                "22023",
                "parameter \"enable_seqscan\" requires a Boolean value"
            ),
            "{proto:?}"
        );
        assert_eq!(
            proto
                .err(&c, "SET dsql.enable_batched_nestloop = 'maybe'")
                .await,
            e(
                "22023",
                "parameter \"dsql.enable_batched_nestloop\" requires a Boolean value"
            )
        );
        for name in [
            "enable_seqscan",
            "dsql.enable_batched_nestloop",
            "extra_float_digits",
            "intervalstyle",
            "application_name",
            "client_encoding",
        ] {
            assert_eq!(
                proto.err(&c, &format!("SET {name} = 'on', 'off'")).await,
                e("22023", &format!("SET {name} takes only one argument")),
                "{proto:?}: {name}"
            );
        }
        // Lists are fine for search_path and datestyle.
        proto.ok(&c, "SET search_path = public, pg_catalog").await;
        proto.ok(&c, "SET datestyle = ISO, MDY").await;
    }
}

#[tokio::test]
async fn remembered_values_are_shown_and_reset() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        // DSQL's defaults and PostgreSQL's column names.
        for (name, column, default) in [
            ("DateStyle", "DateStyle", "ISO, MDY"),
            ("IntervalStyle", "IntervalStyle", "postgres"),
            ("extra_float_digits", "extra_float_digits", "1"),
            ("enable_seqscan", "enable_seqscan", "on"),
            ("enable_hashjoin", "enable_hashjoin", "on"),
            (
                "dsql.enable_batched_nestloop",
                "dsql.enable_batched_nestloop",
                "on",
            ),
        ] {
            assert_eq!(show(proto, &c, name).await, default, "{proto:?}: {name}");
            assert_eq!(
                show_column(proto, &c, name).await,
                column,
                "{proto:?}: {name}"
            );
        }
        proto.ok(&c, "SET enable_seqscan = off").await;
        proto.ok(&c, "SET SESSION enable_hashjoin TO false").await;
        proto.ok(&c, "SET dsql.enable_batched_nestloop = off").await;
        proto.ok(&c, "SET extra_float_digits = 3").await;
        proto.ok(&c, "SET datestyle = ISO, DMY").await;
        proto.ok(&c, "SET intervalstyle = 'sql_standard'").await;
        assert_eq!(show(proto, &c, "enable_seqscan").await, "off");
        assert_eq!(show(proto, &c, "enable_hashjoin").await, "off");
        assert_eq!(show(proto, &c, "dsql.enable_batched_nestloop").await, "off");
        assert_eq!(show(proto, &c, "extra_float_digits").await, "3");
        assert_eq!(show(proto, &c, "datestyle").await, "ISO, DMY");
        assert_eq!(show(proto, &c, "intervalstyle").await, "sql_standard");
        // RESET and TO DEFAULT restore the default.
        proto.ok(&c, "RESET enable_seqscan").await;
        proto.ok(&c, "SET enable_hashjoin TO DEFAULT").await;
        assert_eq!(show(proto, &c, "enable_seqscan").await, "on");
        assert_eq!(show(proto, &c, "enable_hashjoin").await, "on");
        assert_eq!(show(proto, &c, "extra_float_digits").await, "3");
        // RESET ALL clears every remembered value (and reaches the engine, which accepts it).
        proto.ok(&c, "RESET ALL").await;
        assert_eq!(show(proto, &c, "dsql.enable_batched_nestloop").await, "on");
        assert_eq!(show(proto, &c, "extra_float_digits").await, "1");
        assert_eq!(show(proto, &c, "datestyle").await, "ISO, MDY");
        assert_eq!(show(proto, &c, "intervalstyle").await, "postgres");
    }
}

#[tokio::test]
async fn settings_are_per_connection() {
    let server = TestServer::start();
    let a = server.connect().await;
    let b = server.connect().await;
    rows(&a, "SET enable_seqscan = off").await;
    assert_eq!(show(Proto::Simple, &a, "enable_seqscan").await, "off");
    assert_eq!(show(Proto::Simple, &b, "enable_seqscan").await, "on");
}

#[tokio::test]
async fn set_local_ends_with_the_transaction() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        for end in ["COMMIT", "ROLLBACK"] {
            proto.ok(&c, "BEGIN").await;
            proto.ok(&c, "SET LOCAL enable_seqscan = off").await;
            assert_eq!(show(proto, &c, "enable_seqscan").await, "off");
            proto.ok(&c, end).await;
            assert_eq!(
                show(proto, &c, "enable_seqscan").await,
                "on",
                "{proto:?} {end}"
            );
        }
        // Outside a transaction SET LOCAL has no effect.
        proto.ok(&c, "SET LOCAL enable_seqscan = off").await;
        assert_eq!(show(proto, &c, "enable_seqscan").await, "on");
        // SET LOCAL role is accepted inside a transaction and shown until it ends.
        proto.ok(&c, "BEGIN").await;
        assert_eq!(show(proto, &c, "role").await, "none");
        proto.ok(&c, "SET LOCAL role app").await;
        assert_eq!(show(proto, &c, "role").await, "app");
        proto.ok(&c, "COMMIT").await;
        assert_eq!(show(proto, &c, "role").await, "none");
    }
}

#[tokio::test]
async fn forwarded_settings_still_reach_the_engine() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        proto.ok(&c, "SET search_path = public").await;
        assert_eq!(show(proto, &c, "search_path").await, "public", "{proto:?}");
        proto.ok(&c, "SET application_name = 'dsql-test'").await;
        assert_eq!(
            show(proto, &c, "application_name").await,
            "dsql-test",
            "{proto:?}"
        );
        proto.ok(&c, "RESET search_path").await;
        // timezone keeps today's behaviour.
        proto.ok(&c, "SET timezone = 'UTC'").await;
        assert_eq!(
            proto.err(&c, "SET timezone = 'Europe/Rome'").await.code,
            "22023"
        );
    }
}

#[tokio::test]
async fn show_of_locale_parameters_is_unrecognized() {
    let server = TestServer::start();
    let c = server.connect().await;
    for proto in PROTOS {
        for name in ["lc_collate", "lc_ctype"] {
            assert_eq!(
                proto.err(&c, &format!("SHOW {name}")).await,
                e(
                    "42704",
                    &format!("unrecognized configuration parameter \"{name}\"")
                ),
                "{proto:?}"
            );
        }
        // Other SHOW answers are unchanged.
        assert!(show(proto, &c, "server_version").await.starts_with("16."));
        assert_eq!(show(proto, &c, "TimeZone").await, "UTC");
    }
    assert_eq!(simple_err(&c, "SHOW lc_collate").await.code, "42704");
}
