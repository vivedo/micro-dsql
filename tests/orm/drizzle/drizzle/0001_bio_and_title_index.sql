ALTER TABLE "users" ADD COLUMN "bio" text;--> statement-breakpoint
CREATE INDEX ASYNC "posts_title_idx" ON "posts" USING btree ("title");