---
description: "Task list for M3 — DDL that ORMs and migrations emit"
---

# Tasks: M3 — DDL that ORMs and migrations emit

**Input**: Design documents from `specs/006-m3-orm-ddl/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/rule-catalogue-m3.md,
contracts/policy-api-m3.md, contracts/tooling.md, quickstart.md

**Tests**: Required. Constitution Principle IV (test-first) and SC-004: every rule and rewrite gets
a failing policy unit test (no engine) and a failing integration test (SQLSTATE + exact message,
`tokio-postgres`, in-process server on port 0) before it is implemented.

**Markers**: `[orchestrator]` = needs a shared resource or coordination outside this worktree
(MiniStack/Docker, merge order with 007); `[owner]` = needs the owner (a decision, a manual
walkthrough, live DSQL). Everything else is for the implementer. Never use port 5432; manual runs
use a free port such as 15506.

**Conventions**: messages byte-exact from contracts/rule-catalogue-m3.md; never re-capture
`tests/psql/probes/expected/*` against micro-dsql; `research/` read-only; no machine-specific paths;
no edits to 007-owned behaviour (plan.md "Seams with 007"); new logic in new modules
(`identity.rs`, `ddl_rules.rs`, `jobs.rs`, `sys.rs`, `ddl.rs`, `constraints.rs`).

## Format: `[ID] [P?] [Story] Description`

## Phase 1: Setup

- [X] T001 Record the baseline on a free port: `cargo test --workspace`; `tests/psql/run.sh --all` with `RESULTS_DIR=reports/m3-baseline` (gitignored); confirm scenarios pass=5 and note which lines of probes 04, 05, 22, 27, 28 differ (compare with specs/006-m3-orm-ddl/research.md §1)
- [X] T002 [P] Add `turso_parser` to `[workspace.dependencies]` in Cargo.toml with the same `git`/`rev` as the other Turso crates (`d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5`) and to crates/server/Cargo.toml; `cargo build` passes (Principle II: one rev for every Turso crate)
- [X] T003 [P] Create empty test modules crates/server/tests/smoke/dsql_ddl.rs and crates/server/tests/smoke/dsql_jobs.rs and register them in crates/server/tests/smoke/main.rs

## Phase 2: Foundational (blocking prerequisites for every story)

**Purpose**: the extended rewrite seam, plan fields, `Reply::Rows`, the server dispatch and the
`sys` bootstrap, with no new behaviour visible yet. All existing tests stay green.

- [X] T004 [P] Write failing unit tests in crates/policy/src/rewrite.rs for `pre_parse` → `PreParsed { text, async_kind }`: `CREATE INDEX ASYNC i ON t (c)` and `CREATE UNIQUE INDEX ASYNC …` → `Index`, `ALTER TABLE ASYNC t VALIDATE CONSTRAINT c` → `AlterTable`, output has the same byte length with the keyword replaced by five spaces; unchanged (and `async_kind = None`, `Cow::Borrowed`) for `ASYNC` in comments, string literals, `"async"`, `CREATE INDEX async ON t (c)`, `SELECT 1`; `CREATE INDEX ASYNC public.i ON t (c)` still fails to parse after blanking
- [X] T005 Implement `PreParsed` and the blanking `pre_parse` with `pg_query::scan` in crates/policy/src/rewrite.rs (research.md D1)
- [X] T006 [P] Write failing unit tests for `TextEdit { at, remove, insert }` replacements in `apply_edits` (replace a span, insert-only edits keep 005 results, multibyte text, overlapping edits rejected) in crates/policy/src/rewrite.rs
- [X] T007 Add `remove: usize` to `TextEdit` and support replacement in `apply_edits`; update `bare_numeric` to `remove: 0` (crates/policy/src/rewrite.rs); all 005 rewrite tests stay green
- [X] T008 Add the plan fields `async_kind`, `prelude: Vec<String>`, `cleanup_on_failure: Vec<String>`, `job: Option<JobAction>`, `ddl: Option<DdlAction>` and the `JobAction`/`DdlAction`/`NotValidKind` types (contracts/policy-api-m3.md §2) in crates/policy/src/statement.rs and a new crates/policy/src/jobs.rs; `StatementPlan::new` calls `pre_parse` and parses `pre.text`; invariant test: a refused plan has empty `job`/`ddl`/`prelude`
- [X] T009 [P] Write failing unit tests for `jobs::encode_job_id(u128)` (26 chars, alphabet `a-z2-7`, RFC 4648 order, deterministic) and `jobs::is_job_id` (`^[a-z2-7]{26}$`) in crates/policy/src/jobs.rs, then implement them
- [X] T010 Add `ColumnType { Text, Bool, Int4 }`, `ReplyColumn`, `Reply::Rows { tag, columns, rows }` in crates/policy/src/session.rs and `StatementPlan::reply_columns()` (replacing callers of `show_column()`) in crates/policy/src/statement.rs; no change to `admit` order, `TxnState`, `complete` or `failed`
- [X] T011 Add `RuleContext { async_kind }` to `rules::check_rules` and create crates/policy/src/ddl_rules.rs (empty rule set) wired into `RULES` in crates/policy/src/rules.rs; export new modules from crates/policy/src/lib.rs
- [X] T012 In crates/server/src/lib.rs: run `pre_parse` over the whole simple-query string before the `turso_pg_parser::parse` syntax check and split the original text; use the pre-parsed text at extended Parse; Describe uses `plan.reply_columns()`; add an M3 entry to the vendored file's header changes list
- [X] T013 In crates/server/src/dsql.rs add the `Reply::Rows` arm to `respond` (typed header via a helper next to `show_fields` in crates/server/src/lib.rs) and the single dispatch branch in the `Admission::Run` arm (`plan.job` → `sys::run_job_statement`, `plan.ddl`/`plan.prelude` → `ddl::run_ddl_statement`, else unchanged); create stub modules crates/server/src/sys.rs and crates/server/src/ddl.rs that fall back to `run_engine`
- [X] T014 Write a failing integration test (crates/server/tests/smoke/dsql_jobs.rs) that `SELECT * FROM sys.jobs` on a fresh server returns zero rows and the nine columns in order (`job_id, status, details, job_type, class_id, object_id, object_name, start_time, update_time`), and that a `--data` file reopened keeps `sys.jobs` (micro-dsql CLI test in crates/micro-dsql/tests/cli.rs); implement `sys::bootstrap(&Database)` (`CREATE SCHEMA IF NOT EXISTS sys`, `CREATE TABLE IF NOT EXISTS sys.jobs (job_id text PRIMARY KEY, status text NOT NULL, details text, job_type text NOT NULL, class_id oid NOT NULL, object_id oid NOT NULL, object_name text, start_time timestamptz NOT NULL, update_time timestamptz)`) called from `open_database` in crates/server/src/lib.rs
- [X] T015 Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `tests/psql/run.sh` (layer 1) — all green

**Checkpoint**: seams in place; US1, US2, US3, US6 can proceed in parallel (different modules and
test sections); US4 needs US1–US3; US5 is independent.

## Phase 3: User Story 1 - Asynchronous index DDL works as on DSQL (Priority: P1) MVP

**Goal**: `CREATE [UNIQUE] INDEX ASYNC` builds synchronously and returns a `job_id`; `sys.jobs`,
`CALL sys.wait_for_job`, `sys.dsql_major_version()` answer like live DSQL; sync `CREATE INDEX` and
the other index rules refuse.

**Independent Test**: `CREATE INDEX ASYNC i ON t (v)` returns `job_id` (26 chars); `CALL
sys.wait_for_job('<id>')` → `succeeded = t`; `sys.jobs` row `completed | INDEX_BUILD | public.i`.

- [X] T016 [P] [US1] Write failing policy unit tests in crates/policy/src/ddl_rules.rs for `index_concurrently`, `index_sync`, `index_method` (`btree` and no USING pass; `hash`, `gin` refused), `index_key_columns` (8 pass, 9 refused `54011`) with the exact texts of contracts/rule-catalogue-m3.md §1
- [X] T017 [P] [US1] Write failing policy unit tests in crates/policy/src/jobs.rs for `JobAction` extraction: `IndexBuild` (named, unnamed, `IF NOT EXISTS`, unique, schema-qualified table), `WaitForJob` from `CALL sys.wait_for_job('<id>')` and `CALL sys.wait_for_job($1)`, `MajorVersion` from `SELECT sys.dsql_major_version()`, `SELECT sys.dsql_major_version() AS v`, `SELECT * FROM sys.dsql_major_version()`; and the sys rules `wait_for_job_select` (42809), `wait_for_job_subquery` (0A000), `wait_for_job_id` (22P02 for a constant), `sys_write` (42501 `permission denied for table jobs`), `sys_ddl` (42501 `permission denied for schema sys`)
- [X] T018 [P] [US1] Write failing integration tests in crates/server/tests/smoke/dsql_jobs.rs (simple and extended protocol): each acceptance scenario of spec US1 (1–8), the edge cases `CREATE INDEX ASYNC public.i …` → `42601 syntax error at or near "."`, `IF NOT EXISTS` on an existing index returns a `job_id`, an unknown table returns the engine error and records no job, `BEGIN; CREATE INDEX ASYNC …; COMMIT` as one query string returns the `job_id` row, `CREATE TABLE …; CREATE INDEX ASYNC …` as one string → `0A000 multiple ddl statements not supported in a transaction`, a rolled-back transaction leaves neither index nor job, two clients get distinct ids and see each other's committed jobs, Describe of `CREATE INDEX ASYNC` and `CALL sys.wait_for_job($1)` lists `job_id text` / `succeeded bool`
- [X] T019 [US1] Implement the index rules and sys rules in crates/policy/src/ddl_rules.rs and their constructors in crates/policy/src/errors.rs; register them in `RULES` (crates/policy/src/rules.rs) after 005's rules
- [X] T020 [US1] Implement `JobAction` extraction (`IndexBuild`, `WaitForJob`, `MajorVersion`) in crates/policy/src/jobs.rs and call it from `StatementPlan::new` step 8 (crates/policy/src/statement.rs); `admit` answers `MajorVersion` with `Reply::Rows { tag: "SELECT 1", columns: [<name> int4], rows: [["1"]] }` (crates/policy/src/session.rs)
- [X] T021 [US1] Implement `sys::run_job_statement` for `IndexBuild` in crates/server/src/sys.rs: run the engine statement, find the index oid and name in `pg_class` (newest index of the table when unnamed), purge finished jobs older than 30 minutes (cutoff computed in Rust, UTC literal without offset — research.md D6), insert the job row (`class_id 1259`, `object_name <schema>.<index>`, `start_time = update_time = now`), answer tag `CREATE INDEX` + one `job_id` row; id from `uuid::Uuid::new_v4().as_u128()` encoded by `jobs::encode_job_id`
- [X] T022 [US1] Implement the failed unique build in crates/server/src/sys.rs: engine `23505` on `CREATE UNIQUE INDEX ASYNC` → no index left, job `failed` with details naming the duplicate, statement succeeds with the `job_id` row; integration test for it in crates/server/tests/smoke/dsql_jobs.rs (spec US1 scenario 4)
- [X] T023 [US1] Implement `WaitForJob` in crates/server/src/sys.rs: `$n` arguments checked with `jobs::is_job_id` (22P02), lookup in `sys.jobs`, unknown id → `22023 job <id> not found`, answer tag `CALL` + one `succeeded` bool row
- [X] T024 [US1] Verify FR-008 purge with an integration test that inserts an old finished job through a server-side helper exposed for tests (or a `--data` fixture) and checks it disappears when a new job is recorded, in crates/server/tests/smoke/dsql_jobs.rs
- [X] T025 [US1] Run probes 04, 05, 28 against micro-dsql (`tests/psql/run.sh 04_create_index_async 05_sys_jobs 28_index_forms`, results under `reports/m3`); every remaining difference must be a documented MiniStack deviation (`SELECT sys.wait_for_job`, `dsql_major_version`, texts) — record them for T068

**Checkpoint**: US1 complete; DSQL migrations with indexes run.

## Phase 4: User Story 2 - Identity columns and cached sequences work as on DSQL (Priority: P1)

**Goal**: bigint identities with CACHE 1 / ≥ 65536 and `CREATE SEQUENCE … CACHE 65536` work; DSQL's
CACHE and type rules refuse.

**Independent Test**: `CREATE TABLE a (id bigint GENERATED BY DEFAULT AS IDENTITY (CACHE 65536)
PRIMARY KEY, v text)`; `INSERT INTO a (v) VALUES ('x'), ('y') RETURNING id` → `1`, `2`.

- [X] T026 [P] [US2] Write failing policy unit tests in crates/policy/src/ddl_rules.rs for `identity_type` (`integer`/`smallint` → `datatype integer not supported, identity column type must be bigint`), `identity_cache_missing`, `sequence_cache_missing`, `cache_value` (2, 100, 65535 refused; 1, 65536, 1000000 pass; also `ALTER SEQUENCE … CACHE 100`) — texts from contracts/rule-catalogue-m3.md §2
- [X] T027 [P] [US2] Write failing policy unit tests in crates/policy/src/identity.rs for `desugar`: Drizzle's form (`"id" bigint PRIMARY KEY GENERATED BY DEFAULT AS IDENTITY (sequence name "users_id_seq" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 65536)`) → prelude `CREATE SEQUENCE "public"."users_id_seq" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 1` (quoting as needed), engine text with the span replaced by `DEFAULT nextval('public.users_id_seq')`, cleanup `DROP SEQUENCE IF EXISTS …`; default name `<table>_<column>_seq`; `GENERATED ALWAYS` treated like `BY DEFAULT`; two identity columns; schema-qualified table; and `cache_one` rewrite of `CREATE SEQUENCE s CACHE 65536` → `CACHE 1` (crates/policy/src/rewrite.rs)
- [X] T028 [P] [US2] Write failing integration tests in crates/server/tests/smoke/dsql_ddl.rs for spec US2 scenarios 1–7 and the edge cases: identity CREATE TABLE inside `BEGIN … COMMIT` counts as one DDL (a second DDL is refused), a CREATE TABLE that fails in the engine after the prelude leaves no sequence (autocommit and transaction), `DROP TABLE` removes `<table>_<column>_seq` (`pg_sequences` count 0) and re-creating the table restarts numbering at 1
- [X] T029 [US2] Implement the identity/CACHE rules in crates/policy/src/ddl_rules.rs, constructors in crates/policy/src/errors.rs, `cache_one` in `REWRITES` (crates/policy/src/rewrite.rs) and `identity::desugar` in crates/policy/src/identity.rs, called from `StatementPlan::new` step 7; plus `DdlAction::DropTable` extraction in crates/policy/src/jobs.rs
- [X] T030 [US2] Implement `ddl::run_ddl_statement` in crates/server/src/ddl.rs: run `prelude` then `engine_sql` on the session connection; on failure while Idle run `cleanup_on_failure` (never open a transaction — plan.md D12); for `DropTable`, read the tables' `nextval('<schema>.<table>_<column>_seq')` defaults from `pg_attrdef` before the drop and drop those sequences after it
- [X] T031 [US2] Run probe 22 (`tests/psql/run.sh 22_identity_sequences`): outcome and SQLSTATE match the reference; message differences are the live-DSQL texts (record for T068)

**Checkpoint**: US2 complete; DSQL schemas with generated keys run.

## Phase 5: User Story 3 - Constraints on existing tables follow DSQL's NOT VALID / VALIDATE flow (Priority: P1)

**Goal**: `ADD CONSTRAINT … FOREIGN KEY/CHECK … NOT VALID` enforced for new rows; `ALTER TABLE ASYNC
… VALIDATE CONSTRAINT` returns a job that completes or fails; other ADD CONSTRAINT forms refused.

**Independent Test**: after `ADD CONSTRAINT fk FOREIGN KEY (parent) REFERENCES p (id) NOT VALID`,
an orphan insert fails `23503`; VALIDATE's job fails while an old orphan exists.

- [ ] T032 [US3] Spike (decision gate, research.md D7): in a throwaway test in crates/server/src/constraints.rs, on a PG-dialect connection at the pinned rev, build `ast::AlterTableBody::AlterColumn` for an existing column with its full definition plus `REFERENCES p(id) ON DELETE CASCADE` (and separately `CHECK (a > 0)`), prepare it with `PgConnection::inner().prepare_translated_stmt`, and check: (a) it prepares and runs inside and outside a transaction, (b) new orphan/violating rows fail with `23503`/`23514` naming the constraint, (c) existing rows are not checked, (d) data, indexes, PK, defaults and other constraints survive, (e) `pg_constraint` lists the constraint (record `convalidated` and whether `pg_get_constraintdef`/`conkey`/`confkey` are available for D8). Write the result into specs/006-m3-orm-ddl/research.md as "D7 spike result". If any of (a)–(d) fails: stop US3 and raise the blocking question in plan.md (Turso patch + patch-crate, or refuse)
- [ ] T033 [owner] Decide the D7 fallback if T032 fails: first Turso patch (translator `AtAddConstraint`; patch-crate git-workspace constraint must be solved or the constitution amended) or refusing `ADD CONSTRAINT … NOT VALID` (Drizzle exit not met). Skip if T032 succeeds
- [ ] T034 [P] [US3] Write failing policy unit tests in crates/policy/src/ddl_rules.rs and crates/policy/src/jobs.rs: `add_constraint` (FK/CHECK without NOT VALID, PRIMARY KEY, UNIQUE, EXCLUDE → `0A000 unsupported ALTER TABLE ADD CONSTRAINT statement`), `add_constraint_form` (multi-column FK, `DEFERRABLE`, `MATCH FULL`, multi-column CHECK → `0A000 unsupported NOT VALID constraint form: <form>`), `validate_sync`; `DdlAction::AddConstraintNotValid` extraction (FK column, referenced schema/table/column, ON DELETE/ON UPDATE actions incl. Drizzle's `ON DELETE no action ON UPDATE no action`; CHECK column and expression text sliced from the original) and `JobAction::Validate`
- [ ] T035 [P] [US3] Write failing integration tests in crates/server/tests/smoke/dsql_ddl.rs for spec US3 scenarios 1–6: FK and CHECK NOT VALID accepted with existing violating rows; new violating rows fail `23503`/`23514` with PostgreSQL's message; ON DELETE CASCADE works afterwards; VALIDATE job `completed`/`failed` with `CALL sys.wait_for_job` `t`/`f` and `sys.jobs` `job_type = VALIDATE_CONSTRAINT`, `object_name = public.<table>`; unknown constraint → `42704 constraint "<c>" of relation "<t>" does not exist`; inside `BEGIN … COMMIT` the ALTER counts as one DDL
- [ ] T036 [US3] Implement the ADD CONSTRAINT rules in crates/policy/src/ddl_rules.rs with constructors in crates/policy/src/errors.rs; `AddConstraintNotValid`/`Validate` extraction in crates/policy/src/jobs.rs (plans with these actions send no engine text)
- [ ] T037 [US3] Implement `constraints::add_not_valid` in crates/server/src/constraints.rs per the T032 result (column definition read from the engine schema, constraint name kept) and dispatch it from crates/server/src/ddl.rs
- [ ] T038 [US3] Implement `constraints::validate` in crates/server/src/constraints.rs (read the constraint from the catalog per T032; FK: child rows with non-null keys and no parent; CHECK: rows where the expression is false; PostgreSQL-style detail text for a failed job) and the `Validate` branch of `sys::run_job_statement` in crates/server/src/sys.rs (tag `ALTER TABLE` + `job_id`)
- [ ] T039 [US3] If `pg_constraint.convalidated` cannot express NOT VALID (T032 e), record it in docs/known-gaps.md; otherwise add an integration assertion `convalidated = f` before and `t` after a successful VALIDATE in crates/server/tests/smoke/dsql_ddl.rs (FR-022)

**Checkpoint**: US3 complete; Drizzle's foreign keys in DSQL form run.

## Phase 6: User Story 6 - ALTER TABLE forms DSQL refuses are refused (Priority: P2)

**Goal**: ADD COLUMN with constraint, ALTER COLUMN TYPE, SET NOT NULL, DROP PK column refused with
DSQL's texts. (Placed before US4 because Drizzle's later migrations must hit these refusals.)

**Independent Test**: `ALTER TABLE t ADD COLUMN d int DEFAULT 0` → `0A000 ALTER TABLE ADD COLUMN
with constraint not supported`; the column does not exist.

- [ ] T040 [P] [US6] Write failing policy unit tests in crates/policy/src/ddl_rules.rs for `add_column_constraint` (DEFAULT, NOT NULL, NULL, CHECK, UNIQUE, PRIMARY KEY, REFERENCES, GENERATED, identity; plain `ADD COLUMN [IF NOT EXISTS] c text` and bare `numeric` still pass with the 005 rewrite), `alter_column_type`, `set_not_null`; and `DdlAction::DropColumn` extraction in crates/policy/src/jobs.rs
- [ ] T041 [P] [US6] Write failing integration tests in crates/server/tests/smoke/dsql_ddl.rs for spec US6 scenarios 1–5, incl. `DROP COLUMN id` on a PK column → `0A000 cannot drop primary key column id` and `DROP COLUMN` of a non-key column still working
- [ ] T042 [US6] Implement the three AST rules in crates/policy/src/ddl_rules.rs (constructors in crates/policy/src/errors.rs) and `constraints::check_drop_column` (PK columns from the engine catalog, error `errors::drop_pk_column(name)`) called from crates/server/src/ddl.rs before the engine runs
- [ ] T043 [US6] Run probe 27 (`tests/psql/run.sh 27_alter_table`): outcomes and SQLSTATEs match the reference except documented deviations (`SELECT sys.wait_for_job`, live-DSQL texts) — record for T068

**Checkpoint**: all DDL behaviour in place.

## Phase 7: User Story 4 - A DSQL-shaped Drizzle project migrates and runs (Priority: P1, exit criterion)

**Goal**: SC-001.

**Independent Test**: `scripts/drizzle-check.sh` against a fresh micro-dsql exits `0`.

- [ ] T044 [P] [US4] Create tests/orm/drizzle/package.json (private, `"type": "module"`, exact versions `drizzle-orm 0.45.4`, `drizzle-kit 0.31.11`, `pg 8.23.1`, engines `node >= 22`) and generate tests/orm/drizzle/package-lock.json with `npm install`; confirm `node_modules/` is ignored by .gitignore
- [ ] T045 [P] [US4] Create tests/orm/drizzle/src/schema.ts (users: bigint identity `generatedByDefaultAsIdentity({ cache: 65536 })`, varchar email + unique index, text, boolean default, timestamptz default now; posts: uuid `defaultRandom()`, bigint FK to users, text, numeric(10,2), jsonb, index; counters: bigint `generatedAlwaysAsIdentity({ cache: 1 })`, integer default) and tests/orm/drizzle/drizzle.config.ts reading the `PG*` variables
- [ ] T046 [US4] Run `npx drizzle-kit generate --name init` in tests/orm/drizzle, then adapt tests/orm/drizzle/drizzle/0000_init.sql by hand for DSQL (FK `… NOT VALID` followed by `ALTER TABLE ASYNC … VALIDATE CONSTRAINT …`; `CREATE [UNIQUE] INDEX ASYNC`; keep `USING btree`); keep tests/orm/drizzle/drizzle/meta/ consistent; document each edit in tests/orm/drizzle/README.md (spec Clarifications)
- [ ] T047 [US4] Add a second migration tests/orm/drizzle/drizzle/0001_*.sql (e.g. a new nullable column and an `ASYNC` index) so re-run idempotence and multi-migration ordering are exercised
- [ ] T048 [US4] Write tests/orm/drizzle/migrate-dsql.mjs: read migrations with drizzle-orm's `readMigrationFiles`; create `drizzle` schema and journal `drizzle.__drizzle_migrations_dsql (id bigint GENERATED BY DEFAULT AS IDENTITY (CACHE 65536) PRIMARY KEY, hash text NOT NULL, created_at bigint)`; apply each statement of each unapplied migration in its own transaction (DDL alone; the journal INSERT in its own transaction); after a statement that returns a `job_id`, `CALL sys.wait_for_job` and fail unless `succeeded`; print one line per statement; exit non-zero on the first error
- [ ] T049 [US4] Write tests/orm/drizzle/smoke.mjs with drizzle-orm (node-postgres): insert-returning (identity ids 1, 2), select with join and where, update, delete, `db.transaction` commit and rollback, orphan insert → `23503`; assert every result
- [ ] T050 [US4] Write tests/orm/drizzle/stock.mjs: run drizzle-orm's stock `migrate()` on the same folder against a database without its journal and assert the error code `42704` and message `type "serial" does not exist`
- [ ] T051 [US4] Write scripts/drizzle-check.sh per contracts/tooling.md (POSIX sh, `PG*` variables, `npm ci`, runs migrate-dsql twice — second run must apply nothing — then smoke, then stock; exit 0/1/2; no absolute paths)
- [ ] T052 [US4] Run scripts/drizzle-check.sh against a fresh micro-dsql on a free port until it exits 0; record the transcript summary in specs/006-m3-orm-ddl/research.md ("Drizzle exit run")

**Checkpoint**: Drizzle exit criterion met.

## Phase 8: User Story 5 - Conformance against live-DSQL recordings is measured (Priority: P2)

**Goal**: SC-002, a repeatable local measurement.

**Independent Test**: `scripts/conformance.sh` against a fresh micro-dsql exits `0` and writes
`reports/conformance-<date>.md`.

- [ ] T053 [P] [US5] Create scripts/conformance/UPSTREAM containing `38a33bf283d7670b30dc959e8f38d0cf4eef1ddc` and scripts/conformance/compare.go (package `main`; arguments: golden dir, recorded dir, report path, threshold; uses `conformance.LoadDir`, `CompareSuites`, `Failures`; writes the report of contracts/tooling.md; prints `TOTAL p/t (x%)` last; exit 1 below threshold)
- [ ] T054 [US5] Write scripts/conformance.sh per contracts/tooling.md: check `git`, `go` (≥ 1.27.1, `GOTOOLCHAIN=local`) and target reachability (exit 2 otherwise); clone or reuse `target/conformance/dsql-emulator` at the pinned SHA (verify `git rev-parse HEAD`); copy compare.go to `cmd/micro-dsql-compare/main.go` in the checkout; `go run ./cmd/dsql-baseline --host "$PGHOST" --port "$PGPORT" --sslmode disable --token "${PGPASSWORD:-x}" --label micro-dsql --out-dir <tmp>`; run the comparer against `test/conformance/golden`; write `reports/conformance-<UTC date>.md`; honour `CONFORMANCE_MIN` (default 60) and `CONFORMANCE_DIR`
- [ ] T055 [US5] Ensure `target/` and `reports/` stay gitignored and nothing from the upstream tree is committed (`git status` clean after a run)
- [ ] T056 [US5] Run scripts/conformance.sh twice against fresh servers before the M3 code (baseline: expect 157/242) and after it (target ≥ 185/242, every case in research.md §3 "M3 scope" passing, `create_index_async_qualified` still passing); totals identical across the two runs; record both totals in specs/006-m3-orm-ddl/research.md

**Checkpoint**: both exit criteria measured.

## Phase 9: Polish & cross-cutting concerns

- [ ] T057 [P] Update docs/known-gaps.md (Principle V, FR-060): narrow B12 to what remains (`information_schema`, `FOR UPDATE`, `DELETE … USING`, `ALTER COLUMN SET/DROP DEFAULT`, `DROP NOT NULL`, identity ALTER forms, `ADD UNIQUE USING INDEX`, `SET STORAGE`); add entries for: `GENERATED ALWAYS` accepts explicit values; jobs are synchronous (no `submitted`/`processing`, no DROP jobs); failed unique build leaves no INVALID index; `sys.jobs` is a table visible in catalogs and its `oid` columns are described as `integer`; `pg_sequences.cache_size` is 1; NOT VALID forms refused (multi-column, deferrable, MATCH FULL); `convalidated` if T039 found it inexpressible; the `main.` schema name in `42P01` texts; `wait_for_job` unknown-id text and `VALIDATE_CONSTRAINT` job type unverified live
- [ ] T058 [P] Update tests/psql/README.md "Known MiniStack deviations": `04`, `05`, `27`, `28` — MiniStack `SELECT sys.wait_for_job(…)` returns `t`, live DSQL refuses it (`42809`) and uses `CALL` (`succeeded`); MiniStack texts for identity/CACHE/index/ALTER rules vs live-DSQL texts (micro-dsql follows live DSQL); do not touch expected files
- [ ] T059 [P] Update README.md: the DSQL rules section gains the M3 DDL rules, ASYNC jobs and `sys.*`, and how to run scripts/drizzle-check.sh and scripts/conformance.sh (prerequisites Node ≥ 22, Go ≥ 1.27.1)
- [ ] T060 [P] Update specs/006-m3-orm-ddl/contracts/rule-catalogue-m3.md if any text changed during implementation, and mark each rule's integration test name next to it
- [ ] T061 Unit-test sweep: every rule id in contracts/rule-catalogue-m3.md has a policy unit test and an integration test; every rewrite has unit tests that run without an engine (`cargo test -p micro-dsql-policy`)
- [ ] T062 Check the policy crate still depends only on `pg_query` (`cargo tree -p micro-dsql-policy --depth 1`) and that no `turso_*` type appears in crates/policy (Principle III)
- [ ] T063 Grep committed changes for machine-specific paths, user names and tool attribution (`git diff main --stat` plus a grep for absolute home/mount paths); fix any
- [ ] T064 Full gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `tests/psql/run.sh` (layer 1, scenarios pass=5) against a fresh server on a free port
- [ ] T065 Probes report: `tests/psql/run.sh --probes` with `RESULTS_DIR=reports/m3`; SC-003 holds for 04, 22, 27, 28; no previously matching probe line regresses relative to T001
- [ ] T066 Re-run scripts/drizzle-check.sh and scripts/conformance.sh after the final gates (SC-001, SC-002) and record the totals in specs/006-m3-orm-ddl/research.md
- [ ] T067 [orchestrator] Differential run against MiniStack (`scripts/compare.sh`, Docker; exclusive resource) for probes 04, 05, 22, 27, 28 and the new behaviours; attach the report to the merge notes
- [ ] T068 [orchestrator] Coordinate merge order with feature 007 (shared files listed in plan.md "Seams with 007": `session.rs`, `statement.rs`, `rules.rs`, `errors.rs`, `dsql.rs`, `lib.rs`, docs); resolve conflicts keeping 007's autocommit/transaction changes and 006's dispatch branch and `Reply::Rows`; consolidate the probe-deviation notes from T025, T031, T043
- [ ] T069 [owner] Manual walkthrough of quickstart.md §5 against a running micro-dsql (psql), and of the Drizzle example README
- [ ] T070 [owner] Verify on a live DSQL cluster (or a newer golden record): the `VALIDATE_CONSTRAINT` job type, the text for an unknown well-formed job id, whether `USING btree` is accepted, the `CACHE must be 1 or at least 65536` text, `SET NOT NULL` text; update the catalogue sources

## Dependencies & execution order

- Phase 1 → Phase 2 (T004–T015) → stories.
- US1 (T016–T025), US2 (T026–T031), US3 (T032–T039), US6 (T040–T043) depend only on Phase 2; they
  touch different rule groups and test sections and can run in parallel. Within US3, T032 gates
  T036–T039 (and T033 only if T032 fails).
- US4 (T044–T052) needs US1, US2, US3 (indexes, identities, FKs in the migration) and US6 (later
  migrations must not use refused forms). T044/T045 can start any time.
- US5 (T053–T055) is independent of the Rust work; T056's "after" run needs US1–US3, US6.
- Polish (T057–T070) last; T067–T070 are outside the implementer's reach.

## Parallel examples

```text
# Phase 2: T004 + T006 + T009 (different functions/files) in parallel
# US1: T016 (ddl_rules tests) + T017 (jobs tests) + T018 (integration tests) in parallel, then T019–T023
# US2: T026 + T027 + T028 in parallel, then T029 → T030
# Across stories after Phase 2: US1, US2, US6 implementer streams in parallel; US3 starts with the T032 spike
# US4: T044 + T045 while US1–US3 are in progress; US5: T053 + T054 any time
```

## Implementation strategy

1. MVP = Phase 2 + US1: ASYNC indexes and `sys.*` (the most common DSQL DDL and the 13 index-group
   golden cases).
2. Then US2 and US6 (rules and rewrites; low risk), and US3 starting with the T032 spike — the only
   item that can force a Turso patch, so it is started early to surface the owner question.
3. Then US4 (Drizzle exit) and US5 (conformance measurement), then Polish.
4. Known-gap rule (Principle V): if a gate still fails after two implement/converge rounds, mark the
   failing test `#[ignore = "KNOWN-GAP: …"]` and record it in docs/known-gaps.md.
