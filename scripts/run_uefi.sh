#!/usr/bin/env bash
set -euo pipefail

# 1. Build SaeOS for 64-bit UEFI target
echo "==> Building SaeOS for x86_64-unknown-uefi..."
cargo build --target x86_64-unknown-uefi "$@"

# 2. Setup EFI System Partition directory structure (FAT filesystem for QEMU)
ESP_DIR="${ESP_DIR:-build/esp}"
BOOT_DIR="$ESP_DIR/EFI/BOOT"
mkdir -p "$BOOT_DIR"

# 3. Copy PE32+ .efi binary to standard default UEFI fallback path BOOTX64.EFI
cp target/x86_64-unknown-uefi/debug/saeos.efi "$BOOT_DIR/BOOTX64.EFI"
echo "==> Prepared $BOOT_DIR/BOOTX64.EFI"

# 4. Locate OVMF firmware
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

echo "==> Booting SaeOS in UEFI Mode with OVMF ($OVMF_PATH)..."
exec qemu-system-x86_64 \
    -bios "$OVMF_PATH" \
    -drive format=raw,file=fat:rw:"$ESP_DIR" \
    -serial stdio \
    -no-reboot \
    "$@"
