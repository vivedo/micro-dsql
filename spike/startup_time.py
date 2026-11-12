"""Measure tursopg start-up: process launch until the port accepts a TCP connection."""
import os, socket, subprocess, sys, time

binary = sys.argv[1]
host, port = os.environ.get("PGHOST", "127.0.0.1"), int(os.environ.get("PGPORT", "5432"))
t0 = time.monotonic()
p = subprocess.Popen([binary, "--server", f"{host}:{port}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    while True:
        try:
            socket.create_connection((host, port), timeout=0.05).close()
            break
        except OSError:
            if time.monotonic() - t0 > 10:
                print("startup: TIMEOUT"); sys.exit(1)
            time.sleep(0.002)
    print(f"startup to first TCP accept: {(time.monotonic() - t0) * 1000:.1f} ms")
finally:
    p.terminate(); p.wait()
