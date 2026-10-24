#!/usr/bin/env bash
# Boot a kernel built with `mm-guard-test` and check that the deliberate
# kernel-stack overflow is reported by name. The kernel halts after the
# report, so the runner is stopped by a timeout.
set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <runner> [runner arguments...]" >&2
    exit 2
fi

LOG_FILE="$(mktemp -t saeos-guard-test.XXXXXX)"
trap 'rm -f "$LOG_FILE"' EXIT

set +e
timeout "${GUARD_TEST_TIMEOUT:-60}" "$@" 2>&1 | tee "$LOG_FILE"
set -e

if ! rg -q "KERNEL STACK OVERFLOW in task 'guard-test'" "$LOG_FILE"; then
    echo "stack overflow was not reported for guard-test" >&2
    exit 1
fi
echo "guard test passed"
