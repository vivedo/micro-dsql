# Quickstart: validating M2 (005)

Prerequisites: Rust stable, libpq `psql` on `PATH`. Any free local port works; examples use
`$PORT` (pick one that is not 5432).

## 1. Gates

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace          # policy unit tests (no engine) + smoke/dsql_rules integration tests
```

`cargo test -p micro-dsql-policy` runs the rule, rewrite, settings and session state-machine
tests without starting a server.

## 2. Layer-1 scenarios and probes

```sh
cargo run -p micro-dsql -- --listen "127.0.0.1:$PORT" &
export PGHOST=127.0.0.1 PGPORT=$PORT PGPASSWORD=x
tests/psql/run.sh               # must print: scenarios pass=5 fail=0 skip=0
RESULTS_DIR=reports/m2 tests/psql/run.sh --probes   # report-only
```

Expected probe progress (spec SC-003): in `01`, `02`, `03`, `06`, `07`, `08`, `09`, `10`, `20`,
`21`, `23`, `24`, `25`, `29`, every statement an M2 rule covers is refused or accepted as in the
reference, with the same SQLSTATE except the documented deviations; message lines still differ
where the reference uses MiniStack's texts (research.md D1). Never re-capture probe expected files
against micro-dsql.

## 3. Spot checks by hand

```sh
psql -X -v VERBOSITY=verbose <<'SQL'
\set VERBOSITY verbose
CREATE TABLE qs (id int PRIMARY KEY, n numeric);
TRUNCATE qs;                         -- 0A000 unsupported statement: Truncate
BEGIN; CREATE TABLE qs2 (id int); INSERT INTO qs VALUES (1, 1.5);   -- 0A000 ddl and dml …
ROLLBACK;
SET work_mem = '64MB';               -- 0A000 setting configuration parameter "work_mem" not supported
SET enable_seqscan = off; SHOW enable_seqscan;   -- off
INSERT INTO qs VALUES (1, 1.5); SELECT n FROM qs;  -- 1.500000
DROP TABLE qs;
SQL
```

Conflict texts (two sessions, explicit transactions, same row): the losing session reports
`ERROR:  40001: change conflicts with another transaction (OC000)`; `tests/psql/40_conflict.sh`
still passes (it compares SQLSTATEs only).

## 4. MiniStack differential (orchestrator)

Needs Docker and the shared MiniStack instance: `scripts/compare.sh` against micro-dsql and the
reference, then review `reports/diff-<date>.md` against SC-003.
