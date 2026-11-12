# Feature Specification: M0 spike — does Turso's Postgres frontend hold up?

**Feature Branch**: `001-m0-spike`

**Created**: 2026-11-12

**Status**: Draft

**Input**: User description: "M0 spike for micro-dsql (PLAN.md §4 M0). Validate by running, not by reading code, whether Turso's Postgres frontend holds up as the engine for a DSQL emulator: build it, serve SELECT 1 over psql, run a representative app schema and ~15 queries, smoke-test five drivers, run four two-session OCC experiments, and write research/06-spike-findings.md with a go/no-go verdict."

## User Scenarios & Testing *(mandatory)*

The "user" of this spike is the micro-dsql maintainer who must decide whether M1 is built on Turso, and
later contributors who need to re-run the evidence at a newer Turso commit.

### User Story 1 - Build and connect (Priority: P1)

The maintainer builds the upstream Postgres server from a local Turso clone at a recorded commit, starts
it on a local port, and connects with a stock Postgres client using `sslmode=disable` and any password.

**Why this priority**: If this fails, nothing else can be measured; it is the hard-stop condition of the
whole run.

**Independent Test**: Build, start the server, run `SELECT 1` from psql; observe one row with `1`.

**Acceptance Scenarios**:

1. **Given** a local Turso clone at a recorded commit, **When** the release build is run, **Then** it
   succeeds and the build notes (toolchain, native dependencies, duration, relevant warnings) are recorded.
2. **Given** the server is listening on 127.0.0.1:5432, **When** psql connects as `admin` to `postgres`
   with `sslmode=disable` and an arbitrary password, **Then** `SELECT 1` returns `1`.

---

### User Story 2 - Representative application SQL (Priority: P1)

The maintainer runs an app-style schema migration and about 15 representative queries and gets a
per-statement pass/fail record.

**Why this priority**: PLAN.md says the app's own SQL "decides more than any generic test". No target app
exists, so a representative stand-in is used.

**Independent Test**: Run the app SQL script against the server; every statement's outcome (rows or
error with SQLSTATE) is captured in a results file.

**Acceptance Scenarios**:

1. **Given** an empty database, **When** the schema script runs, **Then** each DDL statement's success or
   SQLSTATE is recorded.
2. **Given** the schema and seed data, **When** the ~15 queries run (joins, aggregates, ORDER BY/LIMIT,
   upsert, RETURNING, CTE, window function, JSONB access, timestamptz/uuid/numeric round-trips), **Then**
   each result is recorded and compared to what PostgreSQL 16 would return.

---

### User Story 3 - Driver smoke matrix (Priority: P1)

The maintainer runs the same small smoke scenario through psql, psycopg 3, asyncpg, node-pg and pgx.

**Why this priority**: Go criterion "your drivers work"; binary parameter handling is a known risk.

**Independent Test**: Each driver script exits 0 on success, non-zero with the error and SQLSTATE on
failure, and prints one line per step.

**Acceptance Scenarios**:

1. **Given** the server is running, **When** a driver script connects with an arbitrary password, does
   CREATE/INSERT/SELECT/UPDATE/DELETE, one explicit transaction, and one parameterised query, **Then** each
   step's outcome is printed and summarised in a matrix.

---

### User Story 4 - OCC experiments (Priority: P1)

The maintainer runs four reproducible two-session conflict scenarios and records which statement fails,
with what SQLSTATE and message, and whether any crash, hang or corruption occurs.

**Why this priority**: Go criterion "conflicts surface as 40001 or 55P03 with no crashes or corruption";
the main reason to pick Turso is its optimistic concurrency.

**Independent Test**: Each scenario script interleaves two sessions deterministically, prints the
statement-level outcome of both sessions, and checks the final table state.

**Acceptance Scenarios**:

1. **Given** a row updated by session A inside an open transaction, **When** session B updates the same
   row, **Then** the failing statement (B's UPDATE or a COMMIT) and its SQLSTATE are recorded.
2. **Given** two transactions inserting the same new primary key, **When** both commit, **Then** exactly one
   succeeds and the failure point is recorded.
3. **Given** the classic write-skew setup, **When** both transactions commit, **Then** whether both commit
   (snapshot isolation) is recorded.
4. **Given** many concurrent autocommit writers, **When** they run, **Then** the count and SQLSTATEs of
   failures (expected `55P03`) are recorded.
5. **Given** a session, **When** BEGIN is issued with each isolation level, **Then** which levels are
   accepted is recorded.

---

### User Story 5 - Findings and verdict (Priority: P1)

The maintainer reads one document with the tested commit, toolchain, results tables, a verdict on every
inferred research claim, and a go/no-go verdict with a blocker list.

**Why this priority**: It is the milestone deliverable that PLAN.md mandates.

**Independent Test**: `research/06-spike-findings.md` exists and every `[I]` item in research/02 and 04 has
a CONFIRMED / REFUTED / NOT TESTED line with evidence.

**Acceptance Scenarios**:

1. **Given** the experiments above, **When** the findings document is written, **Then** it ends with a
   GO / NO-GO / CONDITIONAL verdict against the PLAN.md M0 criteria and a blocker list with rough effort.

### Edge Cases

- The server crashes or a connection drops mid-scenario: recorded as a finding (crash), scripts must not
  hang forever (statement/connection timeouts).
- A driver cannot even connect (e.g. a startup parameter or auth message it rejects): recorded as a
  driver-level failure, the rest of the matrix continues.
- A scenario hangs (lock wait with no timeout): the script times out and records "hang".
- A statement in the app SQL fails: the script continues with the next statement (no stop-on-error).

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The spike MUST record the Turso commit tested, the toolchain used, native dependencies needed,
  the build duration and any build warnings that matter.
- **FR-002**: The spike MUST demonstrate (or refute) `SELECT 1` over psql with `sslmode=disable`, user
  `admin`, database `postgres`, and an arbitrary password.
- **FR-003**: The spike MUST provide an app-style schema covering PK, FK, text, int, bigint, numeric, bool,
  timestamptz, uuid and jsonb columns plus secondary indexes, and at least 15 queries covering joins,
  aggregates, ORDER BY/LIMIT, ON CONFLICT upsert, RETURNING, a CTE and a window function.
- **FR-004**: The spike MUST provide one smoke script per driver (psql, psycopg 3, asyncpg, node-pg, pgx),
  each covering connect, CRUD, one explicit transaction and at least one parameterised query.
- **FR-005**: The spike MUST provide reproducible two-session scripts for same-row UPDATE, same-PK INSERT,
  write skew and concurrent autocommit writes, plus an isolation-level acceptance probe.
- **FR-006**: All scripts MUST take connection settings from the standard `PG*` environment variables and
  MUST NOT contain machine-specific paths or hostnames.
- **FR-007**: Local dependency directories (virtualenvs, node_modules, build outputs) MUST be gitignored.
- **FR-008**: `research/06-spike-findings.md` MUST contain results tables, a CONFIRMED / REFUTED / NOT TESTED
  line with evidence for every `[I]` item of research/02 and research/04, a go/no-go verdict against PLAN.md
  M0 criteria, and the blocker list with rough effort.
- **FR-009**: The spike MUST NOT modify the Turso clone (read and build only) and MUST NOT edit existing
  files under `research/`.
- **FR-010**: Whatever the verdict, no-go reasons MUST be listed as known gaps for M1; the embedded-PG16
  pivot is not started.

### Key Entities

- **Scenario result**: scenario name, step, statement, outcome (ok / error), SQLSTATE, message, notes.
- **Driver matrix row**: driver and version, connect, CRUD, transaction, parameterised query, notes.
- **Inferred claim verdict**: source file and claim, verdict, evidence.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A maintainer can re-run every experiment from a clean checkout with one documented command per
  experiment group, in under 10 minutes excluding the engine build.
- **SC-002**: 100% of the `[I]` items in research/02 and research/04 have a verdict line in the findings.
- **SC-003**: 5 of 5 drivers have a recorded row in the driver matrix (pass, partial or fail).
- **SC-004**: 4 of 4 OCC scenarios have a recorded failing statement and SQLSTATE (or an explicit "no
  conflict" outcome), with crash/hang/corruption noted.
- **SC-005**: The findings end with exactly one verdict (GO / NO-GO / CONDITIONAL) traceable to the M0
  criteria.

## Assumptions

- No target application exists; a representative app-style schema and ~15 queries stand in for "your app's
  SQL" (decision; alternative: pick an open-source app's migrations; reason: faster and covers the listed
  feature mix deliberately).
- The upstream `tursopg` binary is used as the server for the spike (PLAN.md M0); our own binary is M1.
- Build toolchain: Rust stable is used if the pinned 1.88 toolchain cannot be installed (PLAN.md §2 says
  "build with stable first"); recorded as a finding.
- Driver set is psql, psycopg 3, asyncpg, node-pg and pgx (decision; JDBC and sqlx are left out because no
  JDK is assumed and sqlx is close to tokio-postgres covered in M1).
- Python drivers are installed into a gitignored virtualenv in the worktree, node-pg via a local
  `package.json`, Go via the system package manager.
- OCC "concurrent autocommit writes" is run with N parallel connections each doing autocommit INSERT/UPDATE
  in a loop (decision; reason: the exclusive-lock path only collides under real parallelism).
- No Rust crate exists in this repo yet, so the cargo gates of Constitution Principle IV do not apply to
  this feature; the gate here is "scripts run and produce recorded results".
- The verdict does not change the plan: the run continues on Turso and no-go reasons become M1 known gaps.
