-- probe 26_locking_reads: only FOR UPDATE and FOR KEY SHARE are supported.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
-- Cases mined from MiniStack tests/test_dsql.py (TestLockingClauses, TestLockingReads).
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p26_t;
CREATE TABLE p26_t (id int PRIMARY KEY, s text);
INSERT INTO p26_t VALUES (1, 'a'), (2, 'b');
BEGIN;
SELECT s FROM p26_t WHERE id = 1 FOR UPDATE;
SELECT s FROM p26_t WHERE s = 'b' FOR UPDATE;
SELECT s FROM p26_t WHERE id = 1 FOR KEY SHARE;
COMMIT;
SELECT s FROM p26_t WHERE id = 1 FOR UPDATE NOWAIT;
SELECT s FROM p26_t WHERE id = 1 FOR UPDATE SKIP LOCKED;
SELECT s FROM p26_t WHERE id = 1 FOR SHARE;
SELECT s FROM p26_t WHERE id = 1 FOR NO KEY UPDATE;
WITH x AS (SELECT s FROM p26_t WHERE id = 1 FOR SHARE) SELECT * FROM x;
SELECT s FROM p26_t WHERE s = 'FOR SHARE';
DROP TABLE p26_t;
