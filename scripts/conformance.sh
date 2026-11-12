#!/bin/sh
# Exit check for M3: replay Dreamescaper's golden probes (recorded against live Aurora DSQL)
# against a running micro-dsql and write a pass-rate report.
#
#   scripts/conformance.sh
#
# Target: the libpq variables PGHOST PGPORT PGPASSWORD (defaults 127.0.0.1, 5432, x); start
# micro-dsql first, fresh, on a free port.
#
# Needs git, Go >= 1.27.1 (never downloads a toolchain) and network access the first time. The
# upstream project is fetched into $CONFORMANCE_DIR (default target/conformance/dsql-emulator,
# gitignored) at the commit in scripts/conformance/UPSTREAM; nothing of it is committed here.
#
#   CONFORMANCE_MIN   pass-rate threshold in percent (default 60)
#   CONFORMANCE_DIR   working directory (default target/conformance)
#
# Output: reports/conformance-<UTC date>.md (gitignored); the last line printed is
# `TOTAL passed/total (pct%)`.
#
# Exit status: 0 = at or above the threshold; 1 = below it; 2 = a prerequisite is missing (git,
# Go, network, unreachable server).

set -u

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)

: "${PGHOST:=127.0.0.1}"
: "${PGPORT:=5432}"
: "${PGPASSWORD:=x}"
: "${CONFORMANCE_MIN:=60}"
: "${CONFORMANCE_DIR:=$root/target/conformance}"
UPSTREAM_URL=${CONFORMANCE_UPSTREAM_URL:-https://github.com/Dreamescaper/dsql-emulator.git}

die() {
  echo "MISSING PREREQUISITE: $1" >&2
  exit 2
}

command -v git >/dev/null 2>&1 || die "git"
command -v go >/dev/null 2>&1 || die "Go >= 1.27.1"
export GOTOOLCHAIN=local
go_version=$(go env GOVERSION 2>/dev/null | sed 's/^go//')
case "$go_version" in
  1.27.[1-9]* | 1.27.[1-9][0-9]* | 1.2[89]* | 1.[3-9][0-9]* | [2-9].*) ;;
  *) die "Go >= 1.27.1 (found ${go_version:-none})" ;;
esac

pinned=$(tr -d ' \n' <"$here/conformance/UPSTREAM")
[ -n "$pinned" ] || die "scripts/conformance/UPSTREAM is empty"

# The target must answer before anything is fetched or built.
if ! (exec 3<>"/dev/tcp/$PGHOST/$PGPORT") 2>/dev/null; then
  # /dev/tcp is a bash feature; fall back to nc.
  if command -v nc >/dev/null 2>&1; then
    nc -z "$PGHOST" "$PGPORT" >/dev/null 2>&1 || die "no server at $PGHOST:$PGPORT"
  else
    die "no server at $PGHOST:$PGPORT (or neither bash /dev/tcp nor nc to check)"
  fi
fi

work="$CONFORMANCE_DIR/dsql-emulator"
mkdir -p "$CONFORMANCE_DIR" || die "cannot create $CONFORMANCE_DIR"
if [ ! -d "$work/.git" ]; then
  git clone --quiet "$UPSTREAM_URL" "$work" || die "cannot fetch $UPSTREAM_URL (network?)"
fi
if [ "$(git -C "$work" rev-parse HEAD 2>/dev/null)" != "$pinned" ]; then
  git -C "$work" fetch --quiet origin "$pinned" 2>/dev/null || git -C "$work" fetch --quiet origin || die "cannot fetch $pinned"
  git -C "$work" checkout --quiet --detach "$pinned" || die "cannot check out $pinned"
fi
[ "$(git -C "$work" rev-parse HEAD)" = "$pinned" ] || die "checkout is not at $pinned"

# Our comparer lives inside the upstream module: its conformance package is internal.
mkdir -p "$work/cmd/micro-dsql-compare"
cp "$here/conformance/compare.go" "$work/cmd/micro-dsql-compare/main.go"

recorded=$(mktemp -d "${TMPDIR:-/tmp}/micro-dsql-conformance.XXXXXX") || die "mktemp"
trap 'rm -rf "$recorded"' EXIT

echo "== recording the suite against $PGHOST:$PGPORT (commit $pinned)"
(
  cd "$work" || exit 2
  go run ./cmd/dsql-baseline --host "$PGHOST" --port "$PGPORT" --sslmode disable \
    --token "$PGPASSWORD" --label micro-dsql --out-dir "$recorded"
) >"$recorded.log" 2>&1
status=$?
tail -n 3 "$recorded.log"
rm -f "$recorded.log"
if [ "$status" -ne 0 ]; then
  echo "the recorder failed (exit $status): is micro-dsql reachable and fresh?" >&2
  exit 2
fi

mkdir -p "$root/reports"
report="$root/reports/conformance-$(date -u +%Y-%m-%d).md"
echo "== comparing with the golden record"
(
  cd "$work" || exit 2
  CONFORMANCE_COMMIT="$pinned" CONFORMANCE_TARGET="$PGHOST:$PGPORT" \
    go run ./cmd/micro-dsql-compare "$work/test/conformance/golden" "$recorded" "$report" "$CONFORMANCE_MIN"
)
status=$?
case "$status" in
  0 | 1) echo "report: $report" ;;
  *) echo "the comparer failed (exit $status)" >&2; exit 2 ;;
esac
exit "$status"
