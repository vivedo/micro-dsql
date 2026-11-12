-- probe 03_temp_table: Temporary and unlogged tables are not supported by DSQL.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
CREATE TEMP TABLE p03_tmp (a int);
CREATE TEMPORARY TABLE p03_tmp2 (a int);
CREATE UNLOGGED TABLE p03_unlogged (a int);
CREATE TEMP VIEW p03_tv AS SELECT 1 AS a;
DROP TABLE IF EXISTS p03_tmp, p03_tmp2, p03_unlogged;
DROP VIEW IF EXISTS p03_tv;
