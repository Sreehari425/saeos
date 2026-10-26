#!/usr/bin/env python3
"""Check a WAV capture of the sound test: the expected tones, in order.

usage: check-wav.py CAPTURE.wav FREQ:SECONDS [FREQ:SECONDS...]
       check-wav.py --self-test

Each channel of the capture (QEMU's `-audiodev wav`) is checked on its own,
so a tone missing from one side fails. It is cut into 20 ms windows: quiet ones
are silence, the rest get a frequency from their zero crossings. Runs of
windows at one frequency are tones. The tones must match the arguments in
order: frequency within 3%, at least 80% of the length. Nothing else may
sound. Inside a tone the zero crossings must be evenly spaced: a lost,
repeated or overwritten period jumps the phase and shows up as one
irregular gap. Two runs of the same tone need silence between them, or one
tone was cut in two by a glitch.

Standard library only. The WAV header's sizes may be 0 if QEMU did not
finish the file; the data then runs to the end of the file.
"""

import math
import struct
import sys

WINDOW = 0.020
SILENCE_RMS = 400
FREQUENCY_TOLERANCE = 0.03
MIN_LENGTH = 0.8
# Windows of a different frequency allowed between two tones (a window
# that holds the end of one and the start of the next).
MAX_TRANSITION = 2
CROSSING_TOLERANCE = 0.05
MIN_GAP = 0.1


def read_wav(path):
    with open(path, "rb") as file:
        data = file.read()
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise SystemExit(f"{path}: not a WAV file")
    offset, fmt = 12, None
    while offset + 8 <= len(data):
        chunk, size = data[offset : offset + 4], struct.unpack_from("<I", data, offset + 4)[0]
        body = offset + 8
        if chunk == b"fmt ":
            tag, channels, rate, _, _, bits = struct.unpack_from("<HHIIHH", data, body)
            if tag != 1 or bits != 16:
                raise SystemExit(f"{path}: expected 16-bit PCM, got tag {tag}, {bits} bits")
            fmt = (channels, rate)
        elif chunk == b"data":
            if fmt is None:
                raise SystemExit(f"{path}: data before fmt")
            end = len(data) if size == 0 or body + size > len(data) else body + size
            channels, rate = fmt
            frames = (end - body) // (2 * channels)
            samples = struct.unpack_from(f"<{frames * channels}h", data, body)
            return rate, [list(samples[channel::channels]) for channel in range(channels)]
        offset = body + size + (size & 1)
    raise SystemExit(f"{path}: no data chunk")


def crossings(samples, start, end):
    """Upward zero crossings in samples[start:end], interpolated."""
    points = []
    for index in range(max(start, 1), end):
        before, after = samples[index - 1], samples[index]
        if before < 0 <= after:
            points.append(index - 1 + (-before) / (after - before))
    return points


def windows(rate, samples):
    size = int(rate * WINDOW)
    result = []
    for start in range(0, len(samples) - size + 1, size):
        window = samples[start : start + size]
        rms = math.sqrt(sum(value * value for value in window) / size)
        points = crossings(samples, start, start + size)
        if rms < SILENCE_RMS or len(points) < 3:
            result.append(None)
        else:
            result.append(rate * (len(points) - 1) / (points[-1] - points[0]))
    return result


def close(a, b):
    return abs(a - b) <= FREQUENCY_TOLERANCE * b


def runs(classified):
    """[(frequency or None, first window, window count)]."""
    found = []
    for index, frequency in enumerate(classified):
        if found:
            last, first, count = found[-1]
            same = (last is None and frequency is None) or (
                last is not None and frequency is not None and close(frequency, last)
            )
            if same:
                # Keep the run's frequency as its running mean.
                mean = None if last is None else (last * count + frequency) / (count + 1)
                found[-1] = (mean, first, count + 1)
                continue
        found.append((frequency, index, 1))
    return found


def regular(rate, samples, first, count):
    """Whether a tone's zero crossings are evenly spaced (no phase jump)."""
    size = int(rate * WINDOW)
    # The whole run, edges included: a phase jump just after a restart
    # (stale data played before the real stream) is a failure too.
    points = crossings(samples, first * size, (first + count) * size)
    gaps = [b - a for a, b in zip(points, points[1:])]
    if len(gaps) < 4:
        return True
    typical = sorted(gaps)[len(gaps) // 2]
    return all(abs(gap - typical) <= CROSSING_TOLERANCE * typical for gap in gaps)


def check(rate, samples, expected):
    """Problems found, as messages (empty: the capture is right)."""
    found = runs(windows(rate, samples))
    tones, problems, silence = [], [], 0.0
    for frequency, first, count in found:
        length = count * WINDOW
        if frequency is None:
            silence += length
            continue
        if count <= MAX_TRANSITION:
            continue
        if tones and close(frequency, tones[-1][0]) and silence < MIN_GAP:
            problems.append(
                f"{frequency:.0f} Hz tone broken at {first * WINDOW:.2f} s (a glitch)"
            )
        if not regular(rate, samples, first, count):
            problems.append(
                f"{frequency:.0f} Hz tone at {first * WINDOW:.2f} s has a phase jump"
            )
        tones.append((frequency, length, first * WINDOW))
        silence = 0.0
    summary = ", ".join(f"{f:.0f} Hz {l:.2f} s @ {t:.2f}" for f, l, t in tones)
    if len(tones) != len(expected):
        problems.append(f"expected {len(expected)} tones, found {len(tones)}: {summary}")
    else:
        for (frequency, length, start), (want, seconds) in zip(tones, expected):
            if not close(frequency, want):
                problems.append(f"tone at {start:.2f} s is {frequency:.1f} Hz, expected {want} Hz")
            if length < MIN_LENGTH * seconds:
                problems.append(f"{want} Hz tone lasts {length:.2f} s, expected {seconds} s")
    return problems, summary


def synthesize(rate, parts, jump_at=None):
    """Test audio: parts are (frequency or None for silence, seconds)."""
    samples, phase = [], 0.0
    for frequency, seconds in parts:
        for _ in range(int(rate * seconds)):
            if frequency is None:
                samples.append(0)
                continue
            if jump_at is not None and len(samples) == int(jump_at * rate):
                phase += math.pi * 0.6
            samples.append(int(12000 * math.sin(phase)))
            phase += 2 * math.pi * frequency / rate
    return samples


def self_test():
    rate = 44100
    expected = [(440, 1.0), (440, 0.5), (1000, 0.5), (660, 0.5)]
    good = [(440, 1.0), (None, 0.3), (440, 0.5), (1000, 0.5), (None, 0.05), (660, 0.5)]
    cases = [
        ("good", good, None, True),
        ("phase jump", good, 0.5, False),
        ("no gap between the 440s", [(440, 1.5), (1000, 0.5), (None, 0.1), (660, 0.5)], None, False),
        ("wrong pitch", [(440, 1.0), (None, 0.3), (440, 0.5), (2177, 0.5), (None, 0.1), (660, 0.5)], None, False),
        ("too short", [(440, 0.5), (None, 0.3), (440, 0.5), (1000, 0.5), (None, 0.1), (660, 0.5)], None, False),
        ("a silent channel", [(None, 3.0)], None, False),
    ]
    failures = 0
    for name, parts, jump, should_pass in cases:
        problems, _ = check(rate, synthesize(rate, parts, jump), expected)
        if (not problems) != should_pass:
            print(f"check-wav self-test: {name}: {'passed' if not problems else problems}")
            failures += 1
    if failures:
        raise SystemExit("check-wav: self-test FAILED")
    print("check-wav: self-test passed")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    expected = [(float(f), float(s)) for f, s in (arg.split(":") for arg in sys.argv[2:])]
    rate, channels = read_wav(sys.argv[1])
    failed = False
    for channel, samples in enumerate(channels):
        problems, summary = check(rate, samples, expected)
        for problem in problems:
            print(f"check-wav: channel {channel}: {problem}")
        failed = failed or bool(problems)
        if not problems:
            print(f"check-wav: {sys.argv[1]} channel {channel}: {summary}: PASS")
    if failed:
        raise SystemExit(f"check-wav: {sys.argv[1]} FAILED")


if __name__ == "__main__":
    main()
