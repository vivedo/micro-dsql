// Command micro-dsql-compare replays nothing: it compares two recorded suites with the
// conformance package of github.com/Dreamescaper/dsql-emulator, the golden record made against
// Aurora DSQL and the record scripts/conformance.sh made against micro-dsql, and writes a report.
//
// It is copied into the fetched upstream tree (cmd/micro-dsql-compare/) at run time, because
// the conformance package is internal to that module.
//
//	micro-dsql-compare <golden-dir> <recorded-dir> <report-path> <threshold-percent>
//
// Optional environment, written into the report header: CONFORMANCE_COMMIT, CONFORMANCE_TARGET.
// The last line printed is `TOTAL passed/total (pct%)`. Exit status 1 when the pass rate is
// below the threshold, 2 on a usage or input error.
package main

import (
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/Dreamescaper/dsql-emulator/internal/conformance"
)

func main() {
	if len(os.Args) != 5 {
		fmt.Fprintln(os.Stderr, "usage: micro-dsql-compare <golden-dir> <recorded-dir> <report-path> <threshold-percent>")
		os.Exit(2)
	}
	goldenDir, recordedDir, reportPath := os.Args[1], os.Args[2], os.Args[3]
	threshold, err := strconv.ParseFloat(os.Args[4], 64)
	if err != nil {
		fmt.Fprintf(os.Stderr, "threshold: %v\n", err)
		os.Exit(2)
	}
	golden, err := conformance.LoadDir(goldenDir)
	if err != nil {
		fmt.Fprintf(os.Stderr, "golden: %v\n", err)
		os.Exit(2)
	}
	recorded, err := conformance.LoadDir(recordedDir)
	if err != nil {
		fmt.Fprintf(os.Stderr, "recorded: %v\n", err)
		os.Exit(2)
	}

	diffs := conformance.CompareSuites(golden, recorded)
	failures := conformance.Failures(diffs)
	notes := 0
	for _, d := range diffs {
		if d.Advisory {
			notes++
		}
	}

	failed := map[string]bool{}
	for _, d := range failures {
		failed[d.Case] = true
	}
	type tally struct{ pass, total int }
	groups := map[string]*tally{}
	pass := 0
	for _, c := range golden.Cases {
		t := groups[c.Group]
		if t == nil {
			t = &tally{}
			groups[c.Group] = t
		}
		t.total++
		if !failed[c.Name] {
			t.pass++
			pass++
		}
	}
	total := len(golden.Cases)
	pct := 0.0
	if total > 0 {
		pct = 100 * float64(pass) / float64(total)
	}

	var b strings.Builder
	fmt.Fprintf(&b, "# micro-dsql conformance against live-DSQL recordings\n\n")
	fmt.Fprintf(&b, "- pinned upstream commit: %s\n", orUnknown(os.Getenv("CONFORMANCE_COMMIT")))
	fmt.Fprintf(&b, "- target: %s\n", orUnknown(os.Getenv("CONFORMANCE_TARGET")))
	fmt.Fprintf(&b, "- server version: %s\n", orUnknown(recorded.ServerVersion))
	fmt.Fprintf(&b, "- golden record: %s, recorded %s\n", golden.Target, golden.RecordedAt.UTC().Format(time.RFC3339))
	fmt.Fprintf(&b, "- compared at: %s\n", time.Now().UTC().Format(time.RFC3339))
	fmt.Fprintf(&b, "- advisory notes (messages and other advisory differences): %d\n\n", notes)
	fmt.Fprintf(&b, "**TOTAL %d/%d (%.1f%%)**, threshold %.0f%%\n\n", pass, total, pct, threshold)
	fmt.Fprintf(&b, "| group | pass | total |\n|---|---|---|\n")
	names := make([]string, 0, len(groups))
	for name := range groups {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, name := range names {
		fmt.Fprintf(&b, "| %s | %d | %d |\n", name, groups[name].pass, groups[name].total)
	}
	fmt.Fprintf(&b, "\n## Failing cases\n\n| case | field | step SQL | golden | micro-dsql |\n|---|---|---|---|---|\n")
	for _, d := range failures {
		sql := ""
		if d.Step < len(d.Steps) {
			sql = d.Steps[d.Step]
		}
		fmt.Fprintf(&b, "| %s | %s | `%s` | %s | %s |\n", d.Case, d.Field, cell(sql), cell(d.Golden), cell(d.Emulator))
	}
	if err := os.WriteFile(reportPath, []byte(b.String()), 0o644); err != nil {
		fmt.Fprintf(os.Stderr, "report: %v\n", err)
		os.Exit(2)
	}

	fmt.Printf("TOTAL %d/%d (%.1f%%)\n", pass, total, pct)
	if pct < threshold {
		os.Exit(1)
	}
}

func orUnknown(s string) string {
	if s == "" {
		return "unknown"
	}
	return s
}

// cell makes text safe for a markdown table cell.
func cell(s string) string {
	s = strings.ReplaceAll(s, "|", "\\|")
	s = strings.ReplaceAll(s, "\n", " ")
	return s
}
