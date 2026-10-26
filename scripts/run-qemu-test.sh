#!/usr/bin/env bash
# Run a test kernel (built with `qemu-exit`, e.g. `boot-test` or
# `mm-guard-test`) in QEMU and turn its isa-debug-exit verdict into an exit
# status. Optional log markers that must appear can be given in
# QEMU_TEST_EXPECT, one regular expression per line. QEMU_TEST_SYMBOLS lists
# function names (regular expressions, one per line) that must appear once
# the log's backtrace is resolved with scripts/symbolize.sh.
#
# Typed input, once QEMU_TEST_INPUT_AFTER (a log marker) appears:
# QEMU_TEST_KEYS is QEMU key names typed on the PS/2 keyboard
# ("h i ret ctrl-d"); QEMU_TEST_SERIAL is text (with \r, \x7f escapes)
# sent to the serial port. See scripts/qemu-input.py.
#
# QEMU_TEST_SCREENDUMPS takes screenshots: lines of `marker|/path.ppm`,
# each saved once its marker appears in the log.
#
# QEMU_TEST_POSTCHECK is a command run after a PASS (e.g. checking a file
# QEMU wrote); the test fails if it does.
#
# usage: run-qemu-test.sh <runner> <image> [qemu options...]
set -euo pipefail

if [ "$#" -lt 2 ]; then
    echo "usage: $0 <runner> <image> [qemu options...]" >&2
    exit 2
fi

# Must match arch::x86_64::qemu: QEMU exits with (value << 1) | 1.
PASS_STATUS=33
FAIL_STATUS=35

LOG_FILE="$(mktemp -t saeos-qemu-test.XXXXXX)"
WORK_DIR="$(mktemp -d -t saeos-qemu-input.XXXXXX)"
trap 'rm -rf "$LOG_FILE" "$WORK_DIR"' EXIT

EXTRA=()
INPUT_MARKER="${QEMU_TEST_INPUT_AFTER:-}"
MONITOR="$WORK_DIR/monitor.sock"
if [ -n "${QEMU_TEST_KEYS:-}${QEMU_TEST_SCREENDUMPS:-}" ]; then
    EXTRA+=(-monitor "unix:$MONITOR,server,nowait")
fi
if [ -n "${QEMU_TEST_KEYS:-}" ]; then
    "$(dirname "$0")/qemu-input.py" "$LOG_FILE" "$INPUT_MARKER" keys "$MONITOR" "$QEMU_TEST_KEYS" &
fi
if [ -n "${QEMU_TEST_SCREENDUMPS:-}" ]; then
    while IFS='|' read -r marker path; do
        [ -z "$marker" ] && continue
        rm -f "$path"
        "$(dirname "$0")/qemu-input.py" "$LOG_FILE" "$marker" screendump "$MONITOR" "$path" &
    done <<< "$QEMU_TEST_SCREENDUMPS"
fi
# The guest's serial input is QEMU's stdin: a FIFO when there is text to
# send (held open here so QEMU never sees end of file), else nothing.
if [ -n "${QEMU_TEST_SERIAL:-}" ]; then
    mkfifo "$WORK_DIR/serial"
    exec 3<>"$WORK_DIR/serial"
    "$(dirname "$0")/qemu-input.py" "$LOG_FILE" "$INPUT_MARKER" serial "$WORK_DIR/serial" "$QEMU_TEST_SERIAL" &
else
    exec 3</dev/null
fi

set +e
timeout "${QEMU_TEST_TIMEOUT:-180}" "$@" \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -display none "${EXTRA[@]}" <&3 2>&1 | tee "$LOG_FILE"
STATUS=${PIPESTATUS[0]}
set -e
exec 3<&-

case "$STATUS" in
    "$PASS_STATUS") ;;
    "$FAIL_STATUS")
        echo "qemu-test: kernel reported FAIL" >&2
        exit 1
        ;;
    124)
        echo "qemu-test: timed out after ${QEMU_TEST_TIMEOUT:-180}s without a verdict" >&2
        exit 1
        ;;
    *)
        # 0 usually means a triple fault reset QEMU (-no-reboot).
        echo "qemu-test: QEMU exited with status $STATUS and no verdict" >&2
        exit 1
        ;;
esac

if [ -n "${QEMU_TEST_EXPECT:-}" ]; then
    while IFS= read -r marker; do
        [ -z "$marker" ] && continue
        if ! grep -aqE "$marker" "$LOG_FILE"; then
            echo "qemu-test: expected log line missing: $marker" >&2
            exit 1
        fi
    done <<< "$QEMU_TEST_EXPECT"
fi
if [ -n "${QEMU_TEST_SYMBOLS:-}" ]; then
    SYMBOLS="$("$(dirname "$0")/symbolize.sh" "$LOG_FILE")"
    echo "qemu-test: backtrace:"
    sed 's/^/    /' <<<"$SYMBOLS"
    while IFS= read -r symbol; do
        [ -z "$symbol" ] && continue
        if ! grep -qE "$symbol" <<<"$SYMBOLS"; then
            echo "qemu-test: backtrace does not reach $symbol" >&2
            exit 1
        fi
    done <<< "$QEMU_TEST_SYMBOLS"
fi
if [ -n "${QEMU_TEST_POSTCHECK:-}" ]; then
    if ! bash -c "$QEMU_TEST_POSTCHECK"; then
        echo "qemu-test: post-check failed: $QEMU_TEST_POSTCHECK" >&2
        exit 1
    fi
fi
echo "qemu-test: PASS"
