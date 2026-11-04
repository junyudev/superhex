# Superhex identity and source installation

```ts
Install Superhex                         // setup.superhex-source
├── scripts/install-app.sh (also curl | bash)
│   ├── Clone https://github.com/junyudev/superhex.git at remote HEAD
│   ├── Build the exact fetched commit with the pinned Rust toolchain
│   ├── Sign with a local Developer ID, or ad hoc when none is available
│   └── Stage, verify, replace only Superhex.app, register, and launch
├── --local -> build the current checkout instead
├── --no-open -> install without launching
└── Repeat -> rebuild current remote HEAD; reuse the build cache

Open Superhex                            // maintain.superhex-identity
├── Bundle: Superhex.app; executable: superhex
├── Identity: io.github.junyudev.superhex (test builds append .test)
├── Links: superhex:// (test builds: superhex-test://)
├── Settings/history/models: platform data folder / superhex
├── Custom transformations: ~/.config/superhex/hex.config.ts
│   └── Existing @hex/commands API and HEX_* build/runtime overrides remain
├── Fresh permissions; no automatic import from Hex
├── No upstream Hex permission cleanup or forced quit
└── Source install -> Local build, commit shown, upstream updates off
```

Linux uses `scripts/install-linux.sh` with the same remote HEAD default and
`--local` override. It installs a user-local executable and launcher without
starting the app or registering autostart.

The test app uses `Superhex Test.app`, `superhex-test` data and config folders.
The bundled virtual microphone displays `Superhex Microphone` and uses this
fork's own device and driver identifiers. The internal Cargo target (`hex`),
and configuration API remain implementation details.

Sources: [macOS installer](../../scripts/install-app.sh),
[Linux installer](../../scripts/install-linux.sh),
[bundle build](../../scripts/build-app.sh), [Info.plist](../../app/Info.plist),
[paths](../../src/app_paths.rs), [permission identity](../../src/desktop/onboarding.rs),
[links](../../src/rune_ui/story_link.rs), [microphone](../../src/audio/hex_microphone.rs),
and [SDK discovery](../../sdk/typescript/src/host.ts).

Checks: `scripts/test-app-identity.sh`, `app_paths::tests`, the onboarding
permission-scope tests, deep-link tests, translation completeness, and the Linux
Rune golden suite. These checks do not grant macOS permissions or prove live
microphone capture, dictation, or pasting in another app.
