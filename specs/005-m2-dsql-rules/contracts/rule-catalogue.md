# Contract: rule catalogue (SQLSTATEs and messages)

Normative for 005. Every row is one rule id in `crates/policy/src/rules.rs` (`RULES` table) or in
the transaction/settings modules, and has one integration test asserting SQLSTATE and message
(`crates/server/tests/smoke/dsql_rules.rs`). Messages are byte-exact. Severity is always `ERROR`.

**Source** column: **L** = live DSQL record (Dreamescaper golden probes, commit `38a33bf`),
**D** = Dreamescaper rule file (same commit), **R** = research/01, **M** = MiniStack reference
(tests/psql/probes/expected), **µ** = micro-dsql text (no reference). **Engine today**: what the
engine does without the rule (A = accepted, else its SQLSTATE).

Placeholders: `<name>` is the identifier as PostgreSQL normalises it; `<type>` is PostgreSQL's
display name for the type (`int4` → `integer`, `int[]` → `integer[]`, `varbit(8)` → `bit
varying`, `bit(8)` → `bit`); `<LEVEL>` is upper case (`SERIALIZABLE`).

## 1. Conflict error shape (`errors.rs`)

| Id | Engine failure (classified by the server) | SQLSTATE | Message | Source |
|---|---|---|---|---|
| `oc000` | `DataConflict`: write-write conflict, conflicting commit, stale snapshot, aborted commit dependency, generic conflict | 40001 | `change conflicts with another transaction (OC000)` | L/R |
| `oc001` | `SchemaConflict`: schema conflict, schema updated | 40001 | `schema has been updated by another transaction (OC001)` | R |
| `busy_as_oc000` | `LockBusy`: database busy, table locked (engine `55P03`) | 40001 | `change conflicts with another transaction (OC000)` | brief (interim until M4) |

Other engine errors keep today's shaping (`42P01`, `42601`, `23xxx`).

## 2. Transaction discipline and limits (`session.rs`)

| Id | Trigger | SQLSTATE | Message | Source | Engine today |
|---|---|---|---|---|---|
| `txn_two_ddl` | 2nd DDL in an explicit transaction, or in one multi-statement query string | 0A000 | `multiple ddl statements not supported in a transaction` | L | A |
| `txn_ddl_dml` | DML after DDL or DDL after DML, same scopes | 0A000 | `ddl and dml are not supported in the same transaction` | L | A |
| `txn_row_limit` | explicit-transaction row total > limit after a statement; autocommit `INSERT … VALUES` with > limit rows | 54000 | `transaction row limit exceeded` | L/R | A |
| `txn_age_limit` | any statement in a transaction older than the age limit | 54000 | `transaction age limit of 300s exceeded` | R | A |
| `savepoint` | `SAVEPOINT` | 0A000 | `unsupported transaction statement: SAVEPOINT` | L/D | A |
| `release_savepoint` | `RELEASE [SAVEPOINT]` | 0A000 | `unsupported transaction statement: RELEASE` | D | A |
| `rollback_to_savepoint` | `ROLLBACK TO [SAVEPOINT]` | 0A000 | `unsupported transaction statement: ROLLBACK TO` | L/D | A |

The age message names the configured limit in seconds (`300s` in the shipped binary).

**DDL** = `Create*Stmt` (any object, incl. `CREATE SCHEMA` with its nested elements as one),
`Alter*Stmt`, `DropStmt`, `IndexStmt`, `RenameStmt`, `CommentStmt`, `GrantStmt`, `GrantRoleStmt`,
`AlterDefaultPrivilegesStmt`, `ViewStmt`, `CreateTableAsStmt`, `CreateSeqStmt`/`AlterSeqStmt`,
`CreateFunctionStmt`/`AlterFunctionStmt`, `CreateDomainStmt`, `CreateStatsStmt`.
**DML** = `InsertStmt`, `UpdateStmt`, `DeleteStmt`, `MergeStmt`, and any statement containing one
of these in a `WITH` clause. Everything else (SELECT incl. `FOR UPDATE`, SHOW, SET, transaction
control, EXPLAIN without ANALYZE) is neither.

## 3. Statement deny rules (`rules.rs`)

### 3.1 Accepted by the engine today (P2)

| Id | Matches | SQLSTATE | Message | Source | Engine today |
|---|---|---|---|---|---|
| `truncate` | `TruncateStmt` | 0A000 | `unsupported statement: Truncate` | L/D | A |
| `unlogged_table` | `CreateStmt` with `relpersistence = 'u'` | 0A000 | `UNLOGGED table not supported for CREATE TABLE` | L/D | A |
| `create_table_as` | `CreateTableAsStmt` with `objtype = OBJECT_TABLE` (also `SELECT … INTO`) | 0A000 | `unsupported statement: CreateTableAs` | L/D | A |
| `enum_type` | `CreateEnumStmt` | 0A000 | `CREATE TYPE not supported` | L/D | A |
| `alter_enum` | `AlterEnumStmt` | 0A000 | `ALTER TYPE not supported` | D | — |
| `drop_type` | `DropStmt` with `remove_type = OBJECT_TYPE` | 0A000 | `unsupported object in DROP statement` | D | A |
| `rename_type` | `RenameStmt` with `rename_type = OBJECT_TYPE` | 0A000 | `unsupported object in RENAME statement` | D | — |
| `copy` | `CopyStmt` (any direction) | 0A000 | `COPY is not supported` | M | A (TO STDOUT) / 42601 (FROM STDIN) |
| `serial` | column type `serial`/`serial4`/`bigserial`/`serial8`/`smallserial`/`serial2` in `CreateStmt` or `ALTER TABLE … ADD COLUMN` | 42704 | `type "<name as written>" does not exist` | L/D | A |
| `unsupported_type` | column type in `money`, `xml`, `inet`, `cidr`, `macaddr`, `macaddr8`, `point`, `line`, `lseg`, `box`, `path`, `polygon`, `circle`, `tsvector`, `tsquery`, `int4range`, `int8range`, `numrange`, `tsrange`, `tstzrange`, `daterange`, `bit`, `varbit`, `pg_lsn` (same statements) | 0A000 | `datatype <type> not supported` | L/D | A (except ranges, `bit(n)`) |
| `array_column` | column type with array bounds (same statements) | 0A000 | `datatype <element type>[] not supported` | L/D | A |

### 3.2 Refused by the engine today with another SQLSTATE or text (P3)

| Id | Matches | SQLSTATE | Message | Source | Engine today |
|---|---|---|---|---|---|
| `temporary_table` | `CreateStmt` with `relpersistence = 't'` | 0A000 | `TEMPORARY or TEMP table not supported for CREATE TABLE` | L/D | 0A000 (other text) |
| `extensions` | `CreateExtensionStmt` | 0A000 | `unsupported statement: CreateExtension` | L/D | 42601 |
| `triggers` | `CreateTrigStmt` | 0A000 | `CREATE TRIGGER not supported` | L/D | 42601 |
| `create_database` | `CreatedbStmt` | 0A000 | `unsupported statement: Createdb` | L/D | 42601 |
| `materialized_view` | `CreateTableAsStmt` with `objtype = OBJECT_MATVIEW` | 0A000 | `unsupported statement: CreateTableAs` | L/D | 42601 |
| `composite_type` | `CompositeTypeStmt` | 0A000 | `unsupported statement: CompositeType` | L/D | 42601 |
| `range_type` | `CreateRangeStmt` | 0A000 | `unsupported statement: CreateRange` | L/D | 42601 |
| `tablespace` | `CreateTableSpaceStmt` | 0A000 | `unsupported statement: CreateTableSpace` | L/D | 42601 |
| `foreign_table` | `CreateForeignTableStmt` | 0A000 | `unsupported statement: CreateForeignTable` | L/D | 42601 |
| `vacuum` | `VacuumStmt` that is a VACUUM (not a bare ANALYZE) | 0A000 | `unsupported VACUUM statement` | L/D | 42601 |
| `listen` / `notify` / `unlisten` | `ListenStmt` / `NotifyStmt` / `UnlistenStmt` | 0A000 | `unsupported statement: Listen` / `Notify` / `Unlisten` | L/D | 42601 |
| `alter_system` | `AlterSystemStmt` | 0A000 | `unsupported statement: AlterSystem` | L/D | 42601 |
| `merge` | `MergeStmt` | 0A000 | `unsupported statement type: MERGE` | D | 42601 |
| `create_function_language` | `CreateFunctionStmt` whose `LANGUAGE` option is not `sql` | 0A000 | `CREATE FUNCTION with language <language> not supported` | L/D | 42601 |
| `unsupported_locking` | locking clause `FOR SHARE` / `FOR NO KEY UPDATE` anywhere in a SELECT | 0A000 | `locking clauses other than FOR UPDATE/FOR KEY SHARE are not supported` | D/M | 0A000 (other text) |
| `tablesample` | `RangeTableSample` anywhere | 0A000 | `TABLESAMPLE clause not supported` | D | 42601 |
| `lock_table` | `LockStmt` | 0A000 | `LOCK TABLE is not supported` | M | A (ACCESS SHARE) / 42601 |
| `do` | `DoStmt` | 0A000 | `DO is not supported` | M | 42601 |
| `create_procedure` | `CreateFunctionStmt` with `is_procedure` | 0A000 | `CREATE PROCEDURE is not supported` | M | 42601 |
| `create_rule` | `RuleStmt` | 0A000 | `CREATE RULE is not supported` | M | 42601 |
| `create_publication` | `CreatePublicationStmt` | 0A000 | `CREATE PUBLICATION is not supported` | M | 42601 |
| `reindex` | `ReindexStmt` | 0A000 | `REINDEX is not supported` | M | 42601 |
| `cluster` | `ClusterStmt` | 0A000 | `CLUSTER is not supported` | M | 42601 |
| `prepare_transaction` | `TransactionStmt` kind `PREPARE` | 0A000 | `prepared transactions is not supported` | M | 42601 |
| `advisory_locks` | `FuncCall` named `pg_advisory_*`, `pg_try_advisory_*` (any schema) | 0A000 | `advisory locks are not supported` | µ | 42883 |

Order: `create_procedure` before `create_function_language`; `materialized_view` before
`create_table_as`; the first matching rule wins. Rules never fire on text that does not parse.

### 3.3 Explicitly not refused in 005

`CREATE DOMAIN`; `CREATE INDEX` with or without `ASYNC`; `CREATE TEMP VIEW` (engine refuses it
already); `PARTITION BY` (engine already answers `0A000 table partitioning is not supported`);
`EXCLUDE` constraints (engine `0A000`); ALTER TABLE forms not listed above; identity/sequence CACHE;
`SELECT … FOR UPDATE` / `FOR KEY SHARE`; arrays and `inet` in expressions; `CREATE FUNCTION …
LANGUAGE sql`; `DECLARE CURSOR` (engine refuses). The M3-family rules are listed in research.md D9.

## 4. Session settings (`settings.rs`)

Whitelist (case-insensitive): `application_name`, `client_encoding`, `datestyle`,
`extra_float_digits`, `intervalstyle`, `timezone` (alias `time zone`), `search_path`,
`enable_bitmapscan`, `enable_hashjoin`, `enable_indexonlyscan`, `enable_indexscan`,
`enable_material`, `enable_mergejoin`, `enable_nestloop`, `enable_seqscan`,
`dsql.enable_batched_nestloop`, `role` (`SET LOCAL` in a transaction only).

| Id | Trigger | SQLSTATE | Message | DETAIL | Source |
|---|---|---|---|---|---|
| `set_not_supported` | `SET [SESSION\|LOCAL] <name>`, `RESET <name>`, `set_config('<name>', …)` (constant name) for a name not on the whitelist; `SET`/`RESET role` outside `SET LOCAL` | 0A000 | `setting configuration parameter "<name>" not supported` | — | L/R |
| `set_transaction` | `SET TRANSACTION …` (any mode) | 0A000 | `setting configuration parameter "TRANSACTION" not supported` | — | L |
| `session_characteristics_level` | `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL` other than `REPEATABLE READ` | 0A000 | `Unsupported isolation level: <LEVEL>` | — | L (BEGIN form) |
| `session_characteristics_mode` | `SET SESSION CHARACTERISTICS AS TRANSACTION` with `READ ONLY`, `DEFERRABLE` or `NOT DEFERRABLE` | 0A000 | `setting configuration parameter "TRANSACTION" not supported` | — | D |
| `dsql_reserved_prefix` | unknown `dsql.*` name in `SET`/`RESET`/`set_config` | 42602 | `invalid configuration parameter name "<name>"` | `"dsql" is a reserved prefix.` | M (PG text) |
| `set_bool_value` | non-boolean value for `enable_*` / `dsql.enable_batched_nestloop` | 22023 | `parameter "<name>" requires a Boolean value` | — | M (PG text) |
| `set_one_value` | value list for a whitelisted name other than `search_path`, `datestyle` | 22023 | `SET <name> takes only one argument` | — | M (PG text) |
| `show_unrecognized` | `SHOW lc_collate`, `SHOW lc_ctype` | 42704 | `unrecognized configuration parameter "<name>"` | — | L |

Accepted forms and answers:

| Statement | Answer |
|---|---|
| `SET <remembered name> {=\|TO} <value>` | tag `SET`; value stored (booleans normalised to `on`/`off`) |
| `SET LOCAL <remembered name> …` in a transaction | tag `SET`; value visible until the transaction ends |
| `SET LOCAL …` outside a transaction | tag `SET`, no effect (PostgreSQL warns; no warning is sent) |
| `SET <name> TO DEFAULT`, `RESET <name>` | tag `SET` / `RESET`; default restored |
| `RESET ALL` | tag `RESET`; every remembered value and the engine settings restored |
| `SHOW <remembered name>` | one row, column named as PostgreSQL does (`DateStyle`, `IntervalStyle`, `extra_float_digits`, `enable_seqscan`, `dsql.enable_batched_nestloop`, …), current value |
| `SET search_path …`, `SET application_name …`, `SET client_encoding …` (and their `RESET`/`SHOW`) | forwarded to the engine unchanged |
| `SET timezone …` | today's intercept (UTC spellings accepted, others `22023`) |
| `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ [, READ WRITE]` | tag `SET`, no effect |
| `SET LOCAL role <x>` in a transaction | tag `SET`; remembered for `SHOW role` until the transaction ends |

Defaults: `DateStyle` `ISO, MDY`; `IntervalStyle` `postgres`; `extra_float_digits` `1`;
`enable_*` `on`; `dsql.enable_batched_nestloop` `on`; `role` `none`.

## 5. DDL rewrite (`rewrite.rs`)

| Id | Matches | Effect |
|---|---|---|
| `bare_numeric` | `ColumnDef` type `numeric`/`decimal` without type modifiers in `CreateStmt` or `ALTER TABLE … ADD COLUMN` | the statement text runs with `(18,6)` inserted after the type name |
