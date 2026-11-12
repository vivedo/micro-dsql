// drizzle-orm queries against the migrated schema: insert-returning with identity values,
// select with a join and a filter, update, delete, transactions (commit and rollback) and a
// foreign key violation.
import assert from "node:assert/strict";
import { and, eq } from "drizzle-orm";
import { drizzle } from "drizzle-orm/node-postgres";
import { counters, posts, users } from "./src/schema.ts";
import { connect, describe, pgError } from "./lib.mjs";

const client = await connect();
const db = drizzle(client);
const step = (name) => console.log(`ok  ${name}`);

try {
  const inserted = await db
    .insert(users)
    .values([
      { email: "ada@example.com", name: "Ada" },
      { email: "grace@example.com", name: "Grace" },
    ])
    .returning({ id: users.id, active: users.active });
  assert.deepEqual(inserted, [
    { id: 1, active: true },
    { id: 2, active: true },
  ]);
  step("insert ... returning gives identity values 1 and 2");

  const [post] = await db
    .insert(posts)
    .values({ authorId: 1, title: "Notes", price: "9.50", meta: { tags: ["a", "b"] } })
    .returning();
  assert.match(post.id, /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
  assert.equal(post.price, "9.50");
  assert.deepEqual(post.meta, { tags: ["a", "b"] });
  step("uuid default, numeric and jsonb round-trip");

  const joined = await db
    .select({ title: posts.title, author: users.name })
    .from(posts)
    .innerJoin(users, eq(posts.authorId, users.id))
    .where(and(eq(users.email, "ada@example.com"), eq(posts.title, "Notes")));
  assert.deepEqual(joined, [{ title: "Notes", author: "Ada" }]);
  step("select with join and where");

  const [updated] = await db
    .update(users)
    .set({ active: false })
    .where(eq(users.id, 2))
    .returning({ id: users.id, active: users.active });
  assert.deepEqual(updated, { id: 2, active: false });
  step("update ... returning");

  await db.insert(counters).values({}).returning();
  const next = await db.insert(counters).values({}).returning();
  assert.equal(next[0].id, 2);
  assert.equal(next[0].n, 0);
  step("generated always identity and column defaults");

  await db.transaction(async (tx) => {
    await tx.insert(users).values({ email: "linus@example.com", name: "Linus" });
  });
  assert.equal((await db.select().from(users).where(eq(users.email, "linus@example.com"))).length, 1);
  step("transaction commits");

  await assert.rejects(
    db.transaction(async (tx) => {
      await tx.insert(users).values({ email: "temp@example.com", name: "Temp" });
      throw new Error("rollback please");
    }),
    /rollback please/,
  );
  assert.equal((await db.select().from(users).where(eq(users.email, "temp@example.com"))).length, 0);
  step("transaction rolls back");

  await assert.rejects(db.insert(posts).values({ authorId: 999, title: "Orphan" }), (error) => {
    assert.equal(pgError(error).code, "23503", describe(error));
    return true;
  });
  step("foreign key violation is 23503");

  await assert.rejects(db.insert(users).values({ email: "ada@example.com" }), (error) => {
    assert.equal(pgError(error).code, "23505", describe(error));
    return true;
  });
  step("unique index violation is 23505");

  await db.delete(posts).where(eq(posts.id, post.id));
  assert.equal((await db.select().from(posts)).length, 0);
  step("delete");
  console.log("smoke OK");
} catch (error) {
  console.error(`SMOKE FAILED: ${describe(error)}`);
  if (error.stack) console.error(error.stack.split("\n").slice(0, 6).join("\n"));
  process.exitCode = 1;
} finally {
  await client.end();
}
