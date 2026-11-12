# Implementation Plan: M4 — OCC fidelity

**Branch**: `007-m4-occ-fidelity` | **Date**: 2026-11-12 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/007-m4-occ-fidelity/spec.md`

## Summary

Make micro-dsql's optimistic concurrency look like Aurora DSQL's, entirely in micro-dsql's own
crates (no Turso patch; evidence in [research.md](research.md) §1):

1. **Autocommit DML as a single-statement transaction** (server): wrap it in `BEGIN … COMMIT`
   (PostgreSQL `BEGIN` is already Turso's `BEGIN CONCURRENT`), retry residual lock contention for a
   bounded time, enforce the 3,000-row limit before `COMMIT`; run idle multi-statement strings as one
   implicit transaction. Remove M2's blanket `55P03 → OC000` mapping (residual contention → OC001).
2. **Doomed transaction**: on a write-time data conflict inside an explicit transaction, roll back,
   re-open, re-run the statement once, flag the transaction; its `COMMIT` fails with OC000.
3. **Snapshot at BEGIN**: a zero-row catalog read right after `BEGIN` pins the snapshot; a DDL as
   first statement gets a fresh, unpinned transaction.
4. **`SELECT … FOR UPDATE`** as a write-set touch (`UPDATE t SET c = c WHERE …`) followed by the
   plain read; `FOR KEY SHARE` accepted as a plain read; its conflicts and FK key-share stay
   documented differences.

A two-session OCC suite (`tests/occ/`) pins every emulated cell of research/01 §2's matrix and
every knowingly different one.

## Feasibility verdicts (items 1–4)

| Item | Verdict | Evidence (research.md) | Residual difference |
|---|---|---|---|
| 1 Autocommit DML as a concurrent single-statement transaction | **Doable in our layer** | E-P2a vs E-P2b: `55P03` 391 → 0 of 1,200; E-P9: bounded retry removes the `Busy` left under concurrent DDL | Under concurrent DDL, contention inside explicit transactions → OC001 (never `55P03`) |
| — Autocommit row limit (M2 gap) | **Fixed by item 1** | E-P7: `n_change` known before `COMMIT` | FK cascade rows still uncounted (unchanged) |
| — Multi-statement atomicity (M2 gap) | **Fixed by item 1** | wrapper is the same mechanism | strings with explicit `BEGIN`/`COMMIT` unchanged |
| 2 Doomed transaction (deferred OC000) | **Doable in our layer, approximate** | E-P6 (block aborted after conflict), E-P6b (rollback/begin/replay works) | later *writer* loses (DSQL: later committer); earlier writes lost and fresh snapshot after the conflict; first-writer rollback still dooms |
| 3 Snapshot at BEGIN | **Doable in our layer** | E-P3, E-P10 (table read pins; `SELECT 1` does not); E-P4/E-P10b (DDL needs a fresh transaction) | tables created after `BEGIN` invisible (`42P01`) |
| 4a FOR UPDATE as a write-set touch | **Doable in our layer** | E-P5–P5e (self-assignment UPDATE conflicts exactly like a write, not with other rows) | single table only; `LIMIT` ignored for the touch |
| 4b FOR KEY SHARE, FK implicit KEY SHARE | **Out of reach in M4 → documented difference** | needs a read-lock set in `turso_core` MVCC commit validation (research/02 §3.4, research/04 §B: L); E-P8 shows an orphan child today | FOR KEY SHARE never conflicts; concurrent parent DELETE + child INSERT both commit |

**Turso patch needed: no.** All hooks are public (`PgConnection::prepare`, `transaction_state`,
`turso_pg::Error::core_error`, `Statement::n_change`, `Connection::current_schema`). `patch-crate` is
not wired by 007. If item 4b is ever taken on, it is a core patch and would be the subject of its own
feature, gated by Principle II's git-dependency constraint (patch-crate copies only the crate
subdirectory; Turso manifests inherit `*.workspace = true`), which is still unresolved.

## Technical Context

**Language/Version**: Rust stable (edition 2021)

**Primary Dependencies**: `pg_query` 6.1 (policy; `NodeEnum::deparse` for the FOR UPDATE edits),
`pgwire` 0.36, Turso crates pinned at `d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5` (server only),
`tokio`, `tracing`; tests `tokio-postgres`

**Storage**: N/A (in-memory engine database per test server)

**Testing**: `cargo test --workspace` — policy unit tests (no engine), server unit tests, smoke
integration tests, and the new `occ` integration target (`tests/occ/`); `tests/psql/run.sh` layer 1
(gate) and probe `26_locking_reads` (report-only)

**Target Platform**: macOS and Linux

**Project Type**: Rust workspace (`crates/policy`, `crates/server`, `crates/micro-dsql`)

**Performance Goals**: autocommit DML ≤ 2× today's single-session latency (SC-006): two extra
engine statements (`BEGIN`, `COMMIT`) per autocommit DML

**Constraints**: policy crate depends only on `pg_query` (Principle III); no Turso patch; layer-1
expected outputs unchanged; probe expected outputs never re-captured; tests bind port 0 (manual
runs use 15507), never 5432; no Docker

**Scale/Scope**: 2 new policy modules, 1 new server module, ~25 OCC scenarios, edits to 7 shared files

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle / gate | Status |
|---|---|
| I. PoC scope | PASS. M4 is in docs/plan.md §4; "exact OCC timing" is a non-goal, so differences are documented, not chased. No TLS/auth/control plane. |
| II. Own repo, pinned upstream | PASS. No Turso patch (research.md D5); the "try our crates first" attempt succeeded for items 1, 2, 3, 4a; 4b is recorded as a documented difference with the reason a patch would be needed. The vendored `lib.rs` header's change list gets an M4 entry. |
| III. Backend-agnostic policy | PASS. New policy state (doomed, implicit, pinned, begin text) and the FOR UPDATE plan are plain values built from the `pg_query` AST; the touch column is supplied by the server. `crates/policy/Cargo.toml` unchanged. |
| IV. Test-first; gates `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `tests/psql/run.sh` against micro-dsql | PLANNED. Every behaviour starts as a failing test (tasks.md). Existing tests are not weakened: `dsql_conflicts.rs` already accepts statement-or-COMMIT failure; `autocommit_storm_reports_only_oc000` keeps holding (no DDL in it); `40_conflict` still sees a `40001` (now at B's COMMIT) with A's value kept. The `LockBusy → OC000` unit test in `policy::errors` is replaced by `LockBusy → OC001` (a requirement change, FR-004, not a loosening). |
| V. Known-gap policy | PLANNED. docs/known-gaps.md: B10, "Autocommit row limit", "Multi-statement strings" closed when their tests pass; B11 refined; new entries for the known differences (spec §Known differences), each pointing at its `tests/occ/` scenario. |
| Repository constraints | PASS. `research/` untouched (findings here); no machine-specific paths (manual commands use `127.0.0.1:15507` and `PG*` variables); no tool attribution. |
| Scope questions | None. |

**Post-design re-check**: PASS. Design adds two policy modules, one server module, fields on
existing types, one integration-test target; no new crate or dependency.

## Design

### Execution path

```text
lib.rs (simple / extended) ──► occ::run(conn, policy, plan, opts, engine)          [new, 007]
   │
   ├─ Idle & plan is DML or FOR UPDATE ─► implicit single-statement transaction:
   │      loop ≤ attempts: BEGIN → dsql::run_planned(… engine') → rows ≤ limit? → COMMIT
   │      Busy at any step: ROLLBACK, back off, retry; other error: ROLLBACK, report
   ├─ Active, pinned, untouched & plan is DDL ─► ROLLBACK; <begin text>; then run
   ├─ otherwise ─► dsql::run_planned(… engine')
   │      engine' = engine wrapped with:
   │        · FOR UPDATE: touch (column chosen from the schema) then read
   │        · DataConflict in explicit txn & defer ─► ROLLBACK; <begin text>; pin;
   │          mark_doomed; replay once (or empty result)
   └─ after success: plan is BEGIN & now Active & snapshot_at_begin ─► pin read

simple query, Idle, ≥2 statements, no txn control, contains DML
   ─► BEGIN; policy.begin_implicit(); each statement via occ::run; COMMIT
      (any error: ROLLBACK; policy.failed(Idle))
```

`dsql::run_planned` is reused unchanged; `occ::run` wraps it and passes a wrapped engine closure.

### Policy additions (contracts/occ-api.md)

- `policy::occ` (new): `OccTxn { doomed, implicit, pinned, touched, begin_sql }` embedded in
  `TxnState`; `SessionPolicy::{begin_implicit, mark_doomed, record_begin, mark_pinned,
  needs_fresh_snapshot, check_implicit_rows, is_doomed, is_implicit}`; `admit` gains one branch:
  COMMIT of a doomed transaction → `RollbackThenRefuse(oc000())` (after the age check).
- `policy::locking` (new): `LockingRead { strength, read_sql, touch: Option<TouchPlan> }` from a
  `SelectStmt` with a locking clause; `TouchPlan::render(column) -> String`; refusal
  `locking_read_form` for unsupported forms. `StatementPlan` gains `locking: Option<LockingRead>` and
  sets `engine_sql = read_sql`.
- `policy::errors`: `LockBusy` → `oc001()`; new constructor `locking_read_form()`.

### Seams with feature 006 (M3)

006 is merged into `main` first; 007 merges `main` before implementing (task T001).

| Owned by 006 | Owned by 007 |
|---|---|
| `policy::rewrite` (pre-parse, AST text edits), `Reply::Rows`, `sys.*` emulation, DDL rules, any `patch-crate` wiring | autocommit execution, implicit transactions, transaction start/snapshot pin, the doomed flag in `TxnState`, FOR UPDATE / FOR KEY SHARE handling, removing `LockBusy → OC000` |

If 006 adds `Admission`/`Reply` variants, `occ::run` treats every non-`Run` admission by delegating
to `dsql::run_planned` unchanged, so new variants need no 007 change.

**Shared files 007 touches** (additions kept in new functions/modules; edits listed):

| File | 007 change |
|---|---|
| `crates/server/src/lib.rs` | `mod occ;` · `ServerOptions.occ` · pass options to the handler · replace the two `dsql::run_planned(…)` call sites by `occ::run(…)` · implicit-transaction wrapping in `run_simple_query` and `do_query` (simple protocol) · M4 entry in the header's change list |
| `crates/server/src/dsql.rs` | none planned (the helper `failure_of(&PgWireError)` goes in `occ.rs`) |
| `crates/server/Cargo.toml` | `[[test]] name = "occ"` |
| `crates/policy/src/session.rs` | `TxnState` gains `occ: OccTxn`; doomed-COMMIT branch in `admit`; new methods delegating to `policy::occ` |
| `crates/policy/src/statement.rs` | `locking` field; `engine_sql` from `LockingRead::read_sql`; refusal merge |
| `crates/policy/src/errors.rs` | `LockBusy` arm; `locking_read_form()` |
| `crates/policy/src/lib.rs` | `pub mod occ; pub mod locking;` + re-exports |
| `docs/known-gaps.md`, `README.md` | gap updates; OCC section |

New files: `crates/policy/src/occ.rs`, `crates/policy/src/locking.rs`, `crates/server/src/occ.rs`,
`tests/occ/*.rs`, `tests/occ/README.md`.

**Seam names** (for the merge): `occ::run` (server entry), `SessionPolicy::mark_doomed`,
`TxnState.occ`, `StatementPlan.locking`, `ServerOptions.occ`, `EngineFailure::LockBusy → oc001`.
**patch-crate seam**: 007 wires none; if 006 wires it, 007 inherits it unchanged.

### Known differences carried to docs/known-gaps.md

Who loses (later writer vs later committer); first-writer rollback still dooms; doomed transaction
loses its earlier writes and reads a fresh snapshot; FOR KEY SHARE never conflicts; FK key-share
missing (orphan child, E-P8); B11 schema-wide conflicts (and OC000 on unrelated writers during DDL
churn, E-P9); FOR UPDATE single-table / LIMIT over-touch; tables created after BEGIN invisible;
residual contention inside explicit transactions reported as OC001.

## Project Structure

### Documentation (this feature)

```text
specs/007-m4-occ-fidelity/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/occ-api.md
├── checklists/requirements.md
└── tasks.md
```

### Source Code (repository root)

```text
crates/policy/src/
├── occ.rs          # new: OccTxn, implicit/doomed/pinned bookkeeping
├── locking.rs      # new: FOR UPDATE / FOR KEY SHARE planning (AST + deparse)
├── session.rs      # TxnState.occ, doomed COMMIT
├── statement.rs    # StatementPlan.locking
└── errors.rs       # LockBusy → OC001, locking_read_form
crates/server/src/
├── occ.rs          # new: occ::run, implicit txns, retry, pin, doomed replay, touch
└── lib.rs          # call sites, ServerOptions.occ, multi-statement wrapping
tests/occ/
├── main.rs         # target registered by crates/server/Cargo.toml
├── matrix.rs       # one scenario per emulated cell (research/01 §2)
├── autocommit.rs   # storms, row limit, multi-statement atomicity, residual contention
├── snapshot.rs     # snapshot at BEGIN
├── locking.rs      # FOR UPDATE / FOR KEY SHARE
├── differences.rs  # knowingly different cells, each naming its known-gaps entry
└── README.md
```

**Structure Decision**: existing workspace; the OCC suite sits at `tests/occ/` per docs/plan.md §3
and is compiled by the server crate so `cargo test --workspace` runs it.

## Complexity Tracking

No constitution violations.
