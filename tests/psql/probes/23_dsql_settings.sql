-- probe 23_dsql_settings: dsql.* settings and refused parameters.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
-- Cases mined from MiniStack tests/test_dsql.py (TestDsqlSettings).
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
SHOW dsql.enable_batched_nestloop;
SET dsql.enable_batched_nestloop = off;
SHOW dsql.enable_batched_nestloop;
SET dsql.enable_batched_nestloop = banana;
SET dsql.enable_batched_nestloop = on, off;
RESET dsql.enable_batched_nestloop;
SET dsql.bogus = 1;
SELECT set_config('dsql.bogus', '1', false);
SET enable_hashjoin = off;
RESET default_statistics_target;
SELECT set_config('default_statistics_target', '50', false);
