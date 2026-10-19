#!/bin/sh
# Builds Cement on Linux:
#   server/Cement.cia, server/Cement.3dsx      the 3DS app (Rust, devkitARM + libctru)
#   client/Cement.exe, client/cement-cli.exe   the Windows apps (mingw-w64 cross compiler)
#   client/cement-cli                          the command-line tool for Linux (native g++)
#
# Needs: for the 3DS app nightly Rust with rust-src, cargo-3ds and libclang,
#        makerom and bannertool (on PATH, or MAKEROM=/path BANNERTOOL=/path),
#        x86_64-w64-mingw32-g++ and -windres (Debian/Ubuntu: g++-mingw-w64-x86-64),
#        and either devkitPro installed (DEVKITARM set) or Docker, which then uses
#        the official devkitpro/devkitarm image.
#
# usage: ./build.sh [all|3ds|windows|linux]
set -eu
cd "$(dirname "$0")"
target=${1:-all}

MAKEROM=$(command -v "${MAKEROM:-makerom}" || true)
BANNERTOOL=$(command -v "${BANNERTOOL:-bannertool}" || true)

# version: <VERSION>.<commit count>-<commit>, plus -dev with uncommitted changes
base=$(tr -d '\r\n' < VERSION) # no CR from a Windows checkout
if count=$(git rev-list --count HEAD 2>/dev/null); then
	dirty=$([ -n "$(git status --porcelain)" ] && echo -dev || true)
	APP_VERSION="$base.$count-$(git rev-parse --short HEAD)$dirty"
else
	APP_VERSION="$base-dev"
fi
echo "version $APP_VERSION"

build_3ds () {
	[ -n "$MAKEROM" ] && [ -n "$BANNERTOOL" ] || { echo "makerom and bannertool are needed for the CIA" >&2; exit 1; }
	if [ -z "${DEVKITARM:-}" ]; then
		# no devkitPro installed: copy it once out of the official Docker image
		# (running the build inside the image fails: its libc is older than
		# what the Linux makerom release needs)
		cache=${XDG_CACHE_HOME:-$HOME/.cache}/cement/devkitpro
		if [ ! -x "$cache/tools/bin/3dsxtool" ] || [ ! -d "$cache/devkitARM" ]; then
			command -v docker >/dev/null || { echo "install devkitPro (set DEVKITARM) or Docker for the 3DS build" >&2; exit 1; }
			echo "copying devkitARM out of the devkitpro/devkitarm image to $cache"
			mkdir -p "$(dirname "$cache")"
			rm -rf "$cache" "$cache.part"
			id=$(docker create devkitpro/devkitarm)
			mkdir -p "$cache.part" # complete copies only; streamed as tar because
			docker cp "$id:/opt/devkitpro/." - | tar -x -C "$cache.part" # one symlink points outside
			docker rm "$id" >/dev/null
			mv "$cache.part" "$cache"
		fi
		export DEVKITPRO="$cache" DEVKITARM="$cache/devkitARM"
		export PATH="$DEVKITPRO/tools/bin:$PATH"
	fi
	command -v cargo-3ds >/dev/null || { echo "the 3DS app is Rust: install nightly Rust with rust-src and cargo-3ds (see the README)" >&2; exit 1; }
	if [ -z "${LIBCLANG_PATH:-}" ]; then
		# ctru-sys generates its libctru bindings with libclang; take the newest LLVM
		for d in /usr/lib/llvm-*/lib; do [ -d "$d" ] && LIBCLANG_PATH=$d; done
		[ -n "${LIBCLANG_PATH:-}" ] && export LIBCLANG_PATH
	fi
	make -C server APP_VERSION="$APP_VERSION" MAKEROM="$MAKEROM" BANNERTOOL="$BANNERTOOL"
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

build_linux () {
	${CXX:-g++} -std=c++17 -O2 -s -static-libstdc++ -static-libgcc -DAPP_VERSION="\"$APP_VERSION\"" \
		client/src/cli.cpp -o client/cement-cli
	echo "built cement-cli (Linux) v$APP_VERSION"
}

case $target in
all) build_3ds; build_windows; build_linux ;;
3ds) build_3ds ;;
windows) build_windows ;;
linux) build_linux ;;
*) echo "usage: $0 [all|3ds|windows|linux]" >&2; exit 1 ;;
esac
