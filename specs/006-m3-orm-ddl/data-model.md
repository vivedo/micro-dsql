# Data model: M3 — DDL that ORMs and migrations emit

Types are in `crates/policy` unless marked (server). Field lists are normative for meaning;
names may change in implementation. See [contracts/policy-api-m3.md](contracts/policy-api-m3.md)
for the signatures.

## PreParsed (policy, `rewrite.rs`)

| Field | Meaning |
|---|---|
| `text` | the statement with the DSQL `ASYNC` keyword blanked (same byte length as the original) |
| `async_kind` | `None`, `Index` (after `CREATE [UNIQUE] INDEX`), `AlterTable` (after `ALTER TABLE`) |

Rule: only a keyword token `ASYNC` (case-insensitive, unquoted) in exactly those positions; at most
one per statement. `CREATE INDEX async ON …` (`async` followed by `ON`) is an index name, not the
keyword.

## TextEdit (policy, `rewrite.rs`)

`{ at: usize, remove: usize, insert: String }` — bytes of the original text. Edits never overlap;
`apply_edits` rejects (debug-asserts) overlapping edits. 005's inserts are `remove = 0`.

## StatementPlan additions (policy, `statement.rs`)

| Field | Set by | Used by |
|---|---|---|
| `async_kind` | pre-parse | rules `index_sync`, `validate_sync`; `JobAction` |
| `prelude: Vec<String>` | identity desugaring | server `ddl.rs`, before `engine_sql` |
| `cleanup_on_failure: Vec<String>` | identity desugaring | server, when `engine_sql` fails while Idle |
| `job: Option<JobAction>` | step 8 | server `sys.rs`; Describe |
| `ddl: Option<DdlAction>` | step 8 | server `ddl.rs` / `constraints.rs` |

Invariant: `refusal.is_some()` ⇒ `job`, `ddl`, `prelude` are empty (a refused statement never
reaches the server's dispatch).

## Job (engine table `sys.jobs`, written by the server)

| Column | Type (declared) | Value |
|---|---|---|
| `job_id` | `text` PK | 26 chars `[a-z2-7]`, from 128 random bits |
| `status` | `text` | `completed` or `failed` (never `submitted`/`processing`) |
| `details` | `text` | NULL when completed; PostgreSQL-style violation text when failed |
| `job_type` | `text` | `INDEX_BUILD` or `VALIDATE_CONSTRAINT` |
| `class_id` | `oid` | `1259` (`pg_class`) |
| `object_id` | `oid` | `pg_class.oid` of the index (INDEX_BUILD) or of the table (VALIDATE) |
| `object_name` | `text` | `<schema>.<index>` or `<schema>.<table>` |
| `start_time` | `timestamptz` | statement time, UTC |
| `update_time` | `timestamptz` | same as `start_time` |

Lifecycle: inserted by the statement that created it, in the session's transaction (rollback
removes it); purged (finished and `update_time` older than 30 minutes) before each new insert.
Read by any session. Never written by clients (`sys_write`, `sys_ddl` rules).

## Identity sequence (engine sequence, created by the server)

Name: the identity's `sequence name` option, else `<table>_<column>_seq`; schema: the table's.
Options carried: `INCREMENT BY`, `MINVALUE`, `MAXVALUE`, `START WITH`, `CYCLE`/`NO CYCLE`; `CACHE`
always `1`. Column default: `nextval('<schema>.<seq>')`. Dropped after `DROP TABLE` of its table
when the table's `pg_attrdef` default is `nextval('<schema>.<table>_<column>_seq')`.

## NOT VALID constraint (engine schema, server `constraints.rs`)

| Kind | Represented in the engine as | Checked |
|---|---|---|
| single-column FOREIGN KEY with ON DELETE/ON UPDATE actions | column constraint `REFERENCES t(c)` added via the core `ALTER COLUMN` path, named as given | rows written after the ALTER |
| CHECK on one column | column constraint `CHECK (expr)` added the same way, named as given | rows written after the ALTER |

State: "not validated" → `VALIDATE` job `completed` → "validated". On micro-dsql the state is
tracked only through `pg_constraint.convalidated` if the engine can store it (FR-022), else not
at all (known gap); a VALIDATE on an already valid constraint succeeds.

## ConformanceReport (file, `reports/conformance-<date>.md`)

Pinned commit, target (`host:port`), server version, UTC time, total pass/total/percent, per-group
pass/total, failing cases (case, field, step SQL, golden, observed), advisory-note count.
