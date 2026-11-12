# Data model: M2 — DSQL rules

All state is in memory, per connection, owned by `crates/policy` (no engine types). Types are
named as in [contracts/policy-api.md](contracts/policy-api.md).

## StatementPlan (per statement text; immutable)

| Field | Meaning |
|---|---|
| `original` | text the client sent (error positions refer to it) |
| `engine_sql` | text sent to the engine after pre-parse rewrites and text edits |
| `parse_failed` | libpg_query could not parse it; no rule applies |
| `class` | `StatementClass`: `Ddl`, `Dml`, `Read`, `Txn(TxnControl)`, `Session`, `Other` |
| `refusal` | first matching deny rule, as a `PolicyError` (stateless) |
| `setting` | parsed `SettingCommand`, if a SET/RESET/SHOW/`set_config` statement |
| `intercept` | today's stateless intercept, if any |
| `values_rows` | row count of an `INSERT … VALUES` list (else `None`) |

`TxnControl`: `Begin`, `Commit`, `Rollback`, `Savepoint`, `Release`, `RollbackTo`, `Prepare`,
`CommitPrepared`, `RollbackPrepared`.

## SessionPolicy (per connection)

| Field | Meaning |
|---|---|
| `limits` | `PolicyLimits { max_rows_per_txn = 3000, max_txn_age = 300 s }` |
| `settings` | `SessionSettings` |
| `txn` | `Option<TxnState>`; `Some` while the engine reports `Active` or `Aborted` |

## TxnState (per explicit transaction)

| Field | Meaning | Validation |
|---|---|---|
| `began_at` | `Instant` when the engine status went Idle → Active | age check: `now - began_at > max_txn_age` → `age_limit` |
| `ddl_count` | DDL statements run successfully | a 2nd DDL → `txn_two_ddl` |
| `saw_dml` | a DML statement ran | DDL after DML, DML after DDL → `txn_ddl_dml` |
| `rows_changed` | sum of engine change counts | `> max_rows_per_txn` → `row_limit`, block aborted |
| `local_settings` | `SET LOCAL` overlay | dropped when the transaction ends |

Counters are updated in `complete()`, i.e. only for statements the engine ran successfully; the
discipline check runs in `admit()` against the counters so far.

### State transitions (driven by engine status, research D4)

```text
          BEGIN ok (Idle→Active): txn = new(now)
   Idle ───────────────────────────────────────────▶ Active
    ▲                                                 │  rule refusal / engine error /
    │ COMMIT, ROLLBACK, COMMIT-of-aborted (→Idle):    │  row limit (server aborts block)
    │ txn = None, local settings dropped              ▼
    └──────────────────────────────────────────── Aborted
                    ROLLBACK / COMMIT (→Idle)
   Active ──COMMIT past age limit──▶ server runs ROLLBACK ──▶ Idle, error 54000
```

In `Aborted`, `admit` always returns `Run`; the engine answers `25P02` (or ends the block).

## SessionSettings (per connection)

| Field | Meaning |
|---|---|
| `values` | remembered value per whitelisted name the engine does not implement (`datestyle`, `extra_float_digits`, `intervalstyle`, `enable_*`, `dsql.enable_batched_nestloop`) |
| `local` | `SET LOCAL` values (including `role`), in `TxnState` |

Lookup for `SHOW`: transaction-local value, else session value, else default
([contracts/rule-catalogue.md](contracts/rule-catalogue.md) §4).

## PolicyError

| Field | Meaning |
|---|---|
| `rule` | rule id from the catalogue (logged at debug level) |
| `sqlstate` | five-character SQLSTATE |
| `message` | exact message |
| `detail` | optional DETAIL |

## EngineFailure

`DataConflict`, `SchemaConflict`, `LockBusy`, `Other`: the only engine information the error
shaping needs; produced by the server from the engine error variant.
