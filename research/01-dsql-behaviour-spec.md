# 01 — Aurora DSQL behaviour reference (what an emulator must reproduce)

Researched 2026-11-12. Sources:
- **User Guide PDF** (© 2026). Its newest release note is 16 Oct 2026 and its newest doc-history entry is 9 Nov 2026. The extracted text is in `sources/aurora-dsql-userguide-2026-11.txt`. "UG p.N" means the guide's own printed page number; the PDF page index is about N+12.
- **API reference pages**, in `sources/dsql-api/`.
- **botocore `dsql` service model and token signer**, in `sources/botocore/`.

Legend:
- **[inferred]**: deduced from the docs, not stated in them.
- **[UNVERIFIED]**: needs confirming against a live cluster. These are collected in §9.

> **Key change since GA (May 2025).** Much of what was "unsupported" at GA is now supported: foreign keys, sequences/IDENTITY, views, json/jsonb columns, SQL functions, expression and partial indexes, DROP COLUMN, FOR KEY SHARE and extended statistics. The release timeline is in §8. AWS no longer publishes an explicit "unsupported features" page. Unsupported features have to be inferred from the "supported" lists, the catalog tables and the ORM adapters. **Treat DSQL as a moving target: the emulator needs a way to track it.**

---

## 1. Connection and authentication (UG p.29–49, 378–381, 508–511, 584–586, 604–606)

### 1.1 Endpoint, port, database
- **Endpoint:** `<cluster-id>.dsql.<region>.on.aws`. The cluster ID matches `[a-z0-9]{26}`.
- **Port:** 5432. There is **one database, `postgres`**; `CREATE DATABASE` fails with `ERROR: unsupported statement`.
- **Locale:** UTF-8 encoding, **C collation only**, timezone UTC.
- **Version:** "based on PostgreSQL 16". The docs' example shows `SHOW server_version` returning `16.13`. Other emulators measured 16.4 (MiniStack, Oct 2026) and 16.15 (Dreamescaper), so make it configurable. `sys.dsql_major_version()` returns `1`.
- **PrivateLink without private DNS:** the client passes the startup option `-c amzn-cluster-id=<id>`. DSQL identifies the cluster from the hostname's cluster-ID portion.
  - **Emulator implication:** route by SNI or hostname, falling back to `amzn-cluster-id`.

### 1.2 TLS
- **TLS is mandatory.** "Connections without SSL are rejected."
- The server certificate chains to Amazon Root CA 1.
- **SNI is required.** psql older than 14 fails with `unable to accept connection, sni was not received`.
- **Direct TLS (PG17 `sslnegotiation=direct`, ALPN `postgresql`) is the default in the official connectors:**
  - JDBC: `verify-full` with direct negotiation.
  - Go, Ruby, .NET: `verify-full` with direct negotiation.
  - SQLAlchemy dialect: direct negotiation when libpq is 17 or later.
  - **The emulator must accept both the classic SSLRequest and direct TLS, and must give users a CA to trust.**

### 1.3 IAM auth token (sent as the PG password)
A SigV4 **query-string presigned GET** of `https://<hostname>/` with the scheme stripped:
```
<hostname>/?Action=DbConnectAdmin&X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIA.../20261112/us-east-1/dsql/aws4_request&X-Amz-Date=20261112T000040Z&X-Amz-Expires=900&X-Amz-SignedHeaders=host[&X-Amz-Security-Token=...]&X-Amz-Signature=<hex>
```
- `Action` is `DbConnect` or `DbConnectAdmin`. The signing service is `dsql`, and the only signed header is `host`.
- Expiry defaults to 900 s; the maximum is 604,800 s. Expiry only matters at connect time.
- Token generation is **purely local**: botocore `generate_db_connect[_admin]_auth_token`, `aws dsql generate-db-connect[-admin]-auth-token`, JS `@aws-sdk/dsql-signer`, Rust `aws_sdk_dsql::auth_token`. So CI clients with dummy credentials still produce well-formed tokens.
- **The user must match the action.** `admin` needs DbConnectAdmin and anyone else needs DbConnect. Documented error texts:
  - `IAM authentication failed for user "..."`
  - `Wrong user to action mapping. user: admin, action: DbConnect`
  - `IAM role <role> does not exist`
  - `IAM role must look like an IAM ARN`
  - The SQLSTATEs are [UNVERIFIED], probably 28000/28P01.
- An emulator can validate the signature with configured credentials (Floci does this), or just check the format, action and expiry.

### 1.4 Roles
- **`admin`** is predefined and can't be modified. It owns `public`. Non-admin roles normally work in their own schemas.
- **Custom roles:**
  - `CREATE ROLE r WITH LOGIN`. LOGIN is mandatory, otherwise `role <role> must have the LOGIN attribute`.
  - `AWS IAM GRANT r TO 'arn:aws:iam::123456789012:role/x'` and `AWS IAM REVOKE r FROM '...'`.
  - The mapping view is `sys.iam_pg_role_mappings(iam_oid, arn, pg_role_oid, pg_role_name, grantor_pg_role_oid, grantor_pg_role_name)`.
  - Granting IAM trust as a non-admin fails with `permission denied to grant IAM trust with role <role>`.
  - Dropping a mapped role fails with `role <role> cannot be dropped because some objects depend on it`.
- `role` can only be set with `SET LOCAL` inside a transaction.

### 1.5 Connection limits

| Limit | Value | Error |
|---|---|---|
| Max connections | 10,000 (configurable) | `53300` "Unable to accept connection, too many open connections." |
| Connection rate | 100/s, burst 1,000 | `53400` "Unable to accept connection, rate exceeded." |
| **Max connection duration** | **60 min** | None documented. Connectors set the pool max lifetime to 3300 s. |
| Max message size | 10 MiB | `08P01` "FATAL: invalid message length" |

No idle timeout is documented [UNVERIFIED].

### 1.6 Session parameters (UG p.380–381)
Only these can be set:
- `application_name`, `client_encoding`, `datestyle`, `extra_float_digits`, `intervalstyle`, `timezone`, `search_path`
- `enable_{bitmapscan,hashjoin,indexonlyscan,indexscan,material,mergejoin,nestloop,seqscan}`
- `dsql.enable_batched_nestloop`
- `role` (SET LOCAL only)

Anything else fails with `ERROR: setting configuration parameter "x" not supported`. The SQLSTATE is [UNVERIFIED]. `statement_timeout`, `idle_in_transaction_session_timeout` and `lock_timeout` cannot be set.

---

## 2. Concurrency control (UG p.123–126, 135–137, 607–608)

- **Isolation** is fixed at the equivalent of REPEATABLE READ (snapshot isolation). Only `BEGIN/START TRANSACTION [ISOLATION LEVEL REPEATABLE READ] [READ WRITE|READ ONLY]` is accepted.
- **The snapshot is taken at BEGIN.** Postgres RR takes it at the first statement instead. This difference can be observed.
- **OCC.** No locks are taken and there are no lock waits. Conflicts are adjudicated **at COMMIT**, and the first committer wins. Errors:
  - `ERROR: change conflicts with another transaction (OC000)` with SQLSTATE **40001**. This is a data conflict.
  - `ERROR: schema has been updated by another transaction (OC001)` with SQLSTATE **40001**. This is a stale catalog cache. Any catalog change can trigger it, including GRANT, and it can appear on a statement that runs after the DDL finished. Retrying fixes it.
  - **The message text matters.** The Rust connector tells OC000 and OC001 apart by parsing the message.
- **Conflict matrix.** "Key columns" are the members of a unique, non-partial, non-expression index. **X** means the later committer fails with OC000.

|   | INSERT / DELETE / UPDATE(key) / FOR UPDATE | UPDATE (non-key only) | FOR KEY SHARE |
|---|---|---|---|
| INSERT / DELETE / UPDATE(key) / FOR UPDATE | X | X | X |
| UPDATE (non-key only) | X | X | |
| FOR KEY SHARE | X | | |

- **Plain reads never conflict** [inferred]. Write skew is allowed, as in SI.
- `SELECT … FOR UPDATE`: the selected rows are adjudicated as if written. Multi-table use is allowed since Aug 2026.
- `FOR KEY SHARE` arrived in Aug 2026.
- Unsupported locking: `FOR NO KEY UPDATE`, `FOR SHARE`, `SKIP LOCKED`, `NOWAIT` [inferred], advisory locks and `LOCK TABLE`.
- **Foreign keys under OCC.** FK checks read at the start snapshot, and DSQL implicitly applies KEY SHARE to the parent rows at commit. So a concurrent parent DELETE and child INSERT produce OC000. Real FK violations raise `23503` and are not retryable.
- **Transient error:** `XX000 server unavailable`. It is retryable and the connection stays usable. If it happens during COMMIT, the outcome is unknown.
- **Connection health:** 40001 and 0A000 errors leave the connection healthy. The Hibernate guide tells HikariCP not to evict connections on them.

---

## 3. Transaction rules and limits (quotas page, UG p.122, 126–127, 597–602)

### 3.1 Transaction rules
- **At most one DDL statement per transaction, and DDL and DML can't be mixed.** For example, `BEGIN; CREATE TABLE …; INSERT …; COMMIT;` fails. The error text and SQLSTATE are [UNVERIFIED]. MiniStack uses `25006`; it is probably `0A000`.
- `CREATE SCHEMA` may nest `CREATE SEQUENCE` and `GRANT` and still counts as one DDL.
- **SAVEPOINT is not supported** (`0A000`).
- `ANALYZE` is not allowed inside an explicit transaction.
- `SET CONSTRAINTS` applies only to deferrable FKs and only inside a transaction block.

### 3.2 Database limits

| Limit | Value | SQLSTATE | Message |
|---|---|---|---|
| PK combined size, secondary index key size | 1 KiB | 54000 | `key size too large` |
| Row size | 2 MiB | 54000 | `maximum row size exceeded` |
| Non-indexed column size | 1 MiB | 54000 | `maximum column size exceeded` |
| Columns per PK or index | 8 | 54011 | `more than 8 column keys in an index are not supported` |
| Columns per table | 255 active, 1,600 lifetime | 54011 | `tables can have at most 255 columns` |
| Indexes per table | 24 | 54000 | `more than 24 indexes per table are not allowed` |
| Extended statistics per table | 5 | 54000 | `more than 5 extended statistics per table are not allowed` |
| **Data modified per transaction** | **10 MiB** | 54000 | `transaction size limit 10mb exceeded` + DETAIL `Current transaction size {n} 10mb` |
| **Rows mutated per transaction** | **3,000** | 54000 | `transaction row limit exceeded` |
| Query memory | 128 MiB | 53200 | `query requires too much temp space, out of memory.` |
| Schemas | 10 | 54000 | `more than 10 schemas not allowed` |
| Tables | 1,000 | 54000 | `creating more than 1000 tables not allowed` |
| Views | 5,000 | 54000 | `creating more than 5000 views not allowed` |
| View definition size | 2 MiB | 54000 | `view definition too large` |
| Sequences | 5,000 | 54000 | `creating more than 5000 sequences is not allowed` |
| Databases | 1 | none | `unsupported statement` |
| **Transaction age** | **300 s** | 54000 | `transaction age limit of 300s exceeded` |
| Statistics target | 100 | none | `statistics target N exceeds maximum allowed value of 100` |

- The 3,000-row limit applies to INSERT, UPDATE and DELETE, however many indexes the table has.
- Rows changed by FK cascades count toward the 3,000-row limit.

---

## 4. SQL surface

### 4.1 Column types

| Type | Notes | Indexable |
|---|---|---|
| int2 / int4 / int8 / float4 / float8 | | yes |
| numeric / decimal | Precision up to 1000. **Bare `numeric` means `numeric(18,6)`**. Max 510 bytes. | yes |
| char(n) / bpchar | 4,096 B | yes |
| varchar(n) | 65,535 B | yes |
| text | 1 MiB | yes |
| date, time, timestamp, timestamptz | | yes |
| timetz, interval | | **no** |
| boolean, uuid | | yes |
| bytea | 1 MiB | **no** |
| json, jsonb | Allowed as columns since May/Jun 2026; 1 MiB compressed. All PG §9.16 JSON functions are supported. | **no** |

- Allowed at query time only, not as column types: **arrays** and **inet**.
- Not supported as column types [inferred]: enums, composite types (`CREATE TYPE … AS` → `unsupported statement: CompositeType`), ranges, geometric types, xml, tsvector, money, bit, and the `serial` pseudo-types.
- `CREATE DOMAIN` is documented as supported, although MiniStack rejects it. Verify this.

### 4.2 Supported commands (UG p.58–63)
- **SELECT clauses:**
  - joins, CTEs, set operations
  - `GROUP BY [ALL|DISTINCT]`, `OVER` (RANK, PARTITION BY)
  - `FOR {UPDATE | KEY SHARE}`
- **DDL:**
  - Tables, indexes and statistics:
    - CREATE / ALTER / DROP TABLE
    - **`CREATE [UNIQUE] INDEX ASYNC`**, DROP INDEX
    - CREATE / ALTER / DROP STATISTICS
  - Views, sequences, roles and schemas:
    - CREATE / ALTER / DROP VIEW
    - CREATE / ALTER / DROP SEQUENCE
    - CREATE ROLE … WITH; ALTER USER/ROLE/GROUP … RENAME TO
    - CREATE SCHEMA, which may nest CREATE SEQUENCE and GRANT
  - Functions and domains:
    - `CREATE FUNCTION … LANGUAGE SQL`; ALTER FUNCTION / ALTER ROUTINE (RENAME, OWNER, SET SCHEMA only)
    - COMMENT ON ROUTINE, DROP ROUTINE
    - CREATE DOMAIN
- **DML:**
  - `INSERT … VALUES / SELECT [ON CONFLICT]`
  - `UPDATE … FROM / WITH`
  - `DELETE … USING`
- **DCL:**
  - GRANT and REVOKE, including on ROUTINE, with CASCADE/RESTRICT
  - ALTER DEFAULT PRIVILEGES for TABLE, SEQUENCE, FUNCTION and TYPE
- **TCL:**
  - BEGIN / START TRANSACTION, COMMIT / END, ROLLBACK / ABORT
  - SET CONSTRAINTS
- **Utility:** EXPLAIN, and ANALYZE (on a single relation).

### 4.3 Feature status

| Feature | Status |
|---|---|
| Foreign keys | **Supported since 26 Aug 2026.** All referential actions, MATCH SIMPLE/FULL, DEFERRABLE (FKs only). On an existing table you must use `ADD CONSTRAINT … NOT VALID` and then `ALTER TABLE ASYNC … VALIDATE CONSTRAINT`. |
| Sequences / IDENTITY | **Supported since 13 Feb 2026.** bigint only. **`CACHE` is mandatory and must be 1 or ≥ 65536.** ORMs emit `GENERATED BY DEFAULT AS IDENTITY (CACHE 65536)`. currval/lastval across transactions are nondeterministic. |
| CHECK / UNIQUE / NOT NULL / DEFAULT / GENERATED STORED / LIKE | Supported. Constraints other than FKs are checked immediately. |
| Primary key | **Defined only at CREATE TABLE and immutable.** Without one, DSQL uses a hidden synthetic ID. At most 8 columns and 1 KiB. |
| Views | Supported, including recursive, check option, security_barrier/invoker and updatable views. **No temporary views.** |
| Functions | `LANGUAGE sql` only. No PL/pgSQL. `CREATE PROCEDURE` fails with `PROCEDURE is not supported`. |
| Triggers, temp tables, extensions, TRUNCATE, VACUUM, partitioning, tablespaces, RLS policies, LISTEN/NOTIFY, cursors, 2PC, large objects, materialized views | Unsupported [mostly inferred; error texts UNVERIFIED]. |
| SAVEPOINT | Unsupported (0A000). |
| `CREATE INDEX` without ASYNC | The docs say ASYNC is required. Troubleshooting shows the error `use CREATE INDEX ASYNC instead` for tables that already have rows. |
| Index features | Expression indexes (Aug 2026), partial indexes (Sep 2026), INCLUDE, NULLS FIRST/LAST, NULLS [NOT] DISTINCT. No operator classes. `pg_get_indexdef` reports the access method as `btree_index`. |
| Catch-all errors | `ERROR: unsupported statement`, `Error: Not supported` |

### 4.4 ALTER TABLE (UG p.69–74)

**Supported:**
- Renames and ownership:
  - RENAME [COLUMN | CONSTRAINT | TO]
  - SET SCHEMA, OWNER TO
- Columns:
  - ADD COLUMN [IF NOT EXISTS]
  - DROP COLUMN. The column still counts toward the 1,600 lifetime limit, and PK columns can't be dropped.
- ALTER COLUMN:
  - SET / DROP DEFAULT, DROP NOT NULL, DROP EXPRESSION, SET STORAGE
  - ADD GENERATED AS IDENTITY (with explicit CACHE), SET GENERATED, RESTART, DROP IDENTITY
- Constraints:
  - ALTER CONSTRAINT (FK deferrability)
  - ADD CHECK / FOREIGN KEY … **NOT VALID** (NOT VALID is required)
  - `ADD UNIQUE USING INDEX`
  - DROP CONSTRAINT
  - `ALTER TABLE ASYNC … VALIDATE CONSTRAINT`

**Not supported:**
- ALTER COLUMN TYPE
- SET NOT NULL
- Adding or changing a PK

### 4.5 Async jobs (UG p.138–143, 152–154)
- `CREATE INDEX ASYNC` and `ALTER TABLE ASYNC … VALIDATE` return one column, `job_id`, holding a 26-character base-32 ID.
- **`sys.jobs`** has these columns:
  - `job_id`
  - `status`: submitted, processing, completed or failed
  - `details`
  - `job_type`: INDEX_BUILD, ANALYZE or DROP; the type for VALIDATE is [UNVERIFIED]
  - `class_id`
  - `object_id`
  - `object_name`
  - `start_time`
  - `update_time`
- Finished jobs are purged after 30 minutes.
- **`sys.wait_for_job(job_id) → boolean`**.
- A failed index build leaves `pg_index.indisvalid = f`. A failed unique build reports the detail `Found duplicate key while validating index for UCVs`.
- DROP INDEX cancels a build that is still running.
- DROP TABLE and DROP INDEX create `DROP` jobs.

### 4.6 Catalogs and `sys.*` (UG p.144–155)
- **DSQL-specific objects:**
  - `sys.jobs`, `sys.iam_pg_role_mappings`, `sys.wait_for_job(text)`, `sys.dsql_major_version()`
  - Floci also emulates `sys.iam_session_user()`, `sys.current_session_id()` and `sys.cluster_size()`.
- **Catalogs present:**
  - pg_am, pg_attrdef, pg_attribute, pg_auth_members, pg_cast, pg_class, pg_collation, pg_constraint
  - pg_default_acl, pg_depend, pg_description, pg_index, pg_inherits, pg_namespace, pg_operator, pg_range, pg_statistic, pg_type
  - Views: pg_indexes, pg_roles, pg_settings, pg_tables, pg_views, pg_user, pg_stats, pg_stat_ssl
- **Marked absent:**
  - pg_authid, pg_database, pg_enum, pg_extension, pg_trigger, pg_policy, pg_language
  - pg_proc and pg_sequence(s). The docs contradict themselves here, since functions and sequences now exist. Verify these.
  - pg_stat_activity, pg_locks, pg_matviews
- **`information_schema`:** the docs don't mention it explicitly, but ORMs (Hibernate validation, Flyway, Prisma, Django inspectdb) use it. **The emulator must provide it.**
- **EXPLAIN** uses DSQL-specific node names: `Full Scan (btree-table)`, `Storage Scan`, `B-Tree Scan`, `Storage Lookup`, `Nested Loop (Batched Join)`. It has three filter levels. Fidelity here is **low priority** for CI.

---

## 5. Control-plane API (only matters if we also emulate it — see 05 for scope)

- **Protocol:** REST-JSON. apiVersion `2018-05-10`. SigV4 signing name `dsql`.
- **Endpoint:** `https://dsql.{region}.api.aws`.
- **16 operations:**
  - CreateCluster, GetCluster, UpdateCluster, DeleteCluster, ListClusters
  - Put/Get/DeleteClusterPolicy
  - CreateStream, GetStream, ListStreams, DeleteStream
  - GetVpcEndpointServiceName
  - Tag/Untag/ListTagsForResource
- **ClusterStatus values:** `CREATING|ACTIVE|IDLE|INACTIVE|UPDATING|DELETING|DELETED|FAILED|PENDING_SETUP|PENDING_DELETE`.
- **Deletion protection** defaults to on.
- **Waiters:** `ClusterActive`, `ClusterNotExists`, `StreamActive`, `StreamNotExists`.

The full shapes are in `sources/botocore/service-2.json` and `sources/dsql-api/`. Several Python/Java/Go emulators already implement all of this (MiniStack, Floci, LocalStack, moto; see 03).

## 6. Multi-region (observable to a client)
Two peered clusters with their own endpoints present **one strongly consistent logical database**. Writes from either Region can conflict (OC000). An emulator can model this as two endpoints that share one database.

## 7. Other behaviours
- **Lazy catalog refresh.** After DDL on another connection, a session may not see new objects until it reconnects or re-runs `SET search_path`. This is also the source of OC001.
- **Permissions.** `admin` owns `public`; other roles need `GRANT USAGE ON SCHEMA`.

## 8. SQL-support timeline (release notes)

| Date | Change |
|---|---|
| 27 May 2025 | GA |
| 3 Feb 2026 | NUMERIC indexable |
| 13 Feb 2026 | Sequences and IDENTITY |
| 24 Apr 2026 | ALTER DEFAULT PRIVILEGES, nested CREATE SCHEMA, role rename, ALTER ROUTINE |
| May–Jun 2026 | json/jsonb columns |
| 6 Jul 2026 | ALTER COLUMN DEFAULT/DROP NOT NULL/IDENTITY, DROP CONSTRAINT |
| 3 Aug 2026 | DROP COLUMN, NOT VALID + VALIDATE |
| 13 Aug 2026 | Expression indexes |
| 24–25 Aug 2026 | Expanded FOR UPDATE, FOR KEY SHARE, numeric(1000) |
| 26 Aug 2026 | Foreign keys |
| 27 Aug 2026 | Extended statistics |
| 11 Sep 2026 | Batched nested loop |
| 15 Sep 2026 | Partial indexes |

## 9. To verify against a live cluster
Dreamescaper's golden probe set may already answer many of these (see 03).

1. Error text and SQLSTATE for each of these:
   - DDL and DML mixed in one transaction, and more than one DDL per transaction
   - SAVEPOINT, TEMP TABLE, TRIGGER, EXTENSION, TRUNCATE, VACUUM
   - LISTEN, DECLARE CURSOR, PARTITION BY
   - isolation levels other than RR
   - CREATE INDEX without ASYNC on an empty table
2. SQLSTATEs for auth failures, and what happens when `dbname` is not `postgres`.
3. Idle timeout; minimum TLS version; behaviour when the 60-minute connection limit is reached.
4. Whether `pg_proc`, `pg_sequence(s)` and `information_schema` are present.
5. The `job_type` value for VALIDATE jobs.
6. Whether unknown startup parameters are tolerated.
7. The current `server_version` string.
