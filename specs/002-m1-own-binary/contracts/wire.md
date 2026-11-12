# Contract: wire behaviour

- SSLRequest → `N`; GSSENCRequest → `N`; direct TLS → connection closed.
- Startup: AuthenticationOk without a password exchange; ParameterStatus set from research R4;
  BackendKeyData; ReadyForQuery `I`.
- Terminate (`X`) → server closes the socket.
- `SHOW server_version` → `16.13`; `SHOW server_version_num` → `160013`; `SHOW timezone` → `UTC`
  (column `TimeZone`); `SHOW is_superuser` → `off`; `SHOW server_encoding` → `UTF8`;
  `SHOW integer_datetimes` → `on`; `SHOW transaction_isolation` / `SHOW default_transaction_isolation`
  → `repeatable read`; `SHOW in_hot_standby` → `off`.
- `SET [SESSION|LOCAL] timezone|TIME ZONE` to `UTC`, `Etc/UTC`, `GMT`, `Z`, `UTC0`, `+00`,
  `00:00` (case-insensitive) → `SET`; other values → `22023`.
- Errors: ErrorResponse with S/C/M and, when the engine provides them, D/t/c/n fields.
- `current_database()` → `postgres` (memory mode).
