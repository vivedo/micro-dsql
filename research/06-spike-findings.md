# 06 — M0 spike findings: running Turso's Postgres frontend

Run on 2026-11-12. This is the first time the research in 01–05 was checked by **running** the code
rather than reading it (docs/plan.md §4 M0). Everything below is reproducible with the scripts in
[`research/spike/`](spike/README.md); the raw output of the recorded run is in `research/spike/results/`.

## Summary

**Verdict: CONDITIONAL GO.** The engine itself holds up: it builds cleanly, starts in under 10 ms, never
crashed or hung, and every conflict surfaced as `40001` or `55P03` with consistent final state. The
weak part is the **wire layer and type fidelity**, not the concurrency model:

1. **Only psql and node-pg work out of the box.** asyncpg and pgx fail every parameterised query
   because the server's ParameterDescription always reports **0 parameters**; psycopg 3 fails every
   parameterised query by default because it sends integers in **binary**, which the server rejects.
2. **Result type OIDs are wrong**: `count(*)`, `sum()`, `avg()` come back as `text` (OID 25),
   `numeric(p,s)` columns as `float8` (OID 701), boolean expressions as integers. Typed drivers (pgx)
   refuse to scan them; others silently change type.
3. **Silent wrong results** in app SQL: foreign keys are **not enforced** (no 23503, no ON DELETE
   CASCADE), `numeric(12,2)` aggregates are computed in floating point (`53.230000000000004`), jsonb
   `@>` returns no rows instead of matching rows, `interval '1 day' + interval '2 hours'` returns `3`.
4. **OCC behaves exactly as research/02 predicted**: second writer fails at the UPDATE with 40001,
   same-PK inserts fail at COMMIT with 40001, write skew is allowed, snapshot is taken at the first
   statement, autocommit writes collide with `55P03 Database is busy` (~4% of writes at 8 workers).

Most driver blockers (1, 2, and the Terminate bug) sit in `postgres/server/lib.rs`, the file docs/plan.md
already says we vendor in M1, so they are fixable in our own crate. FK enforcement is likely a
one-line connection setting. Numeric arithmetic, jsonb and timestamptz gaps are upstream/core work and
become M1 known gaps.

## 1. What was tested

| Item | Value |
|---|---|
| Turso commit | `d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5` (2026-11-11), same commit as research 01–05 |
| `tursopg --version` | `tursopg 0.9.0-pre.5` |
| Host | macOS 26.4 arm64 (Apple M4, 10 cores) |
| Rust | **stable 1.99.1** via `RUSTUP_TOOLCHAIN=stable` (the clone pins 1.88; see §2) |
| C toolchain | Apple clang 17 (Xcode Command Line Tools); protoc 36.2 and cmake 4.4 were present |
| Clients | psql 18.6 (libpq 18.6); psycopg 3.3.8 + Python 3.14; asyncpg 0.32.0; node-pg 8.23.1 + Node 22; pgx v5.13.0 + Go 1.27 |
| Server mode | `tursopg --server 127.0.0.1:5432`, default `:memory:`, a fresh process per script group |
| Connection | `host=127.0.0.1 port=5432 user=admin dbname=postgres sslmode=disable`, arbitrary password |

CLI as built: `tursopg [OPTIONS] [DATABASE] [SQL]` with `--server <ADDR>`, `-q`, `--vfs`,
`--readonly`, `-t <trace file>`. `DATABASE` defaults to `:memory:`.

## 2. Build notes

- `cargo build -p tursopg --release` succeeded on a clean `target/` in **1 min 51 s** wall time
  (575 s CPU, 409 crates). Binary: **31.5 MB** (release profile keeps debuginfo; macOS, dynamically
  linked to system libs).
- **Toolchain:** rustup could not download the pinned 1.88 here (TLS trust-store failure in this
  environment, not a Turso problem). Stable 1.99.1 built the tree with no errors, so docs/plan.md §2's
  "build with stable first" holds; no `rust-toolchain.toml` pin is needed for M1 yet.
- **Native deps:** nothing had to be installed. In the `tursopg` dependency tree: `pg_query` 6.1.1
  (bindgen → libclang, from Xcode CLT), `aws-lc-sys` 0.46 (cc/cmake), `mimalloc`, plus `tantivy` and
  `rustyline` (CLI only). `pg_query`'s build script uses **protoc only if it is on PATH** (otherwise
  it uses pre-generated bindings), so protoc is optional.
- **Warnings that matter:** none blocking. `turso_core` emits 29 warnings (21 build-script, 8 lib):
  `specialization is experimental` (in `core/alloc`), deprecated `Atomic::fetch_update`, function
  pointer comparisons; `proc-macro-error2 v2.0.1` carries a future-incompatibility warning. These
  may become errors on a future stable, which is a reason to keep CI on a pinned toolchain later.
- **Client tooling on this machine:** `brew install libpq go` failed inside this environment (a
  Homebrew error in its temp-dir handling, unrelated to the project); the same bottles were unpacked
  by hand and used. Go module downloads needed `SSL_CERT_FILE` pointing at the system CA bundle.
  None of this affects the scripts, which only rely on `psql`/`python`/`node`/`go` and `PG*` vars.
- **Start-up:** launch to first accepted TCP connection **8–9 ms** (`research/spike/startup_time.py`).

## 3. Connect (`research/spike/results/connect.txt`)

| Check | Result |
|---|---|
| `SELECT 1` over psql, `sslmode=disable`, arbitrary password | **OK** (hard-stop condition passed) |
| Any password accepted | OK (`NoopHandler`; no auth exchange at all) |
| `current_user`, `session_user` | `postgres` (startup user `admin` ignored) |
| `current_database()` | **`:memory:`** (not `postgres`) |
| `version()` | `PostgreSQL 16.6 (Turso v0.9.0-pre.5)` |
| Startup parameter `server_version` | **`16.6-pgwire-0.36.3`** (asyncpg parses it as 16.0.6) |
| `SHOW server_version`, `SHOW timezone`, `SHOW transaction_isolation` | `42601 … is not supported` |
| `SET timezone = 'UTC'` | `42601 SET timezone is not supported` |
| `SET application_name`, `SET extra_float_digits`, `SET datestyle`, `SET intervalstyle` | accepted |
| `SET enable_seqscan`, `SET statement_timeout = 1000` | rejected (`42601`, `22023` "supported value is 0") |
| `SHOW search_path`, `SHOW standard_conforming_strings`, `SHOW datestyle` | OK |

## 4. App-style SQL (`research/spike/app/`, `research/spike/results/app.txt`)

No target app exists, so a small shop schema stands in for "your app's SQL": 5 tables (uuid, bigint,
int, numeric(12,2), bool, timestamptz, jsonb, text), PK/FK/UNIQUE/CHECK/DEFAULT, a composite PK, FKs
with `ON DELETE CASCADE`, a DESC index, a partial index and a unique expression index.

**Schema migration: all 5 tables, 4 indexes and seed inserts succeed** once timestamp literals use
`+00:00`. With `+00` (PG's own output format, and a common literal form) the inserts fail with
`22007 invalid timestamp value` (probe P25).

| # | Query | Result |
|---|---|---|
| Q01 | LEFT JOIN + GROUP BY + count/sum + ORDER BY | rows correct; **`sum(numeric)` = `53.230000000000004`** (float), `count` typed as text |
| Q02 | 4-table join, ORDER BY, LIMIT | OK |
| Q03 | keyset pagination with row comparison | OK |
| Q04 | `ON CONFLICT (id) DO UPDATE … RETURNING` | OK |
| Q05 | `ON CONFLICT (email) DO NOTHING RETURNING` | OK (0 rows) |
| Q06 | `INSERT … RETURNING` with `gen_random_uuid()`, defaults | OK; boolean expression returned as `1`, not `t` |
| Q07 | `UPDATE … RETURNING` | OK |
| Q08 | `DELETE` parent with `ON DELETE CASCADE` | **WRONG: child row not deleted** (FKs not enforced) |
| Q09 | CTE + join | OK with float artefact (`33.730000000000004`) |
| Q10 | `row_number() OVER (PARTITION BY … ORDER BY …)` | OK; timestamptz through a subquery loses its `+00` suffix |
| Q11 | `sum() OVER (… ORDER BY …)` running total | OK with float artefact |
| Q12 | `rank()`, `lag()` | **OK** (research said missing) |
| Q13 | jsonb `->`, `->>`, nested path | OK |
| Q13 | jsonb `@>` | **WRONG: 0 rows** (expected 2), no error |
| Q13 | jsonb `?`, `#>>`, `jsonb_build_object` | `42601 Unsupported operator` / `42883 no such function` |
| Q14 | timestamptz µs round trip | **truncated to ms** (`.111111` → `.111`) |
| Q14 | `date_trunc`, `timestamptz - interval` | `42883 no such function`, `42601 operator does not exist: timestamptz - text` |
| Q15 | numeric sum/avg/round | float artefact in `sum`; `round(avg, 2)` OK |
| Q15 | `0.1::numeric(12,2)` cast with typmod | `0A000 a type modifier in a cast is not supported` |
| Q16 | `count(*) FILTER (WHERE …)`, `EXISTS` | OK |
| Q17 | FK violation | **WRONG: insert accepted** (expected 23503) |
| Q17 | unique violation / CHECK violation | `23505` / `23514` with correct detail, table and constraint fields |
| Q18 | uuid lookup, IN, DISTINCT, HAVING | OK |

Tally: **8 correct, 5 correct with type/precision artefacts, 5 wrong or partly unsupported.**

FK enforcement: Turso core has a `foreign_keys` connection flag (SQLite semantics, off by default)
and nothing in `postgres/frontend` turns it on, which matches the observed behaviour. Enabling it per
connection in our server is the likely fix (unverified).

### Research probes (`research/spike/app/probes.sql`)

| Probe | Result |
|---|---|
| P01/P02 int4 overflow | `2147483647::int4 + 1` = `2147483648`; `3000000000` stored in an `int4` column |
| P03 int2 range | rejected, but with SQLSTATE **`P0001`** (PG: `22003`) |
| P04 bare `numeric` | stored as float: `0.1 + 0.2` = `0.30000000000000004`, `123456789.123456789` → `123456789.12345679` |
| P05 `timestamp(3)`, `char(10)` | `0A000` type modifier not supported |
| P06 interval | `interval '1 day' + interval '2 hours'` = **`3`** (silently wrong); `timestamptz + interval` → `42601` |
| P07 `information_schema.tables/columns` | `42P01 no such table` |
| P08 `pg_class`/`pg_namespace` join | OK |
| P09 `FOR UPDATE`, `FOR KEY SHARE` | `0A000` |
| P10 `DELETE … USING` | `0A000` |
| P11 `CREATE INDEX ASYNC` | `42601` syntax error |
| P12 `GENERATED BY DEFAULT AS IDENTITY` | `0A000` |
| P13 sequence `CACHE 65536` / `CACHE 1` | `0A000` / OK, `nextval` = 1 |
| P14 `ADD CONSTRAINT … NOT VALID` | `42601 ALTER TABLE ADD CONSTRAINT is not supported` (any ADD CONSTRAINT) |
| P15 `ALTER COLUMN SET DEFAULT / DROP NOT NULL` | `42601` |
| P16 `ADD COLUMN`, `RENAME COLUMN` | OK |
| P17 accepted although DSQL rejects | `TRUNCATE`, `SAVEPOINT`/`ROLLBACK TO`, `CREATE DOMAIN`, `CREATE TYPE … ENUM`, `text[]` column, `serial` |
| P17 rejected | `CREATE TEMP TABLE` (`0A000`), `CREATE MATERIALIZED VIEW` (`42601 … not supported in MVCC mode`) |
| P18 | `CREATE FUNCTION`, `CREATE EXTENSION`, `LISTEN`, `NOTIFY` → `42601`; `pg_advisory_lock` → `42883`; **`COPY … TO STDOUT` accepted** |
| P20 error in txn | `22012`, then `25P02` until end; `COMMIT` answers `ROLLBACK` (PG-compatible) |
| P21 DDL+DML and 2 DDLs in one txn | accepted (DSQL rejects both; policy layer work, as planned) |
| P22 multi-statement simple query | not one implicit transaction: first INSERT persists after the second fails |
| P23 PG18-only `uuidv7()`, `VIRTUAL` generated column | rejected (`42883`, `42601`) |
| P24 `EXPLAIN` | SQLite-style plan rows |
| P25 `'…+00'`, `'…+01'` literals | **`22007 invalid timestamp value`**; `'…+00:00'` OK |

## 5. Driver matrix (`research/spike/drivers/`, `research/spike/results/drv-*.txt`, `wire.txt`)

Every driver connected with `sslmode=disable` and an arbitrary password.

| Driver | Connect | CRUD | Explicit txn | Parameterised | Verdict |
|---|---|---|---|---|---|
| psql 18 (simple protocol; `\bind` = extended, text params) | OK | OK | OK (commit + rollback) | OK | **PASS** |
| node-pg 8.23 (extended, text params) | OK | OK | OK | OK, incl. named prepared statements | **PASS** with caveats: `numeric` arrives as a JS number (`1.5`, OID float8), `count` as string |
| psycopg 3.3, default `%s` | OK | literals only | literals only | **FAIL**: every query with an int param → `XX000 invalid integer parameter: invalid digit found in string` (psycopg sends ints in binary) | **FAIL** by default |
| psycopg 3.3, forced text `%t` | OK | OK | OK (incl. default non-autocommit mode) | OK, `prepare=True` and `executemany` OK | **PASS** with caveats: µs lost; `numeric` → float; binary results return garbage (`timestamp too large (after year 10K)`) |
| asyncpg 0.32 | OK | literals only | OK without params | **FAIL**: `the server expects 0 arguments for this query, N were passed` | **FAIL**; also `close()` never completes (server ignores Terminate) |
| pgx v5.13, default (cached statements) | OK | literals only | FAIL (params) | **FAIL**: `mismatched param and argument count` | **FAIL** |
| pgx v5.13, `QueryExecModeSimpleProtocol` | OK | OK | OK | OK (client-side interpolation) | **PARTIAL**: `count(*)` can't scan (`text (OID 25)` into int64), `numeric` can't scan (`ParseInt "2.50"`) |

Root causes, from `research/spike/drivers/wire_probe.py`:

| Wire behaviour | Observed |
|---|---|
| ParameterDescription after Parse+Describe | **always 0 parameters**, for SELECT, INSERT and `SELECT $1::int + 1` |
| Bind with a text parameter | OK |
| Bind with a binary int8 parameter (Parse declared OID 20) | `XX000 invalid integer parameter` |
| RowDescription OIDs for columns | bigint 20, int 23, **smallint 23**, **numeric(10,2) 701**, **bare numeric 701**, float8 701, bool 16, timestamptz 1184, uuid 2950, jsonb 3802, text 25 |
| RowDescription OIDs for expressions | **`count(*)`, `sum`, `avg` → 25 (text)**; literal `1` → 23; `1.5` → 701; `2::bigint` → 20; `now()` → 1184 |
| Binary result format | ints sent in binary; **numeric, timestamptz, uuid sent as text bytes while flagged binary** (clients misdecode) |
| Terminate (`X`) | **server does not close the socket** (waits for the client) |
| Errors | proper ErrorResponse with C/M/D/t/c fields; no connection drops seen |

## 6. OCC experiments (`research/spike/occ/occ.py`, `research/spike/results/occ.txt`)

Two psycopg sessions with explicit `BEGIN`/`COMMIT`, interleaved deterministically, each statement
guarded by a 10 s client-side timeout.

| Scenario | Failing statement | SQLSTATE / message | Final state |
|---|---|---|---|
| S1 same-row UPDATE (A updates, B updates same row, A commits) | **B's UPDATE** | `40001 Write-write conflict`; B then gets `25P02`; B's COMMIT answers ROLLBACK | A's write only (90) — correct |
| S1b B reads, A updates+commits, B updates | **B's UPDATE** | `40001 Write-write conflict` | A's write only — correct |
| S1c different rows | none | — | both writes — correct |
| S1d BEGIN, then concurrent autocommit write, then first read | none | B **sees** the new value (555) | snapshot taken at first statement, not BEGIN |
| S1e same row, first writer rolls back | **B's UPDATE** (before A's rollback) | `40001` | nothing applied (DSQL would let B commit) |
| S2 same-PK INSERT, A commits first | **B's COMMIT** | `40001 Write-write conflict` (not 23505) | A's row only |
| S2b same-PK INSERT, B commits first | **A's COMMIT** | `40001` | B's row only (first committer wins) |
| S2c same UNIQUE (non-PK) key INSERT | **second COMMIT** | `40001` | first row only |
| S3 write skew (on-call doctors) | none | both commit | both off call: **write skew allowed** (SI, same as DSQL) |
| S4 autocommit storm, 8 workers × 150 × (INSERT unique row + UPDATE hot row) | random autocommit statements | **`55P03 Database is busy`**: 91 of 2,400 writes (41 inserts, 50 updates) | consistent: rows = successful inserts, counter = successful updates |
| S4 same storm with explicit BEGIN/COMMIT | the hot-row UPDATE | `40001 Write-write conflict` on 404 of 1,200 updates; inserts never fail | consistent |
| S7 txn A reads/writes table T; session B `ALTER TABLE T ADD COLUMN` | **A's COMMIT** | `40001 Database schema conflict` | A's insert discarded |
| S7b txn A updates T; session B creates an **unrelated** table and commits | **A's COMMIT** | `40001 Database schema conflict` | A's update discarded |

Isolation levels (S5): `BEGIN`, `BEGIN ISOLATION LEVEL REPEATABLE READ`, `BEGIN TRANSACTION
ISOLATION LEVEL REPEATABLE READ, READ WRITE`, `BEGIN READ ONLY`, `START TRANSACTION [ISOLATION LEVEL
REPEATABLE READ]` and `SET TRANSACTION ISOLATION LEVEL REPEATABLE READ` are accepted. `READ
UNCOMMITTED`, `READ COMMITTED` and `SERIALIZABLE` are rejected with `0A000 ISOLATION LEVEL … (Turso
transactions are snapshot isolation, i.e. REPEATABLE READ) is not supported`. `SET SESSION
CHARACTERISTICS …` → `42601`; `SHOW transaction_isolation` → `42601`.

**No crashes, no hangs, no corruption** in any scenario; the server answered `SELECT 1` after the
run, and its log contains only the start-up line.

Differences from DSQL to carry into M2/M4: conflicts on UPDATE fail at the statement, not at COMMIT
(as predicted); a conflicting UPDATE fails even if the first writer later rolls back; **any committed
DDL anywhere aborts every concurrent writing transaction** with `40001` (DSQL's OC001 is about the
objects the transaction used); `55P03` appears under ordinary autocommit concurrency.

## 7. Verdicts on inferred ([I]) and unverified research claims

| Source | Claim | Verdict | Evidence |
|---|---|---|---|
| 02 §1 type table | int4 has no overflow check [I] | **CONFIRMED** | P01 `2147483647::int4 + 1` = 2147483648; P02 stores 3e9 in int4 |
| 02 §2 | Parameters are decoded as text only; asyncpg, pgx, Npgsql send binary, a real risk [I] | **CONFIRMED, and worse than stated** | Binary int8 param → `XX000` (wire probe). psycopg 3 also sends binary ints by default and fails. asyncpg and pgx fail *earlier*, because ParameterDescription reports 0 params. Npgsql **NOT TESTED** |
| 02 §3 | Colliding autocommit writes surface as `55P03` [I, from the code path] | **CONFIRMED** | S4: 91/2,400 autocommit writes → `55P03 Database is busy`; none with explicit transactions |
| 02 §7 | prost-build *may* need protoc (unverified) | **REFUTED** (not required) | `pg_query` 6.1.1 `build.rs` regenerates protobuf bindings only if `protoc` is on PATH |
| 02 §7 | Static musl/distroless binary in the tens of MB, starting in ms [I] | **PARTLY CONFIRMED** | 31.5 MB release binary (with debuginfo, macOS dynamic); 8–9 ms to first TCP accept. Static musl build **NOT TESTED** |
| 04 §A | Binary parameter format is a gap (★ row) | **CONFIRMED** | as above |
| 04 §C | Turso accepts temp tables (?) | **REFUTED** | `CREATE TEMP TABLE` → `0A000 CREATE TEMPORARY TABLE is not supported` |
| 04 §C | int4 range enforcement missing [I] | **CONFIRMED** | P01/P02 |

Other research statements that the run contradicts or sharpens (they were labelled [V], from code):

| Source | Statement | Observed |
|---|---|---|
| 02 §4, 04 §C | Window functions: rank, dense_rank, lag, lead missing | **REFUTED**: `rank()` and `lag()` work (Q12) |
| 02 §4 | CREATE TABLE with FKs ✅ | Parsed but **not enforced** (Q08, Q17) |
| 02 §1 | `numeric(p,s)` → real decimal, OK | Values print with scale in psql, but the wire OID is float8, aggregates are float (`53.230000000000004`) and drivers receive floats |
| 04 §C | Turso accepts matviews | **REFUTED** in MVCC mode (the PG frontend always uses MVCC) |
| 02 §4 | jsonb: only `->`, `->>` | Confirmed, and `@>` is accepted but returns wrong (empty) results |
| 02 §1 | timestamptz has ms precision | Confirmed; also the `+00` offset form is rejected |
| 02 §1 | interval: no arithmetic | Confirmed, and `interval + interval` silently returns `3` |
| 02 §2 | startup user and database ignored, sessions run as `postgres` | Confirmed; `current_database()` is `:memory:` |
| 02 §2 | multi-statement simple query is not one transaction | Confirmed (P22) |
| 02 §3 | write-time conflicts, snapshot at first statement, same-PK insert conflicts at COMMIT, write skew allowed, only REPEATABLE READ | All confirmed (S1–S5) |
| 02 §4 | PG18 is the reference; PG18-only syntax may be accepted | `version()` says PostgreSQL 16.6; `uuidv7()` and `VIRTUAL` are rejected |
| 02 §2 | CancelRequest unsupported | **NOT TESTED** |
| 02 §2 | engine calls block tokio workers | **NOT TESTED** directly; no stalls seen with 8 concurrent connections |

## 8. Go / no-go against docs/plan.md M0

| M0 criterion | Assessment |
|---|---|
| App SQL works, or each blocker ≈ a week of fixes | **Partly.** The schema and 13 of 18 query groups run; the 5 failures split into cheap fixes (FK flag, `+00` parsing) and upstream-sized work (decimal arithmetic, jsonb, interval/date functions). |
| Drivers work, or each blocker ≈ a week of fixes | **Partly.** psql and node-pg pass. psycopg, asyncpg and pgx are blocked by three wire-layer bugs (ParameterDescription, binary params, result OIDs). All three are in the server file we vendor in M1; each looks like 1–2 weeks. |
| Conflicts surface as 40001 or 55P03, no crashes or corruption | **Yes.** All conflicts were 40001 or 55P03; no crash, hang or inconsistency in 2 × 2,400 concurrent writes. |

**Verdict: CONDITIONAL GO.** Proceed with M1 on Turso, on condition that M1/M2 take on the
wire-layer blockers in our vendored server (B1–B4 below) and that the remaining items are recorded
as known gaps. If B1–B3 turn out to need translator-level type inference beyond ~2 weeks, re-check
this verdict. Per the run instructions, the embedded-PG16 pivot is **not** started; the reasons that
would argue for it are listed as known gaps.

## 9. Blockers and known gaps for M1+

| # | Gap | Impact | Where | Effort |
|---|---|---|---|---|
| B1 | ParameterDescription always reports 0 parameters | asyncpg and pgx (default mode) cannot run any parameterised query | vendored server; needs param types from the parser/translator (or `$n` count + inferred OIDs) | M (1–2 wk) |
| B2 | Binary-format parameters rejected (`XX000`) | psycopg 3 default, asyncpg, pgx, Npgsql | vendored server: decode binary for int2/4/8, float4/8, bool, numeric, text, uuid, timestamptz, bytea, jsonb | S–M (3–5 d) |
| B3 | Wrong result OIDs: aggregates → text, numeric → float8, smallint → int4, bool expressions → int; binary results send text bytes for numeric/timestamptz/uuid | typed drivers fail or silently change types | vendored server + frontend type info | M |
| B4 | Terminate does not close the connection | asyncpg `close()` hangs; connections leak | vendored server | S (≤1 d) |
| B5 | FKs not enforced (no 23503, no CASCADE) | silent wrong results | turn on `foreign_keys` per connection in our server; verify cascades | S (1–2 d) |
| B6 | `numeric(p,s)` arithmetic/aggregates in float; bare numeric = REAL | money maths wrong | core/translator (upstream); bare-numeric rewrite already planned (M2) | L |
| B7 | timestamptz: `+00`/`+01` offsets rejected; µs truncated to ms; no `date_trunc`; no interval arithmetic (and `interval + interval` silently wrong) | common app queries fail | upstream (`+00` parsing S; µs M; interval/date functions L) | S → L |
| B8 | jsonb: `@>` returns wrong results; `?`, `#>>`, `jsonb_build_object` etc. missing | silent wrong results / failures | upstream | L |
| B9 | `SHOW server_version/timezone/transaction_isolation`, `SET timezone` rejected; startup `server_version` is `16.6-pgwire-0.36.3`; `current_database()` = `:memory:`; `current_user` = `postgres` | T1 checks (`SHOW server_version`, `SHOW timezone`) fail | own server layer (already M1 step 5 / M2 SET whitelist) | S |
| B10 | `55P03` on concurrent autocommit writes | spurious failures under CI parallelism | planned M2 (map) / M4 (fix) | M |
| B11 | Any committed DDL aborts all concurrent writing transactions (40001 schema conflict) | flaky tests when migrations run beside other work | core (schema cookie granularity); document for M4 | M–L |
| B12 | `information_schema` tables/columns missing; IDENTITY, `CACHE 65536`, `NOT VALID`/ADD CONSTRAINT, ALTER COLUMN, `CREATE INDEX ASYNC`, `FOR UPDATE`, `DELETE USING` rejected | ORMs/migrations | already planned M2/M3/M4 | as research/04 |
| B13 | Accepted although DSQL rejects: TRUNCATE, SAVEPOINT, DOMAIN, ENUM, arrays, serial, COPY TO, DDL+DML in one txn, multiple DDLs | tests pass locally that fail on DSQL | policy layer (M2) | M |
| B14 | SQLSTATE quirks: int2 overflow `P0001` (PG `22003`), bad binary param `XX000` (PG `22P03`); no int4 overflow check | error-shape fidelity | upstream / error remap | S |

## 10. Reproducing

```sh
# in a local Turso clone, at the commit above
cargo build -p tursopg --release          # RUSTUP_TOOLCHAIN=stable if 1.88 is unavailable
# in this repository (driver installation: research/spike/README.md)
TURSOPG=<turso-clone>/target/release/tursopg PSQL=<psql> research/spike/run-all.sh
```

The whole suite takes about 20 s. The OCC storm counts vary between runs (55P03: 83–95 of 2,400;
40001: 396–422 of 1,200 across four runs); the qualitative results do not.
