//! The T1 layer-1 psql scenarios (`tests/psql/*.sql`) replayed through tokio-postgres, over
//! the simple query protocol (what psql uses) and the extended protocol (what most drivers
//! use). psql meta-commands are skipped; the expected results are asserted per statement.

use tokio_postgres::Client;

use crate::common::TestServer;

#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    /// Rows as text (simple protocol) or only the row count (extended protocol).
    Rows(Vec<Vec<Option<String>>>),
    RowCount(usize),
    Error(String),
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
                Ok(rows) => Outcome::RowCount(rows.len()),
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
    let c = server.connect().await;
    let r = run(&c, "00_connect.sql", false).await;
    assert_eq!(
        find(&r, "SELECT 1 AS one")[0],
        &Outcome::Rows(vec![vec![s("1")]])
    );
    assert_eq!(
        find(&r, "SELECT length(current_user)")[0],
        &Outcome::Rows(vec![vec![s("t")]])
    );
    assert_eq!(
        find(&r, "SELECT current_database()")[0],
        &Outcome::Rows(vec![vec![s("postgres")]])
    );
    match find(&r, "SHOW server_version")[0] {
        Outcome::Rows(rows) => assert!(rows[0][0].as_deref().unwrap().starts_with("16.")),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        find(&r, "SHOW timezone")[0],
        &Outcome::Rows(vec![vec![s("UTC")]])
    );
    for (sql, outcome) in run(&c, "00_connect.sql", true).await {
        assert!(
            matches!(outcome, Outcome::RowCount(1)),
            "{sql}: {outcome:?}"
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
            assert!(!matches!(outcome, Outcome::Error(_)), "{sql}: {outcome:?}");
        }
        if extended {
            assert_eq!(find(&r, "INSERT INTO t1_crud")[0], &Outcome::RowCount(3));
            assert_eq!(find(&r, "SELECT count(*)")[0], &Outcome::RowCount(1));
            continue;
        }
        assert_eq!(
            find(&r, "SELECT id, name, qty FROM t1_crud WHERE")[0],
            &Outcome::Rows(vec![
                vec![s("3"), s("cherry"), s("30")],
                vec![s("2"), s("banana"), s("20")]
            ])
        );
        assert_eq!(
            find(&r, "UPDATE t1_crud")[0],
            &Outcome::Rows(vec![vec![s("2"), s("21")]])
        );
        assert_eq!(
            find(&r, "DELETE FROM t1_crud")[0],
            &Outcome::Rows(vec![vec![s("1")]])
        );
        assert_eq!(
            find(&r, "SELECT count(*)")[0],
            &Outcome::Rows(vec![vec![s("2")]])
        );
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
        if !extended {
            let selects = find(&r, "SELECT id, v FROM t1_txn");
            assert_eq!(selects.len(), 2);
            for select in selects {
                assert_eq!(select, &Outcome::Rows(vec![vec![s("1"), s("committed")]]));
            }
            assert_eq!(
                find(&r, "SELECT count(*) AS rr_rows")[0],
                &Outcome::Rows(vec![vec![s("1")]])
            );
        }
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
        if !extended {
            assert_eq!(
                find(&r, "SELECT id, v FROM t1_errors")[0],
                &Outcome::Rows(vec![vec![s("1"), s("first")]])
            );
        }
    }
}
