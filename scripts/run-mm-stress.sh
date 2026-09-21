#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <runner> [runner arguments...]" >&2
    exit 2
fi

LOG_FILE="$(mktemp -t saeos-mm-stress.XXXXXX)"
trap 'rm -f "$LOG_FILE"' EXIT

set +e
"$@" 2>&1 | tee "$LOG_FILE"
STATUS=${PIPESTATUS[0]}
set -e

if [ "$STATUS" -ne 0 ]; then
    echo "MM stress runner failed with status $STATUS" >&2
    exit "$STATUS"
fi

if ! rg -q 'Memory: boot MM stress report: [0-9]+ passed, 0 failed' "$LOG_FILE"; then
    echo "MM stress completion marker missing or reported failures" >&2
    exit 1
fi
if ! rg -q 'Memory: kernel address-space user mapping self-check passed' "$LOG_FILE"; then
    echo "address-space self-test marker missing" >&2
    exit 1
fi
if ! rg -q 'Kernel Heap Allocator initialized' "$LOG_FILE"; then
    echo "heap self-test marker missing" >&2
    exit 1
fi
