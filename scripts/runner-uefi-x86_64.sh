#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 1 ]; then
    echo "Usage: $0 <kernel-efi> [qemu-options...]" >&2
    exit 1
fi

EFI_BINARY="$1"
shift

# 1. Setup a temporary writable EFI System Partition for QEMU
ESP_DIR="$(mktemp -d -t saeos-uefi-esp.XXXXXX)"
trap 'rm -rf "$ESP_DIR"' EXIT
BOOT_DIR="$ESP_DIR/EFI/BOOT"
mkdir -p "$BOOT_DIR"

# 2. Copy PE32+ .efi binary to standard default UEFI fallback path BOOTX64.EFI
cp "$EFI_BINARY" "$BOOT_DIR/BOOTX64.EFI"
echo "==> Prepared $BOOT_DIR/BOOTX64.EFI"

# 3. The kernel reads its initramfs and command line from the ESP:
# INITRAMFS=target/initramfs.cpio CMDLINE="loglevel=debug".
if [ -n "${INITRAMFS:-}" ] || [ -n "${CMDLINE:-}" ]; then
    mkdir -p "$ESP_DIR/saeos"
fi
if [ -n "${INITRAMFS:-}" ]; then
    cp "$INITRAMFS" "$ESP_DIR/saeos/initramfs.cpio"
fi
if [ -n "${CMDLINE:-}" ]; then
    printf '%s\n' "$CMDLINE" > "$ESP_DIR/saeos/cmdline.txt"
fi

# 4. Locate OVMF firmware.
. "$(dirname "$0")/find-ovmf.sh"

# 5. Launch QEMU
echo "==> Booting SaeOS in UEFI Mode with OVMF ($OVMF_PATH)..."
. "$(dirname "$0")/qemu-child.sh"
status=0
run_qemu \
    -bios "$OVMF_PATH" \
    -drive format=raw,file=fat:rw:"$ESP_DIR" \
    -serial stdio \
    -no-reboot \
    "$@" || status=$?
exit "$status"
