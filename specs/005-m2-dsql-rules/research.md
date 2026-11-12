# Research: M2 — DSQL error shape and the "would break in prod" rules

Phase 0 of [plan.md](plan.md). Sections 1–2 record what the code and a running server do today
(2026-11-12, branch base `be7261e`); sections 3–4 record the reference material used for message
texts; section 5 lists the design decisions.

## 1. Current behaviour (reproduced)

Server: `cargo run -p micro-dsql -- --listen 127.0.0.1:15505`; client: psql (libpq) with
`PGHOST=127.0.0.1 PGPORT=15505`.

`tests/psql/run.sh --all`: **scenarios pass=5 fail=0 skip=0; probes match=0 differ=22**.

Selected outputs (verbose errors):

| Statement | micro-dsql today | Reference (MiniStack probe file) |
|---|---|---|
| `TRUNCATE p01_t` | accepted; rows deleted | `0A000 TRUNCATE is not supported` |
| `SAVEPOINT sp1` (in txn) | `SAVEPOINT` | `0A000 SAVEPOINT is not supported` |
| `CREATE UNLOGGED TABLE …` | accepted | `0A000 temporary/unlogged tables is not supported` |
| `CREATE TEMP TABLE …` | `0A000 CREATE TEMPORARY TABLE is not supported` | `0A000 temporary/unlogged tables is not supported` |
| `CREATE TABLE … (id serial …)` | accepted | `0A000 type "serial" is not supported` |
| `CREATE TABLE … (c money / xml / inet / int[] / point)` | accepted | `0A000 type "money" is not supported` etc. |
| `CREATE TABLE … (c bit(8))` | `0A000 a type modifier on bit is not supported` | `0A000 type "bit(8)" is not supported` |
| `CREATE EXTENSION pgcrypto` | `42601 CREATE EXTENSION is not supported` | `0A000 CREATE EXTENSION is not supported` |
| `CREATE TYPE … AS ENUM`, `DROP TYPE` | accepted | `0A000 CREATE TYPE is not supported` |
| `CREATE TABLE q2 AS SELECT …` | `SELECT 2` (accepted) | (not probed) |
| `COPY p20_t TO STDOUT` | accepted | `0A000 COPY is not supported` |
| `COPY q1 FROM STDIN` | `42601 COPY is handled at the postgres frontend layer` | (not probed) |
| `LISTEN` / `NOTIFY` / `DO` / `VACUUM` / … | `42601 … is not supported` | `0A000 … is not supported` |
| `SELECT pg_advisory_lock(1)` | `42883 no such function` | accepted (MiniStack deviation) |
| `CREATE MATERIALIZED VIEW` | `42601 Materialized views are not supported in MVCC mode` | `0A000 materialized views is not supported` |
| `CREATE TABLE … PARTITION BY RANGE` | `0A000 table partitioning is not supported` | same |
| `MERGE …` | `42601 MERGE is not supported` | (not probed) |
| `BEGIN; CREATE TABLE b; INSERT INTO b …` | accepted | `25006 DDL and DML statements cannot be mixed in a transaction` |
| `BEGIN; CREATE TABLE a; CREATE TABLE b` | accepted | `25006 only one DDL statement can be run in a transaction` |
| 3,001-row `INSERT … VALUES` (autocommit) | accepted | `25006 transaction exceeds the 3,000 row limit` |
| 2,000 + 2,000 rows in one txn | accepted | `25006 transaction exceeds the 3,000 row limit` |
| `SET work_mem = '64MB'` | `42601 SET work_mem is not supported` | `0A000 setting configuration parameter "work_mem" not supported` |
| `SET statement_timeout = '5s'` | `22023 invalid value … supported value is "0"` | `0A000 setting configuration parameter "statement_timeout" not supported` |
| `SET enable_seqscan = off; SHOW enable_seqscan` | `42601 SET enable_seqscan is not supported`, `42601 SHOW … not supported` | `SET`, `off` |
| `SHOW dsql.enable_batched_nestloop` (unset) | `42704 unrecognized configuration parameter` | `on` |
| `SET dsql.enable_batched_nestloop = banana` | accepted | `22023 parameter … requires a Boolean value` |
| `SET dsql.bogus = 1`, `set_config('dsql.bogus', …)` | accepted | `42602 invalid configuration parameter name "dsql.bogus"` + DETAIL |
| `SET datestyle = 'ISO'; SHOW datestyle` | `SET`, then `ISO, MDY` (value not remembered) | (probe sets the default) |
| `SET extra_float_digits = 3; SHOW extra_float_digits` | `SET`, then `1` (not remembered) | — |
| `SET application_name`, `SET search_path` + `SHOW` | work (engine settings) | same |
| `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ` | `42601 VARIABLE SET is not supported` | — |
| `SET TRANSACTION ISOLATION LEVEL REPEATABLE READ` | `SET` (accepted) | — |
| `SET default_transaction_isolation = …` | `42601 SET default_transaction_isolation is not supported` | accepted (MiniStack deviation) |
| `SHOW lc_collate` | `C` (answered by micro-dsql) | — |
| `SET role = admin` | `42704 role "admin" does not exist` | — |

Row counting, observed with an FK `ON DELETE CASCADE` parent and 3 children:
`DELETE FROM p1 WHERE id = 1` reports **`DELETE 1`** and the children are gone. The command tag is
built from the statement's change count, so **cascaded rows are not included** in it.

Numeric: `CREATE TABLE q8 (n numeric, d decimal, e numeric(5))`, `INSERT (1.5, 2.25, 3)` returns
`1.5 | 2.25 | 3` (bare numeric has no scale). The probe's `format_type` query cannot be checked on
micro-dsql: `'p21_ok'::regclass` returns no rows (catalog gap, not M2).

## 2. Facts in the brief checked against the code

| Brief says | Code says | Consequence |
|---|---|---|
| `errors.rs` / `intercept.rs` exist | Yes. `errors.rs::shape_error` reshapes only `42P01`, `42601` and `23xxx` (schema field); it cannot change a SQLSTATE. `intercept.rs` answers `SHOW <fixed list>`, `DEALLOCATE`, `SET timezone` (UTC only). Neither knows OC000/OC001. | Error shaping must be able to change the SQLSTATE (55P03 → 40001) and add DETAIL; `ShapedError` gains `sqlstate` and `detail`. |
| Conflicts surface as 40001 | Engine maps `WriteWriteConflict`, `BusySnapshot`, `CommitDependencyAborted`, `SchemaConflict`, `SchemaUpdated`, `Conflict(_)` to `40001` with texts `Write-write conflict`, `Database snapshot is stale…`, `Commit dependency aborted`, `Database schema conflict`, `Database schema changed`, `Conflict: …`; `Busy` and `TableLocked` to `55P03` (`Database is busy`, `database table is locked`). `turso_pg::Error::core_error()` exposes the variant. | The server classifies by variant (not by text) into a policy-owned `EngineFailure` enum; the policy crate never sees engine types (Principle III). |
| 3,000 rows "counted with `n_change()`" | `n_change()` is a per-statement method on the engine statement, read today only in `execute_non_query` (for the command tag). Statements with `RETURNING` go through `execute_query`, which never reads it. Cascaded FK rows are not included (§1). In autocommit the statement has committed before the count is known. | The server passes `rows_changed` after every statement (both paths) to the policy, which accumulates per transaction. Autocommit enforcement is limited to the statically known `INSERT … VALUES` count; cascades and other autocommit statements are new known gaps. Whether `n_change()` is set for `RETURNING` statements must be verified by a failing test first (task). |
| "Error texts and codes are in research/01 §3" | research/01 §3.1 marks the DDL/DML text and SQLSTATE **[UNVERIFIED]** ("MiniStack uses 25006; it is probably 0A000"). §3.2 gives `54000 transaction row limit exceeded` and `54000 transaction age limit of 300s exceeded`. | DDL/DML texts come from the live-DSQL record (§3): `0A000`. |
| SAVEPOINT must be rejected | Engine accepts `SAVEPOINT`, `RELEASE`, `ROLLBACK TO` (§1). | Rule needed (Turso accepts it today). |
| "Prioritise the ones Turso accepts today (research/04 §C)" | research/04 §C is partly stale: temp tables (`0A000`) and materialized views (`42601`, MVCC) are refused by the engine (research/06 §7 already refuted both). Accepted today (§1): TRUNCATE, SAVEPOINT family, UNLOGGED, serial, unsupported column types and arrays (CREATE and ADD COLUMN), ENUM, DROP TYPE, CREATE TABLE AS, COPY TO STDOUT, SET TRANSACTION, DDL/DML discipline, row limit. | These form the P2 group (FR-020/021); the rest is SQLSTATE/text reshaping (P3). |
| Dreamescaper's "45 rules" | The rule file at the pinned commit (§3) has **48** entries under `unsupported`, plus `limits` (`dml_rows_per_txn: 3000`, `txn_age_seconds: 1800`). | Age limit stays 300 s (research/01, quotas page). M3-family rules are deferred (§5 D9). |
| SET whitelist values are "no-ops" | `search_path`, `application_name` and `client_encoding` are real engine settings (`SHOW` works; `search_path` changes name resolution). | These three are forwarded to the engine after the whitelist check; the others are remembered by the policy layer. |
| "SET SESSION CHARACTERISTICS … REPEATABLE READ" included | Dreamescaper's rule refuses `SET SESSION CHARACTERISTICS` with the `"TRANSACTION"` message, by analogy; no live record exists. Live DSQL refuses every `SET TRANSACTION`, including `REPEATABLE READ`. | Accepted per brief (spec Assumptions); owner follow-up to verify on a live cluster. |
| How statements reach the engine | Simple protocol: `run_simple_query` → parse whole text → `split_statements` → per statement `run_simple_statement` on the blocking pool (intercept → COPY → prepare/run). Extended: `PgStatement::analyze` at Parse (intercept computed once) → `do_query` at Execute (`active_intercept` skipped when Aborted) → prepare/bind/run. Errors: `finish_query` downcasts `turso_pg::Error` (→ `shape_error`) or the server's own `SqlError`. Transaction status: `PgConnection::transaction_state()` (`Idle`/`Active`/`Aborted`); refusal inside a block uses `abort_transaction_block_on_error()`. | One policy entry point per statement, called from both paths at execution time; one completion hook after it; one shaping hook in `finish_query`. |

## 3. Live-DSQL message texts (Dreamescaper golden probes)

Source: `github.com/Dreamescaper/dsql-emulator` at commit
`38a33bf283d7670b30dc959e8f38d0cf4eef1ddc`, `test/conformance/golden/*.json` (recorded against
`aurora-dsql`, 2026-09-18) and `rules/dsql-2026.09.yaml`. Referenced, not vendored (constitution,
Repository Constraints). The texts used by the rule catalogue are copied into
[contracts/rule-catalogue.md](contracts/rule-catalogue.md) with their source column.

Recorded observations that matter for M2 (live cluster):

- `SAVEPOINT x` → `0A000 unsupported transaction statement: SAVEPOINT`; `ROLLBACK TO SAVEPOINT x`
  → `… ROLLBACK TO`.
- Two DDLs → `0A000 multiple ddl statements not supported in a transaction`; DDL then DML →
  `0A000 ddl and dml are not supported in the same transaction`. The same errors are raised for a
  **multi-statement simple query** outside a transaction (`CREATE …; INSERT …`), i.e. DSQL treats
  the query string as one implicit transaction.
- `INSERT … SELECT generate_series(1, 3001)` in a transaction **and in autocommit** → `54000
  transaction row limit exceeded`; the transaction is aborted (`25P02` next), rows discarded;
  3,000 rows succeed.
- `TRUNCATE` → `0A000 unsupported statement: Truncate`; inside a transaction the next statement
  gets `25P02`; outside, the next statement runs; in an aborted transaction, `25P02` wins.
- `SET TRANSACTION ISOLATION LEVEL REPEATABLE READ` and `SET TRANSACTION READ ONLY` → `0A000
  setting configuration parameter "TRANSACTION" not supported`; `SET default_transaction_isolation
  = …` → `0A000 setting configuration parameter "default_transaction_isolation" not supported`;
  `BEGIN ISOLATION LEVEL SERIALIZABLE` → `0A000 Unsupported isolation level: SERIALIZABLE`.
- `SHOW lc_collate` → `42704 unrecognized configuration parameter "lc_collate"`.
- Column types: `datatype money not supported`, `datatype integer[] not supported`, `datatype bit
  not supported` (for `bit(8)`), `datatype bit varying not supported`; `serial` → `42704 type
  "serial" does not exist`.
- `CREATE TABLE … AS`, `CREATE MATERIALIZED VIEW` → `unsupported statement: CreateTableAs`;
  `CREATE EXTENSION` → `unsupported statement: CreateExtension`; `CREATE TRIGGER` → `CREATE TRIGGER
  not supported`; enum → `CREATE TYPE not supported`; `CREATE TEMPORARY TABLE` → `TEMPORARY or
  TEMP table not supported for CREATE TABLE`; `UNLOGGED` → `UNLOGGED table not supported for CREATE
  TABLE`; LISTEN/NOTIFY/UNLISTEN → `unsupported statement: Listen` / `Notify` / `Unlisten`;
  `VACUUM` → `unsupported VACUUM statement`; `CREATE FUNCTION … plpgsql` → `CREATE FUNCTION with
  language plpgsql not supported`.
- `server_version` is now `16.15` on DSQL (micro-dsql reports `16.13`; out of scope).

## 4. MiniStack reference deviations relevant to M2

Already listed in `tests/psql/README.md`: `25006` for discipline and row-limit errors, only
`VALUES` rows counted, bare numeric stays `numeric`. Found here in addition: MiniStack's texts
differ from live DSQL for nearly every deny rule (`TRUNCATE is not supported` vs `unsupported
statement: Truncate`; `type "serial" is not supported` (`0A000`) vs `type "serial" does not exist`
(`42704`)); MiniStack accepts `pg_advisory_lock`, `SET lock_timeout`,
`SET idle_in_transaction_session_timeout` and `CREATE TEMP VIEW`, and refuses `CREATE DOMAIN`.

## 5. Decisions

**D1 — Message source precedence.** Decision: live DSQL record > Dreamescaper rule file >
research/01 > MiniStack; when none exists, a micro-dsql text in MiniStack's style. Rationale:
M2's purpose is production fidelity and the Rust connector parses texts. Alternative: MiniStack
texts (byte-identical probes) — rejected because it would bake known MiniStack deviations into the
emulator. Consequence: probes keep differing on message lines; progress is measured on outcome and
SQLSTATE (spec SC-003). The catalogue is one data table, so switching sources is a one-file change.

**D2 — Where rules run.** Decision: one policy entry point per statement, evaluated at execution
time for both protocols, after the "aborted transaction" check. Rationale: transaction state
decides several rules; Parse-time state is wrong for the extended protocol. The stateless part
(parse, deny rules, rewrites, SET classification) is computed once per statement text and cached
in `PgStatement` for the extended protocol.

**D3 — Interface to the engine.** Decision: the policy crate receives plain values only:
`TxnStatus` (`Idle`/`Active`/`Aborted`, mirrored from `TransactionState`), `rows_changed: u64`,
`now: Instant`, and an `EngineFailure` class. Rationale: Principle III; unit tests build these by
hand.

**D4 — Transaction boundaries.** Decision: the session policy learns transaction boundaries by
comparing the engine status before and after each statement (Idle → Active starts a transaction and
records `began_at`; anything → Idle clears it), not by interpreting BEGIN/COMMIT itself.
Rationale: the engine is the authority (COMMIT of an aborted block, implicit rollback on close,
errors). Alternative: track TransactionStmt nodes — rejected, duplicates engine logic.

**D5 — Row limit enforcement.** Decision: in an explicit transaction, after each statement add the
engine's change count; if the total exceeds the limit, abort the block and return `54000`. In
autocommit, refuse `INSERT … VALUES` with more than 3,000 rows before running. Rationale: the only
correct pre-commit point the server has without changing autocommit execution (M4). Alternative:
wrap autocommit DML in BEGIN/COMMIT — rejected for M2 because it changes autocommit concurrency
(M4 item 1) and would remove 55P03 as a side effect outside M4's scope.

**D6 — Age limit.** Decision: checked at admission of every statement in an Active transaction;
for COMMIT the server runs ROLLBACK, then reports `54000`; otherwise it aborts the block. Limits
come from `PolicyLimits` (default 3,000 rows / 300 s) passed to `Server::bind_with`; tests use a
short age. Rationale: deterministic integration tests without waiting 300 s.

**D7 — Multi-statement simple query.** Decision: before running a simple query with more than one
statement while Idle, simulate the discipline rules (FR-010/011) over the statement list with a
scratch transaction state (BEGIN/COMMIT/ROLLBACK inside the string reset it); refuse the whole
string on violation. Rationale: matches the live record; no execution change. Atomicity of such
strings remains a known gap (research/06 P22).

**D8 — Numeric rewrite by text splice.** Decision: find `TypeName` nodes in `ColumnDef`s of
`CreateStmt` and `AlterTableCmd(AT_AddColumn)` whose name is `numeric` (pg_catalog) with no
`typmods`, and insert `(18,6)` into the SQL text at the end of the type-name token (located from
`TypeName.location` and the token stream). Rationale: deparsing would reformat the whole statement
and could change what the engine accepts. Error positions are reported against the client's
original text (FR-041). This text-edit mechanism (`rewrite::TextEdit`) is the seam M3 reuses for
identity desugaring, sequence `CACHE` and `NOT VALID`.

**D9 — Deferred rules (M3).** `CREATE INDEX` without `ASYNC` (`unsupported mode. please use CREATE
INDEX ASYNC.`), identity type and CACHE rules, sequence CACHE rule, ALTER TABLE ADD CONSTRAINT /
NOT VALID / VALIDATE, ADD COLUMN with constraint, ALTER COLUMN TYPE, text-search and geometric
functions. Rationale: they need the M3 pre-parse rewrite (ASYNC is not parseable today) or belong
to M3's DDL work; refusing sync `CREATE INDEX` now would break every schema with an index.

**D10 — Settings storage.** Decision: `search_path`, `application_name`, `client_encoding` pass the
whitelist check and go to the engine (it implements them); `timezone` keeps the existing UTC-only
intercept; all other whitelisted names are answered by the policy layer from `SessionSettings`
(`SET`, `SET LOCAL`, `RESET`, `SHOW`). Rationale: forwarding preserves real effects; remembering
the rest fixes `SHOW` (§1).

**D11 — Out-of-scope known-gap entries.** B1/B2/B3, B14 and DEALLOCATE stay in
`docs/known-gaps.md` unchanged; this plan retags their milestone as "later" (not M2). The docs
entries are not edited by 005 (owner decision).
