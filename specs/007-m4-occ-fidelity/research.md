# Research: M4 — OCC fidelity

All observations below were made on 2026-11-12 on branch `007-m4-occ-fidelity` (= `main` at
`95842a0`), Turso pin `d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5`, in two ways:

- **Wire (W)**: `cargo run -p micro-dsql -- --listen 127.0.0.1:15507`, two `psql` sessions driven
  step by step (each statement waits for the previous one to finish), `VERBOSITY verbose`.
- **Engine (E)**: a scratch integration test in `crates/server/tests/` (not committed) that opens
  one in-memory database with `micro_dsql_server::open_database` and two
  `turso_pg::PgConnection`s, prints every statement's raw `LimboError` variant and the session's
  `TransactionState`, and runs multi-threaded storms. The scenarios are re-created as committed
  tests by tasks.md (T010–T013, T020–T022, T030–T031, T041, T050).

## 1. Observed behaviour today

### 1.1 Two-session matrix over the wire (W)

Table `t (id int PRIMARY KEY, v int, u int UNIQUE)`, rows (1,0,1), (2,0,2).

| Id | Interleaving | Failing statement | Client sees | Final |
|---|---|---|---|---|
| S1 | B: BEGIN, SELECT r1; A: BEGIN, UPDATE r1, COMMIT; B: UPDATE r1, COMMIT | **B's UPDATE** | `40001 change conflicts with another transaction (OC000)`; B's COMMIT answers ROLLBACK | A's value |
| S1x | A, B: BEGIN; A: UPDATE r1; B: UPDATE r1; A: COMMIT; B: COMMIT | **B's UPDATE** | OC000 | A's value |
| S1e | as S1x, but A rolls back | **B's UPDATE** | OC000 | nothing applied (DSQL: B commits) |
| S1d | B: BEGIN; A: autocommit UPDATE r2 = 555; B: SELECT r2 | none | B **sees 555** | snapshot taken at first statement |
| S2 | same PK INSERT in two transactions | **B's COMMIT** | OC000 | A's row |
| S2c | same UNIQUE (non-PK) value | **B's COMMIT** | OC000 | A's row |
| S2d | A: DELETE r2; B: UPDATE r2 (both open) | **B's UPDATE** | OC000 | — |
| S3 | write skew (on-call doctors) | none | both commit | both off call (SI, same as DSQL) |
| S6 | `FOR UPDATE`, `FOR KEY SHARE`, `FOR UPDATE NOWAIT` | the SELECT | `0A000 SELECT ... FOR UPDATE / FOR SHARE is not supported` | — |
| S6b | `FOR SHARE` | the SELECT | `0A000 locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported` (M2 rule) | — |
| S7 | A: BEGIN, UPDATE t; B: `ALTER TABLE t ADD COLUMN`; A: SELECT 1, COMMIT | **A's COMMIT** | `40001 schema has been updated by another transaction (OC001)` | A's write discarded |
| S7b | as S7 with B creating an **unrelated** table | **A's COMMIT** | OC001 | A's write discarded (B11) |
| S7c | A read-only transaction; B creates a table | none | — | — |
| S8 | A: BEGIN, UPDATE r1 (open); B: autocommit UPDATE r1 | **B's UPDATE** | OC000 (engine: `WriteWriteConflict`, E-P1) | A's value |
| S8b | as S8, B updates r2 | none | — | both |
| S9 | `BEGIN CONCURRENT` | the statement | `42601 syntax error at or near "CONCURRENT"` | — |
| S10 | `INSERT …; INSERT …` (second fails) in one string | second | its error; the first stays applied (E: confirmed by M2 known gap) | — |

Layer-1 `tests/psql/run.sh` against the same server: 5/5 PASS. Probe `26_locking_reads` differs
from the MiniStack reference only by the `0A000` refusals of `FOR UPDATE`, `FOR KEY SHARE`,
`NOWAIT`, `SKIP LOCKED` (and the `25P02` that follow in the aborted block).

### 1.2 Engine probes (E)

| Probe | Result |
|---|---|
| E-P1 autocommit UPDATE of a row another transaction has written (uncommitted) | `40001 WriteWriteConflict`, state Idle. Disjoint row and INSERT succeed. |
| E-P2a storm, 8 threads × 150 **plain autocommit** (unique INSERT / hot-row UPDATE / count read) | ok 809, **`55P03 Busy` 391** (191 inserts, 200 updates), reads never fail |
| E-P2b same storm, each DML wrapped `BEGIN; stmt; COMMIT` (PG `BEGIN` = Turso `BEGIN CONCURRENT`, translator.rs:942) | ok 1,053, `WriteWriteConflict` 147 (hot-row updates only), **0 Busy**, inserts never fail |
| E-P2c plain autocommit storm + one thread doing CREATE/DROP TABLE | `55P03 Busy` 883 of 1,050 DML, 26 DDL; 3 `SchemaConflict` |
| E-P9 wrapped storm + DDL thread, stage recorded | Busy only at the **statement** (58) and DDL (6); `WriteWriteConflict` at COMMIT 714 (unrelated inserts too), `SchemaConflict` at COMMIT 20 |
| E-P9 same with bounded retry on Busy (≤10 attempts, 2·n ms sleep) | **0 Busy** left (11 statements needed a retry); the WWC/SchemaConflict counts unchanged |
| E-P3 `BEGIN`, then pin read, then concurrent commit, then read | no pin / `SELECT 1`: **sees** the commit; any table read (`SELECT 1 FROM other LIMIT 0`, `SELECT count(*) FROM other`, `SELECT 1 FROM pg_catalog.pg_class LIMIT 0`): does **not** see it, and a later UPDATE of the changed row is `WriteWriteConflict` |
| E-P4 pinned transaction, concurrent commit, then DDL | DDL fails `55P03 Busy`, block aborted |
| E-P10b pinned, concurrent commit, **ROLLBACK; BEGIN;** DDL; COMMIT | succeeds |
| E-P10c pinned, concurrent `CREATE TABLE fresh3`, then `SELECT FROM fresh3` | `42P01 no such table` (catalog is part of the snapshot) |
| E-P10d pinned, concurrent unrelated commit, then UPDATE of another row, COMMIT | succeeds |
| E-P4b B: BEGIN; CREATE TABLE (open). A: autocommit INSERT → `55P03 Busy`; A: BEGIN; INSERT; COMMIT → COMMIT `WriteWriteConflict` | DDL transactions block / conflict with all writers |
| E-P5 touch `UPDATE t SET v = v WHERE id = 1` in A (open); B updates r1 | B: `WriteWriteConflict` at its UPDATE |
| E-P5b A read r1; B commits update r1; A touches r1 | A: `WriteWriteConflict` |
| E-P5c A touches r1; B touches r1 | B: `WriteWriteConflict` |
| E-P5d touch with `RETURNING` | rows returned, `n_change` = rows touched, values unchanged |
| E-P5e A touches r1; B inserts r50 | no conflict |
| E-P6 after `WriteWriteConflict` in an explicit transaction | state Aborted; next statement `25P02`; ROLLBACK → Idle |
| E-P6b conflict; ROLLBACK; BEGIN; replay the UPDATE; SELECT | replay succeeds (`n_change=1`), the SELECT sees the replayed value; after ROLLBACK nothing is kept |
| E-P7 `INSERT … SELECT` and full-table UPDATE inside a transaction | `n_change` = rows changed (2 and 2) → usable for the autocommit row limit |
| E-P8 A: DELETE parent 2; B: INSERT child → parent 2; both commit | **both commit; an orphan child remains** (FK checks read B's snapshot; no key-share) |

### 1.3 Re-run after merging feature 006 (`main` at `3370541`, same Turso pin)

The engine probes were re-run on the merged tree (scratch test, not committed); results are the
same in kind, counts vary run to run:

| Probe | Before (§1.2) | After the 006 merge |
|---|---|---|
| E-P2a plain autocommit storm | 391 `55P03` | 395 `55P03` (193 inserts, 202 updates) |
| E-P2b wrapped storm | 0 Busy, 147 WWC | 0 Busy, 129 WWC (hot row only) |
| E-P9 wrapped + DDL thread, with retry | 0 Busy left | 0 Busy left (11 retried) |
| E-P3 / E-P10 pin read | table read pins, `SELECT 1` does not | same |
| E-P4 pinned + concurrent commit + DDL | `55P03` | same |

What changed in the server (read in the merged code): every admitted statement now goes through
`dsql::run_planned` → `dsql::dispatch`, which sends job statements (`plan.job`) to
`sys::run_job_statement`, DDL with extra steps (`plan.ddl` or a non-empty `plan.prelude`) to
`ddl::run_ddl_statement`, and everything else to the caller's engine closure. `ddl.rs` runs the
identity prelude and compensates it with `plan.cleanup_on_failure` when Idle;
`constraints::rebuild_with_constraint` (NOT VALID) wraps its steps in
`constraints::in_internal_transaction` (internal `BEGIN`/`COMMIT` when Idle, the client's
transaction otherwise). Statements answered with their own tag (`Outcome::TaggedRows`,
`execute_with_own_tag`, `send_rows_with_own_tag`) are job statements only (DDL/`CALL`). Autocommit
DML still runs as one engine autocommit statement; the M2 gaps (row limit, multi-statement
atomicity) and the `LockBusy → OC000` mapping are unchanged.

## 2. Decisions

### D1 — Item 1, autocommit DML: **doable in our layer**

- **Decision**: the server runs every autocommit DML statement (class `Dml`, plus `SELECT … FOR
  UPDATE`, see D4) as `BEGIN` → statement → `COMMIT` on the same engine connection, all inside the
  one blocking call that runs the statement. The wrapper lives **inside the engine closure** that
  `occ::run` hands to `dsql::run_planned` (the same pattern as 006's
  `in_internal_transaction`), so the policy sees `Idle` before and after and `complete`/`failed`
  run once, as today; a `BEGIN` issued before `run_planned` would make `complete` treat the DML as
  a transaction opener. The PostgreSQL `BEGIN` already compiles to Turso's
  `BEGIN CONCURRENT` (the literal `BEGIN CONCURRENT` is a syntax error through our server, S9), so no
  new engine syntax is needed. On any error the server runs `ROLLBACK` and reports the error.
- **Evidence**: E-P2a vs E-P2b: 391 → 0 `55P03` with disjoint inserts never failing.
- **Residual contention**: E-P2c/E-P9 show `Busy` remains while a DDL transaction holds the
  exclusive lock, for DML and for DDL. Bounded transparent retry (E-P9: 0 Busy left) handles it for
  autocommit, where a failed attempt has applied nothing:
  - autocommit DML: the whole `BEGIN … COMMIT` attempt is retried;
  - autocommit DDL: the client's own engine statement (the closure `dispatch` calls, also from
    `sys::run_job_statement` and `ddl::run_ddl_statement`) is retried; 006's server-issued steps
    before it (identity prelude) stay applied across attempts and are compensated by `ddl.rs` only
    if the statement finally fails; steps after it (`sys.jobs` row, sequence DROP) are not retried;
  - micro-dsql's own internal transaction for a client DDL (`in_internal_transaction`, the NOT VALID
    rebuild) is retried as a whole (it rolls back on any error, so nothing is applied);
  - retry bound: a constant `occ::BUSY_RETRY` (10 attempts, ≤ 200 ms of back-off), not an option,
    because `in_internal_transaction` has no access to the server options. Inside an explicit transaction a retry is not safe; there,
  and when the retry budget is spent, `LockBusy` is shaped as OC001 (spec clarification 5): the
  only observed cause is a concurrent schema change, and every DSQL retry wrapper retries OC001.
- **Mapping change**: `shape_engine_error` maps `EngineFailure::LockBusy` to `oc001()` instead of
  `oc000()`; the blanket M2 mapping is gone. The `LockBusy` variant stays (it is the input of the
  retry decision).
- **Row limit (M2 gap)**: inside the wrapper the statement's `n_change` is known before `COMMIT`
  (E-P7); over the limit → `ROLLBACK` + `54000`. The `values_rows` pre-check is kept as a cheap early
  refusal.
- **Multi-statement strings (M2 gap)**: an idle simple-protocol string of ≥ 2 statements without
  transaction control that contains DML or DDL is wrapped in one `BEGIN … COMMIT` (orchestrator
  decision; M2's `check_batch` already limits it to one DDL and no DDL with DML; inside it 006's
  `ddl.rs` sees an Active session and leaves undo to the `ROLLBACK`, and
  `in_internal_transaction` joins the open transaction); the policy tracks it as an *implicit*
  transaction (so the discipline rules and row counting apply exactly as `check_batch` predicted).
  Conflicts in an implicit transaction are reported at the statement (no deferral: the client sees
  them at the end of the same query, which is DSQL's COMMIT point). Not retried on Busy (earlier
  statements' results may already be on the wire).
- **Alternatives rejected**: Turso patch making autocommit concurrent in the translator/VDBE
  (unnecessary given the evidence; Principle II); a busy timeout alone (still waits on locks, DSQL
  never does, and keeps the exclusive-transaction semantics).

### D2 — Item 2, deferred conflict ("doomed transaction"): **doable in our layer (approximation)**

- **Decision**: in an explicit transaction, when a statement fails with
  `EngineFailure::DataConflict` and deferral is on, the server (before the error leaves the
  statement): runs `ROLLBACK`, re-opens the transaction with the same `BEGIN` text it was opened
  with (and the snapshot pin, D3), marks the policy transaction **doomed**, and re-runs the
  statement once. Re-run OK → its result is the statement's result (E-P6b). Re-run conflicts again
  → the statement reports its command tag with 0 rows (and an empty row set for `RETURNING`).
  Re-run fails otherwise → that error, block aborted as usual. `COMMIT` of a doomed transaction →
  `Admission::RollbackThenRefuse(oc000())` (the seam 005 prepared); `ROLLBACK` runs normally.
- **Why not in `SessionPolicy::failed()`** (the 005 contract §6 sketch): by the time `failed()` runs
  the engine block is aborted (E-P6) and the error is already being returned; the deferral has to
  replace the error, so the server performs it and the policy only records the flag
  (`SessionPolicy::mark_doomed`). Recorded as a deviation from 005's sketch.
- **Caveats** (research/04 §B and new): the doomed transaction loses its earlier writes and runs
  later statements on a fresh snapshot; the *later writer* loses, not the later committer; S1e (first
  writer rolls back) still dooms the second. All listed as known differences.
- **Alternatives rejected**: report `UPDATE 0` without re-running (breaks apps that check the
  count); a core mode with multiple uncommitted versions (research/04: L–XL, a Turso patch).

### D3 — Item 3, snapshot at BEGIN: **doable in our layer**

- **Decision**: after an accepted `BEGIN`/`START TRANSACTION` (engine status Idle → Active), the
  server runs the pin read `SELECT 1 FROM pg_catalog.pg_class LIMIT 0` on the engine connection,
  outside the policy (no counters change, no rows returned to the client). A failing pin is logged at
  debug level and ignored (the transaction then behaves as today).
- **DDL after the pin** (E-P4): if the first client statement of a pinned transaction is DDL, the
  server first runs `ROLLBACK` and the transaction's original `BEGIN` text again without the pin
  (E-P10b), then the DDL. Safe because nothing but the pin ran.
- **Evidence**: E-P3 (any table read pins; `SELECT 1` does not), E-P10 (pg_class `LIMIT 0` pins and
  returns nothing), E-P10d (writes after the pin still commit).
- **Caveat**: tables created after `BEGIN` are invisible to the transaction (E-P10c) — documented.
- **Alternatives rejected**: core patch to take the MVCC begin timestamp at `BEGIN` (research/04:
  S–M, but a patch; unnecessary).

### D4 — Item 4, locking reads: FOR UPDATE **doable in our layer**; FOR KEY SHARE and FK KEY SHARE **out of reach → documented difference**

- **Decision (FOR UPDATE)**: the policy recognises a single-table `SELECT … FOR UPDATE [OF t]
  [NOWAIT | SKIP LOCKED]` and plans two engine statements: the *touch*
  `UPDATE <table> [AS alias] SET <c> = <c> WHERE <same where>` and the *read* (the SELECT without its
  locking clause, by AST edit + `deparse`). The server runs the touch first (its conflicts follow
  D1/D2 exactly like a write, E-P5/P5b/P5c), discards its count, then runs the read and returns its
  rows. `<c>` is chosen by the server (the policy cannot see the schema, Principle III): the first
  column that is not part of the primary key, else the first column. The statement stays class
  `Read` for the discipline rules (FR-032).
- **Over-approximation**: `LIMIT`/`ORDER BY`/`OFFSET` are ignored for the touch (all rows matching
  `WHERE` are touched) — documented.
- **Refused forms**: locking clauses on joins, subqueries, CTEs, set operations, or with several
  `OF` relations → `0A000` (new rule `locking_read_form`), one generic text `locking clause is only
  supported on a single-table SELECT`. No live-DSQL or Dreamescaper record names these forms
  (research/01 §2 only says multi-table FOR UPDATE is supported since Aug 2026; 005's
  rule-catalogue has only `unsupported_locking`); if one appears, its text wins (orchestrator
  decision).
  `FOR SHARE`/`FOR NO KEY UPDATE` keep M2's rule.
- **FOR KEY SHARE**: accepted (same forms) and run as the read only; DSQL's KEY SHARE conflicts
  (vs DELETE / key UPDATE) need a read-lock set validated at commit inside `turso_core` MVCC
  (research/02 §3 item 4). That would be a core patch of size L; not attempted in M4 ("best
  effort; stop when retry loops behave"), so it is a documented difference, as is the implicit FK
  KEY SHARE (E-P8 orphan child).
- **NOWAIT / SKIP LOCKED**: accepted as no-ops (MiniStack reference accepts them; research/01's
  "[inferred] unsupported" is weaker evidence; constitution: research wins on facts only where it
  has them).

### D5 — Turso patch: **none**

Every hook is reachable from `crates/server` through public `turso_pg`/`turso_core` API:
`PgConnection::prepare` for `BEGIN`/`COMMIT`/`ROLLBACK`/pin/touch, `turso_pg::Error::core_error()`
for the failure kind, `Statement::n_change()` for counts, `Connection::current_schema()` for the
touch column. `patch-crate` is not wired by this feature. Seam with 006: if 006 wires
`patch-crate` (for `sys.*`), 007 adds nothing to it.

### D6 — Switches

`ServerOptions` gains `occ: OccOptions { defer_conflicts: bool (true), snapshot_at_begin: bool
(true) }`. No CLI flag. The busy-retry bound is the constant `occ::BUSY_RETRY` (D1).

### D7 — Where the OCC suite lives

`tests/occ/` at the repository root (docs/plan.md §3), compiled as an integration-test target of
`crates/server` through `[[test]] name = "occ", path = "../../tests/occ/main.rs"`, reusing the smoke
harness (`crates/server/tests/smoke/common.rs`) via `#[path]`. Runs in `cargo test --workspace`.

### D8 — Ordering within the feature

Value per effort, per the description: D1 (autocommit, row limit, multi-statement) → D2 → D3 → D4.
Each is behind its own failing tests; the suite's matrix scenarios are written first and marked
`KNOWN-GAP` only if a later item is abandoned (Principle V).

### D9 — Seam with 006's internal transaction (006 T080)

- **Decision**: `constraints::in_internal_transaction` keeps its name and signature shape but its
  Idle branch delegates to `occ::internal_transaction(conn, steps)`: unpinned `BEGIN`, the steps,
  `COMMIT`, `ROLLBACK` on any error, whole-transaction retry on `LockBusy` within `BUSY_RETRY`.
  `steps` becomes `FnMut` (the rebuild closure only borrows, so this is a signature change, not a
  logic change). Inside a client transaction it is unchanged (the client's transaction, pinned or
  not; a DDL as first statement has already been given a fresh unpinned transaction by
  `needs_fresh_snapshot`).
- **Why unpinned**: the rebuild is DDL; a pinned snapshot makes DDL fail with `Busy` after any
  concurrent commit (E-P4). Its `BEGIN` is issued immediately before the steps, so there is no
  window to pin anyway.
- **Not a doomed candidate**: data conflicts inside the internal transaction are reported at once
  (it is micro-dsql's own transaction for one client statement, like an implicit one).
- **Not wrapped twice**: the client statement is DDL, so `occ::run` never puts the DML wrapper
  around it; only the engine-statement retry and this internal retry apply.

### D10 — Interaction with 006's other paths

- `dsql::dispatch` stays as 006 left it; 007 adds no branch to it. `occ::run` passes its wrapped
  engine closure through `run_planned`, so job and DDL plans receive the retry wrapper as their
  `run_engine`.
- `Outcome::TaggedRows` / `execute_with_own_tag` are reached only by job statements (DDL, `CALL`),
  which are never deferred or wrapped as DML; `Engine::empty` (the doomed re-conflict result) only
  builds `Rows`/`Command` outcomes. The extended-protocol path of autocommit DML is `on_execute` →
  `_on_execute` → `do_query` → `occ::run`, the same entry as the simple protocol.
- Describe of a `FOR UPDATE` statement prepares `plan.engine_sql` (the read), which is what 006's
  describe already does for plans the engine runs.
- `ddl::compensate` runs only when Idle; in a wrapped multi-statement string the session is Active,
  so undo is the wrapper's `ROLLBACK` (consistent with 006's design).
