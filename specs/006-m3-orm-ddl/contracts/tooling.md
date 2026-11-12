# Contract: scripts outside the Rust gate

Neither script is run by `cargo test --workspace` (FR-041). Both select the target with the
standard libpq variables (`PGHOST`, `PGPORT`, `PGUSER`, `PGPASSWORD`, `PGDATABASE`, `PGSSLMODE`),
like `tests/psql/run.sh`, and expect a running micro-dsql (never port 5432 in examples or docs).

Exit status for both: `0` pass, `1` check failed, `2` prerequisite missing (tool, network,
unreachable target).

## `scripts/drizzle-check.sh`

| Step | Pass condition |
|---|---|
| `npm ci` in `tests/orm/drizzle` (Node ≥ 22, lockfile) | installs exactly the locked versions |
| `node migrate-dsql.mjs` | every statement of every migration succeeds, one statement per transaction; journal `drizzle.__drizzle_migrations_dsql` gets one row per migration; a second run applies nothing |
| `node smoke.mjs` | drizzle-orm insert-returning (identity values), select with join + where, update, delete, `db.transaction` commit and rollback, FK violation `23503`; all assertions hold |
| `node stock.mjs` | drizzle-orm's stock `migrate()` fails with code `42704` and message `type "serial" does not exist` |

The check assumes a fresh server (in-memory, the default); it prints each step and its result.

## `scripts/conformance.sh`

| Input | Meaning |
|---|---|
| `scripts/conformance/UPSTREAM` | one line: the pinned commit of `github.com/Dreamescaper/dsql-emulator` (initially `38a33bf283d7670b30dc959e8f38d0cf4eef1ddc`) |
| `scripts/conformance/compare.go` | our comparer (package `main`), copied into the fetched tree at run time |
| `CONFORMANCE_MIN` | pass-rate threshold in percent, default `60` |
| `CONFORMANCE_DIR` | working directory, default `target/conformance` (gitignored) |

Output: `reports/conformance-<YYYY-MM-DD>.md` (gitignored) with: pinned commit, target, server
version, UTC time, total `pass/total (pct)`, per-group table, every failing case (`case | field |
step SQL | golden | micro-dsql`), and the advisory notes count. The script prints the total line
last. Requires `git`, Go ≥ 1.27.1 (`GOTOOLCHAIN=local`), network on the first run (later runs reuse
the checkout if it is at the pinned commit).
