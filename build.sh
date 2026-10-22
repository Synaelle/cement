#!/bin/sh
# Builds Cement.cia and Cement.3dsx on Linux (or WSL).
#
# Needs: nightly Rust with rust-src, cargo-3ds and libclang, makerom and
#        bannertool (on PATH, or MAKEROM=/path BANNERTOOL=/path), and either
#        devkitPro installed (DEVKITARM set) or Docker, which then uses the
#        official devkitpro/devkitarm image.
#
# usage: ./build.sh
set -eu
cd "$(dirname "$0")"

MAKEROM=$(command -v "${MAKEROM:-makerom}" || true)
BANNERTOOL=$(command -v "${BANNERTOOL:-bannertool}" || true)

# version: the VERSION file as it is (major.minor.micro, bumped by hand for a
# release), plus -<commit>-dev with uncommitted changes
base=$(tr -d '\r\n' < VERSION) # no CR from a Windows checkout
if git rev-parse HEAD >/dev/null 2>&1; then
	# line endings and file modes differ between a Windows checkout and WSL: not changes
	dirty=$([ -n "$(git -c core.autocrlf=true -c core.filemode=false status --porcelain)" ] && echo "-$(git rev-parse --short HEAD)-dev" || true)
	APP_VERSION="$base$dirty"
else
	APP_VERSION="$base-dev"
fi
echo "version $APP_VERSION"

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
command -v cargo-3ds >/dev/null || { echo "install nightly Rust with rust-src and cargo-3ds (see the README)" >&2; exit 1; }
if [ -z "${LIBCLANG_PATH:-}" ]; then
	# ctru-sys generates its libctru bindings with libclang; take the newest LLVM
	for d in /usr/lib/llvm-*/lib; do [ -d "$d" ] && LIBCLANG_PATH=$d; done
	[ -n "${LIBCLANG_PATH:-}" ] && export LIBCLANG_PATH
fi
make APP_VERSION="$APP_VERSION" MAKEROM="$MAKEROM" BANNERTOOL="$BANNERTOOL"
