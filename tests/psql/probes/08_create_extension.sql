-- probe 08_create_extension: Extensions are not supported by DSQL.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
CREATE EXTENSION pgcrypto;
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
SELECT length(gen_random_uuid()::text) AS builtin_uuid_length;
DROP EXTENSION IF EXISTS pgcrypto;
DROP EXTENSION IF EXISTS "uuid-ossp";
