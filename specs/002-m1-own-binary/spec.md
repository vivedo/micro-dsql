# Feature Specification: M1 — own binary that starts and runs

**Feature Branch**: `002-m1-own-binary`

**Created**: 2026-11-12

**Status**: Draft

**Input**: User description: "M1 'own binary that starts and runs' (PLAN.md §4 M1 items 1–6): own
workspace and binary built on the pinned Turso Postgres frontend with a vendored wire server; CLI
`micro-dsql [--listen 127.0.0.1:5432] [--data <file> | --memory (default)] [--log info]`; graceful
shutdown; ready in under 1 s; statements off the async runtime; any user/password; SSL refused;
DSQL-like startup parameters, `SHOW server_version`/`SHOW timezone`, `current_database()` =
`postgres`, close on Terminate (M0 blockers B9, B4); stretch B1, B2, B3, B5; KNOWN_GAPS.md;
integration tests with a Rust Postgres driver; CI on macOS and Linux; README quick start."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Start the emulator with one command and connect with psql (Priority: P1)

A developer (or a CI job) runs one command and gets a Postgres-speaking endpoint that looks like a
single Aurora DSQL cluster's database. They connect with `psql` using `sslmode=disable`, user
`admin`, database `postgres` and any password, and run CRUD statements and transactions.

**Why this priority**: This is the milestone's exit criterion and the base for every later
milestone (T1, M2–M4). Without it nothing else can be tested.

**Independent Test**: Start the binary on a free port, connect with `psql`, run
`SELECT 1`, create a table, insert/select/update/delete, run `BEGIN`/`COMMIT`/`ROLLBACK`.

**Acceptance Scenarios**:

1. **Given** no running server, **When** the developer runs the binary with no flags, **Then** it
   listens on `127.0.0.1:5432` with a fresh in-memory database and logs the bound address within 1 s.
2. **Given** `--listen 127.0.0.1:0`, **When** the server starts, **Then** it logs the actual port
   chosen by the operating system.
3. **Given** a running server, **When** a client connects with any user name and any password (or
   none) and `sslmode=disable`, **Then** the connection succeeds; a client that asks for SSL is told
   "no" and may continue in plain text.
4. **Given** `--data <file>`, **When** data is written, the server stopped and restarted on the same
   file, **Then** the data is still there.
5. **Given** a running server, **When** it receives SIGINT or SIGTERM, **Then** it stops accepting
   connections, logs the shutdown and exits with status 0.

---

### User Story 2 - The session looks like DSQL to a client (Priority: P1)

Client libraries and test code inspect the server: startup parameters (`server_version`,
`TimeZone`, `is_superuser`, …), `SHOW server_version`, `SHOW timezone` and `current_database()`.
They must see DSQL-shaped values.

**Why this priority**: T1 scenario `00_connect` checks exactly these values, and drivers gate
behaviour on `server_version`.

**Independent Test**: Connect, read the startup parameters reported by the driver, run the SHOW
statements and `SELECT current_database()`.

**Acceptance Scenarios**:

1. **Given** a new connection, **When** the driver reads the startup parameters, **Then**
   `server_version` is a plain `16.x` version, `TimeZone` is `UTC`, `is_superuser` is `off`,
   `server_encoding`/`client_encoding` are `UTF8`, `DateStyle` is `ISO, MDY`,
   `standard_conforming_strings` and `integer_datetimes` are `on`.
2. **Given** a connection, **When** it runs `SHOW server_version`, **Then** it gets one row whose
   value starts with `16.`; `SHOW timezone` returns `UTC` in a column named `TimeZone`.
3. **Given** a connection, **When** it runs `SELECT current_database()`, **Then** it gets `postgres`.
4. **Given** a connection, **When** the client sends Terminate, **Then** the server closes the
   connection (a client waiting for the close returns promptly).

---

### User Story 3 - Errors come back as Postgres errors (Priority: P1)

Test code relies on SQLSTATEs: duplicate keys, unknown tables, syntax errors, statements in an
aborted transaction. Errors must arrive as Postgres error responses, never as dropped connections.

**Why this priority**: T1 scenarios `20_txn` and `30_errors` check SQLSTATEs.

**Independent Test**: Trigger each error and read the SQLSTATE; then run another statement on the
same connection.

**Acceptance Scenarios**:

1. **Given** a table with a primary key, **When** a duplicate is inserted, **Then** the error has
   SQLSTATE `23505` and the connection stays usable.
2. **Given** a connection, **When** it selects from an unknown table, **Then** SQLSTATE `42P01`;
   a syntax error gives `42601`.
3. **Given** an open transaction, **When** a statement fails, **Then** later statements get `25P02`
   until `ROLLBACK`.
4. **Given** two sessions updating the same row in transactions, **Then** one of them gets `40001`.

---

### User Story 4 - Drivers using the extended protocol work (Priority: P2, stretch)

Drivers such as asyncpg, pgx, psycopg 3 and the Rust `tokio-postgres` use the extended protocol:
they ask the server how many parameters a statement has and of which types, often send parameters
in binary, and read typed results.

**Why this priority**: M0 found these fail (B1–B3). They are stretch goals for M1; what is not
fixed is recorded as a known gap.

**Independent Test**: A Rust integration test prepares `SELECT $1::int8 + 1`, binds a binary
parameter and reads a typed result; and reads `count(*)` as a 64-bit integer.

**Acceptance Scenarios**:

1. **Given** a statement with `$1..$n`, **When** the client describes it, **Then** the server
   reports `n` parameters with plausible types (explicit casts and column context honoured; text
   otherwise).
2. **Given** parameters sent in binary for common types (int2/4/8, float4/8, bool, text, bytea,
   uuid, json/jsonb, date, timestamp/timestamptz, numeric), **When** executed, **Then** they are
   decoded correctly.
3. **Given** `count(*)`, **When** described, **Then** the result type is `int8`.
4. **Given** a foreign key, **When** a row referencing a missing parent is inserted, **Then** SQLSTATE
   `23503` (B5).

---

### User Story 5 - Contributors get fast, gated feedback (Priority: P2)

A contributor runs `cargo fmt --check`, `cargo clippy`, `cargo test` locally, and the same checks
plus the T1 psql scenarios run in CI on macOS and Linux.

**Independent Test**: Run the three gates locally; inspect the workflow file.

**Acceptance Scenarios**:

1. **Given** a checkout, **When** the gates run, **Then** they pass (or failing tests are skipped
   as recorded known gaps).
2. **Given** a push, **When** CI runs, **Then** it builds, runs the gates on both OSes and runs
   `tests/psql/run.sh` against a started binary.

### Edge Cases

- The listen address is in use or invalid: the binary exits non-zero with a clear message.
- `--data` and `--memory` given together: rejected as a usage error.
- `--data` points to a file in an unwritable location: exits non-zero with the engine's error.
- A client disconnects in the middle of a transaction: the transaction is rolled back and the
  session's resources are released.
- A long-running statement on one connection does not stall other connections.
- `SHOW` of a DSQL-reported parameter inside an aborted transaction gives `25P02`, like any other
  statement.
- Shutdown while clients are connected: the process still exits promptly.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The project MUST build a single executable `micro-dsql` from its own workspace,
  consuming the Turso Postgres frontend and engine pinned to commit
  `d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5`, with a committed lock file.
- **FR-002**: The wire server MUST be a vendored, adapted copy of the upstream server file that
  keeps the MIT notice and records the upstream commit; the Turso licence text MUST be in the repo.
- **FR-003**: The CLI MUST accept `--listen <addr:port>` (default `127.0.0.1:5432`), `--data <file>`
  or `--memory` (default, mutually exclusive) and `--log <level/filter>` (default `info`).
- **FR-004**: The server MUST log the actual bound address (including when port 0 is requested) and
  be ready to accept connections within 1 s of launch.
- **FR-005**: The server MUST shut down gracefully on SIGINT and SIGTERM, exiting with status 0.
- **FR-006**: SQL statements MUST be executed off the asynchronous network runtime so that one
  connection's statement never blocks the network handling of others.
- **FR-007**: Any user name and any password MUST be accepted without an authentication exchange;
  SSL requests MUST be refused so clients continue in plain text.
- **FR-008**: The server MUST report DSQL-like startup parameters (Story 2, scenario 1) and echo
  the client's `application_name` and user (as `session_authorization`).
- **FR-009**: `SHOW server_version`, `SHOW server_version_num`, `SHOW timezone`, `SHOW is_superuser`,
  `SHOW server_encoding`, `SHOW integer_datetimes`, `SHOW transaction_isolation` and
  `SHOW default_transaction_isolation` MUST return DSQL-shaped values; `SET timezone` to a UTC
  spelling MUST be accepted as a no-op.
- **FR-010**: `current_database()` MUST return `postgres` for the default in-memory database.
- **FR-011**: The server MUST close the connection when the client sends Terminate.
- **FR-012** *(stretch)*: Describe MUST report the real parameter count and inferred types (B1);
  binary-format parameters MUST be decoded for common types (B2); result type OIDs MUST be correct
  for `count(*)`, `smallint`, `numeric` columns and binary results MUST be encoded correctly for
  uuid/timestamptz/numeric (B3); foreign keys MUST be enforced (B5).
- **FR-013**: The M0 blockers not fixed here MUST be recorded in `KNOWN_GAPS.md` with error,
  suspected cause and target milestone.
- **FR-014**: Integration tests MUST start the server on port 0 in-process and connect with a Rust
  Postgres driver: CRUD, explicit transaction, SHOW checks, SQLSTATE checks, and the T1 layer-1
  scenario statements over the simple query protocol.
- **FR-015**: A CI workflow MUST run fmt, clippy (warnings as errors) and tests on macOS and Linux,
  and run `tests/psql/run.sh` against a started binary.
- **FR-016**: The README MUST give short usage and a 3-line quick start.

### Key Entities

- **Server parameters**: the fixed DSQL-shaped name/value set reported at startup and via SHOW.
- **Session**: one client connection with its own engine session and transaction state.
- **Known gap**: a recorded deviation (error, suspected cause, milestone) with its skipped test.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: From launch to accepting the first connection takes under 1 s.
- **SC-002**: psql connects with `sslmode=disable` and an arbitrary password on the first attempt.
- **SC-003**: All non-known-gap integration tests pass on macOS and Linux; fmt and clippy are clean.
- **SC-004**: The T1 layer-1 scenarios `00_connect`, `10_crud`, `20_txn`, `30_errors`, `40_conflict`
  are run against the binary and each result is reported (pass or recorded gap).
- **SC-005**: Every M0 blocker B1–B14 is either fixed (with a test) or listed in `KNOWN_GAPS.md`.

## Assumptions

- **No authentication exchange** (AuthenticationOk right after startup) rather than a cleartext
  password request: clients that send a password are unaffected, clients without one (psql without
  `PGPASSWORD`) do not prompt. Alternative: request cleartext and ignore it (closer to DSQL's token
  flow, but makes psql prompt).
- **server_version = 16.13**, the value in the DSQL user guide example; MiniStack measured 16.15 and
  research/03 cites 16.4; any 16.x satisfies T1.
- **current_user stays `postgres`** (the engine's role); DSQL reports `admin`. Known gap (M2).
- **`current_database()` with `--data <file>`** reports the file's stem (engine behaviour); only the
  default in-memory database is guaranteed to say `postgres`. Known gap, documented.
- **SET whitelist** beyond `SET timezone` to a UTC spelling is M2 scope.
- **Statement execution off the runtime** uses the runtime's blocking thread pool per statement
  (a dedicated thread per connection was judged more code for the same effect).
- Upstream unit tests of the vendored file depend on Turso's internal test client and are not
  carried over; equivalent behaviour is covered by our own integration tests.
- Rust stable is used; no toolchain pin unless stable fails (PLAN.md §2).
