# Feature Specification: M2 — DSQL error shape and the "would break in prod" rules

**Feature Branch**: `005-m2-dsql-rules`

**Created**: 2026-11-12

**Status**: Draft

**Input**: User description: "M2 — DSQL error shape and the "would break in prod" rules. The cheapest rules with the most value: they make a test fail locally when the same code would fail on real Aurora DSQL.
1. Error shaping. Map conflict errors to SQLSTATE 40001 with DSQL's exact texts: `change conflicts with another transaction (OC000)` and `schema has been updated by another transaction (OC001)`. The Rust connector parses these texts, so they must match exactly. Map 55P03 from autocommit contention to 40001/OC000 for now (fixed properly in M4).
2. Per-transaction state machine (crates/policy): at most 1 DDL per transaction; no DDL+DML in the same transaction; 3,000 mutated rows, counted with n_change(); 300 s transaction age; SAVEPOINT rejected. Error texts and codes are in research/01 §3.
3. Statement deny rules on the pg_query AST: TRUNCATE, temp tables, triggers, extensions, COPY, LISTEN/NOTIFY, advisory locks, partitions, savepoints, matviews, and disallowed column types. Use Dreamescaper's 45 rules and MiniStack's list as the spec (research/03). Prioritise the ones Turso accepts today (research/04 §C).
4. SET whitelist: accept DSQL's allowed settings as no-ops that remember their value for SHOW. Reject the rest. Includes SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ.
5. Rewrite bare `numeric` to `numeric(18,6)` in DDL.
Exit: policy unit tests, plus integration tests asserting the SQLSTATE and message for each rule. The DSQL-rule psql probes (tests/psql/probes) that these rules cover should move toward matching the MiniStack reference."

## Context

micro-dsql today accepts many statements that Aurora DSQL refuses, and reports write conflicts with
the engine's own texts (`Write-write conflict`, `Database schema conflict`, `55P03 Database is
busy`). An application tested against micro-dsql can therefore pass locally and fail in
production. This feature adds the rules whose violation is cheapest to detect and most likely to
bite: conflict error shape, per-transaction discipline and limits, a statement deny list, the
session-settings whitelist, and DSQL's meaning of bare `numeric`.

Behaviour observed on the current `main` (2026-11-12, recorded in [research.md](research.md) §1):
all 5 layer-1 psql scenarios pass; all 22 DSQL-rule probes differ from the reference. Accepted
today although DSQL refuses them: `TRUNCATE`, `SAVEPOINT`/`RELEASE`/`ROLLBACK TO`, `CREATE UNLOGGED
TABLE`, `serial` columns, `money`/`xml`/`inet`/`cidr`/`macaddr`/`tsvector`/`point`/array columns,
`CREATE TYPE … AS ENUM`, `DROP TYPE`, `CREATE TABLE … AS`, `COPY … TO STDOUT`,
`SET TRANSACTION ISOLATION LEVEL REPEATABLE READ`, two DDLs or DDL plus DML in one transaction, and
more than 3,000 mutated rows per transaction.

## Clarifications

### Session 2026-11-12

The owner could not be asked during this run; each answer is the recommended default, chosen for
consistency with the constitution and docs/plan.md.

- Q: When the live-DSQL text and the MiniStack reference text differ, which one do the rules use? → A: The live-DSQL text (precedence: live record > Dreamescaper rule > research/01 > MiniStack); probe progress is measured on outcome and SQLSTATE (SC-003).
- Q: Does the 55P03 → 40001/OC000 mapping apply only to autocommit statements? → A: No, to every engine lock-contention error; the spike observed it only in autocommit, so the effect is the same.
- Q: What state is the session in after a COMMIT refused by the age limit? → A: The transaction is rolled back and the session is idle (status `I`), like a failed COMMIT in PostgreSQL.
- Q: How is `role` handled outside `SET LOCAL`? → A: `SET role` / `SET SESSION role` / `RESET role` fail like any non-whitelisted setting; `SET LOCAL role` inside a transaction is a remembered no-op.
- Q: Should rule refusals be visible in the server log? → A: Yes, one debug-level line per refusal naming the rule id; nothing at info level.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Write conflicts look exactly like DSQL's (Priority: P1)

An application's retry helper (the official connectors, including the Rust one that parses the
message text) runs against micro-dsql. When two transactions conflict, or a concurrent schema
change invalidates a transaction, the client receives SQLSTATE `40001` with DSQL's exact message,
so the same retry path that runs in production runs in tests.

**Why this priority**: The Rust connector tells OC000 and OC001 apart only by the message text. A
wrong text means the retry loop under test is not the one that runs in production.

**Independent Test**: Two sessions update the same row in explicit transactions; the losing
statement or COMMIT reports `40001` with `change conflicts with another transaction (OC000)`. A
writing transaction aborted by a concurrent DDL reports `40001` with `schema has been updated by
another transaction (OC001)`.

**Acceptance Scenarios**:

1. **Given** two sessions in explicit transactions that write the same row, **When** the second
   writer's conflict is detected, **Then** it receives SQLSTATE `40001` and the message is exactly
   `change conflicts with another transaction (OC000)`.
2. **Given** two sessions that insert the same primary key and commit, **When** the second COMMIT
   fails, **Then** it receives `40001` and the OC000 message.
3. **Given** a writing transaction and a concurrent committed DDL, **When** the writing transaction
   commits, **Then** it receives `40001` and the message is exactly `schema has been updated by
   another transaction (OC001)`.
4. **Given** concurrent autocommit writes that make the engine report lock contention (`55P03`),
   **When** that error is sent to the client, **Then** the client receives `40001` with the OC000
   message instead.
5. **Given** any other engine error (unique violation, unknown table, syntax error), **When** it is
   sent, **Then** its SQLSTATE and shaping are unchanged from today.

---

### User Story 2 - Transaction discipline and limits fail locally (Priority: P1)

A developer's migration or test runs several statements in one transaction. Where DSQL would
refuse the transaction (two DDLs, DDL mixed with DML, a savepoint, more than 3,000 mutated rows, a
transaction older than 300 s), micro-dsql refuses it at the same statement with DSQL's SQLSTATE and
message, and the transaction is aborted as it would be on DSQL.

**Why this priority**: These are the commonest "works locally, fails on DSQL" surprises (ORM
migrations mixing DDL and seed data, bulk updates, savepoint-based nested transactions).

**Independent Test**: `BEGIN; CREATE TABLE a …; INSERT INTO a …` fails at the INSERT with `0A000
ddl and dml are not supported in the same transaction`; the next statement gets `25P02`; after
ROLLBACK the table does not exist.

**Acceptance Scenarios**:

1. **Given** an explicit transaction that ran one DDL statement, **When** a second DDL statement
   runs, **Then** it fails with `0A000` `multiple ddl statements not supported in a transaction`
   and the transaction is aborted.
2. **Given** an explicit transaction that ran DDL, **When** DML (INSERT, UPDATE, DELETE, or a
   data-modifying statement in a WITH clause) runs, **Then** it fails with `0A000` `ddl and dml are
   not supported in the same transaction`; the same holds for DDL after DML.
3. **Given** an explicit transaction that ran only reads, **When** one DDL runs and the transaction
   commits, **Then** both succeed.
4. **Given** COMMIT or ROLLBACK, **When** a new transaction starts, **Then** its DDL/DML counters
   start from zero.
5. **Given** an explicit transaction, **When** a statement brings the transaction's mutated-row
   total above 3,000, **Then** that statement fails with `54000` `transaction row limit exceeded`,
   the transaction is aborted, and after ROLLBACK none of its rows exist. Exactly 3,000 rows is
   allowed.
6. **Given** an autocommit `INSERT … VALUES` with more than 3,000 rows, **When** it runs, **Then**
   it fails with `54000` `transaction row limit exceeded` and inserts nothing.
7. **Given** an explicit transaction that began more than 300 s ago, **When** any further
   statement (including COMMIT) runs, **Then** it fails with `54000` `transaction age limit of
   300s exceeded` and none of the transaction's writes are kept.
8. **Given** any session, **When** `SAVEPOINT`, `RELEASE [SAVEPOINT]` or `ROLLBACK TO [SAVEPOINT]`
   runs, **Then** it fails with `0A000` and `unsupported transaction statement: SAVEPOINT`,
   `… RELEASE` or `… ROLLBACK TO` respectively.
9. **Given** a single simple-protocol query string with several statements outside an explicit
   transaction, **When** it contains two DDL statements, or DDL and DML, **Then** it fails with the
   corresponding `0A000` message before any of its statements runs.

---

### User Story 3 - Statements DSQL refuses are refused (Priority: P2)

A developer's schema or code uses a PostgreSQL feature DSQL does not have (TRUNCATE, unlogged or
temporary tables, triggers, extensions, enums, unsupported column types, `CREATE TABLE AS`,
materialized views, `COPY`, LISTEN/NOTIFY, advisory locks, …). micro-dsql refuses the statement
with DSQL's SQLSTATE and message instead of running it (or instead of refusing it with an
engine-specific SQLSTATE such as `42601`).

**Why this priority**: The statements Turso accepts today are silent production failures; those it
already refuses only need the right SQLSTATE and message.

**Independent Test**: `TRUNCATE t` fails with `0A000` `unsupported statement: Truncate` and the
rows are still there; `CREATE TABLE t (c money)` fails with `0A000` `datatype money not
supported`.

**Acceptance Scenarios**:

1. **Given** any statement in the rule catalogue (FR-020 to FR-024), **When** it runs, **Then** it
   fails with the catalogue's SQLSTATE and message and has no effect.
2. **Given** a refused statement inside an explicit transaction, **When** the next statement runs,
   **Then** it gets `25P02` until ROLLBACK (the refusal aborts the transaction).
3. **Given** a refused statement outside a transaction, **When** the next statement runs, **Then**
   it runs normally.
4. **Given** an already aborted transaction, **When** a statement that a rule would refuse runs,
   **Then** it gets `25P02`, not the rule's error.
5. **Given** statements DSQL supports (CREATE TABLE with supported types, `CREATE DOMAIN`, arrays
   and `inet` in query expressions, `FOR UPDATE`, partition-free DDL), **When** they run, **Then**
   no rule refuses them.

---

### User Story 4 - Only DSQL's session settings can be set (Priority: P2)

A driver or ORM sets session parameters at connect time. Settings DSQL allows are accepted and
`SHOW` returns the value that was set; settings DSQL refuses fail with DSQL's error, so a
configuration that would fail in production fails locally.

**Why this priority**: Pool and ORM configuration (`statement_timeout`, `work_mem`, isolation
settings) is a common production failure; the allowed list is small and documented.

**Independent Test**: `SET work_mem = '64MB'` fails with `0A000` `setting configuration parameter
"work_mem" not supported`; `SET enable_seqscan = off; SHOW enable_seqscan` returns `off`.

**Acceptance Scenarios**:

1. **Given** a whitelisted setting (FR-030), **When** it is set with `SET`, `SET SESSION` or
   `SET … TO`, **Then** the command succeeds and `SHOW` returns the new value; `RESET` and `SET …
   TO DEFAULT` restore the default.
2. **Given** `SET LOCAL` of a whitelisted setting inside a transaction, **When** the transaction
   ends, **Then** `SHOW` returns the value from before the transaction.
3. **Given** a setting not on the whitelist, **When** it is set with `SET`, `RESET` or
   `set_config('<name>', …)`, **Then** it fails with `0A000` `setting configuration parameter
   "<name>" not supported`.
4. **Given** `SET TRANSACTION …` (any mode), **When** it runs, **Then** it fails with `0A000`
   `setting configuration parameter "TRANSACTION" not supported`.
5. **Given** `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ`, **When**
   it runs, **Then** it succeeds as a no-op; with another isolation level it fails with `0A000`
   `Unsupported isolation level: <LEVEL>`.
6. **Given** a boolean setting (`enable_*`, `dsql.enable_batched_nestloop`), **When** it is set to
   a non-boolean value, **Then** it fails with `22023` `parameter "<name>" requires a Boolean
   value`; with two values, `22023` `SET <name> takes only one argument`.
7. **Given** an unknown `dsql.*` name, **When** it is set, **Then** it fails with `42602` `invalid
   configuration parameter name "<name>"` and DETAIL `"dsql" is a reserved prefix.`
8. **Given** `SHOW lc_collate` or `SHOW lc_ctype`, **When** it runs, **Then** it fails with
   `42704` `unrecognized configuration parameter "<name>"` (as on DSQL, PostgreSQL 16).

---

### User Story 5 - Bare `numeric` columns mean `numeric(18,6)` (Priority: P3)

A schema declares a column as bare `numeric` (or `decimal`). On DSQL that means `numeric(18,6)`;
micro-dsql stores and returns values with the same scale.

**Why this priority**: Silent precision differences are rare in tests but confusing when they
appear; the rewrite is small.

**Independent Test**: `CREATE TABLE t (id int PRIMARY KEY, n numeric); INSERT INTO t VALUES (1,
1.5); SELECT n FROM t` returns `1.500000`.

**Acceptance Scenarios**:

1. **Given** `CREATE TABLE` or `ALTER TABLE … ADD COLUMN` with a bare `numeric` or `decimal`
   column, **When** it runs, **Then** the column behaves as `numeric(18,6)`.
2. **Given** `numeric(p)` or `numeric(p,s)`, or `numeric` in a cast or expression, **When** it
   runs, **Then** nothing is rewritten.

---

### Edge Cases

- A statement that does not parse is never refused by a rule: the engine reports the syntax error
  as today.
- Rules apply identically to the simple and the extended query protocol; for the extended protocol
  they are evaluated when the statement executes (the transaction state at Parse time may differ).
- `CREATE SCHEMA` with nested elements counts as one DDL.
- `SELECT` (including `SELECT … FOR UPDATE`) is neither DDL nor DML; a read-only transaction may
  run one DDL.
- `INSERT … RETURNING`, `UPDATE … RETURNING` and `DELETE … RETURNING` count their rows toward the
  limit like the forms without RETURNING.
- A statement that fails in the engine adds no rows to the count; the transaction is aborted anyway.
- Identifiers in messages use the name as PostgreSQL reports it (lower-cased unless quoted); type
  names use PostgreSQL's display form (`int[]` is reported as `integer[]`).
- `TRUNCATE` inside a transaction, `SAVEPOINT` outside one, and a second `SAVEPOINT` in an already
  aborted transaction follow the abort rules of User Story 3.
- `SET timezone` keeps today's behaviour: UTC spellings accepted, others `22023` (the engine has
  only UTC).
- `search_path`, `application_name` and `client_encoding` are honoured by the engine, so setting
  them must still reach the engine; `SHOW` reports the engine's value.

## Requirements *(mandatory)*

### Functional Requirements

**Error shape**

- **FR-001**: Every engine error classified as a data conflict (write-write conflict, conflicting
  commit, stale snapshot, aborted commit dependency) MUST be sent with SQLSTATE `40001` and the
  message `change conflicts with another transaction (OC000)`.
- **FR-002**: Every engine error classified as a schema conflict MUST be sent with SQLSTATE `40001`
  and the message `schema has been updated by another transaction (OC001)`.
- **FR-003**: Engine lock-contention errors (SQLSTATE `55P03`, "database is busy") MUST be sent as
  `40001` with the OC000 message, in autocommit and in explicit transactions alike. This is an
  interim mapping until autocommit execution changes (milestone M4).
- **FR-004**: Errors raised by micro-dsql's rules MUST be sent as ordinary ErrorResponses with
  severity `ERROR`, the rule's SQLSTATE and message, and DETAIL when the rule has one; the
  connection MUST stay usable.
- **FR-005**: Existing shaping of other engine errors (`42P01`, `42601`, constraint errors) MUST
  be unchanged.
- **FR-006**: Conflict and rule messages MUST be produced from one place, so later milestones
  (deferred OC000 at COMMIT) produce byte-identical texts.

**Transaction discipline and limits**

- **FR-010**: Within one explicit transaction, a second DDL statement MUST fail with `0A000`
  `multiple ddl statements not supported in a transaction`.
- **FR-011**: Within one explicit transaction, DML after DDL and DDL after DML MUST fail with
  `0A000` `ddl and dml are not supported in the same transaction`.
- **FR-012**: The mutated-row total of an explicit transaction (rows inserted, updated or deleted,
  as reported by the engine per statement) MUST NOT exceed 3,000; the statement that exceeds it
  MUST fail with `54000` `transaction row limit exceeded` and abort the transaction.
- **FR-013**: An autocommit `INSERT … VALUES` with more than 3,000 rows MUST fail with `54000`
  `transaction row limit exceeded` before it runs.
- **FR-014**: Once an explicit transaction is older than 300 s (measured from BEGIN), the next
  statement MUST fail with `54000` `transaction age limit of 300s exceeded`; if that statement is
  COMMIT, the transaction MUST be rolled back and the session left idle. Otherwise the
  transaction is aborted.
- **FR-015**: `SAVEPOINT`, `RELEASE [SAVEPOINT]` and `ROLLBACK TO [SAVEPOINT]` MUST fail with
  `0A000` `unsupported transaction statement: SAVEPOINT` / `RELEASE` / `ROLLBACK TO`.
- **FR-016**: A simple-protocol query string containing several statements outside an explicit
  transaction MUST be checked as one implicit transaction for FR-010 and FR-011, and refused
  before any of its statements runs if it violates them.
- **FR-017**: A rule error raised inside an explicit transaction MUST abort it (later statements get
  `25P02` until ROLLBACK); outside a transaction it MUST have no lasting effect; inside an already
  aborted transaction the engine's `25P02` MUST take precedence over any rule.
- **FR-018**: The row and age limits MUST be configurable for tests by the embedding program; the
  shipped binary MUST use 3,000 rows and 300 s.

**Statement deny rules** (the full catalogue with exact texts is in
[contracts/rule-catalogue.md](contracts/rule-catalogue.md); the messages below are normative)

- **FR-020** (accepted by the engine today; P2): `TRUNCATE` → `0A000` `unsupported statement:
  Truncate`; `CREATE UNLOGGED TABLE` → `0A000` `UNLOGGED table not supported for CREATE TABLE`;
  `CREATE TABLE … AS` → `0A000` `unsupported statement: CreateTableAs`; `CREATE TYPE … AS ENUM` →
  `0A000` `CREATE TYPE not supported`; `ALTER TYPE` → `0A000` `ALTER TYPE not supported`; `DROP
  TYPE` → `0A000` `unsupported object in DROP statement`; `ALTER TYPE … RENAME` → `0A000`
  `unsupported object in RENAME statement`; `COPY` (any direction) → `0A000` `COPY is not
  supported`.
- **FR-021** (column types; CREATE TABLE and ALTER TABLE ADD COLUMN): `serial`, `bigserial`,
  `smallserial` → `42704` `type "<name>" does not exist`; `money`, `xml`, `inet`, `cidr`,
  `macaddr`, `macaddr8`, `point`, `line`, `lseg`, `box`, `path`, `polygon`, `circle`, `tsvector`,
  `tsquery`, the range types, `bit`, `varbit`, `pg_lsn` → `0A000` `datatype <display name> not
  supported`; any array column → `0A000` `datatype <element display name>[] not supported`.
- **FR-022** (refused by the engine today with another SQLSTATE or text; P3): `CREATE TEMPORARY
  TABLE` → `TEMPORARY or TEMP table not supported for CREATE TABLE`; `CREATE EXTENSION` →
  `unsupported statement: CreateExtension`; `CREATE TRIGGER` → `CREATE TRIGGER not supported`;
  `CREATE DATABASE` → `unsupported statement: Createdb`; `CREATE MATERIALIZED VIEW` →
  `unsupported statement: CreateTableAs`; `CREATE TYPE … AS (…)` → `unsupported statement:
  CompositeType`; `CREATE TYPE … AS RANGE` → `unsupported statement: CreateRange`; `CREATE
  TABLESPACE` → `unsupported statement: CreateTableSpace`; `CREATE FOREIGN TABLE` → `unsupported
  statement: CreateForeignTable`; `VACUUM` → `unsupported VACUUM statement`; `LISTEN` / `NOTIFY` /
  `UNLISTEN` → `unsupported statement: Listen` / `Notify` / `Unlisten`; `ALTER SYSTEM` →
  `unsupported statement: AlterSystem`; `MERGE` → `unsupported statement type: MERGE`; `CREATE
  FUNCTION … LANGUAGE <x>` with x ≠ sql → `CREATE FUNCTION with language <x> not supported`; `FOR
  SHARE` / `FOR NO KEY UPDATE` → `locking clauses other than FOR UPDATE/FOR KEY SHARE are not
  supported`; `TABLESAMPLE` → `TABLESAMPLE clause not supported`. All `0A000`.
- **FR-023** (no live-DSQL record; MiniStack text; P3): `LOCK TABLE` → `LOCK TABLE is not
  supported`; `DO` → `DO is not supported`; `CREATE PROCEDURE` → `CREATE PROCEDURE is not
  supported`; `CREATE RULE` → `CREATE RULE is not supported`; `CREATE PUBLICATION` → `CREATE
  PUBLICATION is not supported`; `REINDEX` → `REINDEX is not supported`; `CLUSTER` → `CLUSTER is
  not supported`; `PREPARE TRANSACTION` → `prepared transactions is not supported`; calls to
  `pg_advisory_*` / `pg_try_advisory_*` functions → `advisory locks are not supported`. All
  `0A000`.
- **FR-024**: Statements DSQL supports MUST NOT be refused by any rule; in particular `CREATE
  DOMAIN`, `CREATE INDEX` (with or without `ASYNC`; ASYNC handling is M3), `ALTER TABLE` forms
  other than those named above, `SELECT … FOR UPDATE` / `FOR KEY SHARE`, arrays and `inet` in
  expressions, and `CREATE FUNCTION … LANGUAGE sql`.

**Session settings**

- **FR-030**: The settable parameters MUST be exactly: `application_name`, `client_encoding`,
  `datestyle`, `extra_float_digits`, `intervalstyle`, `timezone`, `search_path`,
  `enable_bitmapscan`, `enable_hashjoin`, `enable_indexonlyscan`, `enable_indexscan`,
  `enable_material`, `enable_mergejoin`, `enable_nestloop`, `enable_seqscan`,
  `dsql.enable_batched_nestloop`, and `role` (only with `SET LOCAL` inside a transaction; `SET
  role`, `SET SESSION role` and `RESET role` are treated as non-whitelisted, FR-032).
- **FR-031**: Setting a whitelisted parameter MUST succeed; `SHOW <name>` MUST then return the value
  set (for parameters the engine does not implement, the value is remembered per session, with
  PostgreSQL's column name and DSQL's defaults: `on` for `enable_*` and
  `dsql.enable_batched_nestloop`); `RESET <name>`, `RESET ALL` and `SET <name> TO DEFAULT` MUST
  restore the default; `SET LOCAL` values MUST end with the transaction.
- **FR-032**: `SET`, `SET LOCAL`, `RESET` and `set_config()` (with a constant name) of any other
  parameter MUST fail with `0A000` `setting configuration parameter "<name>" not supported`, except
  unknown `dsql.*` names, which MUST fail with `42602` `invalid configuration parameter name
  "<name>"` and DETAIL `"dsql" is a reserved prefix.`
- **FR-033**: `SET TRANSACTION …` MUST fail with `0A000` `setting configuration parameter
  "TRANSACTION" not supported`.
- **FR-034**: `SET SESSION CHARACTERISTICS AS TRANSACTION` with only `ISOLATION LEVEL REPEATABLE
  READ` and/or `READ WRITE` MUST succeed as a no-op; any other isolation level MUST fail with
  `0A000` `Unsupported isolation level: <LEVEL>`; other modes MUST fail like FR-033.
- **FR-035**: Boolean parameters MUST reject non-boolean values with `22023` `parameter "<name>"
  requires a Boolean value`, and every whitelisted parameter except `search_path` and `datestyle`
  MUST reject a value list with `22023` `SET <name> takes only one argument`.
- **FR-036**: `SHOW lc_collate` and `SHOW lc_ctype` MUST fail with `42704` `unrecognized
  configuration parameter "<name>"`; other `SHOW` answers of today stay unchanged.

**DDL rewrite**

- **FR-040**: In `CREATE TABLE` and `ALTER TABLE … ADD COLUMN`, a column type written as bare
  `numeric` or `decimal` (no precision) MUST be treated as `numeric(18,6)`.
- **FR-041**: Rewrites MUST NOT change any other part of the statement, and error positions
  reported for the statement MUST refer to the text the client sent.

**Extensibility and testing**

- **FR-050**: Rules, rewrites, the settings whitelist and the per-transaction state MUST work
  without a database engine (unit-testable) and depend only on the parsed statement and on facts
  the server passes in (transaction status, rows changed, current time).
- **FR-051**: Every rule in FR-001 to FR-041 MUST have an integration test that asserts its
  SQLSTATE and exact message through a real client connection.
- **FR-052**: The layer-1 psql scenarios MUST stay green.
- **FR-053**: `docs/known-gaps.md` MUST be updated: entries closed by this feature removed or
  narrowed, new gaps recorded (see Assumptions).
- **FR-054**: Each rule refusal MUST be logged once at debug level with the rule id; nothing is
  logged at info level for refusals.

### Key Entities

- **Rule**: an identifier, the statements it matches, the SQLSTATE, message (and optional DETAIL)
  it raises, and the source of that text (live DSQL record, Dreamescaper rule, research/01,
  MiniStack).
- **Session settings**: per connection, the current value of each whitelisted parameter the engine
  does not implement, plus `SET LOCAL` overrides scoped to the current transaction.
- **Transaction state**: per connection, whether an explicit transaction is open, when it began,
  how many DDL statements it ran, whether it ran DML, and its mutated-row total.
- **Conflict class**: the kind of an engine failure as far as error shaping needs it (data
  conflict, schema conflict, lock contention, other).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of the rules in FR-001 to FR-041 have at least one integration test asserting
  SQLSTATE and exact message (or, for FR-040, the stored scale), and all pass.
- **SC-002**: The 5 layer-1 psql scenarios pass against micro-dsql, before and after the change.
- **SC-003**: In the 14 probes these rules cover (`01`, `02`, `03`, `06`, `07`, `08`, `09`, `10`,
  `20`, `21`, `23`, `24`, `25`, `29`), every statement covered by an M2 rule is accepted or refused
  exactly as in the reference output, with the same SQLSTATE, except the documented reference
  deviations (MiniStack's `25006` for discipline and row-limit errors, `0A000` for `serial`, and
  statements the engine cannot run for unrelated reasons such as `to_regclass`).
- **SC-004**: The rules add no noticeable latency: the layer-1 scenarios and the integration suite
  run no more than 10% slower than before.
- **SC-005**: Every M2-closed entry in `docs/known-gaps.md` is removed or narrowed in the same
  change, and every new gap found is recorded.

## Assumptions

Scope decisions (from the owner, recorded here):

- In scope from `docs/known-gaps.md`: B9 (SET whitelist part only), B10 (55P03 → 40001/OC000
  mapping only), B13, "SET SESSION CHARACTERISTICS" and "Errors".
- Out of scope for 005: B1/B2/B3 (parameter and result type inference), B14 (integer range and
  SQLSTATE quirks), DEALLOCATE. Their entries stay in `docs/known-gaps.md`; the plan may retag
  their milestone to "later".

Defaults chosen without asking (one-line reason each):

- **Message source precedence**: live DSQL recording (Dreamescaper golden probes, pinned commit) >
  Dreamescaper rule set > research/01 > MiniStack. Reason: the goal is production fidelity;
  research wins on facts (docs/plan.md), and MiniStack documents its own deviations.
- **DDL/DML discipline SQLSTATE is `0A000`**, not MiniStack's `25006`. Reason: live DSQL record;
  research/01 §3.1 already guessed `0A000`.
- **Row limit is `54000` `transaction row limit exceeded`**. Reason: research/01 §3.2 and the live
  record agree.
- **Transaction age limit is 300 s** although Dreamescaper's rule file uses 1,800 s. Reason:
  research/01 §3.2 (quotas page) and the brief.
- **Autocommit row limit is enforced only when the row count is known before running** (`INSERT …
  VALUES`). Reason: an autocommit statement has committed by the time its row count is known;
  changing autocommit execution is M4. Recorded as a new known gap.
- **FK cascade rows are not counted**. Reason: the engine's per-statement change count excludes
  cascaded rows (observed: `DELETE 1` for a parent with 3 cascaded children); DSQL counts them.
  Recorded as a new known gap.
- **A multi-statement simple query is checked as one implicit transaction but not made atomic**.
  Reason: the static check is cheap; atomic execution changes the engine's autocommit behaviour
  (M4). Recorded as a known gap.
- **`SET SESSION CHARACTERISTICS … REPEATABLE READ` is accepted** as a no-op. Reason: owner brief
  and the existing known-gap entry; DSQL's behaviour is unrecorded (Dreamescaper refuses it by
  analogy with `SET TRANSACTION`), so this is an owner follow-up.
- **`SET TRANSACTION ISOLATION LEVEL REPEATABLE READ` is refused**. Reason: live DSQL record
  refuses every `SET TRANSACTION`.
- **`CREATE DOMAIN` stays accepted**. Reason: DSQL documents it as supported and Dreamescaper does
  not refuse it; MiniStack's refusal is a known MiniStack deviation.
- **`COPY` is refused in both directions** with MiniStack's text. Reason: brief and MiniStack; no
  live record (owner follow-up).
- **GRANT/REVOKE, COMMENT ON, CREATE/ALTER/DROP of any object, CREATE INDEX and RENAME count as
  DDL**; INSERT/UPDATE/DELETE/MERGE and data-modifying WITH as DML. Reason: DSQL's OC001 is caused
  by any catalog change including GRANT (research/01 §2).
- **CREATE INDEX without ASYNC, identity/sequence CACHE rules, ALTER TABLE ADD CONSTRAINT / NOT
  VALID / ADD COLUMN with constraint, and ALTER COLUMN TYPE rules are M3**. Reason: they belong to
  the DDL-that-ORMs-emit work and need the pre-parse rewrite seam first.
- **Probe expected files are never re-captured** against micro-dsql; probe progress is measured by
  SC-003. Reason: constitution and tests/psql README.
- Dependencies: the engine reports a per-statement changed-row count and the session's transaction
  status; the server can classify engine failures into the conflict classes above.
