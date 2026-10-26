# Sourced by the UEFI runners: set OVMF_PATH to UEFI firmware for QEMU.
# A path already in OVMF_PATH wins (the Nix devShell sets one); otherwise
# the usual distribution locations are tried.
if [ -z "${OVMF_PATH:-}" ]; then
    for candidate in /usr/share/edk2/x64/OVMF.4m.fd /usr/share/OVMF/OVMF.fd; do
        if [ -f "$candidate" ]; then
            OVMF_PATH="$candidate"
            break
        fi
    done
fi
if [ ! -f "${OVMF_PATH:-}" ]; then
    echo "ERROR: OVMF UEFI firmware not found! Set OVMF_PATH, or run inside nix develop." >&2
    exit 1
fi
