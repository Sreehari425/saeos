#!/usr/bin/env python3
"""BAV: black-and-white video as run lengths of the rows that changed.

usage: bav.py --self-test
       bav.py --fixture OUT.bav   (the host tests' fixture, see fixture_frames)
       (a module for make-badapple.py and make-test-clip.py)

    header  "BAV1", width u16, height u16, fps numerator u16, fps
            denominator u16, frame count u32 (little-endian)
    frame   varint byte length, then: varint count of changed rows, and per
            changed row: varint gap (the first row's index, then rows
            skipped since the previous changed row), varint run count, the
            run lengths (varints), alternating black and white, starting
            with black (so the first may be 0); they add up to the width.

Frames are deltas: a row not mentioned is as it was; before the first
frame everything is black. Varints are unsigned LEB128. The SaeOS player
decodes this (userspace/coreutils/src/bav.rs). Grey frames become black
and white at 50%. Standard library only; the hot loops stay in C (bytes
translate, row comparison, regex), so a 6,572-frame video takes a minute.
"""

import re
import struct
import sys

MAGIC = b"BAV1"
# Grey byte -> 0 (black) or 1 (white).
THRESHOLD = bytes(1 if value >= 128 else 0 for value in range(256))
RUN = re.compile(rb"\x00+|\x01+")


def varint(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def read_varint(data, offset):
    value = shift = 0
    while True:
        if offset >= len(data):
            raise ValueError("truncated varint")
        byte = data[offset]
        offset += 1
        value |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return value, offset
        shift += 7
        if shift > 35:
            raise ValueError("varint too long")


def row_runs(row):
    """Run lengths of a 0/1 row, starting with black."""
    runs = [len(match) for match in RUN.findall(row)]
    if row[:1] == b"\x01":
        runs.insert(0, 0)
    return runs


class Encoder:
    """Writes a BAV file: header, then one encoded frame per `add`."""

    def __init__(self, file, width, height, fps_num, fps_den=1):
        self.file, self.width, self.height = file, width, height
        self.previous = [bytes(width)] * height
        self.frames = 0
        self.header = (width, height, fps_num, fps_den)
        file.write(MAGIC + struct.pack("<HHHHI", width, height, fps_num, fps_den, 0))

    def add_grey(self, frame):
        """A frame of `width * height` grey bytes."""
        self.add(frame.translate(THRESHOLD))

    def add(self, bits):
        """A frame of `width * height` bytes, 0 (black) or 1 (white)."""
        if len(bits) != self.width * self.height:
            raise ValueError("frame size")
        body = bytearray()
        changed = 0
        last = -1
        for y in range(self.height):
            row = bits[y * self.width : (y + 1) * self.width]
            if row == self.previous[y]:
                continue
            self.previous[y] = row
            runs = row_runs(row)
            assert sum(runs) == self.width
            body += varint(y - last - 1) + varint(len(runs))
            for run in runs:
                body += varint(run)
            changed += 1
            last = y
        payload = varint(changed) + bytes(body)
        self.file.write(varint(len(payload)) + payload)
        self.frames += 1

    def finish(self):
        """Patch the frame count into the header."""
        self.file.seek(len(MAGIC) + 8)
        self.file.write(struct.pack("<I", self.frames))


def decode(data):
    """(width, height, fps_num, fps_den, [frames as 0/1 bytes])."""
    if data[:4] != MAGIC:
        raise ValueError("not a BAV file")
    width, height, fps_num, fps_den, count = struct.unpack_from("<HHHHI", data, 4)
    offset = 16
    screen = [bytearray(width) for _ in range(height)]
    frames = []
    for _ in range(count):
        length, offset = read_varint(data, offset)
        end = offset + length
        changed, offset = read_varint(data, offset)
        y = -1
        for _ in range(changed):
            gap, offset = read_varint(data, offset)
            y += gap + 1
            runs, offset = read_varint(data, offset)
            x, colour = 0, 0
            for _ in range(runs):
                run, offset = read_varint(data, offset)
                screen[y][x : x + run] = bytes([colour]) * run
                x += run
                colour ^= 1
            if x != width:
                raise ValueError("runs do not fill the row")
        if offset != end:
            raise ValueError("frame length mismatch")
        frames.append(b"".join(screen))
    return width, height, fps_num, fps_den, frames


def self_test():
    import io
    import random

    rng = random.Random(7)
    width, height = 37, 11
    frames = [bytes(width * height)]
    for _ in range(40):
        frame = bytearray(frames[-1])
        for _ in range(rng.randrange(0, 30)):
            frame[rng.randrange(len(frame))] ^= 1
        frames.append(bytes(frame))
    frames.append(bytes([1]) * (width * height))
    file = io.BytesIO()
    encoder = Encoder(file, width, height, 30)
    for frame in frames:
        encoder.add(frame)
    encoder.finish()
    decoded = decode(file.getvalue())
    if decoded[:4] != (width, height, 30, 1) or decoded[4] != frames:
        raise SystemExit("bav: self-test FAILED")
    # A frame identical to the one before costs two bytes: its length
    # (1) and "no rows changed" (0).
    file = io.BytesIO()
    encoder = Encoder(file, width, height, 30)
    encoder.add(frames[5])
    before = file.tell()
    encoder.add(frames[5])
    if file.getvalue()[before:] != b"\x01\x00":
        raise SystemExit("bav: self-test FAILED (an unchanged frame is not 2 bytes)")
    print(f"bav: self-test passed ({len(frames)} frames)")


def fixture_frames():
    """23x9, 13 frames: frame i has (x, y) white when (x + 2y + 3k) % 7 < 3,
    with k = 0 on every third row (rows that never change after the first
    frame), else i, except that frame 6 repeats frame 5 (nothing changes);
    the last is all white. tools/host-tests/src/bav_tests.rs computes the
    same and must decode the file to it."""
    width, height = 23, 9

    def shift(i, y):
        return 0 if y % 3 == 0 else (5 if i == 6 else i)

    frames = [
        bytes(1 if (x + 2 * y + 3 * shift(i, y)) % 7 < 3 else 0 for y in range(height) for x in range(width))
        for i in range(12)
    ]
    frames.append(bytes([1]) * (width * height))
    return width, height, frames


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    elif len(sys.argv) == 3 and sys.argv[1] == "--fixture":
        width, height, frames = fixture_frames()
        with open(sys.argv[2], "wb") as file:
            encoder = Encoder(file, width, height, 30)
            for frame in frames:
                encoder.add(frame)
            encoder.finish()
    else:
        raise SystemExit(__doc__)
