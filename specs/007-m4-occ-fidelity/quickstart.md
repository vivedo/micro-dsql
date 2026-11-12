# Quickstart: validating M4 — OCC fidelity

## Automated

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace                 # includes the OCC suite (target `occ`, tests/occ/)
cargo test -p micro-dsql-server --test occ -- --nocapture   # suite only
```

Expected: all green; `tests/occ/differences.rs` scenarios pass by asserting today's (documented)
behaviour, each naming its docs/known-gaps.md entry.

## Layer-1 psql gate

```sh
cargo run -p micro-dsql -- --listen 127.0.0.1:15507 &
export PGHOST=127.0.0.1 PGPORT=15507 PGUSER=admin PGDATABASE=postgres PGPASSWORD=x PGSSLMODE=disable
tests/psql/run.sh                       # 5/5 PASS, expected outputs unchanged
tests/psql/run.sh --probes 26_locking_reads   # report-only; should now match the reference
```

## Manual two-session checks (psql, `\set VERBOSITY verbose`)

| Check | Session B | Session A | Expect |
|---|---|---|---|
| Deferred conflict | `BEGIN; SELECT v FROM t WHERE id=1;` | `UPDATE t SET v=10 WHERE id=1;` | — |
| | `UPDATE t SET v=20 WHERE id=1;` → `UPDATE 1`; `COMMIT;` | | COMMIT: `40001 … (OC000)`; `v = 10` |
| Snapshot at BEGIN | `BEGIN;` | `UPDATE t SET v=555 WHERE id=2;` | — |
| | `SELECT v FROM t WHERE id=2;` | | old value, not 555 |
| FOR UPDATE | `BEGIN; SELECT v FROM t WHERE id=1 FOR UPDATE;` | `BEGIN; UPDATE t SET v=1 WHERE id=1; COMMIT;` | A's COMMIT fails OC000 (A wrote after B's touch); B commits |
| Autocommit atomic string | `INSERT INTO t VALUES (30,0); INSERT INTO t VALUES (30,0);` | | `23505`; `SELECT count(*) FROM t WHERE id=30` = 0 |
| Autocommit row limit | `INSERT INTO big SELECT g FROM generate_series(1,3001) g;` | | `54000`; table unchanged |

## Owner / orchestrator checks (not automated)

- MiniStack differential of `tests/psql/probes/26_locking_reads.sql` (Docker, report-only).
- Live DSQL: confirm the "who loses" and FOR KEY SHARE rows of data-model.md's matrix, and the
  locking-clause refusal text for joins.
