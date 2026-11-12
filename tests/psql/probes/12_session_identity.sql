-- probe 12_session_identity: Role, version and session parameters the client sees.
-- Report-only. Expected output is the MiniStack (DSQL_STRICT=1) reference behaviour.
\set ON_ERROR_STOP 0
\set VERBOSITY verbose
\set ECHO all
SELECT current_user, session_user;
SHOW is_superuser;
SHOW server_version;
SHOW server_encoding;
SHOW client_encoding;
SHOW DateStyle;
SHOW standard_conforming_strings;
SHOW integer_datetimes;
SELECT datcollate FROM pg_database WHERE datname = current_database();
CREATE DATABASE p12_db;
