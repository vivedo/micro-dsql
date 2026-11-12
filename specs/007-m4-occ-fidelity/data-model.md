# Data model: M4 — OCC fidelity

All state is per connection and in memory. Policy types live in `crates/policy` (no engine types,
constitution III); server types in `crates/server`.

## OccTxn (policy, new; field `occ` of `TxnState`)

| Field | Type | Meaning | Set by |
|---|---|---|---|
| `implicit` | `bool` | opened by micro-dsql (autocommit DML or multi-statement string), not by the client | `SessionPolicy::begin_implicit` |
| `begin_sql` | `Option<String>` | the client's `BEGIN`/`START TRANSACTION` text (options such as `READ ONLY` are re-applied when the server re-opens the transaction) | `complete()` of a `Txn(Begin)` plan |
| `pinned` | `bool` | the snapshot was fixed by the pin read | `SessionPolicy::mark_pinned` |
| `touched` | `bool` | a client statement ran after `BEGIN` (the pin no longer can be dropped) | `complete()` / `failed()` of any non-BEGIN plan |
| `doomed` | `bool` | a data conflict was deferred; COMMIT must fail with OC000 | `SessionPolicy::mark_doomed` |

Validation rules:

- `doomed` implies `!implicit` (implicit transactions report conflicts at once).
- `needs_fresh_snapshot(plan)` ⇔ `pinned && !touched && plan.class == Ddl`.
- `doomed` survives every statement until the transaction ends; it is dropped with `TxnState`.

### State transitions (explicit transaction)

```text
Idle ──BEGIN ok──► Active{pinned?} ──client stmt──► Active{touched}
Active ──DataConflict at stmt (defer on)──► Active{doomed} (engine txn re-opened, statement replayed)
Active{doomed} ──COMMIT──► Idle + 40001 OC000      Active{doomed} ──ROLLBACK──► Idle
Active{doomed} ──other error──► Aborted ──ROLLBACK/COMMIT──► Idle (COMMIT answers ROLLBACK)
Active{pinned, !touched} ──DDL──► server: ROLLBACK; BEGIN(begin_sql) ──► Active{!pinned} ──► DDL runs
```

### Implicit transactions

Single autocommit DML (inside the engine closure; the policy only sees `Idle` before and after and
never creates a `TxnState` for it):

```text
Idle ──autocommit DML──► (BEGIN) ──stmt ok, rows ≤ limit──► (COMMIT) Idle
                                ──Busy──► (ROLLBACK) retry within BUSY_RETRY
                                ──error / rows > limit──► (ROLLBACK) Idle + error
```

Multi-statement string with DML or DDL (`occ::run_batch`; the policy tracks it as a `TxnState`
with `occ.implicit = true`, so discipline and row counting apply per statement):

```text
Idle ──string──► (BEGIN) begin_implicit ──each stmt ok──► (COMMIT) failed(Idle)/complete → Idle
                                       ──any error──► (ROLLBACK) failed(Idle) → Idle + error
```

Internal transaction for one client DDL statement (006's rebuild, via
`occ::internal_transaction`): unpinned, never doomed, retried as a whole on `Busy`.

## LockingRead (policy, new; field `locking` of `StatementPlan`)

| Field | Type | Meaning |
|---|---|---|
| `strength` | `LockStrength::{Update, KeyShare}` | `FOR UPDATE` or `FOR KEY SHARE` |
| `read_sql` | `String` | the SELECT without its locking clause (deparsed); becomes `plan.engine_sql` |
| `touch` | `Option<TouchPlan>` | `Some` for `Update`; `None` for `KeyShare` |

`TouchPlan { relation: String, alias: Option<String>, where_sql: Option<String> }`;
`render(column)` → `UPDATE <relation> [AS <alias>] SET <column> = <column> [WHERE <where_sql>]`
(built as an AST and deparsed, so quoting is correct).

Accepted forms: one `RangeVar` in `FROM`, no `GROUP BY`/`HAVING`/`DISTINCT`/set operation/window,
locking clause on the top-level SELECT, at most one `OF` relation equal to that table or alias, any
`NOWAIT`/`SKIP LOCKED`. `LIMIT`/`OFFSET`/`ORDER BY` allowed (ignored by the touch). Anything else
with a `FOR UPDATE`/`FOR KEY SHARE` clause → `locking_read_form` refusal (`0A000`).

## OccOptions (server, new; field `occ` of `ServerOptions`)

| Field | Default | Meaning |
|---|---|---|
| `defer_conflicts` | `true` | doomed-transaction shim (FR-016) |
| `snapshot_at_begin` | `true` | pin read after BEGIN (FR-023) |

Busy retry is not an option: the constant `occ::BUSY_RETRY = { attempts: 10, budget: 200 ms }`
bounds every retry (autocommit DML attempt, autocommit DDL statement, internal transaction), because
006's `constraints::in_internal_transaction` has no access to the server options.

## Conflict matrix cells (test data, tests/occ/)

| Cell | Op A | Op B | DSQL | micro-dsql after M4 |
|---|---|---|---|---|
| M1 | UPDATE r | UPDATE r | later committer OC000 at COMMIT | later writer OC000 at COMMIT (deferred) |
| M2 | INSERT k | INSERT k | later committer OC000 at COMMIT | same |
| M3 | DELETE r | UPDATE r | X | later writer OC000 at COMMIT |
| M4 | UPDATE(key) r | UPDATE(non-key) r | X | later writer OC000 at COMMIT |
| M5 | FOR UPDATE r | UPDATE r | X | later toucher/writer OC000 at COMMIT |
| M6 | FOR UPDATE r | FOR UPDATE r | X | same as M5 |
| M7 | FOR UPDATE r | INSERT/UPDATE other row | — | no conflict |
| M8 | read r | UPDATE r | — (write skew allowed) | no conflict |
| M9 | FOR KEY SHARE r | UPDATE(non-key) r | — | no conflict |
| D1 | FOR KEY SHARE r | DELETE / UPDATE(key) r | X | **no conflict** (difference) |
| D2 | DELETE parent | INSERT child | X (implicit key share) | **both commit** (difference) |
| D3 | UPDATE r, later commits first | UPDATE r earlier, commits second | first committer wins | earlier writer wins (difference) |
| D4 | UPDATE r then ROLLBACK | UPDATE r | B commits | B doomed (difference) |
| D5 | write T | DDL on unrelated table | OC001 only if T used | OC001 (difference, B11) |
