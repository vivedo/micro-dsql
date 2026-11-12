# Phase 0 research: M0 spike

## R1. Toolchain for building tursopg

- **Decision**: build with the locally installed Rust stable (`RUSTUP_TOOLCHAIN=stable`), overriding the
  clone's `rust-toolchain.toml` (1.88) via the environment, not by editing the clone.
- **Rationale**: rustup could not download 1.88 on this machine (TLS certificate error from
  static.rust-lang.org); docs/plan.md §2 says to build with stable first.
- **Alternatives**: fix rustup's TLS and install 1.88 (blocked by the network environment).

## R2. Language for OCC scenarios

- **Decision**: Python + psycopg 3 with two connections in one process, steps interleaved
  deterministically, plus a thread pool for the autocommit storm.
- **Rationale**: exact control over statement order, easy SQLSTATE capture (`e.sqlstate`), timeouts via
  `statement_timeout` are not reliable on Turso, so a client-side socket timeout (`connect_timeout`
  plus a watchdog thread) is used instead.
- **Alternatives**: two psql coprocesses (fragile ordering), Rust tokio-postgres (no crate yet; M1).

## R3. App SQL stand-in

- **Decision**: a small "shop" schema (customers, products, orders, order_items, events) with uuid,
  bigint, int, numeric(12,2), bool, timestamptz, jsonb, text columns, FKs and secondary indexes.
- **Rationale**: covers every feature listed in docs/plan.md risk table (JSONB, timestamptz, window functions…)
  in a shape typical of CI apps.

## R4. Driver versions

- **Decision**: latest released versions available from PyPI / npm / Go proxy at run time; exact versions
  are recorded in results.
- **Rationale**: the spike measures what a new app would use today.
