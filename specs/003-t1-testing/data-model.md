# Data model: T1 testing phase

| Entity | Fields | Rules |
|---|---|---|
| Scenario | name (`NN_slug`), script (`tests/psql/<name>.sql` or `.sh`), expected (`tests/psql/expected/<name>.out`) | Blocking. `.sql` runs via `psql -X -q -f`; `.sh` runs via `sh` and must print deterministic output itself. |
| Probe | name, script (`tests/psql/probes/<name>.sql`), expected (`tests/psql/probes/expected/<name>.out`) | Report-only; expected captured from the reference. Creates objects prefixed `p<NN>_` and drops them. |
| Target | label, `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`, `PGPASSWORD`, `PGSSLMODE` | `compare.sh` reads `TARGET_A_*` / `TARGET_B_*` (or env files). |
| Skip entry | scenario name, reason | One per line in `tests/psql/known-gaps.txt`; `#` comments and blank lines ignored. |
| Result | name, kind (scenario/probe), status (PASS/FAIL/SKIP/MATCH/DIFFER/ERROR), diff | Written by `run.sh` into its output dir when `RESULTS_DIR` is set. |
| Report | date, targets, result table, per-item diff sections | `reports/diff-<YYYY-MM-DD>.md`. |

State: a target is `reachable` or `unreachable` (checked with `SELECT 1` and a connect timeout) before
any scenario runs.
