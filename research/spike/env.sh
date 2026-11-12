# Source this file: default libpq connection settings for the spike scripts.
# Values already set in the environment win.
: "${PGHOST:=127.0.0.1}"
: "${PGPORT:=5432}"
: "${PGUSER:=admin}"
: "${PGDATABASE:=postgres}"
: "${PGPASSWORD:=not-a-real-token}"
: "${PGSSLMODE:=disable}"
: "${PSQL:=psql}"
export PGHOST PGPORT PGUSER PGDATABASE PGPASSWORD PGSSLMODE PSQL
