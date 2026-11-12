-- probe 21_column_types: column types DSQL refuses, and bare numeric = numeric(18,6).
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
-- Cases mined from MiniStack tests/test_dsql.py (TestColumnTypes).
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
CREATE TABLE p21_money (id int, c money);
CREATE TABLE p21_xml (id int, c xml);
CREATE TABLE p21_inet (id int, c inet);
CREATE TABLE p21_arr (id int, c int[]);
CREATE TABLE p21_point (id int, c point);
CREATE TABLE p21_bit (id int, c bit(8));
DROP TABLE IF EXISTS p21_ok;
CREATE TABLE p21_ok (id int PRIMARY KEY, n numeric, d numeric(10,2), j jsonb, u uuid, tz timetz, iv interval, b bytea, t timestamptz);
SELECT attname, format_type(atttypid, atttypmod) AS type
  FROM pg_attribute WHERE attrelid = 'p21_ok'::regclass AND attnum > 0 AND NOT attisdropped
  ORDER BY attnum;
-- Arrays are allowed at query time.
SELECT ARRAY[1, 2, 3] AS arr, '10.0.0.1'::inet AS addr;
DROP TABLE IF EXISTS p21_money;
DROP TABLE IF EXISTS p21_xml;
DROP TABLE IF EXISTS p21_inet;
DROP TABLE IF EXISTS p21_arr;
DROP TABLE IF EXISTS p21_point;
DROP TABLE IF EXISTS p21_bit;
DROP TABLE IF EXISTS p21_ok;
