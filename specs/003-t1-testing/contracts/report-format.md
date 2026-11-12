# Contract: reports/diff-<YYYY-MM-DD>.md

1. Title, date, the two targets (name, host:port, reachable/unreachable, server_version).
2. Summary line per target (run.sh summary).
3. Table: `| item | kind | <A> | <B> | same output? |`, one row per scenario and probe.
4. "Differences" section: for each item whose outputs differ between A and B, a fenced `diff`
   block (`diff -u A B`); then a "Failures against expected output" section with a target's diff vs expected, shown
   only when the other target did not pass/match either (otherwise the A/B diff already shows it).
5. "Backlog candidates": probes where A differs from the reference expected output.
