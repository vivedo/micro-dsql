-- probe 24_txn_discipline: per-transaction DDL/DML discipline.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
-- Cases mined from MiniStack tests/test_dsql.py (TestTxnDiscipline).
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p24_a;
DROP TABLE IF EXISTS p24_b;
CREATE TABLE p24_a (x int PRIMARY KEY);
-- A transaction that only reads may run one DDL.
BEGIN;
SELECT count(*) FROM p24_a;
CREATE TABLE p24_b (x int PRIMARY KEY);
COMMIT;
-- COMMIT resets the per-transaction state.
BEGIN;
INSERT INTO p24_a VALUES (1);
COMMIT;
BEGIN;
INSERT INTO p24_a VALUES (2);
INSERT INTO p24_b VALUES (2);
COMMIT;
SELECT (SELECT count(*) FROM p24_a) AS a_rows, (SELECT count(*) FROM p24_b) AS b_rows;
-- DROP is DDL too.
BEGIN;
DROP TABLE p24_b;
DROP TABLE p24_a;
ROLLBACK;
DROP TABLE IF EXISTS p24_a;
DROP TABLE IF EXISTS p24_b;
