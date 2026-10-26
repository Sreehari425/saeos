#!/usr/bin/env bash
# Boot a SaeOS ISO (scripts/make-iso.sh) in QEMU, the way real hardware
# would: as a CD or as a USB stick it was written to, under BIOS or UEFI.
# The command line and initramfs are inside the ISO.
#
# usage: ISO_FIRMWARE=bios|uefi ISO_MEDIUM=cdrom|usb runner-iso.sh ISO [qemu options...]
set -euo pipefail

if [ "$#" -lt 1 ]; then
    echo "usage: ISO_FIRMWARE=bios|uefi ISO_MEDIUM=cdrom|usb $0 ISO [qemu options...]" >&2
    exit 2
fi
ISO="$1"
shift

FIRMWARE=()
case "${ISO_FIRMWARE:-uefi}" in
    bios) ;;
    uefi)
        . "$(dirname "$0")/find-ovmf.sh"
        FIRMWARE=(-bios "$OVMF_PATH")
        ;;
    *) echo "ISO_FIRMWARE must be bios or uefi" >&2; exit 2 ;;
esac

case "${ISO_MEDIUM:-usb}" in
    cdrom) MEDIUM=(-drive "if=none,id=disc,media=cdrom,readonly=on,format=raw,file=$ISO" -device ide-cd,drive=disc,bootindex=0) ;;
    usb) MEDIUM=(-drive "if=none,id=stick,readonly=on,format=raw,file=$ISO" -device qemu-xhci -device usb-storage,drive=stick,bootindex=0) ;;
    *) echo "ISO_MEDIUM must be cdrom or usb" >&2; exit 2 ;;
esac

echo "==> Booting $ISO as a ${ISO_MEDIUM:-usb} (${ISO_FIRMWARE:-uefi})"
exec qemu-system-x86_64 \
    "${FIRMWARE[@]}" \
    "${MEDIUM[@]}" \
    -serial stdio \
    -no-reboot \
    "$@"
