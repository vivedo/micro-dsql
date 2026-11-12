-- Representative application queries (Q01..Q18). Each is preceded by an \echo label so the
-- results file can be mapped back to the table in research/06-spike-findings.md.
\set VERBOSITY verbose

\echo Q01 join + aggregate: revenue per customer
SELECT c.name, count(o.id) AS orders, coalesce(sum(o.total), 0) AS revenue
FROM customers c LEFT JOIN orders o ON o.customer_id = c.id
GROUP BY c.name
ORDER BY revenue DESC, c.name;

\echo Q02 three-way join + ORDER BY + LIMIT
SELECT o.id, c.email, p.sku, oi.qty, oi.qty * oi.unit_price AS line_total
FROM orders o
JOIN customers c ON c.id = o.customer_id
JOIN order_items oi ON oi.order_id = o.id
JOIN products p ON p.id = oi.product_id
ORDER BY o.placed_at DESC, p.sku
LIMIT 3;

\echo Q03 keyset pagination
SELECT id, status, placed_at FROM orders
WHERE (placed_at, id) < ('2026-10-07 00:00:00+00:00', 999)
ORDER BY placed_at DESC, id DESC
LIMIT 2;

\echo Q04 upsert ON CONFLICT DO UPDATE ... RETURNING
INSERT INTO products (id, sku, name, price, stock)
VALUES (2, 'MUG-BLUE', 'Blue mug', 9.99, 5)
ON CONFLICT (id) DO UPDATE SET stock = products.stock + EXCLUDED.stock
RETURNING id, sku, stock;

\echo Q05 ON CONFLICT DO NOTHING on a unique (non-PK) column
INSERT INTO customers (id, email, name)
VALUES ('44444444-4444-4444-4444-444444444444', 'ada@example.com', 'Ada again')
ON CONFLICT (email) DO NOTHING
RETURNING id;

\echo Q06 INSERT ... RETURNING with defaults (gen_random_uuid, now)
INSERT INTO customers (id, email, name)
VALUES (gen_random_uuid(), 'dora@example.com', 'Dora')
RETURNING length(id::text) AS uuid_len, vip, created_at IS NOT NULL AS has_ts;

\echo Q07 UPDATE ... RETURNING
UPDATE orders SET status = 'shipped' WHERE id = 102 RETURNING id, status;

\echo Q08 DELETE with ON DELETE CASCADE, then count children
DELETE FROM orders WHERE id = 101 RETURNING id;
SELECT count(*) AS items_of_101 FROM order_items WHERE order_id = 101;

\echo Q09 CTE
WITH paid AS (
    SELECT customer_id, sum(total) AS spent FROM orders WHERE status IN ('paid', 'shipped') GROUP BY customer_id
)
SELECT c.name, p.spent FROM paid p JOIN customers c ON c.id = p.customer_id ORDER BY p.spent DESC;

\echo Q10 window: latest order per customer (row_number)
SELECT customer_id, id, placed_at FROM (
    SELECT o.*, row_number() OVER (PARTITION BY customer_id ORDER BY placed_at DESC) AS rn FROM orders o
) t WHERE rn = 1 ORDER BY customer_id;

\echo Q11 window: running total (aggregate OVER)
SELECT id, total, sum(total) OVER (PARTITION BY customer_id ORDER BY placed_at) AS running
FROM orders ORDER BY customer_id, placed_at;

\echo Q12 window: rank() and lag()
SELECT id, total, rank() OVER (ORDER BY total DESC) AS rnk,
       lag(total) OVER (ORDER BY placed_at) AS prev_total
FROM orders ORDER BY placed_at;

\echo Q13 jsonb: -> / ->> / nested path / containment / jsonb_build_object
SELECT sku, attrs->>'color' AS color, attrs->'dims'->>'w' AS width FROM products ORDER BY id;
SELECT sku FROM products WHERE attrs @> '{"color": "red"}' ORDER BY sku;
SELECT sku FROM products WHERE attrs ? 'size';
SELECT jsonb_build_object('sku', sku, 'stock', stock) FROM products WHERE id = 1;
SELECT attrs #>> '{dims,h}' AS h FROM products WHERE id = 1;

\echo Q14 timestamptz: microsecond round trip, date_trunc, interval comparison
SELECT id, placed_at FROM orders WHERE id = 100;
SELECT created_at = '2026-01-02 03:04:05.123456+00:00'::timestamptz AS exact_match FROM customers WHERE name = 'Ada';
SELECT date_trunc('day', placed_at) AS day, count(*) FROM orders GROUP BY 1 ORDER BY 1;
SELECT count(*) FROM orders WHERE placed_at > '2026-10-07 00:00:00+00:00'::timestamptz - interval '3 days';

\echo Q15 numeric arithmetic and rounding
SELECT sum(qty * unit_price) AS gross, round(avg(price), 2) AS avg_price, max(price) - min(price) AS spread FROM order_items JOIN products ON products.id = order_items.product_id;
SELECT 0.1::numeric(12,2) + 0.2::numeric(12,2) AS exact_sum;

\echo Q16 bool + aggregate FILTER + EXISTS subquery
SELECT count(*) FILTER (WHERE vip) AS vips, count(*) FILTER (WHERE NOT vip) AS others FROM customers;
SELECT name FROM customers c WHERE EXISTS (SELECT 1 FROM orders o WHERE o.customer_id = c.id AND o.status = 'paid') ORDER BY name;

\echo Q17 constraint errors: FK violation (23503), unique violation (23505), CHECK violation (23514)
INSERT INTO orders (id, customer_id) VALUES (999, '99999999-9999-9999-9999-999999999999');
INSERT INTO customers (id, email, name) VALUES ('55555555-5555-5555-5555-555555555555', 'ada@example.com', 'dup');
UPDATE products SET price = -1 WHERE id = 1;

\echo Q18 uuid lookup + IN list + DISTINCT + HAVING
SELECT name FROM customers WHERE id = '22222222-2222-2222-2222-222222222222'::uuid;
SELECT DISTINCT status FROM orders WHERE id IN (100, 102, 103) ORDER BY status;
SELECT customer_id, count(*) FROM orders GROUP BY customer_id HAVING count(*) > 1;
