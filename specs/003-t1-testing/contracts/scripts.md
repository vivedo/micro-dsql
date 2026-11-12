# Contract: reference and comparison scripts

## scripts/ministack-up.sh
- Env: `MINISTACK_NAME` (default `micro-dsql-ministack`), `MINISTACK_IMAGE`
  (default `ministackorg/ministack`), `AWS_REGION` (default `us-east-1`).
- Starts the container (`-p 4566:4566 -p 25432-25440:25432-25440`, Docker socket, `DSQL_STRICT=1`)
  unless it already runs; waits for `/_ministack/health`; creates a cluster; waits for `ACTIVE`;
  waits until the endpoint accepts SQL (when psql is on PATH).
- stdout: only `export PG...=...` lines (eval-able). Progress goes to stderr.
- Exit nonzero on any failure.

## scripts/ministack-down.sh
- Removes the MiniStack container and every container named `ministack-dsql-*`. Idempotent.

## scripts/compare.sh
- Targets: `TARGET_A_*`, `TARGET_B_*` (`HOST PORT USER DATABASE PASSWORD SSLMODE`) and
  `TARGET_A_NAME`/`TARGET_B_NAME` (defaults `micro-dsql`, `ministack`), or `--a <envfile>`
  `--b <envfile>` where each env file sets `PG*` variables (and optionally `NAME`).
- Runs `run.sh --all` per target with `RESULTS_DIR`, then writes the report (see
  report-format.md). `REPORT_DIR` defaults to `reports/`.
- Exit 0 when the report was written, even if targets fail or are unreachable.
