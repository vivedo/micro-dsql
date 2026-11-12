-- 10_crud: create, insert, select, update, delete, drop in autocommit mode.
DROP TABLE IF EXISTS t1_crud;
CREATE TABLE t1_crud (id int PRIMARY KEY, name text NOT NULL, qty int);
INSERT INTO t1_crud (id, name, qty) VALUES (1, 'apple', 10), (2, 'banana', 20), (3, 'cherry', 30)
RETURNING id, name, qty;
SELECT id, name, qty FROM t1_crud WHERE qty >= 20 ORDER BY id DESC;
UPDATE t1_crud SET qty = qty + 1 WHERE id = 2 RETURNING id, qty;
SELECT id, name, qty FROM t1_crud ORDER BY id;
DELETE FROM t1_crud WHERE id = 1 RETURNING id;
SELECT count(*) AS remaining FROM t1_crud;
DROP TABLE t1_crud;
