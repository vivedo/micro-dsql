# Implementation Plan: M3 — DDL that ORMs and migrations emit

**Branch**: `006-m3-orm-ddl` | **Date**: 2026-11-12 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/006-m3-orm-ddl/spec.md`

## Summary

Make the DDL that DSQL migrations (and Drizzle, the M3 target ORM) emit run on micro-dsql, with
DSQL's answers and refusals, entirely in our own crates (no Turso patch planned):

1. **ASYNC** (`CREATE [UNIQUE] INDEX ASYNC`, `ALTER TABLE ASYNC … VALIDATE`): a same-length
   pre-parse blanking in `policy::rewrite::pre_parse`; the server builds/validates synchronously and
   answers with a `job_id` row (`Reply::Rows` / `Outcome::Rows`).
2. **`sys.*`**: a real `sys.jobs` table created at database open and written by the server in the
   session's transaction; `CALL sys.wait_for_job` and `sys.dsql_major_version()` recognised on the
   AST and answered by micro-dsql (live-DSQL forms, not MiniStack's).
3. **Identity and CACHE**: rules (DSQL's mandatory CACHE, bigint-only identity) plus rewrites
   (CACHE → 1; identity → prelude `CREATE SEQUENCE` + `DEFAULT nextval(…)`), sequence cleanup on
   DROP TABLE.
4. **`ADD CONSTRAINT … NOT VALID` / `VALIDATE`**: the engine has no ADD CONSTRAINT; the server adds
   single-column FK/CHECK constraints through the core engine's `ALTER COLUMN` path
   (`PgConnection::inner().prepare_translated_stmt`), gated by a spike; VALIDATE runs a check query
   and records a completed/failed job.
5. **Deferred DSQL DDL rules** from 005 D9 (sync CREATE INDEX, CONCURRENTLY, USING, >8 keys, ADD
   COLUMN with constraint, ALTER COLUMN TYPE, SET NOT NULL, DROP PK column, ADD CONSTRAINT forms,
   sync VALIDATE, `sys` writes).
6. **Exit checks outside the Rust gate**: `scripts/drizzle-check.sh` over `tests/orm/drizzle/`
   (DSQL-shaped Drizzle project) and `scripts/conformance.sh` (Dreamescaper golden probes fetched
   at the pinned commit; report in `reports/`).

Evidence for every decision is in [research.md](research.md); the description's facts that differ
from the code are in research.md §4.

## Technical Context

**Language/Version**: Rust stable (edition 2021); Node ≥ 22 (Drizzle check only); Go ≥ 1.27.1
(conformance script only)

**Primary Dependencies**: `pg_query` 6.1 (policy's only dependency), `pgwire` 0.36, Turso crates at
`d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5` (server only; **adds `turso_parser`** at the same rev for
the core `ALTER COLUMN` AST, D7), `uuid` (job ids, already a server dependency). Drizzle check:
`drizzle-orm 0.45.4`, `drizzle-kit 0.31.11`, `pg 8.23.1` (lockfile). Conformance: upstream
`Dreamescaper/dsql-emulator` at `38a33bf283d7670b30dc959e8f38d0cf4eef1ddc`, fetched at run time.

**Storage**: `sys.jobs` is an engine table (persisted with `--data`); no other state

**Testing**: `cargo test --workspace`: policy unit tests (no engine); integration tests in
`crates/server/tests/smoke/` (new `dsql_ddl.rs`, `dsql_jobs.rs`); `tests/psql/run.sh` layer 1
(gate) and probes (report-only); exit checks by script

**Target Platform**: macOS and Linux

**Project Type**: Rust workspace (`policy`, `server` libraries; `micro-dsql` binary) plus two
scripts and one Node test project

**Performance Goals**: async DDL costs the synchronous DDL plus one job insert (SC-006); no extra
parse per statement beyond the pre-parse scan (only statements containing the `ASYNC` token are
rescanned)

**Constraints**: Principle III (policy has no engine types); no Turso patch unless the D7 spike
fails; no change to 007-owned behaviour (autocommit, txn start, doomed flag, FOR UPDATE, 55P03);
probe expected files never re-captured; port 5432 never used; no machine-specific paths

**Scale/Scope**: ~20 new rule ids, 4 rewrites, 6 new policy/server modules, 2 new integration test
files, 2 scripts, 1 Node project

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle / gate | Status |
|---|---|
| I. PoC scope | PASS. DDL fidelity is M3 scope (docs/plan.md §4). No TLS/auth/control plane. `information_schema` and ORM introspection (`drizzle-kit push`) stay non-goals: the Drizzle check uses generated SQL, not introspection. |
| II. Own repo, pinned upstream; patches last | PASS (planned). `sys.*`: done in our crates (research D2), so the "probably needs the first Turso patch" expectation of docs/plan.md does not materialise. ADD CONSTRAINT: attempted in the server layer through a public core API (D7), gated by spike T030. A patch is used only if the spike fails; then the feature stops for an owner decision (blocking question, below) because the first patch must also solve the `patch-crate` git-workspace constraint. New dependency `turso_parser` uses the same pinned rev (one rev for every Turso crate). |
| III. Backend-agnostic policy | PASS. New policy modules (`identity`, `ddl_rules`, `jobs`) work on the AST and plain values; job ids are encoded from a `u128` the server supplies; the engine-facing work (`sys.rs`, `ddl.rs`, `constraints.rs`) is in the server. All rules and rewrites unit-tested without an engine. |
| IV. Test-first; gates `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `tests/psql/run.sh` vs micro-dsql | PLANNED. Each rule/rewrite gets a failing unit test and integration test first (tasks.md). Layer-1 expected outputs unchanged (they use no index, identity or ALTER). The two exit scripts are additional checks, not gates, and are not run by `cargo test`. |
| V. Known-gap policy | PLANNED. docs/known-gaps.md: B12 narrowed; new entries (GENERATED ALWAYS, synchronous jobs, no invalid index, `oid` described as `integer`, `sys` visible as a schema/table, CACHE stored as 1, NOT VALID forms refused, `convalidated` if not expressible, `main.` in 42P01 texts, out-of-scope ALTER forms). |
| Repository constraints | PASS. `research/` untouched; golden probes fetched at a pinned commit recorded in `scripts/conformance/UPSTREAM`, never committed; `reports/`, `target/`, `node_modules/` gitignored; no tool attribution; no absolute paths. |
| Scope questions | One reinterpretation (not a scope change): "Drizzle's migrations run cleanly" cannot mean the stock migrator, which fails on live DSQL (research §2); spec Clarifications record the reading. |

**Post-design re-check**: PASS, with one conditional item: if spike T030 fails, Principle II's
patch path applies and needs the owner (see Blocking questions).

## Design

### Execution path

```text
simple Query: pre_parse(whole text) → syntax check on blanked text → split ORIGINAL (scanner) → plans
extended Parse: StatementPlan::new(sql) (pre_parse inside) → Describe uses plan.reply_columns()

dsql::run_planned (005) — Admission::Run{sql} arm gains one dispatch:
    plan.job  = IndexBuild | Validate | WaitForJob  → sys::run_job_statement
    plan.ddl or plan.prelude non-empty              → ddl::run_ddl_statement
    otherwise                                       → run_engine(sql)   (unchanged)
  Admission::Respond(Reply::Rows)                    → respond() builds Outcome::Rows
complete()/failed() called once per client statement, as today.

sys::run_job_statement(IndexBuild): run_engine(engine_sql) → pg_class lookup (oid, name)
    → purge old jobs → INSERT INTO sys.jobs … → Outcome::Rows{tag "CREATE INDEX", job_id}
    unique build fails with 23505 → INSERT failed job (details) → Outcome::Rows (statement succeeds, as DSQL)
sys::run_job_statement(Validate): constraints::validate → completed/failed job → Outcome::Rows{"ALTER TABLE"}
sys::run_job_statement(WaitForJob): SELECT status FROM sys.jobs WHERE job_id = $1 → succeeded row | 22023
ddl::run_ddl_statement: prelude… → engine_sql → (DropTable: drop identity sequences)
    on failure while Idle → cleanup_on_failure; AddConstraintNotValid → constraints::add_not_valid;
    DropColumn → constraints::check_drop_column (drop_pk_column) before running
```

Server-issued statements (job insert, catalog lookups, prelude, cleanup) go straight to
`conn.prepare`, never through the policy; they are not counted by the discipline rules.

### Key decisions (details and alternatives in [research.md](research.md) §5)

- D1 blank `ASYNC` (same length) instead of deleting it; split the original string.
- D2 `sys.jobs` as a real engine table + AST-recognised `CALL`/`SELECT` forms; no Turso patch.
- D3 `TextEdit` gains `remove`; plans carry `prelude`, `cleanup_on_failure`, `job`, `ddl`.
- D4 CACHE rules then CACHE → 1.
- D5 identity → sequence + `DEFAULT nextval`; DROP TABLE drops `<t>_<c>_seq` defaults.
- D6 job ids: 128 random bits, base-32; `Reply::Rows`.
- D7 NOT VALID FK/CHECK via core `ALTER COLUMN` (spike-gated; fallback = patch + owner).
- D8 VALIDATE by check query; failed validation = failed job, statement succeeds.
- D9 tooling: `tests/orm/drizzle` + `scripts/drizzle-check.sh`; `scripts/conformance.sh` + our Go
  comparer copied into the fetched upstream tree; reports to `reports/`.
- D10 new rules in `ddl_rules.rs`, registered in `RULES` with one-line rows.
- D11 out-of-scope ALTER forms and async-only behaviours become known gaps.
- D12 never open a transaction on the client's behalf (007 owns autocommit).

### Where the exit checks live and how they run

| Check | Files | How it runs | Gate? |
|---|---|---|---|
| Drizzle | `tests/orm/drizzle/{package.json, package-lock.json, drizzle.config.ts, src/schema.ts, drizzle/0000_*.sql, drizzle/meta/*, migrate-dsql.mjs, smoke.mjs, stock.mjs, README.md}`, `scripts/drizzle-check.sh` | by hand against a running micro-dsql (`PG*` variables); `npm ci` uses the lockfile; `node_modules/` gitignored | no (exit criterion; not in `cargo test`) |
| Golden probes | `scripts/conformance.sh`, `scripts/conformance/{UPSTREAM, compare.go}`; checkout in `target/conformance/` | by hand against a running micro-dsql; fetches upstream at the pinned SHA; writes `reports/conformance-<date>.md` | no (exit criterion; threshold 60%, overridable) |

Both scripts exit `2` when a prerequisite (Node/npm, Go, git/network, reachable target) is missing,
so a missing toolchain is never reported as a fidelity failure. No CI workflow is added (the
repository has none since `ff325a3`).

### Turso patch: needed? No (planned), with one gated risk

- `sys.*`: **no patch**. research/02 §5 assumed `sys` functions must be registered in the private
  `PostgresDialect` modules. DSQL's real interface does not need engine functions: `sys.jobs` is a
  table the engine can hold (`CREATE SCHEMA sys` works), `wait_for_job` is a procedure called with
  `CALL` (answered by micro-dsql), and `dsql_major_version()` is used in two fixed forms.
- `ADD CONSTRAINT … NOT VALID`: **no patch if spike T030 succeeds**. The core engine already has the
  operation (`AlterTableBody::AlterColumn` with a full column definition, including `REFERENCES`
  and `CHECK`), reachable from our server through the public
  `turso_core::Connection::prepare_translated_stmt`. Only the PG translator lacks the mapping.
- If T030 fails, the minimal generic patch would be in `turso_pg_parser`'s translator
  (`postgres/parser/translator.rs`, `AtAddConstraint` → core `AlterColumn` for single-column FK/CHECK
  with `NOT VALID` accepted). Wiring it requires the first `patch-crate` setup: list the crate in
  `[workspace.metadata.patch]`, `cargo patch-crate` copies it to `target/patch/`, and a
  `[patch."https://github.com/tursodatabase/turso"]` entry points `turso_pg_parser` there. Because
  patch-crate 0.1.13 copies only the crate subdirectory of a git workspace (issue #18) and
  `turso_pg_parser`'s manifest inherits `*.workspace = true` fields, the copy does not build as is;
  the constitution requires the first patch to demonstrate a working setup or to amend the
  constitution first. This is **not** assumed to work; it is a blocking question if reached.

### Seams with 007 (M4), shared files

006 owns: `policy::rewrite::{pre_parse, PreParsed, TextEdit, apply_edits, REWRITES}`,
`policy::{identity, ddl_rules, jobs}`, `Reply::Rows`/`ReplyColumn`/`ColumnType`, `server::{sys,
ddl, constraints}`, the M3 rows of `RULES`, `sys.*` emulation, DDL rules.

007 owns: autocommit execution, transaction start/snapshot, `TxnState` (doomed flag), `FOR
UPDATE`/`FOR KEY SHARE`, the `55P03` mapping. 006 does not edit them.

| Shared file | 006 change (kept small; new logic lives in new modules) |
|---|---|
| `crates/policy/src/session.rs` | add `Reply::Rows`, `ReplyColumn`, `ColumnType`; one `admit` arm for `JobAction::MajorVersion` in the final match (step 10). No `TxnState`/`complete`/`failed` edits. |
| `crates/policy/src/statement.rs` | new plan fields; pipeline steps 1, 7, 8 call into `rewrite`/`identity`/`jobs`; `reply_columns()` |
| `crates/policy/src/rules.rs` | `check_rules` gains a `RuleContext` argument; new rows referencing `ddl_rules` functions |
| `crates/policy/src/errors.rs` | new constructors only |
| `crates/policy/src/classify.rs` | `CallStmt` → `Other` (if not already) |
| `crates/policy/src/lib.rs` | `pub mod` lines and re-exports |
| `crates/server/src/dsql.rs` | one dispatch branch in the `Admission::Run` arm; `Reply::Rows` arm in `respond` |
| `crates/server/src/lib.rs` | pre-parse before the whole-string syntax check; Describe via `reply_columns()`; `open_database` calls `sys::bootstrap`; header changes list gets an M3 entry |
| `crates/server/Cargo.toml`, `Cargo.toml` | `turso_parser` at the pinned rev |
| `docs/known-gaps.md`, `README.md`, `tests/psql/README.md` | entries/rows for M3 |

Patch-crate seam: **006 does not wire `patch-crate`**. If D7's fallback were needed, 006 would
stop and ask; if 007 needs a patch for autocommit/snapshot first, 007 owns the first wiring and 006
reuses it. Only one feature wires it.

### Blocking questions

None at plan time. Conditional: if spike T030 shows the core `ALTER COLUMN` path cannot add an
enforced single-column FK/CHECK on a PG-dialect connection, the owner must decide between (a) the
first Turso patch (with the patch-crate constraint resolved or the constitution amended), or (b)
refusing `ADD CONSTRAINT … NOT VALID` (Drizzle FKs then fail; exit criterion not met).

## Project Structure

### Documentation (this feature)

```text
specs/006-m3-orm-ddl/
├── spec.md
├── plan.md                    # this file
├── research.md                # reproduced behaviour, Drizzle, golden baseline, fact check, decisions
├── data-model.md              # PreParsed, TextEdit, plan fields, Job, identity sequence, NOT VALID
├── quickstart.md              # gates, probes, exit checks, spot checks
├── contracts/
│   ├── rule-catalogue-m3.md   # new rule ids, texts, sources; rewrites; micro-dsql answers
│   ├── policy-api-m3.md       # pipeline/plan/session deltas, server seams, 007 boundary
│   └── tooling.md             # drizzle-check.sh and conformance.sh contracts
├── checklists/requirements.md
└── tasks.md                   # /speckit-tasks
```

### Source Code (repository root)

```text
crates/policy/src/
├── rewrite.rs        # pre_parse → PreParsed (ASYNC blanking); TextEdit.remove; cache_one
├── identity.rs       # new: identity desugaring
├── ddl_rules.rs      # new: M3 rule functions
├── jobs.rs           # new: job ids, JobAction/DdlAction extraction, sys.* forms
├── statement.rs      # plan fields, reply_columns()
├── session.rs        # Reply::Rows (+ MajorVersion answer)
├── rules.rs          # RuleContext, new RULES rows
├── errors.rs         # M3 constructors
└── classify.rs, lib.rs

crates/server/src/
├── sys.rs            # new: bootstrap, run_job_statement, wait_for_job, purge
├── ddl.rs            # new: prelude/cleanup, DROP TABLE sequence cleanup, dispatch to constraints
├── constraints.rs    # new: add_not_valid (core ALTER COLUMN), validate, check_drop_column
├── dsql.rs           # dispatch branch, Reply::Rows
└── lib.rs            # pre-parse in the simple path, Describe, bootstrap call

crates/server/tests/smoke/
├── dsql_ddl.rs       # new: rules + identity + NOT VALID/VALIDATE + ALTER refusals
└── dsql_jobs.rs      # new: ASYNC index, job rows, sys.jobs, CALL wait_for_job, major version, extended protocol

tests/orm/drizzle/    # new: DSQL-shaped Drizzle project (lockfile; node_modules ignored)
scripts/drizzle-check.sh
scripts/conformance.sh
scripts/conformance/UPSTREAM, scripts/conformance/compare.go
docs/known-gaps.md, README.md, tests/psql/README.md
```

**Structure Decision**: no new crate. docs/plan.md §3 sketches `crates/sys/`; the emulation needs
the engine connection and is one module, so it lives in `crates/server/src/sys.rs` (Complexity
Tracking). The conformance runner lives in `scripts/` rather than `tests/conformance/` because it
is a script with its own toolchain, not a test the gate runs.

## Complexity Tracking

| Deviation | Why needed | Simpler alternative rejected because |
|---|---|---|
| `sys` emulation as a server module, not the `crates/sys/` of docs/plan.md §3 | It reads and writes an engine table on the session's connection; a crate would wrap one module and depend on the server's engine types | A crate adds a workspace member and an API boundary with no reuse |
| New Turso dependency `turso_parser` (same pinned rev) | The core `ALTER COLUMN` AST types for D7 | Building SQLite-dialect text is impossible: the connection parses with the PG dialect |
| Go and Node toolchains for the exit scripts | Upstream's recorder/comparer are Go; Drizzle is Node | Reimplementing the upstream suite in Rust would drift from the live-DSQL record; both stay outside the Rust gate |
