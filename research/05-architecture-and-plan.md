# 05 — Proposed architecture, PoC plan, go/no-go criteria

## Scope (decided 2026-11-12)

The goal is **a functional replica of DSQL as a database**, for local tests and CI.

In scope for the PoC:
- DSQL transaction semantics: snapshot isolation, optimistic concurrency, the OC000/OC001 errors.
- DSQL transaction rules and limits.
- DSQL's SQL dialect: what is accepted, what is rejected, ASYNC jobs, `sys.*`.
- Enough pg_catalog and `information_schema` for the common drivers and ORMs.

**Out of the PoC (later improvements):**
- TLS, direct TLS and SNI routing.
- IAM token validation, roles and IAM mapping. The PoC accepts any user and password.
- The control-plane API.
- Multi-region.
- DSQL-shaped EXPLAIN.

## Positioning

Every existing DSQL emulator (MiniStack, Floci, Dreamescaper, LocalStack) runs real PG16 behind a proxy. Those give exact PG fidelity, but they need Docker, and their OCC is lock-based, not optimistic.

This project would be different on two points:
1. **A single binary with no Docker**, millisecond startup and an in-memory DB. It fits CI, `cargo test`/`pytest` fixtures and tiny runners.
2. **Natively optimistic concurrency.** Turso's MVCC gives real snapshot isolation without lock waits, so concurrent-conflict behaviour can be closer to DSQL's than any PG-backed emulator manages.

The price is lower PG fidelity in the long tail. The emulator's honest promise is: *"if it passes here, it uses only DSQL-legal SQL and survives OCC retries."* It does not promise *"bit-exact PG16"*.

## Architecture

```
micro-dsql (binary, Rust)
├── server       pgwire listener (forked/adapted from turso_pg_server)
│                - startup params mimic DSQL (server_version 16.x, TimeZone=UTC, …)
│                - per-connection blocking executor thread
│                - [later] TLS/direct-TLS/SNI, IAM token auth, cluster routing
├── dsql-policy  operates on the pg_query AST before execution
│                - pre-parse rewrites: CREATE [UNIQUE] INDEX ASYNC, ALTER TABLE ASYNC,
│                  [later] AWS IAM GRANT/REVOKE
│                - statement allow/deny rules (DSQL dialect), PG18-only syntax rejection
│                - DDL rewrites: bare numeric → numeric(18,6), IDENTITY/CACHE handling
│                - per-txn state machine: 1 DDL, no DDL+DML, 3k rows, 10 MiB, 300 s, no SAVEPOINT
│                - SET whitelist
│                - error shaping: 40001 + "(OC000)/(OC001)" texts, 54000/54011 limit errors
├── dsql-sys     sys schema: sys.jobs, sys.wait_for_job, sys.dsql_major_version,
│                [later] sys.iam_pg_role_mappings, sys.current_session_id …
└── turso (git dependency → our fork, pinned commit)
    ├── turso_pg_parser   (published)
    ├── turso_pg          (fork: hooks for sys.*, error remapping, snapshot-at-BEGIN, deferred conflicts)
    └── turso_core        (fork only if needed: FOR UPDATE / KEY SHARE, byte accounting)
```

**Fork strategy.**
- Keep a `micro-dsql` branch on a fork of `tursodatabase/turso`, with a small patch stack.
- Send generic PG-compatibility fixes upstream as PRs: `CACHE`, `NOT VALID`, IDENTITY, information_schema, binary parameters, int4 range, char(n).
- Keep only DSQL-specific hooks in the fork.
- Rebase regularly. Upstream churn is about 100 commits a week in `postgres/`.

## PoC phases

### Phase 0 — Spike (≈2–3 days)

**Goal:** confirm the assumptions this research rests on by running code. So far everything here comes from reading code.

1. Install Rust 1.88, clang/libclang and protoc if needed, and build `tursopg`. Run `tursopg --server 127.0.0.1:5432`.
2. Connect with psql, psycopg, asyncpg, pgx, JDBC and node-postgres, all with `sslmode=disable`.
   - Smoke CRUD.
   - Record failures. Binary parameters (asyncpg, pgx) are the main suspect.
3. **OCC experiments with two sessions:**
   - concurrent UPDATE of the same row;
   - concurrent INSERT of the same PK;
   - write skew;
   - autocommit vs autocommit under load (expected `55P03`);
   - DDL vs DML (expected 40001).
   - Confirm *when* each error appears.
4. Run Dreamescaper's 242 golden probes against raw Turso as a baseline. Expect many failures; the point is to size the gap.

**Exit:** a findings doc that confirms or refutes each [I] item in 02/04.

### Phase 1 — Minimal DSQL-shaped DB (≈2–3 weeks)

- Own server binary (no TLS or auth); startup params like DSQL; non-blocking executor.
- Policy layer:
  - statement deny rules;
  - per-transaction state machine (1 DDL, no mixing, 3k rows, 300 s, no SAVEPOINT);
  - SET whitelist;
  - bare numeric rewrite.
- `CREATE INDEX ASYNC`: the pre-parse rewrite, a synchronous build, a `job_id` result, `sys.jobs`, `sys.wait_for_job`.
- Error shaping: OC000/OC001 texts; 55P03 → 40001; DSQL's limit messages.
- Fork patches:
  - accept `CACHE 1|≥65536`;
  - IDENTITY desugaring;
  - `NOT VALID` plus `ALTER TABLE ASYNC … VALIDATE`.
- **Test harness:**
  - Dreamescaper's golden probes.
  - A port of the relevant MiniStack `test_dsql.py` cases.
  - A small set of OCC scenario tests.

### Phase 2 — OCC fidelity (≈2–4 weeks, the riskiest part)

- Snapshot at BEGIN.
- Autocommit DML as concurrent transactions.
- Defer write-write conflicts to COMMIT. Try the "doomed transaction" shim first; evaluate core changes if the shim isn't good enough.
- `SELECT … FOR UPDATE` as a write-set touch; then `FOR KEY SHARE` and FK KEY SHARE semantics.
- 10 MiB byte accounting.

### Phase 3 — ORM/driver compatibility (ongoing)

- **ORM targets:** SQLAlchemy plus the DSQL dialect, Django plus its adapter, Hibernate, Prisma, Drizzle and Flyway against the emulator.
- **Gaps to fill:** `information_schema`, catalog gaps, binary params, ALTER COLUMN ops, window functions, JSONB, timestamptz µs. Upstream first.

### Phase 4 — Improvements (post-PoC)

- TLS (SSLRequest + direct + ALPN), a local CA, and SNI/`amzn-cluster-id` routing for multiple clusters. pgwire already supports all of this.
- IAM token parsing and SigV4 verification; admin and custom roles; `AWS IAM GRANT`.
- Connection limits and the 60-minute cap.
- An optional control-plane API, or integration with MiniStack as a drop-in backend. MiniStack's proxy targets `(host, port)`; micro-dsql could be launched instead of the `postgres:16-alpine` container.
- Distribution: a static binary, a Docker image, and a GitHub Action.

## Go / no-go criteria (end of Phase 0 / Phase 1)

**Go if:**
- Mainstream drivers (psycopg, asyncpg, pgx, JDBC, node-pg) do CRUD and transactions, or the blockers are fixable in about a week.
- The OCC experiments show conflicts surface as 40001 and nothing ends up deadlocked or corrupted. Timing differences are acceptable at this point.
- At least ~60% of Dreamescaper's probes pass after Phase 1, with a credible path for the rest.

**No-go / pivot if:**
- MVCC turns out to be unstable under concurrent PG-frontend load: crashes, wrong results, or `Busy` storms that can't be tamed.
- Driver incompatibility runs deep: the extended protocol or catalogs need a rewrite.
- Upstream churn makes the fork unmaintainable.

**Pivot target:** the same policy, sys and OCC-shaping layer in Rust, on **embedded PG16** (`postgresql_embedded`, no Docker) behind a pgwire proxy. That keeps the single-binary story and exact PG16 fidelity, but loses native optimistic concurrency. **Design the policy layer to be backend-agnostic from day one**: it works on the pg_query AST and per-session state, not on Turso internals. It can then be reused in either case.

## Open questions / decisions for the user

1. **Control plane: build one or not?** Options: keep micro-dsql DB-only and let MiniStack, Floci or moto supply the API; or plug into MiniStack as its DSQL backend.
2. **Fork hosting and upstreaming policy.** Who owns the turso fork? Do we aim to upstream aggressively?
3. **Fidelity target.** Use Dreamescaper's golden record as the acceptance spec? It's MIT; check whether vendoring it or contributing back is preferred. Do we also want our own live-cluster probe runs? That needs an AWS account.
4. **Language and distribution** (assumed: a Rust binary plus a Docker image).
5. **Name.** The working name is `micro-dsql`.
