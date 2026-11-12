#!/bin/sh
# M0 step 3: app-style schema migration plus representative queries, then the research probes.
# Never stops on error; every statement is echoed with its result or verbose error (SQLSTATE).
set -u
here="$(dirname "$0")"
. "$here/../env.sh"
"$PSQL" -X -At -c "SELECT 1" >/dev/null || { echo "cannot connect to $PGHOST:$PGPORT"; exit 1; }
for f in schema.sql queries.sql probes.sql; do
  echo "=================== $f"
  (cd "$here" && "$PSQL" -X -e -v ON_ERROR_STOP=0 -v VERBOSITY=verbose -f "$f" 2>&1)
done
