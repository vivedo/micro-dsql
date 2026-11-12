# Data model: M0 spike

The spike has no persistent data. Its "entities" are result records printed by the scripts and tabulated in
`research/06-spike-findings.md`.

- **StepResult**: `script`, `step`, `outcome` (`OK` / `ERR` / `TIMEOUT` / `CRASH`), `sqlstate`, `message`.
- **DriverRow**: `driver`, `version`, `connect`, `crud`, `txn`, `params`, `notes`.
- **OccScenario**: `name`, `session` (A/B), `statement`, `outcome`, `sqlstate`, `message`, `final_state`,
  `crash_or_hang`.
- **ClaimVerdict**: `source` (02/04 + section), `claim`, `verdict` (CONFIRMED / REFUTED / NOT TESTED),
  `evidence`.

App schema used by `spike/app/schema.sql`: `customers(id uuid PK, email text unique, name text, vip bool,
created_at timestamptz)`, `products(id bigint PK, sku text unique, price numeric(12,2), stock int, attrs
jsonb)`, `orders(id bigint PK, customer_id uuid FK, status text, total numeric(12,2), placed_at
timestamptz)`, `order_items(order_id FK, product_id FK, qty int, unit_price numeric(12,2), PK(order_id,
product_id))`, `events(id bigint PK, payload jsonb, created_at timestamptz)`.
