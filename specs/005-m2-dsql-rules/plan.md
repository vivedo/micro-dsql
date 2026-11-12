# Implementation Plan: M2 — DSQL error shape and the "would break in prod" rules

**Branch**: `005-m2-dsql-rules` | **Date**: 2026-11-12 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/005-m2-dsql-rules/spec.md`

## Summary

Add the DSQL policy layer that docs/plan.md §4 M2 describes, inside the existing backend-agnostic
`crates/policy` and wired into the vendored server's execution path:

1. **Error shaping** in one module (`policy::errors`): OC000/OC001 constructors; engine failures are
   classified by the server into `EngineFailure` and shaped to `40001` with DSQL's exact texts;
   `55P03` (engine busy) becomes `40001`/OC000 as an interim mapping.
2. **Per-session/per-transaction state** (`policy::session::SessionPolicy`): one-DDL rule, DDL/DML
   separation, savepoint refusal, 3,000-row limit from the engine's per-statement change count,
   300 s age limit, multi-statement query strings checked as one implicit transaction.
3. **Stateless deny rules** (`policy::rules::RULES`) on the pg_query AST, with texts from the live
   DSQL record where one exists (research.md D1), engine-accepted statements first.
4. **SET whitelist** (`policy::settings`): remembered values for SHOW, forwarded engine settings,
   DSQL's refusals.
5. **Rewrites** (`policy::rewrite`): bare `numeric` → `numeric(18,6)` by text edit, inside a
   pipeline with a pre-parse stage left empty for M3.

The server gets one adapter module (`crates/server/src/dsql.rs`) that runs every statement through
`admit → engine → complete`, for both protocols.

## Technical Context

**Language/Version**: Rust stable (edition 2021), workspace at the repository root

**Primary Dependencies**: `pg_query` 6.1 (AST, policy crate's only dependency), `pgwire` 0.36,
Turso crates pinned at `d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5` (server crate only), `tokio`,
`tracing`; tests: `tokio-postgres`

**Storage**: N/A (session state in memory)

**Testing**: `cargo test --workspace`: unit tests in `crates/policy` (no engine); integration tests
in `crates/server/tests/smoke/` (in-process server on port 0, `tokio-postgres`, simple and
extended protocol); `tests/psql/run.sh` layer 1 (gate) and probes (report-only)

**Target Platform**: macOS and Linux

**Project Type**: Rust workspace: library crates (`policy`, `server`) and a binary (`micro-dsql`)

**Performance Goals**: rule evaluation adds no noticeable latency (spec SC-004); a statement is
parsed by `pg_query` once per execution in the simple protocol (today's separate `intercept` and
`analyze` parses are merged into the plan) and once per Parse in the extended protocol

**Constraints**: policy crate depends only on `pg_query` (Principle III); no Turso patch; no
change to autocommit execution (M4); probe expected files are never re-captured; no
machine-specific paths in committed files; port 5432 not used by tests

**Scale/Scope**: ~50 rule ids (contracts/rule-catalogue.md), 7 new policy modules, 1 new server
module, 3 new integration test files

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle / gate | Status |
|---|---|
| I. PoC scope | PASS. DSQL rules are core M2 scope (docs/plan.md §4). No TLS, auth, control plane, multi-DB. |
| II. Own repo, pinned upstream | PASS. No Turso patch: every hook needed exists in our crates (engine error variant via `turso_pg::Error::core_error()`, change count via `Statement::n_change()`, status via `PgConnection::transaction_state()`, block abort via `abort_transaction_block_on_error()`). The vendored `lib.rs` header's changes list gets an M2 entry. |
| III. Backend-agnostic policy | PASS. `crates/policy` keeps `pg_query` as its only dependency; engine facts cross as `TxnStatus`, `rows_changed: u64`, `Instant`, `EngineFailure` (contracts/policy-api.md). All rules unit-tested without an engine. |
| IV. Test-first; gates `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `tests/psql/run.sh` vs micro-dsql | PLANNED. Every rule gets a failing policy unit test and a failing integration test before its implementation (tasks.md order). No existing test is weakened; layer-1 expected outputs stay unchanged (they use only DSQL-legal statements; `40_conflict` compares SQLSTATE only). |
| V. Known-gap policy | PLANNED. `docs/known-gaps.md`: B13, B9 (SET part), B10 (mapping), "SET SESSION CHARACTERISTICS", "Errors" narrowed or closed; new entries for the autocommit row limit, FK cascade rows, non-atomic multi-statement strings. Gaps that still fail a test after two rounds follow the skip-marker rule. |
| Repository constraints | PASS by design: `research/` untouched (new findings in this feature's research.md); third-party golden probes referenced by commit, not vendored; no tool attribution. |
| Scope questions | None. Out-of-scope known gaps (B1/B2/B3, B14, DEALLOCATE) are retagged "later" in this plan only (research.md D11); their docs entries are untouched. |

**Post-design re-check**: PASS. The design adds modules to existing crates, one server adapter
module and tests; no new dependency, crate or Turso change.

## Design

### Execution path (both protocols)

```text
simple Query ─ parse text ─ split ─┬─ n > 1 and Idle: SessionPolicy::check_batch(plans) → refuse whole string?
                                   └─ for each statement: dsql::run_planned
extended Parse ─ StatementPlan::new (cached in PgStatement) ─ Execute: dsql::run_planned

dsql::run_planned(conn, policy, plan):
   status = txn_status(conn)
   match policy.admit(plan, status, now):
     Run{sql}               → engine (prepare/bind/run on sql); rows = n_change()
                               ok  → policy.complete(plan, rows, txn_status(conn), now)
                                       Err(row_limit) → abort block, return error
                               err → policy.failed(txn_status(conn)); return engine error
     Respond(reply)         → SHOW row / command tag
     Refuse(e)              → if Active: abort block; debug log e.rule; return e
     RollbackThenRefuse(e)  → engine ROLLBACK; policy.failed(Idle); return e
finish_query: engine error → shape_engine_error(engine_failure(err), …); PolicyError → C/M/D fields
```

COPY: the `copy` rule refuses `COPY` before today's `parse_copy_to_stdout` path; that path is only
reached in an aborted block (engine answers `25P02`). The `Intercept::Error` variant and the
private `SqlError` type are replaced by `PolicyError`.

### Key decisions (details and alternatives in [research.md](research.md) §5)

- D1 live-DSQL texts first; probes measured on outcome + SQLSTATE.
- D4 transaction boundaries come from engine status before/after each statement.
- D5 row limit: explicit transactions after each statement; autocommit only the `INSERT … VALUES`
  pre-check (the rest is a known gap until M4).
- D6 limits configurable through `Server::bind_with(…, ServerOptions { limits })`.
- D7 multi-statement strings: static discipline check, not atomic.
- D8 numeric rewrite by text splice at the AST location (the seam for M3 rewrites).
- D9 M3-family rules deferred.
- D10 `search_path` / `application_name` / `client_encoding` forwarded; other whitelisted names
  remembered.

### Named seams for M3 (006) and M4 (007)

Normative list in [contracts/policy-api.md](contracts/policy-api.md) §6:

- `policy::rewrite::pre_parse` — M3 `ASYNC` strip before libpg_query.
- `policy::rewrite::{TextEdit, REWRITES}` — M3 identity desugaring, sequence `CACHE`, `NOT VALID`.
- `policy::rules::RULES` — M3 deferred rules (sync `CREATE INDEX`, CACHE, ADD CONSTRAINT, ALTER
  COLUMN TYPE).
- `policy::session::{Admission, Reply}` — M3 `job_id` rows and `sys.*` answers (`Reply::Rows`).
- `policy::session::{SessionPolicy, TxnState}`, `SessionPolicy::{admit, complete, failed}` — M4
  autocommit-as-transaction and the doomed-transaction flag.
- `policy::errors::{oc000, oc001, EngineFailure, shape_engine_error}` — M4 deferred OC000 at
  COMMIT; removal of the `LockBusy` mapping.
- `server::dsql::{engine_failure, txn_status, run_planned}` — the single engine-facing adapter.

### Known-gap milestone retags (plan only, research.md D11)

| Entry | docs/known-gaps.md says | This plan |
|---|---|---|
| B1 (rest), B2 (rest), B3 (rest) | M2 | later (not 005) |
| B14 | upstream / M2 error remap | later (not 005) |
| DEALLOCATE | M2 | later (not 005) |

## Project Structure

### Documentation (this feature)

```text
specs/005-m2-dsql-rules/
├── spec.md
├── plan.md                    # this file
├── research.md                # current behaviour, fact check, reference texts, decisions
├── data-model.md              # StatementPlan, SessionPolicy, TxnState, SessionSettings
├── quickstart.md              # gates, probes, spot checks
├── contracts/
│   ├── rule-catalogue.md      # every rule id: SQLSTATE, message, source
│   └── policy-api.md          # policy modules, pipeline, session API, server seams, M3/M4 seams
├── checklists/requirements.md
└── tasks.md                   # /speckit-tasks
```

### Source Code (repository root)

```text
crates/policy/src/
├── lib.rs                # re-exports; module docs updated
├── errors.rs             # + PolicyError, EngineFailure, oc000/oc001/…, shape_engine_error
├── statement.rs          # new: StatementPlan pipeline
├── classify.rs           # new: StatementClass, TxnControl
├── rules.rs              # new: RULES, check_rules
├── types.rs              # new: column type checks, display names
├── rewrite.rs            # new: pre_parse, TextEdit, REWRITES (bare_numeric)
├── settings.rs           # new: whitelist, SettingCommand, SessionSettings
├── session.rs            # new: SessionPolicy, TxnState, PolicyLimits, Admission, Reply
├── intercept.rs          # kept; Error variant replaced by PolicyError
├── server_params.rs      # lc_collate / lc_ctype removed from SHOW_PARAMETERS
└── params.rs             # unchanged

crates/server/src/
├── lib.rs                # handler holds Arc<Mutex<SessionPolicy>>; PgStatement holds Arc<StatementPlan>;
│                         #   run_simple_statement / do_query go through dsql::run_planned;
│                         #   finish_query uses shape_engine_error; Server::bind_with; header changes list
└── dsql.rs               # new: engine_failure, txn_status, run_planned

crates/server/tests/smoke/
├── main.rs               # + mod dsql_rules, dsql_conflicts, dsql_settings
├── common.rs             # + TestServer::start_with(ServerOptions)
├── dsql_rules.rs         # new: deny rules, discipline and limits (SQLSTATE + message)
├── dsql_conflicts.rs     # new: OC000 / OC001 / 55P03 mapping with two clients
└── dsql_settings.rs      # new: whitelist, SHOW, SET LOCAL, SESSION CHARACTERISTICS, numeric rewrite

docs/known-gaps.md        # updated (FR-053)
tests/psql/README.md      # "Known MiniStack deviations" extended with the live-DSQL text differences
```

**Structure Decision**: no new crate. The policy logic lives in `crates/policy` (Principle III);
the server adapter is a single new module so the engine-facing glue stays in one file next to the
vendored `lib.rs`.

## Complexity Tracking

No constitution violations to justify.
