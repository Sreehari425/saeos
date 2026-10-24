#!/usr/bin/env bash
# Run a test kernel (built with `qemu-exit`, e.g. `boot-test` or
# `mm-guard-test`) in QEMU and turn its isa-debug-exit verdict into an exit
# status. Optional log markers that must appear can be given in
# QEMU_TEST_EXPECT, one regular expression per line.
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
trap 'rm -f "$LOG_FILE"' EXIT

set +e
timeout "${QEMU_TEST_TIMEOUT:-180}" "$@" \
    -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
    -display none 2>&1 | tee "$LOG_FILE"
STATUS=${PIPESTATUS[0]}
set -e

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
echo "qemu-test: PASS"
