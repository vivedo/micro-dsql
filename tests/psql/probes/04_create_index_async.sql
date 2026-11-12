-- probe 04_create_index_async: CREATE INDEX ASYNC returns a job_id; plain CREATE INDEX is refused. Job ids are only checked for shape.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p04_t;
CREATE TABLE p04_t (id int PRIMARY KEY, v text, w int);
INSERT INTO p04_t VALUES (1, 'a', 10), (2, 'b', 20);
CREATE INDEX p04_plain ON p04_t (v);
CREATE INDEX ASYNC p04_v ON p04_t (v) \gset
SELECT length(:'job_id') AS job_id_length, :'job_id' ~ '^[a-z0-9]+$' AS job_id_is_base32;
SELECT sys.wait_for_job(:'job_id') AS wait_for_job;
CREATE UNIQUE INDEX ASYNC p04_w ON p04_t (w) \gset
SELECT sys.wait_for_job(:'job_id') AS wait_for_job;
SELECT indexname FROM pg_indexes WHERE tablename = 'p04_t' ORDER BY indexname;
DROP TABLE p04_t;
