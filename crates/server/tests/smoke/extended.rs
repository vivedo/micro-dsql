//! User story 4 (stretch): drivers using the extended protocol with binary parameters and
//! typed results (M0 blockers B1, B2, B3) and foreign keys (B5).
//!
//! tokio-postgres asks the server for parameter types (Describe), sends every parameter in
//! binary and requests binary results, like asyncpg and pgx.

use std::error::Error;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio_postgres::types::{FromSql, Type};
use uuid::Uuid;

use crate::common::{rows, sqlstate, TestServer};

/// A `numeric` read in binary and rendered as text.
#[derive(Debug, PartialEq)]
struct NumericText(String);

impl<'a> FromSql<'a> for NumericText {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        let word = |i: usize| i16::from_be_bytes([raw[i], raw[i + 1]]);
        let (ndigits, weight, sign, dscale) = (word(0), word(2), word(4) as u16, word(6));
        let digits: Vec<i64> = (0..ndigits as usize)
            .map(|i| i64::from(word(8 + 2 * i)))
            .collect();
        let group = |pos: i64| -> i64 {
            usize::try_from(i64::from(weight) - pos)
                .ok()
                .and_then(|i| digits.get(i).copied())
                .unwrap_or(0)
        };
        let mut int = String::new();
        for pos in (0..=i64::from(weight).max(0)).rev() {
            if int.is_empty() {
                int = group(pos).to_string();
            } else {
                int.push_str(&format!("{:04}", group(pos)));
            }
        }
        let mut frac = String::new();
        let mut pos = -1;
        while frac.len() < dscale as usize {
            frac.push_str(&format!("{:04}", group(pos)));
            pos -= 1;
        }
        frac.truncate(dscale as usize);
        let sign = if sign == 0x4000 { "-" } else { "" };
        Ok(NumericText(if frac.is_empty() {
            format!("{sign}{int}")
        } else {
            format!("{sign}{int}.{frac}")
        }))
    }

    fn accepts(ty: &Type) -> bool {
        *ty == Type::NUMERIC
    }
}

#[tokio::test]
async fn describe_reports_parameter_count_and_types() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(
        &c,
        "CREATE TABLE typed (id int PRIMARY KEY, name text, qty smallint, big bigint, \
         ok boolean, price numeric(10,2), u uuid, ts timestamptz)",
    )
    .await;
    let stmt = c.prepare("SELECT $1::int8 + 1").await.unwrap();
    assert_eq!(stmt.params(), &[Type::INT8]);
    let stmt = c
        .prepare("SELECT name FROM typed WHERE id = $1 AND name = $2 LIMIT $3")
        .await
        .unwrap();
    assert_eq!(stmt.params(), &[Type::INT4, Type::TEXT, Type::INT8]);
    let stmt = c
        .prepare("INSERT INTO typed (id, qty, big, ok, u, ts) VALUES ($1, $2, $3, $4, $5, $6)")
        .await
        .unwrap();
    assert_eq!(
        stmt.params(),
        &[
            Type::INT4,
            Type::INT2,
            Type::INT8,
            Type::BOOL,
            Type::UUID,
            Type::TIMESTAMPTZ
        ]
    );
    let stmt = c
        .prepare("UPDATE typed SET price = $1 WHERE id = $2")
        .await
        .unwrap();
    assert_eq!(stmt.params(), &[Type::NUMERIC, Type::INT4]);
}

#[tokio::test]
async fn binary_parameters_and_typed_results_round_trip() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(
        &c,
        "CREATE TABLE rt (id int PRIMARY KEY, name text, qty smallint, big bigint, \
         ok boolean, f float8, u uuid, ts timestamptz)",
    )
    .await;
    let u = Uuid::parse_str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11").unwrap();
    let ts = UNIX_EPOCH + Duration::from_micros(1_700_000_000_123_000);
    c.execute(
        "INSERT INTO rt (id, name, qty, big, ok, f, u, ts) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[
            &1i32,
            &"apple",
            &7i16,
            &9_000_000_000i64,
            &true,
            &1.5f64,
            &u,
            &ts,
        ],
    )
    .await
    .unwrap();
    let row = c
        .query_one(
            "SELECT id, name, qty, big, ok, f, u, ts FROM rt WHERE id = $1",
            &[&1i32],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, i32>(0), 1);
    assert_eq!(row.get::<_, &str>(1), "apple");
    assert_eq!(row.get::<_, i16>(2), 7);
    assert_eq!(row.get::<_, i64>(3), 9_000_000_000);
    assert!(row.get::<_, bool>(4));
    assert_eq!(row.get::<_, f64>(5), 1.5);
    assert_eq!(row.get::<_, Uuid>(6), u);
    assert_eq!(row.get::<_, SystemTime>(7), ts);
}

#[tokio::test]
async fn count_and_comparisons_have_postgres_types() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(&c, "CREATE TABLE agg (id int PRIMARY KEY)").await;
    rows(&c, "INSERT INTO agg VALUES (1), (2), (3)").await;
    let row = c
        .query_one("SELECT count(*), 1 = 1, max(id) IS NULL FROM agg", &[])
        .await
        .unwrap();
    assert_eq!(row.columns()[0].type_(), &Type::INT8);
    assert_eq!(row.get::<_, i64>(0), 3);
    assert!(row.get::<_, bool>(1));
    assert!(!row.get::<_, bool>(2));
}

#[tokio::test]
async fn numeric_columns_are_numeric() {
    let server = TestServer::start();
    let c = server.connect().await;
    rows(
        &c,
        "CREATE TABLE money (id int PRIMARY KEY, price numeric(10,2))",
    )
    .await;
    rows(&c, "INSERT INTO money VALUES (1, 12.5)").await;
    let row = c
        .query_one("SELECT price FROM money WHERE id = 1", &[])
        .await
        .unwrap();
    assert_eq!(row.columns()[0].type_(), &Type::NUMERIC);
    let value: NumericText = row.get(0);
    assert!(value.0 == "12.5" || value.0 == "12.50", "{value:?}");
}

#[tokio::test]
async fn foreign_keys_are_enforced() {
    let server = TestServer::start();
    let c = server.connect().await;
    // One DDL per statement: DSQL refuses two in one query string.
    rows(&c, "CREATE TABLE parent (id int PRIMARY KEY)").await;
    rows(
        &c,
        "CREATE TABLE child (id int PRIMARY KEY, pid int REFERENCES parent(id) ON DELETE CASCADE)",
    )
    .await;
    assert_eq!(
        sqlstate(&c, "INSERT INTO child VALUES (1, 42)").await,
        "23503"
    );
    rows(
        &c,
        "INSERT INTO parent VALUES (1); INSERT INTO child VALUES (1, 1)",
    )
    .await;
    rows(&c, "DELETE FROM parent WHERE id = 1").await;
    assert_eq!(
        crate::common::value(&c, "SELECT count(*) FROM child").await,
        crate::common::s("0")
    );
}
