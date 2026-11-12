#!/bin/sh
# 40_conflict: two psql sessions update the same row inside transactions, and one
# of them must get SQLSTATE 40001. Only *that* a conflict is reported is checked,
# not *when* (DSQL fails the later committer at COMMIT; Postgres and Turso fail
# the second writer at its UPDATE). See specs/003-t1-testing/research.md R3.
#
# Ordering (no statement ever waits on a row lock, so lock-based engines cannot
# hang and the result is deterministic):
#   B: BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT   -- B's snapshot exists
#   A: BEGIN ISOLATION LEVEL REPEATABLE READ; UPDATE; COMMIT
#   B: UPDATE; COMMIT                                  -- 40001 here or at COMMIT
#
# Uses only POSIX sh, mkfifo and psql; the target comes from the PG* variables.
# Prints a short deterministic summary; raw session output is shown only on error.

set -u
: "${PGUSER:=admin}" "${PGDATABASE:=postgres}" "${PGSSLMODE:=disable}" "${PGCONNECT_TIMEOUT:=5}"
export PGUSER PGDATABASE PGSSLMODE PGCONNECT_TIMEOUT
: "${CONFLICT_TIMEOUT:=30}"   # seconds to wait for each step

work=$(mktemp -d "${TMPDIR:-/tmp}/t1-conflict.XXXXXX") || exit 2
pid_a=""; pid_b=""
cleanup() {
    exec 3>&- 4>&- 2>/dev/null
    [ -n "$pid_a" ] && kill "$pid_a" 2>/dev/null
    [ -n "$pid_b" ] && kill "$pid_b" 2>/dev/null
    rm -rf "$work"
}
trap cleanup EXIT INT TERM

fail() {
    echo "error: $*"
    for s in a b; do
        echo "--- session $s output ---"
        cat "$work/$s.out" 2>/dev/null
    done
    exit 1
}

# Run a one-off statement in its own session (setup and verification).
oneshot() {
    psql -X -q -At -v ON_ERROR_STOP=1 -c "$1" 2>&1
}

# Wait until session $1's output contains the marker line $2.
wait_for() {
    i=0
    while ! grep -qx "$2" "$work/$1.out" 2>/dev/null; do
        i=$((i + 1))
        [ "$i" -gt $((CONFLICT_TIMEOUT * 10)) ] && fail "timeout waiting for '$2' in session $1"
        sleep 0.1
    done
}

oneshot "DROP TABLE IF EXISTS t1_conflict" >/dev/null
out=$(oneshot "CREATE TABLE t1_conflict (id int PRIMARY KEY, v text)") || fail "create table: $out"
out=$(oneshot "INSERT INTO t1_conflict VALUES (1, 'initial')") || fail "insert: $out"

mkfifo "$work/a.in" "$work/b.in" || fail "mkfifo"
psql -X -q < "$work/a.in" > "$work/a.out" 2>&1 &
pid_a=$!
psql -X -q < "$work/b.in" > "$work/b.out" 2>&1 &
pid_b=$!
exec 3> "$work/a.in"
exec 4> "$work/b.in"

for fd in 3 4; do
    printf '%s\n' '\set ON_ERROR_STOP 0' '\set VERBOSITY sqlstate' '\pset tuples_only on' >&$fd
done

# Step 1: B opens its transaction and takes its snapshot.
printf '%s\n' 'BEGIN ISOLATION LEVEL REPEATABLE READ;' \
    'SELECT v FROM t1_conflict WHERE id = 1;' '\echo B1-DONE' >&4
wait_for b B1-DONE

# Step 2: A updates the row and commits.
printf '%s\n' 'BEGIN ISOLATION LEVEL REPEATABLE READ;' \
    "UPDATE t1_conflict SET v = 'from A' WHERE id = 1;" 'COMMIT;' '\echo A-DONE' >&3
wait_for a A-DONE

# Step 3: B updates the same row and tries to commit.
printf '%s\n' "UPDATE t1_conflict SET v = 'from B' WHERE id = 1;" 'COMMIT;' '\echo B2-DONE' >&4
wait_for b B2-DONE

exec 3>&- 4>&-
wait "$pid_a" 2>/dev/null; pid_a=""
wait "$pid_b" 2>/dev/null; pid_b=""

conflict_a=no; conflict_b=no
grep -q '40001' "$work/a.out" && conflict_a=yes
grep -q '40001' "$work/b.out" && conflict_b=yes
# Any error other than 40001 is unexpected.
if grep 'ERROR' "$work/a.out" "$work/b.out" | grep -vq '40001'; then
    fail "unexpected error"
fi

final=$(oneshot "SELECT v FROM t1_conflict WHERE id = 1") || fail "final select: $final"
oneshot "DROP TABLE t1_conflict" >/dev/null

if [ "$conflict_a" = yes ] || [ "$conflict_b" = yes ]; then
    echo "40001 reported: yes"
else
    echo "40001 reported: no"
fi
echo "session A (first committer) got 40001: $conflict_a"
echo "final value: $final"
