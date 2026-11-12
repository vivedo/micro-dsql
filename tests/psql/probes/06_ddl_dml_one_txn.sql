-- probe 06_ddl_dml_one_txn: DDL and DML cannot be mixed in one DSQL transaction.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p06_a;
DROP TABLE IF EXISTS p06_b;
CREATE TABLE p06_a (id int PRIMARY KEY);
-- DDL then DML
BEGIN;
CREATE TABLE p06_b (id int PRIMARY KEY);
INSERT INTO p06_b VALUES (1);
COMMIT;
-- DML then DDL
BEGIN;
INSERT INTO p06_a VALUES (1);
ALTER TABLE p06_a ADD COLUMN v text;
ROLLBACK;
SELECT to_regclass('p06_b') IS NOT NULL AS p06_b_exists;
DROP TABLE IF EXISTS p06_a;
DROP TABLE IF EXISTS p06_b;
