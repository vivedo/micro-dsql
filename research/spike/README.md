# M0 spike scripts

Evidence for `research/06-spike-findings.md` (PLAN.md milestone M0). They target the upstream `tursopg`
binary, not micro-dsql. Every script reads the standard libpq variables (`PGHOST`, `PGPORT`, `PGUSER`,
`PGDATABASE`, `PGPASSWORD`, `PGSSLMODE`); `env.sh` fills in defaults (`127.0.0.1:5432`, `admin`,
`postgres`, an arbitrary password, `sslmode=disable`).

## Prerequisites

- `tursopg` built in a local Turso clone: `cargo build -p tursopg --release`
  (add `RUSTUP_TOOLCHAIN=stable` if the pinned 1.88 toolchain can't be installed).
- `psql` 16+ (for `\bind`). Set `PSQL=/path/to/psql` if it isn't on `PATH`
  (e.g. `PSQL="$(brew --prefix libpq)/bin/psql"`).
- Python 3.10+, Node.js 18+, Go 1.24+, `nc`.

Install the drivers from the repository root:

```sh
python3 -m venv .venv && .venv/bin/pip install -r research/spike/drivers/requirements.txt
(cd research/spike/drivers/node && npm ci)
(cd research/spike/drivers/go && go mod download)
```

## Run

```sh
TURSOPG=<turso-clone>/target/release/tursopg research/spike/run-all.sh
```

With `TURSOPG` set, a fresh `:memory:` server is started for each group and stopped afterwards.
Without it, the scripts run against whatever already listens on `$PGHOST:$PGPORT`.
Output lands in `research/spike/results/<group>.txt`.

| Group | Script | What it does |
|---|---|---|
| connect | `connect.sh` | `SELECT 1`, identity, `SHOW`/`SET` basics, startup `server_version` |
| app | `app/run.sh` | app-style schema (`schema.sql`), 18 query groups (`queries.sql`), research probes (`probes.sql`) |
| drv-* | `drivers/*` | psql, psycopg 3, asyncpg, node-pg, pgx: connect, CRUD, explicit txn, parameterised queries |
| wire | `drivers/wire_probe.py` | type OIDs in RowDescription, ParameterDescription, binary params/results, Terminate |
| occ | `occ/occ.py` | two-session OCC scenarios, concurrent-write storm, isolation levels |

Each script prints one line per step (`OK …` or `ERR <SQLSTATE> <message>`); errors are findings, not
script failures. A watchdog kills any group that runs longer than `SPIKE_SCRIPT_TIMEOUT` seconds (300).
