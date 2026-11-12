-- probe 01_truncate: TRUNCATE is not supported by DSQL (use DELETE).
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p01_t;
CREATE TABLE p01_t (id int PRIMARY KEY);
INSERT INTO p01_t VALUES (1), (2);
TRUNCATE p01_t;
TRUNCATE TABLE p01_t;
SELECT count(*) AS rows_left FROM p01_t;
DELETE FROM p01_t;
DROP TABLE p01_t;
