# micro-dsql — research

Research for a standalone **Aurora DSQL emulator built on Turso's Rust Postgres frontend**, targeting local tests and CI. Researched on 2026-11-12. No implementation yet.

| File | Contents |
|---|---|
| [01-dsql-behaviour-spec.md](01-dsql-behaviour-spec.md) | DSQL behaviour the emulator must reproduce: connection and auth, OCC, transaction limits, SQL surface, async jobs, catalogs, the control plane, and items still to verify |
| [02-turso-postgres-assessment.md](02-turso-postgres-assessment.md) | Code-level assessment of `tursodatabase/turso/postgres` at `d5fdf4d` (architecture, wire server, MVCC, SQL coverage, extension points, maturity, build) |
| [03-landscape.md](03-landscape.md) | Existing DSQL emulators (MiniStack, Floci, Dreamescaper, LocalStack, moto…), what the official connectors and ORMs require, and alternative bases |
| [04-gap-analysis.md](04-gap-analysis.md) | Requirement-by-requirement gaps, with effort and where each fix lives (our layer, fork, or upstream) |
| [05-architecture-and-plan.md](05-architecture-and-plan.md) | Proposed architecture, phased PoC, go/no-go criteria, open decisions |
| `sources/` | DSQL User Guide text (Nov 2026), API reference pages, and the botocore `dsql` model and token signer, for offline grep |

All Turso code references are to `tursodatabase/turso` at commit `d5fdf4d` (2026-11-11).

## Executive summary

**Verdict: viable as a differentiated, Docker-free emulator. Not viable as a high-fidelity PG16 replica in the short term.** The next step is a 2–3 day spike to validate the findings, which so far come only from reading code.

**Why Turso fits**
- The Postgres frontend runs **every transaction as an MVCC `BEGIN CONCURRENT`**: snapshot isolation with optimistic, row-level conflict detection that returns **SQLSTATE 40001**. That is structurally DSQL's model.
- Every existing emulator is PG16 behind a proxy, so it is lock-based.
- SQL is parsed by the real libpg_query and the translator fails closed. A DSQL policy layer can work on the true PG AST.
- It is a single Rust binary with an in-memory DB and millisecond startup, under the MIT licence.
- pgwire 0.36 already supports TLS, direct TLS (ALPN) and SNI, so those later improvements are cheap.

**Why it's hard**
- **OCC timing differs.** Turso fails the *second writer at the UPDATE/DELETE*. DSQL fails the *second committer at COMMIT*. Also:
  - autocommit writes are exclusive and can return `55P03`;
  - the snapshot is taken at the first statement, not at BEGIN;
  - `FOR UPDATE` and `FOR KEY SHARE` are not supported.
  - Fixing these needs patches to the frontend or to MVCC in core.
- **DSQL-specific DDL that ORMs emit is rejected today:**
  - `CREATE INDEX ASYNC` (libpg_query can't parse it; fixable with a pre-parse rewrite)
  - `IDENTITY`
  - sequence `CACHE 65536`
  - `NOT VALID`
  - most `ALTER COLUMN` operations
- **The PG-compatibility long tail:**
  - no `information_schema`
  - partial JSONB
  - millisecond timestamptz
  - float-backed bare numeric
  - no interval arithmetic
  - text-only bind parameters
  - missing window functions
- **Upstream maturity and churn.** The frontend is "experimental", pre-1.0 and unpublished (except the parser crate), with about 500 commits in `postgres/` over six weeks, many of them AI-authored. We would carry a pinned fork with a small patch set and upstream generic fixes.
- **DSQL is a moving target.** Foreign keys, sequences, IDENTITY, json columns, views and partial/expression indexes all arrived in 2026.

**Competitive landscape**
- MiniStack, Floci, Dreamescaper/dsql-emulator and LocalStack (paid) already emulate DSQL on real PG16.
- Floci leads on connection fidelity.
- Dreamescaper has a **242-probe golden record from a live cluster**, which is the best available acceptance suite.
- micro-dsql's niche: **no Docker, instant start, native optimistic concurrency**, with an honest "DSQL-legal SQL + OCC behaviour" promise rather than exact PG16.

**Recommendation**
1. Phase 0 spike: build and run `tursopg`, smoke-test the main drivers, run the OCC experiments, and baseline Dreamescaper's probes.
2. Go/no-go.
3. Phase 1: a minimal DSQL-shaped DB with the policy layer, ASYNC jobs, transaction rules and error shaping.
4. Phase 2: OCC fidelity.

Keep the policy layer backend-agnostic so we can pivot to embedded PG16 if Turso's frontend doesn't hold up. TLS, auth and the control plane are deferred, per the scope decision.
