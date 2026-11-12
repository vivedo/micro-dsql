# Contract: M3 rule catalogue additions

Extends [005's rule catalogue](../../005-m2-dsql-rules/contracts/rule-catalogue.md) (same columns,
same conventions: messages byte-exact, severity `ERROR`, one integration test per row in
`crates/server/tests/smoke/dsql_ddl.rs`). **Source**: **L** live DSQL (golden record at
`38a33bf`), **G** user guide (research/sources), **D** Dreamescaper emulator, **M** MiniStack
probe reference, **P** PostgreSQL text, **µ** micro-dsql text. **Engine today**: A = accepted,
else SQLSTATE.

Rules are stateless (AST only) and run in pipeline step 4, after 005's rules; the first match wins.
Order inside this table is the evaluation order.

## 1. Index DDL (`ddl_rules.rs`)

| Id | Matches | SQLSTATE | Message | Source | Engine today |
|---|---|---|---|---|---|
| `index_concurrently` | `IndexStmt` with `concurrent` | 0A000 | `CONCURRENTLY not supported for CREATE INDEX` | M | A |
| `index_sync` | `IndexStmt` whose text had no `ASYNC` (plan `async_kind != Index`) | 0A000 | `unsupported mode. please use CREATE INDEX ASYNC.` | L | A |
| `index_method` | `IndexStmt` with `access_method` other than `btree` (case-insensitive; `btree` and no `USING` pass) | 0A000 | `USING not supported for CREATE INDEX` | M | A / engine error |
| `index_key_columns` | `IndexStmt` with more than 8 `index_params` | 54011 | `more than 8 column keys in an index are not supported` | M | A |

`index_sync` needs the pre-parse result, so `check_rules` receives the plan's `async_kind`
(contracts/policy-api-m3.md §2).

## 2. Identity and sequences (`ddl_rules.rs`)

| Id | Matches | SQLSTATE | Message | Source | Engine today |
|---|---|---|---|---|---|
| `identity_type` | `CONSTR_IDENTITY` on a column whose type is not `int8`/`bigint` | 0A000 | `datatype <type> not supported, identity column type must be bigint` (`<type>` as PostgreSQL displays it: `integer`, `smallint`) | L | 0A000 (other text) |
| `identity_cache_missing` | `CONSTR_IDENTITY` without a `cache` option | 0A000 | `identity column is not supported without an explicit cache size. please define CACHE greater than or equal to 65536 or equal to 1` | L | 0A000 (other text) |
| `sequence_cache_missing` | `CreateSeqStmt` without a `cache` option | 0A000 | `CREATE SEQUENCE is not supported without an explicit cache size. please define CACHE greater than or equal to 65536 or equal to 1` | L | A |
| `cache_value` | `cache` option (identity, `CreateSeqStmt`, `AlterSeqStmt`) with value 2..65535 (or < 1) | 0A000 | `CACHE must be 1 or at least 65536` | M | 0A000 (other text) / A for CACHE 1 |

## 3. ALTER TABLE (`ddl_rules.rs`)

| Id | Matches | SQLSTATE | Message | Source | Engine today |
|---|---|---|---|---|---|
| `add_column_constraint` | `AT_AddColumn` whose `ColumnDef` has any constraint (DEFAULT, NOT NULL, NULL, CHECK, UNIQUE, PRIMARY KEY, REFERENCES, GENERATED, IDENTITY) or `raw_default` | 0A000 | `ALTER TABLE ADD COLUMN with constraint not supported` | L | A (DEFAULT) / 42000 |
| `alter_column_type` | `AT_AlterColumnType` | 0A000 | `unsupported ALTER TABLE ALTER COLUMN ... SET DATA TYPE statement` | L | 0A000 (other text) |
| `set_not_null` | `AT_SetNotNull` | 0A000 | `SET NOT NULL is not supported (only DROP NOT NULL)` | M | 42601 |
| `drop_pk_column` | `AT_DropColumn` naming a column that is part of the table's primary key | 0A000 | `cannot drop primary key column <name>` | L | 42601 |
| `add_constraint` | `AT_AddConstraint` that is not (FOREIGN KEY or CHECK) with `skip_validation` (NOT VALID) | 0A000 | `unsupported ALTER TABLE ADD CONSTRAINT statement` | L | 42601 |
| `add_constraint_form` | `AT_AddConstraint` FK/CHECK NOT VALID that the engine cannot enforce: FK with more than one column, `DEFERRABLE`/`INITIALLY DEFERRED`, `MATCH FULL`; CHECK referencing more than one column or none | 0A000 | `unsupported NOT VALID constraint form: <form>` (`<form>`: `multi-column foreign key`, `deferrable foreign key`, `MATCH FULL foreign key`, `multi-column check`) | µ | 42601 |
| `validate_sync` | `AT_ValidateConstraint` whose text had no `ASYNC` | 0A000 | `unsupported ALTER TABLE VALIDATE CONSTRAINT statement` | L | 42601 |

`drop_pk_column` cannot see the table definition from the AST alone. It is therefore evaluated by
the server (`constraints.rs`) from the engine catalog before the statement runs, and its error is
built by `errors::drop_pk_column(name)` (policy-owned text). It has no row in `RULES`.

## 4. `sys` schema (`ddl_rules.rs`, `jobs.rs`)

| Id | Matches | SQLSTATE | Message | Source | Engine today |
|---|---|---|---|---|---|
| `wait_for_job_select` | `FuncCall` `sys.wait_for_job` anywhere in a non-CALL statement | 42809 | `sys.wait_for_job(text) is a procedure` | L | 3F000 |
| `wait_for_job_subquery` | `CallStmt` `sys.wait_for_job` whose argument contains a `SubLink` | 0A000 | `cannot use subquery in CALL argument` | L | 42601 |
| `wait_for_job_id` | `CallStmt` `sys.wait_for_job` with a constant argument not matching `^[a-z2-7]{26}$` (checked at execution for `$n` parameters) | 22P02 | `Unable to convert text to UUID` | L | 42601 |
| `wait_for_job_unknown` | well-formed id not in `sys.jobs` (server, at execution) | 22023 | `job <id> not found` | µ (D: `unknown job <id>`) | — |
| `sys_write` | `InsertStmt`/`UpdateStmt`/`DeleteStmt` on a `sys.` relation | 42501 | `permission denied for table <name>` | P | A |
| `sys_ddl` | DDL whose target relation or schema is `sys` (incl. `CREATE SCHEMA sys`, `DROP SCHEMA sys`, `CREATE TABLE sys.x`, `DROP TABLE sys.jobs`) | 42501 | `permission denied for schema sys` | P | A |

## 5. Rewrites (`rewrite.rs`, `identity.rs`)

| Id | Matches | Effect on the engine text |
|---|---|---|
| `async_strip` (pre-parse) | keyword `ASYNC` right after `CREATE [UNIQUE] INDEX` or `ALTER TABLE` | five bytes replaced by spaces; plan records `async_kind` |
| `cache_one` | `cache` option ≥ 65536 in `CreateSeqStmt`, `AlterSeqStmt`, identity options | value replaced by `1` |
| `identity` | `bigint` identity column in `CreateStmt` (after the rules passed) | prelude `CREATE SEQUENCE <schema>.<seq> … CACHE 1`; constraint span replaced by `DEFAULT nextval('<schema>.<seq>')`; cleanup `DROP SEQUENCE IF EXISTS <schema>.<seq>` |
| `not_valid` | `AT_AddConstraint` FK/CHECK NOT VALID | not sent to the engine as text; plan carries `DdlAction::AddConstraintNotValid` for the server |
| `validate_async` | `AT_ValidateConstraint` with `ASYNC` | not sent to the engine; plan carries `JobAction::Validate` |

## 6. Statements answered by micro-dsql

| Statement | Answer |
|---|---|
| `CREATE [UNIQUE] INDEX ASYNC …` (rules passed) | engine builds the index; tag `CREATE INDEX`; one row `job_id text` |
| `ALTER TABLE ASYNC t VALIDATE CONSTRAINT c` | tag `ALTER TABLE`; one row `job_id text`; job `completed` or `failed` |
| `CALL sys.wait_for_job('<id>' \| $n)` | tag `CALL`; one row `succeeded bool` (`t` completed, `f` failed) |
| `SELECT sys.dsql_major_version()` [`AS x`] | tag `SELECT 1`; one row, column `dsql_major_version` (or alias) `int4` `1` |
| `SELECT * FROM sys.dsql_major_version()` | same |
| `ALTER TABLE t VALIDATE CONSTRAINT c` on an unknown constraint (ASYNC) | 42704 `constraint "<c>" of relation "<t>" does not exist` (P) |
