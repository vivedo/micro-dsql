# Implementation Plan: T1 testing phase (psql scenarios, DSQL probes, differential run)

**Branch**: `003-t1-testing` | **Date**: 2026-11-12 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/003-t1-testing/spec.md`

## Summary

Build the target-agnostic T1 harness from PLAN.md §4: five plain-psql scenarios with expected
outputs and a POSIX-sh runner (blocking gate), a set of report-only DSQL-rule probes with outputs
captured from the MiniStack DSQL emulator, scripts to bring MiniStack up/down and to diff two targets
into `reports/diff-<date>.md`, and an optional CI workflow. All expected outputs are recorded against
MiniStack (`DSQL_STRICT=1`). Work that needs the micro-dsql binary is a deferred "Reconcile (after
M1)" phase.

## Technical Context

**Language/Version**: POSIX `sh` (scripts), SQL (psql scripts), GitHub Actions YAML

**Primary Dependencies**: `psql` (libpq client; expected outputs recorded with psql 16), Docker,
AWS CLI v2 (or its official container image), `ministackorg/ministack` image

**Storage**: N/A (targets hold their own data; reports written to gitignored `reports/`)

**Testing**: the harness is the test; validated by running it against MiniStack and by negative
checks (unreachable target, deliberately wrong expected output, skip list)

**Target Platform**: macOS and Linux developer machines; Linux GitHub runners

**Project Type**: test harness (scripts + data files) inside the micro-dsql repository

**Performance Goals**: full layer-1 run < 30 s against a local target; reference bring-up < 3 min
with images pulled

**Constraints**: only libpq env vars to select a target; no hard-coded tool paths; port 5432 is
not used; no machine-specific paths in committed files; no Rust/Cargo changes (M1 owns them)

**Scale/Scope**: 5 scenarios, ~20 probe files, 4 scripts, 1 workflow

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle / gate | Status |
|---|---|
| I. PoC scope (one DB, no TLS, any password, no control plane) | PASS. The harness connects with `sslmode=disable`, user `admin`, db `postgres`, any password. The control plane is only used on MiniStack, never on micro-dsql. |
| II. Own repo, pinned upstream | N/A (no Rust/Turso changes). |
| III. Backend-agnostic policy | N/A; the probes describe target behaviour only through SQL. |
| IV. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`, `tests/psql/run.sh` vs micro-dsql | Gates 1–3: unaffected by this feature (no Rust code); run once M1 lands. Gate 4: this feature *creates* `tests/psql/run.sh`; running it against micro-dsql is the Reconcile (after M1) phase. Tests are not weakened: expected outputs come from the reference, not from micro-dsql. |
| V. Known-gap policy | `tests/psql/known-gaps.txt` is created (empty) and honoured by `run.sh`; `KNOWN_GAPS.md` is created only if a gap is recorded during Reconcile. |
| Repo constraints (research/ read-only, no machine paths, no tool attribution) | PASS by design; checked before commit. |
| Scope questions | None: T1 is in PLAN.md scope. Probes describe M2/M3 behaviour but implement nothing. |

**Post-design re-check**: PASS (design adds only scripts, SQL and docs; no new scope).

## Project Structure

### Documentation (this feature)

```text
specs/003-t1-testing/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── run-sh.md          # runner CLI, env, exit codes, output format
│   ├── scripts.md         # ministack-up/down, compare.sh contracts
│   └── report-format.md   # reports/diff-<date>.md layout
├── checklists/requirements.md
└── tasks.md
```

### Source Code (repository root)

```text
tests/psql/
├── 00_connect.sql  10_crud.sql  20_txn.sql  30_errors.sql  40_conflict.sh
├── expected/<name>.out
├── known-gaps.txt
├── run.sh
└── probes/
    ├── <NN>_<rule>.sql
    └── expected/<NN>_<rule>.out
scripts/
├── ministack-up.sh
├── ministack-down.sh
└── compare.sh
.github/workflows/differential.yml
reports/            # gitignored
```

**Structure Decision**: follow PLAN.md §3 exactly; probes are one rule family per file so a mismatch
maps to one backlog item.

## Complexity Tracking

No constitution violations.
