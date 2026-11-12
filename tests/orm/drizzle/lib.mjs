// Shared by the runners: connection settings from the libpq variables, and exit status 2
// (prerequisite missing) when the server cannot be reached.
import pg from "pg";

// `MIGRATIONS_FOLDER` points the runners at another folder of Drizzle migrations.
export const migrationsFolder =
  process.env.MIGRATIONS_FOLDER ?? new URL("./drizzle", import.meta.url).pathname;

export function config() {
  return {
    host: process.env.PGHOST ?? "127.0.0.1",
    port: Number(process.env.PGPORT ?? 5432),
    user: process.env.PGUSER ?? "admin",
    password: process.env.PGPASSWORD ?? "x",
    database: process.env.PGDATABASE ?? "postgres",
    ssl: false,
  };
}

const UNREACHABLE = new Set(["ECONNREFUSED", "ENOTFOUND", "EHOSTUNREACH", "ETIMEDOUT"]);

/** Opens a client; an unreachable server ends the process with status 2. */
export async function connect() {
  const client = new pg.Client(config());
  try {
    await client.connect();
  } catch (error) {
    if (UNREACHABLE.has(error.code) || error.errors?.some((e) => UNREACHABLE.has(e.code))) {
      console.error(`UNREACHABLE target ${client.host}:${client.port}: ${error.code ?? error.message}`);
      process.exit(2);
    }
    throw error;
  }
  return client;
}

/** drizzle-orm wraps the driver's error in a DrizzleQueryError; the server's error is its cause. */
export function pgError(error) {
  return error?.cause?.code ? error.cause : error;
}

/** Prints an error the way the runners report failures. */
export function describe(error) {
  const e = pgError(error);
  return `${e.code ?? "error"} ${e.message}`;
}
