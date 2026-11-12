"""Driver smoke test: asyncpg (binary protocol for params and results). Settings from PG* env vars."""
import asyncio
import datetime as dt
import decimal
import sys
import uuid

import asyncpg

NAME = "asyncpg"
failures = 0
TS = dt.datetime(2026, 11, 12, 10, 0, 0, 123456, tzinfo=dt.timezone.utc)
U = uuid.UUID("11111111-1111-1111-1111-111111111111")


async def step(label, coro_fn):
    global failures
    try:
        res = await asyncio.wait_for(coro_fn(), timeout=10)
        print(f"[{NAME}] {label}: OK {res if res is not None else ''}".rstrip())
    except asyncpg.PostgresError as e:
        failures += 1
        print(f"[{NAME}] {label}: ERR {e.sqlstate} {str(e).strip().splitlines()[0]}")
    except Exception as e:
        failures += 1
        print(f"[{NAME}] {label}: ERR client {type(e).__name__}: {str(e).strip()[:300]}")


async def main():
    print(f"[{NAME}] version asyncpg {asyncpg.__version__}")
    try:
        conn = await asyncio.wait_for(asyncpg.connect(), timeout=10)
    except Exception as e:
        print(f"[{NAME}] connect: ERR {type(e).__name__}: {e}")
        sys.exit(1)
    print(f"[{NAME}] connect: OK server_version={conn.get_server_version()}")

    await step("drop (simple protocol)", lambda: conn.execute("DROP TABLE IF EXISTS drv_asyncpg"))
    await step("create", lambda: conn.execute(
        "CREATE TABLE drv_asyncpg (id bigint PRIMARY KEY, name text NOT NULL, n int, amount numeric(10,2), ok bool, at timestamptz, u uuid)"))
    await step("select no params (extended)", lambda: conn.fetchval("SELECT 1"))
    await step("insert params bigint+text", lambda: conn.execute(
        "INSERT INTO drv_asyncpg (id, name) VALUES ($1, $2)", 1, "one"))
    await step("insert params all types", lambda: conn.execute(
        "INSERT INTO drv_asyncpg VALUES ($1, $2, $3, $4, $5, $6, $7)", 2, "two", 2, decimal.Decimal("2.50"), True, TS, U))
    await step("select by param", lambda: conn.fetchrow("SELECT * FROM drv_asyncpg WHERE id = $1", 1))
    await step("select all types", lambda: conn.fetchrow("SELECT n, amount, ok, at, u FROM drv_asyncpg WHERE id = $1", 2))
    await step("update by param", lambda: conn.execute("UPDATE drv_asyncpg SET name = $1 WHERE id = $2", "uno", 1))

    async def txn():
        async with conn.transaction():
            await conn.execute("INSERT INTO drv_asyncpg (id, name) VALUES (3, 'three')")
            await conn.execute("UPDATE drv_asyncpg SET n = 30 WHERE id = 3")
        return await conn.fetchval("SELECT n FROM drv_asyncpg WHERE id = 3")

    await step("explicit txn commit (no params)", txn)

    async def txn_rb():
        tr = conn.transaction()
        await tr.start()
        await conn.execute("INSERT INTO drv_asyncpg (id, name) VALUES (4, 'four')")
        await tr.rollback()
        return f"rows with id 4: {await conn.fetchval('SELECT count(*) FROM drv_asyncpg WHERE id = 4')}"

    await step("explicit txn rollback", txn_rb)
    await step("delete by param", lambda: conn.execute("DELETE FROM drv_asyncpg WHERE id = $1", 1))
    await step("delete literal", lambda: conn.execute("DELETE FROM drv_asyncpg WHERE id = 1"))
    await step("count", lambda: conn.fetchval("SELECT count(*) FROM drv_asyncpg"))
    await step("error SQLSTATE (dup key)", lambda: conn.execute("INSERT INTO drv_asyncpg (id, name) VALUES (3, 'dup')"))
    try:
        await asyncio.wait_for(conn.close(), timeout=5)
        print(f"[{NAME}] close: OK")
    except Exception as e:
        print(f"[{NAME}] close: ERR client {type(e).__name__} (graceful close did not complete in 5s)")
        conn.terminate()
    print(f"[{NAME}] failures={failures}")


asyncio.run(main())
