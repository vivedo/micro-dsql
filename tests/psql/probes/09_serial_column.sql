-- probe 09_serial_column: serial pseudo-types are not supported by DSQL (use IDENTITY or sequences).
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
CREATE TABLE p09_s (id serial PRIMARY KEY);
CREATE TABLE p09_b (id bigserial PRIMARY KEY);
CREATE TABLE p09_ss (id smallserial PRIMARY KEY);
DROP TABLE IF EXISTS p09_s;
DROP TABLE IF EXISTS p09_b;
DROP TABLE IF EXISTS p09_ss;
