# Data model: M1

No persistent data model of its own; the engine owns storage. Runtime entities:

| Entity | Fields | Notes |
|---|---|---|
| ServerConfig | listen addr, storage (`Memory` / `File(path)`), log filter | from CLI |
| ServerParams | ordered (name, value) pairs, see research R4 | static; reported at startup and by SHOW |
| Session | `PgConnection` (engine session), wire state (pgwire client), transaction status | one per TCP connection; closed when the socket ends |
| Intercept | `Show { name, column, value }` / `SetNoop { tag }` | produced by `policy` from the AST |
| ParamHint | per `$n`: `Declared(oid)` / `Cast(type name)` / `Column{table, column}` / `Int8` / `Unknown` | produced by `policy`; resolved to an OID by `server` |
| KnownGap | id, observed error, suspected cause, milestone, test name | `KNOWN_GAPS.md` |

Session transaction state: Idle → Active (BEGIN) → Idle (COMMIT/ROLLBACK); Active → Aborted
(error) → Idle (ROLLBACK, or COMMIT answering ROLLBACK).
