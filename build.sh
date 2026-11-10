#!/bin/sh
# Builds Cement.cia and Cement.3dsx on Linux (or WSL).
#
# Needs: the Makefile's nightly Rust with rust-src, cargo-3ds and libclang,
#        makerom and bannertool (on PATH, or MAKEROM=/path BANNERTOOL=/path),
#        and either devkitPro installed (DEVKITARM set) or Docker, which then
#        uses the official devkitpro/devkitarm image at the digest below.
#
# usage: ./build.sh        a release build
#        ./build.sh fast   a quicker build to try changes, no LTO (always -<commit>-dev)
set -eu
cd "$(dirname "$0")"

MAKEROM=$(command -v "${MAKEROM:-makerom}" || true)
BANNERTOOL=$(command -v "${BANNERTOOL:-bannertool}" || true)

PROFILE=release
case "${1:-}" in
	"") ;;
	fast) PROFILE=fast ;;
	*) echo "usage: ./build.sh [fast]" >&2; exit 1 ;;
esac

# version: the VERSION file as it is (major.minor.micro, bumped by hand for a
# release), plus -<commit>-dev with uncommitted changes or for a fast build
base=$(tr -d '\r\n' < VERSION) # no CR from a Windows checkout
if git rev-parse HEAD >/dev/null 2>&1; then
	# line endings and file modes differ between a Windows checkout and WSL: not changes
	dirty=$({ [ "$PROFILE" = fast ] || [ -n "$(git -c core.autocrlf=true -c core.filemode=false status --porcelain)" ]; } && echo "-$(git rev-parse --short HEAD)-dev" || true)
	APP_VERSION="$base$dirty"
else
	APP_VERSION="$base-dev"
fi
echo "version $APP_VERSION"

[ -n "$MAKEROM" ] && [ -n "$BANNERTOOL" ] || { echo "makerom and bannertool are needed for the CIA" >&2; exit 1; }
# the devkitarm image release builds use: a newer one is a deliberate change here
IMAGE=devkitpro/devkitarm@sha256:116afba8df8453961de2936ffab20dd441edf4d682856c1ec8b0e53d7ed0bbf5
if [ -z "${DEVKITARM:-}" ]; then
	# no devkitPro installed: copy it once out of the official Docker image
	# (running the build inside the image fails: its libc is older than
	# what the Linux makerom release needs)
	cache=${XDG_CACHE_HOME:-$HOME/.cache}/cement/devkitpro
	if [ ! -x "$cache/tools/bin/3dsxtool" ] || [ ! -d "$cache/devkitARM" ] || [ "$(cat "$cache/.image" 2>/dev/null)" != "$IMAGE" ]; then
		command -v docker >/dev/null || { echo "install devkitPro (set DEVKITARM) or Docker for the 3DS build" >&2; exit 1; }
		echo "copying devkitARM out of $IMAGE to $cache (a few minutes, about 600 MB)"
		mkdir -p "$(dirname "$cache")"
		rm -rf "$cache" "$cache.part" "$cache.tar"
		id=$(docker create "$IMAGE")
		trap 'docker rm "$id" >/dev/null 2>&1 || true; rm -f "$cache.tar"' EXIT
		# taken as a tar (one symlink points outside the folder), each step
		# checked, into a folder that only becomes the cache once complete
		docker cp "$id:/opt/devkitpro/." - > "$cache.tar"
		echo "unpacking $(du -h "$cache.tar" | cut -f1)"
		mkdir -p "$cache.part"
		tar -x -C "$cache.part" -f "$cache.tar"
		echo "$IMAGE" > "$cache.part/.image"
		mv "$cache.part" "$cache"
	fi
	export DEVKITPRO="$cache" DEVKITARM="$cache/devkitARM"
	export PATH="$DEVKITPRO/tools/bin:$PATH"
fi
command -v cargo-3ds >/dev/null || { echo "install the nightly Rust with rust-src and cargo-3ds (see the README)" >&2; exit 1; }
if [ -z "${LIBCLANG_PATH:-}" ]; then
	# ctru-sys generates its libctru bindings with libclang; take the newest LLVM
	for d in /usr/lib/llvm-*/lib; do [ -d "$d" ] && LIBCLANG_PATH=$d; done
	[ -n "${LIBCLANG_PATH:-}" ] && export LIBCLANG_PATH
fi
make PROFILE="$PROFILE" APP_VERSION="$APP_VERSION" MAKEROM="$MAKEROM" BANNERTOOL="$BANNERTOOL"
