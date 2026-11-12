# Known gaps

Deviations from Aurora DSQL / PostgreSQL that micro-dsql knowingly has, per constitution
Principle V. Each entry: what is observed, the suspected cause, and the milestone expected to
close it. Blocker ids (B1–B14) refer to the M0 spike findings (research/06 §9).

**Skipped tests:** none. No Rust test is `#[ignore]`d and `tests/psql/known-gaps.txt` is empty:
all five T1 layer-1 scenarios pass against micro-dsql. The entries below are gaps without a
failing test in the suite (probes and M0 findings), kept here so they are not forgotten.

Fixed in M1 (with tests in `tests/smoke/`): B4 (Terminate closes the connection), B5 (foreign
keys enforced, `23503`, `ON DELETE CASCADE`), B9 except the items below, and most of B1–B3.

| Id | Observed | Suspected cause | Milestone |
|---|---|---|---|
| B1 (rest) | Describe reports the right parameter count, and types from casts (`$1::int8`), column context (`col = $1`, `INSERT … VALUES`, `UPDATE … SET col = $1`) and `LIMIT/OFFSET`. Parameters in other positions (function arguments, arithmetic, `= ANY($1)`, subqueries) are described as `text`; asyncpg then refuses non-string arguments client-side. | The engine does not infer parameter types; micro-dsql infers them from the AST with simple rules (`crates/policy/src/params.rs`). | M2 (more rules) / upstream (translator type inference) |
| B2 (rest) | Binary parameters for `interval`, `time`, arrays and other types not in the supported list fail with `22P03`. `numeric` parameters are bound as floating point. | Decoders exist only for common OIDs (`crates/server/src/binary.rs`); the engine has no exact decimal (B6). | M2 |
| B3 (rest) | `sum()`, `avg()`, `min()/max()` over expressions and other untyped expressions are described as `text`; `real` columns are reported as `float8`. | The engine reports no type for these expressions; micro-dsql only overrides `count()` (→ `int8`) and boolean expressions (→ `bool`). | M2 / upstream |
| B6 | `numeric(p,s)` arithmetic and aggregates are computed in floating point (`sum` = `53.230000000000004`); bare `numeric` is stored as a float. | Engine has no decimal type (stores REAL). | upstream; bare-numeric rewrite in M2 |
| B7 | `timestamptz` literals with `+00`/`+01` offsets fail with `22007 invalid timestamp value`; microseconds are truncated to milliseconds; no `date_trunc`, no interval arithmetic (`interval '1 day' + interval '2 hours'` returns `3`). | Engine date/time support. | upstream |
| B8 | jsonb `@>` returns no rows instead of matching; `?`, `#>>`, `jsonb_build_object` missing. | Engine JSON support. | upstream |
| B9 (rest) | `current_user`/`session_user` are `postgres` (DSQL: the login role, e.g. `admin`). With `--data <file>`, `current_database()` is the file's stem (only the default in-memory database is `postgres`). `SET timezone` accepts only UTC spellings (`22023` otherwise); other non-whitelisted `SET`s keep the engine's behaviour (e.g. `SET statement_timeout = 1000` → `22023`). | Engine roles and naming; the DSQL SET whitelist is M2 scope. | M2 |
| B10 | Concurrent autocommit writes can fail with `55P03 Database is busy`. | Engine autocommit uses non-concurrent transactions. | M2 (map to 40001/OC000), M4 (fix) |
| B11 | Any committed DDL aborts every concurrent writing transaction with `40001 Database schema conflict`. | Schema cookie is database-wide in the engine. | M4 / upstream |
| B12 | `information_schema` missing; `GENERATED … AS IDENTITY`, sequence `CACHE 65536`, `ADD CONSTRAINT [NOT VALID]`, `ALTER COLUMN`, `CREATE INDEX ASYNC`, `FOR UPDATE`, `DELETE … USING` rejected. | Engine / DSQL-specific syntax. | M2–M4 (PLAN.md) |
| B13 | Accepted although DSQL rejects them: `TRUNCATE`, `SAVEPOINT`, `CREATE DOMAIN`, `CREATE TYPE … ENUM`, arrays, `serial`, `COPY … TO STDOUT`, DDL and DML in one transaction, two DDLs in one transaction. The DSQL-rule probes (`tests/psql/run.sh --probes`) all differ from the MiniStack reference (22 of 22). | No DSQL policy layer yet. | M2 |
| B14 | `int2` overflow gives `P0001` (PG: `22003`); `int4` overflow is not detected (`2147483647::int4 + 1` = `2147483648`). | Engine type checks. | upstream / M2 error remap |
| DEALLOCATE | `DEALLOCATE <name>` and `DEALLOCATE ALL` are answered with their command tags but do not remove protocol-level prepared statements; an unknown name is not reported (`26000` in PostgreSQL). Enough for psycopg, which sends `DEALLOCATE ALL` after DDL. | The simple-query handler cannot reach pgwire's statement store; the engine has no SQL `PREPARE`. | M2 |
| SET SESSION CHARACTERISTICS | `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ` fails with `42601 VARIABLE SET is not supported`. | Engine; not in the SET whitelist yet. | M2 |
| COPY | `COPY … TO STDOUT` collects all rows before sending them (upstream streamed them). | Statements now run on the blocking pool and results are materialised there. | later, if large exports matter |
| Errors | Only the errors the T1 scenarios check are reshaped like PostgreSQL's (`42P01` message and position, `42601` position, schema name on constraint errors). Other engine messages are passed through unchanged. | Error shaping is M2 (`crates/policy/src/errors.rs`). | M2 |
