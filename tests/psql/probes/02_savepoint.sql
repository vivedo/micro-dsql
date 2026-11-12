-- probe 02_savepoint: SAVEPOINT is not supported by DSQL (0A000).
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p02_t;
CREATE TABLE p02_t (id int PRIMARY KEY);
BEGIN;
INSERT INTO p02_t VALUES (1);
SAVEPOINT sp1;
INSERT INTO p02_t VALUES (2);
ROLLBACK TO SAVEPOINT sp1;
RELEASE SAVEPOINT sp1;
COMMIT;
SELECT count(*) AS rows_kept FROM p02_t;
DROP TABLE p02_t;
