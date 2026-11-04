#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
. "$root/scripts/macos-signing.sh"
# HEX_TEST_BUILD=1 builds the CI test app: ad-hoc signed, under its own
# name and bundle ID so it installs beside the released Superhex and never
# touches its permissions; it never checks for updates.
# HEX_LOCAL_BUILD=1 marks a build made from a checkout for its owner's own
# machine (Developer ID signed as usual): it shows "Local build · updates off"
# with its commit and never checks for or offers updates.
# HEX_RELEASE_UNSIGNED=1 builds the release app (Superhex, io.github.junyudev.superhex) ad-hoc
# signed: the Release workflow's build job, which holds no credentials. Its
# sign-macos job signs it with the Developer ID (.github/release/sign-macos.sh).
if [ "${HEX_RELEASE_UNSIGNED:-}" = 1 ]; then
  team_id=
  identity=-
  app_name=Superhex
  bundle_id=io.github.junyudev.superhex
elif [ "${HEX_TEST_BUILD:-}" = 1 ]; then
  team_id=
  identity=-
  app_name="Superhex Test"
  bundle_id=io.github.junyudev.superhex.test
else
  team_id=${HEX_TEAM_ID:-}
  if [ -z "$team_id" ]; then
    team_id=$(security find-identity -v -p codesigning | sed -n 's/.*"Developer ID Application:.*(\([A-Z0-9]*\))"/\1/p' | head -1)
  fi
  identity=-
  [ -z "$team_id" ] || identity=$(hex_codesign_identity "$team_id")
  export HEX_LOCAL_BUILD=${HEX_LOCAL_BUILD:-1}
  app_name=Superhex
  bundle_id=io.github.junyudev.superhex
fi
# Every build carries the `capture` experiment (dormant until enabled), with
# Scrawl's `scrawl-helper` beside `hex`, where the engine looks for it.
# crates/capture/build.rs compiles the helper from crates/capture/helper into
# target/release.
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
case "$target_dir" in /*) ;; *) target_dir="$root/$target_dir" ;; esac
export CARGO_TARGET_DIR="$target_dir"
icon_output="$target_dir/AppIcon.assets"
icon_info="$target_dir/AppIcon-info.plist"
version=$(hex_version "$root")
build_number=$(hex_build_number "$version")
bundle_name="$app_name.app"
executable_name=superhex

bundle="$target_dir/app/$bundle_name"
executable="$bundle/Contents/MacOS/$executable_name"

sign() {
  if [ "$identity" = - ]; then
    codesign --force --sign - "$@"
  else
    codesign --force --timestamp --options runtime --sign "$identity" "$@"
  fi
}

export MACOSX_DEPLOYMENT_TARGET=15.0
# Cargo may run through a wrapper that does not forward the environment (Kit's
# HiveMind-backed cargo), so build-time variables also travel as --config.
env_args=()
if [ -z "${HEX_COMMIT:-}" ] && { [ "${HEX_LOCAL_BUILD:-}" = 1 ] || [ "${HEX_TEST_BUILD:-}" = 1 ]; }; then
  HEX_COMMIT=$(git -C "$root" rev-parse --short=7 HEAD 2>/dev/null || true)
fi
for name in HEX_LOCAL_BUILD HEX_TEST_BUILD HEX_COMMIT HEX_BUILD_CHANNEL; do
  value=${!name:-}
  if [ -n "$value" ]; then
    case "$value" in
      *\'*) echo "$name must not contain a single quote" >&2; exit 1 ;;
    esac
    env_args+=(--config "env.$name='$value'")
  fi
done
# The helper's embedded Info.plist names Superhex, not Scrawl.
SCRAWL_HELPER_PLIST="$root/packaging/macos/scrawl-helper-Info.plist" \
  cargo build --locked --release ${env_args[@]+"${env_args[@]}"} --manifest-path "$root/Cargo.toml"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$bundle/Contents/Library"
cp "$target_dir/release/hex" "$executable"
cp "$target_dir/release/scrawl-helper" "$bundle/Contents/MacOS/scrawl-helper"
cp "$root/app/Info.plist" "$bundle/Contents/Info.plist"
cp "$root/LICENSE" "$root/THIRD_PARTY_NOTICES.md" "$bundle/Contents/Resources/"
HEX_VERSION="$version" HEX_BUILD_NUMBER="$build_number" HEX_TEAM_ID="$team_id" HEX_CODESIGN_IDENTITY="${identity#-}" HEX_SKIP_INSTALL_SCRIPT=1 \
  "$root/scripts/build-hex-microphone-driver.sh" "$bundle/Contents/Library" >&2
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $build_number" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $bundle_id" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable $executable_name" "$bundle/Contents/Info.plist"
plutil -replace CFBundleName -string "$app_name" "$bundle/Contents/Info.plist"
plutil -replace CFBundleDisplayName -string "$app_name" "$bundle/Contents/Info.plist"
# Links into the playground (src/rune_ui/story_link.rs); a test build's own
# scheme never opens the installed Superhex.
url_scheme=superhex
[ "${HEX_TEST_BUILD:-}" = 1 ] && url_scheme=superhex-test
/usr/libexec/PlistBuddy -c "Set :CFBundleURLTypes:0:CFBundleURLName $bundle_id" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleURLTypes:0:CFBundleURLSchemes:0 $url_scheme" "$bundle/Contents/Info.plist"
# The helper mirrors a cabled iPhone as a camera; macOS asks on Superhex's behalf.
plutil -replace NSCameraUsageDescription -string "Superhex mirrors and records a cabled iPhone's screen for a capture." "$bundle/Contents/Info.plist"
rm -rf "$icon_output"
mkdir -p "$icon_output"
xcrun actool "$root/app/AppIcon.icon" \
  --compile "$icon_output" \
  --platform macosx \
  --minimum-deployment-target 15.0 \
  --app-icon AppIcon \
  --standalone-icon-behavior all \
  --output-partial-info-plist "$icon_info" \
  --output-format human-readable-text \
  --warnings \
  --notices >&2
cp "$icon_output/AppIcon.icns" "$icon_output/Assets.car" "$bundle/Contents/Resources/"

sign "$bundle/Contents/Library/SuperhexMicrophone.driver"
# Superhex runs it only to decode the capture's movie, which needs no
# entitlement; anything it touches is attributed to Superhex, its responsible
# process.
sign --identifier "$bundle_id.scrawl-helper" "$bundle/Contents/MacOS/scrawl-helper"
sign --entitlements "$root/.github/release/hex.entitlements" "$bundle"
codesign --verify --deep --strict --verbose=2 "$bundle"
echo "$bundle"
