#!/usr/bin/env bash
set -euo pipefail

missing=0

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing: $1" >&2
        missing=1
    fi
}

need nix

if command -v nix >/dev/null 2>&1; then
    nix_probe="$(mktemp -t saeos-nix-check.XXXXXX)"
    if ! nix --extra-experimental-features 'nix-command flakes' flake metadata path:. >"$nix_probe" 2>&1; then
        echo "nix flake metadata check failed:" >&2
        sed 's/^/  /' "$nix_probe" >&2
        missing=1
    fi
    rm -f "$nix_probe"
fi

for optional in cargo rustc nasm qemu-system-x86_64; do
    if ! command -v "$optional" >/dev/null 2>&1; then
        echo "optional outside nix shell: $optional" >&2
    fi
done

if [ "$missing" -ne 0 ]; then
    exit 1
fi

echo "SaeOS build prerequisites look usable."
