#!/bin/sh
# Writes the app version (major.minor.micro, any -suffix ignored) as the CIA's
# title version, in its TMD: makerom's -ver clears the minor bits (4-9).
# Without a version it prints the title version the CIA has.
#
# usage: sh tools/set-cia-version.sh <file.cia> [<version>]
set -eu
cia=$1

u32() { od -An -tu4 -j "$1" -N4 "$cia" | tr -d ' \n'; } # little-endian, as the CIA header
align() { echo $(( ($1 + 63) / 64 * 64 )); }
# the header, the certificate chain and the ticket come first
tmd=$(( $(align "$(u32 0)") + $(align "$(u32 8)") + $(align "$(u32 12)") ))
# makerom signs with RSA-2048 SHA-256 (type 0x00010004): a 0x140-byte signature,
# then the TMD header, with the big-endian version at 0x9C
[ "$(od -An -tu1 -j $((tmd + 3)) -N1 "$cia" | tr -d ' \n')" = 4 ] || { echo "$cia: unexpected TMD signature type" >&2; exit 1; }
at=$((tmd + 0x140 + 0x9C))

if [ $# -lt 2 ]; then
	set -- $(od -An -tu1 -j "$at" -N2 "$cia")
	v=$(( ($1 << 8) | $2 ))
	echo "$((v >> 10)).$(((v >> 4) & 63)).$((v & 15))"
	exit 0
fi

base=${2%%-*}
case $base in
	*[!0-9.]* | *..* | .* | *. | *.*.*.*) base= ;;
	*.*.*) ;;
	*) base= ;;
esac
[ -n "$base" ] || { echo "version $2 isn't major.minor.micro" >&2; exit 1; }
major=${base%%.*}
rest=${base#*.}
minor=${rest%%.*}
micro=${rest#*.}
[ "$major" -lt 64 ] && [ "$minor" -lt 64 ] && [ "$micro" -lt 16 ] || { echo "version $2 doesn't fit a title version" >&2; exit 1; }
v=$(( (major << 10) | (minor << 4) | micro ))
printf "$(printf '\\%03o\\%03o' $((v >> 8)) $((v & 255)))" | dd of="$cia" bs=1 seek="$at" conv=notrunc status=none
