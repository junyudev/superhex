#!/bin/bash
# Build this fork from remote HEAD, with independent data and launch entries.
set -euo pipefail

local_build=0
for argument in "$@"; do
  case "$argument" in
    install) ;;
    --local) local_build=1 ;;
    --help|-h)
      echo 'Usage: install-linux.sh [--local]'
      echo 'Build junyudev/superhex HEAD, or the current checkout with --local.'
      exit 0 ;;
    *) echo 'Usage: install-linux.sh [--local]' >&2; exit 1 ;;
  esac
done
[ "$(uname -s)" = Linux ] || { echo 'This installer requires Linux.' >&2; exit 1; }
for required in git cargo cmake pkg-config python3; do
  command -v "$required" >/dev/null || { echo "Missing $required." >&2; exit 1; }
done
cache=${SUPERHEX_BUILD_CACHE:-"${XDG_CACHE_HOME:-$HOME/.cache}/superhex"}
temporary=
trap '[ -z "$temporary" ] || rm -rf "$temporary"' EXIT
if [ "$local_build" = 1 ]; then
  root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
else
  mkdir -p "$cache"
  temporary=$(mktemp -d "$cache/source.XXXXXX")
  root="$temporary/source"
  git clone --depth 1 https://github.com/junyudev/superhex.git "$root"
  export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$cache/target"}
fi
cd "$root"
commit=$(git rev-parse HEAD)
HEX_LOCAL_BUILD=1 cargo --config "env.HEX_LOCAL_BUILD='1'" build --locked --release
support=${HEX_APPLICATION_SUPPORT_DIR:-"${XDG_DATA_HOME:-$HOME/.local/share}/superhex"}
bin_dir="$HOME/.local/bin"
applications="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
launcher="$applications/io.github.junyudev.superhex.desktop"
if [ -e "$bin_dir/superhex" ] || [ -L "$bin_dir/superhex" ]; then
  [ "$(readlink "$bin_dir/superhex")" = "$support/current/hex" ] || {
    echo "Refusing to replace $bin_dir/superhex: it is not this Superhex install." >&2; exit 1;
  }
fi
if [ -e "$launcher" ] && ! grep -Fxq 'X-Superhex-Managed=true' "$launcher"; then
  echo "Refusing to replace unmanaged $launcher." >&2; exit 1
fi
mkdir -p "$support/versions/$commit" "$bin_dir" "$applications"
chmod 700 "$support"
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
install -m755 "$target_dir/release/hex" "$support/versions/$commit/.hex.staging"
mv -f "$support/versions/$commit/.hex.staging" "$support/versions/$commit/hex"
ln -sfn "versions/$commit" "$support/current"
ln -sfn "$support/current/hex" "$bin_dir/superhex"
python3 - "$root" "$bin_dir/superhex" "$launcher" <<'PY'
from pathlib import Path
import sys
root, executable, destination = sys.argv[1:]
quoted = '"' + executable.replace('\\', '\\\\').replace('"', '\\"').replace('`', '\\`').replace('$', '\\$') + '"'
text = (Path(root) / 'packaging/io.github.junyudev.superhex.desktop').read_text()
Path(destination).write_text(text.replace('@HEX_BIN@', quoted).replace('[Desktop Entry]', '[Desktop Entry]\nX-Superhex-Managed=true'))
PY
icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons"
mkdir -p "$icons"
for icon in "$root"/packaging/linux/icons/hicolor/*/apps/io.github.junyudev.superhex.png; do
  relative=${icon#"$root/packaging/linux/icons/"}
  mkdir -p "$icons/$(dirname "$relative")"
  install -m644 "$icon" "$icons/$relative"
done
echo "Installed Superhex $commit. Open Superhex from your application launcher."
