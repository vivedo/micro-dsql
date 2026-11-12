-- probe 27_alter_table: the ALTER TABLE subset DSQL supports.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
-- Cases mined from MiniStack tests/test_dsql.py (TestAlterTableSubset, TestForeignKeys, TestDropColumn).
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
DROP TABLE IF EXISTS p27_child;
DROP TABLE IF EXISTS p27_t;
CREATE TABLE p27_t (id int PRIMARY KEY, a int, b text);
CREATE TABLE p27_child (id int PRIMARY KEY, parent int);
INSERT INTO p27_t VALUES (1, 1, 'x');
ALTER TABLE p27_t ADD COLUMN c int;
ALTER TABLE p27_t ADD COLUMN d int DEFAULT 0;
ALTER TABLE p27_t ADD COLUMN e text NOT NULL;
ALTER TABLE p27_t ALTER COLUMN a TYPE bigint;
ALTER TABLE p27_t ALTER COLUMN b SET NOT NULL;
ALTER TABLE p27_t ALTER COLUMN b SET DEFAULT 'z';
ALTER TABLE p27_t ALTER COLUMN b DROP DEFAULT;
ALTER TABLE p27_t RENAME COLUMN c TO c2;
ALTER TABLE p27_t DROP COLUMN c2;
ALTER TABLE p27_t DROP COLUMN id;
ALTER TABLE p27_t ADD CONSTRAINT p27_chk CHECK (a > 0);
ALTER TABLE p27_t ADD CONSTRAINT p27_chk CHECK (a > 0) NOT VALID;
ALTER TABLE ASYNC p27_t VALIDATE CONSTRAINT p27_chk \gset
SELECT sys.wait_for_job(:'job_id') AS validate_job;
ALTER TABLE p27_child ADD CONSTRAINT p27_fk FOREIGN KEY (parent) REFERENCES p27_t (id);
ALTER TABLE p27_child ADD CONSTRAINT p27_fk FOREIGN KEY (parent) REFERENCES p27_t (id) NOT VALID;
INSERT INTO p27_child VALUES (1, 99);
ALTER TABLE p27_t ADD CONSTRAINT p27_uq UNIQUE (a);
ALTER TABLE p27_t DROP CONSTRAINT p27_chk;
SELECT attname FROM pg_attribute WHERE attrelid = 'p27_t'::regclass AND attnum > 0 AND NOT attisdropped ORDER BY attnum;
DROP TABLE IF EXISTS p27_child;
DROP TABLE IF EXISTS p27_t;
