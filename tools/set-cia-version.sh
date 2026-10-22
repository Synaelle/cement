#!/bin/sh
# Writes the app version (major.minor.micro, any -suffix ignored) as the CIA's
# title version, in its TMD: makerom's -ver clears the minor bits (4-9).
#
# usage: sh tools/set-cia-version.sh <file.cia> <version>
set -eu
cia=$1
base=${2%%-*}
major=${base%%.*}
rest=${base#*.}
minor=${rest%%.*}
micro=${rest#*.}
[ "$major" -lt 64 ] && [ "$minor" -lt 64 ] && [ "$micro" -lt 16 ] || { echo "version $2 doesn't fit a title version" >&2; exit 1; }
v=$(( (major << 10) | (minor << 4) | micro ))

u32() { od -An -tu4 -j "$1" -N4 "$cia" | tr -d ' \n'; } # little-endian, as the CIA header
align() { echo $(( ($1 + 63) / 64 * 64 )); }
# the header, the certificate chain and the ticket come first
tmd=$(( $(align "$(u32 0)") + $(align "$(u32 8)") + $(align "$(u32 12)") ))
# makerom signs with RSA-2048 SHA-256 (type 0x00010004): a 0x140-byte signature,
# then the TMD header, with the big-endian version at 0x9C
[ "$(od -An -tu1 -j $((tmd + 3)) -N1 "$cia" | tr -d ' \n')" = 4 ] || { echo "$cia: unexpected TMD signature type" >&2; exit 1; }
printf "$(printf '\\%03o\\%03o' $((v >> 8)) $((v & 255)))" | dd of="$cia" bs=1 seek=$((tmd + 0x140 + 0x9C)) conv=notrunc status=none
