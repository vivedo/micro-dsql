# Contract: tests/psql/run.sh

```
tests/psql/run.sh [--probes | --all] [--capture] [name ...]
```
- Default: layer-1 scenarios only. `--probes`: probes only. `--all`: both. Names restrict the run.
- `--capture`: write the normalised actual output to the expected file instead of comparing
  (status `CAPTURED`); only against the reference target, reviewed with `git diff`.
- Target: `PGHOST PGPORT PGUSER PGDATABASE PGPASSWORD PGSSLMODE` (defaults: `PGUSER=admin`,
  `PGDATABASE=postgres`, `PGSSLMODE=disable`, `PGCONNECT_TIMEOUT=5`).
- Optional env: `RESULTS_DIR` (write `<name>.out`, `<name>.diff`, `results.tsv`), `KNOWN_GAPS`
  (skip-list path, default `tests/psql/known-gaps.txt`).
- Output lines: `PASS <name>`, `FAIL <name>` (+ diff), `SKIP <name>: <reason>`, `MATCH <probe>`,
  `DIFFER <probe>` (+ diff), `CAPTURED <name>` (with `--capture`), then `summary: scenarios pass=N fail=N skip=N; probes match=N differ=N`.
- Exit: 0 all scenarios pass/skip; 1 a scenario failed; 2 target unreachable or psql missing.
  Probe mismatches never change the exit status.
- Normalisation before diff: drop `LOCATION:` lines; drop NOTICE lines ending in
  `does not exist, skipping` (and their verbose-mode code form).
- `results.tsv` columns: `kind<TAB>name<TAB>status`.
