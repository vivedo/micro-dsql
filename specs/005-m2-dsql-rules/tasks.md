---
description: "Task list for M2 — DSQL error shape and the would-break-in-prod rules"
---

# Tasks: M2 — DSQL error shape and the "would break in prod" rules

**Input**: Design documents from `specs/005-m2-dsql-rules/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/rule-catalogue.md,
contracts/policy-api.md, quickstart.md

**Tests**: Required. Constitution Principle IV (test-first) and the spec's exit criteria (FR-051):
every rule gets a failing policy unit test and a failing integration test (SQLSTATE + exact
message) before it is implemented. Policy unit tests never start an engine.

**Markers**: `[orchestrator]` = needs the shared MiniStack/Docker resource or coordination outside
this worktree; `[owner]` = needs the owner (live DSQL access or a decision). Everything else is for
the implementer. Tests use port 0 (in-process) or a free non-5432 port; never 5432.

**Conventions**: never re-capture `tests/psql/probes/expected/*` against micro-dsql; `research/`
is read-only; no machine-specific paths in committed files; messages are copied byte-exact from
contracts/rule-catalogue.md.

## Format: `[ID] [P?] [Story] Description`

## Phase 1: Setup

- [x] T001 Record the baseline: run `cargo test --workspace` and, against a server on a free port, `tests/psql/run.sh --all` with `RESULTS_DIR=reports/m2-baseline` (gitignored); confirm scenarios pass=5 and probes differ=22 as in specs/005-m2-dsql-rules/research.md §1

## Phase 2: Foundational (blocking prerequisites for every story)

**Purpose**: the pipeline, the session object and the server adapter, with no rule active yet.
All existing tests must stay green at the end of this phase.

- [x] T002 [P] Write failing unit tests for `PolicyError` (fields `rule`, `sqlstate`, `message`, `detail`) and the constructors `oc000()`, `oc001()`, `row_limit()`, `age_limit(300)` (exact texts from contracts/rule-catalogue.md §1–2) in crates/policy/src/errors.rs
- [x] T003 Implement `PolicyError`, `EngineFailure { DataConflict, SchemaConflict, LockBusy, Other }` and the constructors in crates/policy/src/errors.rs; extend `ShapedError` with `sqlstate: String` and `detail: Option<String>` keeping today's `shape_error` results unchanged (existing unit tests stay green)
- [x] T004 [P] Write failing unit tests for `classify()` in crates/policy/src/classify.rs: DDL / DML / Read / Txn / Session per the definitions in contracts/rule-catalogue.md §2 (incl. `CREATE SCHEMA` with nested elements = one DDL, `GRANT` = DDL, `WITH x AS (INSERT …) SELECT` = DML, `SELECT … FOR UPDATE` = Read, `SAVEPOINT`/`RELEASE`/`ROLLBACK TO`/`PREPARE TRANSACTION` kinds)
- [x] T005 Implement `StatementClass`, `TxnControl` and `classify()` in crates/policy/src/classify.rs
- [x] T006 [P] Write failing unit tests for `rewrite::pre_parse` (identity in 005), `TextEdit` and `apply_edits` (several non-overlapping inserts, offsets in bytes of the original text) in crates/policy/src/rewrite.rs
- [x] T007 Implement `pre_parse`, `TextEdit`, `apply_edits` and an empty `REWRITES` registry in crates/policy/src/rewrite.rs
- [x] T008 Implement `StatementPlan::new(sql)` in crates/policy/src/statement.rs following the pipeline in contracts/policy-api.md §2 (fields per data-model.md: `original`, `engine_sql`, `parse_failed`, `class`, `refusal`, `setting`, `intercept`, `values_rows`); `refusal`/`setting` stay `None` until US3/US4; unit tests for `parse_failed`, `values_rows` (`INSERT … VALUES` with 1 and 3,001 rows; `None` for `INSERT … SELECT`) and `engine_sql == original` when nothing is rewritten
- [x] T009 Implement the skeleton of crates/policy/src/session.rs: `PolicyLimits` (Default: `max_rows_per_txn = 3000`, `max_txn_age = 300 s`), `TxnStatus { Idle, Active, Aborted }`, `TxnState`, `Admission { Run, Respond, Refuse, RollbackThenRefuse }`, `Reply { Show, Command }`, `SessionPolicy::{new, admit, complete, failed, check_batch}` where `admit` returns `Run` (or the existing intercept as `Respond`/`Refuse`) and `complete`/`failed` maintain transaction boundaries from status (research.md D4); unit tests for Idle→Active→Idle bookkeeping
- [x] T010 Replace `Intercept::Error { sqlstate, message }` with `Intercept::Error(PolicyError)` in crates/policy/src/intercept.rs and update its tests; export the new modules and types from crates/policy/src/lib.rs and update the crate docs
- [x] T011 Create crates/server/src/dsql.rs with `txn_status(&PgConnection) -> TxnStatus`, `engine_failure(&turso_pg::Error) -> EngineFailure` (variant mapping from contracts/policy-api.md §5, via `core_error()`) and `run_planned(conn, policy, plan, …)` implementing the execution path in plan.md (admit → engine → `n_change()` → complete/failed; Refuse aborts an Active block with `abort_transaction_block_on_error()` and logs the rule id at debug level; RollbackThenRefuse runs `ROLLBACK`)
- [x] T012 Wire the server in crates/server/src/lib.rs: `TursoPgHandler` holds `Arc<Mutex<SessionPolicy>>`; `PgStatement` holds `Arc<StatementPlan>` instead of `intercept` (built once in `PgStatement::analyze`); `run_simple_statement` and extended `do_query` call `dsql::run_planned`; read `n_change()` after `execute_query` too; `finish_query` sends `PolicyError` as C/M/D fields and drops the private `SqlError`; keep the COPY path for aborted blocks only
- [x] T013 Add `ServerOptions { limits: PolicyLimits }` and `Server::bind_with(address, storage, options)` in crates/server/src/lib.rs (`Server::bind` delegates with defaults); add `TestServer::start_with(ServerOptions)` in crates/server/tests/smoke/common.rs; register `mod dsql_conflicts; mod dsql_rules; mod dsql_settings;` (empty files) in crates/server/tests/smoke/main.rs
- [x] T014 Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace` and `tests/psql/run.sh` (layer 1) — all green, no behaviour change yet

**Checkpoint**: pipeline in place; stories can proceed (US1–US5 touch different modules and test files).

## Phase 3: User Story 1 - Write conflicts look exactly like DSQL's (Priority: P1) MVP

**Goal**: conflict errors are `40001` with DSQL's exact OC000/OC001 texts; `55P03` becomes OC000.

**Independent Test**: two `tokio-postgres` clients update the same row in explicit transactions; the
loser's error has SQLSTATE `40001` and message `change conflicts with another transaction (OC000)`.

- [x] T015 [P] [US1] Write failing unit tests for `shape_engine_error` in crates/policy/src/errors.rs: `DataConflict` and `LockBusy` (with engine SQLSTATE `55P03`) → `40001` OC000; `SchemaConflict` → `40001` OC001; `Other` → unchanged `shape_error` behaviour (42P01 message/position, 42601 position, 23xxx schema)
- [x] T016 [P] [US1] Write failing integration tests in crates/server/tests/smoke/dsql_conflicts.rs: (a) same-row UPDATE in two explicit transactions → loser gets `40001` + OC000 text; (b) same-PK INSERT, second COMMIT → `40001` + OC000; (c) transaction A writes, session B commits `ALTER TABLE … ADD COLUMN`, A's COMMIT → `40001` + `schema has been updated by another transaction (OC001)`; (d) autocommit storm (several clients, unique inserts + hot-row updates, bounded iterations) → no error has SQLSTATE `55P03`, and every error is `40001` with the OC000 text; (e) the connection stays usable after each
- [x] T017 [US1] Implement `shape_engine_error` in crates/policy/src/errors.rs and use it in `finish_query` with `dsql::engine_failure` in crates/server/src/lib.rs (positions computed against the client's original text)
- [x] T018 [US1] Verify tests/psql/40_conflict.sh and tests/psql/20_txn.sql still pass unchanged (they compare SQLSTATEs only)

**Checkpoint**: US1 is the MVP: retry helpers see DSQL's texts.

## Phase 4: User Story 2 - Transaction discipline and limits fail locally (Priority: P1)

**Goal**: one DDL per transaction, no DDL+DML, savepoints refused, 3,000-row limit, 300 s age limit,
multi-statement strings checked as one implicit transaction.

**Independent Test**: `BEGIN; CREATE TABLE a (…); INSERT INTO a …` fails at the INSERT with `0A000
ddl and dml are not supported in the same transaction`; next statement `25P02`; after ROLLBACK the
table does not exist.

- [x] T019 [P] [US2] Write failing unit tests in crates/policy/src/session.rs (hand-built `TxnStatus`, `Instant`, `rows_changed`; no engine): `txn_two_ddl` ("a 2nd DDL → `txn_two_ddl`"), `txn_ddl_dml` both orders ("DDL after DML, DML after DDL → `txn_ddl_dml`"), read-only txn + one DDL allowed, counters reset after Idle, savepoint/release/rollback-to refused with their texts, row total 3,000 allowed and 3,001 → `row_limit` ("`> max_rows_per_txn` → `row_limit`, block aborted"), autocommit `values_rows` 3,001 → `row_limit` before running, age: `now - began_at > max_txn_age` → `age_limit` and COMMIT → `RollbackThenRefuse`, Aborted status → always `Run`, `check_batch` (two DDLs / DDL+DML refused; `BEGIN; CREATE INDEX …; COMMIT` and `SELECT 1; SELECT 2` allowed)
- [x] T020 [P] [US2] Write a failing integration test in crates/server/tests/smoke/dsql_rules.rs proving the engine change count is passed for `INSERT/UPDATE/DELETE … RETURNING` (2,000 + 1,001 rows via RETURNING in one transaction → `54000 transaction row limit exceeded`); if `n_change()` is not set on that path, record the finding in specs/005-m2-dsql-rules/research.md and count result rows for RETURNING DML instead
- [x] T021 [P] [US2] Write failing integration tests in crates/server/tests/smoke/dsql_rules.rs for every row of contracts/rule-catalogue.md §2, simple and extended protocol: exact SQLSTATE and message; `25P02` on the next statement inside a transaction; no effect after ROLLBACK (table absent / 0 rows); 3,000 rows OK (`INSERT … SELECT generate_series(1, 3000)` in a txn); cumulative 2,000 + 2,000 refused; autocommit 3,001-row `INSERT … VALUES` refused and nothing inserted; age limit with `ServerOptions { limits: PolicyLimits { max_txn_age: 1 s, .. } }` (statement after the limit → `54000`; COMMIT after the limit → `54000`, session idle, writes gone); multi-statement simple query `CREATE TABLE …; INSERT …` refused with no table created
- [x] T022 [US2] Implement discipline, savepoint refusal, age limit and the autocommit `values_rows` check in `SessionPolicy::admit`, and row accumulation in `SessionPolicy::complete` (order of checks per contracts/policy-api.md §3) in crates/policy/src/session.rs; the age message uses the configured limit in seconds
- [x] T023 [US2] Implement `SessionPolicy::check_batch` and call it in `run_simple_query` before running a multi-statement string while Idle (refuse the whole string, nothing runs) in crates/server/src/lib.rs
- [x] T024 [US2] Handle `Admission::RollbackThenRefuse` and the row-limit `Err` from `complete` in crates/server/src/dsql.rs (ROLLBACK then error; abort block then error), and make all T019–T021 tests pass

**Checkpoint**: probes 02, 06, 07, 24, 25, 29 now refuse the same statements as the reference (SQLSTATE per research.md D1).

## Phase 5: User Story 3 - Statements DSQL refuses are refused (Priority: P2)

**Goal**: the deny rules of contracts/rule-catalogue.md §3, engine-accepted ones first.

**Independent Test**: `TRUNCATE t` → `0A000 unsupported statement: Truncate`, rows still present;
`CREATE TABLE t (c money)` → `0A000 datatype money not supported`.

- [ ] T025 [P] [US3] Write failing unit tests for type display names and column-type checks in crates/policy/src/types.rs (`int[]`→`integer[]`, `text[]`→`text[]`, `bit(8)`→`bit`, `varbit(8)`→`bit varying`, `serial`/`bigserial`/`smallserial` as written, all names in rule `unsupported_type`; supported types such as `numeric(10,2)`, `jsonb`, `uuid`, `timetz`, `interval`, `bytea`, `timestamptz` pass)
- [ ] T026 [P] [US3] Write failing unit tests for every rule in contracts/rule-catalogue.md §3.1 and §3.2 in crates/policy/src/rules.rs (one positive case per rule with exact SQLSTATE/message; negative cases from §3.3: `CREATE DOMAIN`, `CREATE INDEX`, `SELECT … FOR UPDATE`/`FOR KEY SHARE`, `SELECT ARRAY[1,2]`, `'10.0.0.1'::inet`, `CREATE FUNCTION … LANGUAGE sql`, partitioned table left to the engine; rule order: `create_procedure` before `create_function_language`, `materialized_view` before `create_table_as`)
- [ ] T027 [P] [US3] Write failing integration tests in crates/server/tests/smoke/dsql_rules.rs, table-driven over the §3.1 rows (P2) through a real client: SQLSTATE + exact message, no effect (TRUNCATE leaves rows; refused CREATE leaves no table; ALTER TABLE ADD COLUMN `money`/`int[]`/`serial` refused), refusal inside a transaction aborts it (`25P02` next), refusal outside does not, refusal in an aborted transaction yields `25P02`
- [ ] T028 [US3] Implement `types.rs` and the §3.1 rules (`truncate`, `unlogged_table`, `create_table_as`, `enum_type`, `alter_enum`, `drop_type`, `rename_type`, `copy`, `serial`, `unsupported_type`, `array_column`) as `RULES` entries in crates/policy/src/rules.rs, and set `StatementPlan.refusal` from `check_rules` in crates/policy/src/statement.rs
- [ ] T029 [P] [US3] Write failing integration tests in crates/server/tests/smoke/dsql_rules.rs, table-driven over the §3.2 rows (P3): SQLSTATE + exact message for each
- [ ] T030 [US3] Implement the §3.2 rules in crates/policy/src/rules.rs (`temporary_table`, `extensions`, `triggers`, `create_database`, `materialized_view`, `composite_type`, `range_type`, `tablespace`, `foreign_table`, `vacuum`, `listen`/`notify`/`unlisten`, `alter_system`, `merge`, `create_function_language`, `unsupported_locking`, `tablesample`, `lock_table`, `do`, `create_procedure`, `create_rule`, `create_publication`, `reindex`, `cluster`, `prepare_transaction`, `advisory_locks`)
- [ ] T031 [US3] Make the `copy` refusal precede `parse_copy_to_stdout` in `run_simple_statement` (crates/server/src/lib.rs) and confirm `COPY … TO STDOUT` and `COPY … FROM STDIN` both return `0A000 COPY is not supported` without entering COPY mode

**Checkpoint**: probes 01, 03, 08, 09, 20, 21, 29 refuse the same statements as the reference.

## Phase 6: User Story 4 - Only DSQL's session settings can be set (Priority: P2)

**Goal**: the whitelist, remembered values for SHOW, and DSQL's refusals (contracts/rule-catalogue.md §4).

**Independent Test**: `SET work_mem = '64MB'` → `0A000 setting configuration parameter "work_mem"
not supported`; `SET enable_seqscan = off; SHOW enable_seqscan` → `off`.

- [ ] T032 [P] [US4] Write failing unit tests in crates/policy/src/settings.rs for `SettingCommand::from_ast` and `SessionSettings`: every whitelisted name accepted; `SET`/`SET SESSION`/`SET LOCAL`/`TO DEFAULT`/`RESET`/`RESET ALL`; `SHOW` column names and defaults (`DateStyle` `ISO, MDY`, `IntervalStyle` `postgres`, `extra_float_digits` `1`, `enable_*` `on`, `dsql.enable_batched_nestloop` `on`); `SET LOCAL` overlay dropped at transaction end; non-whitelisted names via SET/RESET/`set_config('<const>', …)` → `set_not_supported`; `SET TRANSACTION …` → `set_transaction`; `SET SESSION CHARACTERISTICS … REPEATABLE READ[, READ WRITE]` → no-op, other levels → `Unsupported isolation level: <LEVEL>`, `READ ONLY` → `set_transaction`; unknown `dsql.*` → `42602` with DETAIL `"dsql" is a reserved prefix.`; `enable_seqscan = banana` → `22023 parameter "enable_seqscan" requires a Boolean value`; `SET dsql.enable_batched_nestloop = on, off` → `22023 SET dsql.enable_batched_nestloop takes only one argument`; `role` only with `SET LOCAL`; `search_path`/`application_name`/`client_encoding` classified as forwarded; `timezone` left to the existing intercept
- [ ] T033 [P] [US4] Write failing integration tests in crates/server/tests/smoke/dsql_settings.rs for every row of contracts/rule-catalogue.md §4 (SQLSTATE, message, DETAIL) and the accepted forms (SHOW after SET, RESET, SET LOCAL scope across COMMIT and ROLLBACK, `SET search_path` still changes name resolution, `SHOW lc_collate` → `42704`), simple and extended protocol
- [ ] T034 [US4] Implement crates/policy/src/settings.rs and connect it in `StatementPlan::new` (step 5) and `SessionPolicy::admit` (step 8); SET/RESET of remembered names answer `Reply::Command`, SHOW answers `Reply::Show`; forwarded names return `Run`
- [ ] T035 [US4] Remove `lc_collate` and `lc_ctype` from `SHOW_PARAMETERS` in crates/policy/src/server_params.rs so they fall through to the `show_unrecognized` refusal; update its unit tests
- [ ] T036 [US4] Detect `set_config('<constant name>', …)` calls anywhere in a statement (`FuncCall` with a constant first argument) in crates/policy/src/settings.rs and refuse non-whitelisted names before the engine runs the statement

**Checkpoint**: probes 10, 11 (SET lines) and 23 refuse/accept the same statements as the reference.

## Phase 7: User Story 5 - Bare `numeric` means `numeric(18,6)` (Priority: P3)

**Goal**: DSQL's meaning of bare `numeric` in DDL.

**Independent Test**: `CREATE TABLE t (id int PRIMARY KEY, n numeric); INSERT INTO t VALUES (1,
1.5); SELECT n FROM t` returns `1.500000`.

- [ ] T037 [P] [US5] Write failing unit tests for the `bare_numeric` rewrite in crates/policy/src/rewrite.rs: `CREATE TABLE … (n numeric, d decimal, e numeric(5), f numeric(10,2))` → only `n` and `d` get `(18,6)`; quoted identifiers and comments untouched; `ALTER TABLE … ADD COLUMN n numeric` rewritten; `SELECT 1.5::numeric` and `CAST(x AS numeric)` not rewritten; `engine_sql` differs only by the inserted text
- [ ] T038 [P] [US5] Write failing integration tests in crates/server/tests/smoke/dsql_settings.rs (numeric section): stored value of a bare `numeric` column reads back as `1.500000` (text) and `numeric(5)` / `numeric(10,2)` columns are unchanged; an error in a rewritten statement reports a position in the client's original text
- [ ] T039 [US5] Implement `bare_numeric` in `REWRITES` (locate the type-name token from `TypeName.location` and the scanner token stream; insert `(18,6)` after it) in crates/policy/src/rewrite.rs and apply edits in `StatementPlan::new` (step 7) in crates/policy/src/statement.rs

## Phase 8: Polish & Cross-Cutting Concerns

- [ ] T040 Update docs/known-gaps.md (FR-053): remove B13 and the "SET SESSION CHARACTERISTICS" entry if fully closed; narrow B9 to the remaining items (`current_user`, `current_database()` with `--data`, non-UTC `SET timezone`); narrow B10 to "mapped to 40001/OC000; still raised by the engine (M4)"; narrow "Errors"; add new rows: autocommit row limit enforced only for `INSERT … VALUES` (M4), FK cascade rows not counted (engine), multi-statement query strings checked but not atomic (M4), message texts without a live-DSQL record (`copy`, `lock_table`, `advisory_locks`, MiniStack-sourced rows); leave B1/B2/B3, B14 and DEALLOCATE unchanged
- [ ] T041 [P] Extend the "Known MiniStack deviations" table in tests/psql/README.md with the live-DSQL differences that keep probe message lines differing (research.md §4: deny-rule texts, `serial` 42704 vs 0A000, discipline 0A000 vs 25006, row limit 54000 vs 25006); do not touch any expected file
- [ ] T042 [P] Add an M2 entry to the "Changes from upstream" list in the header of crates/server/src/lib.rs (DSQL policy layer, OC000/OC001 shaping, 55P03 mapping) and update the module docs in crates/policy/src/lib.rs
- [ ] T043 Run the gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, and `tests/psql/run.sh` against micro-dsql (5 × PASS); compare the run time of the layer-1 scenarios and `cargo test -p micro-dsql-server` with T001's baseline (SC-004: ≤ 10% slower)
- [ ] T044 Run `RESULTS_DIR=reports/m2 tests/psql/run.sh --probes` against micro-dsql and check SC-003 line by line for probes 01, 02, 03, 06, 07, 08, 09, 10, 20, 21, 23, 24, 25, 29; record the per-probe result (outcome/SQLSTATE matches, remaining differences and their reason) in specs/005-m2-dsql-rules/research.md §6
- [ ] T045 [orchestrator] Run the MiniStack differential (`scripts/ministack-up.sh`, `scripts/compare.sh`) against micro-dsql and the reference; attach `reports/diff-<date>.md` and confirm the layer-1 scenarios are still green against MiniStack
- [ ] T046 [owner] Verify on a live DSQL cluster the texts this feature could not source from a live record: `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ` (accepted here), `COPY … FROM STDIN` / `TO STDOUT`, `LOCK TABLE`, `pg_advisory_lock`, `CREATE DOMAIN`, the `role` rules; update contracts/rule-catalogue.md in a follow-up if they differ
- [ ] T047 [owner] Decide whether probe message lines should follow live DSQL (this plan, research.md D1) or MiniStack; the catalogue is one table, so a switch is a one-file change in crates/policy/src/rules.rs

## Dependencies & Execution Order

- Phase 1 → Phase 2 (T002–T014, in order except the [P] test-writing tasks) → stories.
- US1 (Phase 3) depends only on Phase 2. **MVP = Phase 2 + US1.**
- US2 (Phase 4) depends on Phase 2 (`classify`, `SessionPolicy`, `run_planned`).
- US3 (Phase 5) depends on Phase 2 (`StatementPlan.refusal`); the abort-semantics tests in T027
  reuse the Phase 2 Refuse path, not US2.
- US4 (Phase 6) depends on Phase 2 (`Admission::Respond`).
- US5 (Phase 7) depends on T007 (`TextEdit`).
- US1–US5 touch different policy modules (`errors`, `session`, `rules`/`types`, `settings`,
  `rewrite`); their integration tests live in different files except US2/US3, which share
  crates/server/tests/smoke/dsql_rules.rs (serialise T020/T021 with T027/T029 edits).
- Phase 8 after all stories; T045–T047 can run any time after T044.

## Parallel Examples

- Phase 2: T002, T004, T006 (unit tests in three different files) together.
- US1: T015 (policy unit tests) with T016 (integration tests).
- US3: T025, T026 together; T027 after T021 if the same file is open.
- US4: T032 with T033. US5: T037 with T038.
- Across stories after Phase 2: US1, US4 and US5 can proceed in parallel (separate modules and test files).

## Implementation Strategy

1. Phase 1–2: no behaviour change; all gates green.
2. US1 (MVP): conflict texts; ship-able on its own.
3. US2: the highest-value "would break in prod" rules (P1).
4. US3 §3.1 (engine-accepted statements) before §3.2 (reshaping).
5. US4, then US5.
6. Phase 8: known gaps, docs, gates, probe review; orchestrator and owner tasks last.
