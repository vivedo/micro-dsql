//! `SELECT ... FOR UPDATE` over the extended protocol: the portal's parameters are bound to
//! both the touch and the read.

use crate::common::{rows, simple_err, TestServer};

#[tokio::test]
async fn for_update_binds_its_parameter_in_the_touch_and_the_read() {
    let server = TestServer::start();
    let a = server.connect().await;
    let b = server.connect().await;
    rows(&a, "CREATE TABLE t (id int PRIMARY KEY, v int)").await;
    rows(&a, "INSERT INTO t VALUES (1, 0), (2, 0), (3, 0)").await;

    let select = a
        .prepare("SELECT id, v FROM t WHERE id = $1 FOR UPDATE")
        .await
        .unwrap();
    let found = a.query(&select, &[&2i32]).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].get::<_, i32>(0), 2);

    // The touch is bound to the same parameter: it conflicts with a write of row 2 only.
    a.simple_query("BEGIN").await.unwrap();
    let rows_of_two = a.query(&select, &[&2i32]).await.unwrap();
    assert_eq!(rows_of_two.len(), 1);
    b.simple_query("BEGIN").await.unwrap();
    b.simple_query("UPDATE t SET v = 1 WHERE id = 1")
        .await
        .unwrap();
    b.simple_query("UPDATE t SET v = 1 WHERE id = 3")
        .await
        .unwrap();
    b.simple_query("COMMIT").await.unwrap();
    // A write of row 2 by B conflicts with A's touch.
    b.simple_query("BEGIN").await.unwrap();
    b.simple_query("UPDATE t SET v = 5 WHERE id = 2")
        .await
        .unwrap();
    a.simple_query("COMMIT").await.unwrap();
    let error = simple_err(&b, "COMMIT").await;
    assert_eq!(error.code, "40001");

    // Autocommit with a parameter, two parameters out of order.
    let two = a
        .prepare("SELECT id FROM t WHERE v = $2 AND id = $1 FOR UPDATE")
        .await
        .unwrap();
    let found = a.query(&two, &[&3i32, &1i32]).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].get::<_, i32>(0), 3);
}
