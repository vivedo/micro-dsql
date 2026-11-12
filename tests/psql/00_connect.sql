-- 00_connect: the target answers like a DSQL cluster's database.
-- The session itself proves that user admin, database postgres and an arbitrary
-- password are accepted (run.sh connects with PGUSER=admin PGDATABASE=postgres).
-- current_user is only checked for presence: the MiniStack reference maps every
-- login to its backend superuser, so the role name is compared in probe
-- 12_session_identity instead.
-- server_version is checked for the major version only (DSQL reports 16.x; the
-- exact minor differs between DSQL, MiniStack and micro-dsql).
SELECT 1 AS one;
SELECT length(current_user) > 0 AS has_current_user;
SELECT current_database();
SHOW server_version \gset
SELECT :'server_version' LIKE '16.%' AS server_version_is_16;
SHOW timezone;
