# Research: M1 — own binary

Each entry: decision, rationale, alternatives considered.

## R1. Turso dependency and features
- **Decision**: `turso_core` (default features + `conn_raw_api`), `turso_pg`, `turso_pg_parser` as git
  dependencies on `https://github.com/tursodatabase/turso`, `rev = "d5fdf4d876ad7eb68e9cbef9a88e6b65c56b19f5"`;
  `pgwire = "0.36"`, `pg_query = "6.1"` (same as upstream `postgres/server/Cargo.toml` and workspace).
- **Rationale**: constitution II; these are exactly what `turso_pg_server` uses at that rev.
- **Alternatives**: depend on `turso_pg_server` directly (rejected by PLAN.md: handler is private and
  needs changes); crates.io releases (not at the pinned rev).

## R2. Database options
- **Decision**: open with the same `DatabaseOpts` as `tursopg` in M0 (views, custom types,
  encryption, index method, autovacuum, attach), MVCC is forced by `turso_pg`.
- **Rationale**: keeps M0 findings valid; `get_column_type_info` (result OIDs) needs custom types.

## R3. `current_database()` = `postgres`
- **Decision**: in memory mode open the database on a fresh `MemoryIO` with the path `postgres`
  instead of `:memory:`. Turso derives `current_database()` from the path's file stem.
- **Alternatives**: rewrite SQL (fragile), patch Turso (constitution II: last resort). With
  `--data`, the stem of the file name is reported (documented known gap).

## R4. Startup, auth and SSL
- **Decision**: custom `StartupHandler`: protocol negotiation, save startup params, then
  `finish_authentication` with our `ServerParameterProvider` (no password exchange). SSLRequest is
  refused by pgwire's `negotiate_tls(.., None)`.
- **Parameters**: `server_version=16.13`, `server_encoding=UTF8`, `client_encoding=UTF8`,
  `DateStyle=ISO, MDY`, `IntervalStyle=postgres`, `TimeZone=UTC`, `integer_datetimes=on`,
  `is_superuser=off`, `standard_conforming_strings=on`, `in_hot_standby=off`,
  `default_transaction_read_only=off`, `scram_iterations=4096`, `application_name` (client's),
  `session_authorization` (client's user).
- **Alternatives**: cleartext password request (makes psql prompt without PGPASSWORD).

## R5. Terminate (B4)
- **Decision**: our own per-connection loop over pgwire's public pieces; on `Terminate`, break and
  drop the socket.

## R6. Off-runtime execution
- **Decision**: every engine call (prepare, bind, step, describe) runs inside
  `tokio::task::spawn_blocking` with a cloned `PgConnection`; rows are materialised there and
  streamed to the client afterwards.
- **Alternatives**: a dedicated OS thread per connection with a channel (more code, same isolation).
  Materialising rows is what upstream already did (it collected into a `Vec`).

## R7. SHOW / SET interception
- **Decision**: `policy::intercept(sql)` parses with pg_query and returns `Show{name}` for the
  DSQL-reported parameters Turso doesn't know and `SetNoop` for `SET timezone` to a UTC spelling.
  The server answers them itself (one text row). In an aborted transaction the statement is passed
  to the engine, which answers `25P02`.

## R8. Parameter description (B1)
- **Decision**: count = highest `$n` in the AST (pg_query `ParamRef`). Types: client-declared OID
  if non-zero; else an AST hint (`$n::type` casts; `INSERT INTO t (cols) VALUES ($n…)`;
  `col <op> $n` and `SET col = $n` / `LIMIT/OFFSET $n` → int8); column hints are resolved by the
  server by preparing `SELECT col FROM table` and reading its type; otherwise `text`.
- **Alternatives**: full type inference in the translator (upstream, M-size).

## R9. Binary parameters and results (B2, B3)
- **Decision**: decode binary parameters by OID (int2/4/8, float4/8, bool, text/varchar/name/bpchar,
  bytea, uuid, json, jsonb, date, timestamp, timestamptz, numeric); encode binary results for
  int2, int4, int8, float4, float8, bool, text, bytea, uuid, date, timestamp(tz), numeric, json(b)
  from the engine's value. Result OIDs: `count(*)` → int8 (AST hint on the target list),
  `smallint`/`int2` → int2, `numeric`/`decimal` → numeric.
- **Limits**: arbitrary expressions (`sum`, boolean expressions) keep the engine's typing → known gap.

## R10. Foreign keys (B5)
- **Decision**: `conn.inner().set_foreign_keys_enabled(true)` after each session connects; verified
  by a test for `23503`.

## R11. Integration tests
- **Decision**: `tests/smoke/` compiled as a `[[test]]` of the server crate; each test starts the
  server on `127.0.0.1:0` inside the test process and connects with `tokio-postgres` (`NoTls`).
  T1 layer-1 statements replayed via `simple_query`.

## R12. CI
- **Decision**: `.github/workflows/ci.yml`, matrix `macos-latest`/`ubuntu-latest`; Linux installs
  `clang libclang-dev postgresql-client`; macOS uses `libpq` from Homebrew for psql. Steps: fmt,
  clippy, test, build, start `micro-dsql --listen 127.0.0.1:15432`, run `tests/psql/run.sh`.
