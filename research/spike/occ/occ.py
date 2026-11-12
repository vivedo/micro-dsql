"""Two-session OCC experiments for the M0 spike (docs/plan.md M0.5).

Every statement runs in a worker thread with a client-side timeout, so a server-side wait or hang is
recorded as TIMEOUT instead of blocking the run (the server does not support CancelRequest).
Connection settings come from the PG* environment variables.

Output: one line per statement: `[scenario] <session> <statement> -> OK <rows> | ERR <sqlstate> <msg>`.
"""
import collections
import concurrent.futures as cf
import os
import random
import sys
import threading
import time

import psycopg

TIMEOUT = float(os.environ.get("OCC_STEP_TIMEOUT", "10"))
pool = cf.ThreadPoolExecutor(max_workers=4)
hung = False


def connect():
    # autocommit=True: the script sends BEGIN/COMMIT itself, so the statement order is exact.
    return psycopg.connect("", autocommit=True, connect_timeout=5)


def run(scn, who, conn, sql):
    """Run one statement; return (ok, sqlstate, rows_or_msg)."""
    global hung

    def go():
        cur = conn.execute(sql)
        return cur.fetchall() if cur.description else cur.rowcount

    fut = pool.submit(go)
    try:
        res = fut.result(timeout=TIMEOUT)
        print(f"[{scn}] {who} {sql} -> OK {res}")
        return True, None, res
    except cf.TimeoutError:
        hung = True
        print(f"[{scn}] {who} {sql} -> TIMEOUT after {TIMEOUT}s (hang)")
        return False, "TIMEOUT", None
    except psycopg.Error as e:
        msg = str(e).strip().splitlines()[0] if str(e).strip() else type(e).__name__
        print(f"[{scn}] {who} {sql} -> ERR {e.sqlstate} {msg}")
        return False, e.sqlstate, msg


def fresh(scn, ddl, seed):
    c = connect()
    name = ddl.split()[2]
    run(scn, "setup", c, f"DROP TABLE IF EXISTS {name}")
    run(scn, "setup", c, ddl)
    for s in seed:
        run(scn, "setup", c, s)
    return c


def same_row_update():
    scn = "S1 same-row UPDATE"
    setup = fresh(scn, "CREATE TABLE occ_acct (id int PRIMARY KEY, balance int NOT NULL)",
                  ["INSERT INTO occ_acct VALUES (1, 100), (2, 100)"])
    a, b = connect(), connect()
    run(scn, "A", a, "BEGIN")
    run(scn, "A", a, "UPDATE occ_acct SET balance = balance - 10 WHERE id = 1")
    run(scn, "B", b, "BEGIN")
    run(scn, "B", b, "UPDATE occ_acct SET balance = balance - 20 WHERE id = 1")
    run(scn, "B", b, "SELECT balance FROM occ_acct WHERE id = 1")
    run(scn, "A", a, "COMMIT")
    run(scn, "B", b, "COMMIT")
    run(scn, "final", setup, "SELECT id, balance FROM occ_acct ORDER BY id")

    scn = "S1b update after concurrent commit (row committed after B's snapshot)"
    run(scn, "B", b, "BEGIN")
    run(scn, "B", b, "SELECT balance FROM occ_acct WHERE id = 1")
    run(scn, "A", a, "BEGIN")
    run(scn, "A", a, "UPDATE occ_acct SET balance = balance - 1 WHERE id = 1")
    run(scn, "A", a, "COMMIT")
    run(scn, "B", b, "UPDATE occ_acct SET balance = balance - 2 WHERE id = 1")
    run(scn, "B", b, "COMMIT")
    run(scn, "final", setup, "SELECT id, balance FROM occ_acct ORDER BY id")

    scn = "S1c different rows (no conflict expected)"
    run(scn, "A", a, "BEGIN")
    run(scn, "B", b, "BEGIN")
    run(scn, "A", a, "UPDATE occ_acct SET balance = balance + 1 WHERE id = 1")
    run(scn, "B", b, "UPDATE occ_acct SET balance = balance + 1 WHERE id = 2")
    run(scn, "A", a, "COMMIT")
    run(scn, "B", b, "COMMIT")
    run(scn, "final", setup, "SELECT id, balance FROM occ_acct ORDER BY id")

    scn = "S1d snapshot point: BEGIN, concurrent autocommit write, then first read"
    run(scn, "B", b, "BEGIN")
    run(scn, "A", a, "UPDATE occ_acct SET balance = 555 WHERE id = 2")
    run(scn, "B", b, "SELECT balance FROM occ_acct WHERE id = 2")
    run(scn, "B", b, "COMMIT")
    print(f"[{scn}] note: 555 => snapshot taken at first statement; old value => snapshot at BEGIN")

    scn = "S1e same-row UPDATE, then the first writer rolls back"
    run(scn, "A", a, "BEGIN")
    run(scn, "A", a, "UPDATE occ_acct SET balance = 0 WHERE id = 1")
    run(scn, "B", b, "BEGIN")
    run(scn, "B", b, "UPDATE occ_acct SET balance = 1 WHERE id = 1")
    run(scn, "A", a, "ROLLBACK")
    run(scn, "B", b, "COMMIT")
    run(scn, "final", setup, "SELECT id, balance FROM occ_acct ORDER BY id")
    for c in (a, b, setup):
        c.close()


def same_pk_insert():
    scn = "S2 same-PK INSERT"
    setup = fresh(scn, "CREATE TABLE occ_pk (id int PRIMARY KEY, who text)", [])
    a, b = connect(), connect()
    run(scn, "A", a, "BEGIN")
    run(scn, "A", a, "INSERT INTO occ_pk VALUES (10, 'A')")
    run(scn, "B", b, "BEGIN")
    run(scn, "B", b, "INSERT INTO occ_pk VALUES (10, 'B')")
    run(scn, "A", a, "COMMIT")
    run(scn, "B", b, "COMMIT")
    run(scn, "final", setup, "SELECT * FROM occ_pk ORDER BY id")

    scn = "S2b same-PK INSERT, second inserter commits first"
    run(scn, "A", a, "BEGIN")
    run(scn, "A", a, "INSERT INTO occ_pk VALUES (20, 'A')")
    run(scn, "B", b, "BEGIN")
    run(scn, "B", b, "INSERT INTO occ_pk VALUES (20, 'B')")
    run(scn, "B", b, "COMMIT")
    run(scn, "A", a, "COMMIT")
    run(scn, "final", setup, "SELECT * FROM occ_pk ORDER BY id")

    scn = "S2c same unique (non-PK) key INSERT"
    run(scn, "setup", setup, "DROP TABLE IF EXISTS occ_uq")
    run(scn, "setup", setup, "CREATE TABLE occ_uq (id int PRIMARY KEY, email text UNIQUE)")
    run(scn, "A", a, "BEGIN")
    run(scn, "A", a, "INSERT INTO occ_uq VALUES (1, 'x@example.com')")
    run(scn, "B", b, "BEGIN")
    run(scn, "B", b, "INSERT INTO occ_uq VALUES (2, 'x@example.com')")
    run(scn, "A", a, "COMMIT")
    run(scn, "B", b, "COMMIT")
    run(scn, "final", setup, "SELECT * FROM occ_uq ORDER BY id")
    for c in (a, b, setup):
        c.close()


def write_skew():
    scn = "S3 write skew"
    setup = fresh(scn, "CREATE TABLE occ_oncall (doctor text PRIMARY KEY, on_call bool NOT NULL)",
                  ["INSERT INTO occ_oncall VALUES ('alice', true), ('bob', true)"])
    a, b = connect(), connect()
    run(scn, "A", a, "BEGIN")
    run(scn, "A", a, "SELECT count(*) FROM occ_oncall WHERE on_call")
    run(scn, "B", b, "BEGIN")
    run(scn, "B", b, "SELECT count(*) FROM occ_oncall WHERE on_call")
    run(scn, "A", a, "UPDATE occ_oncall SET on_call = false WHERE doctor = 'alice'")
    run(scn, "B", b, "UPDATE occ_oncall SET on_call = false WHERE doctor = 'bob'")
    run(scn, "A", a, "COMMIT")
    run(scn, "B", b, "COMMIT")
    run(scn, "final", setup, "SELECT * FROM occ_oncall ORDER BY doctor")
    print(f"[{scn}] note: both on_call=false => write skew allowed (snapshot isolation, same as DSQL)")
    for c in (a, b, setup):
        c.close()


def storm(explicit_txn, workers=8, ops=150):
    """Concurrent writers. Each op: insert a unique row and bump a shared hot counter."""
    kind = "explicit BEGIN/COMMIT" if explicit_txn else "autocommit"
    scn = f"S4 concurrent writes ({kind}, {workers} workers x {ops} ops)"
    tbl, ctr = ("occ_log_tx", "occ_ctr_tx") if explicit_txn else ("occ_log", "occ_ctr")
    setup = fresh(scn, f"CREATE TABLE {tbl} (id int PRIMARY KEY, worker int NOT NULL)", [])
    run(scn, "setup", setup, f"DROP TABLE IF EXISTS {ctr}")
    run(scn, "setup", setup, f"CREATE TABLE {ctr} (id int PRIMARY KEY, n int NOT NULL)")
    run(scn, "setup", setup, f"INSERT INTO {ctr} VALUES (1, 0)")
    errors = collections.Counter()
    samples = {}
    ok_inserts = [0]
    ok_bumps = [0]
    lock = threading.Lock()

    def worker(w):
        c = connect()
        for i in range(ops):
            rid = w * 100000 + i
            for what, sql in (("insert", f"INSERT INTO {tbl} VALUES ({rid}, {w})"),
                              ("bump", f"UPDATE {ctr} SET n = n + 1 WHERE id = 1")):
                try:
                    if explicit_txn:
                        c.execute("BEGIN")
                        c.execute(sql)
                        c.execute("COMMIT")
                    else:
                        c.execute(sql)
                    with lock:
                        (ok_inserts if what == "insert" else ok_bumps)[0] += 1
                except psycopg.Error as e:
                    key = (what, e.sqlstate)
                    with lock:
                        errors[key] += 1
                        samples.setdefault(key, str(e).strip().splitlines()[0])
                    if explicit_txn:
                        try:
                            c.execute("ROLLBACK")
                        except psycopg.Error:
                            pass
                time.sleep(random.random() / 2000)
        c.close()

    t0 = time.monotonic()
    threads = [threading.Thread(target=worker, args=(w,)) for w in range(workers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=120)
    alive = sum(t.is_alive() for t in threads)
    dt = time.monotonic() - t0
    print(f"[{scn}] elapsed {dt:.2f}s, threads still running: {alive}")
    print(f"[{scn}] successful inserts={ok_inserts[0]} successful bumps={ok_bumps[0]} "
          f"attempted={workers * ops} each")
    for (what, st), n in sorted(errors.items()):
        print(f"[{scn}] errors {what} {st}: {n}  e.g. {samples[(what, st)]}")
    _, _, rows = run(scn, "check", setup, f"SELECT count(*) FROM {tbl}")
    _, _, ctr_rows = run(scn, "check", setup, f"SELECT n FROM {ctr} WHERE id = 1")
    consistent = rows and ctr_rows and int(rows[0][0]) == ok_inserts[0] and int(ctr_rows[0][0]) == ok_bumps[0]
    print(f"[{scn}] consistency (rows == successful inserts, counter == successful bumps): "
          f"{'OK' if consistent else 'MISMATCH'}")
    setup.close()


def isolation_levels():
    scn = "S5 isolation levels"
    c = connect()
    for stmt in ["BEGIN",
                 "BEGIN ISOLATION LEVEL READ UNCOMMITTED",
                 "BEGIN ISOLATION LEVEL READ COMMITTED",
                 "BEGIN ISOLATION LEVEL REPEATABLE READ",
                 "BEGIN ISOLATION LEVEL SERIALIZABLE",
                 "BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ WRITE",
                 "BEGIN READ ONLY",
                 "START TRANSACTION",
                 "START TRANSACTION ISOLATION LEVEL REPEATABLE READ",
                 "START TRANSACTION ISOLATION LEVEL SERIALIZABLE"]:
        ok, _, _ = run(scn, "-", c, stmt)
        if ok:
            run(scn, "-", c, "ROLLBACK")
    run(scn, "-", c, "BEGIN")
    ok, _, _ = run(scn, "-", c, "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
    run(scn, "-", c, "ROLLBACK")
    run(scn, "-", c, "BEGIN")
    run(scn, "-", c, "SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
    run(scn, "-", c, "ROLLBACK")
    run(scn, "-", c, "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL REPEATABLE READ")
    run(scn, "-", c, "SHOW transaction_isolation")
    run(scn, "-", c, "SHOW default_transaction_isolation")
    c.close()


def aborted_txn():
    scn = "S6 error inside a transaction"
    c = connect()
    run(scn, "-", c, "BEGIN")
    run(scn, "-", c, "SELECT 1/0")
    run(scn, "-", c, "SELECT 1")
    run(scn, "-", c, "COMMIT")
    run(scn, "-", c, "SELECT 'after commit of aborted txn' AS state")
    c.close()


def schema_conflict():
    scn = "S7 DDL vs concurrent transaction"
    setup = fresh(scn, "CREATE TABLE occ_ddl (id int PRIMARY KEY, v int)", ["INSERT INTO occ_ddl VALUES (1, 1)"])
    a, b = connect(), connect()
    run(scn, "A", a, "BEGIN")
    run(scn, "A", a, "SELECT v FROM occ_ddl WHERE id = 1")
    run(scn, "B", b, "ALTER TABLE occ_ddl ADD COLUMN w int")
    run(scn, "A", a, "INSERT INTO occ_ddl (id, v) VALUES (2, 2)")
    run(scn, "A", a, "COMMIT")
    run(scn, "A2", a, "BEGIN")
    run(scn, "A2", a, "UPDATE occ_ddl SET v = 10 WHERE id = 1")
    run(scn, "B2", b, "BEGIN")
    run(scn, "B2", b, "CREATE TABLE occ_ddl2 (id int PRIMARY KEY)")
    run(scn, "B2", b, "COMMIT")
    run(scn, "A2", a, "COMMIT")
    run(scn, "final", setup, "SELECT * FROM occ_ddl ORDER BY id")
    for c in (a, b, setup):
        c.close()


def main():
    print(f"[occ] psycopg {psycopg.__version__}; step timeout {TIMEOUT}s")
    for fn in (isolation_levels, aborted_txn, same_row_update, same_pk_insert, write_skew,
               lambda: storm(False), lambda: storm(True), schema_conflict):
        try:
            fn()
        except Exception as e:  # connection loss, server crash
            print(f"[occ] scenario aborted: {type(e).__name__}: {e}")
    try:
        c = connect()
        c.execute("SELECT 1")
        c.close()
        print("[occ] server still alive at the end: yes")
    except Exception as e:
        print(f"[occ] server still alive at the end: NO ({e})")
    print(f"[occ] any hang: {'yes' if hung else 'no'}")
    sys.stdout.flush()
    os._exit(1 if hung else 0)  # do not wait for threads stuck on a hung statement


if __name__ == "__main__":
    main()
