//! The T1 layer-1 psql scenarios (`tests/psql/*.sql`) replayed through tokio-postgres, over
//! the simple query protocol (what psql uses) and the extended protocol (what most drivers
//! use). psql meta-commands are skipped; the expected results are asserted per statement.
//! `40_conflict.sh` is a shell scenario; its session ordering is replayed directly.

use tokio_postgres::types::Type;
use tokio_postgres::{Client, Row};

use crate::common::TestServer;

#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    /// Rows as text: sent as text (simple protocol) or decoded from the column's declared
    /// type (extended protocol), so both protocols are asserted with the same values.
    Rows(Vec<Vec<Option<String>>>),
    Error(String),
}

/// One extended-protocol value as the text psql would print, decoded by its declared type.
fn cell(row: &Row, i: usize) -> Option<String> {
    let ty = row.columns()[i].type_();
    match *ty {
        Type::BOOL => row
            .get::<_, Option<bool>>(i)
            .map(|b| if b { "t" } else { "f" }.to_owned()),
        Type::INT2 => row.get::<_, Option<i16>>(i).map(|v| v.to_string()),
        Type::INT4 => row.get::<_, Option<i32>>(i).map(|v| v.to_string()),
        Type::INT8 => row.get::<_, Option<i64>>(i).map(|v| v.to_string()),
        Type::TEXT | Type::VARCHAR | Type::NAME | Type::BPCHAR => row.get::<_, Option<String>>(i),
        _ => panic!("column {i}: no text decoding for type {ty}"),
    }
}

/// The SQL statements of a scenario file, without psql meta-commands, comments and
/// statements that use psql variables.
fn statements(file: &str) -> Vec<String> {
    let path = format!("{}/../../tests/psql/{file}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        // `SHOW server_version \gset`: a meta-command ends the statement on its line.
        let (line, meta) = match line.split_once(" \\") {
            Some((sql, _)) => (sql, true),
            None => (line, false),
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("--") || trimmed.starts_with('\\') {
            continue;
        }
        current.push_str(line);
        current.push('\n');
        if meta || trimmed.ends_with(';') {
            let stmt = current.trim().trim_end_matches(';').to_owned();
            current.clear();
            // Statements using psql variables (`:'server_version'`) cannot run as SQL.
            if !stmt.contains(":'") {
                out.push(stmt);
            }
        }
    }
    out
}

async fn run(client: &Client, file: &str, extended: bool) -> Vec<(String, Outcome)> {
    let mut results = Vec::new();
    for sql in statements(file) {
        let outcome = if extended {
            match client.query(sql.as_str(), &[]).await {
                Ok(rows) => Outcome::Rows(
                    rows.iter()
                        .map(|row| (0..row.len()).map(|i| cell(row, i)).collect())
                        .collect(),
                ),
                Err(e) => Outcome::Error(e.code().map(|c| c.code().to_owned()).unwrap_or_default()),
            }
        } else {
            match crate::common::simple(client, &sql).await {
                Ok(rows) => Outcome::Rows(rows),
                Err(e) => Outcome::Error(e.code().map(|c| c.code().to_owned()).unwrap_or_default()),
            }
        };
        results.push((sql, outcome));
    }
    results
}

fn s(v: &str) -> Option<String> {
    Some(v.to_owned())
}

fn find<'a>(results: &'a [(String, Outcome)], prefix: &str) -> Vec<&'a Outcome> {
    results
        .iter()
        .filter(|(sql, _)| sql.starts_with(prefix))
        .map(|(_, o)| o)
        .collect()
}

#[tokio::test]
async fn scenario_00_connect() {
    let server = TestServer::start();
    for extended in [false, true] {
        let c = server.connect().await;
        let r = run(&c, "00_connect.sql", extended).await;
        assert_eq!(
            find(&r, "SELECT 1 AS one")[0],
            &Outcome::Rows(vec![vec![s("1")]]),
            "extended={extended}"
        );
        assert_eq!(
            find(&r, "SELECT length(current_user)")[0],
            &Outcome::Rows(vec![vec![s("t")]]),
            "extended={extended}"
        );
        assert_eq!(
            find(&r, "SELECT current_database()")[0],
            &Outcome::Rows(vec![vec![s("postgres")]]),
            "extended={extended}"
        );
        match find(&r, "SHOW server_version")[0] {
            Outcome::Rows(rows) => assert!(rows[0][0].as_deref().unwrap().starts_with("16.")),
            other => panic!("extended={extended}: {other:?}"),
        }
        assert_eq!(
            find(&r, "SHOW timezone")[0],
            &Outcome::Rows(vec![vec![s("UTC")]]),
            "extended={extended}"
        );
    }
}

#[tokio::test]
async fn scenario_10_crud() {
    let server = TestServer::start();
    for extended in [false, true] {
        let c = server.connect().await;
        let r = run(&c, "10_crud.sql", extended).await;
        for (sql, outcome) in &r {
            assert!(
                !matches!(outcome, Outcome::Error(_)),
                "extended={extended}: {sql}: {outcome:?}"
            );
        }
        let rows = |v: &[&[&str]]| {
            Outcome::Rows(
                v.iter()
                    .map(|row| row.iter().map(|c| s(c)).collect())
                    .collect(),
            )
        };
        let check = |prefix: &str, expected: Outcome| {
            assert_eq!(
                find(&r, prefix)[0],
                &expected,
                "extended={extended}: {prefix}"
            );
        };
        check(
            "INSERT INTO t1_crud",
            rows(&[
                &["1", "apple", "10"],
                &["2", "banana", "20"],
                &["3", "cherry", "30"],
            ]),
        );
        check(
            "SELECT id, name, qty FROM t1_crud WHERE",
            rows(&[&["3", "cherry", "30"], &["2", "banana", "20"]]),
        );
        check("UPDATE t1_crud", rows(&[&["2", "21"]]));
        check(
            "SELECT id, name, qty FROM t1_crud ORDER BY id",
            rows(&[
                &["1", "apple", "10"],
                &["2", "banana", "21"],
                &["3", "cherry", "30"],
            ]),
        );
        check("DELETE FROM t1_crud", rows(&[&["1"]]));
        check("SELECT count(*)", rows(&[&["2"]]));
    }
}

#[tokio::test]
async fn scenario_20_txn() {
    let server = TestServer::start();
    for extended in [false, true] {
        let c = server.connect().await;
        let r = run(&c, "20_txn.sql", extended).await;
        let errors: Vec<&str> = r
            .iter()
            .filter_map(|(_, o)| match o {
                Outcome::Error(code) => Some(code.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(errors, ["22012", "25P02", "25P02"], "extended={extended}");
        assert_eq!(
            find(&r, "SELECT count(*) AS inside_txn")[0],
            &Outcome::Rows(vec![vec![s("2")]]),
            "extended={extended}"
        );
        let selects = find(&r, "SELECT id, v FROM t1_txn");
        assert_eq!(selects.len(), 2);
        for select in selects {
            assert_eq!(
                select,
                &Outcome::Rows(vec![vec![s("1"), s("committed")]]),
                "extended={extended}"
            );
        }
        assert_eq!(
            find(&r, "SELECT count(*) AS rr_rows")[0],
            &Outcome::Rows(vec![vec![s("1")]]),
            "extended={extended}"
        );
    }
}

#[tokio::test]
async fn scenario_30_errors() {
    let server = TestServer::start();
    for extended in [false, true] {
        let c = server.connect().await;
        let r = run(&c, "30_errors.sql", extended).await;
        let errors: Vec<&str> = r
            .iter()
            .filter_map(|(_, o)| match o {
                Outcome::Error(code) => Some(code.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(errors, ["23505", "42P01", "42601"], "extended={extended}");
        assert_eq!(
            find(&r, "SELECT id, v FROM t1_errors")[0],
            &Outcome::Rows(vec![vec![s("1"), s("first")]]),
            "extended={extended}"
        );
    }
}

/// Runs one statement over the chosen protocol; `Err` carries the SQLSTATE.
async fn exec(client: &Client, sql: &str, extended: bool) -> Result<(), String> {
    let result = if extended {
        client.execute(sql, &[]).await.map(|_| ())
    } else {
        crate::common::simple(client, sql).await.map(|_| ())
    };
    result.map_err(|e| e.code().map(|c| c.code().to_owned()).unwrap_or_default())
}

/// `40_conflict.sh`: the same session ordering as the psql scenario (B takes its snapshot,
/// A updates and commits, B updates and commits), over both protocols. One session must
/// get 40001 and the first committer's value must be kept.
#[tokio::test]
async fn scenario_40_conflict() {
    let server = TestServer::start();
    for extended in [false, true] {
        let setup = server.connect().await;
        crate::common::rows(&setup, "DROP TABLE IF EXISTS t1_conflict").await;
        crate::common::rows(
            &setup,
            "CREATE TABLE t1_conflict (id int PRIMARY KEY, v text)",
        )
        .await;
        crate::common::rows(&setup, "INSERT INTO t1_conflict VALUES (1, 'initial')").await;

        let a = server.connect().await;
        let b = server.connect().await;
        let mut errors = Vec::new();
        for (session, sql) in [
            (&b, "BEGIN ISOLATION LEVEL REPEATABLE READ"),
            (&b, "SELECT v FROM t1_conflict WHERE id = 1"),
            (&a, "BEGIN ISOLATION LEVEL REPEATABLE READ"),
            (&a, "UPDATE t1_conflict SET v = 'from A' WHERE id = 1"),
            (&a, "COMMIT"),
            (&b, "UPDATE t1_conflict SET v = 'from B' WHERE id = 1"),
            (&b, "COMMIT"),
        ] {
            if let Err(code) = exec(session, sql, extended).await {
                errors.push((std::ptr::eq(session, &a), sql, code));
            }
        }
        assert!(
            errors
                .iter()
                .all(|(is_a, _, code)| !is_a && code == "40001"),
            "extended={extended}: {errors:?}"
        );
        assert!(!errors.is_empty(), "extended={extended}: no 40001 reported");
        assert_eq!(
            crate::common::value(&setup, "SELECT v FROM t1_conflict WHERE id = 1").await,
            s("from A"),
            "extended={extended}"
        );
        let _ = b.simple_query("ROLLBACK").await;
        crate::common::rows(&setup, "DROP TABLE t1_conflict").await;
    }
}
