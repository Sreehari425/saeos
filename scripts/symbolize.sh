#!/usr/bin/env bash
# Turn the addresses in a kernel backtrace into function names and lines.
#
# usage: symbolize.sh [log-file] [bios|uefi]
#   Reads the log (default: stdin), finds the "Backtrace (image=...)" block
#   printed by the fatal path, and resolves each "#N 0x..." line against the
#   matching debug build in target/. The image is taken from the log unless
#   given explicitly.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG="${1:--}"
IMAGE="${2:-}"
# Must match mm::layout::KERNEL_IMAGE_BASE (where UEFI rebases the image).
KERNEL_IMAGE_BASE=0xffffffff80000000

INPUT="$(cat "$LOG")"
if [ -z "$IMAGE" ]; then
    IMAGE="$(grep -aoE 'Backtrace \(image=[a-z]+' <<<"$INPUT" | tail -1 | sed 's/.*=//')"
fi
ADDRESSES="$(grep -aoE '^ *#[0-9]+ 0x[0-9a-f]+' <<<"$INPUT" | awk '{print $2}')"
if [ -z "$ADDRESSES" ]; then
    echo "symbolize: no backtrace found" >&2
    exit 1
fi

case "$IMAGE" in
    bios)
        ELF="$ROOT/target/x86_64-unknown-none/debug/saeos"
        # The ELF is linked at its runtime addresses.
        # shellcheck disable=SC2086
        addr2line -f -C -i -p -e "$ELF" $ADDRESSES
        ;;
    uefi)
        EFI="$ROOT/target/x86_64-unknown-uefi/debug/saeos.efi"
        IMAGE_BASE="$(llvm-readobj --file-headers "$EFI" | awk '/ImageBase:/ {print $2}')"
        for address in $ADDRESSES; do
            # The PE was rebased from its ImageBase to KERNEL_IMAGE_BASE.
            pe=$(printf '0x%x' $(( address - KERNEL_IMAGE_BASE + IMAGE_BASE )))
            printf '%s: ' "$address"
            llvm-symbolizer --obj="$EFI" --pretty-print "$pe" | sed "/^$/d"
        done
        ;;
    *)
        echo "symbolize: unknown image '$IMAGE' (expected bios or uefi)" >&2
        exit 1
        ;;
esac
