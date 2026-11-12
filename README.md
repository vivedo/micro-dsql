# micro-dsql

A small local emulator of one [Aurora DSQL](https://aws.amazon.com/rds/aurora/dsql/) cluster's
database, for unit and integration tests and CI. One binary, no Docker; ordinary Postgres
drivers connect to it with `sslmode=disable` and any user and password. It runs on the
PostgreSQL frontend of [Turso](https://github.com/tursodatabase/turso), pinned to one commit.

This is a proof of concept: see [docs/plan.md](docs/plan.md) for scope and milestones and
[docs/known-gaps.md](docs/known-gaps.md) for what does not behave like DSQL yet.

## Quick start

```sh
cargo run --release -p micro-dsql -- --listen 127.0.0.1:5432
psql "host=127.0.0.1 port=5432 user=admin dbname=postgres password=anything sslmode=disable"
# done: CREATE TABLE, INSERT, SELECT, BEGIN/COMMIT ...
```

## Docker

The image is built `FROM scratch` around a static (musl) binary, about 36 MB:

```sh
docker build -t micro-dsql .
docker run --rm -p 5432:5432 micro-dsql
# keep data between runs: mount a volume on /data
docker run --rm -p 5432:5432 -v micro-dsql-data:/data micro-dsql --listen 0.0.0.0:5432 --data /data/postgres.db
```

The container runs as an unprivileged user and stops cleanly on `docker stop`. There is no
shell in the image; use `pg_isready` from the host or another container as a health check.

## Usage

```
micro-dsql [--listen 127.0.0.1:5432] [--data <file> | --memory] [--log info]
```

| Flag | Default | Meaning |
|---|---|---|
| `--listen` | `127.0.0.1:5432` | address to listen on; port `0` picks a free port (the chosen one is logged) |
| `--data <file>` | | keep the database in a file (created if missing) |
| `--memory` | yes | fresh in-memory database on every start (the default) |
| `--log` | `info` | log level or `tracing` filter, e.g. `debug` or `micro_dsql_server=debug,warn` |

Logs go to stderr; the server is ready when it logs
`micro-dsql listening on <ip>:<port>`. SIGINT or SIGTERM stops it (exit status 0).

What a client sees: database `postgres`, `server_version` 16.x, `TimeZone` `UTC`,
`is_superuser` `off`, no TLS (an SSL request is answered "no"), no password check, Postgres
SQLSTATEs on errors, and DSQL's only isolation level (`REPEATABLE READ`).

### DSQL rules

Code that would break on DSQL breaks here, with DSQL's SQLSTATE and message:

- **Conflicts**: a write conflict is `40001` `change conflicts with another transaction (OC000)`
  (also when the engine reports lock contention, `55P03`); a concurrent schema change is `40001`
  `schema has been updated by another transaction (OC001)`.
- **Transactions**: one DDL statement per transaction, no DDL and DML in the same transaction,
  no savepoints, at most 3,000 changed rows and 300 s per transaction (`54000`). A query string
  with several statements is checked as one implicit transaction. A refusal inside a
  transaction aborts it (`25P02` until `ROLLBACK`).
- **Statements DSQL does not have** are refused with `0A000` (`TRUNCATE`, `CREATE TYPE`,
  `CREATE TEMP TABLE`, `COPY`, `serial`, arrays, `inet`, `money`, triggers, extensions, `LOCK`,
  ...).
- **Settings**: only DSQL's session parameters can be set (`application_name`, `client_encoding`,
  `datestyle`, `extra_float_digits`, `intervalstyle`, `timezone`, `search_path`, the `enable_*`
  planner switches and `dsql.enable_batched_nestloop`); anything else is `0A000`. `SHOW` returns
  what was set.
- **DDL**: a column typed bare `numeric` or `decimal` is `numeric(18,6)`. Indexes are built with
  `CREATE [UNIQUE] INDEX ASYNC`; plain `CREATE INDEX`, `CONCURRENTLY`, access methods other than
  `btree` and more than 8 key columns are refused. Identity columns must be `bigint` with an
  explicit `CACHE` of 1 or at least 65536 (`CREATE SEQUENCE` needs one too); `ADD COLUMN` with a
  default or constraint, `ALTER COLUMN TYPE`, `SET NOT NULL`, dropping a primary-key column and
  every `ADD CONSTRAINT` other than `FOREIGN KEY`/`CHECK ... NOT VALID` are refused. A
  `NOT VALID` constraint is added by rebuilding the table (see
  [docs/known-gaps.md](docs/known-gaps.md) for the differences from DSQL).
- **Jobs and `sys`**: `CREATE INDEX ASYNC` and `ALTER TABLE ASYNC ... VALIDATE CONSTRAINT` run to
  completion before they return, answer with a `job_id` row and record the job in `sys.jobs`;
  `CALL sys.wait_for_job('<id>')` returns `succeeded`, `SELECT sys.wait_for_job(...)` fails with
  `42809` (it is a procedure), `SELECT * FROM sys.dsql_major_version()` returns `1`, and `sys` is
  read-only for clients.

Raise the log level to `debug` to see one line per refused statement with the rule id.

## Development

Needs Rust (stable) and clang/libclang (Xcode command line tools on macOS,
`clang libclang-dev` on Debian/Ubuntu). The gates:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
PGHOST=127.0.0.1 PGPORT=5432 PGPASSWORD=x tests/psql/run.sh   # against a running micro-dsql
```

### Exit checks (outside `cargo test`)

Two scripts check M3's goals against a running server (start micro-dsql on a free port first,
fresh, and point `PGHOST`/`PGPORT`/`PGPASSWORD` at it; they exit `2` when a prerequisite is
missing):

```sh
scripts/drizzle-check.sh   # a DSQL-shaped Drizzle project: migrations, queries, stock migrator; needs Node 22+ and npm
scripts/conformance.sh     # Dreamescaper's golden probes against live-DSQL recordings; needs git, Go 1.27.1+ and network
```

`scripts/conformance.sh` fetches the probes at the commit pinned in `scripts/conformance/UPSTREAM`
into `target/conformance/` and writes `reports/conformance-<date>.md` (both gitignored). See
[tests/orm/drizzle/README.md](tests/orm/drizzle/README.md) for the Drizzle project.

## Repository layout

```
crates/
  micro-dsql/   the binary: CLI flags, logging, signal handling (tests/: CLI tests)
  server/       Postgres wire server, adapted from Turso's postgres/server/lib.rs (tests/: integration tests)
  policy/       DSQL rules on the pg_query AST, engine-agnostic
tests/psql/     target-agnostic psql scenarios and DSQL-rule probes (see its README)
tests/orm/      Drizzle example project (migrations in DSQL form, runners, smoke)
scripts/        MiniStack reference setup and differential comparison, drizzle-check.sh, conformance.sh
docker/         container build helper (see Dockerfile)
docs/           plan and known gaps
research/       background research and the M0 spike
specs/          feature specifications, plans and task lists
```

## Licence

MIT, see [LICENSE](LICENSE). `crates/server` includes code adapted from Turso (MIT), see [crates/server/LICENSE-turso](crates/server/LICENSE-turso).
