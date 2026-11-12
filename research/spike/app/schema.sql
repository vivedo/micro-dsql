-- App-style schema standing in for "your app's SQL" (no target app exists).
-- A small shop: customers, products, orders, order lines, an event log.
\set VERBOSITY verbose

DROP TABLE IF EXISTS events;
DROP TABLE IF EXISTS order_items;
DROP TABLE IF EXISTS orders;
DROP TABLE IF EXISTS products;
DROP TABLE IF EXISTS customers;

CREATE TABLE customers (
    id          uuid        PRIMARY KEY,
    email       text        NOT NULL UNIQUE,
    name        text        NOT NULL,
    vip         boolean     NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE products (
    id     bigint        PRIMARY KEY,
    sku    text          NOT NULL UNIQUE,
    name   text          NOT NULL,
    price  numeric(12,2) NOT NULL CHECK (price >= 0),
    stock  integer       NOT NULL DEFAULT 0,
    attrs  jsonb         NOT NULL DEFAULT '{}'
);

CREATE TABLE orders (
    id           bigint        PRIMARY KEY,
    customer_id  uuid          NOT NULL REFERENCES customers (id),
    status       text          NOT NULL DEFAULT 'pending',
    total        numeric(12,2) NOT NULL DEFAULT 0,
    placed_at    timestamptz   NOT NULL DEFAULT now()
);

CREATE TABLE order_items (
    order_id    bigint        NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    product_id  bigint        NOT NULL REFERENCES products (id),
    qty         integer       NOT NULL CHECK (qty > 0),
    unit_price  numeric(12,2) NOT NULL,
    PRIMARY KEY (order_id, product_id)
);

CREATE TABLE events (
    id           bigint      PRIMARY KEY,
    customer_id  uuid        REFERENCES customers (id),
    kind         text        NOT NULL,
    payload      jsonb       NOT NULL,
    created_at   timestamptz NOT NULL
);

CREATE INDEX orders_customer_idx ON orders (customer_id, placed_at DESC);
CREATE INDEX orders_open_idx ON orders (status) WHERE status <> 'shipped';
CREATE UNIQUE INDEX customers_email_lower_idx ON customers (lower(email));
CREATE INDEX events_kind_idx ON events (kind, created_at);

-- Seed data. Timestamps carry microseconds on purpose (round-trip check in queries.sql).
-- Offsets are written as +00:00: Turso rejects the short "+00" form (see probes.sql P25).
INSERT INTO customers (id, email, name, vip, created_at) VALUES
    ('11111111-1111-1111-1111-111111111111', 'ada@example.com',   'Ada',   true,  '2026-01-02 03:04:05.123456+00:00'),
    ('22222222-2222-2222-2222-222222222222', 'brian@example.com', 'Brian', false, '2026-02-03 04:05:06.654321+00:00'),
    ('33333333-3333-3333-3333-333333333333', 'chen@example.com',  'Chen',  false, '2026-03-04 05:06:07.000001+00:00');

INSERT INTO products (id, sku, name, price, stock, attrs) VALUES
    (1, 'MUG-RED',  'Red mug',    9.99, 100, '{"color": "red",  "dims": {"w": 8, "h": 10}, "tags": ["kitchen", "gift"]}'),
    (2, 'MUG-BLUE', 'Blue mug',   9.99,  50, '{"color": "blue", "dims": {"w": 8, "h": 10}, "tags": ["kitchen"]}'),
    (3, 'TEE-L',    'T-shirt L', 19.50,  20, '{"color": "red",  "size": "L"}'),
    (4, 'POSTER',   'Poster',     4.25,   0, '{}');

INSERT INTO orders (id, customer_id, status, total, placed_at) VALUES
    (100, '11111111-1111-1111-1111-111111111111', 'shipped', 29.48, '2026-10-01 10:00:00.111111+00:00'),
    (101, '11111111-1111-1111-1111-111111111111', 'pending', 19.50, '2026-10-05 12:30:00.222222+00:00'),
    (102, '22222222-2222-2222-2222-222222222222', 'paid',     9.99, '2026-10-06 08:15:00.333333+00:00'),
    (103, '11111111-1111-1111-1111-111111111111', 'paid',     4.25, '2026-10-07 09:45:00.444444+00:00');

INSERT INTO order_items (order_id, product_id, qty, unit_price) VALUES
    (100, 1, 1, 9.99), (100, 3, 1, 19.50) ,
    (101, 3, 1, 19.50),
    (102, 2, 1, 9.99),
    (103, 4, 1, 4.25);

INSERT INTO events (id, customer_id, kind, payload, created_at) VALUES
    (1, '11111111-1111-1111-1111-111111111111', 'login',    '{"ip": "10.0.0.1", "ok": true}',  '2026-10-01 09:59:00+00:00'),
    (2, '11111111-1111-1111-1111-111111111111', 'checkout', '{"order": 100, "items": 2}',      '2026-10-01 10:00:01+00:00'),
    (3, '22222222-2222-2222-2222-222222222222', 'login',    '{"ip": "10.0.0.2", "ok": false}', '2026-10-06 08:00:00+00:00');
