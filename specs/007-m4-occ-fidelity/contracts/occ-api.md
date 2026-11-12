# Contract: OCC interfaces (M4)

Internal Rust interfaces between `crates/server` and `crates/policy`, plus the client-visible
behaviour. Extends specs/005-m2-dsql-rules/contracts/policy-api.md (§6 seams).

## 1. Client-visible behaviour

| Situation | Statement result | COMMIT result | Session after |
|---|---|---|---|
| Autocommit DML, no conflict | normal | — | `I` |
| Autocommit DML, data conflict | `40001` `change conflicts with another transaction (OC000)` | — | `I` |
| Autocommit DML, contention persists past retries | `40001` `schema has been updated by another transaction (OC001)` | — | `I` |
| Autocommit DML, > 3,000 rows | `54000` `transaction row limit exceeded`, nothing applied | — | `I` |
| Multi-statement string, statement k fails | statements < k: normal responses; k: its error; nothing applied | — | `I` |
| Explicit txn, write conflict (defer on) | normal result of the replay (or tag with 0 rows) | `40001` OC000; all rolled back | `I` |
| Explicit txn, write conflict (defer off) | `40001` OC000; block aborted | answers `ROLLBACK` | `I` |
| Explicit txn, conflict detected at COMMIT | normal | `40001` OC000 | `I` |
| Explicit txn, schema changed concurrently | normal | `40001` OC001 | `I` |
| Explicit txn, lock contention at a statement | `40001` OC001; block aborted | answers `ROLLBACK` | `I` |
| `SELECT … FOR UPDATE` (accepted form) | rows of the plain SELECT | conflicts as if the rows were written | — |
| `SELECT … FOR KEY SHARE` (accepted form) | rows of the plain SELECT | never conflicts | — |
| locking clause on an unsupported form | `0A000` `locking clause is only supported on a single-table SELECT` | — | — |

`55P03` never reaches a client.

## 2. Policy API additions (`micro_dsql_policy`)

```rust
// policy::session (existing type, new methods)
impl SessionPolicy {
    /// Opens policy tracking of an implicit transaction (server has run BEGIN itself).
    pub fn begin_implicit(&mut self, now: Instant);
    /// Records that the engine transaction was re-opened after a deferred data conflict.
    pub fn mark_doomed(&mut self);
    pub fn is_doomed(&self) -> bool;
    pub fn is_implicit(&self) -> bool;
    /// Records that the snapshot of the open transaction was pinned.
    pub fn mark_pinned(&mut self);
    /// The client's BEGIN text of the open transaction, for re-opening it.
    pub fn begin_sql(&self) -> Option<&str>;
    /// True when the open transaction must be re-opened unpinned before `plan` runs.
    pub fn needs_fresh_snapshot(&self, plan: &StatementPlan) -> bool;
    /// Row limit for one autocommit statement (its own implicit transaction).
    pub fn check_implicit_rows(&self, rows_changed: u64) -> Result<(), PolicyError>;
}
// admit(): Active + doomed + COMMIT → Admission::RollbackThenRefuse(errors::oc000())
//          (the age check keeps precedence; ROLLBACK is never refused)

// policy::locking (new)
pub enum LockStrength { Update, KeyShare }
pub struct LockingRead { pub strength: LockStrength, pub read_sql: String, pub touch: Option<TouchPlan> }
pub struct TouchPlan { /* relation, alias, where clause */ }
impl TouchPlan { pub fn relation(&self) -> &str; pub fn render(&self, column: &str) -> String; }
pub fn plan_locking(node: &NodeEnum) -> Option<Result<LockingRead, PolicyError>>;

// policy::statement
pub struct StatementPlan { /* … */ pub locking: Option<LockingRead> }

// policy::errors
pub fn locking_read_form() -> PolicyError;   // rule "locking_read_form", 0A000
// shape_engine_error: EngineFailure::LockBusy → oc001()
```

Invariants: no new dependency in `crates/policy/Cargo.toml`; every function above is unit-tested
without an engine.

## 3. Server API additions (`micro_dsql_server`)

```rust
#[derive(Debug, Clone, Copy)]
pub struct OccOptions {
    pub defer_conflicts: bool,          // default true
    pub snapshot_at_begin: bool,        // default true
    pub busy_retry_attempts: u32,       // default 10
    pub busy_retry_budget: Duration,    // default 200 ms
}
pub struct ServerOptions { pub limits: PolicyLimits, pub occ: OccOptions }   // Default derives

// crate-private (crates/server/src/occ.rs)
pub(crate) trait Engine {
    /// Runs `sql` and returns its outcome and change count.
    fn run(&mut self, sql: &str) -> PgWireResult<(Outcome, u64)>;
    /// The outcome of `sql` with no rows and a 0 count (describe only, no execution).
    fn empty(&mut self, sql: &str) -> PgWireResult<(Outcome, u64)>;
}
pub(crate) fn run(conn: &PgConnection, policy: &Mutex<SessionPolicy>, plan: &StatementPlan,
                  format: &Format, opts: &OccOptions, engine: &mut dyn Engine) -> PgWireResult<Outcome>;
pub(crate) fn run_batch(conn: &PgConnection, policy: &Mutex<SessionPolicy>, plans: &[StatementPlan],
                        opts: &OccOptions, each: impl FnMut(&StatementPlan) -> PgWireResult<Outcome>)
                        -> PgWireResult<Vec<Outcome>>;
pub(crate) fn failure_of(error: &PgWireError) -> Option<EngineFailure>;
pub(crate) const PIN_SQL: &str = "SELECT 1 FROM pg_catalog.pg_class LIMIT 0";
```

Engine statements the server issues on its own: `BEGIN`, the client's BEGIN text, `COMMIT`,
`ROLLBACK`, `PIN_SQL`, rendered touches. They never pass through `SessionPolicy::admit` and are
never shown to the client.

Logging (debug level, once each): `deferred conflict`, `busy retry` (attempt n), `residual
contention`, `snapshot re-opened for DDL`.
