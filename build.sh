#!/bin/sh
# Builds Cement on Linux:
#   server/Cement.cia                          the 3DS app (devkitARM + libctru)
#   client/Cement.exe, client/cement-cli.exe   the Windows apps (mingw-w64 cross compiler)
#
# Needs: makerom and bannertool (on PATH, or MAKEROM=/path BANNERTOOL=/path),
#        x86_64-w64-mingw32-g++ and -windres (Debian/Ubuntu: g++-mingw-w64-x86-64),
#        and either devkitPro installed (DEVKITARM set) or Docker, which then uses
#        the official devkitpro/devkitarm image.
#
# usage: ./build.sh [all|3ds|windows]
set -eu
cd "$(dirname "$0")"
target=${1:-all}

MAKEROM=$(command -v "${MAKEROM:-makerom}" || true)
BANNERTOOL=$(command -v "${BANNERTOOL:-bannertool}" || true)

# version: <VERSION>.<commit count>-<commit>, plus -dev with uncommitted changes
base=$(cat VERSION)
if count=$(git rev-list --count HEAD 2>/dev/null); then
	dirty=$([ -n "$(git status --porcelain)" ] && echo -dev || true)
	APP_VERSION="$base.$count-$(git rev-parse --short HEAD)$dirty"
else
	APP_VERSION="$base-dev"
fi
echo "version $APP_VERSION"

build_3ds () {
	[ -n "$MAKEROM" ] && [ -n "$BANNERTOOL" ] || { echo "makerom and bannertool are needed for the CIA" >&2; exit 1; }
	if [ -n "${DEVKITARM:-}" ]; then
		make -C server APP_VERSION="$APP_VERSION" MAKEROM="$MAKEROM" BANNERTOOL="$BANNERTOOL"
	elif command -v docker >/dev/null; then
		# CIA tools are mounted next to the sources; files are written as the current user
		docker run --rm --user "$(id -u):$(id -g)" -e HOME=/tmp \
			-v "$PWD:/src" -v "$MAKEROM:/cement-tools/makerom:ro" -v "$BANNERTOOL:/cement-tools/bannertool:ro" \
			-w /src/server devkitpro/devkitarm \
			sh -c 'git config --global --add safe.directory "*" && make APP_VERSION="$0" MAKEROM=/cement-tools/makerom BANNERTOOL=/cement-tools/bannertool' "$APP_VERSION"
	else
		echo "set DEVKITARM (devkitPro) or install Docker for the 3DS build" >&2
		exit 1
	fi
}

build_windows () {
	cxx=x86_64-w64-mingw32-g++
	flags="-std=c++17 -O2 -s -static -municode -DUNICODE -D_UNICODE -D_WIN32_WINNT=0x0A00 -DWINVER=0x0A00"
	x86_64-w64-mingw32-windres -I client/src client/src/Cement.rc -O coff -o client/Cement_res.o
	# shellcheck disable=SC2086
	$cxx -specs=client/src/nomanifest.specs $flags -mwindows -DAPP_VERSION="\"$APP_VERSION\"" \
		client/src/main.cpp client/Cement_res.o -o client/Cement.exe \
		-lws2_32 -lcomctl32 -lole32 -luuid -lshell32 -lshlwapi -luxtheme -ldwmapi -lgdi32 -luser32
	rm -f client/Cement_res.o
	echo "built Cement.exe v$APP_VERSION"
	# shellcheck disable=SC2086
	$cxx $flags -DAPP_VERSION="\"$APP_VERSION\"" client/src/cli.cpp -o client/cement-cli.exe -lws2_32 -lole32 -luuid -lshell32
	echo "built cement-cli.exe v$APP_VERSION"
}

case $target in
all) build_3ds; build_windows ;;
3ds) build_3ds ;;
windows) build_windows ;;
*) echo "usage: $0 [all|3ds|windows]" >&2; exit 1 ;;
esac
