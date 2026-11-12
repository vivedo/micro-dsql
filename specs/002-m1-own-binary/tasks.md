---
description: "Task list for M1 — own binary that starts and runs"
---

# Tasks: M1 — own binary that starts and runs

**Input**: Design documents from `specs/002-m1-own-binary/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/

**Tests**: Required (constitution IV, test-first). Integration tests in `crates/server/tests/smoke/`, unit tests
in `crates/policy`.

## Phase 1: Setup

- [X] T001 Create workspace `Cargo.toml` (members `crates/policy`, `crates/server`, `crates/micro-dsql`; `[workspace.dependencies]` with Turso git deps at rev `d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5`, pgwire 0.36, pg_query 6.1, tokio, clap, tracing, anyhow, tokio-postgres)
- [X] T002 [P] Add `crates/server/LICENSE-turso` with the upstream licence text
- [X] T003 [P] Create `crates/policy/Cargo.toml` and `crates/policy/src/lib.rs` (pg_query only)
- [X] T004 Vendor upstream `postgres/server/lib.rs` into `crates/server/src/lib.rs` with MIT header and upstream commit; `crates/server/Cargo.toml`
- [X] T005 Create `crates/micro-dsql/Cargo.toml` and `crates/micro-dsql/src/main.rs`; first `cargo build`; commit `Cargo.lock`

## Phase 2: Foundational

- [X] T006 Server API in `crates/server/src/lib.rs`: `Storage {Memory, File}`, `open_database`, `Server::bind(addr) -> local_addr`, `Server::serve(shutdown future)`
- [X] T007 Smoke-test harness `crates/server/tests/smoke/main.rs` + `crates/server/tests/smoke/common.rs` (start server on `127.0.0.1:0` in a background runtime, connect with tokio-postgres `NoTls`); `[[test]]` entry in `crates/server/Cargo.toml`

## Phase 3: User Story 1 — start and connect (P1)

**Independent test**: start on port 0, connect, CRUD + txn.

- [X] T008 [US1] Tests `crates/server/tests/smoke/crud.rs`: connect with any user/password; CRUD; BEGIN/COMMIT; ROLLBACK discards; concurrent connection not blocked by a long statement
- [X] T009 [US1] Own connection loop in `crates/server/src/conn.rs` (negotiate_tls(None) → SSL 'N'; process_message/process_error)
- [X] T010 [US1] Run engine calls via `spawn_blocking` in `crates/server/src/lib.rs` (simple query, describe, execute, COPY)
- [X] T011 [US1] CLI in `crates/micro-dsql/src/main.rs`: `--listen`, `--data|--memory`, `--log`; tracing to stderr; log bound address; SIGINT/SIGTERM graceful shutdown exit 0
- [X] T012 [US1] Test `crates/micro-dsql/tests/cli.rs`: spawn the built binary with `--listen 127.0.0.1:0`, parse the logged address, connect, SIGTERM → exit 0, startup < 1 s; `--data` persistence across restarts

## Phase 4: User Story 2 — looks like DSQL (P1)

- [X] T013 [US2] Tests `crates/server/tests/smoke/session.rs`: startup params (via `SHOW` and tokio-postgres `parameter()`), SHOW server_version/timezone (column `TimeZone`), current_database()=postgres, Terminate closes the socket, SET timezone UTC accepted
- [X] T014 [P] [US2] Policy unit tests + implementation `crates/policy/src/intercept.rs` and `crates/policy/src/server_params.rs`
- [X] T015 [US2] Startup handler `crates/server/src/startup.rs` (no auth, DSQL ParameterStatus)
- [X] T016 [US2] Intercept SHOW/SET in simple and extended paths in `crates/server/src/lib.rs`; 25P02 when aborted
- [X] T017 [US2] Memory DB opened as `postgres` on MemoryIO (`crates/server/src/lib.rs`)
- [X] T018 [US2] Close on Terminate in `crates/server/src/conn.rs` (B4)

## Phase 5: User Story 3 — Postgres errors (P1)

- [X] T019 [US3] Tests `crates/server/tests/smoke/errors.rs`: 23505, 42P01, 42601, 25P02 until ROLLBACK, 40001 two-session conflict; session survives
- [X] T020 [US3] Test `crates/server/tests/smoke/t1_layer1.rs`: replay statements of `tests/psql/{00_connect,10_crud,20_txn,30_errors}.sql` via `simple_query` and assert key results/SQLSTATEs

## Phase 6: User Story 4 — extended protocol (P2, stretch)

- [X] T021 [US4] Tests `crates/server/tests/smoke/extended.rs`: param count/types on prepare; binary int/text/bool/float/uuid/timestamptz/numeric params; `count(*)` as i64; smallint as i16; numeric round trip; FK 23503
- [X] T022 [P] [US4] `crates/policy/src/params.rs`: max `$n`, per-param hints (cast, insert column, comparison column, limit/offset) and result-column hints (`count` → int8) with unit tests
- [X] T023 [US4] B1 in `crates/server/src/types.rs`: resolve hints to OIDs, Describe reports them; store types for Bind
- [X] T024 [US4] B2 `crates/server/src/binary.rs`: decode binary params by OID
- [X] T025 [US4] B3: result OIDs (int2, numeric, count) and binary result encoding (uuid, timestamp(tz), date, numeric, int2, float4) in `crates/server/src/binary.rs`
- [X] T026 [US4] B5: enable foreign keys per session in `crates/server/src/lib.rs`

## Phase 7: User Story 5 — gates and CI (P2)

- [X] T027 [US5] `.github/workflows/ci.yml` (fmt, clippy, test on macos-latest/ubuntu-latest; libclang; start micro-dsql; tests/psql/run.sh)
- [X] T028 [US5] Run the gates locally; fix fmt/clippy

## Phase 8: Polish

- [X] T029 `docs/known-gaps.md` (B1–B14 not fixed, ignored tests, psql scenario results)
- [X] T030 [P] `README.md` usage + 3-line quick start
- [X] T031 Run `tests/psql/run.sh` against micro-dsql; record per-scenario results; fix cheap issues
- [X] T032 Update `.github/workflows/differential.yml` to build micro-dsql (Reconcile hook) — only if trivial

## Dependencies

Setup → Foundational → US1 → (US2, US3 in parallel) → US4 → US5 → Polish. T014 and T022 (policy)
can proceed in parallel with server work.

## Implementation strategy

MVP = Phases 1–5 (required scope). Phase 6 is timeboxed; anything left becomes `#[ignore =
"KNOWN-GAP: …"]` plus a `docs/known-gaps.md` entry after two implement/converge rounds.

## Phase 9: Convergence

- [X] T033 Add a CLI test in crates/micro-dsql/tests/cli.rs that SIGTERMs the binary while a client is connected and a slow statement is running, and asserts exit status 0 within a few seconds per spec Edge Cases "Shutdown while clients are connected" (missing)
- [X] T034 Add a CLI test in crates/micro-dsql/tests/cli.rs that `--data` under a non-existent directory exits non-zero with an error message per spec Edge Cases "--data in an unwritable location" (missing)
- [X] T035 Record in specs/002-m1-own-binary/research.md the work beyond FR-009/FR-012 that T1 needed: PostgreSQL-shaped error messages/positions (crates/policy/src/errors.rs) and the extra SHOW parameters (in_hot_standby, lc_collate, lc_ctype) per plan: policy decisions (unrequested)
