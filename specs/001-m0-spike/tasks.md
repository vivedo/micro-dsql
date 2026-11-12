# Tasks: M0 spike — does Turso's Postgres frontend hold up?

**Input**: Design documents from `specs/001-m0-spike/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/script-interface.md, quickstart.md

**Tests**: the spike scripts *are* the tests; no separate test tasks. Cargo gates are N/A (no Rust crate yet).

## Format: `[ID] [P?] [Story] Description`

## Phase 1: Setup

- [x] T001 Add `.venv/`, `node_modules/`, Go build output to `.gitignore`
- [x] T002 Create `research/spike/env.sh` (PG* defaults, overridable) and `research/spike/README.md` (prerequisites, how to run)
- [x] T003 [P] Create `research/spike/drivers/requirements.txt` (psycopg[binary], asyncpg) and install into `.venv`
- [x] T004 [P] Create `research/spike/drivers/node/package.json` with `pg`, run `npm install` (commit lockfile)
- [x] T005 [P] Create `research/spike/drivers/go/go.mod` with pgx v5, run `go mod tidy` (commit go.sum)

## Phase 2: Foundational (blocking)

- [x] T006 Build `tursopg` release in the local Turso clone; record commit, toolchain, duration, warnings
- [x] T007 Start `tursopg --server 127.0.0.1:5432`; verify `SELECT 1` over psql (HARD STOP if it fails)

## Phase 3: User Story 1 — Build and connect (P1)

- [x] T008 [US1] Create `research/spike/connect.sh`: SELECT 1, current_user, current_database(), SHOW server_version/timezone, arbitrary password

## Phase 4: User Story 2 — App SQL (P1)

- [x] T009 [P] [US2] Write `research/spike/app/schema.sql` (5 tables, PK/FK, all listed types, indexes, seed data)
- [x] T010 [P] [US2] Write `research/spike/app/queries.sql` (≥15 queries incl. joins, aggregates, ORDER BY/LIMIT, upsert, RETURNING, CTE, window, jsonb, timestamptz µs, uuid, numeric, int4 overflow probe)
- [x] T011 [US2] Write `research/spike/app/run.sh` (psql -X -e, ON_ERROR_STOP 0, VERBOSITY verbose)

## Phase 5: User Story 3 — Driver matrix (P1)

- [x] T012 [P] [US3] `research/spike/drivers/psql.sh`
- [x] T013 [P] [US3] `research/spike/drivers/psycopg_smoke.py`
- [x] T014 [P] [US3] `research/spike/drivers/asyncpg_smoke.py`
- [x] T015 [P] [US3] `research/spike/drivers/node/pg_smoke.js`
- [x] T016 [P] [US3] `research/spike/drivers/go/main.go`

## Phase 6: User Story 4 — OCC (P1)

- [x] T017 [US4] `research/spike/occ/occ.py`: same-row UPDATE, same-PK INSERT, write skew, autocommit storm, isolation-level probe, final-state checks, watchdog timeouts

## Phase 7: User Story 5 — Findings (P1)

- [x] T018 [US5] `research/spike/run-all.sh` running all of the above into `research/spike/results/*.txt`; run it against tursopg
- [x] T019 [US5] Extra probes for research [I] items (int4 overflow, binary params, startup/timing, DSQL-rejected statements accepted by Turso) recorded in `research/spike/results/`
- [x] T020 [US5] Write `research/06-spike-findings.md` (commit, toolchain, build, tables, [I] verdicts, go/no-go, blockers + effort)

## Phase 8: Polish

- [x] T021 Check no machine-specific paths in committed files; stop tursopg; commit

## Dependencies

T006 → T007 → all US phases. US2–US4 independent of each other. T018 needs T008–T017. T020 needs T018–T019.

## Implementation Strategy

Hard-stop check first (T007), then scripts in parallel, one recorded run, then the findings document.

## Phase 9: Convergence

- [x] T022 [US3] Conform `research/spike/app/run.sh`, `research/spike/drivers/psql.sh` and `research/spike/drivers/go/main.go` to `contracts/script-interface.md`: exit 1 when the server cannot be reached at all (today they exit 0) (partial: contract exit-code rule)
