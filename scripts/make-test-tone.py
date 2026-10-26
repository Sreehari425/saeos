#!/usr/bin/env python3
"""Write the sound test's WAV file: 660 Hz, 16 kHz mono 16-bit, 0.5 s.

usage: make-test-tone.py OUTPUT.wav

`usertest sound` plays it with /bin/play; check-wav.py expects the tone in
QEMU's capture. A rate the card must be set to (not 48 kHz), mono, and an
extra chunk before the data, so play's WAV parsing is exercised too.
"""

import math
import struct
import sys

RATE, FREQUENCY, SECONDS = 16_000, 660, 0.5


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    samples = b"".join(
        struct.pack("<h", int(12000 * math.sin(2 * math.pi * FREQUENCY * n / RATE)))
        for n in range(int(RATE * SECONDS))
    )
    fmt = struct.pack("<HHIIHH", 1, 1, RATE, RATE * 2, 2, 16)
    # An odd-sized LIST chunk: parsers must honour the padding byte.
    info = b"INFOISFT\x05\x00\x00\x00test\x00"
    chunks = (
        b"fmt " + struct.pack("<I", len(fmt)) + fmt
        + b"LIST" + struct.pack("<I", len(info)) + info + b"\x00"
        + b"data" + struct.pack("<I", len(samples)) + samples
    )
    with open(sys.argv[1], "wb") as file:
        file.write(b"RIFF" + struct.pack("<I", 4 + len(chunks)) + b"WAVE" + chunks)


if __name__ == "__main__":
    main()
