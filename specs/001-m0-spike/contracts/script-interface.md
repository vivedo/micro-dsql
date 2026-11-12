# Contract: spike script interface

Every script under `spike/` follows the same interface:

- **Input**: standard libpq environment variables `PGHOST`, `PGPORT`, `PGUSER`, `PGDATABASE`,
  `PGPASSWORD`, `PGSSLMODE`. `spike/env.sh` sets defaults (`127.0.0.1`, `5432`, `admin`, `postgres`,
  an arbitrary password, `disable`) without overriding values already set.
- **psql location**: `PSQL` env var, else `psql` on `PATH`.
- **Output**: one line per step on stdout: `[<script>] <step>: OK [detail]` or
  `[<script>] <step>: ERR <SQLSTATE> <message>`.
- **Exit code**: 0 if all steps ran (errors are findings, not script failures) and the server stayed up;
  1 if the script could not connect at all or the server crashed / a step timed out.
