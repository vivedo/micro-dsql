-- probe 05_sys_jobs: sys.jobs / sys.wait_for_job / sys.dsql_major_version. Volatile columns are never printed.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p05_t;
CREATE TABLE p05_t (id int PRIMARY KEY, v text);
-- Column names and types (rows are volatile, so the query itself is discarded).
SELECT * FROM sys.jobs \gdesc
SELECT * FROM sys.jobs \g /dev/null
CREATE INDEX ASYNC p05_v ON p05_t (v) \gset
SELECT sys.wait_for_job(:'job_id') AS wait_for_job;
SELECT status, job_type, object_name FROM sys.jobs WHERE job_id = :'job_id';
SELECT sys.dsql_major_version();
SELECT sys.wait_for_job('notajobid') AS malformed_job_id;
DROP TABLE p05_t;
