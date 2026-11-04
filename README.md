# Superhex

Voice dictation for Mac, Windows and Linux. Hold a shortcut, speak, then release
to paste into the app you're using. A personal fork of [Hex](https://github.com/anomalyco/hex).

On macOS, set up VoiceBuddy in General › Speech to use its speech service, or
choose Local model to transcribe on your Mac. Windows and Linux use local models.
Experimental features live in Labs.

## Install on macOS

Requires Apple silicon, macOS 15+, Xcode 26 or newer selected with `xcode-select`,
Rust 1.99.1 through [rustup](https://rustup.rs/), CMake and pkg-config
(`brew install cmake pkg-config`).

```sh
curl -fsSL https://raw.githubusercontent.com/junyudev/superhex/HEAD/scripts/install-app.sh | bash
```

The installer clones **the current HEAD of `junyudev/superhex`**, builds it, and
installs `/Applications/Superhex.app`. Run the same command again to update.
Build artifacts are cached in `~/Library/Caches/superhex`.

It uses your Developer ID certificate when available; otherwise it signs ad hoc
for local use. Set `HEX_TEAM_ID` to choose a signing team. These source builds
show their commit and keep automatic updates off.

Superhex has its own app identity, permissions, settings, history and models.
Grant its permissions during setup. It can be installed alongside Hex.

## Install on Linux

With the [native build dependencies](docs/linux.md) installed:

```sh
curl -fsSL https://raw.githubusercontent.com/junyudev/superhex/HEAD/scripts/install-linux.sh | bash
```

This builds remote HEAD and adds Superhex to your application launcher.

## Build this checkout

```sh
scripts/install-app.sh --local
```

Use `--no-open` to install without launching. `SUPERHEX_INSTALL_DIR` overrides
`/Applications`; `SUPERHEX_BUILD_CACHE` overrides the build cache.

The native Rust engine and Cargo target retain the internal name `hex`:

```sh
cargo run -- app
scripts/check
```

More docs are in [`docs/`](docs/README.md), including the
[Superhex identity and source installation contract](docs/features/branding.md).
[Report an issue](https://github.com/junyudev/superhex/issues).

MIT licensed. Original copyright notices are preserved.
