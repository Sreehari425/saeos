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

# 3. Locate OVMF firmware. Nix users can provide the store path explicitly.
OVMF_PATH="${OVMF_PATH:-}"
if [ -z "$OVMF_PATH" ]; then
    OVMF_PATH="/usr/share/edk2/x64/OVMF.4m.fd"
fi
if [ ! -f "$OVMF_PATH" ]; then
    OVMF_PATH="/usr/share/OVMF/OVMF.fd"
fi

if [ ! -f "$OVMF_PATH" ]; then
    echo "ERROR: OVMF UEFI firmware not found!"
    exit 1
fi

# 4. Launch QEMU
echo "==> Booting SaeOS in UEFI Mode with OVMF ($OVMF_PATH)..."
exec qemu-system-x86_64 \
    -bios "$OVMF_PATH" \
    -drive format=raw,file=fat:rw:"$ESP_DIR" \
    -serial stdio \
    -no-reboot \
    "$@"
