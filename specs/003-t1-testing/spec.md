# Feature Specification: T1 testing phase (psql scenarios, DSQL probes, differential run)

**Feature Branch**: `003-t1-testing`

**Created**: 2026-11-12

**Status**: Harness implemented; Reconcile (after M1) pending

**Input**: User description: "T1 testing phase of docs/plan.md: target-agnostic psql scenario harness (layer 1: 00_connect, 10_crud, 20_txn, 30_errors, 40_conflict, run.sh with known-gaps skip list), DSQL-rule probes (report-only) incl. cases mined from MiniStack tests/test_dsql.py, MiniStack reference bring-up scripts (ministack-up/down), differential compare.sh writing reports/diff-<date>.md, optional CI differential workflow; exit criteria from docs/plan.md T1 (layer 1 green vs micro-dsql locally & CI, green vs MiniStack, first diff report with probe mismatches as M2/M3 backlog, README 3-line quick start). Parts needing the micro-dsql binary are deferred to a post-M1 reconcile phase."

> **Delivery note.** This spec covers all of T1. The harness (scenarios, expected outputs, probes,
> runner, reference bring-up, comparison report, CI draft) is delivered in this wave. Everything
> that needs a running micro-dsql binary (milestone M1, built in parallel) is delivered in a
> separate **Reconcile (after M1)** phase of `tasks.md`.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Run the plain-psql acceptance scenarios against any endpoint (Priority: P1)

A developer points the standard libpq connection variables at any Postgres-speaking endpoint
(micro-dsql, the MiniStack DSQL emulator, a plain Postgres) and runs one command. Each scenario
(connect, CRUD, transactions, errors, write conflict) runs in its own session; its output is
compared with a stored expected output; the command prints a pass/fail summary and exits nonzero if
any non-skipped scenario fails.

**Why this priority**: This is the blocking T1 gate (constitution gate 4) and the definition of
"PoC done". Without it nothing else in T1 can be measured.

**Independent Test**: Start the reference emulator, export its connection variables, run the
scenario runner; all five scenarios pass and the exit status is 0.

**Acceptance Scenarios**:

1. **Given** a reachable DSQL-shaped endpoint, **When** the runner is started, **Then** each of
   `00_connect`, `10_crud`, `20_txn`, `30_errors`, `40_conflict` is reported as PASS and the exit
   status is 0.
2. **Given** a scenario whose output differs from its expected output, **When** the runner is
   started, **Then** that scenario is reported as FAIL with a unified diff, the summary counts it,
   and the exit status is nonzero.
3. **Given** a scenario listed in the known-gaps skip list with a reason, **When** the runner is
   started, **Then** it is reported as SKIP with that reason and does not affect the exit status.
4. **Given** an unreachable endpoint, **When** the runner is started, **Then** it reports the
   target as unreachable and exits nonzero without hanging.

---

### User Story 2 - Measure the DSQL-rule gap with report-only probes (Priority: P2)

A developer runs the DSQL-rule probes (TRUNCATE, SAVEPOINT, temp tables, async index, `sys.jobs`,
transaction discipline, extensions, SERIAL, settings whitelist, isolation levels, and the cases
mined from MiniStack's test classes) against a target. Each probe's output is compared with the
reference behaviour; mismatches are listed but never fail the run.

**Why this priority**: Probes turn "looks like DSQL" into a measured backlog for M2/M3. They do not
gate the PoC.

**Independent Test**: Run the probes against the reference; every probe matches its stored expected
output. Run them against plain Postgres; mismatches are listed and the exit status is still 0.

**Acceptance Scenarios**:

1. **Given** a target, **When** the probes are run, **Then** each probe is reported as MATCH or
   DIFFER and the exit status is 0 regardless of mismatches.
2. **Given** the reference emulator, **When** the probes are run, **Then** all probes report MATCH.

---

### User Story 3 - Bring up the DSQL reference and produce a differential report (Priority: P2)

A developer starts the reference emulator with one command, gets the connection variables for its
cluster, runs one comparison command against two targets (micro-dsql and the reference), and gets a
dated Markdown report listing, per scenario and probe, the result on each target and the differing
output lines. A second command tears the reference down.

**Why this priority**: The report is a T1 exit criterion and the source of the M2/M3 backlog.

**Independent Test**: Bring up the reference, run the comparison with target A down and target B the
reference; the report lists A as unreachable and B's results, and is written under `reports/`.

**Acceptance Scenarios**:

1. **Given** Docker is available, **When** the bring-up command runs, **Then** a cluster reaches
   ACTIVE and the command prints `PGHOST`/`PGPORT`/`PGUSER`/`PGDATABASE`/`PGPASSWORD`/`PGSSLMODE`
   exports for it.
2. **Given** two target descriptions, **When** the comparison runs, **Then**
   `reports/diff-<date>.md` contains one row per scenario/probe with each target's result and a
   section with the differing lines.
3. **Given** one target is down, **When** the comparison runs, **Then** that target is reported as
   unreachable and the report is still written.
4. **Given** the reference is running, **When** the teardown command runs, **Then** the emulator
   and the per-cluster backend containers it started are removed.

---

### User Story 4 - Prove the PoC on micro-dsql (Priority: P1, deferred to Reconcile after M1)

Once M1 provides the binary, the developer runs layer 1 against micro-dsql locally and in CI, runs
the same scenarios through the M1 `tokio-postgres` harness (extended protocol), produces the first
differential report, files probe mismatches as M2/M3 backlog items, and adds the 3-line quick start
to the README.

**Why this priority**: This closes the PoC, but it depends on M1 and cannot be done in this wave.

**Independent Test**: With the binary built, `micro-dsql --listen 127.0.0.1:<port>` plus the runner
is green (or every failure is recorded under the known-gap rule).

**Acceptance Scenarios**:

1. **Given** a running micro-dsql, **When** the runner is started, **Then** layer 1 is green, or
   each failing scenario is in the skip list and in `docs/known-gaps.md` (constitution Principle V).
2. **Given** the CI workflow, **When** a commit is pushed, **Then** layer 1 runs against micro-dsql.

---

### Edge Cases

- Leftover tables from an interrupted earlier run on a persistent reference cluster: scenarios
  drop their own objects first; the runner drops the volatile "does not exist, skipping" notices.
- Volatile output (server source `LOCATION:` lines, timings, PIDs, OIDs, generated job ids):
  scenarios avoid printing it; the runner removes `LOCATION:` lines; job ids are only checked for
  shape, never printed.
- Two-session conflict where the engine reports `40001` at a different statement (at the write on
  Postgres/Turso, at `COMMIT` on DSQL): the scenario only checks that `40001` was reported by one
  session and that the winner's value is the one stored.
- Engines that block the second writer on a row lock: the conflict scenario orders the sessions so
  that no statement ever waits on a lock, and every wait has a timeout.
- A scenario name in the skip list that does not exist: reported as a warning, not an error.
- No `psql` on PATH: the runner fails fast with a clear message.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The harness MUST select its target only through the standard libpq variables
  `PGHOST`, `PGPORT`, `PGUSER` (`admin`), `PGDATABASE` (`postgres`), `PGPASSWORD` (any) and
  `PGSSLMODE=disable`.
- **FR-002**: The harness MUST provide the layer-1 scenarios exactly as docs/plan.md T1 lists them:
  `00_connect` (`SELECT 1`, `current_user`, `current_database()`, `SHOW server_version`,
  `SHOW timezone`), `10_crud` (CREATE TABLE with PK, `INSERT … RETURNING`, `SELECT` with
  `WHERE`/`ORDER BY`, `UPDATE`, `DELETE`, `DROP TABLE`), `20_txn` (`BEGIN`/`COMMIT`, `ROLLBACK`
  discarding writes, an error in a transaction followed by `25P02` until `ROLLBACK`), `30_errors`
  (`23505`, `42P01`, `42601`, with `ON_ERROR_STOP 0` and `VERBOSITY verbose`), `40_conflict` (two
  sessions update the same row inside transactions; one of them gets `40001`).
- **FR-003**: Each layer-1 scenario MUST have a stored expected output, and the runner MUST compare
  actual and expected output line by line and print a diff on mismatch.
- **FR-004**: The runner MUST use only POSIX shell and `psql` (invoked as `psql -X -q -f`), MUST
  exit nonzero when any non-skipped scenario fails, and MUST print a summary of pass/fail/skip
  counts.
- **FR-005**: The runner MUST honour a skip list (`tests/psql/known-gaps.txt`, one scenario name and
  a reason per line, `#` comments allowed) and report skipped scenarios with their reason.
- **FR-006**: Scenario output MUST be deterministic across runs and across conforming targets: no
  OIDs, PIDs, timings, generated ids or server source locations. The runner MUST document every
  normalisation it applies.
- **FR-007**: The harness MUST provide DSQL-rule probes covering at least: TRUNCATE, SAVEPOINT,
  CREATE TEMP TABLE, CREATE INDEX ASYNC, `SELECT * FROM sys.jobs`, DDL+DML in one transaction, two
  DDLs in one transaction, CREATE EXTENSION, a SERIAL column, SET of a non-whitelisted setting, and
  cases mined from MiniStack's test classes (denylist, transaction discipline, transaction limits,
  async index, DSQL settings, locking reads, column types, identity/sequences, ALTER TABLE subset).
- **FR-008**: Probe expected outputs MUST be captured from the reference emulator, and probe
  mismatches MUST NOT affect the runner's exit status.
- **FR-009**: A bring-up command MUST start the reference emulator in strict mode with Docker,
  create one cluster through the DSQL control-plane API, wait until it is ACTIVE, and print the
  connection variables for its endpoint. A matching teardown command MUST remove what it started.
- **FR-010**: A comparison command MUST run layer 1 and the probes against two targets, each given
  by its own set of connection variables, and write `reports/diff-<date>.md` listing each
  scenario/probe, each target's result, and the differing output lines. An unreachable target MUST
  be reported as such without aborting the report.
- **FR-011**: An optional CI workflow MUST be able to run the differential on a Linux runner with
  Docker, upload `reports/` as an artifact, and be allowed to fail.
- **FR-012** *(Reconcile after M1)*: Layer 1 MUST be green against micro-dsql locally and in CI, or
  each failure recorded under the known-gap rule.
- **FR-013** *(Reconcile after M1)*: The scenarios MUST also run through the M1 `tokio-postgres`
  harness so the extended protocol is covered.
- **FR-014** *(Reconcile after M1)*: The first differential report MUST exist and every probe
  mismatch MUST be listed as an M2/M3 backlog item; the README MUST have a 3-line quick start.

### Key Entities

- **Scenario**: a named psql script plus its expected output; blocking.
- **Probe**: a named psql script plus its reference output; report-only.
- **Target**: an endpoint described by a set of libpq connection variables.
- **Skip list entry**: scenario name plus reason; marks a known gap.
- **Differential report**: dated Markdown file with per-scenario, per-target results and diffs.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 5 of 5 layer-1 scenarios pass against the reference emulator.
- **SC-002**: 100% of probes match their stored reference outputs when run against the reference.
- **SC-003**: Two consecutive runs against the same target produce byte-identical outputs.
- **SC-004**: The reference comes up and prints connection variables with one command, in under
  3 minutes on a machine with the images already pulled.
- **SC-005**: A comparison run with one target down still produces a report within the normal run
  time and marks that target unreachable.
- **SC-006** *(after M1)*: 5 of 5 layer-1 scenarios pass against micro-dsql (or are recorded known
  gaps), and the first differential report lists every probe mismatch as a backlog item.

## Assumptions

Decisions taken without a human in the loop (decision; alternatives; reason):

- **Reference = MiniStack, run with `DSQL_STRICT=1`**; alternatives: plain `postgres:16`,
  Dreamescaper emulator; MiniStack is the docs/plan.md T1 reference and came up successfully.
- **MiniStack's default isolation is READ COMMITTED and it accepts other isolation levels**
  (measured: `SHOW transaction_isolation` = `read committed`, `BEGIN ISOLATION LEVEL READ
  COMMITTED` succeeds). The "other isolation levels are refused" check therefore lives in a probe,
  not in layer 1; alternatives: keep it in layer 1 (could never be green on the reference); reason:
  docs/plan.md says to fix the scenario when it can't be green on MiniStack.
- **`40_conflict` uses an explicit `BEGIN ISOLATION LEVEL REPEATABLE READ`** and a lock-free
  ordering (session B takes its snapshot, session A updates and commits, session B updates and
  commits); alternatives: overlapping writers (would block on Postgres-backed engines), default
  isolation (no conflict under READ COMMITTED); reason: deterministic on every engine model, and
  DSQL accepts the explicit RR form.
- **`20_txn` and `40_conflict` use `\set VERBOSITY sqlstate`** so that only SQLSTATEs (not engine
  message wording) are compared; `30_errors` uses `verbose` exactly as docs/plan.md says; alternatives:
  default verbosity (compares message text, which differs between engines); reason: the T1 bar is
  "SQLSTATEs like DSQL", not identical messages.
- **The runner deletes `LOCATION:` lines and "does not exist, skipping" notices** before diffing;
  alternatives: avoid verbose mode entirely (contradicts docs/plan.md); reason: those lines name server
  source files or depend on leftover state.
- **Expected outputs are recorded with a psql 16 client**, the version shipped by CI Linux runners;
  alternatives: newer clients; reason: psql's own formatting can change between major versions.
- **AWS CLI fallback**: the bring-up script uses `aws` when it is on PATH and otherwise runs the
  official AWS CLI v2 container in MiniStack's network namespace; alternatives: raw HTTP, boto3;
  reason: keeps the documented CLI and needs nothing but Docker.
- **Probes are separate files under `tests/psql/probes/`, one rule family per file**, with their
  own expected outputs and objects named with a probe-specific prefix; reason: a mismatch maps to one
  backlog item.
- **MiniStack's own test suite is mined, not run** (docs/plan.md).
- **`00_connect` checks that `current_user` is present, not that it equals `admin`**: MiniStack maps
  every login to its backend superuser `postgres`; the exact role (and `is_superuser`) is compared
  in probe `12_session_identity` instead; alternatives: assert `admin` (never green on the
  reference); reason: docs/plan.md "fix the scenario if it can't be green on MiniStack". Logging in as
  `admin` with any password is still proven by every scenario's connection.
- **`server_version` is checked as `16.%`** in `00_connect` (exact value in probe
  `12_session_identity`); reason: DSQL, MiniStack (16.15) and micro-dsql report different minors.
- **`30_errors` keeps the full verbose error fields** (`SCHEMA NAME`, `TABLE NAME`,
  `CONSTRAINT NAME` for `23505`) as captured from the reference; reason: tests are never loosened
  (constitution IV); a micro-dsql mismatch is handled under the known-gap rule in Reconcile.
- **Probe `05_sys_jobs` describes `sys.jobs` with `\gdesc`** and discards its rows; alternatives:
  `LIMIT 0` / `WHERE false` (MiniStack's emulation ignores them and prints volatile rows);
  reason: determinism.
- **No `COPY … FROM STDIN` in probes**: with `psql -f` it reads the rest of the file as data;
  `COPY … TO STDOUT` is probed instead.
- **Validation clients**: the harness was validated against MiniStack with psql 16 (Debian `dash`
  and Alpine `busybox sh`) and with psql 18; all three produce identical output.
- **Ports**: the reference uses 4566 and 25432–25440 only; port 5432 is never used by this feature.

Reconcile (after M1) decisions:

- **Differential run topology**: both targets were driven by the same psql 16 client running in
  MiniStack's network namespace, with micro-dsql listening on all interfaces of the host and
  reached through the container runtime's host alias; alternatives: run each side with its own
  psql and merge the outputs, or compare against the stored MiniStack expected outputs only;
  reason: one client version for both columns keeps psql formatting out of the diff, and the
  committed scripts stay unchanged (the workaround lives only in the local shell).
- **`DEALLOCATE` is answered with its command tag without touching the statement store**;
  alternatives: remove the named statement and report `26000` for unknown names (needs the
  statement store in the simple-query path, which pgwire's handler bounds do not expose);
  reason: psycopg only needs `DEALLOCATE ALL` to succeed; the difference is in `docs/known-gaps.md`.
- **CI layer-1 and differential wiring is kept as M1 delivered it** (it already runs layer 1
  against micro-dsql on macOS and Linux as a blocking step, and uses micro-dsql as target A in the
  allowed-to-fail differential job); alternatives: a separate layer-1 job; reason: no gap found.
- **README quick start kept as M1 wrote it** (three lines: start, connect with psql, done).
- **Probe mismatches are recorded, not fixed** (docs/plan.md T1: "fidelity gaps found here are
  recorded, not fixed"); the `DEALLOCATE` fix is the exception because it broke a mainstream
  driver outright.

## Reconcile results (after M1)

Run on 2026-11-12 against micro-dsql built from this branch (debug build, `--memory`).

### Layer 1

| Scenario | micro-dsql (psql 18 client) | micro-dsql (psql 16 client) | MiniStack (psql 16 client) | A/B output |
|---|---|---|---|---|
| 00_connect | PASS | PASS | PASS | identical |
| 10_crud | PASS | PASS | PASS | identical |
| 20_txn | PASS | PASS | PASS | identical |
| 30_errors | PASS | PASS | PASS | identical |
| 40_conflict | PASS | PASS | PASS | identical |

No layer-1 diff between micro-dsql and MiniStack; `tests/psql/known-gaps.txt` stays empty. The same
five scenarios also pass through the tokio-postgres harness (`crates/server/tests/smoke/t1_layer1.rs`) over both
the simple and the extended protocol, with the same values asserted for both. Start-up to first
accepted TCP connection: about 20 ms (the first launch of a freshly linked binary on macOS took
about 2 s, an OS first-run check, not micro-dsql).

### Differential report

`reports/diff-2026-11-12.md` (gitignored; regenerate with `scripts/compare.sh`): scenarios 5 of 5
identical; probes 0 of 22 match on micro-dsql, 22 of 22 match on MiniStack.

## Follow-ups / M2–M3 backlog

One item per probe mismatch (`tests/psql/probes/`). "MiniStack" is the reference column; where
MiniStack itself differs from real DSQL (tests/psql/README.md) the target is real DSQL.

| # | Probe | MiniStack behaviour | micro-dsql behaviour | Target |
|---|---|---|---|---|
| 1 | 01_truncate | `TRUNCATE` refused, `0A000 TRUNCATE is not supported`; rows kept | accepted, rows removed | M2 |
| 2 | 02_savepoint | `SAVEPOINT` refused (`0A000`), transaction aborted (`25P02`), `ROLLBACK TO` gives `3B001` | savepoints work | M2 |
| 3 | 03_temp_table | TEMP/TEMPORARY/UNLOGGED tables refused with `0A000 temporary/unlogged tables is not supported`; TEMP VIEW accepted (MiniStack only; DSQL refuses); `DROP TABLE IF EXISTS a, b, c` accepted | TEMP refused with a different message; UNLOGGED accepted; TEMP VIEW refused; `DROP TABLE` of several objects refused (`0A000`) | M2 (deny rule + message); multi-object DROP M3 |
| 4 | 04_create_index_async | plain `CREATE INDEX` refused (`0A000 unsupported mode. please use CREATE INDEX ASYNC.`); `CREATE [UNIQUE] INDEX ASYNC` returns a 26-char job id, `sys.wait_for_job` returns `t`; PK index named `<table>_pkey` | plain `CREATE INDEX` accepted; `ASYNC` is a syntax error (`42601`); PK index named `__turso_internal_pk_index_<table>` | M3 (ASYNC, jobs, index naming); M2 (refuse synchronous CREATE INDEX) |
| 5 | 05_sys_jobs | `sys.jobs` with 9 columns, completed `INDEX_BUILD` job, malformed job id gives `f`; `sys.dsql_major_version()` missing (MiniStack only; DSQL returns 1) | schema `sys` missing (`42P01`/`3F000`) | M3 |
| 6 | 06_ddl_dml_one_txn | DDL+DML in one transaction refused, `25006 DDL and DML statements cannot be mixed in a transaction`, both orders; `to_regclass` works | both orders accepted; `to_regclass` missing (`42883`) | M2 (rule); `to_regclass` M3 |
| 7 | 07_two_ddl_one_txn | second DDL refused, `25006 only one DDL statement can be run in a transaction` | accepted | M2 |
| 8 | 08_create_extension | `CREATE EXTENSION` refused with `0A000`; `DROP EXTENSION IF EXISTS` is a silent no-op | refused with `42601`; `DROP EXTENSION IF EXISTS` refused | M2 (error shaping) |
| 9 | 09_serial_column | `serial`/`bigserial`/`smallserial` refused, `0A000 type "serial" is not supported` | accepted | M2 |
| 10 | 10_set_not_whitelisted | non-whitelisted `SET`/`set_config` refused, `0A000 setting configuration parameter "<name>" not supported`; `lock_timeout`, `idle_in_transaction_session_timeout`, `enable_seqscan` (SET/SHOW/RESET) accepted | `42601 SET <name> is not supported`, `22023` for the timeouts, `42704` from `set_config`; `enable_seqscan` refused | M2 (SET whitelist) |
| 11 | 11_isolation_levels | default `read committed`, every level accepted (MiniStack only; DSQL is REPEATABLE READ only) | default `repeatable read`, other levels refused `0A000` (matches real DSQL); `SET default_transaction_isolation` refused `42601`; `SET SESSION CHARACTERISTICS … REPEATABLE READ` refused `42601` (driver re-test) | M2 (accept the RR forms of SET); levels: no change |
| 12 | 12_session_identity | `is_superuser` `on`, `server_version` 16.15, `datcollate` `en_US.utf8`, `CREATE DATABASE` refused `0A000 CREATE DATABASE is not supported` | `is_superuser` `off` (matches DSQL), 16.13, `en_US.UTF-8` (DSQL: `C`), `CREATE DATABASE` refused `42601 CREATEDB is not supported` | M2 (error shaping, `datcollate` `C`) |
| 13 | 20_denylist | every denied statement gives `0A000 <X> is not supported`; `CREATE FUNCTION … LANGUAGE sql` accepted and callable, plpgsql/C/procedures/triggers refused; `COPY … TO STDOUT` refused; `CREATE TYPE` (enum and composite) and `CREATE DOMAIN` refused; `pg_advisory_lock` callable; `DECLARE` outside a transaction `25P01`; `PREPARE TRANSACTION` `0A000`; `DROP … IF EXISTS` of unsupported objects silent | mostly `42601` with engine wording; every `CREATE FUNCTION` refused; `COPY TO STDOUT`, `CREATE TYPE … ENUM` and `CREATE DOMAIN` accepted; `pg_advisory_lock` missing (`42883`); `LOCK TABLE` gives a different message; `DROP FUNCTION/PROCEDURE/TYPE IF EXISTS` fail `42601 DROP: unexpected object format` | M2 (deny rules, `0A000` shaping, IF EXISTS no-ops); SQL functions M3/upstream |
| 14 | 21_column_types | `money`, `xml`, `inet`, `int[]`, `point`, `bit(8)` refused, `0A000 type "<t>" is not supported`; `pg_attribute` + `format_type` list a table's columns (bare `numeric` stays `numeric` on MiniStack; DSQL reports `numeric(18,6)`) | those types accepted (`bit(8)` refused with another message); `pg_attribute` returns no rows for the table | M2 (type deny rules, numeric rewrite); M3 (`pg_attribute`/`format_type` catalog) |
| 15 | 22_identity_sequences | `bigint GENERATED … AS IDENTITY (CACHE 65536 / CACHE 1)` works; non-bigint identity refused `0A000 identity columns are only supported on bigint columns`; `CACHE 100` refused `CACHE must be 1 or at least 65536` | every identity column refused with a misleading `0A000 WITH (storage parameters) / USING INDEX TABLESPACE …`; sequence `CACHE 65536` refused | M3 |
| 16 | 23_dsql_settings | `dsql.enable_batched_nestloop` shows `on`, settable, non-boolean refused `22023`; unknown `dsql.*` refused `42602 … "dsql" is a reserved prefix`; `SET enable_hashjoin`, `RESET default_statistics_target` accepted | `dsql.*` unknown (`42704`) and `set_config('dsql.bogus', …)` accepted; `enable_hashjoin`/`RESET` refused `42601` | M2 |
| 17 | 24_txn_discipline | two `DROP TABLE` in one transaction refused (`25006`); scalar-subquery `count(*)` typed `int8` | accepted; scalar-subquery counts described as `text` (B3) | M2 (rule); typing M2/upstream |
| 18 | 25_txn_row_limit | 3,001 rows in one `VALUES` list and a 4,000-row cumulative transaction refused `25006 transaction exceeds the 3,000 row limit` (MiniStack counts only `VALUES` rows; DSQL counts every mutated row and uses `54000`) | no limit | M2 (count changed rows, DSQL's `54000`) |
| 19 | 26_locking_reads | `FOR UPDATE` and `FOR KEY SHARE` accepted (also inside transactions); `FOR SHARE`/`FOR NO KEY UPDATE` refused `0A000 locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported` | every locking clause refused `0A000`; inside a transaction the refusal aborts it | M3 (accept as plain reads); M4 (`FOR UPDATE` as a write-set touch) |
| 20 | 27_alter_table | `ADD COLUMN` with a default/NOT NULL refused `0A000 … with constraint not supported`; `ALTER COLUMN TYPE`, `SET NOT NULL` refused `0A000`; `SET/DROP DEFAULT` accepted; `ADD CONSTRAINT … NOT VALID` + `ALTER TABLE ASYNC … VALIDATE CONSTRAINT` (job) accepted; FK added and enforced (`23503`); `DROP CONSTRAINT` accepted; dropping the PK column `0A000` | `ADD COLUMN … DEFAULT 0` accepted, NOT NULL gives `42000`; most refusals `42601`; `SET/DROP DEFAULT`, `ADD/DROP CONSTRAINT` refused; `ALTER TABLE ASYNC` syntax error; `pg_attribute` empty | M2 (error codes, refuse ADD COLUMN with constraint); M3 (NOT VALID, VALIDATE ASYNC, SET/DROP DEFAULT, ADD/DROP CONSTRAINT) |
| 21 | 28_index_forms | `CONCURRENTLY` refused `0A000`; `USING hash` refused; more than 8 key columns `54011`; ASYNC 8-column, expression, partial and `INCLUDE` indexes built; volatile predicate `42P17` | `CREATE INDEX CONCURRENTLY` accepted; every `ASYNC` form a syntax error | M2 (CONCURRENTLY/USING/8-column rules); M3 (ASYNC forms) |
| 22 | 29_txn_abort | a refused statement (`TRUNCATE`) aborts the transaction: next statement `25P02`, `ROLLBACK` keeps the table unchanged | `TRUNCATE` accepted, transaction continues | M2 (follows from the TRUNCATE rule; refusals must abort the block) |

### Driver re-test (M0 spike scripts against micro-dsql)

The M0 smoke scripts (`research/spike/`) were run unchanged against fresh micro-dsql instances (one per
group). Every step that is not an intentional error passes; the only `ERR` lines are the
deliberate duplicate-key steps (`23505`).

| Driver | M0 (upstream `tursopg`) failures | micro-dsql failures | Remaining notes |
|---|---|---|---|
| psql | 0 | 0 | |
| psycopg 3 (binary + text) | 13 + 4 | 1 + 1 (the dup-key step) | `timestamptz` microseconds truncated to milliseconds (B7); `DEALLOCATE ALL` fixed in this phase |
| asyncpg | 7 | 1 (dup-key step) | |
| node-pg | 1 | 1 (dup-key step) | |
| pgx (extended + simple) | 13 | 2 (dup-key steps) | |
| wire probe | 1 error | 0 | Terminate closes the connection |
| OCC scripts | — | all errors are the expected `40001`/`25P02`/refused isolation levels | `SET SESSION CHARACTERISTICS … REPEATABLE READ` refused (backlog #11) |
| app schema/queries/probes | 39 errors | 40 errors | engine gaps already in `docs/known-gaps.md` (B6–B8, B12, B14) |

JDBC was not re-tested (no smoke script in `research/spike/`).

## T1 exit checklist (docs/plan.md §4 T1)

- [x] Layer 1 is green against micro-dsql locally (5/5, psql 16 and 18 clients, two consecutive
  runs). **In CI: not verified** — `.github/workflows/ci.yml` runs it as a blocking step on
  `ubuntu-latest` and `macos-latest`, but the workflow was not run in this phase.
- [x] Layer 1 is also green against MiniStack (5/5, output identical to micro-dsql's).
- [x] A first differential report exists (`reports/diff-2026-11-12.md`, local only) and every
  probe mismatch is listed above as an M2/M3/M4 backlog item (22 items). The differential CI job
  (`.github/workflows/differential.yml`, micro-dsql as target A, allowed to fail) was not run.
- [x] The README has a 3-line quick start: start the binary, connect with `psql`, done.
- [x] Any password works: arbitrary passwords and a token-shaped password (the format
  `aws dsql generate-db-connect-admin-auth-token` prints) were accepted. **Not verified with a real
  AWS-generated token** (no AWS CLI on the test machine); micro-dsql never inspects the password.
