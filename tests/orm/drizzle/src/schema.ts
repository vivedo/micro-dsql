import {
  bigint,
  boolean,
  index,
  integer,
  jsonb,
  numeric,
  pgTable,
  text,
  timestamp,
  uniqueIndex,
  uuid,
  varchar,
} from "drizzle-orm/pg-core";

// DSQL has no serial columns: keys are bigint identities with an explicit CACHE (1 or >= 65536).
export const users = pgTable(
  "users",
  {
    id: bigint("id", { mode: "number" })
      .primaryKey()
      .generatedByDefaultAsIdentity({ cache: 65536 }),
    email: varchar("email", { length: 255 }).notNull(),
    name: text("name"),
    bio: text("bio"),
    active: boolean("active").notNull().default(true),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull().defaultNow(),
  },
  (t) => [uniqueIndex("users_email_idx").on(t.email)],
);

export const posts = pgTable(
  "posts",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    authorId: bigint("author_id", { mode: "number" })
      .notNull()
      .references(() => users.id),
    title: text("title").notNull(),
    price: numeric("price", { precision: 10, scale: 2 }),
    meta: jsonb("meta"),
  },
  (t) => [index("posts_author_idx").on(t.authorId), index("posts_title_idx").on(t.title)],
);

export const counters = pgTable("counters", {
  id: bigint("id", { mode: "number" }).primaryKey().generatedAlwaysAsIdentity({ cache: 1 }),
  n: integer("n").notNull().default(0),
});
