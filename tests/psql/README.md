# tests/psql: plain-psql scenarios and DSQL-rule probes (T1)

Target-agnostic: everything here needs only POSIX `sh` and `psql`, and selects its target
through the standard libpq variables. It runs against micro-dsql, the MiniStack DSQL
emulator, or any Postgres-speaking endpoint.

```sh
export PGHOST=127.0.0.1 PGPORT=15432 PGUSER=admin PGDATABASE=postgres \
       PGPASSWORD=anything PGSSLMODE=disable
tests/psql/run.sh            # layer-1 scenarios (blocking)
tests/psql/run.sh --probes   # DSQL-rule probes (report-only)
tests/psql/run.sh --all      # both
tests/psql/run.sh 20_txn     # one scenario or probe by name
```

Exit status: `0` all scenarios passed or were skipped, `1` a scenario failed, `2` psql is
missing or the target is unreachable. Probe mismatches never change the exit status.

## Layout

| Path | What |
|---|---|
| `NN_name.sql`, `NN_name.sh` | Layer-1 scenarios (PLAN.md T1). `.sql` runs as `psql -X -q -f`; `.sh` prints its own deterministic output. |
| `expected/NN_name.out` | Expected scenario output. |
| `probes/NN_name.sql` | DSQL-rule probes; one rule family per file. |
| `probes/expected/NN_name.out` | Reference behaviour (captured from MiniStack with `DSQL_STRICT=1`). |
| `known-gaps.txt` | Skip list: `<scenario> <reason>` per line (constitution Principle V). |

## Scenarios

| Scenario | Checks |
|---|---|
| `00_connect` | `SELECT 1`, `current_user` present, `current_database()`, `server_version` is 16.x, `TimeZone` is `UTC` |
| `10_crud` | CREATE TABLE with PK, `INSERT … RETURNING`, `SELECT … WHERE … ORDER BY`, `UPDATE`, `DELETE`, `DROP TABLE` |
| `20_txn` | `BEGIN`/`COMMIT`; `ROLLBACK` discards; error in a transaction then `25P02` until `ROLLBACK`; `BEGIN ISOLATION LEVEL REPEATABLE READ` |
| `30_errors` | `23505`, `42P01`, `42601` with `VERBOSITY verbose`; the session survives |
| `40_conflict` | Two sessions update one row inside RR transactions; one gets `40001`; the first committer's value is kept |

`40_conflict.sh` orders the sessions so that no statement waits on a row lock (B takes its
snapshot, A updates and commits, B updates and commits). It only checks *that* `40001` is
reported, not *when* (DSQL reports it at `COMMIT`, Postgres and Turso at the `UPDATE`).
`CONFLICT_TIMEOUT` (seconds, default 30) bounds every wait.

## Determinism and normalisation

Scenarios and probes never print OIDs, PIDs, timings, generated job ids or exact version
strings (job ids are checked for shape only; `server_version` for its major version). Error
lines carry `psql:<file>:<line>:` prefixes, so editing a script means re-capturing its
expected output.

`run.sh` applies exactly two normalisations to actual output before comparing:

1. lines starting with `LOCATION:` are deleted (`VERBOSITY verbose` prints the server's source
   file and line, which differ between engines and builds);
2. lines ending in `does not exist, skipping` are deleted (NOTICEs from `DROP … IF EXISTS`
   depend on leftovers from earlier runs).

`20_txn` and `40_conflict` use `\set VERBOSITY sqlstate` so only SQLSTATEs are compared;
`30_errors` and the probes use `verbose` so DSQL's exact messages are visible.

## Re-capturing expected output

Expected outputs come from the reference, never from micro-dsql. Use a psql 16 client (the
version CI installs); psql's own formatting can change between major versions.

```sh
eval "$(scripts/ministack-up.sh)"
tests/psql/run.sh --all --capture            # or name the files to re-capture
tests/psql/run.sh --all                      # must be green twice in a row
git diff tests/psql                          # review every change
```

## Known MiniStack deviations from real DSQL

The probes record MiniStack's behaviour. Where MiniStack is known to differ from real DSQL
(research/01), a micro-dsql mismatch on these lines is not necessarily a bug:

| Probe | MiniStack | Real DSQL |
|---|---|---|
| `12_session_identity` | `current_user` = `postgres`, `is_superuser` = `on`, collation `en_US.utf8` | `admin`, `off`, `C` |
| `11_isolation_levels` | default `read committed`; every isolation level accepted | REPEATABLE READ only |
| `05_sys_jobs` | `sys.dsql_major_version()` missing | returns `1` |
| `25_txn_row_limit` | only rows in `VALUES` lists count; error `25006` | every mutated row counts; `54000 transaction row limit exceeded` |
| `21_column_types` | bare `numeric` stays `numeric` | `numeric(18,6)` |
| `03_temp_table` | `CREATE TEMP VIEW` accepted | no temporary views |
| `06`/`07`/`24` | discipline errors use `25006` | SQLSTATE unverified (probably `0A000`) |

MiniStack also closes the connection (instead of returning a FATAL error) when the database
name is not `postgres`.
