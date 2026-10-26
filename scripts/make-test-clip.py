#!/usr/bin/env python3
"""The Bad Apple player's test clip: OUTPUT_DIR/clip.bav and clip.wav.

usage: make-test-clip.py OUTPUT_DIR

61 frames of 480x360 at 30 fps: a 40x40 white square moving right across
black (frames 0-59), then a final frame (see `final_frame`, which
check-screen.py's `bav-final` mode expects on screen, scaled and centred);
and a 440 Hz tone (44.1 kHz stereo 16-bit) as long as the video.
"""

import math
import os
import struct
import sys
import wave

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bav  # noqa: E402

WIDTH, HEIGHT, FPS, FRAMES = 480, 360, 30, 61


def final_frame():
    """The top-left quadrant and a 20-pixel column at x = 400 white."""
    return bytes(
        1 if (x < 240 and y < 180) or 400 <= x < 420 else 0
        for y in range(HEIGHT)
        for x in range(WIDTH)
    )


def square_frame(index):
    left, top = 20 + index * 7, 160
    return bytes(
        1 if left <= x < left + 40 and top <= y < top + 40 else 0
        for y in range(HEIGHT)
        for x in range(WIDTH)
    )


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    output = sys.argv[1]
    os.makedirs(output, exist_ok=True)
    with open(os.path.join(output, "clip.bav"), "wb") as file:
        encoder = bav.Encoder(file, WIDTH, HEIGHT, FPS)
        for index in range(FRAMES - 1):
            encoder.add(square_frame(index))
        encoder.add(final_frame())
        encoder.finish()
    rate = 44_100
    samples = int(rate * FRAMES / FPS)
    with wave.open(os.path.join(output, "clip.wav"), "wb") as audio:
        audio.setnchannels(2)
        audio.setsampwidth(2)
        audio.setframerate(rate)
        audio.writeframes(b"".join(
            struct.pack("<hh", value, value)
            for value in (int(12000 * math.sin(2 * math.pi * 440 * n / rate)) for n in range(samples))
        ))


if __name__ == "__main__":
    main()
