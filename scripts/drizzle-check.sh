#!/bin/sh
# Exit check for M3: a DSQL-shaped Drizzle project against a running micro-dsql.
#
#   scripts/drizzle-check.sh
#
# Target: the libpq variables PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE (defaults 127.0.0.1,
# 5432, admin, x, postgres); start micro-dsql first and give it a free port. Expects a fresh,
# in-memory server. Needs Node >= 22 and npm (network access the first time, for `npm ci`).
#
# Steps: npm ci; apply the migrations one statement per transaction (twice: the second run must
# apply nothing); a drizzle-orm CRUD and transaction smoke; Drizzle's stock migrator, which must
# fail with 42704 `type "serial" does not exist` as it does on DSQL.
#
# Exit status: 0 = all steps passed; 1 = a step failed; 2 = a prerequisite is missing (Node, npm,
# network, unreachable server).

set -u

here=$(cd "$(dirname "$0")" && pwd)
project="$here/../tests/orm/drizzle"

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "MISSING PREREQUISITE: $1 ($2)" >&2
    exit 2
  fi
}

need node "Node >= 22"
need npm "npm"
node_major=$(node -p 'process.versions.node.split(".")[0]')
if [ "$node_major" -lt 22 ]; then
  echo "MISSING PREREQUISITE: Node >= 22 (found $(node -v))" >&2
  exit 2
fi

cd "$project" || exit 2

echo "== npm ci"
if ! npm ci --no-audit --no-fund >/dev/null 2>"${TMPDIR:-/tmp}/drizzle-check-npm.$$"; then
  cat "${TMPDIR:-/tmp}/drizzle-check-npm.$$" >&2
  rm -f "${TMPDIR:-/tmp}/drizzle-check-npm.$$"
  echo "MISSING PREREQUISITE: npm ci failed (network?)" >&2
  exit 2
fi
rm -f "${TMPDIR:-/tmp}/drizzle-check-npm.$$"

# Runs a step; its status 2 (unreachable server) is passed on, any other failure is a check failure.
run() {
  label=$1
  shift
  echo "== $label"
  "$@"
  status=$?
  if [ "$status" -eq 2 ]; then
    exit 2
  elif [ "$status" -ne 0 ]; then
    echo "FAIL: $label" >&2
    failed=1
  fi
  return "$status"
}

failed=0
run "migrate (DSQL style, first run)" node migrate-dsql.mjs
migrated=$?
if [ "$migrated" -eq 0 ]; then
  second=$(node migrate-dsql.mjs 2>&1)
  second_status=$?
  echo "== migrate (second run must apply nothing)"
  echo "$second"
  if [ "$second_status" -ne 0 ] || ! echo "$second" | grep -q "^applied 0 migration"; then
    echo "FAIL: the second migration run applied something or failed" >&2
    failed=1
  fi
  run "smoke (drizzle-orm queries)" node --experimental-strip-types --disable-warning=ExperimentalWarning smoke.mjs
fi
run "stock migrator (must fail with 42704 as on DSQL)" node stock.mjs

if [ "$failed" -ne 0 ]; then
  echo "DRIZZLE CHECK FAILED"
  exit 1
fi
echo "DRIZZLE CHECK OK"
