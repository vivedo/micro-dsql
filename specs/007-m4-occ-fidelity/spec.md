# Feature Specification: M4 — OCC fidelity

**Feature Branch**: `007-m4-occ-fidelity`

**Created**: 2026-11-12

**Status**: Draft

**Input**: User description: "M4 — OCC fidelity (best effort; the riskiest part). In order of value per effort; stop when retry-loop tests behave like DSQL's.
1. Autocommit DML as a single-statement concurrent transaction, so 55P03 disappears instead of being remapped to OC000 (M2's interim mapping is then removed). Try doing it from our server layer first (e.g. wrapping autocommit DML in BEGIN CONCURRENT … COMMIT); a Turso patch is a last resort.
2. "Doomed transaction" shim: turn a write-time WriteWriteConflict into a deferred OC000 at COMMIT. It's an approximation; the caveat is in research/04 §B.
3. Snapshot at BEGIN instead of at the first statement. Try our layer first (e.g. a read at BEGIN); otherwise a core patch.
4. FOR UPDATE treated as a write-set touch; FOR KEY SHARE and FK KEY SHARE come after.
Also: the M2 autocommit row-limit gap (only INSERT … VALUES is checked before running, because an autocommit statement has committed before its count is known) and the multi-statement-string atomicity gap, if item 1's execution model makes them fixable.
Exit: an OCC scenario suite (tests/occ/) asserts DSQL's conflict matrix from research/01 §2. Knowingly different cases are written down."

## Context

Aurora DSQL uses optimistic concurrency control: no locks, no waits, snapshot taken at `BEGIN`,
conflicts adjudicated at `COMMIT` where the first committer wins and the later committer gets
`40001` `change conflicts with another transaction (OC000)` (research/01 §2). micro-dsql's engine
is snapshot-isolated too, but differs in four observable ways. Behaviour on the current `main`
(2026-11-12, two psql sessions against `micro-dsql --listen 127.0.0.1:15507`, plus engine-level
probes; recorded in [research.md](research.md) §1):

| Scenario | Today | DSQL |
|---|---|---|
| Same-row UPDATE in two transactions | the **second writer's UPDATE** fails, `40001` OC000 | the **later committer's COMMIT** fails, OC000 |
| Same row, first writer rolls back | second writer still fails at its UPDATE | second writer commits |
| Same-PK / same-UNIQUE INSERT | second **COMMIT** fails, OC000 | same |
| Write skew (different rows) | both commit | same |
| `BEGIN`, concurrent commit, then first read | the read **sees** the concurrent commit | the read does not (snapshot at BEGIN) |
| Concurrent autocommit writes (8 × 150 statements) | 391 of 1,200 fail inside the engine with `55P03 Database is busy`; shown to the client as OC000 by M2's interim mapping | only real conflicts fail, with OC000 |
| `SELECT … FOR UPDATE` / `FOR KEY SHARE` (also with `NOWAIT`, `SKIP LOCKED`) | refused, `0A000` | accepted; FOR UPDATE rows are adjudicated as written |
| DDL committed while another transaction writes (any table) | the writer's COMMIT fails, OC001 | OC001 only when the writer used the changed objects |
| Autocommit `UPDATE`/`DELETE`/`INSERT … SELECT` over 3,000 rows | succeeds | refused, `54000` |
| Multi-statement query string where a later statement fails | earlier statements stay applied | the string is one implicit transaction; nothing applied |

This feature closes as much of that gap as micro-dsql's own layer can, in order of value per
effort, and writes down what stays different.

## Clarifications

### Session 2026-11-12

The owner could not be asked during this run; each answer is the recommended default, chosen for
consistency with the constitution and docs/plan.md. The five questions below are the full quota of
this session; lower-impact gaps found by the coverage scan are resolved in Assumptions.

- Q: Do the deferred-conflict and snapshot-at-BEGIN behaviours apply by default, or behind a switch? → A: By default, as on DSQL; an embedding test server can turn each off (no new command-line flag, docs/plan.md §3 keeps the surface tiny).
- Q: What does a statement report when its write conflict is deferred to COMMIT? → A: Its normal result, obtained by re-running it once on a fresh snapshot; if that re-run also conflicts, the command tag reports 0 rows and no result rows.
- Q: How is `FOR KEY SHARE` handled, given the engine has no read-lock set? → A: Accepted and run as a plain read (never conflicts); the missing conflicts are a documented difference.
- Q: Are `NOWAIT` and `SKIP LOCKED` accepted with FOR UPDATE / FOR KEY SHARE? → A: Yes, as no-ops (there are no locks to wait for), matching the MiniStack reference in `tests/psql/probes/expected/26_locking_reads.out`.
- Q: What does a client see when lock contention remains (only under a concurrent schema change)? → A: Autocommit statements retry transparently for a bounded time; contention that remains, or occurs inside an explicit transaction, is reported as `40001` OC001, never `55P03`.

### Session 2026-11-12 (orchestrator decisions after feature 006 was merged)

- Q: Is autocommit DDL that meets lock contention retried too? → A: Yes, bounded like DML, including the transactions micro-dsql opens itself for a client DDL statement (006's NOT VALID table rebuild).
- Q: Which multi-statement strings run as one implicit transaction? → A: Every idle string of two or more statements without transaction control that contains DML or DDL.
- Q: Which text refuses a locking clause on an unsupported form? → A: One generic text, unless a live-DSQL or Dreamescaper record names the form (none does today: research/01 §2, 005's rule catalogue); then that record's text.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Autocommit writes behave like single-statement DSQL transactions (Priority: P1)

A CI suite runs many tests in parallel against one micro-dsql. Their setup and assertions use plain
autocommit `INSERT`/`UPDATE`/`DELETE`. Each such statement is its own transaction, as on DSQL:
independent statements never fail because another session happens to be writing at the same
moment; real conflicts on the same row fail with OC000; a statement that would change more than
3,000 rows is refused and changes nothing; a multi-statement query string is all-or-nothing.

**Why this priority**: Spurious failures under parallel CI are the most visible OCC difference
today (a third of the writes in a contention test failed), and they hit applications that never
open an explicit transaction.

**Independent Test**: Eight sessions each run 150 autocommit statements (unique inserts and
updates of one hot row) at once; no statement fails except hot-row updates, and every failure is
`40001` OC000.

**Acceptance Scenarios**:

1. **Given** several sessions running autocommit inserts of distinct rows concurrently, **When**
   they all finish, **Then** none failed and every row exists.
2. **Given** several sessions running autocommit updates of the same row concurrently, **When**
   one is rejected, **Then** it receives `40001` with the exact OC000 text, nothing of it is
   applied, and the session stays usable and idle.
3. **Given** an autocommit `UPDATE`, `DELETE` or `INSERT … SELECT` that changes more than 3,000
   rows, **When** it runs, **Then** it fails with `54000` `transaction row limit exceeded` and the
   table is unchanged; exactly 3,000 rows succeeds.
4. **Given** a query string of several statements without transaction control, **When** a later
   statement fails, **Then** none of the string's statements is applied, the session is idle, and
   the error is that statement's error.
5. **Given** another session holding an uncommitted write to a row, **When** an autocommit
   statement writes the same row, **Then** it fails with OC000 (never `55P03`).
6. **Given** a concurrent schema change in progress, **When** an autocommit DML or DDL statement
   meets lock contention, **Then** it is retried transparently and either succeeds or fails with
   `40001` (OC001), never `55P03`.
7. **Given** the extended protocol (Parse/Bind/Execute), **When** autocommit DML runs, **Then** it
   behaves exactly as in scenarios 1–5.

---

### User Story 2 - Conflicts are reported at COMMIT (Priority: P1)

A test asserts the DSQL contract: "the transaction that commits second fails at COMMIT with OC000".
In micro-dsql the engine detects a same-row write conflict at the conflicting statement. micro-dsql
lets that statement appear to succeed, keeps the transaction open, and fails its COMMIT with OC000;
nothing of the transaction is committed.

**Why this priority**: Tests and retry wrappers written for DSQL expect statements inside a
transaction to succeed and COMMIT to fail; code that inspects the statement result before deciding
to commit takes a different path otherwise.

**Independent Test**: Session B reads a row in a transaction, A updates and commits it, B updates
it (succeeds, `UPDATE 1`) and commits: B's COMMIT fails with `40001` OC000; the row holds A's value.

**Acceptance Scenarios**:

1. **Given** B's transaction read a row and A then committed an update to it, **When** B updates
   the row, **Then** B's UPDATE succeeds with its normal row count, and B's COMMIT fails with
   `40001` OC000; afterwards the session is idle and the row holds A's value.
2. **Given** A and B both update the same row in open transactions, **When** A commits and then B
   commits, **Then** A's COMMIT succeeds and B's COMMIT fails with OC000.
3. **Given** a transaction whose conflict was deferred, **When** it runs `ROLLBACK`, **Then**
   ROLLBACK succeeds and no error is reported.
4. **Given** a transaction whose conflict was deferred, **When** it runs further reads and writes,
   **Then** they run normally (with the caveats in Known differences) and nothing of them is ever
   committed.
5. **Given** the 40_conflict layer-1 scenario, **When** it runs, **Then** it still reports
   `40001 reported: yes`, A (first committer) gets no 40001, and the final value is A's.

---

### User Story 3 - The snapshot is taken at BEGIN (Priority: P2)

A test opens a transaction, lets another session commit a change, then reads. On DSQL the read
does not see that change, because the snapshot is taken at `BEGIN`; micro-dsql behaves the same.

**Why this priority**: Observable only in deliberately interleaved tests, but those are exactly the
tests that check isolation semantics.

**Independent Test**: B runs `BEGIN`; A commits an update; B reads the row and sees the old value,
and B's later update of that row is reported as a conflict at COMMIT.

**Acceptance Scenarios**:

1. **Given** B ran `BEGIN` (any accepted form, including `START TRANSACTION` and `READ ONLY`),
   **When** A commits a change and B then reads, **Then** B does not see A's change.
2. **Given** B ran `BEGIN` and A then committed an update of row r, **When** B updates r and
   commits, **Then** B's COMMIT fails with OC000.
3. **Given** B ran `BEGIN` and other sessions then committed changes, **When** B's first statement
   is a DDL, **Then** the DDL and its COMMIT succeed (a DDL transaction is not made to fail by the
   early snapshot).
4. **Given** `BEGIN` is refused or the session is in an aborted block, **When** it runs, **Then**
   behaviour is unchanged from today.

---

### User Story 4 - `SELECT … FOR UPDATE` takes part in conflict detection (Priority: P2)

An application uses the DSQL-recommended pattern "SELECT … FOR UPDATE, then decide, then write" to
guard against concurrent changes. micro-dsql accepts the statement, returns the rows, and treats
the selected rows as written by the transaction, so a concurrent change of those rows makes one of
the transactions fail with OC000, as on DSQL. `FOR KEY SHARE` is accepted as a plain read.

**Why this priority**: Today the statement is refused with `0A000`, so code using it cannot be
tested at all; with it accepted, the most common conflict pattern behaves like DSQL.

**Independent Test**: A runs `BEGIN; SELECT … WHERE id = 1 FOR UPDATE`; B runs `BEGIN`, updates
row 1 and commits; A commits. Exactly one of the two COMMITs fails with `40001` OC000 (DSQL: the
later committer; micro-dsql: the later toucher, see Known differences), and the other is kept.

**Acceptance Scenarios**:

1. **Given** a single-table `SELECT … [WHERE …] FOR UPDATE` (optionally with `OF <table>`,
   `NOWAIT` or `SKIP LOCKED`), **When** it runs, **Then** it returns the same rows as without the
   locking clause.
2. **Given** A selected row r `FOR UPDATE` in an open transaction, **When** B updates or deletes r,
   or selects r `FOR UPDATE`, and both commit, **Then** exactly one of them fails with OC000.
3. **Given** A selected row r `FOR UPDATE`, **When** B inserts or updates a different row, **Then**
   neither fails.
4. **Given** `SELECT … FOR KEY SHARE`, **When** it runs, **Then** it returns the rows and never
   causes a conflict.
5. **Given** `FOR SHARE` or `FOR NO KEY UPDATE`, **When** it runs, **Then** it is refused with
   `0A000` `locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported` (unchanged).
6. **Given** a locking clause on a join, a subquery or a CTE, **When** it runs, **Then** it is
   refused with `0A000` (documented difference).

---

### User Story 5 - An OCC scenario suite pins the conflict matrix (Priority: P1)

A maintainer changes the engine pin or the server. A suite of two-session scenarios checks every
cell of DSQL's conflict matrix that micro-dsql emulates, and the cells it knowingly does not are
listed with the observed behaviour, so regressions and progress are both visible.

**Why this priority**: It is the feature's exit criterion and the only guard against silent
regressions across Turso bumps.

**Independent Test**: `cargo test --workspace` runs the suite; each scenario asserts which
statement fails, with which SQLSTATE and text, and the final data.

**Acceptance Scenarios**:

1. **Given** the suite, **When** it runs, **Then** every emulated matrix cell (FR-030) has at least
   one scenario asserting the failing statement, SQLSTATE `40001`, the exact OC000/OC001 text and
   the final row values.
2. **Given** a knowingly different cell, **When** the suite runs, **Then** a scenario asserts
   today's behaviour and points at its entry in docs/known-gaps.md.

### Edge Cases

- A deferred-conflict transaction that then hits another error (for example `23505`) behaves like
  any aborted block: `25P02` until `ROLLBACK`; COMMIT answers `ROLLBACK`.
- A deferred-conflict transaction past the 300 s age limit: COMMIT reports the age-limit error
  (the age check runs first, as in M2).
- A doomed statement whose re-run fails with a different error (for example `23505` on a fresh
  snapshot) reports that error and aborts the block.
- A doomed transaction's re-run conflicts again (the other writer is still uncommitted): the
  statement reports 0 rows; COMMIT still fails with OC000.
- Autocommit DML that fails for a non-conflict reason (`23505`, `23503`, `22xxx`): same error and
  SQLSTATE as today, nothing applied, session idle.
- Autocommit statements that are not DML (reads, DDL, `SET`, `SHOW`) are not wrapped in a
  transaction by micro-dsql (DDL keeps its own engine transaction, and 006's rebuild its internal
  one), but DDL meeting lock contention is retried like DML (FR-003).
- A multi-statement string that contains `BEGIN`/`COMMIT`/`ROLLBACK` keeps today's per-statement
  behaviour; a string with one DDL plus reads or session statements runs as one implicit
  transaction (two DDLs, or DDL with DML, are already refused as a whole by M2).
- An autocommit DDL statement with server-side extra steps (006: identity prelude, `sys.jobs` row,
  sequence cleanup) is retried only while its client statement has not reached the engine
  successfully; contention in a later server-issued step is reported as OC001 after compensation.
- Extended protocol: each `Execute` of a DML portal outside a transaction is its own single-
  statement transaction.
- `SELECT … FOR UPDATE` outside a transaction runs as a single-statement transaction.
- `SELECT … FOR UPDATE` with `LIMIT`/`ORDER BY`: the rows matched by its `WHERE` are treated as
  written, which may conflict more often than DSQL (documented difference).
- The rows touched by `FOR UPDATE` do not count toward the 3,000-row limit.
- A pinned snapshot hides tables created after `BEGIN`: the transaction gets `42P01` for them
  (DSQL reports OC001 or the same; documented).

## Requirements *(mandatory)*

### Functional Requirements

**Autocommit execution (item 1)**

- **FR-001**: An `INSERT`, `UPDATE`, `DELETE` (and a data-modifying `WITH`) run while no explicit
  transaction is open MUST run as a single-statement transaction: it is applied completely or not
  at all, and its conflicts are first-committer-wins against other transactions.
- **FR-002**: Autocommit DML MUST NOT fail because other sessions are concurrently writing other
  rows (no `55P03`, no OC000 for disjoint writes).
- **FR-003**: When autocommit execution of a DML or DDL statement (including a transaction
  micro-dsql opens itself for one client DDL statement) meets engine lock contention, micro-dsql
  MUST retry the statement transparently for a bounded time (a fixed bound: up to 10 attempts, at most about 200 ms in
  total) before reporting an error; nothing is applied by a failed attempt.
- **FR-004**: The client MUST never receive SQLSTATE `55P03`. Lock contention that remains after
  FR-003, or occurs inside an explicit transaction, MUST be reported as `40001` with the OC001
  text. M2's blanket "lock contention → OC000" mapping MUST be removed.
- **FR-005**: An autocommit DML statement whose changed-row count exceeds the 3,000-row limit MUST
  fail with `54000` `transaction row limit exceeded` and leave no change, whatever its form
  (`VALUES`, `SELECT`, `UPDATE`, `DELETE`).
- **FR-006**: A simple-protocol query string with two or more statements, no transaction-control
  statement and at least one DML or DDL statement, run while no transaction is open, MUST run
  atomically as one implicit transaction:
  on any statement's error, nothing of the string is applied and the session ends idle.
- **FR-007**: After an autocommit statement or implicit transaction (success or failure), the
  session MUST report idle status (`I`) and stay usable.
- **FR-008**: Autocommit execution (FR-001–FR-005, FR-007) MUST behave the same in the simple and
  the extended protocol.

**Deferred conflicts (item 2)**

- **FR-010**: When a statement in an explicit transaction fails with a data conflict (write-write,
  conflicting commit, stale snapshot), micro-dsql MUST mark the transaction doomed and MUST NOT
  report the error at that statement.
- **FR-011**: The doomed statement MUST report the result of re-running it once on a fresh snapshot
  (row count, returned rows); if the re-run also conflicts, it MUST report 0 rows and no result
  rows; if the re-run fails with another error, that error is reported and the block is aborted.
- **FR-012**: `COMMIT` of a doomed transaction MUST roll back everything and fail with `40001`
  OC000; the session is idle afterwards. `ROLLBACK` of a doomed transaction MUST succeed silently.
- **FR-013**: Nothing written by a doomed transaction (before or after the conflict) may ever be
  committed.
- **FR-014**: A data conflict reported by `COMMIT` itself (for example same-PK insert) MUST keep
  today's behaviour: `40001` OC000 at COMMIT, session idle.
- **FR-015**: Schema conflicts (OC001) MUST NOT be deferred; they are reported where the engine
  reports them (today: at COMMIT).
- **FR-016**: The deferral MUST be switchable off for an embedding test server (then conflicts are
  reported at the statement, as today); it is on by default.

**Snapshot at BEGIN (item 3)**

- **FR-020**: After an accepted `BEGIN` / `START TRANSACTION`, the transaction's snapshot MUST be
  fixed at that moment: changes committed by other sessions afterwards are invisible to it, and
  writes to rows changed by such commits are conflicts.
- **FR-021**: If the first statement after `BEGIN` is DDL, micro-dsql MUST take a new snapshot
  before running it, so the DDL is not refused because of commits since `BEGIN`.
- **FR-022**: Fixing the snapshot MUST NOT change any statement's visible result, row count,
  command tag or the transaction's row/DDL counters.
- **FR-023**: Snapshot-at-BEGIN MUST be switchable off for an embedding test server; it is on by
  default.

**Locking reads (item 4)**

- **FR-030**: The emulated conflict matrix is: rows inserted, deleted, updated or selected
  `FOR UPDATE` by two overlapping transactions conflict (one of them gets OC000); disjoint rows
  never conflict; plain reads never conflict; `FOR KEY SHARE` never conflicts.
- **FR-031**: A `SELECT` over a single table with `FOR UPDATE` (optionally `OF` that table,
  `NOWAIT`, `SKIP LOCKED`) MUST be accepted, MUST return exactly the rows the same `SELECT` without
  the locking clause returns, and MUST make the transaction conflict with concurrent writes or
  `FOR UPDATE` of the selected rows as if it had written them.
- **FR-032**: `FOR UPDATE` row touches MUST NOT change any data, MUST NOT count toward the
  3,000-row limit, and MUST NOT count as DML for the DDL/DML discipline rule.
- **FR-033**: `SELECT … FOR KEY SHARE` on a single table (with the same optional modifiers) MUST be
  accepted and run as a plain read.
- **FR-034**: Locking clauses on joins, subqueries, CTEs or set operations MUST be refused with
  `0A000` and the text `locking clause is only supported on a single-table SELECT`, unless a
  live-DSQL or Dreamescaper record gives a text for that form (none does today); `FOR SHARE` /
  `FOR NO KEY UPDATE` keep M2's refusal.

**Suite and documentation**

- **FR-040**: An OCC scenario suite MUST exist under `tests/occ/` and run as part of the workspace
  tests, with one two-session scenario per emulated matrix cell and per documented difference.
- **FR-041**: Each scenario MUST assert the failing statement, SQLSTATE, exact message text, and
  final data.
- **FR-042**: docs/known-gaps.md MUST be updated: B10 and the autocommit row-limit and
  multi-statement entries closed (if their tests pass), and every knowingly different matrix cell
  recorded with the observed behaviour and suspected cause.
- **FR-043**: The five layer-1 psql scenarios MUST stay green with unchanged expected outputs.

### Key Entities

- **Transaction state**: per-session record of an explicit transaction (start time, DDL count, DML
  seen, rows changed, local settings) gains: doomed (conflict deferred to COMMIT), and snapshot
  pinned at BEGIN.
- **Implicit transaction**: a single autocommit DML statement or an atomic multi-statement string,
  opened and closed by micro-dsql without the client's knowledge.
- **Conflict matrix cell**: a pair of concurrent operations (insert/delete/update/FOR UPDATE, plain
  read, FOR KEY SHARE) with DSQL's expected outcome and micro-dsql's observed outcome.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: In a run of 8 sessions × 150 concurrent autocommit statements (inserts of distinct
  rows and updates of one hot row), 0 inserts fail and 0 errors carry any SQLSTATE other than
  `40001` OC000 (today: 391 `55P03` engine errors).
- **SC-002**: For same-row update conflicts between explicit transactions, the error is reported at
  COMMIT in 100% of the suite's scenarios (today: 0%).
- **SC-003**: Every emulated cell of DSQL's conflict matrix has a passing scenario; every other cell
  has a scenario plus a docs/known-gaps.md entry.
- **SC-004**: All five layer-1 psql scenarios pass with unchanged expected output, and the
  `26_locking_reads` probe matches the reference except for documented differences.
- **SC-005**: A retry loop (retry the whole transaction on `40001`) written for DSQL completes a
  contended counter-increment workload (4 sessions × 50 increments) with the exact final total and
  no other error class.
- **SC-006**: Single-session latency of autocommit DML stays within 2× of today's for a 1,000-row
  insert loop.

## Known differences (expected to remain; to be recorded in docs/known-gaps.md)

- **Who loses**: the engine detects a same-row conflict when the second transaction *writes*; DSQL
  fails the second to *commit*. If the later writer commits first, DSQL keeps its write and fails
  the other; micro-dsql fails the later writer.
- **First writer rolls back**: the later writer is already doomed; DSQL would let it commit.
- **Doomed transaction reads**: after the deferred conflict, the transaction runs on a fresh
  snapshot without its earlier writes (they were discarded); later reads may see data committed
  after `BEGIN`.
- **FOR KEY SHARE and foreign keys**: no key-share conflicts; a concurrent parent DELETE and child
  INSERT both commit, leaving an orphan child (DSQL: OC000).
- **Key vs non-key columns**: not distinguished (DSQL only differs for FOR KEY SHARE vs non-key
  UPDATE, which never conflicts here anyway).
- **Schema changes** (B11): any committed DDL fails every concurrently writing transaction with
  OC001, and concurrent DDL activity can make unrelated writers fail with OC000.
- **FOR UPDATE scope**: single-table only; with `LIMIT`/`ORDER BY` all rows matched by `WHERE` are
  treated as written.

## Assumptions

- All work is done in micro-dsql's own server and policy crates; engine-level probes (research.md
  §1) show the hooks needed exist, so no Turso patch is planned (constitution Principle II).
- DSQL's documented behaviour (research/01 §2) is the target where it is explicit; the MiniStack
  reference outputs are the target for statement acceptance (`26_locking_reads`).
- Retry wrappers treat OC000 and OC001 alike (both `40001`, retryable); reporting residual lock
  contention as OC001 therefore keeps every retry loop working.
- The 3,000-row limit counts only the statement's own rows (foreign-key cascades remain uncounted,
  as in M2).
- Feature 006 (M3) is merged into `main` before this feature is implemented; the two features
  touch some of the same files (listed in plan.md).
- Observability follows M2 (FR-054 of feature 005): a deferred conflict, a transparent retry and a
  residual-contention OC001 are each logged once at debug level; nothing new at info level.
- No new command-line flag: the deferral and snapshot switches exist only for embedding test
  servers (docs/plan.md §3 keeps the command line tiny).
- Live-DSQL and MiniStack differential verification are done by the owner/orchestrator, not in
  this feature's automated gates.
