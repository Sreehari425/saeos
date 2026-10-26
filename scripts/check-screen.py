#!/usr/bin/env python3
"""Check QEMU screendumps of `usertest fb`.

usage: check-screen.py drawn SCREEN.ppm [WIDTHxHEIGHT]
       check-screen.py console SCREEN.ppm [WIDTHxHEIGHT]
       check-screen.py bav-final SCREEN.ppm
       check-screen.py --self-test

`drawn`: the screen shows usertest's picture, pixel for pixel (so nothing
else, like console text, drew on it): red, green, blue and white
quadrants, the 40x40 yellow square a forked child drew (centred at a
quarter of the width and height), and the magenta row written with
write() (20 rows from the bottom). With WIDTHxHEIGHT the screen must be
exactly that size.

`console`: the program let go and the console is back: the picture is
gone (on BIOS the card is in VGA text mode again, a differently sized
image, 720x400 for VGA text) and the console shows just its first line
(usertest's "fb released"): text in the top row, nothing below the third.
A wiped font, or a VGA left in graphics mode, would show nothing or
garbage everywhere, blank cells included.

`bav-final`: the Bad Apple player's last frame of the test clip
(make-test-clip.py's `final_frame`), at the largest whole scale that fits,
centred, black around it, pixel for pixel.

Screendumps are binary PPM (P6), as QEMU's monitor writes them. Standard
library only.
"""

import sys

RED, GREEN, BLUE, WHITE = (255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255)
YELLOW, MAGENTA = (255, 255, 0), (255, 0, 255)


def read_ppm(path):
    with open(path, "rb") as file:
        data = file.read()
    fields, offset = [], 0
    # Header: magic, width, height, maximum, then one whitespace byte.
    while len(fields) < 4:
        while data[offset : offset + 1].isspace():
            offset += 1
        if data[offset : offset + 1] == b"#":
            offset = data.index(b"\n", offset) + 1
            continue
        end = offset
        while not data[end : end + 1].isspace():
            end += 1
        fields.append(data[offset:end])
        offset = end
    if fields[0] != b"P6" or fields[3] != b"255":
        raise SystemExit(f"{path}: not an 8-bit P6 PPM")
    width, height = int(fields[1]), int(fields[2])
    return width, height, data[offset + 1 : offset + 1 + width * height * 3]


def pixel(image, x, y):
    width, _, data = image
    start = (y * width + x) * 3
    return tuple(data[start : start + 3])


def expected_colour(width, height, x, y):
    """usertest's picture at (x, y)."""
    if y == height - 20:
        return MAGENTA
    if width // 4 - 20 <= x < width // 4 + 20 and height // 4 - 20 <= y < height // 4 + 20:
        return YELLOW
    return [[RED, GREEN], [BLUE, WHITE]][y >= height // 2][x >= width // 2]


def drawn_problems(image, expected_size=None):
    width, height, data = image
    problems = []
    if expected_size and (width, height) != expected_size:
        problems.append(f"screen is {width}x{height}, expected {expected_size[0]}x{expected_size[1]}")
    wrong = []
    for y in range(height):
        row = data[y * width * 3 : (y + 1) * width * 3]
        for x in range(width):
            found = tuple(row[x * 3 : x * 3 + 3])
            if found != expected_colour(width, height, x, y):
                wrong.append((x, y, found))
    if wrong:
        x, y, found = wrong[0]
        problems.append(
            f"{len(wrong)} pixels differ from the picture; the first at ({x}, {y}) is {found}, "
            f"expected {expected_colour(width, height, x, y)}"
        )
    return problems


def is_drawn(image):
    return not drawn_problems(image)


def bav_final_problems(image):
    import importlib
    import os

    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    clip = importlib.import_module("make-test-clip")
    frame, video_width, video_height = clip.final_frame(), clip.WIDTH, clip.HEIGHT
    width, height, data = image
    scale = max(1, min(width // video_width, height // video_height))
    left = (width - video_width * scale) // 2
    top = (height - video_height * scale) // 2
    wrong = []
    for y in range(height):
        for x in range(width):
            vx, vy = (x - left) // scale, (y - top) // scale
            inside = x >= left and y >= top and vx < video_width and vy < video_height
            white = inside and frame[vy * video_width + vx]
            want = WHITE if white else (0, 0, 0)
            found = pixel(image, x, y)
            if found != want:
                wrong.append((x, y, found, want))
    if wrong:
        x, y, found, want = wrong[0]
        return [f"{len(wrong)} pixels differ from the clip's last frame ({scale}x at {left},{top}); "
                f"the first at ({x}, {y}) is {found}, expected {want}"]
    return []


def lit(image, rows):
    """Lit pixels in these rows."""
    width, height, data = image
    return sum(
        1
        for y in rows
        if y < height
        for x in range(width)
        if any(data[(y * width + x) * 3 : (y * width + x) * 3 + 3])
    )


def console_problems(image, expected_size=None):
    width, height, _ = image
    if expected_size and (width, height) != expected_size:
        return [f"screen is {width}x{height}, expected {expected_size[0]}x{expected_size[1]}"]
    if is_drawn(image):
        return ["the picture is still up"]
    if lit(image, range(16)) < 50:
        return ["no text in the first row (font wiped, or text mode not back)"]
    if lit(image, range(48, height)) > 20:
        return ["the screen below the first lines is not blank (garbage: a broken font?)"]
    return []


def synthesize(width, height, child=True, row=True, swap=False, text=False):
    rows = []
    for y in range(height):
        line = bytearray()
        for x in range(width):
            colour = expected_colour(width, height, x, y)
            if not child and colour == YELLOW:
                colour = RED
            if not row and colour == MAGENTA:
                colour = [[RED, GREEN], [BLUE, WHITE]][y >= height // 2][x >= width // 2]
            if swap:
                colour = (colour[2], colour[1], colour[0])
            if text and y < 16 and x < 8 * 18 and (x + y) % 3 == 0:
                colour = (85, 255, 85)
            line += bytes(colour)
        rows.append(bytes(line))
    return width, height, b"".join(rows)


def self_test():
    cases = [
        ("the picture", synthesize(320, 240), True),
        ("no child square (copy-on-write)", synthesize(320, 240, child=False), False),
        ("no written row", synthesize(320, 240, row=False), False),
        ("red and blue swapped", synthesize(320, 240, swap=True), False),
        ("text console", (72, 40, bytes(72 * 40 * 3)), False),
        ("console text over the picture", synthesize(320, 240, text=True), False),
    ]
    failures = [name for name, image, drawn in cases if is_drawn(image) != drawn]
    if drawn_problems(synthesize(320, 240), (960, 720)) == []:
        failures.append("the size check")
    blank = (72, 40, bytes(72 * 40 * 3))
    text = (72, 64, bytes([0, 170, 0] * 72 * 16 + [0, 0, 0] * 72 * 48))
    garbage = (72, 64, bytes([0, 170, 0] * 72 * 64))
    if (
        console_problems(blank) == []
        or console_problems(text) != []
        or console_problems(garbage) == []
        or console_problems(text, (720, 400)) == []
    ):
        failures.append("the console check")
    if failures:
        raise SystemExit(f"check-screen: self-test FAILED: {failures}")
    print("check-screen: self-test passed")


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return
    if len(sys.argv) < 3 or sys.argv[1] not in ("drawn", "console", "bav-final"):
        raise SystemExit(__doc__)
    image = read_ppm(sys.argv[2])
    if sys.argv[1] == "bav-final":
        problems = bav_final_problems(image)
        for problem in problems:
            print(f"check-screen: {problem}")
        if problems:
            raise SystemExit(f"check-screen: {sys.argv[2]} FAILED")
        print(f"check-screen: {sys.argv[2]}: {image[0]}x{image[1]} last frame: PASS")
        return
    if sys.argv[1] == "drawn":
        size = None
        if len(sys.argv) > 3:
            width, height = sys.argv[3].split("x")
            size = (int(width), int(height))
        problems = drawn_problems(image, size)
        for problem in problems:
            print(f"check-screen: {problem}")
        if problems:
            raise SystemExit(f"check-screen: {sys.argv[2]} FAILED")
        print(f"check-screen: {sys.argv[2]}: {image[0]}x{image[1]} picture: PASS")
    else:
        size = None
        if len(sys.argv) > 3:
            width, height = sys.argv[3].split("x")
            size = (int(width), int(height))
        problems = console_problems(image, size)
        if problems:
            raise SystemExit(f"check-screen: {sys.argv[2]}: {problems[0]}: FAILED")
        print(f"check-screen: {sys.argv[2]}: {image[0]}x{image[1]} console back: PASS")


if __name__ == "__main__":
    main()
