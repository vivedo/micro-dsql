"""Wire-protocol probes behind the driver results: RowDescription type OIDs, ParameterDescription,
binary-format parameters and results via raw libpq calls, and Terminate handling.
Settings from PG* env vars."""
import os
import socket
import struct

import psycopg
from psycopg import pq

c = psycopg.connect("", autocommit=True, connect_timeout=5)
c.execute("DROP TABLE IF EXISTS t_wire")
c.execute("CREATE TABLE t_wire (id bigint PRIMARY KEY, n int, s smallint, amount numeric(10,2), bn numeric, "
          "f float8, ok bool, at timestamptz, u uuid, j jsonb, t text)")
c.execute("INSERT INTO t_wire VALUES (1, 2, 3, 4.50, 5.5, 6.5, true, '2026-01-01 00:00:00.123456+00:00', "
          "'11111111-1111-1111-1111-111111111111', '{\"k\": 1}', 'x')")
for q in ["SELECT * FROM t_wire",
          "SELECT count(*), sum(n), sum(amount), avg(n), max(at), 1, 1.5, 'a', now(), 2::bigint FROM t_wire"]:
    cur = c.execute(q)
    print(f"[wire] RowDescription for {q!r}:")
    print("        " + ", ".join(f"{d.name}={d.type_code}" for d in cur.description))
    print(f"        row text values: {cur.fetchone()}")

pg = c.pgconn
for name, sql in [(b"s1", b"SELECT * FROM t_wire WHERE id = $1 AND n = $2"),
                  (b"s2", b"SELECT $1::int + 1"),
                  (b"s3", b"INSERT INTO t_wire (id, t) VALUES ($1, $2)")]:
    r = pg.prepare(name, sql)
    d = pg.describe_prepared(name)
    print(f"[wire] Parse+Describe {sql.decode()!r}: status={pq.ExecStatus(r.status).name} "
          f"ParameterDescription nparams={d.nparams} types={[d.param_type(i) for i in range(d.nparams)]}")

# Parse with explicit parameter types, then text vs binary parameter values.
r = pg.prepare(b"s4", b"SELECT t FROM t_wire WHERE id = $1", [20])
for label, val, fmt in [("text '1'", b"1", 0), ("binary int8 1", struct.pack("!q", 1), 1)]:
    res = pg.exec_prepared(b"s4", [val], [fmt])
    out = res.get_value(0, 0) if res.status == pq.ExecStatus.TUPLES_OK and res.ntuples else None
    print(f"[wire] Bind param {label}: status={pq.ExecStatus(res.status).name} rows={res.ntuples} value={out} "
          f"err={res.error_message.decode().strip()!r}")
res = pg.exec_params(b"SELECT id, n, amount, at, u FROM t_wire WHERE id = 1", [], result_format=1)
print(f"[wire] binary result format: status={pq.ExecStatus(res.status).name} err={res.error_message.decode().strip()!r}")
if res.status == pq.ExecStatus.TUPLES_OK:
    print("        " + ", ".join(f"{res.fname(i).decode()}(oid {res.ftype(i)})={res.get_value(0, i)!r}" for i in range(res.nfields)))
c.close()

# Terminate ('X'): a server should close the socket.
host, port = os.environ.get("PGHOST", "127.0.0.1"), int(os.environ.get("PGPORT", "5432"))
s = socket.create_connection((host, port), timeout=5)
params = f"user\0{os.environ.get('PGUSER', 'admin')}\0database\0{os.environ.get('PGDATABASE', 'postgres')}\0\0".encode()
s.sendall(struct.pack("!ii", 8 + len(params), 196608) + params)
buf = b""
while b"Z\x00\x00\x00\x05" not in buf:  # ReadyForQuery
    chunk = s.recv(65536)
    if not chunk:
        break
    if buf == b"" and chunk[:1] == b"R" and struct.unpack("!i", chunk[5:9])[0] == 3:
        pw = os.environ.get("PGPASSWORD", "x").encode() + b"\0"
        s.sendall(b"p" + struct.pack("!i", 4 + len(pw)) + pw)
    buf += chunk
s.sendall(b"X" + struct.pack("!i", 4))
s.settimeout(3)
try:
    d = s.recv(100)
    print(f"[wire] Terminate: server closed the connection: {'yes' if d == b'' else 'no, got ' + repr(d)}")
except socket.timeout:
    print("[wire] Terminate: server did NOT close the connection within 3s")
s.close()
