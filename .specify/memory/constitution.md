# micro-dsql Constitution

## Core Principles

### I. PoC Scope Discipline

micro-dsql is a proof of concept that emulates the database of a single Aurora DSQL cluster for
local tests and CI. Within the PoC:

- The process MUST serve exactly one database (`postgres`). Multi-cluster, multi-database and SNI
  routing MUST NOT be implemented.
- The server MUST NOT implement TLS; clients connect with `sslmode=disable`.
- The server MUST accept any user and any password, including SDK-generated IAM tokens, without
  validating them.
- There MUST be no control plane (`CreateCluster`, `GetCluster`, ...). Tests that need one run a
  separate emulator alongside micro-dsql.
- Work outside this scope (the docs/plan.md non-goals and "Later" list) MUST NOT be started until this
  constitution is amended to admit it.

Rationale: the PoC succeeds by proving database behaviour (SQL surface, DSQL rules, OCC) quickly.
Every out-of-scope feature delays that answer and adds surface to keep working across Turso bumps.

### II. Own Repository, Pinned Upstream

- micro-dsql MUST live in its own repository. DSQL-specific code MUST NOT be hosted in a Turso fork.
- Turso crates MUST be consumed as a git dependency pinned to a single commit (`rev = "<sha>"`),
  identical for every Turso crate in the workspace. Bumping the pin MUST be a deliberate change that
  passes the full gate set (Principle IV).
- The upstream wire server (`postgres/server/lib.rs`) MUST be vendored, not forked. The vendored
  file MUST keep its MIT licence notice and MUST record the upstream commit hash it was taken from.
- Missing behaviour MUST first be attempted in our own crates (server or policy layer). A Turso
  patch is allowed only when a required hook is genuinely missing inside `turso_pg` /
  `turso_core` and that attempt has failed; the feature's plan MUST record why.
- An unavoidable Turso patch MUST be minimal and generic (not DSQL-specific) and MUST be carried
  as a patch file committed in this repository under `patches/`, applied with the `patch-crate`
  tool: the crate is listed under `crates` in `[workspace.metadata.patch]` (or
  `[package.metadata.patch]`), `cargo patch-crate` writes the patched copy to `target/patch/`, and
  a Cargo `[patch]` entry points the dependency at that path. A Turso fork or patch branch MUST
  NOT be used. Changes MUST NOT be proposed to Turso upstream (owner decision, 2026-11-12).
- Patch files MUST apply to the same pinned rev as every other Turso crate and MUST be re-checked
  at every pin bump.
- Known constraint the first patch MUST resolve: `patch-crate` (0.1.13) documents only
  crates.io dependencies with `[patch.crates-io]`. For a crate inside a git workspace it copies
  only that crate's subdirectory to `target/patch/` (upstream issue
  mokeyish/cargo-patch-crate#18, open), so Turso crates, whose manifests inherit
  `*.workspace = true` fields from the Turso workspace root, do not build from the copy as is.
  The first patch MUST demonstrate a working setup at the pinned rev; if none exists, this
  constitution MUST be amended before any other mechanism is used.

Rationale: the Turso Postgres frontend is pre-1.0 with heavy churn. A pinned dependency plus a
small vendored file keeps upgrades cheap and predictable; a growing fork does not. Patch files in
this repository keep every deviation from upstream reviewable in one place and tied to the pin.

### III. Backend-Agnostic Policy Layer

- The DSQL policy layer (statement deny rules, per-transaction limits, SET whitelist, rewrites,
  error shaping) MUST operate only on the `pg_query` AST and on per-session/per-transaction state
  owned by micro-dsql.
- The policy crate MUST NOT depend on, import or inspect Turso internals (`turso_core`, `turso_pg`
  types). Any information it needs from the engine MUST cross a narrow interface defined by
  micro-dsql (for example, a changed-row count).
- Policy rules MUST be unit-testable without starting an engine.

Rationale: if Turso does not hold up, the plan pivots to an embedded PG16 backend. A policy layer
that only knows the PG AST and its own state survives that pivot unchanged.

### IV. Test-First with Mandatory Gates (NON-NEGOTIABLE)

- Behaviour MUST be specified by a failing test before it is implemented (red, green, refactor).
- Every change MUST pass these gates before merge:
  1. `cargo fmt --check`
  2. `cargo clippy --all-targets -- -D warnings`
  3. `cargo test --workspace`
  4. From milestone T1 onward: `tests/psql/run.sh` against a running micro-dsql instance.
- `cargo fmt --check` and `cargo clippy` MUST always pass; they are never subject to the known-gap
  rule in Principle V.
- Tests MUST NOT be deleted, weakened, or have their expectations loosened to make a gate pass.

Rationale: the emulator is only useful if its behaviour is pinned down by tests that apps can trust.
Gates that run identically for every change keep regressions out of `main`.

### V. Local Merge and Known-Gap Policy

- Features are merged directly into the local `main` branch once all gates pass. Pull requests are
  not used.
- Known-gap rule (experimental PoC): if a feature still fails a test gate after two
  implement/converge rounds, it MAY be merged anyway, provided that:
  - every failing test stays in the suite;
  - each failing Rust test is skipped with `#[ignore = "KNOWN-GAP: <reason>"]`;
  - each failing psql scenario is listed in the skip list `tests/psql/known-gaps.txt` with a reason;
  - every skipped test is recorded in `docs/known-gaps.md` with the observed error and a suspected cause.
- A known gap is closed by removing its skip marker and its `docs/known-gaps.md` entry in the same change
  that makes the test pass.

Rationale: the PoC's goal is to measure how close Turso gets to DSQL. Recording gaps visibly keeps
progress moving without hiding failures or losing the tests that define the target behaviour.

## Repository Constraints

- `research/` is read-only reference material. It MUST NOT be edited by feature work; new findings
  go in new files or in feature documentation.
- Committed files MUST NOT contain machine-specific paths, user names or hostnames (for example
  absolute home-directory or mount paths). Use repository-relative paths and environment variables.
- Commits, code, comments and documentation MUST NOT name or credit the tools or assistants used to
  author them, and commits MUST NOT carry `Co-Authored-By` or similar attribution trailers.
- Third-party test suites (for example Dreamescaper's golden probes) MUST be referenced, not
  vendored: tests fetch them at test time at a pinned upstream commit recorded in this repository.
- docs/plan.md is the source of truth for scope; where docs/plan.md and `research/` disagree, docs/plan.md wins on
  scope and `research/` wins on facts.

## Development Workflow

- Feature branches MUST follow the Spec Kit naming pattern `<NNN>-<slug>` (for example
  `001-own-binary`), where `NNN` is the zero-padded feature number.
- Each feature flows through Spec Kit: specify, plan, tasks, implement, with converge rounds as
  needed. The plan's Constitution Check MUST list the gates of Principle IV and any scope question
  raised by Principle I.
- Merging follows Principle V: gates green (or known gaps recorded per Principle V), then a direct
  merge into local `main`. Commit messages SHOULD use Conventional Commits.

## Governance

- This constitution supersedes other project practices. Where a plan, spec or task conflicts with
  it, the constitution wins until amended.
- Amendments are made by editing this file in a dedicated commit that states the change and the new
  version. Dependent Spec Kit templates MUST be checked for consistency in the same change.
- Versioning follows semantic versioning: MAJOR for removing or redefining a principle, MINOR for a
  new principle or section or materially expanded guidance, PATCH for clarifications and wording.
- Compliance is checked at every plan's Constitution Check and before every merge to `main`. Any
  deviation MUST be justified in the plan's Complexity Tracking table.

**Version**: 1.2.0 | **Ratified**: 2026-11-12 | **Last Amended**: 2026-11-12
