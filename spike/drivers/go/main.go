// Driver smoke test: pgx v5. Settings from PG* env vars. pgx uses the extended protocol
// with binary parameters by default; a second pass uses QueryExecModeSimpleProtocol.
package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"runtime/debug"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgtype"
)

var failures int

func step(name, label string, fn func(ctx context.Context) (any, error)) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	res, err := fn(ctx)
	if err != nil {
		failures++
		var pgErr *pgconn.PgError
		if errors.As(err, &pgErr) {
			fmt.Printf("[%s] %s: ERR %s %s\n", name, label, pgErr.Code, pgErr.Message)
		} else {
			fmt.Printf("[%s] %s: ERR client %s\n", name, label, strings.SplitN(err.Error(), "\n", 2)[0])
		}
		return
	}
	if res == nil {
		fmt.Printf("[%s] %s: OK\n", name, label)
	} else {
		fmt.Printf("[%s] %s: OK %v\n", name, label, res)
	}
}

func run(name string, mode pgx.QueryExecMode) {
	cfg, err := pgx.ParseConfig("")
	if err != nil {
		fmt.Printf("[%s] config: ERR %v\n", name, err)
		os.Exit(1)
	}
	cfg.DefaultQueryExecMode = mode
	cfg.ConnectTimeout = 5 * time.Second
	ctx := context.Background()
	conn, err := pgx.ConnectConfig(ctx, cfg)
	if err != nil {
		fmt.Printf("[%s] connect: ERR %v\n", name, err)
		failures++
		return
	}
	defer conn.Close(ctx)
	fmt.Printf("[%s] connect: OK server_version=%s\n", name, conn.PgConn().ParameterStatus("server_version"))
	tbl := "drv_pgx_" + strings.ReplaceAll(name, "-", "_")
	ts := time.Date(2026, 11, 12, 10, 0, 0, 123456000, time.UTC)
	u := pgtype.UUID{Bytes: [16]byte{0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11}, Valid: true}
	exec := func(sql string, args ...any) func(ctx context.Context) (any, error) {
		return func(ctx context.Context) (any, error) {
			tag, err := conn.Exec(ctx, sql, args...)
			return tag.String(), err
		}
	}
	step(name, "drop", exec("DROP TABLE IF EXISTS "+tbl))
	step(name, "create", exec("CREATE TABLE "+tbl+" (id bigint PRIMARY KEY, name text NOT NULL, n int, amount numeric(10,2), ok bool, at timestamptz, u uuid)"))
	step(name, "select 1 (no params)", func(ctx context.Context) (any, error) {
		var v int
		err := conn.QueryRow(ctx, "SELECT 1").Scan(&v)
		return v, err
	})
	step(name, "insert params bigint+text", exec("INSERT INTO "+tbl+" (id, name) VALUES ($1, $2)", int64(1), "one"))
	step(name, "insert params all types", exec("INSERT INTO "+tbl+" VALUES ($1, $2, $3, $4, $5, $6, $7)",
		int64(2), "two", int32(2), "2.50", true, ts, u))
	step(name, "select by param", func(ctx context.Context) (any, error) {
		var s string
		err := conn.QueryRow(ctx, "SELECT name FROM "+tbl+" WHERE id = $1", int64(1)).Scan(&s)
		return s, err
	})
	step(name, "select all types", func(ctx context.Context) (any, error) {
		var n int32
		var amt pgtype.Numeric
		var ok bool
		var at time.Time
		var uu pgtype.UUID
		err := conn.QueryRow(ctx, "SELECT n, amount, ok, at, u FROM "+tbl+" WHERE id = $1", int64(2)).Scan(&n, &amt, &ok, &at, &uu)
		f, _ := amt.Float64Value()
		return fmt.Sprintf("n=%d amount=%v ok=%v at=%s (sent %s) u=%x", n, f.Float64, ok, at.UTC().Format(time.RFC3339Nano), ts.Format(time.RFC3339Nano), uu.Bytes), err
	})
	step(name, "update by param", exec("UPDATE "+tbl+" SET name = $1 WHERE id = $2", "uno", int64(1)))
	step(name, "explicit txn commit", func(ctx context.Context) (any, error) {
		tx, err := conn.Begin(ctx)
		if err != nil {
			return nil, err
		}
		if _, err = tx.Exec(ctx, "INSERT INTO "+tbl+" (id, name) VALUES ($1, $2)", int64(3), "three"); err != nil {
			tx.Rollback(ctx)
			return nil, err
		}
		if _, err = tx.Exec(ctx, "UPDATE "+tbl+" SET n = 30 WHERE id = 3"); err != nil {
			tx.Rollback(ctx)
			return nil, err
		}
		return nil, tx.Commit(ctx)
	})
	step(name, "explicit txn rollback", func(ctx context.Context) (any, error) {
		tx, err := conn.Begin(ctx)
		if err != nil {
			return nil, err
		}
		tx.Exec(ctx, "INSERT INTO "+tbl+" (id, name) VALUES (4, 'four')")
		if err := tx.Rollback(ctx); err != nil {
			return nil, err
		}
		var n int64
		err = conn.QueryRow(ctx, "SELECT count(*) FROM "+tbl+" WHERE id = 4").Scan(&n)
		return fmt.Sprintf("rows with id 4: %d", n), err
	})
	step(name, "delete by param", exec("DELETE FROM "+tbl+" WHERE id = $1", int64(1)))
	step(name, "count", func(ctx context.Context) (any, error) {
		var n int64
		err := conn.QueryRow(ctx, "SELECT count(*) FROM "+tbl).Scan(&n)
		return n, err
	})
	step(name, "error SQLSTATE (dup key)", exec("INSERT INTO "+tbl+" (id, name) VALUES (2, 'dup')"))
}

func main() {
	if bi, ok := debug.ReadBuildInfo(); ok {
		for _, d := range bi.Deps {
			if d.Path == "github.com/jackc/pgx/v5" {
				fmt.Printf("[pgx] version pgx %s, %s\n", d.Version, bi.GoVersion)
			}
		}
	}
	run("pgx", pgx.QueryExecModeCacheStatement)
	run("pgx-simple", pgx.QueryExecModeSimpleProtocol)
	fmt.Printf("[pgx] failures=%d\n", failures)
}
