# Contract: policy API and server seams — M3 changes

Delta on [005 contracts/policy-api.md](../../005-m2-dsql-rules/contracts/policy-api.md). Names are
normative for 006; signatures are indicative. Everything in `crates/policy` keeps `pg_query` as its
only dependency (Principle III): no engine type appears below.

## 1. Module map

| Module | Status | Owns |
|---|---|---|
| `rewrite.rs` | changed | `pre_parse(sql) -> PreParsed { text: Cow<str>, async_kind: Option<AsyncKind> }` (D1, same-length blanking); `TextEdit { at, remove, insert }`; `apply_edits` handles replacements; `REWRITES` gains `cache_one` |
| `identity.rs` | new | identity desugaring: `desugar(&CreateStmt, sql) -> IdentityPlan { edits, prelude, cleanup }`; sequence naming |
| `ddl_rules.rs` | new | the M3 rule functions (rule-catalogue-m3.md §1–4); registered as rows of `rules::RULES` |
| `jobs.rs` | new | `encode_job_id(u128) -> String`, `is_job_id(&str) -> bool`, recognition of `CALL sys.wait_for_job`, `sys.dsql_major_version()` forms, `JobAction`, `DdlAction` |
| `statement.rs` | changed | `StatementPlan` new fields (§2); pipeline steps 1 and 7 extended |
| `session.rs` | changed (additive) | `Reply::Rows`, `ReplyColumn`, `ColumnType`; no change to `TxnState`, `admit` order or `complete` |
| `rules.rs` | changed (additive) | `check_rules(node, sql, ctx: RuleContext)` where `RuleContext { async_kind }`; new rows only |
| `errors.rs` | changed (additive) | constructors for every M3 text |
| `classify.rs` | changed (additive) | `CallStmt` → `Other` (not DDL/DML); `IndexStmt` stays DDL |

## 2. Statement pipeline (changes in bold)

```text
client text
  │ 1. rewrite::pre_parse(sql) -> PreParsed            **blank ASYNC, record async_kind**
  │ 2. pg_query::parse(pre.text)                       parse error → parse_failed (engine reports it, on the blanked text)
  │ 3. classify
  │ 4. rules::check_rules(&ast, text, ctx)             **+ M3 rules (index, identity/cache, ALTER TABLE, sys)**
  │ 5. settings, 6. intercept                          unchanged
  │ 7. rewrite::ast_rewrites + identity::desugar       **cache_one; identity → prelude + edits + cleanup**
  │ 8. static facts                                    **+ job: Option<JobAction>, ddl: Option<DdlAction>, reply_columns**
  ▼
StatementPlan {
    original, engine_sql, parse_failed, class, refusal, setting, intercept, values_rows,   // 005
    async_kind: Option<AsyncKind>,          // Index | AlterTable
    prelude: Vec<String>,                   // engine statements before engine_sql (same txn)
    cleanup_on_failure: Vec<String>,        // run if engine_sql fails while Idle
    job: Option<JobAction>,                 // see below
    ddl: Option<DdlAction>,                 // see below
}

enum JobAction {
    IndexBuild { schema: Option<String>, index: Option<String>, table: String, unique: bool, if_not_exists: bool },
    Validate { schema: Option<String>, table: String, constraint: String },
    WaitForJob { id: JobIdSource },         // Const(String) | Param(usize)
    MajorVersion { column: String },
}
enum DdlAction {
    AddConstraintNotValid { schema: Option<String>, table: String, name: String, kind: NotValidKind },
    DropTable { schema: Option<String>, tables: Vec<String> },   // identity-sequence cleanup
    DropColumn { schema: Option<String>, table: String, column: String }, // drop_pk_column check
}
enum NotValidKind {
    ForeignKey { column: String, ref_schema: Option<String>, ref_table: String, ref_column: Option<String>, on_delete: FkAction, on_update: FkAction },
    Check { column: String, expr_sql: String },   // expr text sliced from the original statement
}
```

`StatementPlan::reply_columns() -> Option<Vec<ReplyColumn>>` generalises 005's `show_column()`
for Describe: `job_id text` for `JobAction::{IndexBuild, Validate}`, `succeeded bool` for
`WaitForJob`, `<column> int4` for `MajorVersion`, plus today's SHOW columns.

`IndexBuild` with no index name: the server learns the engine-generated name from `pg_class`
(the newest index on `table`) for `object_name`.

## 3. Session API changes

```rust
pub enum ColumnType { Text, Bool, Int4 }
pub struct ReplyColumn { pub name: String, pub ty: ColumnType }
pub enum Reply {
    Show { column: String, value: String },                 // 005
    Command { tag: &'static str },                          // 005
    Rows { tag: String, columns: Vec<ReplyColumn>, rows: Vec<Vec<Option<String>>> }, // new
}
```

`admit` order is unchanged. New behaviour inside step (10): a plan with `job:
Some(MajorVersion)` returns `Respond(Reply::Rows …)`; every other `job`/`ddl` plan returns
`Run { sql }` as before and the server dispatches on the plan (§4). `complete`/`failed` are called
exactly as today, once per client statement (the prelude, job writes and catalog reads are not
seen by the policy), so a desugared identity CREATE TABLE counts as one DDL.

## 4. Server seams (`crates/server`)

| Seam | Where | Role |
|---|---|---|
| `sys.rs` (new) | `bootstrap(&Database)` | called from `open_database`: `CREATE SCHEMA IF NOT EXISTS sys`, `CREATE TABLE IF NOT EXISTS sys.jobs (…)` on a bootstrap connection |
| `sys.rs` | `run_job_statement(conn, plan, run_engine) -> PgWireResult<(Outcome, u64)>` | `IndexBuild`: run engine, look up oid/name, purge + insert job, `Outcome::Rows` with `job_id`; `Validate`: delegates to `constraints::validate`; `WaitForJob`: read status → `succeeded` row or 22P02/22023 |
| `constraints.rs` (new) | `add_check_not_valid(…)` (D7, interim), `rebuild_with_constraint(conn, schema, table, constraint, original) -> PgWireResult<()>` (D13, owner decision T033), `validate(conn, action) -> Job`, `check_drop_column(conn, action) -> Option<PolicyError>` | D7/D8/D13; the only module using `turso_parser::ast`, `PgConnection::inner()` and the core switches `set_foreign_keys_enabled` / `set_check_constraints_ignored` (always restored by a guard) |
| `ddl.rs` (new) | `run_ddl_statement(conn, plan, run_engine)` | prelude, `cleanup_on_failure`, `DropTable` sequence cleanup, `AddConstraintNotValid`, `DropColumn` check |
| `dsql::run_planned` | `dsql.rs` (shared with 007) | **one** added branch in the `Admission::Run` arm: `if plan.job.is_some() → sys::run_job_statement`, `else if plan.ddl.is_some() or !plan.prelude.is_empty() → ddl::run_ddl_statement`, else today's `run_engine(sql)`; `respond` gains the `Reply::Rows` arm |
| `run_simple_query` | `lib.rs` (shared) | syntax check on `pre_parse(text).text`; split the original text (scanner split accepts ASYNC) |
| `PgStatement::analyze` / `parse_sql` | `lib.rs` (shared) | parse/describe on the pre-parsed text; Describe uses `plan.reply_columns()` |
| `Outcome::Rows` | `lib.rs` | reused for job rows (header built by a helper next to `show_fields`) |

## 4a. Table rebuild (D13) seam

`rebuild_with_constraint` runs server-issued statements only (never through the policy). Inside a
client transaction it uses that transaction; when Idle it opens an internal `BEGIN`/`COMMIT` on the
session's connection and always leaves it Idle (research.md §8.2 item 4). If 007 adds a
transaction-start helper, the rebuild must use it (orchestrator seam).

## 5. What 006 does not touch (007 seams)

`TxnState` fields, `SessionPolicy::{admit order, complete, failed}` semantics, autocommit
execution, transaction start/snapshot, `LockBusy → oc000`, `FOR UPDATE`/`FOR KEY SHARE`,
`errors::{oc000, oc001, shape_engine_error}`. If 007 adds `Admission` variants or `run_planned`
branches, the two features' edits are disjoint hunks in `session.rs` and `dsql.rs`.
