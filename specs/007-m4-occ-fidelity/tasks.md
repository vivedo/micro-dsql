---

description: "Task list for 007 M4 — OCC fidelity"
---

# Tasks: M4 — OCC fidelity

**Input**: Design documents from `specs/007-m4-occ-fidelity/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/occ-api.md, quickstart.md

**Tests**: Required. Constitution IV is test-first: every behaviour task is preceded by a failing
test task. Tests are never weakened; the only expectation that changes is `LockBusy → OC000`
becoming `LockBusy → OC001` (FR-004, a requirement change).

**Organization**: by user story (spec.md). Order of value per effort: US1 → US2 → US3 → US4; US5
(the suite as exit criterion) closes the feature.

**Revision** (2026-11-12, after merging 006): existing ids kept; T001 done; T006, T008, T012,
T013, T017, T018, T026, T043 reworded for the merged code; T070–T074 added (placed in their phase;
ids are new, not renumbered).

**Markers**: `[orchestrator]` / `[owner]` = cannot be done by the implementing agent (Docker
differential, port 5432, live DSQL, manual walkthroughs).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: US1–US5 from spec.md

---

## Phase 1: Setup

- [x] T001 Merge local `main` (with feature 006 merged, `3370541`) into `007-m4-occ-fidelity` (done: merge commit `817ec1d`, no conflicts). Before implementing, merge `main` again if it moved, and confirm `cargo test --workspace` and `tests/psql/run.sh` (server on `127.0.0.1:15507`) are green before any 007 change
- [ ] T002 Re-run the research.md §1.1 two-session wire matrix on the merged tree (the engine probes were re-run already, research.md §1.3) and append any change to `specs/007-m4-occ-fidelity/research.md` §1.3 (do not edit `research/`)
- [ ] T003 Register the OCC suite target: add `[[test]] name = "occ"`, `path = "../../tests/occ/main.rs"` to `crates/server/Cargo.toml`; create `tests/occ/main.rs` that includes the smoke harness with `#[path = "../../crates/server/tests/smoke/common.rs"] mod common;` and declares modules `matrix`, `autocommit`, `snapshot`, `locking`, `differences` (empty files); `cargo test -p micro-dsql-server --test occ` compiles and runs 0 tests

---

## Phase 2: Foundational (blocking)

- [ ] T004 Add a two-session step helper to `tests/occ/main.rs` (not to `common.rs`): `Session` wrapping a `tokio_postgres::Client` with `ok(sql)`, `err(sql) -> DbErr`, `value(sql)`, and constants `OC000`/`OC001` texts; plus `TestServer::start_with(ServerOptions { occ: … })` usage once T006 exists
- [ ] T005 [P] Write failing unit tests in `crates/policy/src/occ.rs` (new module, declared in `crates/policy/src/lib.rs`) for `OccTxn` defaults and the data-model.md rules: "`doomed` implies `!implicit`", "`needs_fresh_snapshot(plan)` ⇔ `pinned && !touched && plan.class == Ddl`", "`doomed` survives every statement until the transaction ends"
- [ ] T006 Add `OccOptions { defer_conflicts: true, snapshot_at_begin: true }` and `ServerOptions.occ` (Default) in `crates/server/src/lib.rs`; thread the options from `Server::serve` through `serve_connection` into `TursoPgHandler`; define `occ::BUSY_RETRY = BusyRetry { attempts: 10, budget: 200 ms }` as a constant in `crates/server/src/occ.rs` (not an option: 006's `in_internal_transaction` has no access to the options)
- [ ] T007 Implement `OccTxn` and wire it into `TxnState` (`occ` field) in `crates/policy/src/occ.rs` and `crates/policy/src/session.rs`: record `begin_sql` in `complete()` for a `Txn(Begin)` plan, set `touched` in `complete()`/`failed()` for every other plan; add `SessionPolicy::{begin_sql, is_implicit, is_doomed}`; T005 passes
- [ ] T008 Create `crates/server/src/occ.rs` (declared in `crates/server/src/lib.rs`) with the `Engine` trait, `failure_of(&PgWireError) -> Option<EngineFailure>` (downcast to `turso_pg::Error`, then `dsql::engine_failure`), `PIN_SQL`, and `run(...)` that for now only delegates to `dsql::run_planned`; replace both `dsql::run_planned(...)` call sites in `crates/server/src/lib.rs` (simple `run_simple_statement`, which also handles COPY, and extended `do_query`) by `occ::run(...)` with `Engine` impls wrapping today's closures (`empty()` prepares the statement and returns its header with no rows, or its command tag with 0 rows; never `Outcome::TaggedRows`, which only job statements produce); `occ::run` passes its wrapped closure to `dsql::run_planned`, leaving `dsql::dispatch` (006) unchanged; unit test `failure_of` in `crates/server/src/occ.rs`; all existing tests stay green (including 006's `dsql_ddl`, `dsql_jobs` and `constraints::tests`)

**Checkpoint**: no behaviour change; suite target exists; seams in place.

---

## Phase 3: User Story 1 — Autocommit writes behave like single-statement DSQL transactions (P1) 🎯 MVP

**Goal**: autocommit DML never sees `55P03`; row limit for every autocommit DML; atomic
multi-statement strings; residual contention retried, else OC001.

**Independent Test**: 8 sessions × 150 autocommit statements: 0 insert failures, every error OC000.

### Tests (write first, must fail)

- [ ] T010 [P] [US1] In `tests/occ/autocommit.rs`: storm of 8 sessions × 150 statements (distinct-row INSERT / hot-row UPDATE) asserting 0 INSERT failures, every error equal to `40001` OC000, and final counts consistent (rows = successful inserts + 1, counter = successful updates) — SC-001, research E-P2
- [ ] T011 [P] [US1] In `tests/occ/autocommit.rs`: row limit — autocommit `INSERT … SELECT … generate_series(1,3001)`, `UPDATE` of 3,001 rows, `DELETE` of 3,001 rows each fail with `54000` `transaction row limit exceeded` and leave the table unchanged; exactly 3,000 rows succeeds (server with default limits; use `ServerOptions.limits` with a small `max_rows_per_txn` to keep the test fast where the size is not the point)
- [ ] T012 [P] [US1] In `tests/occ/autocommit.rs`: multi-statement atomicity (FR-006) — simple query `INSERT (30); INSERT (30)` fails with `23505`, `count(*) WHERE id = 30` is 0, ReadyForQuery status `I`; `CREATE TABLE a (…); SELECT * FROM missing` fails with `42P01` and table `a` does not exist; a string with `BEGIN; …; COMMIT` keeps per-statement behaviour; a string of only reads/session statements is not wrapped (unchanged results)
- [ ] T013 [P] [US1] In `tests/occ/autocommit.rs`: autocommit UPDATE of a row another session wrote in an open transaction → OC000, session idle (E-P1); autocommit DML **and autocommit DDL** (`CREATE TABLE`) while another session holds an open DDL transaction → succeeds after the DDL commits within the retry budget, or `40001` OC001, never `55P03` (E-P4b, E-P9)
- [ ] T070 [P] [US1] In `tests/occ/autocommit.rs`: autocommit DML over the extended protocol (FR-008) — `client.execute` with a prepared `INSERT … VALUES ($1, 0)` / `UPDATE … WHERE id = $1`: a 4-session storm reports only OC000 and no insert failure; a duplicate key gives `23505` with the session idle (`client.is_closed()` false, next `SELECT 1` works); an `INSERT … SELECT` over the row limit gives `54000` with the table unchanged
- [ ] T071 [P] [US1] In `tests/occ/autocommit.rs`: autocommit DDL with 006's server-side steps under contention — while session A holds `BEGIN; CREATE TABLE hold (id int PRIMARY KEY)` and commits it after ~50 ms, session B's autocommit `CREATE INDEX ASYNC ON t (v)` and `ALTER TABLE child ADD CONSTRAINT … FOREIGN KEY … NOT VALID` (rebuild) both succeed; `sys.jobs` has exactly one row per job statement; no `55P03` ever; when A holds the lock past the budget, B gets `40001` OC001 and the table/index/constraint state is unchanged
- [ ] T074 [P] [US1] Unit test in `crates/server/src/occ.rs`: `internal_transaction` on two connections of one database — with the second connection holding an open DDL transaction released after 30 ms, the steps run more than once and finally commit; with it never released, the result is a `LockBusy` failure (shaped OC001 by the caller), the session is Idle and nothing the steps wrote exists
- [ ] T014 [P] [US1] In `crates/policy/src/errors.rs`: change `engine_conflicts_become_dsql_errors` so `DataConflict` → OC000 and `LockBusy` → OC001 (FR-004); in `crates/policy/src/session.rs` tests: `check_implicit_rows(3000)` is `Ok`, `check_implicit_rows(3001)` is `54000`; `begin_implicit` makes `is_implicit()` true and a following `complete(…, Idle)` clears it

### Implementation

- [ ] T015 [US1] `shape_engine_error`: map `EngineFailure::LockBusy` to `oc001()` and update the doc comment (no more interim mapping) in `crates/policy/src/errors.rs`; T014 errors part passes
- [ ] T016 [US1] Add `SessionPolicy::{begin_implicit, check_implicit_rows}` in `crates/policy/src/session.rs` delegating to `crates/policy/src/occ.rs`; T014 passes
- [ ] T017 [US1] In `crates/server/src/occ.rs`: implicit single-statement transaction for `plan.class == Dml` while `txn_status == Idle` (and `plan.locking` with a touch, later US4), placed **inside the engine closure** handed to `dsql::run_planned` (so the policy sees Idle before and after, research.md D1): `BEGIN` → statement → `check_implicit_rows(rows)` (over → `ROLLBACK`, `54000`) → `COMMIT`; any error → `ROLLBACK` then report; `LockBusy` at any step → `ROLLBACK`, sleep `2·n` ms, retry within `BUSY_RETRY`; log `busy retry` / `residual contention` once at debug level; same code path for the simple and the extended protocol; T010, T011, T013 (DML part), T070 pass
- [ ] T072 [US1] In `crates/server/src/occ.rs`: for `plan.class == Ddl` while Idle, wrap the engine closure with a retry of the client's own engine statement on `LockBusy` within `BUSY_RETRY` (no `BEGIN`); 006's `dispatch` passes this closure to `sys::run_job_statement` and `ddl::run_ddl_statement`, whose own server-issued steps are not retried; T013 (DDL part) passes
- [ ] T073 [US1] Add `occ::internal_transaction(conn, steps: impl FnMut() -> PgWireResult<T>)` (unpinned `BEGIN`, steps, `COMMIT`, `ROLLBACK` on any error, whole retry on `LockBusy` within `BUSY_RETRY`) in `crates/server/src/occ.rs`, and make 006's `constraints::in_internal_transaction` (`crates/server/src/constraints.rs`) take `steps: impl FnMut` and delegate its Idle branch to it (Active/Aborted branch unchanged; update its doc comment: the M4 helper is now in use); T071, T074 and 006's `constraints::tests` / `dsql_ddl` rebuild tests pass
- [ ] T018 [US1] In `crates/server/src/occ.rs` add `run_batch` and call it from `run_simple_query` and `do_query` in `crates/server/src/lib.rs` when the session is idle, the string has ≥ 2 statements, none is transaction control, and at least one is DML or DDL (FR-006): `BEGIN`, `policy.begin_implicit(now)`, each statement through `occ::run` (implicit ⇒ no deferral, no retry; 006's `ddl.rs` sees Active and leaves undo to the `ROLLBACK`), `COMMIT`; on error `ROLLBACK` + `failed(Idle)`; the wire status after the query is `I`; T012 passes
- [ ] T019 [US1] Run the existing `crates/server/tests/smoke/dsql_conflicts.rs`, `dsql_rules.rs` (autocommit `values_rows` refusal), `t1_layer1.rs` and 006's `dsql_ddl.rs`, `dsql_jobs.rs` unchanged; fix the implementation, not the tests, if any fails

**Checkpoint**: US1 complete; `55P03` never reaches a client in the suite.

---

## Phase 4: User Story 2 — Conflicts are reported at COMMIT (P1)

**Goal**: doomed-transaction shim (FR-010–FR-016).

**Independent Test**: B reads r1; A updates and commits r1; B updates r1 → `UPDATE 1`; B COMMIT → OC000; r1 = A's value.

### Tests (write first, must fail)

- [ ] T020 [P] [US2] In `tests/occ/matrix.rs`: cells M1 (UPDATE/UPDATE, both orders of A-commit vs B-write: S1, S1x), M3 (DELETE/UPDATE), M4 (key UPDATE vs non-key UPDATE) — the later writer's statement succeeds with its normal row count, its COMMIT fails `40001` OC000, the session is `I`, final data = the other transaction's
- [ ] T021 [P] [US2] In `tests/occ/matrix.rs`: M2 same-PK and same-UNIQUE INSERT — second COMMIT fails OC000 (unchanged path, FR-014); M8 write skew both commit
- [ ] T022 [P] [US2] In `tests/occ/matrix.rs`: doomed transaction edge cases — ROLLBACK after deferral is silent; a later `23505` in a doomed transaction gives `25P02` until ROLLBACK and COMMIT answers ROLLBACK; doomed + age limit → COMMIT reports `54000` age error (use `ServerOptions.limits.max_txn_age` small); re-run conflicting again (other writer still open, S1x before A commits) → tag with 0 rows, `RETURNING` gives 0 rows, COMMIT OC000; nothing of a doomed transaction is ever committed (FR-013)
- [ ] T023 [P] [US2] In `tests/occ/matrix.rs`: with `OccOptions { defer_conflicts: false, .. }` the conflict is reported at the UPDATE as today (FR-016)
- [ ] T024 [P] [US2] Policy unit tests in `crates/policy/src/session.rs`: after `mark_doomed()`, `admit(COMMIT, Active)` → `RollbackThenRefuse(oc000())`; `admit(ROLLBACK)` → `Run`; age check still wins over doomed; `mark_doomed()` on an implicit transaction is a no-op (debug-asserted)

### Implementation

- [ ] T025 [US2] Add `SessionPolicy::mark_doomed` and the doomed-COMMIT branch in `admit` (after the age check) in `crates/policy/src/session.rs` / `crates/policy/src/occ.rs`; T024 passes
- [ ] T026 [US2] In `crates/server/src/occ.rs` wrap the engine for explicit, non-implicit transactions when `defer_conflicts` (DML and FOR UPDATE plans only; job/DDL plans and 006's internal transaction are never deferred): on `failure_of(err) == Some(DataConflict)` from a statement (not COMMIT): `ROLLBACK`, run `policy.begin_sql()` (fallback `BEGIN`), pin if `snapshot_at_begin` (after US3; until then skip), `mark_doomed()`, re-run the statement once; on a second `DataConflict` return `engine.empty(sql)`; any other error is returned (block aborted by the engine); log `deferred conflict` once at debug level; T020, T022, T023 pass
- [ ] T027 [US2] Run `tests/psql/run.sh` against `micro-dsql --listen 127.0.0.1:15507`: `40_conflict` must still print `40001 reported: yes`, `session A (first committer) got 40001: no`, `final value: from A` with `tests/psql/expected/40_conflict.out` unchanged

**Checkpoint**: US1 + US2 = the MVP for DSQL retry-loop tests.

---

## Phase 5: User Story 3 — The snapshot is taken at BEGIN (P2)

**Goal**: FR-020–FR-023.

**Independent Test**: B BEGIN; A commits r2 = 555; B reads r2 → old value; B updates r2 and COMMIT → OC000.

### Tests (write first, must fail)

- [ ] T030 [P] [US3] In `tests/occ/snapshot.rs`: S1d for `BEGIN`, `BEGIN ISOLATION LEVEL REPEATABLE READ`, `START TRANSACTION`, `BEGIN READ ONLY` — the read after a concurrent commit returns the old value; a following write of that row ends in OC000 at COMMIT
- [ ] T031 [P] [US3] In `tests/occ/snapshot.rs`: `BEGIN`; concurrent autocommit commit; `CREATE TABLE`; `COMMIT` succeeds (FR-021, E-P4/E-P10b); `BEGIN READ ONLY` + DDL keeps the engine's refusal
- [ ] T032 [P] [US3] In `tests/occ/snapshot.rs`: the pin is invisible — `BEGIN` returns tag `BEGIN`, no rows; row counters, DDL/DML discipline and `25P02` behaviour are unchanged (`20_txn`-style sequence); with `snapshot_at_begin: false` S1d sees the new value (FR-023)
- [ ] T033 [P] [US3] Policy unit tests in `crates/policy/src/session.rs`: `mark_pinned` + DDL plan first → `needs_fresh_snapshot` true; after any other statement → false; DML first → false

### Implementation

- [ ] T034 [US3] Add `SessionPolicy::{mark_pinned, needs_fresh_snapshot}` in `crates/policy/src/session.rs` / `crates/policy/src/occ.rs`; T033 passes
- [ ] T035 [US3] In `crates/server/src/occ.rs`: after a successful `Txn(Begin)` plan that left the engine Active, run `PIN_SQL` (errors logged at debug and ignored) and `mark_pinned()`; before running a plan with `needs_fresh_snapshot`, run `ROLLBACK` and `begin_sql()` (no pin), log `snapshot re-opened for DDL`; also pin the transaction re-opened by T026; T030–T032 pass

---

## Phase 6: User Story 4 — `SELECT … FOR UPDATE` takes part in conflict detection (P2)

**Goal**: FR-030–FR-034.

**Independent Test**: A `BEGIN; SELECT … WHERE id = 1 FOR UPDATE`; B `BEGIN; UPDATE id = 1; COMMIT` → B's COMMIT OC000; A commits.

### Tests (write first, must fail)

- [ ] T040 [P] [US4] Policy unit tests in `crates/policy/src/locking.rs` (new module): `plan_locking` for `SELECT s FROM t WHERE id = 1 FOR UPDATE`, `… FOR UPDATE OF t`, alias `FROM t AS x … FOR UPDATE OF x`, `NOWAIT`, `SKIP LOCKED`, `ORDER BY … LIMIT 1 FOR UPDATE`, no `WHERE` → `LockingRead` with `read_sql` free of any locking clause and `touch.render("v")` = `UPDATE t SET v = v WHERE id = 1` (deparsed form); `FOR KEY SHARE` → `touch: None`; joins, subquery in `FROM`, CTE with a locking clause, `UNION`, two `OF` relations, `GROUP BY`/`DISTINCT` → `Err(locking_read_form())` (`0A000`); `FOR SHARE` / `FOR NO KEY UPDATE` still refused by the M2 rule; a SELECT without a locking clause → `None`; a string literal containing `FOR UPDATE` → `None`
- [ ] T041 [P] [US4] In `tests/occ/locking.rs`: M5 (FOR UPDATE vs UPDATE, both orders), M6 (FOR UPDATE vs FOR UPDATE), M7 (FOR UPDATE vs other-row INSERT/UPDATE: no conflict), M9 (FOR KEY SHARE vs non-key UPDATE: no conflict); FOR UPDATE returns the same rows as the plain SELECT; FOR UPDATE rows do not count toward the row limit (small `max_rows_per_txn`) and do not trip the DDL/DML rule (`BEGIN; SELECT … FOR UPDATE; CREATE TABLE …` behaves like a read); autocommit FOR UPDATE works; data is unchanged after a touch
- [ ] T042 [P] [US4] In `crates/server/tests/smoke/` (new file `dsql_locking.rs`, declared in `crates/server/tests/smoke/main.rs`): extended-protocol FOR UPDATE with a `$1` parameter in `WHERE` binds the parameter in both the touch and the read

### Implementation

- [ ] T043 [US4] Implement `crates/policy/src/locking.rs` (`LockStrength`, `LockingRead`, `TouchPlan::{relation, render}`, `plan_locking`) using AST clones and `NodeEnum::deparse`; add `locking_read_form()` (rule id `locking_read_form`, `0A000`, message `locking clause is only supported on a single-table SELECT`) in `crates/policy/src/errors.rs` and record the rule in `specs/007-m4-occ-fidelity/contracts/occ-api.md` §1 (the 005 folder is not edited). FR-034: before coding, search `research/` and the referenced Dreamescaper rules for a recorded text for these forms; if one exists, use it instead of the generic text and note it in research.md D4; T040 passes
- [ ] T044 [US4] Add `StatementPlan.locking` in `crates/policy/src/statement.rs`: set from `plan_locking`, `engine_sql = read_sql` when accepted, refusal merged into `plan.refusal`; existing `statement.rs` tests stay green
- [ ] T045 [US4] In `crates/server/src/occ.rs`: when `plan.locking` has a touch, choose the column (first non-primary-key column of the relation from `conn.inner().current_schema()`, else the first column; fall back to the first column of `SELECT * FROM <relation> LIMIT 0` if the schema lookup fails), run `touch.render(column)` with the portal's parameters (extended protocol) before the read, discard its count (not passed to `complete`), apply US1/US2 handling to its errors; include FOR UPDATE in the autocommit implicit-transaction path (T017); T041, T042 pass
- [ ] T046 [US4] Run `tests/psql/run.sh --probes 26_locking_reads` against `127.0.0.1:15507` and record in `specs/007-m4-occ-fidelity/research.md` §1 which lines still differ from the reference (expected: none except documented differences); do not re-capture the probe's expected file

---

## Phase 7: User Story 5 — An OCC scenario suite pins the conflict matrix (P1, exit)

**Goal**: FR-040–FR-043, SC-002/SC-003/SC-005.

**Independent Test**: `cargo test -p micro-dsql-server --test occ` passes; every data-model.md matrix row has a scenario.

- [ ] T050 [P] [US5] In `tests/occ/differences.rs`: one scenario per knowingly different cell D1–D5 of data-model.md (FOR KEY SHARE vs DELETE no conflict; FK parent DELETE + child INSERT both commit, orphan remains; earlier writer wins when the later writer commits first; first-writer rollback still dooms; unrelated DDL → OC001 at the writer's COMMIT) plus "tables created after BEGIN are invisible (`42P01`)" and "residual contention inside an explicit transaction → OC001"; each asserts today's behaviour and names its docs/known-gaps.md entry in a doc comment
- [ ] T051 [P] [US5] In `tests/occ/autocommit.rs`: SC-005 retry loop — 4 sessions × 50 increments, each `BEGIN; SELECT v; UPDATE v = v + 1; COMMIT` retried on `40001`, final total exactly 200 and no other SQLSTATE
- [ ] T052 [P] [US5] SC-006: add `tests/occ/autocommit.rs::autocommit_latency` (`#[ignore = "timing; run manually"]`) timing 1,000 single-session autocommit INSERTs; run it before T017 and after T045 on the same host and record both numbers in `specs/007-m4-occ-fidelity/research.md` §1 (after ≤ 2× before)
- [ ] T053 [US5] Write `tests/occ/README.md`: the matrix (data-model.md table) with the scenario name per cell, how to run, and the differences list linked to docs/known-gaps.md
- [ ] T054 [US5] Update `docs/known-gaps.md`: close B10 (55P03), "Autocommit row limit" and "Multi-statement strings" (removing their rows in the same change their tests pass, Principle V); refine B11 with E-P9 (unrelated writers get OC000 during DDL churn); add rows for each difference of T050 with observed behaviour, suspected cause and milestone; update the "Fixed in M4" summary line and the `SELECT … FOR UPDATE` part of the "Engine gaps found by the probes" row
- [ ] T055 [US5] Update `README.md` (OCC behaviour section: conflicts at COMMIT, snapshot at BEGIN, FOR UPDATE, differences link) and the change list in the vendored header of `crates/server/src/lib.rs` (M4 entry)

---

## Phase 8: Polish & gates

- [ ] T060 Run the gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`; start `cargo run -p micro-dsql -- --listen 127.0.0.1:15507` and run `tests/psql/run.sh` (5/5 PASS, expected outputs unchanged)
- [ ] T061 Grep the diff for machine-specific paths and tool attribution (`git diff main --stat`, then search for absolute home/mount paths); none allowed
- [ ] T062 Walk through quickstart.md's manual two-session table on `127.0.0.1:15507` and paste the observed results into `specs/007-m4-occ-fidelity/research.md` §1 (dated block)
- [ ] T063 [orchestrator] MiniStack differential (`scripts/ministack-up.sh`, `scripts/compare.sh`, Docker) for `tests/psql/probes/26_locking_reads.sql` and the layer-1 scenarios; attach the report
- [ ] T064 [owner] Live-DSQL verification of the matrix rows marked as differences (who loses; FOR KEY SHARE vs DELETE; FK parent/child), of the locking-clause refusal text for joins, and of `NOWAIT`/`SKIP LOCKED` acceptance; update docs/known-gaps.md "Rule texts without a live record" accordingly
- [ ] T065 [owner] Manual walkthrough with a real retry-wrapping client (an official DSQL connector or Drizzle with a retry helper) against micro-dsql under parallel CI-like load; confirm "retry-loop tests behave like DSQL's" (the M4 stop criterion)
- [ ] T066 [orchestrator] Merge `007-m4-occ-fidelity` into local `main` after the gates pass (Principle V), or apply the known-gap rule after two implement/converge rounds

---

## Dependencies & Execution Order

- Phase 1 (T001–T003) → Phase 2 (T004–T008) → stories.
- Within US1: T010–T014, T070, T071, T074 (tests) → T015, T016 → T017 → T072 → T073 → T018 → T019.
- US1 (Phase 3) first: US2's replay and US4's autocommit path build on `occ::run`'s implicit transaction.
- US2 (Phase 4) depends on US1 (T017) only for shared code in `occ.rs`; its tests are independent.
- US3 (Phase 5) is independent of US2 except T035's "pin the re-opened transaction", which needs T026.
- US4 (Phase 6) depends on US1 (autocommit wrapping) and on US2 for conflict deferral of the touch.
- US5 (Phase 7) after US1–US4; T050 can be written any time after Phase 2.
- Phase 8 last; T063–T066 are outside the implementing agent.

## Parallel Opportunities

- T005 ∥ T006; T010–T014, T070, T071, T074 together (different files or modules); T020–T024 together; T030–T033
  together; T040–T042 together; T050–T052 together.
- Implementation tasks in `crates/server/src/occ.rs` (T017, T072, T073, T018, T026, T035, T045) are sequential
  (same file).

## Implementation Strategy

- **MVP**: Phases 1–4 (US1 + US2): parallel CI stops seeing spurious failures and conflicts surface
  at COMMIT — the "retry-loop tests behave like DSQL's" stop criterion is checked here (T065).
- Then US3, US4, US5. If a later item fails after two implement/converge rounds, its tests stay
  with `#[ignore = "KNOWN-GAP: …"]` and a docs/known-gaps.md entry (Principle V).
