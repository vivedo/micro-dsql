# Contract: policy crate API and the server seams

Internal contract between `crates/policy` (backend-agnostic, Principle III) and `crates/server`.
Names here are normative: later specs (M3 = 006, M4 = 007) build on them. Signatures are
indicative; the implementation may adjust argument types without changing the roles.

## 1. Module map (`crates/policy/src/`)

| Module | Owns | New in 005 |
|---|---|---|
| `errors.rs` | **The single error-shaping module.** `PolicyError`, `EngineFailure`, constructors for every DSQL error text (`oc000()`, `oc001()`, `row_limit()`, `age_limit(secs)`, …), `shape_engine_error()`, and today's PostgreSQL-style `shape_error()` | extended |
| `statement.rs` | `StatementPlan`: everything known about one statement text without session state (pipeline §2) | new |
| `classify.rs` | `StatementClass`, `TxnControl`: DDL / DML / read / transaction control / session command | new |
| `rules.rs` | `Rule`, the `RULES` table and `check_rules()`: stateless deny rules on the AST | new |
| `types.rs` | column-type checks and PostgreSQL display names for types (`integer[]`, `bit varying`) | new |
| `rewrite.rs` | `pre_parse()` (identity in 005), `TextEdit`, `apply_edits()`, `ast_rewrites()` with `REWRITES` (005: `bare_numeric`) | new |
| `settings.rs` | the whitelist, `SettingCommand` (parsed SET/RESET/SHOW/set_config), `SessionSettings` (remembered values, `SET LOCAL` overlay) | new |
| `session.rs` | `SessionPolicy` (per connection: `SessionSettings`, `TxnState`, `PolicyLimits`), `TxnStatus`, `Admission`, `Reply`, `admit()`, `complete()`, `check_batch()` | new |
| `intercept.rs` | today's stateless intercepts (`SHOW` of fixed server parameters, `DEALLOCATE`, `SET timezone`); called from `StatementPlan` | kept |
| `params.rs`, `server_params.rs` | unchanged (`lc_collate`/`lc_ctype` removed from `SHOW_PARAMETERS`) | touched |

The crate's only dependency stays `pg_query`. No `turso_*` type appears in any signature.

## 2. Statement pipeline (`StatementPlan::new(sql)`)

```text
client text
  │ 1. rewrite::pre_parse(sql) -> Cow<str>        005: identity. M3: strip ASYNC, …
  │ 2. pg_query::parse                             parse error → Plan { parse_failed: true } (engine reports it)
  │ 3. classify::classify(&ast) -> StatementClass
  │ 4. rules::check_rules(&ast) -> Option<PolicyError>         first matching rule
  │ 5. settings::SettingCommand::from_ast(&ast)               SET/RESET/SHOW/set_config/SET TRANSACTION
  │ 6. intercept::intercept(...)                               fixed SHOW, DEALLOCATE, SET timezone
  │ 7. rewrite::ast_rewrites(&ast, text) -> Vec<TextEdit>      005: bare_numeric. M3: identity, CACHE, NOT VALID
  │ 8. static facts: values_rows (INSERT … VALUES row count)
  ▼
StatementPlan { original, engine_sql: Cow<str>, class, refusal, setting, intercept, values_rows }
```

`engine_sql` is the text sent to the engine (original with pre-parse rewrites and text edits
applied). Error positions are always computed against `original`.

## 3. Session API (`session.rs`)

```rust
pub struct PolicyLimits { pub max_rows_per_txn: u64, pub max_txn_age: Duration }  // Default: 3000, 300 s

pub enum TxnStatus { Idle, Active, Aborted }        // mirrored from the engine by the server

pub enum Admission<'a> {
    Run { sql: &'a str },                   // send to the engine
    Respond(Reply),                         // answered by micro-dsql (SHOW, SET no-op, ...)
    Refuse(PolicyError),                    // error; server aborts an Active block
    RollbackThenRefuse(PolicyError),        // server runs ROLLBACK first (COMMIT past the age limit)
}

pub enum Reply { Show { column: String, value: String }, Command { tag: &'static str } }

impl SessionPolicy {
    pub fn new(limits: PolicyLimits) -> Self;
    /// Before running: aborted-block bypass, age limit, rules, savepoints, discipline,
    /// autocommit VALUES row limit, settings.
    pub fn admit<'p>(&mut self, plan: &'p StatementPlan, status: TxnStatus, now: Instant) -> Admission<'p>;
    /// After the engine ran the statement successfully: transaction boundaries (D4),
    /// row accumulation, SET LOCAL scope; Err = row limit exceeded (server aborts the block).
    pub fn complete(&mut self, plan: &StatementPlan, rows_changed: u64, status_after: TxnStatus, now: Instant) -> Result<(), PolicyError>;
    /// After the engine failed: transaction boundary bookkeeping only.
    pub fn failed(&mut self, status_after: TxnStatus);
    /// Multi-statement simple query while Idle (research D7).
    pub fn check_batch(&self, plans: &[StatementPlan]) -> Result<(), PolicyError>;
}
```

Rules of `admit` (in order): (1) `status == Aborted` → `Run` (engine answers `25P02`, or
COMMIT/ROLLBACK); (2) `plan.parse_failed` → `Run`; (3) Active and older than `max_txn_age` →
`RollbackThenRefuse` for COMMIT/END, else `Refuse(age_limit)`; (4) `plan.refusal` →
`Refuse`; (5) savepoint statements → `Refuse`; (6) Active: discipline (FR-010/011); (7) Idle and
`values_rows > max_rows_per_txn` → `Refuse(row_limit)`; (8) setting command → `Respond`, `Refuse`
or `Run` (forwarded names); (9) intercept → `Respond`/`Refuse`; (10) `Run { sql: engine_sql }`.

## 4. Error shaping API (`errors.rs`)

```rust
pub struct PolicyError { pub rule: &'static str, pub sqlstate: &'static str, pub message: String, pub detail: Option<String> }

pub enum EngineFailure { DataConflict, SchemaConflict, LockBusy, Other }

pub fn oc000() -> PolicyError;     // 40001 "change conflicts with another transaction (OC000)"
pub fn oc001() -> PolicyError;     // 40001 "schema has been updated by another transaction (OC001)"
pub fn row_limit() -> PolicyError; // 54000 "transaction row limit exceeded"
pub fn age_limit(secs: u64) -> PolicyError; // 54000 "transaction age limit of {secs}s exceeded"

pub struct ShapedError { pub sqlstate: String, pub message: String, pub detail: Option<String>, pub position: Option<usize>, pub schema: Option<String> }

/// Engine error → wire error. DataConflict/LockBusy → oc000, SchemaConflict → oc001,
/// Other → today's PostgreSQL shaping (`shape_error`).
pub fn shape_engine_error(failure: EngineFailure, sqlstate: &str, message: &str, detail: Option<&str>, has_table: bool, original_sql: &str) -> ShapedError;
```

## 5. Server seams (`crates/server`)

| Seam | Where | Role |
|---|---|---|
| `dsql.rs` (new module) | `engine_failure(&turso_pg::Error) -> EngineFailure` | the only place that maps engine error variants (`WriteWriteConflict`, `BusySnapshot`, `CommitDependencyAborted`, `Conflict` → `DataConflict`; `SchemaConflict`, `SchemaUpdated` → `SchemaConflict`; `Busy`, `TableLocked` → `LockBusy`) |
| `dsql.rs` | `txn_status(&PgConnection) -> TxnStatus` | the narrow interface for transaction status |
| `dsql.rs` | `run_planned(conn, policy, plan, run_engine) -> PgWireResult<Outcome>` | admit → engine → complete/failed, shared by the simple and extended paths; reads `n_change()` after every engine statement (both result kinds) and passes it as `rows_changed` |
| `TursoPgHandler.policy` | `Arc<Mutex<SessionPolicy>>` | one per connection, locked only inside blocking closures |
| `PgStatement.plan` | `Arc<StatementPlan>` | built once at Parse for the extended protocol (replaces the `intercept` field) |
| `Server::bind_with(addr, storage, ServerOptions { limits })` | `lib.rs` | `Server::bind` keeps today's signature and uses `PolicyLimits::default()` |
| `finish_query` | `lib.rs` | uses `shape_engine_error` for engine errors and sends `PolicyError` (with DETAIL) for policy errors; replaces the private `SqlError` |

## 6. What M3 and M4 build on

| Later work | Seam |
|---|---|
| M3: `CREATE [UNIQUE] INDEX ASYNC` (strip `ASYNC` before libpg_query) | `rewrite::pre_parse` (pipeline step 1); the plan records that ASYNC was stripped so `admit` can answer with a `job_id` row (`Reply` gains a `Rows` variant) |
| M3: identity desugaring, sequence `CACHE 65536`, `NOT VALID` | `rewrite::REWRITES` + `TextEdit` (pipeline step 7) |
| M3: deferred rules (sync `CREATE INDEX`, CACHE, ADD CONSTRAINT, ALTER COLUMN TYPE) | new rows in `rules::RULES` + `contracts/rule-catalogue.md` |
| M3: `sys.jobs`, `sys.wait_for_job`, `sys.dsql_major_version` | `Admission::Respond(Reply::Rows …)` or a server-layer catalog hook; the policy decides, the server answers |
| M4: autocommit DML as a single-statement transaction | `SessionPolicy::admit`/`complete` already see Idle → Active → Idle; the autocommit row limit then moves from `values_rows` to `complete` |
| M4: deferred OC000 at COMMIT ("doomed transaction") | `TxnState` gains a `doomed: Option<PolicyError>` set from `EngineFailure::DataConflict` in `failed()`; `admit` on COMMIT returns `RollbackThenRefuse(errors::oc000())` |
| M4: drop the 55P03 mapping | remove `LockBusy → oc000` in `shape_engine_error` once autocommit no longer reports `Busy` |
