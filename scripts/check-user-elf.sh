#!/usr/bin/env bash
# Check a SaeOS user program is what exec::prepare accepts: a static x86_64
# ET_EXEC with no interpreter or dynamic section, whose entry point lies in
# its executable (R E) load segment.
#
# usage: check-user-elf.sh <elf>
set -euo pipefail
elf="$1"
fail() { echo "check-user-elf: $elf: $*" >&2; exit 1; }

header="$(readelf -h "$elf")"
grep -q 'Type:[[:space:]]*EXEC' <<<"$header" || fail "not a static executable (ET_EXEC)"
grep -q 'Machine:[[:space:]]*Advanced Micro Devices X86-64' <<<"$header" || fail "not x86_64"
segments="$(readelf -lW "$elf")"
grep -qE '^[[:space:]]*(INTERP|DYNAMIC)' <<<"$segments" && fail "dynamically linked"
entry=$(( $(awk '/Entry point address/ { print $4 }' <<<"$header") ))
inside=0
while read -r start size; do
    if (( entry >= start && entry < start + size )); then inside=1; fi
done < <(awk '$1 == "LOAD" && $7 == "R" && $8 == "E" { print strtonum($3), strtonum($6) }' <<<"$segments")
(( inside )) || fail "entry point $(printf '%#x' "$entry") is not in an executable segment"
