-- 30_errors: errors come back with Postgres SQLSTATEs and the session survives.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
DROP TABLE IF EXISTS t1_errors;
CREATE TABLE t1_errors (id int PRIMARY KEY, v text);
INSERT INTO t1_errors VALUES (1, 'first');
-- 23505 unique_violation
INSERT INTO t1_errors VALUES (1, 'duplicate');
-- 42P01 undefined_table
SELECT * FROM t1_no_such_table;
-- 42601 syntax_error
SELEC 1;
-- The connection is still usable.
SELECT id, v FROM t1_errors ORDER BY id;
DROP TABLE t1_errors;
