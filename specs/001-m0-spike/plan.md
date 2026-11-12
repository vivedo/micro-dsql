# Implementation Plan: M0 spike — does Turso's Postgres frontend hold up?

**Branch**: `001-m0-spike` | **Date**: 2026-11-12 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/001-m0-spike/spec.md`

## Summary

Build the upstream `tursopg` binary from a local Turso clone (read and build only), run it as a pgwire
server, and measure it with small, reproducible scripts under `research/spike/`: an app-style SQL script, one smoke
script per driver, and two-session OCC scenarios. Results go into `research/06-spike-findings.md` with a
verdict on every `[I]` claim of research/02 and research/04 and a go/no-go verdict against docs/plan.md M0.

## Technical Context

**Language/Version**: POSIX shell + psql; Python 3 (psycopg 3, asyncpg); Node.js (pg); Go (pgx v5). The
engine under test is Rust (Turso, built with the toolchain available locally).

**Primary Dependencies**: libpq `psql` (Homebrew keg), psycopg[binary] 3.x, asyncpg, node `pg`, `github.com/jackc/pgx/v5`.

**Storage**: Turso `:memory:` database inside `tursopg --server` (default).

**Testing**: each script prints one line per step (`OK` / `ERR <sqlstate> <message>`); `research/spike/run-all.sh`
runs everything and tees output to `research/spike/results/`.

**Target Platform**: macOS arm64 developer machine (scripts are portable to Linux).

**Project Type**: spike (scripts + findings document). No Rust crate in this repo yet.

**Performance Goals**: none; record build time and server start time only.

**Constraints**: connection settings only from `PG*` env vars; no machine-specific paths; client-side
timeouts so a hang is recorded instead of blocking the run.

**Scale/Scope**: 1 schema file, ~15 queries, 5 drivers, 4 OCC scenarios + isolation probe.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status |
|---|---|
| I. PoC scope | PASS. Spike only measures DB behaviour; no TLS, auth, control plane. Connections use `sslmode=disable`. |
| II. Own repo, pinned upstream | PASS. Turso clone is read/build only; the tested commit is recorded for M1's pin. No fork. |
| III. Backend-agnostic policy | N/A. No policy code in this feature. |
| IV. Gates | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`: **N/A, no Rust crate exists yet** in this repo. `tests/psql/run.sh` starts at T1. Feature gate instead: every spike script runs to completion against `tursopg` and its output is recorded. |
| V. Merge / known gaps | Failures found are the deliverable (recorded in research/06), not test gates. Merge is done by the orchestrator. |
| Repo constraints | `research/` read-only except the new `06-spike-findings.md`, which docs/plan.md M0.6 mandates. No machine paths, no tool attribution. |

Post-design re-check: PASS (no new violations).

## Project Structure

### Documentation (this feature)

```text
specs/001-m0-spike/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/script-interface.md
└── tasks.md
```

### Source Code (repository root)

```text
research/spike/
├── README.md               # how to run (env vars, prerequisites)
├── env.sh                  # default PG* values (overridable)
├── run-all.sh              # runs everything, writes research/spike/results/*.txt
├── app/
│   ├── schema.sql          # app-style schema + seed
│   ├── queries.sql         # ~15 representative queries
│   └── run.sh              # psql -X -e, ON_ERROR_STOP off, VERBOSITY verbose
├── drivers/
│   ├── psql.sh
│   ├── psycopg_smoke.py
│   ├── asyncpg_smoke.py
│   ├── requirements.txt
│   ├── node/{package.json,package-lock.json,pg_smoke.js}
│   └── go/{go.mod,go.sum,main.go}
├── occ/
│   └── occ.py              # two-session scenarios + autocommit storm + isolation probe
└── results/                # captured output of the recorded run (committed)
research/06-spike-findings.md
```

**Structure Decision**: a flat `research/spike/` directory, separate from the future `tests/` tree (docs/plan.md §3),
because these scripts target upstream `tursopg` and are evidence for M0, not the M1/T1 suites.

## Complexity Tracking

None.
