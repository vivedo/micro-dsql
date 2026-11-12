-- 20_txn: explicit transactions. Only SQLSTATEs are printed for errors.
DROP TABLE IF EXISTS t1_txn;
CREATE TABLE t1_txn (id int PRIMARY KEY, v text);
\set ON_ERROR_STOP 0
\set VERBOSITY sqlstate
-- BEGIN / COMMIT keeps the write.
BEGIN;
INSERT INTO t1_txn VALUES (1, 'committed');
COMMIT;
-- ROLLBACK discards the write.
BEGIN;
INSERT INTO t1_txn VALUES (2, 'rolled back');
SELECT count(*) AS inside_txn FROM t1_txn;
ROLLBACK;
SELECT id, v FROM t1_txn ORDER BY id;
-- An error inside a transaction aborts it: 25P02 until ROLLBACK.
BEGIN;
INSERT INTO t1_txn VALUES (3, 'lost');
SELECT 1 / 0;
SELECT 1;
INSERT INTO t1_txn VALUES (4, 'also lost');
ROLLBACK;
SELECT id, v FROM t1_txn ORDER BY id;
-- The DSQL isolation level is accepted explicitly.
BEGIN ISOLATION LEVEL REPEATABLE READ;
SELECT count(*) AS rr_rows FROM t1_txn;
COMMIT;
DROP TABLE t1_txn;
