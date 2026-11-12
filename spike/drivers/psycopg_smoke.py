"""Driver smoke test: psycopg 3. Connection settings come from PG* env vars."""
import datetime as dt
import decimal
import sys
import uuid

import psycopg

NAME = "psycopg"
failures = 0


def step(label, fn):
    global failures
    try:
        res = fn()
        print(f"[{NAME}] {label}: OK {res if res is not None else ''}".rstrip())
    except psycopg.Error as e:
        failures += 1
        print(f"[{NAME}] {label}: ERR {getattr(e, 'sqlstate', None)} {str(e).strip().splitlines()[0]}")
    except Exception as e:  # driver-side failures (decode errors, protocol errors)
        failures += 1
        print(f"[{NAME}] {label}: ERR client {type(e).__name__}: {e}")


print(f"[{NAME}] version psycopg {psycopg.__version__}, libpq {psycopg.pq.version()}")
try:
    conn = psycopg.connect("", autocommit=True, connect_timeout=5)
except Exception as e:
    print(f"[{NAME}] connect: ERR {e}")
    sys.exit(1)
print(f"[{NAME}] connect: OK server_version={conn.info.parameter_status('server_version')}")
TS_ = dt.datetime(2026, 11, 12, 10, 0, 0, 123456, tzinfo=dt.timezone.utc)
U_ = uuid.UUID("11111111-1111-1111-1111-111111111111")


def suite(P, NAME_):
    """Run the smoke steps with placeholder P: %s (psycopg AUTO: binary for numbers) or %t (text)."""
    global NAME, failures
    NAME, failures = NAME_, 0
    cur = conn.cursor()
    TS, U = TS_, U_
    T = "drv_psycopg_" + ("text" if P == "%t" else "auto")

    step("drop", lambda: cur.execute(f"DROP TABLE IF EXISTS {T}") and None)
    step("create", lambda: cur.execute(
        f"CREATE TABLE {T} (id bigint PRIMARY KEY, name text NOT NULL, n int, amount numeric(10,2), ok bool, at timestamptz, u uuid, doc jsonb)") and None)
    step("insert params", lambda: cur.execute(
        f"INSERT INTO {T} VALUES ({P}, {P}, {P}, {P}, {P}, {P}, {P}, {P})",
        (1, "one", 1, decimal.Decimal("1.50"), True, TS, U, psycopg.types.json.Jsonb({"a": 1}))).rowcount)
    step("select by param", lambda: cur.execute(f"SELECT * FROM {T} WHERE id = {P}", (1,)).fetchone())


    def check_roundtrip():
        row = cur.execute(f"SELECT n, amount, ok, at, u, doc FROM {T} WHERE id = {P}", (1,)).fetchone()
        diffs = []
        if row[1] != decimal.Decimal("1.50"): diffs.append(f"amount={row[1]!r}")
        if row[3] != TS: diffs.append(f"at={row[3]!r} (sent {TS.isoformat()})")
        if row[4] != U: diffs.append(f"u={row[4]!r}")
        if row[5] != {"a": 1}: diffs.append(f"doc={row[5]!r}")
        return "exact" if not diffs else "DIFF " + "; ".join(diffs)


    step("type round trip", check_roundtrip)
    step("update by param", lambda: cur.execute(f"UPDATE {T} SET name = {P}, n = n + 1 WHERE id = {P}", ("uno", 1)).rowcount)


    def txn_commit():
        with conn.transaction():
            cur.execute(f"INSERT INTO {T} (id, name) VALUES ({P}, {P})", (2, "two"))
            cur.execute(f"UPDATE {T} SET n = {P} WHERE id = {P}", (20, 2))
        return cur.execute(f"SELECT n FROM {T} WHERE id = 2").fetchone()


    def txn_rollback():
        class Bail(Exception):
            pass
        try:
            with conn.transaction():
                cur.execute(f"INSERT INTO {T} (id, name) VALUES ({P}, {P})", (3, "three"))
                raise Bail()
        except Bail:
            pass
        return "rows with id 3: %s" % cur.execute(f"SELECT count(*) FROM {T} WHERE id = 3").fetchone()[0]


    step("explicit txn commit", txn_commit)
    step("explicit txn rollback", txn_rollback)


    def non_autocommit_txn():
        c2 = psycopg.connect("", connect_timeout=5)  # psycopg default: implicit BEGIN
        c2.execute(f"INSERT INTO {T} (id, name) VALUES ({P}, {P})", (5, "five"))
        c2.commit()
        n = c2.execute(f"SELECT count(*) FROM {T} WHERE id = 5").fetchone()[0]
        c2.close()
        return f"committed rows: {n}"


    step("default (non-autocommit) connection commit", non_autocommit_txn)
    step("binary params (%b)", lambda: cur.execute(
        f"INSERT INTO {T} (id, name, n, amount, ok, at, u) VALUES (%b, %b, %b, %b, %b, %b, %b)",
        (4, "four", 4, decimal.Decimal("4.25"), False, TS, U)).rowcount)
    step("binary params select", lambda: cur.execute(f"SELECT name, n, amount, ok, at, u FROM {T} WHERE id = %b", (4,)).fetchone())
    step("binary results", lambda: cur.execute(f"SELECT id, n, amount, at FROM {T} WHERE id = {P}", (1,), binary=True).fetchone())
    step("server-side prepared (prepare=True)", lambda: [cur.execute(f"SELECT name FROM {T} WHERE id = {P}", (i,), prepare=True).fetchone() for i in (1, 2, 1)])
    step("executemany", lambda: cur.executemany(f"INSERT INTO {T} (id, name) VALUES ({P}, {P})", [(10, "a"), (11, "b")]) or "ok")
    step("delete by param", lambda: cur.execute(f"DELETE FROM {T} WHERE id = {P}", (1,)).rowcount)
    step("count", lambda: cur.execute(f"SELECT count(*) FROM {T}").fetchone())
    step("error SQLSTATE (dup key)", lambda: cur.execute(f"INSERT INTO {T} (id, name) VALUES (2, 'dup')"))
    print(f"[{NAME}] failures={failures}")


suite("%s", "psycopg")
suite("%t", "psycopg-text")
conn.close()
