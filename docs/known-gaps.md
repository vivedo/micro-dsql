# Known gaps

Deviations from Aurora DSQL / PostgreSQL that micro-dsql knowingly has, per constitution
Principle V. Each entry: what is observed, the suspected cause, and the milestone expected to
close it. Blocker ids (B1–B14) refer to the M0 spike findings (research/06 §9).

**Skipped tests:** none. No Rust test is `#[ignore]`d and `tests/psql/known-gaps.txt` is empty:
all five T1 layer-1 scenarios pass against micro-dsql. The entries below are gaps without a
failing test in the suite (probes and M0 findings), kept here so they are not forgotten.

Fixed in M1 (with tests in `crates/server/tests/smoke/`): B4 (Terminate closes the connection), B5 (foreign
keys enforced, `23503`, `ON DELETE CASCADE`), B9 except the items below, and most of B1–B3.

Fixed in M2 (tests in `crates/server/tests/smoke/dsql_*.rs` and `crates/policy`): B13 (the DSQL
deny rules, transaction discipline and limits), the SET whitelist part of B9, the
`SET SESSION CHARACTERISTICS` entry, the `55P03` mapping of B10, DSQL's OC000/OC001 error shapes,
and the bare-`numeric` rewrite of B6.

| Id | Observed | Suspected cause | Milestone |
|---|---|---|---|
| B1 (rest) | Describe reports the right parameter count, and types from casts (`$1::int8`), column context (`col = $1`, `INSERT … VALUES`, `UPDATE … SET col = $1`) and `LIMIT/OFFSET`. Parameters in other positions (function arguments, arithmetic, `= ANY($1)`, subqueries) are described as `text`; asyncpg then refuses non-string arguments client-side. | The engine does not infer parameter types; micro-dsql infers them from the AST with simple rules (`crates/policy/src/params.rs`). | M2 (more rules) / upstream (translator type inference) |
| B2 (rest) | Binary parameters for `interval`, `time`, arrays and other types not in the supported list fail with `22P03`. `numeric` parameters are bound as floating point. | Decoders exist only for common OIDs (`crates/server/src/binary.rs`); the engine has no exact decimal (B6). | M2 |
| B3 (rest) | `sum()`, `avg()`, `min()/max()` over expressions and other untyped expressions are described as `text`; `real` columns are reported as `float8`. | The engine reports no type for these expressions; micro-dsql only overrides `count()` (→ `int8`) and boolean expressions (→ `bool`). | M2 / upstream |
| B6 | `numeric(p,s)` arithmetic and aggregates are computed in floating point (`sum` = `53.230000000000004`). Bare `numeric` columns are now declared `numeric(18,6)` as on DSQL. | Engine has no decimal type (stores REAL). | upstream |
| B7 | `timestamptz` literals with `+00`/`+01` offsets fail with `22007 invalid timestamp value`; microseconds are truncated to milliseconds; no `date_trunc`, no interval arithmetic (`interval '1 day' + interval '2 hours'` returns `3`). | Engine date/time support. | upstream |
| B8 | jsonb `@>` returns no rows instead of matching; `?`, `#>>`, `jsonb_build_object` missing. | Engine JSON support. | upstream |
| B9 (rest) | `current_user`/`session_user` are `postgres` (DSQL: the login role, e.g. `admin`). With `--data <file>`, `current_database()` is the file's stem (only the default in-memory database is `postgres`). `SET timezone` accepts only UTC spellings (`22023` otherwise). | Engine roles and naming; the engine has only UTC. | later |
| B10 | Concurrent autocommit writes still fail inside the engine with `55P03 Database is busy`; micro-dsql reports every such error as `40001` `change conflicts with another transaction (OC000)`, so clients see DSQL's retryable error. | Engine autocommit uses non-concurrent transactions. | M4 (fix; then drop the mapping) |
| B11 | Any committed DDL aborts every concurrent writing transaction with `40001 Database schema conflict`. | Schema cookie is database-wide in the engine. | M4 / upstream |
| B12 | `information_schema` missing; `GENERATED … AS IDENTITY`, sequence `CACHE 65536`, `ADD CONSTRAINT [NOT VALID]`, `ALTER COLUMN`, `CREATE INDEX ASYNC`, `FOR UPDATE`, `DELETE … USING` rejected. | Engine / DSQL-specific syntax. | M2–M4 (docs/plan.md) |
| B14 | `int2` overflow gives `P0001` (PG: `22003`); `int4` overflow is not detected (`2147483647::int4 + 1` = `2147483648`). | Engine type checks. | upstream / M2 error remap |
| DEALLOCATE | `DEALLOCATE <name>` and `DEALLOCATE ALL` are answered with their command tags but do not remove protocol-level prepared statements; an unknown name is not reported (`26000` in PostgreSQL). Enough for psycopg, which sends `DEALLOCATE ALL` after DDL. | The simple-query handler cannot reach pgwire's statement store; the engine has no SQL `PREPARE`. | M2 |
| COPY | `COPY … TO STDOUT` collects all rows before sending them (upstream streamed them). | Statements now run on the blocking pool and results are materialised there. | later, if large exports matter |
| Errors | Besides DSQL's own errors (OC000, OC001, the rule refusals) only a few engine errors are reshaped like PostgreSQL's (`42P01` message and position, `42601` position, schema name on constraint errors, `3B001` text). Other engine messages are passed through unchanged. | Error shaping covers what the scenarios and probes check (`crates/policy/src/errors.rs`). | as found |
| Autocommit row limit | The 3,000-row limit is enforced for explicit transactions after each statement, and for an autocommit statement only when it is an `INSERT … VALUES` list (checked before it runs). Other autocommit statements that change more rows succeed. | Autocommit is not run as a transaction yet. | M4 |
| Row counts | The row total of a transaction is the engine's per-statement change count; rows deleted or updated by foreign-key `ON DELETE`/`ON UPDATE` actions are not counted. | The engine reports only the statement's own rows. | later |
| Multi-statement strings | A simple-protocol query string with several statements is checked as one implicit transaction (one DDL, no DDL with DML) and refused as a whole, but its statements are not run atomically: when a later one fails, earlier ones stay applied. | Atomic execution of the string needs autocommit as a transaction. | M4 |
| Rule texts without a live record | `COPY is not supported`, `LOCK TABLE is not supported`, `advisory locks are not supported`, `DO`, `CREATE PROCEDURE`, `CREATE RULE`, `CREATE PUBLICATION`, `REINDEX`, `CLUSTER`, `prepared transactions` and the `role` rules come from MiniStack or from micro-dsql itself; no live DSQL record exists. `SET SESSION CHARACTERISTICS … REPEATABLE READ` is accepted as a no-op on the strength of the `BEGIN` form. | No live cluster was available. | when verified on a live cluster |
| SET scope | Remembered settings (`datestyle`, `enable_*`, …) set with plain `SET` inside a transaction that is rolled back keep their new value (PostgreSQL reverts them). `set_config()` of a whitelisted name reaches the engine, not the remembered values; `set_config()` with a non-constant name is not checked. | Remembered values live in the policy layer, outside the engine's transactions. | later |
| Engine gaps found by the probes | The engine rejects or lacks: `SELECT … FOR UPDATE` / `FOR KEY SHARE` (`0A000`, DSQL supports them), `CREATE FUNCTION` (`42601`, even `LANGUAGE sql`), `ANALYZE`, `DECLARE CURSOR`, `DROP TABLE a, b` (several objects), `DROP FUNCTION`, `DROP EXTENSION`, `DROP PUBLICATION`, `to_regclass()`, `CREATE TEMP VIEW` (refused with another text), and `pg_attribute` rows for user tables (so `format_type` queries return nothing). | Engine / translator coverage. | upstream / later |
