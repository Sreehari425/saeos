#!/usr/bin/env python3
"""Type into a running QEMU guest once a marker appears in its log.

usage: qemu-input.py LOG MARKER keys MONITOR_SOCKET "h e l l o ret ..."
       qemu-input.py LOG MARKER serial FIFO "text with \\r and \\x7f escapes"
       qemu-input.py LOG MARKER screendump MONITOR_SOCKET /path/to/screen.ppm

`keys` sends QEMU key names (`sendkey`, so the guest sees PS/2 scancodes)
through the monitor socket; `serial` writes bytes to the FIFO that feeds
the guest's serial port; `screendump` saves what the screen shows (a
PPM image) once the marker appears. Waiting for the marker keeps the input
from arriving before the program reads it (UEFI firmware reads serial too).
"""
import os
import socket
import sys
import time

log_path, marker, mode, target, payload = sys.argv[1:6]
deadline = time.time() + 120
while time.time() < deadline:
    try:
        with open(log_path, "rb") as log:
            if marker.encode() in log.read():
                break
    except FileNotFoundError:
        pass
    time.sleep(0.1)
else:
    sys.exit(f"qemu-input: marker {marker!r} never appeared")
time.sleep(0.3)

def connect(path):
    monitor = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    for _ in range(100):
        try:
            monitor.connect(path)
            return monitor
        except OSError:
            # Another helper may hold the monitor for a moment.
            time.sleep(0.1)
    sys.exit(f"qemu-input: cannot reach the monitor at {path}")


if mode == "keys":
    monitor = connect(target)
    monitor.settimeout(0.2)
    for key in payload.split():
        monitor.sendall(f"sendkey {key}\n".encode())
        time.sleep(0.05)
        try:
            monitor.recv(65536)
        except socket.timeout:
            pass
    monitor.close()
elif mode == "serial":
    data = payload.encode().decode("unicode_escape").encode("latin-1")
    with open(target, "wb", buffering=0) as fifo:
        for byte in data:
            fifo.write(bytes([byte]))
            time.sleep(0.01)
elif mode == "screendump":
    monitor = connect(target)
    monitor.settimeout(0.5)
    monitor.sendall(f"screendump {payload}\n".encode())
    # Done once the file stops growing.
    size = -1
    for _ in range(100):
        time.sleep(0.1)
        try:
            monitor.recv(65536)
        except socket.timeout:
            pass
        current = os.path.getsize(payload) if os.path.exists(payload) else -1
        if current > 0 and current == size:
            break
        size = current
    monitor.close()
else:
    sys.exit(f"qemu-input: unknown mode {mode}")
