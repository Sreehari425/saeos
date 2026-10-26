#!/usr/bin/env bash
# Build a hybrid ISO that boots SaeOS under BIOS and UEFI, as a CD or
# written to a USB stick (dd).
#
# usage: make-iso.sh BIOS_ELF UEFI_EFI INITRAMFS OUT.iso
#        CMDLINE="..." is the kernel command line, as for the runners.
#
#   OUT.iso (ISO 9660, hybrid MBR + GPT)
#   ├── boot/saeos.bin, boot/initramfs.cpio    the BIOS kernel (ELF32) and its module
#   ├── boot/grub/                             grub.cfg, eltorito.img, i386-pc modules
#   ├── El Torito BIOS entry: GRUB (i386-pc), which loads the kernel by Multiboot
#   └── El Torito EFI entry = GPT partition 2, an ESP (FAT):
#         EFI/BOOT/BOOTX64.EFI, saeos/initramfs.cpio, saeos/cmdline.txt
#
# The UEFI kernel reads \saeos\initramfs.cpio from the volume it was loaded
# from (arch/x86_64/uefi/boot_files.rs), so the initramfs is in the ESP
# too; UEFI needs no GRUB. El Torito records a boot image's size in 16 bits
# of 512-byte sectors (32 MiB): written to a stick, firmware boots the ESP
# as a GPT partition and the limit does not apply, but from a CD a larger
# ESP does not boot under UEFI (OVMF reports the boot option "Not Found").
# BIOS boots are unaffected: GRUB reads the initramfs from the ISO 9660.
#
# Tools: objcopy, grub-mkimage (with its i386-pc directory; GRUB_PC_DIR
# overrides where that is), xorriso, mkfs.fat, mtools. `nix develop` has
# them all.
set -euo pipefail

if [ "$#" -ne 4 ]; then
    echo "usage: $0 BIOS_ELF UEFI_EFI INITRAMFS OUT.iso" >&2
    exit 2
fi
BIOS_ELF="$1"
UEFI_EFI="$2"
INITRAMFS="$3"
OUT="$4"
CMDLINE="${CMDLINE:-}"

for tool in objcopy grub-mkimage xorriso mkfs.fat mmd mcopy; do
    if ! command -v "$tool" >/dev/null; then
        echo "make-iso: $tool not found (run inside nix develop)" >&2
        exit 1
    fi
done
GRUB_PC_DIR="${GRUB_PC_DIR:-$(dirname "$(readlink -f "$(command -v grub-mkimage)")")/../lib/grub/i386-pc}"
if [ ! -f "$GRUB_PC_DIR/boot_hybrid.img" ]; then
    echo "make-iso: no GRUB i386-pc files in $GRUB_PC_DIR (set GRUB_PC_DIR)" >&2
    exit 1
fi
for file in "$BIOS_ELF" "$UEFI_EFI" "$INITRAMFS"; do
    if [ ! -f "$file" ]; then
        echo "make-iso: no $file" >&2
        exit 1
    fi
done

WORK="$(mktemp -d -t saeos-iso.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
ROOT="$WORK/root"
mkdir -p "$ROOT/boot/grub"

# BIOS: GRUB loads the Multiboot kernel (ELF32, as runner-bios does) and
# hands it the initramfs as a module. Text console: the kernel's BIOS
# console expects VGA text mode.
objcopy -O elf32-i386 "$BIOS_ELF" "$ROOT/boot/saeos.bin"
cp "$INITRAMFS" "$ROOT/boot/initramfs.cpio"
cat > "$ROOT/boot/grub/grub.cfg" <<EOF
set timeout=0
set default=0
terminal_output console
menuentry "SaeOS" {
    multiboot /boot/saeos.bin $CMDLINE
    module /boot/initramfs.cpio
}
EOF
# Modules GRUB's normal mode loads on demand.
cp -r "$GRUB_PC_DIR" "$ROOT/boot/grub/i386-pc"
chmod -R u+w "$ROOT/boot/grub"
grub-mkimage -O i386-pc-eltorito -d "$GRUB_PC_DIR" -p /boot/grub \
    -o "$ROOT/boot/grub/eltorito.img" \
    biosdisk iso9660 part_msdos part_gpt multiboot normal configfile

# UEFI: an ESP with the kernel as the removable-media default loader.
# mkfs.fat picks FAT12/16/32 by size; UEFI reads all three on removable
# media.
ESP="$WORK/efiboot.img"
payload=$(( $(stat -c %s "$UEFI_EFI") + $(stat -c %s "$INITRAMFS") ))
esp_kib=$(( payload / 1024 * 105 / 100 + 2048 ))
mkfs.fat -C -n SAEOS_ESP "$ESP" "$esp_kib" >/dev/null
export MTOOLS_SKIP_CHECK=1
mmd -i "$ESP" ::/EFI ::/EFI/BOOT ::/saeos
mcopy -i "$ESP" "$UEFI_EFI" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$ESP" "$INITRAMFS" ::/saeos/initramfs.cpio
if [ -n "$CMDLINE" ]; then
    printf '%s\n' "$CMDLINE" > "$WORK/cmdline.txt"
    mcopy -i "$ESP" "$WORK/cmdline.txt" ::/saeos/cmdline.txt
fi

xorriso -as mkisofs -quiet -o "$OUT" -iso-level 3 -R -J -V SAEOS \
    -b boot/grub/eltorito.img -no-emul-boot -boot-load-size 4 -boot-info-table \
    --grub2-boot-info --grub2-mbr "$GRUB_PC_DIR/boot_hybrid.img" \
    -append_partition 2 0xef "$ESP" -appended_part_as_gpt \
    -eltorito-alt-boot -e --interval:appended_partition_2:all:: -no-emul-boot \
    "$ROOT"

echo "make-iso: $OUT ($(( $(stat -c %s "$OUT") / 1048576 )) MiB; ESP $(( esp_kib / 1024 )) MiB)"
if [ "$esp_kib" -gt 32768 ]; then
    echo "make-iso: the ESP is over 32 MiB, El Torito's limit: UEFI boots it from a USB stick but not from a CD (BIOS boots both)"
fi
