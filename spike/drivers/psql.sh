#!/bin/sh
# Driver smoke test: psql (simple protocol; \bind uses the extended protocol with text parameters).
set -u
. "$(dirname "$0")/../env.sh"
"$PSQL" --version
"$PSQL" -X -e -v ON_ERROR_STOP=0 -v VERBOSITY=verbose <<'SQL'
\set VERBOSITY verbose
DROP TABLE IF EXISTS drv_psql;
CREATE TABLE drv_psql (id bigint PRIMARY KEY, name text NOT NULL, n int, amount numeric(10,2), ok bool, at timestamptz, u uuid);
INSERT INTO drv_psql VALUES (1, 'one', 1, 1.50, true, '2026-11-12 10:00:00.123456+00:00', '11111111-1111-1111-1111-111111111111');
SELECT * FROM drv_psql WHERE id = 1;
UPDATE drv_psql SET name = 'uno', n = n + 1 WHERE id = 1;
SELECT name, n FROM drv_psql WHERE id = 1;
-- explicit transaction: commit
BEGIN;
INSERT INTO drv_psql (id, name) VALUES (2, 'two');
UPDATE drv_psql SET n = 20 WHERE id = 2;
COMMIT;
-- explicit transaction: rollback
BEGIN;
INSERT INTO drv_psql (id, name) VALUES (3, 'three');
ROLLBACK;
SELECT id, name, n FROM drv_psql ORDER BY id;
-- parameterised (extended protocol, text format)
SELECT name FROM drv_psql WHERE id = $1 \bind 2 \g
INSERT INTO drv_psql (id, name, n, amount, ok, at, u) VALUES ($1, $2, $3, $4, $5, $6, $7) \bind 4 'four' 4 4.25 true '2026-11-12 10:00:00.5+00:00' '44444444-4444-4444-4444-444444444444' \g
SELECT * FROM drv_psql WHERE id = $1 \bind 4 \g
DELETE FROM drv_psql WHERE id = 1;
SELECT count(*) FROM drv_psql;
SQL
