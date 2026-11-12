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
