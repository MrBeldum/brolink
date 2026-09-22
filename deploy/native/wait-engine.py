#!/usr/bin/python3
"""Wait until the systemd-managed engine is listening before starting BroLink."""
import socket
import time

for _ in range(100):
    try:
        with socket.create_connection(("127.0.0.1", 47984), timeout=0.2):
            break
    except OSError:
        time.sleep(0.2)
else:
    raise SystemExit("BroLink streaming engine did not start listening")
