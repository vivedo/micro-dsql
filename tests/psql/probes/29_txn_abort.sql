-- probe 29_txn_abort: a refused statement aborts the transaction block.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
-- Cases mined from MiniStack tests/test_dsql.py (TestTransactionAbortSemantics).
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p29_t;
CREATE TABLE p29_t (id int PRIMARY KEY);
BEGIN;
INSERT INTO p29_t VALUES (1);
TRUNCATE p29_t;
SELECT 1 AS after_refusal;
ROLLBACK;
SELECT count(*) AS rows_after_rollback FROM p29_t;
DROP TABLE p29_t;
