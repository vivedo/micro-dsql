---
description: "Task list for T1 testing phase"
---

# Tasks: T1 testing phase (psql scenarios, DSQL probes, differential run)

**Input**: Design documents from `specs/003-t1-testing/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: The feature *is* a test harness. Each scenario/probe is validated by running it against
the MiniStack reference (`DSQL_STRICT=1`); expected outputs are captured from the reference, never
from micro-dsql.

## Format: `[ID] [P?] [Story] Description`

## Phase 1: Setup

- [x] T001 Create directories `tests/psql/expected/`, `tests/psql/probes/expected/`, `scripts/`, `.github/workflows/`; confirm `reports/` is in `.gitignore`
- [x] T002 [P] Create empty skip list with header comment (format: `<name> <reason>`, `#` comments) in tests/psql/known-gaps.txt

## Phase 2: Foundational (reference target; blocks expected-output capture)

- [x] T003 Implement scripts/ministack-up.sh per contracts/scripts.md (docker run with `-p 4566:4566 -p 25432-25440:25432-25440`, Docker socket, `DSQL_STRICT=1`; health wait; `aws dsql create-cluster` via `aws` or `amazon/aws-cli` container in MiniStack's network namespace; wait for ACTIVE; print only `export PG*` lines on stdout)
- [x] T004 [P] Implement scripts/ministack-down.sh (remove MiniStack container and `ministack-dsql-*` backends; idempotent)
- [x] T005 Bring MiniStack up with scripts/ministack-up.sh and record measured facts (server_version, isolation default) in specs/003-t1-testing/research.md

## Phase 3: User Story 1 - Layer-1 scenarios and runner (Priority: P1) MVP

**Goal**: five deterministic scenarios plus a POSIX runner; green against the reference.

**Independent Test**: `eval "$(scripts/ministack-up.sh)"; tests/psql/run.sh` prints 5 × PASS and exits 0.

- [x] T006 [US1] Implement tests/psql/run.sh per contracts/run-sh.md (POSIX sh + psql only; `psql -X -q -f` from tests/psql; reachability check with `PGCONNECT_TIMEOUT`; normalise `LOCATION:` lines and "does not exist, skipping" notices; skip list; `--probes`/`--all`; `RESULTS_DIR`; summary; exit 0/1/2; documented normalisation in header comment)
- [x] T007 [P] [US1] Write tests/psql/00_connect.sql (`SELECT 1`, `current_user`, `current_database()`, `SHOW server_version` via `\gset` checked as `16.%`, `SHOW timezone`)
- [x] T008 [P] [US1] Write tests/psql/10_crud.sql (CREATE TABLE with PK, INSERT … RETURNING, SELECT WHERE/ORDER BY, UPDATE, DELETE, DROP TABLE; pre-drop with IF EXISTS)
- [x] T009 [P] [US1] Write tests/psql/20_txn.sql (`\set ON_ERROR_STOP 0`, `\set VERBOSITY sqlstate`; BEGIN/COMMIT; ROLLBACK discards; error in txn then 25P02 until ROLLBACK; `BEGIN ISOLATION LEVEL REPEATABLE READ` works)
- [x] T010 [P] [US1] Write tests/psql/30_errors.sql (`\set ON_ERROR_STOP 0`, `\set VERBOSITY verbose`; 23505, 42P01, 42601)
- [x] T011 [P] [US1] Write tests/psql/40_conflict.sh (two psql sessions over FIFOs, lock-free ordering from research.md R3, timeouts, prints deterministic summary incl. `40001 reported: yes` and the final value)
- [x] T012 [US1] Capture tests/psql/expected/{00_connect,10_crud,20_txn,30_errors,40_conflict}.out from MiniStack with a psql 16 client and review each for DSQL shape and absence of volatile data
- [x] T013 [US1] Validate: two consecutive runs green against MiniStack; negative checks (wrong expected → FAIL + exit 1; skip-list entry → SKIP; unreachable target → exit 2 quickly)

## Phase 4: User Story 2 - DSQL-rule probes (Priority: P2)

**Goal**: report-only probes whose expected outputs are the reference behaviour.

**Independent Test**: `tests/psql/run.sh --probes` against MiniStack → all MATCH, exit 0.

- [x] T014 [P] [US2] Write PLAN.md probes in tests/psql/probes/: 01_truncate.sql, 02_savepoint.sql, 03_temp_table.sql, 04_create_index_async.sql, 05_sys_jobs.sql, 06_ddl_dml_one_txn.sql, 07_two_ddl_one_txn.sql, 08_create_extension.sql, 09_serial_column.sql, 10_set_not_whitelisted.sql, 11_isolation_levels.sql
- [x] T015 [P] [US2] Write mined probes in tests/psql/probes/: 20_denylist.sql (TestDenylist), 21_column_types.sql (TestColumnTypes), 22_identity_sequences.sql (TestIdentityAndSequences), 23_dsql_settings.sql (TestDsqlSettings), 24_txn_discipline.sql (TestTxnDiscipline), 25_txn_row_limit.sql (TestTxnLimits), 26_locking_reads.sql (TestLockingClauses/TestLockingReads), 27_alter_table.sql (TestAlterTableSubset/TestForeignKeys), 28_index_forms.sql (TestIndexAsync/TestIndexLimits), 29_txn_abort.sql (TestTransactionAbortSemantics)
- [x] T016 [US2] Capture tests/psql/probes/expected/*.out from MiniStack; confirm two runs are identical (no job ids, OIDs, timestamps)
- [x] T017 [US2] Run probes against a plain postgres:16 container on port 55432 to confirm DIFFER is reported without affecting the exit status

## Phase 5: User Story 3 - Differential report (Priority: P2)

**Goal**: one command compares two targets and writes reports/diff-<date>.md.

**Independent Test**: compare with A down and B = MiniStack → report written, A marked unreachable.

- [x] T018 [US3] Implement scripts/compare.sh per contracts/scripts.md and contracts/report-format.md
- [x] T019 [US3] Validate compare.sh: (a) A unreachable, B MiniStack; (b) A postgres:16 on 55432, B MiniStack (exercises the diff sections)
- [x] T020 [P] [US3] Draft optional CI workflow .github/workflows/differential.yml (ubuntu-latest, postgresql-client, MiniStack up, compare.sh, upload reports/ artifact, `continue-on-error: true`; micro-dsql target wired as a placeholder for Reconcile)

## Phase 6: Polish

- [x] T021 [P] Document the harness (usage, normalisation, how to re-capture expected outputs, known-gaps format) in tests/psql/README.md
- [x] T022 Tear down all containers (scripts/ministack-down.sh; remove the postgres:16 fallback container)
- [x] T023 Check committed files for machine-specific paths/hostnames and attribution; commit on 003-t1-testing

## Phase 7: Reconcile (after M1) — DEFERRED, needs the micro-dsql binary

**Goal**: User Story 4 / FR-012..FR-014 / SC-006. Not executed in this wave.

- [ ] T024 [US4] Start `micro-dsql --listen 127.0.0.1:<port>` and run tests/psql/run.sh against it; for each failing scenario either fix micro-dsql (M1 follow-up) or record it in tests/psql/known-gaps.txt and KNOWN_GAPS.md (constitution Principle V); never edit expected outputs to match micro-dsql
- [ ] T025 [US4] Run the five scenarios through the M1 tokio-postgres harness (extended protocol), e.g. a Rust integration test that replays tests/psql/*.sql statements, in tests/smoke/ (owned by the Rust workspace)
- [ ] T026 [US4] Add layer-1 CI against micro-dsql (macOS + Linux) using the binary built by M1's workflow; make it blocking
- [ ] T027 [US4] Wire micro-dsql as target A in .github/workflows/differential.yml
- [ ] T028 [US4] Produce the first differential report with scripts/compare.sh (micro-dsql vs MiniStack) and list each probe mismatch as an M2/M3 backlog item (e.g. in a backlog doc or issues)
- [ ] T029 [US4] Add the 3-line quick start (start the binary, connect with psql, done) to README.md
- [ ] T030 [US4] Tick the T1 exit checklist in PLAN.md-derived docs once all of the above hold

## Dependencies & Execution Order

- Phase 1 → Phase 2 → US1 (T006–T013) → US2/US3 (can run in parallel) → Polish.
- T012/T016 need T005 (reference up). T019 needs T018 and T006.
- Phase 7 depends on M1 (binary + tokio-postgres harness) and on all earlier phases.

## Parallel Examples

- US1: T007, T008, T009, T010, T011 (different files) after T006 is designed.
- US2: T014 and T015 in parallel.
- US3: T020 in parallel with T018.

## Implementation Strategy

MVP = Phases 1–3 (layer 1 green against MiniStack). Then probes and the differential report, then
polish. Phase 7 is executed by the post-M1 reconcile step.

## Phase 8: Convergence

- [x] T031 Document the `--capture` option and the `CAPTURED` status of tests/psql/run.sh in specs/003-t1-testing/contracts/run-sh.md per plan: contracts (unrequested)
- [x] T032 Align the Result statuses in specs/003-t1-testing/data-model.md with run.sh (PASS/FAIL/SKIP/MATCH/DIFFER/CAPTURED; no ERROR) per plan: data model (partial)
- [x] T033 Update specs/003-t1-testing/contracts/report-format.md §4 to say expected-output diffs are shown only when the A/B diff does not already show them per FR-010 (partial)
