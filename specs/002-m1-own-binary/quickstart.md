# Quickstart / validation: M1

```sh
cargo build -p micro-dsql
target/debug/micro-dsql --listen 127.0.0.1:55433 &          # logs "listening on 127.0.0.1:55433"
psql "host=127.0.0.1 port=55433 user=admin dbname=postgres password=anything sslmode=disable" \
     -c 'SHOW server_version' -c 'SHOW timezone' -c 'SELECT current_database()'
```

Expected: `16.13`, `UTC`, `postgres`.

Gates:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
PGHOST=127.0.0.1 PGPORT=55433 PGPASSWORD=x tests/psql/run.sh
kill %1     # graceful shutdown, exit 0
```

Port 0: `micro-dsql --listen 127.0.0.1:0` logs the chosen port.
Persistence: `micro-dsql --data /tmp/x.db`, write, restart, read back.
