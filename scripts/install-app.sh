#!/bin/bash
# Install the current HEAD of junyudev/superhex. Also works via `curl | bash`.
set -euo pipefail

usage() {
  cat <<'HELP'
Usage: install-app.sh [--local] [--no-open]

Build and install Superhex.app from https://github.com/junyudev/superhex HEAD.
Requires Apple silicon, macOS 15+, Xcode 26+, Rust, CMake and pkg-config.
Uses a Developer ID certificate when available; otherwise signs ad hoc.

  --local    Build this checkout instead of fetching GitHub HEAD.
  --no-open  Install without launching.

SUPERHEX_INSTALL_DIR overrides /Applications.
SUPERHEX_BUILD_CACHE overrides ~/Library/Caches/superhex.
HEX_TEAM_ID selects a Developer ID signing team.
HELP
}

local_build=0
launch=1
for argument in "$@"; do
  case "$argument" in
    --local) local_build=1 ;;
    --no-open) launch=0 ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 1 ;;
  esac
done
if [ "$(uname -s) $(uname -m)" != 'Darwin arm64' ]; then
  echo 'Superhex.app requires an Apple silicon Mac.' >&2
  exit 1
fi
for required in git cargo cmake pkg-config xcrun; do
  if ! command -v "$required" >/dev/null 2>&1; then
    echo "Missing $required. Install the prerequisites listed in the README." >&2
    exit 1
  fi
done
xcrun --find actool >/dev/null

temporary=
cleanup() {
  [ -z "$temporary" ] || rm -rf "$temporary"
}
stop_bundle() {
  [ -d "$1" ] || return 0
  local executable="$1/Contents/MacOS/superhex"
  local pids
  pids=$(pgrep -f "^$executable([[:space:]]|$)" || true)
  [ -n "$pids" ] || return 0
  kill -TERM $pids
  for ((attempt=0; attempt<30; attempt++)); do
    pgrep -f "^$executable([[:space:]]|$)" >/dev/null || return 0
    sleep 1
  done
  echo 'Superhex is still finishing its work. Try again after it quits.' >&2
  exit 1
}
trap cleanup EXIT
if [ "$local_build" = 1 ]; then
  root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
else
  cache=${SUPERHEX_BUILD_CACHE:-"$HOME/Library/Caches/superhex"}
  mkdir -p "$cache"
  temporary=$(mktemp -d "$cache/source.XXXXXX")
  root="$temporary/source"
  git clone --depth 1 https://github.com/junyudev/superhex.git "$root" >&2
  export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$cache/target"}
fi
commit=$(git -C "$root" rev-parse HEAD)
short_commit=$(git -C "$root" rev-parse --short=7 HEAD)
echo "Building Superhex from $commit" >&2
source_bundle=$(HEX_LOCAL_BUILD=1 HEX_TEST_BUILD=0 HEX_RELEASE_UNSIGNED=0 HEX_COMMIT="$short_commit" "$root/scripts/build-app.sh")
codesign --verify --deep --strict "$source_bundle"
source_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$source_bundle/Contents/Info.plist")
if [ "$source_id" != io.github.junyudev.superhex ]; then
  echo 'The build did not produce the Superhex application.' >&2
  exit 1
fi

install_dir=${SUPERHEX_INSTALL_DIR:-/Applications}
installed_bundle="$install_dir/Superhex.app"
if [ -e "$installed_bundle" ] || [ -L "$installed_bundle" ]; then
  existing_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$installed_bundle/Contents/Info.plist" 2>/dev/null || true)
  if [ "$existing_id" != io.github.junyudev.superhex ]; then
    echo "Refusing to replace $installed_bundle: it is not Superhex." >&2
    exit 1
  fi
fi
mkdir -p "$install_dir"
# Stage and verify before stopping or replacing an installed copy.
staging=$(mktemp -d "$install_dir/.superhex.XXXXXX")
trap 'rm -rf "$staging"; cleanup' EXIT
ditto "$source_bundle" "$staging/Superhex.app"
codesign --verify --deep --strict "$staging/Superhex.app"
stop_bundle "$installed_bundle"
if [ -d "$installed_bundle" ]; then
  mv "$installed_bundle" "$staging/previous.app"
fi
if ! mv "$staging/Superhex.app" "$installed_bundle"; then
  [ ! -d "$staging/previous.app" ] || mv "$staging/previous.app" "$installed_bundle"
  exit 1
fi
'/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister' -f "$installed_bundle"
if [ "$launch" = 1 ]; then
  open "$installed_bundle"
fi
echo "$installed_bundle"
