#!/bin/sh
# Run every M0 spike experiment and write the output to spike/results/.
#
# If TURSOPG points at a tursopg binary, a fresh in-memory server is started on
# $PGHOST:$PGPORT for each experiment group (clean slate) and stopped afterwards.
# Otherwise the scripts run against whatever server is already listening.
#
#   TURSOPG=<turso-clone>/target/release/tursopg PSQL=<path-to-psql> spike/run-all.sh
set -u
here="$(cd "$(dirname "$0")" && pwd)"
. "$here/env.sh"
out="$here/results"
mkdir -p "$out"
PY="${PYTHON:-$here/../.venv/bin/python}"
server_pid=""
export PYTHONUNBUFFERED=1

start_server() {
  [ -n "${TURSOPG:-}" ] || return 0
  "$TURSOPG" --server "$PGHOST:$PGPORT" >"$out/server-$1.log" 2>&1 &
  server_pid=$!
  i=0
  until nc -z "$PGHOST" "$PGPORT" 2>/dev/null; do
    i=$((i + 1)); [ $i -gt 100 ] && { echo "server did not start"; exit 1; }
    sleep 0.05
  done
}
stop_server() {
  [ -n "$server_pid" ] || return 0
  if kill -0 "$server_pid" 2>/dev/null; then kill "$server_pid"; wait "$server_pid" 2>/dev/null
  else echo "!!! server for $1 was not running at the end (crash?)" | tee -a "$out/$1.txt"; fi
  server_pid=""
}
run() { # name command...  (killed after $SPIKE_SCRIPT_TIMEOUT seconds, default 300)
  name=$1; shift
  echo "### $name"
  start_server "$name"
  "$@" >"$out/$name.txt" 2>&1 &
  pid=$!
  ( sleep "${SPIKE_SCRIPT_TIMEOUT:-300}"; kill "$pid" 2>/dev/null && echo "!!! killed after timeout" >>"$out/$name.txt" ) >/dev/null 2>&1 &
  guard=$!
  wait "$pid"; rc=$?
  kill "$guard" 2>/dev/null; wait "$guard" 2>/dev/null
  echo "exit=$rc" >>"$out/$name.txt"
  stop_server "$name"
  tail -1 "$out/$name.txt"
}

if [ -n "${TURSOPG:-}" ]; then
  "$TURSOPG" --version >"$out/server-version.txt" 2>&1
  # Startup time: launch to first accepted TCP connection.
  "$PY" "$here/startup_time.py" "$TURSOPG" >>"$out/server-version.txt" 2>&1
fi

run connect     "$here/connect.sh"
run app         "$here/app/run.sh"
run drv-psql    "$here/drivers/psql.sh"
run drv-psycopg "$PY" "$here/drivers/psycopg_smoke.py"
run drv-asyncpg "$PY" "$here/drivers/asyncpg_smoke.py"
run drv-node    node "$here/drivers/node/pg_smoke.js"
run drv-pgx     sh -c "cd '$here/drivers/go' && go build -o pgx-smoke . && ./pgx-smoke"
run wire        "$PY" "$here/drivers/wire_probe.py"
run occ         "$PY" "$here/occ/occ.py"
echo "results in $out"
