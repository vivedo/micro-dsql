-- probe 28_index_forms: index forms and limits.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
-- Cases mined from MiniStack tests/test_dsql.py (TestIndexAsync, TestIndexLimits).
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p28_t;
CREATE TABLE p28_t (id int PRIMARY KEY, c1 int, c2 int, c3 int, c4 int, c5 int, c6 int, c7 int, c8 int, c9 int, s text);
CREATE INDEX CONCURRENTLY p28_conc ON p28_t (c1);
CREATE INDEX ASYNC p28_hash ON p28_t USING hash (c1);
CREATE INDEX ASYNC IF NOT EXISTS ON p28_t (c1);
CREATE INDEX ASYNC p28_nine ON p28_t (c1, c2, c3, c4, c5, c6, c7, c8, c9);
CREATE INDEX ASYNC p28_eight ON p28_t (c1, c2, c3, c4, c5, c6, c7, c8) \gset
SELECT sys.wait_for_job(:'job_id') AS eight_columns;
CREATE INDEX ASYNC p28_expr ON p28_t (lower(s)) \gset
SELECT sys.wait_for_job(:'job_id') AS expression_index;
CREATE INDEX ASYNC p28_partial ON p28_t (c2) WHERE c2 > 0 \gset
SELECT sys.wait_for_job(:'job_id') AS partial_index;
CREATE INDEX ASYNC p28_volatile ON p28_t (c3) WHERE c3 > random();
CREATE INDEX ASYNC p28_incl ON p28_t (c4) INCLUDE (c5) \gset
SELECT sys.wait_for_job(:'job_id') AS include_index;
SELECT indexname FROM pg_indexes WHERE tablename = 'p28_t' ORDER BY indexname;
DROP TABLE p28_t;
