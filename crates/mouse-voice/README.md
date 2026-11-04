# MouseVoice native client

One recording per session, using only an imported configuration. No identity
creation, subprocesses, or runtime codec downloads. The client implements the
speech service's presence-sensitive protobuf envelopes, signing, raw Snappy,
AES/PKCS7, fresh secp128r1 exchange, Opus upload, partials, polling, and final text.

`Session::feed` accepts normalized 16 kHz mono samples. It never waits for the
network. Audio survives connection setup; exceeding the 16 MiB PCM queue fails
the session. `transcribe` uploads a whole clip as a burst, waiting for local
queue capacity for large clips. Dropping a Session, Pending, **or any Canceller
clone** cancels the work. Keep cancellation handles alive until completion.

Connections have a 10 s total DNS/TCP/TLS/WebSocket budget; handshake requests
have 5 s each. DNS runs asynchronously on the session's current-thread runtime,
without a detached blocking resolver. Final and polishing deadlines start at
the final upload and the first polishing response, respectively. A refresh
cannot extend either deadline. Codes 5004/5011 drain outstanding responses for
up to 3 s, refresh the clock/key as required, and resend each failed business
body at most once. Only handshake business rejection or device binding mismatch
is `Rejected` (binding mismatch uses synthetic code -1); transport and ordinary
service errors remain retryable.

The requested additions are `Canceller::new()` and `Default` for an independent
whole-clip cancellation handle. Other public signatures, fields, and defaults
are unchanged from the initial contract. `Update` and `Failed` retain `Debug`, with transcript fields redacted.
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
RUSTUP_TOOLCHAIN=1.99.1-aarch64-apple-darwin cargo test -p mouse-voice
RUSTUP_TOOLCHAIN=1.99.1-aarch64-apple-darwin cargo clippy -p mouse-voice --all-targets -- -D warnings
```

The root test command passed all 26 tests. Tests use invented credentials,
a local independent WebSocket server, and
Python-generated secp128r1/AES vectors embedded as constants. They cover retained
pre-handshake audio, arbitrary sample boundaries, short/empty clips, partial and
refined text precedence, heartbeat/voice filtering, poll presence, both phase
deadlines, service failure classification, refresh/drain/resend, cancellation
through connection and result stages, and queue overflow. `testing` adds
`Session::scripted` without a thread or network. Python IPC frame parsing is
intentionally absent because this client has no IPC channel.

## Explicit live verification

This example only runs when requested. Config and WAV paths must stay outside
the repository. It prints counts and timing, never transcript/config values.
`--output` optionally creates a new owner-only text file at the supplied path.
Use a 16 kHz mono PCM16 WAV.

```sh
RUSTUP_TOOLCHAIN=1.99.1-aarch64-apple-darwin cargo run \
  --manifest-path crates/mouse-voice/Cargo.toml --features live --example live -- \
  --config /private/context.json --wav /private/clip.wav --pace 1
```

`--check` performs only the handshake. `--pace 4` uploads four times faster than
real time; `--pace 0` feeds a burst. `--transcribe` exercises the whole-clip API.
`--defaults` uses the API's default options, `--no-polish` disables polishing,
and `--cancel` cancels immediately after ending the recording.

Observed on macOS, 2026-11-04, using one 12.114 s synthetic Chinese/English clip,
with ten service sessions total (including the Python comparison):

| Run | First partial | Release to final | Total |
| --- | ---: | ---: | ---: |
| Real time, polish | 941 ms | 656 ms | 12,776 ms |
| 4x, polish | 965 ms | 931 ms | 3,962 ms |
| Burst, polish | 2,691 ms | 2,998 ms | 3,050 ms |
| Burst, API defaults | 1,335 ms | 1,515 ms | 1,567 ms |
| `transcribe`, API defaults | 1,225 ms | 1,634 ms | 1,681 ms |
| Real time, polish off, API defaults | 1,064 ms | 295 ms | 12,416 ms |
| Final transport `transcribe`, API defaults | 2,971 ms | 3,065 ms | 3,135 ms |

Release-to-final for a burst includes connection/key exchange and encoding;
the final-phase clock begins at upload, not at the caller's release. Every
polished result matched, including the Python worker (16.903 s total). Streaming
produced 35 partial/polishing updates. The no-polish result succeeded, cancellation
after a 4x upload returned immediately, and `check` took 640 ms. Burst is the
fastest tested upload policy and stays the default for `transcribe`.

These are compatibility measurements on one short clip, not a reliability or
long-recording benchmark. Refresh errors were induced only in offline tests.
The custom curve code has independent vectors but has not had a cryptographic
audit. No native Hex UI or microphone integration was exercised by this crate.
