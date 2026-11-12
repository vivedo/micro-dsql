# 04 — Gap analysis: DSQL requirements vs Turso Postgres

Effort levels:
- **S**: days, in our own layer.
- **M**: about 1–2 weeks, or a small upstream patch.
- **L**: weeks, needs a core or translator change.
- **XL**: open-ended.

Where the work happens:
- **Own:** our server or policy crate; no fork needed.
- **Fork:** a patch to turso_pg or turso_core. Upstreamable where it's generic PG work.
- **Upstream:** generic PG compatibility that Turso is likely to build anyway.

## A. Connection layer

> **Scope decision (user, 2026-11-12).** The target is local tests and CI, so the PoC leaves out TLS, SNI routing, token validation and role/IAM mapping. They are later improvements. The PoC accepts plaintext connections and any password. Only the rows marked ★ are in the PoC.
>
> **Known consequence.** The official DSQL connectors default to `verify-full` plus direct TLS (JDBC, Go, .NET, Ruby, Node). Until TLS lands, they work only if TLS is turned off in their config, or by connecting with the plain driver (`sslmode=disable`). Plain drivers and ORMs are unaffected.

| Requirement | Turso today | Work | Effort |
|---|---|---|---|
| TLS mandatory, SSLRequest + direct TLS + ALPN | pgwire supports it; Turso passes `None` | Own: build a rustls acceptor, reject plaintext with DSQL's FATAL | S |
| SNI → cluster routing, `amzn-cluster-id` fallback | pgwire exposes SNI | Own: map cluster IDs to `Database` instances | S |
| Local CA + wildcard cert for `*.dsql.<region>.<suffix>` | — | Own: generate the CA on first run, export it like Floci's `/ca.pem` | S |
| Cleartext password = IAM token; validate format, action, expiry, optional SigV4 | NoopHandler | Own: pgwire cleartext handler + SigV4 verifier | S–M |
| `admin` vs custom role / DbConnect mapping, `CREATE ROLE … WITH LOGIN`, `AWS IAM GRANT/REVOKE`, `sys.iam_pg_role_mappings` | Roles/GRANT are very new; LOGIN rejected; startup user ignored | Fork: accept LOGIN, run the session as the startup role. Own: `AWS IAM …` is a pre-parse rewrite into a mapping table | M |
| ★ Startup params (server_version 16.x, TimeZone UTC, …) | — | Own | S |
| Connection limits, 60 min cap, 10 MiB message | — | Own | S |
| ★ Binary parameter format in the extended protocol | **Text only** | Upstream/Fork: decode binary for common OIDs | M |
| CancelRequest | ❌ | Upstream: interrupt the connection | M |
| ★ Statement execution blocks tokio workers | sync calls | Own: `spawn_blocking` or a dedicated thread per connection | S |

## B. Transactions and concurrency

| Requirement | Turso today | Work | Effort |
|---|---|---|---|
| Snapshot isolation, RR only, other levels rejected | ✅ | — | — |
| 40001 on write-write conflict | ✅ but **raised at the write statement** | Fork (frontend): on `WriteWriteConflict` mid-transaction, mark the txn doomed and fail at COMMIT with OC000. **Caveat:** the statement still can't apply, so reads later in the txn won't see that write. A cleaner option is a core mode that allows multiple uncommitted versions with first-committer-wins validation. | M (approximate) / L–XL (exact) |
| OC000 / OC001 message texts | 40001 with Turso text | Own/Fork: remap messages | S |
| OC001 on stale catalog | `SchemaConflict`/`SchemaUpdated` → 40001 | Map to OC001 text; check it triggers in the same situations | S–M |
| **Autocommit DML must not raise 55P03** | Exclusive txn → `Busy` → 55P03 | Fork: run autocommit DML as a single-statement concurrent txn, or add a busy timeout and map to 40001 | M |
| **Snapshot at BEGIN** | Snapshot at first statement | Fork (core): begin the read snapshot eagerly | S–M |
| `SELECT … FOR UPDATE` | ❌ rejected | Fork (translator + core): add the selected rows to the write set (a no-op version write). Matches the DSQL matrix. | L |
| `SELECT … FOR KEY SHARE`, FK implicit KEY SHARE | ❌ | Fork (core): a read-lock set validated at commit against key-changing writes | L |
| One DDL per txn; no DDL+DML mix | Not enforced | Own: classify each statement from the parse tree, track per-session state | S |
| 3,000-row limit (FK cascades included) | `n_change()` per statement | Own: per-txn accumulator. Fork if cascades aren't counted. | S–M |
| 10 MiB per-txn data limit | — | Fork (core): count bytes written to the MVCC write set | M |
| 300 s transaction age | — | Own: timestamp at BEGIN | S |
| No SAVEPOINT | Turso supports it | Own: reject | S |

## C. SQL surface

| Requirement | Turso today | Work | Effort |
|---|---|---|---|
| Reject what DSQL rejects (triggers, temp, extensions, TRUNCATE, COPY, LISTEN, advisory locks, partitions, enums/arrays as column types, PG18-only syntax, matviews, serial pseudo-types…) | Turso fails closed on much of this already, but **accepts** TRUNCATE, temp tables (?), domains, enums, arrays, serial, matviews, savepoints, COPY | Own: rules over the pg_query AST (Dreamescaper's 45 rules and MiniStack's list are the spec) | M |
| `CREATE [UNIQUE] INDEX ASYNC` → `job_id`, `sys.jobs`, `sys.wait_for_job` | ❌ parse error | Own: pre-parse token rewrite, build synchronously, return a job row. Fork: `sys` schema objects. | M |
| `ALTER TABLE ASYNC … VALIDATE CONSTRAINT`, `ADD CONSTRAINT … NOT VALID` | ❌ | Fork: accept NOT VALID; validate synchronously | M |
| Sequences with `CACHE 65536` | ❌ (CACHE must be 1) | Fork: accept CACHE ≥ 65536 or 1, ignore it | S |
| `GENERATED BY DEFAULT/ALWAYS AS IDENTITY` (bigint) | ❌ | Fork/Upstream: desugar to a sequence + default + catalog bits | M–L |
| ALTER COLUMN SET/DROP DEFAULT, DROP NOT NULL, DROP CONSTRAINT, identity ops | ❌ | Upstream/Fork | M–L |
| PK immutable, ≤8 cols, 1 KiB keys, 255 cols, 24 indexes, 1,000 tables, 10 schemas, … | — | Own: catalog checks + size checks | S–M |
| Bare `numeric` = numeric(18,6) | Bare numeric → REAL | Own: rewrite at DDL time | S |
| timestamptz microsecond precision | ms only | Upstream/Fork (core type encoding) | M |
| int4 range enforcement | missing [I] | Upstream | S |
| interval arithmetic | ❌ | Upstream | L |
| JSON/JSONB full operator and function set | `->`, `->>` only | Upstream | L |
| char(n) | rejected | Upstream | S–M |
| Window functions rank/lag/lead/dense_rank | ❌ | Upstream | M |
| `CREATE FUNCTION … LANGUAGE sql` | ❌ | Upstream/Fork | L |
| `information_schema` (tables, columns, key_column_usage, table_constraints, referential_constraints, sequences, views…) | Only `schemata` | Fork/Upstream: views over the pg_catalog vtabs | L |
| pg_catalog fidelity for ORM introspection | Good coverage, needs checking per ORM | Iterative | L (long tail) |
| SET whitelist (application_name, datestyle, timezone, extra_float_digits, intervalstyle, enable_*, dsql.enable_batched_nestloop) | Most are rejected | Own: accept as no-op (store and SHOW) | S |
| DSQL-shaped EXPLAIN | Turso format | Not worth it | — |

## D. Control plane (optional, see 05)
16 REST-JSON operations. MiniStack, Floci, LocalStack and moto all implement them. Effort S–M if built as a thin Rust HTTP service that creates in-memory `Database`s on CreateCluster.

## Summary of the hard parts

1. **OCC timing (B).** The engine is optimistic, but it fails the *second writer at the statement*, while DSQL fails the *second committer at COMMIT*. A frontend "doomed transaction" shim gets close enough for retry-loop testing. Exact semantics need core MVCC changes, which is where Turso's design would have to bend.
2. **FOR UPDATE / FOR KEY SHARE (B).** Needs core work. These appear in DSQL docs and ORM locking paths, but are less common in typical CI apps.
3. **The PG-compat long tail (C).** `information_schema`, IDENTITY, ALTER COLUMN, JSONB, timestamptz precision, binary params, window functions. These are Turso's general roadmap problems, not DSQL-specific ones. **Upstream is moving fast (≈500 commits in 6 weeks), so we can share this work, but we also depend on it.**
4. **Upstream churn.** A fork against a pre-1.0, fast-moving codebase that is mostly AI-written. Keep patches small, and upstream everything that is generic PG.

Everything in **A**, and most rule enforcement in **B/C**, is straightforward work in our own layer.
