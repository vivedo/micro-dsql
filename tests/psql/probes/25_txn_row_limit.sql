-- probe 25_txn_row_limit: 3,000 mutated rows per transaction.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
-- Cases mined from MiniStack tests/test_dsql.py (TestTxnLimits).
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
-- \gexec lines run with ECHO none so the generated 3,000-row statements are not echoed.
DROP TABLE IF EXISTS p25_t;
CREATE TABLE p25_t (id int PRIMARY KEY);
-- 3,000 rows in one VALUES list: allowed.
\set ECHO none
\echo '1..3000 rows via VALUES'
SELECT 'INSERT INTO p25_t VALUES ' || string_agg('(' || g || ')', ',') FROM generate_series(1, 3000) g \gexec
\set ECHO all
SELECT count(*) AS after_3000 FROM p25_t;
DELETE FROM p25_t;
-- 3,001 rows in one VALUES list: refused.
\set ECHO none
\echo '1..3001 rows via VALUES'
SELECT 'INSERT INTO p25_t VALUES ' || string_agg('(' || g || ')', ',') FROM generate_series(1, 3001) g \gexec
\set ECHO all
SELECT count(*) AS after_3001 FROM p25_t;
-- 3,001 rows from INSERT ... SELECT: DSQL refuses; the reference only counts VALUES rows.
INSERT INTO p25_t SELECT g FROM generate_series(1, 3001) g;
SELECT count(*) AS after_select_3001 FROM p25_t;
DELETE FROM p25_t WHERE id <= 1500;
DELETE FROM p25_t;
-- Cumulative: 2,000 + 2,000 rows in one transaction.
BEGIN;
\set ECHO none
\echo '1..2000 rows via VALUES'
SELECT 'INSERT INTO p25_t VALUES ' || string_agg('(' || g || ')', ',') FROM generate_series(1, 2000) g \gexec
\set ECHO all
\set ECHO none
\echo '2001..4000 rows via VALUES'
SELECT 'INSERT INTO p25_t VALUES ' || string_agg('(' || g || ')', ',') FROM generate_series(2001, 4000) g \gexec
\set ECHO all
COMMIT;
SELECT count(*) AS after_cumulative FROM p25_t;
DROP TABLE p25_t;
