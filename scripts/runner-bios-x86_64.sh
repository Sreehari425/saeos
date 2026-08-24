#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 1 ]; then
    echo "Usage: $0 <kernel-elf> [qemu-options...]" >&2
    exit 1
fi

KERNEL_ELF="$1"
shift

TEMP_DIR="$(mktemp -d -t saeos-bios.XXXXXX)"
trap 'rm -rf "$TEMP_DIR"' EXIT
KERNEL_BIN="$TEMP_DIR/saeos.bin"

# 1. Convert 64-bit ELF container to Multiboot-compatible container for QEMU/GRUB
objcopy -O elf32-i386 "$KERNEL_ELF" "$KERNEL_BIN"

# 2. Launch QEMU with kernel and pass through any additional flags.
# High-frame testing requires explicit QEMU memory, for example: cargo run -- -m 16G
exec qemu-system-x86_64 \
    -kernel "$KERNEL_BIN" \
    -serial stdio \
    -no-reboot \
    "$@"
