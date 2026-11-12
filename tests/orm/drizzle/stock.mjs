// Drizzle's stock migrator must fail on micro-dsql where it fails on DSQL: its journal table is
// created with `id SERIAL PRIMARY KEY`, and DSQL has no serial type.
import { drizzle } from "drizzle-orm/node-postgres";
import { migrate } from "drizzle-orm/node-postgres/migrator";
import { connect, describe, migrationsFolder, pgError } from "./lib.mjs";

const client = await connect();
try {
  await migrate(drizzle(client), { migrationsFolder });
  console.error("STOCK MIGRATOR UNEXPECTEDLY SUCCEEDED");
  process.exitCode = 1;
} catch (error) {
  const cause = pgError(error);
  if (cause.code === "42704" && cause.message === 'type "serial" does not exist') {
    console.log(`stock migrator failed as on DSQL: ${describe(error)}`);
  } else {
    console.error(`STOCK MIGRATOR FAILED DIFFERENTLY: ${describe(error)}`);
    process.exitCode = 1;
  }
} finally {
  await client.end();
}
