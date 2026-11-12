# micro-dsql

A small local emulator of one [Aurora DSQL](https://aws.amazon.com/rds/aurora/dsql/) cluster's
database, for unit and integration tests and CI. One binary, no Docker; ordinary Postgres
drivers connect to it with `sslmode=disable` and any user and password. It runs on the
PostgreSQL frontend of [Turso](https://github.com/tursodatabase/turso), pinned to one commit.

This is a proof of concept: see [PLAN.md](PLAN.md) for scope and milestones and
[KNOWN_GAPS.md](KNOWN_GAPS.md) for what does not behave like DSQL yet.

## Quick start

```sh
cargo run --release -p micro-dsql -- --listen 127.0.0.1:5432
psql "host=127.0.0.1 port=5432 user=admin dbname=postgres password=anything sslmode=disable"
# done: CREATE TABLE, INSERT, SELECT, BEGIN/COMMIT ...
```

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

## Development

Needs Rust (stable) and clang/libclang (Xcode command line tools on macOS,
`clang libclang-dev` on Debian/Ubuntu). The gates:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
PGHOST=127.0.0.1 PGPORT=5432 PGPASSWORD=x tests/psql/run.sh   # against a running micro-dsql
```

Layout: `bin/micro-dsql` (CLI), `crates/server` (wire server, adapted from Turso's
`postgres/server/lib.rs`, MIT, see [LICENSES/turso-MIT.txt](LICENSES/turso-MIT.txt)),
`crates/policy` (DSQL rules on the `pg_query` AST, engine-agnostic), `tests/smoke`
(integration tests), `tests/psql` (target-agnostic psql scenarios, see its README).
