import { defineConfig } from "drizzle-kit";

// `drizzle-kit generate` only writes SQL; it does not connect. The connection settings are for
// `drizzle-kit` commands that do, and come from the libpq variables like every other script here.
export default defineConfig({
  dialect: "postgresql",
  schema: "./src/schema.ts",
  out: "./drizzle",
  dbCredentials: {
    host: process.env.PGHOST ?? "127.0.0.1",
    port: Number(process.env.PGPORT ?? 5432),
    user: process.env.PGUSER ?? "admin",
    password: process.env.PGPASSWORD ?? "x",
    database: process.env.PGDATABASE ?? "postgres",
    ssl: false,
  },
});
