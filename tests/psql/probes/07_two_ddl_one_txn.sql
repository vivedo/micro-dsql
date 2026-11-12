-- probe 07_two_ddl_one_txn: At most one DDL statement per DSQL transaction.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p07_a;
DROP TABLE IF EXISTS p07_b;
BEGIN;
CREATE TABLE p07_a (id int PRIMARY KEY);
CREATE TABLE p07_b (id int PRIMARY KEY);
COMMIT;
SELECT to_regclass('p07_a') IS NOT NULL AS p07_a_exists, to_regclass('p07_b') IS NOT NULL AS p07_b_exists;
-- Sequential DDL in autocommit is fine.
CREATE TABLE p07_b (id int PRIMARY KEY);
DROP TABLE IF EXISTS p07_a;
DROP TABLE IF EXISTS p07_b;
