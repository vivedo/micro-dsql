# Research: T1 testing phase

Each entry: decision, rationale, alternatives considered.

## R1. Reference target
- **Decision**: MiniStack (`ministackorg/ministack`, `DSQL_STRICT=1`, Docker socket mounted).
- **Rationale**: docs/plan.md T1 names it; it fronts a real `postgres:16-alpine` with a DSQL rule proxy
  whose messages were measured against live DSQL. The proxy runs inside the MiniStack container and
  listens on `25432+n`, so publishing `25432-25440` on the MiniStack container is sufficient (the
  backend Postgres containers are not published).
- **Alternatives**: plain `postgres:16` (no DSQL rules; kept as the documented fallback),
  Dreamescaper emulator (not the docs/plan.md reference).

## R2. Measured reference facts (MiniStack, 2026-11-12)
- `server_version` = `16.15`, `TimeZone` = `UTC`, `is_superuser` = `on`.
- Default `transaction_isolation` = `read committed`; `BEGIN ISOLATION LEVEL READ COMMITTED` is
  accepted. Real DSQL refuses non-RR levels, so the isolation check is a probe, not a scenario.
- `GetCluster` reports the endpoint as `localhost:<port>`; the cluster is `ACTIVE` within seconds.
- Endpoint values are therefore normalised in scenarios: `00_connect` prints
  `server_version LIKE '16.%'` instead of the exact version.

## R3. Conflict scenario design (`40_conflict.sh`)
- **Decision**: two psql coprocesses fed through FIFOs; ordering B: `BEGIN ISOLATION LEVEL
  REPEATABLE READ; SELECT` (snapshot) → A: `BEGIN …; UPDATE; COMMIT` → B: `UPDATE; COMMIT`. The
  script checks that some session reported SQLSTATE `40001` and that the stored value is A's.
- **Rationale**: no statement ever waits on a row lock, so it is deterministic on lock-based
  engines (Postgres RR fails B's UPDATE with 40001), on Turso (write-write conflict on B's
  UPDATE) and on DSQL (B's COMMIT fails with OC000). The explicit RR form is accepted by DSQL.
- **Alternatives**: overlapping writers (blocks on Postgres; timing dependent), relying on the
  default isolation (no conflict on READ COMMITTED engines).
- Synchronisation uses `\echo` markers polled from each session's output file with a timeout;
  only POSIX `sh`, `mkfifo`, `grep`, `sleep`.

## R4. Output determinism
- `psql -X -q -f <relative file>` run from `tests/psql`, so error prefixes read
  `psql:30_errors.sql:<line>:`.
- `\set VERBOSITY verbose` only in `30_errors.sql` (docs/plan.md); `20_txn` and `40_conflict` use
  `\set VERBOSITY sqlstate` so only codes are compared.
- Runner normalisation (documented in `run.sh` and contracts/run-sh.md): delete lines starting with
  `LOCATION:` and NOTICE lines ending in `does not exist, skipping`.
- Probes never print generated job ids, OIDs or timestamps; they print shape checks
  (e.g. `length(job_id) = 26`).

## R5. Tooling availability
- `psql`: expected outputs recorded with psql 16 (the `postgres:16-alpine` client), matching the
  `postgresql-client` of Ubuntu 24.04 runners. Scripts take `psql` from PATH only.
- AWS CLI: `ministack-up.sh` uses `aws` from PATH; if missing it runs `amazon/aws-cli` with
  `--network container:<ministack>` so `http://localhost:4566` resolves inside MiniStack's network
  namespace. Dummy credentials `test/test`, region `us-east-1`.

## R6. Probe catalogue (mined from MiniStack `tests/test_dsql.py`)
Class → probe file: `TestDenylist` → `20_denylist`; `TestColumnTypes` → `21_column_types`;
`TestIdentityAndSequences` → `22_identity_sequences`; `TestDsqlSettings` → `23_dsql_settings`;
`TestTxnDiscipline` → `06/07` + `24_txn_discipline`; `TestTxnLimits` → `25_txn_row_limit`;
`TestLockingClauses`/`TestLockingReads` → `26_locking_reads`; `TestAlterTableSubset`/
`TestForeignKeys` → `27_alter_table`; `TestIndexAsync`/`TestIndexLimits` → `04_create_index_async`
+ `28_index_forms`; `TestTransactionAbortSemantics` → `29_txn_abort`; `TestWaitForJob` → `05_sys_jobs`.
