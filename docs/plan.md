# micro-dsql — PoC implementation plan

Written 2026-11-12. Builds on [`research/`](../research/README.md); read its executive summary first. This file turns the research into an ordered list of things to build. Where they disagree, **this file wins on scope**, and the research wins on facts.

## 1. Goal and scope

**Goal:** a single Rust binary that runs locally and in CI. Apps and tests connect to it with ordinary Postgres drivers, and it behaves like **one Aurora DSQL cluster's database** well enough for unit and integration tests.

**Decided:**

| Topic | Decision |
|---|---|
| Databases | **One** database (`postgres`) per process. No multi-cluster, no SNI routing. |
| TLS | **None** in the PoC. Clients set `sslmode=disable` locally. DSQL clients are plain PG drivers, so it's only a connection-string setting. |
| Auth | **Accept anything.** Any user, any password; SDK-generated IAM tokens are ignored. When Turso ships auth in its wire server, reuse its hook point for DSQL token checks. DSQL needs a *cleartext* password handler (the token *is* the password), so SCRAM won't map 1:1. |
| Control plane | **Out.** If a test needs `GetCluster` etc., run moto, MiniStack or the AWS emulator beside it. |
| Engine | Turso's Postgres frontend (`turso_pg` + `turso_core`, MVCC). |
| Repo shape | **Own small repo** (this folder). Do **not** fork Turso to host DSQL code. Depend on Turso crates as a **git dependency pinned to a commit**. |
| Wire server | **Vendor and adapt** `postgres/server/lib.rs` (about 930 lines, MIT) into our own crate. Its handler and factory are private and the file is small, so owning it is cheaper than a fork. Keep the MIT notice and record the upstream commit it came from. |
| Turso patches | Only when a hook is missing inside `turso_pg` / `turso_core` (e.g. `sys.*` registration, autocommit concurrency). Carry them as a minimal patch branch on a fork, wired in with `[patch]`. **No changes are proposed to Turso upstream** (owner decision, 2026-11-12). |
| Policy layer | **Backend-agnostic.** It works on the `pg_query` AST plus per-session state, never on Turso internals, so it survives a pivot to embedded PG16 (see research/05, "Pivot target"). |

**Non-goals for the PoC:**
- TLS and IAM validation.
- Control plane and multi-region.
- Exact OCC timing (Phase C is best effort).
- `information_schema` and ORM introspection beyond what the smoke tests need.
- DSQL-shaped EXPLAIN.
- Binary releases.

## 2. Prerequisites

To build:
- **Rust**, stable via rustup. Turso pins **1.88** in its `rust-toolchain.toml`, but that file doesn't apply to a git dependency. Build with stable first and pin 1.88 in our own `rust-toolchain.toml` only if stable fails.
- **clang/libclang**, needed by `pg_query`'s bindgen. On macOS the Xcode Command Line Tools provide it; on Debian/Ubuntu install `clang libclang-dev`.
- **protoc and cmake** may be needed by `pg_query`/prost and `aws-lc-sys`. Install them up front, or wait until the first build asks.

To test:
- **AWS CLI v2**, to create the MiniStack reference cluster and to generate a real DSQL token for the "any password" check (T1).
- **psql** (libpq) and the drivers for the M0 smoke matrix, depending on your targets: Python `psycopg`/`asyncpg`, Node `pg`, Go `pgx`, a JDK for JDBC.
- **Docker**, needed for the T1 differential run (not by micro-dsql itself). Used for the **reference side** of testing:
  - the MiniStack DSQL emulator (`ministackorg/ministack` with `DSQL_STRICT=1`), the T1 reference;
  - a `postgres:16` container to diff results against;
  - `ghcr.io/dreamescaper/dsql-emulator` to compare DSQL-rule behaviour and to run its golden probes against something known-good;
  - the embedded-PG16 pivot prototype, if M0 is a no-go.

For reading and spiking:
- **A local clone of `tursodatabase/turso`.** The research was done at `d5fdf4d` (2026-11-11). Use it for reading code and building `tursopg` in M0; it is not a dependency path.

## 3. Repo layout (target)

```
micro-dsql/
├── docs/plan.md                    # this file
├── README.md                  # usage, once there is something to run
├── research/                  # existing analysis (read-only reference)
├── Cargo.toml                 # workspace; turso crates = git dep, pinned rev
├── rust-toolchain.toml        # only if needed (see §2)
├── crates/
│   ├── server/                # vendored + adapted turso_pg_server: pgwire listener,
│   │                          #   startup params, per-connection executor thread
│   ├── policy/                # DSQL rules on the pg_query AST + per-txn state machine
│   │                          #   + error shaping (OC000/OC001, 54000/54011)
│   └── sys/                   # sys.jobs / sys.wait_for_job / sys.dsql_major_version (M3)
├── crates/micro-dsql/            # main.rs: clap flags, tracing, signal handling
├── scripts/
│   ├── ministack-up.sh        # start MiniStack (DSQL_STRICT=1) + create a reference cluster (T1)
│   └── compare.sh             # run tests/psql against micro-dsql and MiniStack, write a diff report (T1)
├── reports/                   # differential reports (gitignored, or uploaded as CI artifacts)
└── tests/
    ├── psql/                  # plain-psql scenarios + expected output; target-agnostic (T1)
    │   └── probes/            # DSQL-rule probes, expected to differ until M2/M3 (T1)
    ├── smoke/                 # tokio-postgres CRUD + txn tests against a spawned server
    ├── occ/                   # two-session conflict scenarios
    └── conformance/           # runner for Dreamescaper's golden probes (M3)
```

**Command-line surface (keep it tiny):**
```
micro-dsql [--listen 127.0.0.1:5432] [--data <file> | --memory (default)] [--log info]
```
A `:memory:` database by default gives a clean slate on every start. That suits CI.

**Workspace dependency sketch:**
```toml
[workspace.dependencies]
turso_core      = { git = "https://github.com/tursodatabase/turso", rev = "<pinned>", features = ["conn_raw_api"] }
turso_pg        = { git = "https://github.com/tursodatabase/turso", rev = "<pinned>" }
turso_pg_parser = { git = "https://github.com/tursodatabase/turso", rev = "<pinned>" }  # also on crates.io
pgwire          = "0.36"
pg_query        = "6.1"   # same version Turso uses
```
Before you write this, check `postgres/server/Cargo.toml` at the pinned rev for the exact features the server uses.

## 4. Milestones

Each milestone ends with something you can run and an exit check. Do them in order.

### M0 — Spike: does Turso hold up? (≈2–3 days; go/no-go gate)

This is research/05 Phase 0, cut down. **Everything in the research so far comes from reading code; M0 is the first time it runs.**

1. In a Turso clone: `cargo build -p tursopg --release`. Note any missing native deps.
2. `tursopg --server 127.0.0.1:5432`, then connect with `psql "host=127.0.0.1 port=5432 user=admin dbname=postgres sslmode=disable"`.
3. **Your own app's SQL first.** Run its schema migration and its 10–20 most important queries. That decides more than any generic test.
4. Driver smoke test (CRUD + an explicit transaction) for the drivers the target apps use. Candidates: psycopg, asyncpg, pgx, node-pg, JDBC, sqlx. Expect failures from **binary parameters** (asyncpg, pgx, Npgsql).
5. OCC experiments with two sessions:
   - same-row UPDATE;
   - same-PK INSERT;
   - write skew;
   - concurrent autocommit writes (expected `55P03`; see research/02 §3).
   - Record **which statement** fails and with what SQLSTATE.
6. Write the results to `research/06-spike-findings.md`. For each **[I]** item in research/02 and research/04, note whether it was confirmed or refuted.

**Go:**
- your app's SQL and your drivers work, or each blocker looks like about a week of fixes;
- conflicts surface as 40001 or 55P03 with no crashes or corruption.

**No-go:** fall back to the embedded-PG16 pivot in research/05.

### M1 — Own binary that starts and runs (≈3–5 days)

*This is the "starts and runs as expected" milestone.*

1. `git init`, create the workspace, and pin the Turso rev to the one M0 used.
2. Vendor `postgres/server/lib.rs` → `crates/server`, with an attribution header and the upstream commit hash.
3. Build our own binary. Don't use `tursopg`; it pulls in tantivy and rustyline.
   - clap flags, `tracing`, graceful shutdown on SIGINT/SIGTERM;
   - `:memory:` default, `--data` for a file.
4. **Run statements off the async runtime.** Upstream calls the engine synchronously inside async handlers (research/02 §2). Use a dedicated thread per connection, or `spawn_blocking`.
5. Startup parameters like DSQL's: `server_version=16.x`, `TimeZone=UTC`, `is_superuser=off`, and so on. Take the values from research/03 (MiniStack's measured set).
6. Integration-test harness: spawn the server on port 0, read back the bound port, connect with `tokio-postgres`. Add `cargo test` CI (GitHub Actions on macOS and Linux).

**Exit:**
- `cargo run -p micro-dsql` starts in under 1 s;
- psql and the M0 drivers connect with `sslmode=disable` and any password;
- the smoke suite is green in CI.

### T1 — Testing phase: PoC acceptance (≈2–3 days, right after M1)

**PoC done = M0 + M1 + T1.** M2–M4 add fidelity after that.

**The bar is deliberately low.** It is not a replica test. micro-dsql passes T1 if it:
- **starts**: one command, no Docker, ready in under 1 s;
- **works with plain `psql`**: connect, create a table, insert, select, update, delete, and run transactions;
- **looks vaguely like DSQL** to a client:
  - user `admin` and database `postgres` work;
  - any password is accepted, including a real token from `aws dsql generate-db-connect-admin-auth-token`;
  - `SHOW server_version` reports 16.x and `SHOW timezone` reports `UTC`;
  - errors come back with Postgres SQLSTATEs (not dropped connections);
  - `BEGIN ISOLATION LEVEL REPEATABLE READ` works, and other isolation levels are refused.

Fidelity gaps found here are **recorded, not fixed**. They become the backlog for M2/M3.

**Two layers:**

**1. Plain psql scenarios (blocking; run in CI on every commit)**
- `tests/psql/*.sql`, one file per scenario:
  - `00_connect.sql`: `SELECT 1`, `current_user`, `current_database()`, `SHOW server_version`, `SHOW timezone`.
  - `10_crud.sql`: `CREATE TABLE` with a PK, then `INSERT … RETURNING`, `SELECT` with `WHERE`/`ORDER BY`, `UPDATE`, `DELETE`, `DROP TABLE`.
  - `20_txn.sql`: `BEGIN`/`COMMIT`; `ROLLBACK` discarding writes; an error inside a transaction followed by `25P02` until `ROLLBACK`.
  - `30_errors.sql`: duplicate PK gives `23505`; unknown table gives `42P01`; syntax error gives `42601`. Run with `\set ON_ERROR_STOP 0` and `\set VERBOSITY verbose` so the SQLSTATE shows in the output.
  - `40_conflict.sh`: two psql sessions update the same row inside transactions, and one of them must get `40001`. This only checks that a conflict is reported; it doesn't check *when* (that's M4).
- `tests/psql/run.sh` takes the target from the standard `PGHOST`/`PGPORT`/`PGUSER`/`PGDATABASE`/`PGPASSWORD`/`PGSSLMODE=disable` variables. It runs each file with `psql -X -q -f` and compares the output with `tests/psql/expected/<name>.out`, the way PG's own regression tests do.
- The script uses only `psql` and POSIX shell, so it runs against **any** Postgres-speaking endpoint. That's what makes layer 2 possible.
- Also run the scenarios through the Rust harness from M1.6 (`tokio-postgres`), so the extended protocol is covered as well as the simple protocol psql uses.

**2. Differential run against MiniStack's DSQL emulator (report only; not blocking)**

MiniStack (MIT) emulates DSQL with a real `postgres:16-alpine` per cluster behind a DSQL-rules proxy. Treat it as the "what DSQL roughly looks like" reference: point the same psql scenarios at it and at micro-dsql, and diff.

- `scripts/ministack-up.sh`:
  - `docker run -d -p 4566:4566 -p 25432-25440:25432-25440 -v /var/run/docker.sock:/var/run/docker.sock -e DSQL_STRICT=1 ministackorg/ministack`.
  - `DSQL_STRICT=1` plus the Docker socket switches on its real data plane. Without them it only stubs the control plane.
  - Create a cluster with `aws dsql create-cluster --endpoint-url http://localhost:4566`, wait for `ACTIVE`, and read back the endpoint. Data-plane ports start at `25432` (`DSQL_BASE_PORT`).
  - Check the port publishing against MiniStack's README when you write the script. The backend containers it starts run beside it, not inside it.
- `scripts/compare.sh` runs `tests/psql/run.sh` against both targets and writes `reports/diff-<date>.md`: each scenario, each target, pass/fail, and the differing output lines.
- **Add DSQL-rule probe files** (`tests/psql/probes/*.sql`). They are expected to *differ* at the PoC stage; their only job is to measure the gap:
  - `TRUNCATE`, `SAVEPOINT`, `CREATE TEMP TABLE`, `CREATE INDEX ASYNC`, `SELECT * FROM sys.jobs`;
  - DDL and DML in one transaction, two DDLs in one transaction;
  - `CREATE EXTENSION`, a `SERIAL` column, `SET` of a non-whitelisted setting.
  - The MiniStack column of the report is the target behaviour. Each mismatch becomes an M2/M3 backlog item.
- **Mine MiniStack's own tests for cases, don't run them.** `tests/test_dsql.py` mostly tests MiniStack's Python internals and its own proxy, so it can't be pointed at micro-dsql. Its test classes (`TestDenylist`, `TestTxnDiscipline`, `TestTxnLimits`, `TestIndexAsync`, `TestDsqlSettings`, `TestLockingReads`, …) are a ready list of SQL cases to turn into probe files.
- CI: an optional GitHub Actions job (Linux runners have Docker) that runs the differential and uploads the report as an artifact. Allow it to fail.

**Exit (the PoC is done when):**
- [ ] Layer 1 is green against micro-dsql locally and in CI.
- [ ] Layer 1 is also green against MiniStack. This proves the scenarios are DSQL-shaped rather than Turso-shaped; fix the scenario if it isn't.
- [ ] A first differential report exists, and the probe mismatches are listed as M2/M3 backlog items.
- [ ] The README has a 3-line quick start: start the binary, connect with `psql`, done.

### M2 — DSQL error shape and the "would break in prod" rules (≈1 week)

These are the cheapest rules with the most value: they make a test fail locally when the same code would fail on real DSQL.

1. **Error shaping.**
   - Map conflict errors to SQLSTATE `40001` with DSQL's exact texts:
     - `change conflicts with another transaction (OC000)`
     - `schema has been updated by another transaction (OC001)`
   - The Rust connector parses these texts, so they must match exactly.
   - Map `55P03` from autocommit contention to 40001/OC000 for now. Fix it properly in M4.
2. **Per-transaction state machine** (`crates/policy`):
   - at most 1 DDL per transaction;
   - no DDL+DML in the same transaction;
   - 3,000 mutated rows, counted with `n_change()`;
   - 300 s transaction age;
   - SAVEPOINT rejected.
   - Error texts and codes are in research/01 §3.
3. **Statement deny rules** on the pg_query AST: TRUNCATE, temp tables, triggers, extensions, COPY, LISTEN/NOTIFY, advisory locks, partitions, savepoints, matviews, and disallowed column types. Use Dreamescaper's 45 rules and MiniStack's list as the spec (research/03). Prioritise the ones Turso *accepts* today; research/04 §C lists them.
4. **SET whitelist:** accept DSQL's allowed settings as no-ops that remember their value for SHOW. Reject the rest.
5. **Rewrite bare `numeric` to `numeric(18,6)`** in DDL.

**Exit:** policy unit tests, plus integration tests asserting the SQLSTATE and message for each rule.

### M3 — DDL that ORMs and migrations emit (≈1–2 weeks)

1. **`CREATE [UNIQUE] INDEX ASYNC`.**
   - Pre-parse rewrite: strip `ASYNC`, because libpg_query can't parse it.
   - Build the index synchronously and return a `job_id` row.
   - Emulate `sys.jobs` and `sys.wait_for_job`, plus `sys.dsql_major_version`. This **probably needs the first Turso patch**: `sys.*` registration goes through private `PostgresDialect` modules (research/02 §5). Try doing it from our server layer first.
2. **Sequence `CACHE 65536`:** accept it and ignore it (Turso requires `CACHE 1`).
3. **`GENERATED BY DEFAULT|ALWAYS AS IDENTITY`:** desugar to a sequence plus a default.
4. **`ADD CONSTRAINT … NOT VALID`** and **`ALTER TABLE ASYNC … VALIDATE CONSTRAINT`:** accept them and validate synchronously.
5. Wire in **Dreamescaper's golden probes** (MIT) as a conformance runner, and track the pass rate in CI.

Items 2–4 are generic PG-compatibility work. Do them in our own crates (pre-parse rewrites in the server/policy layer) where possible; otherwise carry them on the patch branch. No upstream PRs (§6.2).

**Exit:**
- Drizzle's migrations run cleanly (the M3 target ORM, §6.1);
- golden-probe pass rate is at or above ~60% (the research/05 go criterion).

### M4 — OCC fidelity (best effort; the riskiest part)

These are listed in order of value per effort. Stop when retry-loop tests behave like DSQL's.

1. **Autocommit DML as a single-statement concurrent transaction**, so `55P03` disappears instead of being remapped. Needs a frontend change: patch branch only (no upstream, §6.2).
2. **"Doomed transaction" shim:** turn a write-time `WriteWriteConflict` into a deferred OC000 at COMMIT. It's an approximation; the caveat is in research/04 §B.
3. **Snapshot at BEGIN** instead of at the first statement. Needs a core change.
4. `FOR UPDATE` treated as a write-set touch; `FOR KEY SHARE` and FK KEY SHARE come after.

**Exit:** the OCC scenario suite (`tests/occ/`) asserts DSQL's conflict matrix from research/01 §2. Write down any cases that are knowingly different.

### Later (not in the PoC)
- TLS: SSLRequest plus direct TLS with ALPN. pgwire 0.36 already supports it. Add a local CA.
- `<id>.dsql.<region>.…` hostnames for the official connectors (research/03 §2).
- IAM token checks, roles, `AWS IAM GRANT`.
- Control plane.
- `information_schema`.
- Binary releases, Docker image, GitHub Action.
- Driver/ORM compatibility table (drivers from the M0 matrix plus Drizzle and others): status, required client settings, known gaps (§6.1).

## 5. Risks and how the plan handles them

| Risk | Mitigation |
|---|---|
| The Turso PG frontend is experimental and pre-1.0, with heavy churn (≈100 commits/week in `postgres/`) | Pinned rev; bump deliberately with the full test suite; weekly CI job that tries Turso `main` |
| Your app's SQL hits gaps: JSONB, `information_schema`, ALTER COLUMN, window functions, µs timestamptz | Found in M0 step 3, *before* investing. Work around in our own crates, or on the patch branch; no upstream changes. |
| Binary parameters break asyncpg, pgx and Npgsql | Found in M0. Decode binary for common OIDs in our vendored server. |
| OCC timing differs (Turso fails at the statement, DSQL at COMMIT) | Retry wrappers handle both. M4 narrows the gap. Document the rest. |
| The fork grows | Patches only for missing hooks. Review the patch count at every Turso bump. |
| Turso doesn't hold up | Policy layer is backend-agnostic → pivot to embedded PG16 (research/05). |

## 6. Open questions

Decided by the owner on 2026-11-12:

1. **Target ORM:** **Drizzle** is the M3 target ORM, chosen as a generic, representative choice rather than a specific app's stack. A driver/ORM **compatibility table** (what works, what doesn't, with which settings) is required later; see "Later".
2. **Upstream engagement:** **none.** No changes are proposed to Turso upstream. Anything missing is handled in our own crates, or, only if a hook is genuinely missing, on our own patch branch.
5. **Licence:** **MIT** (see `LICENSE`).

Still open:

3. **Who owns the Turso patch branch fork**, under which GitHub account or org? Only needed once a patch is unavoidable (likely M3 `sys.*` registration).
4. **Do we vendor Dreamescaper's golden probes or reference them** (MIT either way)?

## 7. First session checklist

- [ ] Install the prerequisites (§2)
- [ ] `git init`
- [ ] Clone `tursodatabase/turso` and run `cargo build -p tursopg --release`
- [ ] Run `tursopg --server 127.0.0.1:5432` and connect with psql (`sslmode=disable`)
- [ ] Run the target app's schema and key queries; record the results in `research/06-spike-findings.md`
- [ ] Two-session OCC experiments (M0.5)
- [ ] Go/no-go → start M1
- [ ] After M1: T1 testing phase (psql scenarios + MiniStack differential) → PoC done
