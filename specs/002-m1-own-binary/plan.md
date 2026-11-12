# Implementation Plan: M1 — own binary that starts and runs

**Branch**: `002-m1-own-binary` | **Date**: 2026-11-12 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/002-m1-own-binary/spec.md`

## Summary

Create the Cargo workspace from docs/plan.md §3 and a `micro-dsql` binary that serves the pinned Turso
Postgres frontend over a vendored, adapted copy of Turso's wire server. The adaptations are: our own
connection loop (Terminate closes, SSL refused), a no-auth startup handler with DSQL-shaped
server parameters, statement execution on the blocking thread pool, `SHOW`/`SET timezone`
interception driven by the backend-agnostic `policy` crate (pg_query AST), an in-memory database
named `postgres`, foreign keys enabled per session, and best-effort fixes for parameter
description (B1), binary parameters (B2) and result type OIDs/binary encodings (B3).
Integration tests start the server in-process on port 0 and talk to it with `tokio-postgres`.

## Technical Context

**Language/Version**: Rust stable (1.99 locally; CI uses the runner's stable), edition 2021

**Primary Dependencies**: `turso_core`, `turso_pg`, `turso_pg_parser` (git, rev `d5fdf4d…`),
`pgwire` 0.36, `pg_query` 6.1, `tokio`, `clap` 4, `tracing` + `tracing-subscriber`, `anyhow`;
dev: `tokio-postgres` 0.7

**Storage**: Turso MVCC database; in-memory by default, a file with `--data`

**Testing**: `cargo test` (unit tests in `policy` and `server`, integration tests in
`crates/server/tests/smoke/` wired as a test target of the server crate), plus `tests/psql/run.sh`

**Target Platform**: macOS and Linux (x86_64/arm64)

**Project Type**: CLI server (single binary) + two library crates

**Performance Goals**: ready to accept connections in < 1 s (M0: tursopg 8–9 ms)

**Constraints**: no TLS, no auth, one database; Turso clone is read-only reference; no
machine-specific paths in committed files

**Scale/Scope**: local/CI use, tens of concurrent connections

## Constitution Check

| Principle | Check | Status |
|---|---|---|
| I. Scope | One database, no TLS (SSL refused), any user/password, no control plane | PASS |
| II. Pinned upstream | All Turso crates git-pinned to one rev; server file vendored with MIT notice + upstream commit; no fork/patch | PASS |
| III. Backend-agnostic policy | `crates/policy` depends only on `pg_query`; it classifies SHOW/SET and counts/infers parameters from the AST; no Turso types | PASS |
| IV. Gates | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `tests/psql/run.sh` vs micro-dsql (informational for M1; T1 reconcile owns layer-1 fixes) | PASS (enforced at implement) |
| V. Known gaps | Failing tests after two rounds stay with `#[ignore = "KNOWN-GAP: …"]` and in `docs/known-gaps.md` | PASS |
| Repo constraints | `research/` untouched; no tool attribution; conventional commits; branch `002-m1-own-binary` | PASS |

Post-design re-check: PASS (no new violations; see Complexity Tracking for the deviations).

## Project Structure

### Documentation (this feature)

```text
specs/002-m1-own-binary/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── cli.md
│   └── wire.md
└── tasks.md
```

### Source Code (repository root)

```text
Cargo.toml                 # workspace, [workspace.dependencies], pinned Turso rev
Cargo.lock                 # committed
crates/server/LICENSE-turso     # upstream licence text
docs/known-gaps.md
README.md
crates/
├── policy/                # pg_query-only: SHOW/SET intercepts, server parameters, $n counting and type hints
└── server/                # vendored + adapted turso_pg_server (MIT header, upstream commit)
    └── Cargo.toml         # tests/smoke/main.rs (auto-discovered)
crates/micro-dsql/            # main.rs: clap, tracing, signals
crates/server/tests/smoke/               # tokio-postgres integration tests (server started in-process, port 0)
.github/workflows/ci.yml
```

**Structure Decision**: docs/plan.md §3 layout. Integration tests live in `crates/server/tests/smoke/` (PLAN.md path)
and are compiled as a test target of the `micro-dsql-server` crate via `[[test]] path`, so they can
start the server in-process. `crates/sys` is skipped (M3).

## Complexity Tracking

| Deviation | Why needed | Simpler alternative rejected because |
|---|---|---|
| Own connection loop instead of `pgwire::tokio::process_socket` | pgwire ignores Terminate (B4) | Wrapping the socket to sniff bytes is more fragile than ~40 lines reusing pgwire's public `negotiate_tls`/`process_message`/`process_error` |
| Upstream `#[cfg(test)]` module not vendored | it depends on Turso's internal `turso_pg_client` test crate | Our own tokio-postgres tests cover the same behaviour |
