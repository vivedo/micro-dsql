# Quickstart: T1 harness validation

Prerequisites: Docker, `psql` on PATH (psql 16 recommended), optionally AWS CLI v2.

1. Bring up the reference and load its connection variables:
   `eval "$(scripts/ministack-up.sh)"`
2. Layer 1 against the reference (expect 5 × PASS, exit 0): `tests/psql/run.sh`
3. Probes against the reference (expect all MATCH): `tests/psql/run.sh --probes`
4. Differential with micro-dsql down (expect a report marking it unreachable):
   `TARGET_B_HOST=$PGHOST TARGET_B_PORT=$PGPORT TARGET_A_HOST=127.0.0.1 TARGET_A_PORT=15432 scripts/compare.sh`
   then open `reports/diff-<date>.md`.
5. Tear down: `scripts/ministack-down.sh`

After M1 (Reconcile): `micro-dsql --listen 127.0.0.1:15432 &`, then
`PGHOST=127.0.0.1 PGPORT=15432 tests/psql/run.sh` and re-run step 4 with both targets up.
