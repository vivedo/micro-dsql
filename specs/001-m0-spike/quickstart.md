# Quickstart: re-running the M0 spike

1. Build the server in a local Turso clone (record `git rev-parse HEAD`):
   `cargo build -p tursopg --release` (with `RUSTUP_TOOLCHAIN=stable` if 1.88 is unavailable).
2. Start it: `<turso-clone>/target/release/tursopg --server 127.0.0.1:5432`.
3. Install clients: `brew install libpq go`; `export PSQL="$(brew --prefix libpq)/bin/psql"`.
4. Install drivers: see `spike/README.md` (venv, `npm ci`, `go mod download`).
5. Run everything: `spike/run-all.sh` → results in `spike/results/`.

Expected: `SELECT 1` succeeds; each script prints per-step OK/ERR lines; the OCC script prints the failing
statement and SQLSTATE for each scenario. Interpretation is in `research/06-spike-findings.md`.
