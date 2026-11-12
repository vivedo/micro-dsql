-- probe 10_set_not_whitelisted: Only whitelisted session settings may be SET.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
SET work_mem = '64MB';
SET statement_timeout = '5s';
SET lock_timeout = '1s';
SET idle_in_transaction_session_timeout = '1min';
SET default_statistics_target = 200;
SELECT set_config('work_mem', '8MB', false);
-- Whitelisted settings work and are visible to SHOW.
SET application_name = 'p10';
SHOW application_name;
SET search_path = public;
SHOW search_path;
SET timezone = 'UTC';
SHOW timezone;
SET datestyle = 'ISO, MDY';
SHOW datestyle;
SET extra_float_digits = 1;
SET intervalstyle = 'postgres';
SET enable_seqscan = off;
SHOW enable_seqscan;
RESET enable_seqscan;
