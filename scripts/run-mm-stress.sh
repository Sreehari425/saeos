#!/usr/bin/env bash
# Boot a `boot-test` + `mm-stress` kernel and require the MM stress markers
# on top of the kernel's own PASS verdict.
#
# usage: run-mm-stress.sh <runner> <image> [qemu options...]
set -euo pipefail

if [ "$#" -lt 2 ]; then
    echo "usage: $0 <runner> <image> [qemu options...]" >&2
    exit 2
fi

QEMU_TEST_EXPECT='Memory: boot MM stress report: [0-9]+ passed, 0 failed
Memory: kernel address-space user mapping self-check passed
Memory: process-root activation self-check passed
Kernel Heap Allocator initialized' \
    exec "$(dirname "$0")/run-qemu-test.sh" "$@"
