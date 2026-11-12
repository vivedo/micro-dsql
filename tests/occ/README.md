# tests/occ: OCC scenarios

Two (or more) sessions against one in-process micro-dsql server, one scenario per cell of the
conflict matrix of research/01 section 2, plus one per knowingly different cell. The target is
compiled by `crates/server` (`[[test]] name = "occ"`) and runs with `cargo test --workspace`;
the harness is `crates/server/tests/smoke/common.rs`, the step helpers are in `main.rs`.

```sh
cargo test -p micro-dsql-server --test occ                    # the suite
cargo test -p micro-dsql-server --test occ autocommit_latency -- --ignored --nocapture   # SC-006 timing
```

DSQL reports every conflict at `COMMIT` of the later committer. micro-dsql reports it at
`COMMIT` of the later *writer*: inside an explicit transaction a write conflict is deferred
(the engine transaction is rolled back, re-opened on a fresh snapshot and the statement
replayed once; the transaction is doomed and its `COMMIT` fails with `40001` OC000).

## The matrix

`t (id int PRIMARY KEY, v int, u int UNIQUE)`, rows (1, 0, 1) and (2, 0, 2).

| Cell | A | B | DSQL | micro-dsql | Scenario |
|---|---|---|---|---|---|
| M1 | UPDATE r1, commits | UPDATE r1 (snapshot older than A's commit) | B's COMMIT: OC000 | B's UPDATE answers `UPDATE 1`; B's COMMIT: OC000 | `matrix::m1_update_update_conflict_is_reported_at_the_later_writers_commit` |
| M1 | UPDATE r1 (open) | UPDATE r1 | the later committer: OC000 | B's UPDATE answers `UPDATE 0`; B's COMMIT: OC000 | `matrix::m1_update_update_while_the_other_writer_is_still_open` |
| M2 | INSERT k | INSERT k (same PK, or same UNIQUE value) | later committer: OC000 | B's COMMIT: OC000 (unchanged engine path) | `matrix::m2_same_key_inserts_conflict_at_commit` |
| M3 | DELETE r | UPDATE r | OC000 | B's COMMIT: OC000 | `matrix::m3_delete_update_conflict` |
| M4 | UPDATE key column of r | UPDATE non-key column of r | OC000 | B's COMMIT: OC000 | `matrix::m4_key_update_against_non_key_update` |
| M5 | FOR UPDATE r | UPDATE r (both orders) | OC000 | the later toucher/writer's COMMIT: OC000 | `locking::m5_*` |
| M6 | FOR UPDATE r | FOR UPDATE r | OC000 | B's COMMIT: OC000 | `locking::m6_for_update_against_for_update` |
| M7 | FOR UPDATE r1 | write of other rows | no conflict | no conflict | `locking::m7_for_update_against_other_rows` |
| M8 | read r1, write r2 | read r2, write r1 | no conflict (write skew) | no conflict | `matrix::m8_write_skew_both_commit` |
| M9 | FOR KEY SHARE r | UPDATE (non-key) r | no conflict | no conflict | `locking::m9_for_key_share_against_a_non_key_update` |
| S7 | UPDATE t | concurrent ALTER TABLE | A's COMMIT: OC001 | A's COMMIT: OC001, never deferred | `matrix::schema_conflict_is_reported_at_commit_as_oc001` |
| S1d | BEGIN | other session commits r2 | B reads the old value | B reads the old value (snapshot pinned at BEGIN) | `snapshot::the_snapshot_is_taken_at_begin` |
| autocommit | 8 sessions x 150 INSERT / hot-row UPDATE | | no insert fails; conflicts are OC000 | same | `autocommit::autocommit_storm_never_fails_an_insert_and_only_reports_oc000` |
| retry loop | 4 sessions x 50 increments with retry on `40001` | | total 200 | total 200 | `autocommit::retry_loop_increments_converge` |

Doomed-transaction edge cases (ROLLBACK, `25P02`, age limit, nothing committed, no rows on a
repeated conflict, switch off) are in `matrix.rs`; autocommit row limit, multi-statement
atomicity, extended protocol and contention with DDL in `autocommit.rs`; FOR UPDATE forms and
refusals in `locking.rs` and `crates/server/tests/smoke/dsql_locking.rs`.

## Knowingly different cells

Each scenario in `differences.rs` asserts today's behaviour and names its row in
[docs/known-gaps.md](../../docs/known-gaps.md) (`OCC:` rows and B11).

| Cell | DSQL | micro-dsql | Scenario |
|---|---|---|---|
| D1 | FOR KEY SHARE conflicts with DELETE / key UPDATE | never conflicts | `d1_for_key_share_does_not_conflict_with_a_delete` |
| D2 | FK parent DELETE vs child INSERT: one fails | both commit, orphan child | `d2_parent_delete_and_child_insert_both_commit_leaving_an_orphan` |
| D3 | the first committer wins | the earlier writer wins | `d3_the_earlier_writer_wins_even_when_the_later_one_commits_first` |
| D4 | a rolled-back writer does not block | the second writer is still doomed | `d4_a_rolled_back_first_writer_still_dooms_the_second` |
| D4b | the transaction keeps its writes and snapshot | the doomed transaction restarts on a fresh snapshot | `d4b_a_doomed_transaction_reads_a_fresh_snapshot` |
| D5 / B11 | only conflicting transactions fail | any DDL fails writers (OC001); unrelated writers get OC000 while a DDL transaction is open | `d5_unrelated_ddl_conflicts_with_writers` |
| | not recorded | a table created after BEGIN is invisible (`42P01`; the catalog is part of the snapshot) | `tables_created_after_begin_are_invisible` |
| | | contention inside an explicit transaction: OC001, block aborted | `residual_contention_inside_an_explicit_transaction_is_oc001` |
| | NOWAIT / SKIP LOCKED unsupported ([inferred]) | accepted, no-ops | `nowait_and_skip_locked_are_no_ops` |

## Observations behind the design (2026-11-12, merged tree)

- Engine probes (two connections, one database, `LimboError` variants): autocommit storms hit
  `Busy` (~390 of 1,200 statements) when plain, 0 when each DML runs as `BEGIN; stmt; COMMIT`;
  a DDL transaction makes a concurrent DML COMMIT fail with `WriteWriteConflict` and a concurrent
  DDL statement fail with `Busy`; a bounded retry (up to 10 attempts, 2 ms steps, about 200 ms)
  removes the `Busy` of autocommit statements; any table read pins the snapshot (`SELECT 1` does
  not); DDL on a pinned snapshot that is out of date fails with `Busy` unless the transaction is
  re-opened; `UPDATE t SET c = c WHERE ...` conflicts exactly like a write.
- SC-006 (1,000 single-session autocommit INSERTs through `psql -f`, debug build, same host):
  0.30 s before M4 (3 runs: 0.31, 0.30, 0.30), 0.33 s after (0.34, 0.33, 0.33): about 1.1x, within
  the 2x budget.
- `tests/psql/run.sh --probes`: before M4 match 0 / differ 22, after match 1 / differ 21.
  `26_locking_reads` now matches the reference; `25_txn_row_limit` changed (an autocommit
  `INSERT ... SELECT` over 3,000 rows is now refused with `54000`, which the MiniStack reference
  does not do) and still differs; every other probe's output is byte-identical to before.
- Manual two-session walkthrough of the feature quickstart with `psql`: the deferred conflict
  (`UPDATE 1`, then `COMMIT` fails OC000, value is A's), the snapshot at `BEGIN` (old value), the
  `FOR UPDATE` touch (A's `COMMIT` fails OC000 after B's `FOR UPDATE`, B commits), the atomic
  string (`INSERT ...\; INSERT ...` with a duplicate key leaves 0 rows; plain `;` in `psql`
  splits the string into separate queries, so use `\;`) and the autocommit row limit (`54000`,
  table unchanged) all behave as specified. The `\bind` row could not be run (the installed
  `psql` has no `\bind`); the extended protocol is covered by
  `autocommit::extended_protocol_autocommit_storm_and_errors` and `dsql_locking.rs`.
