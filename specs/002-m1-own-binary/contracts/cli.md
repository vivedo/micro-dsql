# Contract: `micro-dsql` CLI

```
micro-dsql [--listen <ADDR:PORT>] [--data <FILE> | --memory] [--log <FILTER>]
```

| Flag | Default | Meaning |
|---|---|---|
| `--listen` | `127.0.0.1:5432` | address to bind; port `0` lets the OS choose |
| `--data <FILE>` | — | persistent database file (created if missing) |
| `--memory` | on | fresh in-memory database (conflicts with `--data`) |
| `--log <FILTER>` | `info` | `tracing` env-filter syntax (`debug`, `micro_dsql=debug,warn`); `RUST_LOG` is not read |

Output: logs go to stderr. On readiness one line is logged:
`micro-dsql listening on <ip>:<port> (database: memory|<file>)`.

Exit status: `0` after SIGINT/SIGTERM; non-zero (`1`) when the address can't be bound or the
database can't be opened; `2` for usage errors (clap).
