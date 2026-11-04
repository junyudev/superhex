# VoiceBuddy native client

One recording per session, using only an imported configuration. No identity
creation, subprocesses, or runtime codec downloads. The client implements the
speech service's presence-sensitive protobuf envelopes, signing, raw Snappy,
AES/PKCS7, fresh secp128r1 exchange, Opus upload, partials, polling, and final text.

`Session::feed` accepts normalized 16 kHz mono samples. It never waits for the
network. Audio survives connection setup; exceeding the 16 MiB PCM queue fails
the session. `transcribe` uploads a whole clip as a burst, waiting for local
queue capacity for large clips. Call `Canceller::cancel()` or drop a Session or
Pending to cancel the work. Dropping a Canceller or any of its clones has no
effect on the work.

Connections have a 10 s total DNS/TCP/TLS/WebSocket budget; handshake requests
have 5 s each. DNS uses the system resolver (`ToSocketAddrs` / `getaddrinfo`) on
a detached thread, preserving macOS scoped and split DNS. It receives only the
hostname and port, with no identity or credentials. Cancellation abandons the
oneshot reply immediately; a blocked lookup may finish independently. TLS uses
the platform's `native-tls` through `tokio-websockets`. Final and polishing
deadlines start at the final upload and the first polishing response,
respectively. A refresh
cannot extend either deadline. Codes 5004/5011 drain outstanding responses for
up to 3 s, refresh the clock/key as required, and resend each failed business
body at most once. Only handshake business rejection or device binding mismatch
is `Rejected` (binding mismatch uses synthetic code -1); transport and ordinary
service errors remain retryable.

Public methods, fields, and defaults are unchanged. `Canceller::new()` and
`Default` provide an independent whole-clip cancellation handle. `Update` and
`Failed` retain `Debug`, with transcript fields redacted.
The implementation emits no logs. Dependency tracing is suppressed within the
session, and the production WebSocket transport has no payload/endpoint logger.
`Config::to_json` intentionally returns durable credentials for private storage.
Config import accepts a context object or a `configuration` wrapper, ignores
unknown fields, and accepts the optional empty or `/ppp` WebSocket path.

Opus uses `opus` / `opusic-sys`'s bundled, statically linked libopus 1.6.1
(license in `OPUS-COPYING`). Settings
match the reference: 16 kHz mono, AUDIO, 64 kbps, complexity 10, VOICE, wideband,
constrained VBR, no DTX/FEC/loss, 24-bit LSB depth, 320 samples per frame. Only
the last frame is zero-padded. The initial magic and each little-endian u16
packet length match the worker. Floating-point libopus was accepted live and
produced the same final text as the reference's fixed-point library. Bit-exact
Opus packets are not required or promised.

## Offline checks

From the Hex worktree root, with the installed toolchain selected explicitly:

```sh
RUSTUP_TOOLCHAIN=1.99.1-aarch64-apple-darwin cargo test -p voice-buddy
RUSTUP_TOOLCHAIN=1.99.1-aarch64-apple-darwin cargo clippy -p voice-buddy --all-targets -- -D warnings
```

The root test command passed all 27 tests. Tests use invented credentials,
a local fake service using `tokio-websockets`' server feature, and
Python-generated secp128r1/AES vectors embedded as constants. They cover retained
pre-handshake audio, arbitrary sample boundaries, short/empty clips, partial and
refined text precedence, heartbeat/voice filtering, poll presence, both phase
deadlines, service failure classification, refresh/drain/resend, cancellation
through connection and result stages, and queue overflow. `testing` adds
`Session::scripted` without a thread or network. Python IPC frame parsing is
intentionally absent because this client has no IPC channel. Tests also prove
that dropping cancellation handles preserves linked work and a cancelled
session exits before an abandoned system lookup finishes.

The lockfile adds 13 packages over the pre-client baseline, down from 38:
`aes`, `block-padding`, `cipher`, `crypto-bigint`, `ecb`, `inout`, `md-5`,
`opus`, `opusic-sys`, `sha1_smol`, `snap`, `tokio-native-tls`, and
`tokio-websockets`. Platform TLS otherwise reuses Hex's existing dependencies.
The lockfile change shrank from 463 added / 3 removed lines to 150 added lines.

## Explicit live verification

This example only runs when requested. Config and WAV paths must stay outside
the repository. It prints counts and timing, never transcript/config values.
`--output` optionally creates a new owner-only text file at the supplied path.
Use a 16 kHz mono PCM16 WAV. The crate's empty `[workspace]` is needed to run
this standalone example from a worktree nested inside the main checkout;
without it, Cargo treats the crate as an unlisted member of that checkout.

```sh
RUSTUP_TOOLCHAIN=1.99.1-aarch64-apple-darwin cargo run \
  --manifest-path crates/voice-buddy/Cargo.toml --features live --example live -- \
  --config /private/context.json --wav /private/clip.wav --pace 1
```

`--check` performs only the handshake. `--pace 4` uploads four times faster than
real time; `--pace 0` feeds a burst. `--transcribe` exercises the whole-clip API.
`--defaults` uses the API's default options, `--no-polish` disables polishing,
and `--cancel` cancels immediately after ending the recording.

Follow-up verification on macOS, 2026-11-04, used system DNS and platform TLS
with the same 12.114 s synthetic Chinese/English clip. Exactly three service
sessions ran, using API defaults:

| Run | First partial | Release to final | Total |
| --- | ---: | ---: | ---: |
| `check()` | — | — | 1,181 ms |
| Real time, polish | 881 ms | 512 ms | 12,634 ms |
| `transcribe()` burst, polish | 2,368 ms | 2,432 ms | 2,505 ms |

Release-to-final for a burst includes connection/key exchange and encoding;
the final-phase clock begins at upload, not at the caller's release. Real-time
streaming produced 34 partial/polishing updates. Both polished results matched
each other and the earlier Python worker result, without logging transcript
or configuration values.

The initial ten-session verification also compared real-time, 4x, and burst
uploads: every polished result matched the fixed-point Python worker. Polish
off and cancellation passed in that run. Burst remains the fastest tested
upload policy and the default for `transcribe()`.

These are compatibility measurements on one short clip, not a reliability or
long-recording benchmark. Refresh errors were induced only in offline tests.
The custom curve code has independent vectors but has not had a cryptographic
audit. No native Hex UI or microphone integration was exercised by this crate.
