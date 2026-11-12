#!/bin/sh
# M0 step 2: can a stock client connect (sslmode=disable, arbitrary password) and run SELECT 1?
set -u
. "$(dirname "$0")/env.sh"
"$PSQL" -X -e -v ON_ERROR_STOP=0 -v VERBOSITY=verbose <<'SQL'
\set VERBOSITY verbose
SELECT 1;
SELECT current_user, session_user, current_database();
SELECT version();
SHOW server_version;
SHOW timezone;
SHOW TimeZone;
SHOW search_path;
SHOW transaction_isolation;
SHOW standard_conforming_strings;
SET application_name = 'spike';
SET timezone = 'UTC';
SET extra_float_digits = 3;
SELECT now();
SQL
echo "--- startup parameters reported by the server (via libpq):"
"$PSQL" -X -At -c 'SELECT 1' >/dev/null && "$PSQL" -X -c '\echo server_version=:SERVER_VERSION_NAME :SERVER_VERSION_NUM'
