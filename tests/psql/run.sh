#!/bin/sh
# T1 plain-psql runner. Target-agnostic: uses only POSIX sh and psql.
#
# Usage: tests/psql/run.sh [--probes | --all] [--capture] [name ...]
#
#   (default)   run the layer-1 scenarios (tests/psql/NN_*.sql, NN_*.sh); blocking
#   --probes    run only the DSQL-rule probes (tests/psql/probes/NN_*.sql); report-only
#   --all       scenarios, then probes
#   --capture   write the (normalised) actual output to the expected file instead of
#               comparing; use ONLY against the reference target (MiniStack, DSQL_STRICT=1)
#   name ...    restrict the run to these scenario/probe names (e.g. 20_txn 05_sys_jobs)
#
# Target: the standard libpq variables PGHOST PGPORT PGUSER PGDATABASE PGPASSWORD
# PGSSLMODE. Defaults: PGUSER=admin PGDATABASE=postgres PGSSLMODE=disable
# PGCONNECT_TIMEOUT=5.
#
# Optional environment:
#   KNOWN_GAPS   skip list (default tests/psql/known-gaps.txt): "<name> <reason>" per line
#   RESULTS_DIR  if set, write <name>.out, <name>.diff, results.tsv and target.txt there
#
# Exit status: 0 = every non-skipped scenario passed; 1 = a scenario failed;
# 2 = psql missing or target unreachable. Probe mismatches never change the status.
#
# Each .sql file runs as `psql -X -q -f <file>` from its own directory, with stderr
# merged into stdout, so error lines read "psql:<file>:<line>: ERROR: ...".
# Each .sh scenario is run with `sh <file>` from tests/psql and prints its own output.
#
# Normalisation applied to actual output before comparing (and when capturing):
#   1. lines starting with "LOCATION:" are deleted (VERBOSITY verbose prints the
#      server's source file and line, which differ between engines and builds);
#   2. lines ending in "does not exist, skipping" are deleted (NOTICEs from
#      DROP ... IF EXISTS depend on leftovers from earlier runs).
# Nothing else is rewritten: scenarios and probes must avoid printing OIDs, PIDs,
# timings, generated ids and exact version strings themselves.

set -u

here=$(cd "$(dirname "$0")" && pwd)

: "${PGUSER:=admin}"
: "${PGDATABASE:=postgres}"
: "${PGSSLMODE:=disable}"
: "${PGCONNECT_TIMEOUT:=5}"
export PGUSER PGDATABASE PGSSLMODE PGCONNECT_TIMEOUT
: "${KNOWN_GAPS:=$here/known-gaps.txt}"
: "${RESULTS_DIR:=}"

run_scenarios=1
run_probes=0
capture=0
only=""
while [ $# -gt 0 ]; do
    case $1 in
        --probes) run_scenarios=0; run_probes=1 ;;
        --all) run_scenarios=1; run_probes=1 ;;
        --capture) capture=1 ;;
        -h|--help) sed -n '2,36p' "$0"; exit 0 ;;
        -*) echo "run.sh: unknown option $1" >&2; exit 2 ;;
        *) only="$only $1" ;;
    esac
    shift
done

if ! command -v psql >/dev/null 2>&1; then
    echo "run.sh: psql not found on PATH (install the PostgreSQL client, e.g. libpq)" >&2
    exit 2
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/t1-run.XXXXXX") || exit 2
trap 'rm -rf "$tmp"' EXIT INT TERM

if [ -n "$RESULTS_DIR" ]; then
    mkdir -p "$RESULTS_DIR" || exit 2
    : > "$RESULTS_DIR/results.tsv"
fi

target="${PGHOST:-<default>}:${PGPORT:-<default>}"
if ! psql -X -q -At -c 'SELECT 1' >/dev/null 2>"$tmp/connect.err"; then
    echo "UNREACHABLE target $target"
    sed 's/^/  /' "$tmp/connect.err"
    [ -n "$RESULTS_DIR" ] && printf 'status\tunreachable\ntarget\t%s\n' "$target" > "$RESULTS_DIR/target.txt"
    exit 2
fi
version=$(psql -X -q -At -c 'SHOW server_version' 2>/dev/null)
echo "target $target (server_version ${version:-unknown})"
[ -n "$RESULTS_DIR" ] && printf 'status\treachable\ntarget\t%s\nserver_version\t%s\n' "$target" "$version" > "$RESULTS_DIR/target.txt"

normalise() {
    sed -e '/^LOCATION:/d' -e '/does not exist, skipping$/d'
}

selected() {
    [ -z "$only" ] && return 0
    for o in $only; do [ "$o" = "$1" ] && return 0; done
    return 1
}

skip_reason() {
    [ -f "$KNOWN_GAPS" ] || return 1
    while read -r gap_name gap_reason; do
        case $gap_name in ''|'#'*) continue ;; esac
        if [ "$gap_name" = "$1" ]; then
            echo "${gap_reason:-no reason given}"
            return 0
        fi
    done < "$KNOWN_GAPS"
    return 1
}

record() {  # kind name status
    [ -n "$RESULTS_DIR" ] && printf '%s\t%s\t%s\n' "$1" "$2" "$3" >> "$RESULTS_DIR/results.tsv"
    return 0
}

s_pass=0; s_fail=0; s_skip=0; p_match=0; p_differ=0

run_one() {  # kind dir file
    kind=$1; dir=$2; file=$3
    name=${file%.*}
    expected="$dir/expected/$name.out"
    if [ "$kind" = scenario ] && reason=$(skip_reason "$name"); then
        echo "SKIP $name: $reason"
        s_skip=$((s_skip + 1)); record "$kind" "$name" SKIP
        return
    fi
    case $file in
        *.sql) (cd "$dir" && psql -X -q -f "$file") > "$tmp/raw" 2>&1 ;;
        *.sh) (cd "$dir" && sh "$file") > "$tmp/raw" 2>&1 ;;
    esac
    normalise < "$tmp/raw" > "$tmp/$name.out"
    [ -n "$RESULTS_DIR" ] && cp "$tmp/$name.out" "$RESULTS_DIR/$name.out"
    if [ "$capture" -eq 1 ]; then
        mkdir -p "$dir/expected"
        cp "$tmp/$name.out" "$expected"
        echo "CAPTURED $name"
        record "$kind" "$name" CAPTURED
        return
    fi
    if [ ! -f "$expected" ]; then
        printf 'no expected output %s\n' "$expected" > "$tmp/$name.diff"
    elif diff -u "$expected" "$tmp/$name.out" > "$tmp/$name.diff"; then
        : > "$tmp/$name.diff"
    fi
    [ -n "$RESULTS_DIR" ] && cp "$tmp/$name.diff" "$RESULTS_DIR/$name.diff"
    if [ ! -s "$tmp/$name.diff" ]; then
        if [ "$kind" = scenario ]; then
            echo "PASS $name"; s_pass=$((s_pass + 1)); record "$kind" "$name" PASS
        else
            echo "MATCH $name"; p_match=$((p_match + 1)); record "$kind" "$name" MATCH
        fi
    else
        if [ "$kind" = scenario ]; then
            echo "FAIL $name"; s_fail=$((s_fail + 1)); record "$kind" "$name" FAIL
        else
            echo "DIFFER $name"; p_differ=$((p_differ + 1)); record "$kind" "$name" DIFFER
        fi
        sed 's/^/    /' "$tmp/$name.diff"
    fi
}

list_files() {  # dir
    for f in "$1"/[0-9][0-9]_*.sql "$1"/[0-9][0-9]_*.sh; do
        [ -f "$f" ] && basename "$f"
    done | sort
}

# Warn about skip-list entries that name no scenario.
if [ -f "$KNOWN_GAPS" ]; then
    while read -r gap_name _; do
        case $gap_name in ''|'#'*) continue ;; esac
        if [ ! -f "$here/$gap_name.sql" ] && [ ! -f "$here/$gap_name.sh" ]; then
            echo "warning: known-gaps entry '$gap_name' matches no scenario" >&2
        fi
    done < "$KNOWN_GAPS"
fi

if [ "$run_scenarios" -eq 1 ]; then
    for f in $(list_files "$here"); do
        selected "${f%.*}" && run_one scenario "$here" "$f"
    done
fi
if [ "$run_probes" -eq 1 ]; then
    for f in $(list_files "$here/probes"); do
        selected "${f%.*}" && run_one probe "$here/probes" "$f"
    done
fi

summary=""
[ "$run_scenarios" -eq 1 ] && summary="scenarios pass=$s_pass fail=$s_fail skip=$s_skip"
[ "$run_probes" -eq 1 ] && summary="${summary:+$summary; }probes match=$p_match differ=$p_differ (report-only)"
echo "summary: $summary"

[ "$s_fail" -eq 0 ] || exit 1
exit 0
