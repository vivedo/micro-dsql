# 02 — Turso Postgres frontend: technical assessment

I read `tursodatabase/turso` at commit `d5fdf4d` (2026-11-11). Paths below are relative to that repo. Nothing was built or run; this is a code read only.

Labels:
- **[V]**: verified in the code or docs. The key ones were spot-checked a second time.
- **[I]**: inferred.

## TL;DR

**Fits DSQL well:**
- **Every PG transaction is a Turso MVCC `BEGIN CONCURRENT`** [V]. That gives snapshot isolation (REPEATABLE READ) with optimistic, row-level conflict detection. Conflicts come back as SQLSTATE **40001** [V].
  - Structurally this is the closest match to DSQL's model of any available engine. Postgres is lock-based; Turso is not.
- **SQL is parsed by the real libpg_query**, and the translator **fails closed**. A DSQL policy layer can therefore operate on the genuine PG parse tree.
- **pgwire 0.36 already supports** classic SSLRequest, direct TLS with ALPN `postgresql`, SNI capture, and cleartext/MD5/SCRAM auth handlers [V for TLS, ALPN and SNI, read in the pgwire 0.36.3 source]. Turso just doesn't wire any of it up yet.
- Single process, `:memory:` or file DB, Rust, MIT.

**Gaps:**
- No auth or TLS wired up.
- Conflicts are raised **at write time, not at COMMIT**.
- **Autocommit writes take an exclusive lock and fail with `55P03`.**
- **The snapshot is taken at the first statement**, not at BEGIN.
- **`FOR UPDATE` is rejected**, and so are **`NOT VALID`** and **sequence `CACHE > 1`**.
- No `information_schema`.
- Weaker type fidelity: bare numeric is stored as float, timestamptz has millisecond precision, there is no interval arithmetic, and jsonb operators are partial.
- Parameters are text-only on the wire.
- The frontend and server crates are **not published**, and the frontend is labelled experimental.
- PG18 is the reference version, not PG16.

## 1. Architecture

| Crate | Role |
|---|---|
| `postgres/parser` (`turso_pg_parser`, **published on crates.io**) | Wraps `pg_query` 6.1 and translates the PG AST into the Turso/SQLite AST (`translator.rs`, about 12.7k lines). Also holds `sqlstate.rs` and `cmdtag.rs`. |
| `postgres/frontend` (`turso_pg`, unpublished) | Sessions and transactions (`session.rs`, `statement.rs`), pg_catalog vtabs (`catalog.rs`), PG functions, ACLs and roles, COPY, and the error → SQLSTATE mapping (`error.rs`). |
| `postgres/server` (`turso_pg_server`, unpublished) | pgwire server in a single file, `server/lib.rs` (about 930 non-test lines). |
| `postgres/cli` (`tursopg`) | psql-like REPL. `--server ADDR` hosts the wire server. The DB defaults to `:memory:`. |
| `conformance/`, `regress/`, `dump-fuzzer/`, `tests/` | Differential testing against real PG 18. |

**Statement flow** (`frontend/session.rs:372-503`):
1. `pg_query::parse`.
2. If the session is aborted, reject the statement with `25P02`.
3. `try_prepare_special` handles statements the frontend runs itself: SET/SHOW, SCHEMA, GRANT, COPY, and others.
4. Text rewrites: schema qualification, index naming.
5. `PostgreSQLTranslator::translate_with_prereqs` translates the statement; unsupported nodes give "X is not supported".
6. `conn.prepare_translated_cmd`.
7. Turso core compiles to VDBE bytecode.

`PostgresDialect` (`frontend/catalog.rs:44-328`) plugs into core through a `Dialect` trait (`core/dialect/mod.rs`).

**Schemas.** Namespaces are a first-class core feature (`core/namespace.rs`). Objects are stored under mangled names (`__turso_ns<id>_<name>`). `public` is a real schema and `search_path` is honoured.

**pg_catalog.** Implemented as `SnapshotCatalog<T>` virtual tables that rebuild their rows from the live schema.
- **Live:** pg_class, namespace, attribute, type, index, constraint, attrdef, tables, indexes, sequence(s), database, roles, proc, am, settings, depend, language and description.
- **Empty stubs:** pg_trigger, pg_policy and others.
- **`information_schema`: only `schemata` exists.**

### Type mapping (`translator.rs:6148-6222`, `core/schema.rs:1438-1475`)

| PG type | Turso storage | Issue for DSQL emulation |
|---|---|---|
| bool, int8, float8, text, bytea, uuid (16-byte blob) | native or custom types | OK |
| int2 | integer, range-checked | wire OID is reported as int4 |
| int4 | 64-bit integer | **no int4 overflow check** [I] |
| numeric(p,s) | real decimal (blob encoding) | OK |
| **bare `numeric`** | **REAL (float)** | Wrong. DSQL treats bare numeric as `numeric(18,6)`, so the policy layer can **rewrite bare numeric to numeric(18,6)**. |
| **timestamptz** | text, **millisecond precision**, UTC | **Microseconds are truncated**, which round-trip tests will notice. `timestamp(p)` is rejected. |
| timetz | mapped to `time` | timezone lost |
| interval | TEXT | **no interval arithmetic** |
| json | text | |
| jsonb | binary JSON | **only `->` and `->>`**. DSQL supports all PG JSON functions and operators. |
| char(n) | **rejected** | DSQL supports char(n) |
| arrays, enum, domain, inet, … | supported | DSQL forbids most of these as column types; the policy layer must reject them |

## 2. Wire server (`postgres/server/lib.rs`)

**pgwire 0.36.3**, default features (tokio-rustls, aws-lc-rs, SCRAM, MD5).

**Auth.** `startup_handler()` returns `NoopHandler` (`lib.rs:162-164`), so every connection is trusted. The startup `user` and `database` are ignored, and every session runs as the superuser `postgres`.

**TLS.** `process_socket(socket, None, factory)` (`lib.rs:115`) passes no acceptor. The pgwire 0.36.3 `tokio/server.rs` source confirms the library already has what's needed:
- detection of direct SSL negotiation;
- strict ALPN `postgresql` checks;
- SNI exposed through `ClientInfo::sni_server_name`.

**Enabling TLS and auth needs only a small change to the server.**

**Connections.** Each socket gets its own `turso_core::Connection` on a shared `Arc<Database>`, with its own session state.
- Engine calls are **synchronous inside the async handlers**, so a running statement blocks a tokio worker.
- All rows are buffered before they are sent.

**Extended protocol:**
- **Re-prepares on every Execute/Describe.**
- **Parameters are decoded as text only.** asyncpg, pgx and Npgsql commonly send binary parameters, so this is a real risk for the official connectors [I].
- Binary results are supported for numbers only. `max_rows` / portal suspension is ignored.

**Not supported:**
- **CancelRequest.**
- `COPY FROM STDIN`. This doesn't matter, because DSQL rejects COPY anyway.
- A multi-statement simple query does not run as one implicit transaction; each statement gets its own.

**Errors.** `frontend/error.rs:220-278` maps core errors to SQLSTATEs:

| Core error | SQLSTATE |
|---|---|
| `WriteWriteConflict`, `BusySnapshot`, `CommitDependencyAborted`, `SchemaConflict`, `SchemaUpdated`, `Conflict` | **40001** |
| `Busy`, `TableLocked` | **55P03** |

The server sends the C/M/D/t/c/n fields.

## 3. Concurrency (the main reason to choose Turso)

**Always MVCC.** `frontend/session.rs:101` opens the database with `with_mvcc(true)`. **PG `BEGIN` becomes `BEGIN CONCURRENT`** (`parser/translator.rs:932-945`, verified).
- Only `ISOLATION LEVEL REPEATABLE READ` is accepted. Other levels are rejected, which matches DSQL.

**Algorithm.** Hekaton-style MVCC (`core/mvcc/`). Row-level version chains cover table rows and unique-index keys. **The snapshot is taken at the first statement**, not at BEGIN (`docs/manual.md:195`).

**Conflict detection:**
- **Eager, at write time.** UPDATE or DELETE of a row that another active transaction is writing, or that was committed after our snapshot, gives `WriteWriteConflict` immediately. `core/mvcc/database/hermitage_tests.rs:15-25` states this explicitly: "NOT deferred to commit".
- **At commit.** First-committer-wins validation of row IDs and index keys, plus schema-cookie validation (`SchemaConflict`).
- **Result:** snapshot isolation. G0, G1*, OTV, PMP, P4 and G-single are prevented; **write skew (G2) is allowed**. That is the same as DSQL.

**Divergences from DSQL:**
1. **When the error appears.** DSQL fails the *second committer at COMMIT*. Turso fails the *second writer at the UPDATE/DELETE statement*.
   - Clients with retry wrappers (all the official connectors, see 03) handle both.
   - Tests that assert "COMMIT fails" will not pass.
   - Two concurrent INSERTs of the same new PK conflict only at commit, which matches DSQL.
2. **Autocommit writes** (DML without `BEGIN`) compile to `TransactionMode::Write`, which takes an **exclusive** MVCC transaction (`core/vdbe/execute.rs:4288-4312`).
   - `acquire_exclusive_tx` returns `Busy` if anything else is committing (`mvcc/database/mod.rs:7713-7742`). The busy handler defaults to none, and the PG frontend never sets a timeout.
   - So under CI parallelism, colliding autocommit writes would surface as **`55P03`**, a code DSQL never returns [I, from the code path].
   - Fix: wrap autocommit DML in BEGIN CONCURRENT/COMMIT in the frontend, or map the error / add a busy timeout.
3. **DDL inside BEGIN upgrades the transaction to exclusive.** This matters less, because DSQL allows only one DDL per transaction anyway.
4. **No `FOR UPDATE` / `FOR KEY SHARE`.** They are rejected at `translator.rs:2062, 2312`.
   - FOR UPDATE could be emulated by writing an identical row version, so it joins the write set. That gives exactly DSQL's FOR UPDATE row of the conflict matrix.
   - FOR KEY SHARE needs a read-set/lock-set concept in core's commit validation.
5. **No key vs non-key column distinction.** Turso conflicts per row, so two transactions updating different non-key columns of the same row *do* conflict in DSQL as well, as the matrix shows. The only difference is FOR KEY SHARE vs a non-key UPDATE, which DSQL allows and a naive emulation would reject.

**Maturity.** `docs/manual.md` calls MVCC "a supported journal mode" with listed limitations. Some MVCC tests are `#[ignore]`d with the note "write busy lock yet to be fixed". The MVCC test corpus is very large (hermitage tests, Antithesis, simulators). The README marks the Postgres frontend as **experimental**, and Turso is pre-1.0.

**Durability.** `:memory:` (ideal for CI) or a file with a `.db-log` logical log.

## 4. SQL coverage vs DSQL needs (from COMPAT.md plus code)

| DSQL needs | Turso PG | Gap |
|---|---|---|
| CREATE TABLE with PK/UNIQUE/CHECK/DEFAULT/NOT NULL, FKs | ✅ (tables are STRICT) | — |
| CREATE INDEX: unique, partial, expression | ✅ (`USING`, `INCLUDE` and `NULLS NOT DISTINCT` are rejected) | INCLUDE, NULLS NOT DISTINCT |
| **`CREATE INDEX ASYNC`** | ❌ libpg_query can't parse it | **Strip ASYNC in a pre-parse rewrite** and emulate `sys.jobs` |
| **`ALTER TABLE ASYNC … VALIDATE CONSTRAINT`** | ❌ | Same pre-parse rewrite |
| ALTER TABLE ADD/DROP/RENAME COLUMN, RENAME | ✅ | — |
| ALTER COLUMN SET/DROP DEFAULT, DROP NOT NULL, ADD/DROP CONSTRAINT, IDENTITY ops | ❌ rejected | **Need translator/core work** |
| **`ADD CONSTRAINT … NOT VALID`** | ❌ (`translator.rs:8298`) | **Needed: ORMs emit it** |
| Sequences | ✅, but **CACHE must be 1** (`translator.rs:5251-5266`) | **Accept and ignore `CACHE 65536`** |
| **IDENTITY columns** | ❌ (`translator.rs:8307`) | **Needed: ORMs emit `GENERATED BY DEFAULT AS IDENTITY (CACHE 65536)`.** Could desugar to sequence + default. |
| Views | ✅ (plus incremental materialized views, which DSQL rejects) | — |
| `CREATE FUNCTION … LANGUAGE sql` | ❌ (functions are rejected) | **Gap** (lower priority) |
| ON CONFLICT, RETURNING, CTEs | ✅ (no writable CTEs) | minor |
| Window functions | 🟡 aggregates OVER, row_number only | **rank, dense_rank, lag, lead missing** (DSQL docs list RANK) |
| DELETE USING, DISTINCT ON, GROUPING SETS | ❌ | DELETE USING is in DSQL's list |
| JSON/JSONB | 🟡 | Large gap |
| `information_schema` | ❌ | **Large gap for ORMs and migration tools** |
| Roles/GRANT | 🟡 Very recent: CREATE ROLE without options, GRANT on tables/sequences, privileges enforced. CREATE USER and LOGIN rejected. | Need `WITH LOGIN`, `AWS IAM GRANT`, admin semantics |
| SET/SHOW | 🟡 search_path, custom `x.y`, a few no-ops; anything else rejected | The DSQL whitelist (application_name, datestyle, timezone, extra_float_digits, …) must be accepted |
| SAVEPOINT, TRUNCATE, temp tables, domains, serial, LISTEN/advisory locks | Some supported | **The policy layer must reject them** to match DSQL |

**Other gap: PG18 vs PG16.** Mostly harmless, but syntax that only exists in PG18 (virtual generated columns, `uuidv7()`, OLD/NEW in RETURNING, …) should be rejected if Turso accepts it. `server_version` is just a startup parameter, so it can report 16.x.

## 5. Extension points

| Need | Where | Fork needed? |
|---|---|---|
| Pre-parse rewrite (ASYNC) | Server `do_query` and extended Parse path (`server/lib.rs:180-205, 336-361`) | No (own server crate) |
| Statement policy (reject FK-less…, temp, TRUNCATE, …) | After `pg_query::parse` in `prepare_statement_inner` (`session.rs:372`), or in our own server before `PgConnection::prepare` | No for statement-level rules |
| Transaction rules (1 DDL, no DDL+DML, 3k rows, 10 MiB, 300 s) | Per-session state. `stmt.n_change()` is available at the server level. Byte counting needs core support. | Mostly no; byte size needs core |
| `sys.*` functions and tables | `PostgresDialect::register_native_extensions` (`catalog.rs:190`). The modules are private, and `open_database_with_io` hard-codes the dialect. | **Yes (light fork or upstream PR)** |
| Auth and TLS | `TursoPgFactory::startup_handler`, `process_socket(…, Some(acceptor), …)` | No (own server crate) |
| Error codes and OC000/OC001 text | `frontend/error.rs:220-278` | Light fork, or remap in our server |
| Snapshot at BEGIN, FOR UPDATE, deferred conflict reporting, autocommit concurrency | Core MVCC and frontend | **Yes (core changes)** |

**Library use.** Only `turso_pg_parser` is published. `turso_pg` depends on `turso_core` with the unstable `conn_raw_api` feature, everything is versioned `0.9.0-pre.5`, and there are no API stability promises. **Realistic setup:** a git dependency pinned to a commit of our own fork, carrying a small, upstreamable patch set.

## 6. Maturity signals
- **Commit activity in `postgres/`:** 1 commit in September, **366 in October** and **110 on 1–11 November 2026**.
  - The main human authors are Jussi Saurio and Pekka Enberg.
  - A large share of commits come from AI agents ("Turso Agent", Amp, Claude). Expect high churn.
  - The latest work is GRANT/privileges and RLS.
- **Tests:**
  - `conformance/upstream` runs 103 of the 233 upstream PG 18 regression files. No pass rate is recorded.
  - A SQLSTATE differential fuzzer.
  - A pg_dump differential fuzzer.
  - Large integration suites.
- **Release status:** the blog post (16 Jul 2026) says "a foundation, not a finished product" and "no published packages". There are still no tursopg binaries, crates or Docker images (checked crates.io, GitHub releases and Docker Hub).

## 7. Build footprint
- **Rust 1.88** (pinned in `rust-toolchain.toml`).
- **Native code to compile:**
  - **pg_query** compiles the libpg_query C sources and uses bindgen, so it **needs clang/libclang**. prost-build *may* also need `protoc`, which is unverified.
  - aws-lc-sys (TLS; may need cmake), mimalloc and SimSIMD.
  - The CLI also pulls in tantivy (FTS) and rustyline, so build our own lean server binary instead of `tursopg`.
- **Expected result:** a static musl or distroless binary in the tens of MB, starting in milliseconds [I]. The builder image needs clang.
