# Drizzle on micro-dsql (M3 exit check)

A small Drizzle project written the way Aurora DSQL needs it, used to check that the DDL ORMs and
migrations emit runs on micro-dsql and that drizzle-orm queries work afterwards.

```sh
# start micro-dsql on a free port, then (from the repository root):
PGHOST=127.0.0.1 PGPORT=<port> PGPASSWORD=x scripts/drizzle-check.sh
```

Needs Node 22.6 or newer and npm (network access the first time, for `npm ci`). It is not part of
`cargo test --workspace`.

## What is here

| File | Role |
|---|---|
| `src/schema.ts` | `users` (bigint identity with `CACHE 65536`, unique index), `posts` (uuid, numeric, jsonb, foreign key to `users`, index), `counters` (`GENERATED ALWAYS` identity with `CACHE 1`) |
| `drizzle/` | the migrations `drizzle-kit generate` wrote, adapted for DSQL by hand (below), with their `meta/` |
| `migrate-dsql.mjs` | applies the migrations one statement per transaction, waits on every job, keeps its own journal `drizzle.__drizzle_migrations_dsql` |
| `smoke.mjs` | drizzle-orm queries: insert-returning, join, update, delete, transactions, constraint violations |
| `stock.mjs` | Drizzle's stock `migrate()`, which must fail with `42704 type "serial" does not exist`, as it does on DSQL |

Why a runner of our own: the stock migrator creates its journal with `id SERIAL PRIMARY KEY`
(DSQL has no serial) and applies all pending migrations in one transaction (DSQL allows one DDL
statement per transaction and no DDL together with DML). The runner reads the same migration files
through drizzle-orm's own `readMigrationFiles`. `MIGRATIONS_FOLDER` points it at another folder.

## Adapting generated migrations for DSQL

`drizzle-kit generate` writes PostgreSQL SQL. Each generated migration needs these edits, made by
hand once in the committed files (repeat them for a new migration):

1. Keys are `bigint` identities with an explicit `CACHE` (1 or at least 65536) in `src/schema.ts`
   (`generatedByDefaultAsIdentity({ cache: 65536 })`); an `integer` identity is refused by DSQL.
2. Foreign keys: `ADD CONSTRAINT ... FOREIGN KEY ...` gets `NOT VALID` appended, followed by a new
   statement `ALTER TABLE ASYNC "<table>" VALIDATE CONSTRAINT "<name>";` (the `0000_init.sql`
   statements for `posts_author_id_users_id_fk`).
3. Indexes: `CREATE INDEX` and `CREATE UNIQUE INDEX` become `CREATE [UNIQUE] INDEX ASYNC`
   (`USING btree` may stay).
4. Statements stay separated by `--> statement-breakpoint`, so the runner applies each in its own
   transaction. `ADD COLUMN` must be plain (`ALTER TABLE ... ADD COLUMN "bio" text;`): DSQL refuses a
   default or constraint on `ADD COLUMN`.

`0001_bio_and_title_index.sql` exercises a second migration (a plain column and an `ASYNC` index).
