# 03 — Landscape: existing DSQL emulators, client tooling, alternative bases

Researched on 2026-11-12. Repository existence was spot-checked; I did not run any of the emulators.

## Headline

**The field is more crowded than expected.** At least five DSQL data-plane emulators exist, and **all of them run a real PostgreSQL 16 behind a proxy or rule layer**. None of them is truly optimistic: each one admits that conflicts go through PG row locks. **None is Docker-free.**

A Turso-based emulator would be the first with native optimistic snapshot isolation, and the first to ship as a single binary with no Postgres inside. That is its niche. It cannot compete on PG fidelity.

## 1. Existing emulators

| Project | Stack | Data plane | TLS / direct TLS / SNI | Token auth | Isolation / OCC | Dialect rules | Control plane |
|---|---|---|---|---|---|---|---|
| **MiniStack** ([repo](https://github.com/ministackorg/ministack), MIT) `services/dsql.py` + `core/pgproxy.py` | Python asyncio proxy, one `postgres:16-alpine` container per cluster (needs `DSQL_STRICT=1` and the Docker socket) | Port `25432+n`, max 30 clusters; endpoint is reported as `localhost:<port>` | **No.** SSLRequest is answered `N` | **None.** It answers AuthenticationOk and connects to PG as the superuser. IAM GRANT mappings are recorded but not enforced. | PG default (READ COMMITTED) [I]. OC001 is emulated with a catalog version. **No OC000**: writers wait on a row lock. | Broadest set: a regex/lexer denylist plus allowlist. Async index jobs are rewritten to sync, and `sys.jobs` is emulated. 3k-row counting only counts `VALUES` rows. | All 16 ops |
| **Floci** ([repo](https://github.com/floci-io/floci), MIT, Java/Quarkus) | PG16 per cluster in Docker | `<id>.dsql.<region>.localhost.floci.io:5432`; one port, routed by SNI | **Yes.** Mandatory TLS, a local CA at `/_floci/ca.pem`, verify-full, direct TLS with ALPN | **Real SigV4 verification**, admin/DbConnectAdmin mapping, IAM GRANT required | REPEATABLE READ, the 300 s age limit, 1 h connection cap. OCC "waits for the other transaction and fails when it commits". | 3k rows (FK cascades counted via triggers), `sys.*` functions | Yes, including CloudFormation |
| **Dreamescaper/dsql-emulator** ([repo](https://github.com/Dreamescaper/dsql-emulator), MIT, Go, v0.3.0 Sep 2026, `ghcr.io/dreamescaper/dsql-emulator`) | One container: proxy + PG16 | `:5432` | Self-signed TLS | **Not validated** | RR pinned. **Conflicts are reported at COMMIT** as `40001 … (OC000)`, using a bounded lock wait plus "fake success until COMMIT". The loser is the first writer that is refused the lock, not the second committer. It can inject conflicts deterministically. | 45 rules on a real parse tree | No |
| **LocalStack** ([docs](https://docs.localstack.cloud/aws/services/dsql/), Ultimate tier, since 2026.06) | Embedded PG (version not stated) | — | Not documented | Plain `test/test` credentials; IAM tokens not used | Not documented | Only ASYNC index and `sys.jobs`; "broader dialect restrictions … are not enforced" | All 16 ops |
| **tomodian/deesql** ([repo](https://github.com/tomodian/deesql), Go) | `proxy` command in front of PG | — | — | — | — | Unsupported-SQL rejection; README outdated (says no FKs) | — |
| **moto** (`moto/dsql`) | — | — | — | — | — | — | Control plane only. Its endpoint omits the `.dsql.` label |
| renebrandel/dsql-local-simulator | 2024 PG extension prototype | stale | | | | | |

### What's worth reusing
- **Dreamescaper's conformance suite.** `test/conformance/golden/` is a record of 242 probes run against a live cluster, with a diff harness. It is the best existing acceptance test for any DSQL emulator. Licence: MIT.
- **MiniStack's rule list and tests.** `tests/test_dsql.py` has 214 tests, and the CHANGELOG shows the DSQL parity work is your own. The startup parameters measured on real DSQL (`server_version=16.4`, `TimeZone=UTC`, `is_superuser=off`, …) are useful too.
- **Floci's connection model.** SNI routing on one port, a hostname shape the connectors accept, a downloadable CA, real token verification, and DSQL's exact FATAL messages.

## 2. What client tooling requires (the real acceptance surface)

The official connectors live in [awslabs/aurora-dsql-connectors](https://github.com/awslabs/aurora-dsql-connectors). It covers Python, Node pg, postgres-js, JDBC, Go pgx, Rust sqlx, .NET Npgsql, Ruby, and PHP PDO. The ORM adapters live in [awslabs/aurora-dsql-orms](https://github.com/awslabs/aurora-dsql-orms): Django, SQLAlchemy, Tortoise, Hibernate, Flyway, EF Core, Prisma, and Drizzle.

**Hostname**
- Region is parsed with `\.dsql[^.]*\.([^.]+)\.on\.aws$`.
- Python expands a host with no `.dsql` label into `<id>.dsql.<region>.on.aws`.
- **So `localhost` breaks some connectors.** The emulator needs `<id>.dsql.<region>.<something>` names, like Floci's `*.localhost.floci.io` wildcard DNS, or users have to set `/etc/hosts` / `--add-host`.

**TLS**
- JDBC defaults to `sslmode=verify-full` with `sslNegotiation=direct`.
- Node pg uses `rejectUnauthorized: true`.
- SQLAlchemy uses direct TLS when libpq is 17 or newer.
- **TLS with a trustable CA and direct negotiation is mandatory for out-of-the-box use.**

**Tokens**
- Every connection generates one locally. The default lifetime is 15 min; the Python connector uses 60 s.
- Tokens are well-formed even with dummy credentials.

**OCC retry helpers**
- Go, Python, Rust, JDBC, and Node retry on `40001` / `OC000` / `OC001`.
- **Rust parses "(OC000)" / "(OC001)" from the message**, so the message text must match DSQL's.

**DDL that ORMs emit**
- `BIGINT GENERATED BY DEFAULT AS IDENTITY (CACHE 65536)` and `CREATE INDEX ASYNC`.
- `ALTER TABLE … ADD CONSTRAINT … NOT VALID`, then `ALTER TABLE ASYNC … VALIDATE CONSTRAINT`.
- Polling of `sys.jobs` / `sys.wait_for_job`.
- `DELETE` instead of TRUNCATE.
- No savepoints.

**Introspection**
- Hibernate validate, Flyway, Prisma, Django `inspectdb`, and the migration tools use the standard `pg_catalog` and `information_schema` queries. I have not audited them query by query.

**Driver protocol behaviour**
- asyncpg, pgx, and Npgsql use the extended protocol, often with **binary parameters**.
- JDBC uses server-side prepared statements after 5 executions.

**AWS sample CI.** [aws-samples/aurora-dsql-samples](https://github.com/aws-samples/aurora-dsql-samples/tree/main/.github/workflows) has about 30 `*-integ-tests.yml` workflows across C++, Deno, .NET, Go, Java/Spring/Liquibase, Node/Sequelize/TypeORM/Prisma/Drizzle, PHP, Python, Ruby/Rails, and Rust/sqlx. Each takes `CLUSTER_ENDPOINT` / `REGION` / `CLUSTER_USER=admin`, which makes them a ready-made compatibility matrix if the emulator can impersonate the endpoint.

**AWS's own position.** There is no local emulator. AWS offers the hosted Playground (Feb 2026) and the console query editor. Its own CI creates real clusters.

## 3. Turso Postgres: public status

- **Blog post.** ["We're building Postgres in Rust. Using the LLVM of databases"](https://turso.tech/blog/a-new-modern-version-of-postgres-in-rust), 2026-07-16. It says "a foundation, not a finished product", "compatible enough", and "no published packages yet".
- **[glommer/pgmicro](https://github.com/glommer/pgmicro)** is archived and has been merged in-tree. Its npm package `pg-micro` stopped at 0.0.5 (April 2026).
- **Packages.** As of 2026-11-12 there are still no tursopg crates, binaries, or Docker images. The Turso v0.8.x–0.9.0-pre releases carry no Postgres assets.
- **MVCC.** It is "a supported journal mode" in the in-repo manual. Turso Cloud concurrent writes have been in "early preview" since August 2026 ([blog](https://turso.tech/blog/concurrent-writes-on-turso-cloud)). Treat it as maturing.
- **Roadmap.** I found no public dates for TLS or auth in the Turso wire server.

## 4. Alternative bases

| Base | PG16 fidelity | Footprint | OCC fidelity | Verdict |
|---|---|---|---|---|
| **Real PG16 + proxy** (all existing emulators) | Exact | Docker + PG, about 1–3 s startup | Poor. Lock-based; needs lock-timeout tricks. The snapshot-at-BEGIN difference can be patched with a dummy read. | Proven, but the space is already well served |
| **Embedded PG binaries** (e.g. `postgresql_embedded`, theseus-rs) + proxy | Exact | No Docker, but a full postmaster of tens of MB; initdb takes seconds (estimate) | Same as above | Plausible Docker-free option with PG fidelity |
| **PGlite + pglite-socket** | PG17, single-user | Small, Node/WASM | Essentially none (one backend) | No |
| **Turso Postgres** | Medium and improving; PG18 grammar via libpg_query | One small Rust binary, ms startup, `:memory:` | **Native optimistic snapshot isolation**: the best structural match | **The differentiated option.** See 04. |
| **Custom pgwire over SQLite / DataFusion** | Low (you'd reimplement everything Turso already has) | Tiny | Hand-rolled | No. Turso already is this, further along. |
| **CockroachDB / YugabyteDB** | Different dialects and catalogs | Heavy | SERIALIZABLE/SI, but different semantics | Overkill and off-target |
| **Neon Local** | Real PG, but a proxy to the cloud | — | — | Not offline |
