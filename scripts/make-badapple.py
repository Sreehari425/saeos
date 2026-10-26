#!/usr/bin/env python3
"""Turn Bad Apple's video into what SaeOS plays.

usage: make-badapple.py SOURCE_DIR OUTPUT_DIR

Reads SOURCE_DIR/video.mp4 (any video works: it is scaled to 480x360)
and writes OUTPUT_DIR/video.bav (black and white run lengths, see bav.py)
and OUTPUT_DIR/audio.wav (the video's own audio track, 44.1 kHz stereo
16-bit). The audio comes from the same file as the frames so the two start
together. Needs ffmpeg; the frames are streamed, never all in memory.
"""

import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bav  # noqa: E402  (scripts/bav.py, next to this one)

WIDTH, HEIGHT, FPS = 480, 360, 30


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    source, output = sys.argv[1:]
    video = os.path.join(source, "video.mp4")
    if not os.path.exists(video):
        raise SystemExit(f"make-badapple: no {video}")
    os.makedirs(output, exist_ok=True)
    started = time.time()

    frames_path = os.path.join(output, "video.bav")
    decoder = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-i", video, "-an",
         "-vf", f"scale={WIDTH}:{HEIGHT},fps={FPS},format=gray",
         "-f", "rawvideo", "-"],
        stdout=subprocess.PIPE,
    )
    assert decoder.stdout is not None
    size = WIDTH * HEIGHT
    with open(frames_path, "wb") as file:
        encoder = bav.Encoder(file, WIDTH, HEIGHT, FPS)
        while True:
            frame = decoder.stdout.read(size)
            if len(frame) < size:
                break
            encoder.add_grey(frame)
        encoder.finish()
    if decoder.wait() != 0:
        raise SystemExit("make-badapple: ffmpeg could not decode the video")

    audio_path = os.path.join(output, "audio.wav")
    subprocess.run(
        ["ffmpeg", "-v", "error", "-y", "-i", video, "-vn",
         "-ac", "2", "-ar", "44100", "-c:a", "pcm_s16le", audio_path],
        check=True,
    )
    print(
        f"make-badapple: {encoder.frames} frames ({encoder.frames / FPS:.1f} s) -> "
        f"{frames_path} ({os.path.getsize(frames_path) / 1e6:.1f} MB), "
        f"{audio_path} ({os.path.getsize(audio_path) / 1e6:.1f} MB) "
        f"in {time.time() - started:.0f} s"
    )


if __name__ == "__main__":
    main()
