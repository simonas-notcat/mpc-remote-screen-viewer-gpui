# MPC Remote Screen Viewer GPUI — client specification

## 1. Goal and scope

Provide a responsive Rust desktop client for viewing and controlling an MPC device,
playing and analyzing its audio, and saving upright screenshots and recordings.
The desktop UI uses **Zed's GPUI**; macOS is the initial product target.
User testing has succeeded on macOS and Omarchy Linux (Arch-based). Other Linux
distributions and the full platform acceptance matrix remain unverified.

This document describes this client's implemented behavior and validation
requirements. It is **not an official device protocol specification**. Synthetic
tests establish local consistency, not broad hardware/firmware compatibility.
Keep unverified behavior explicit; do not guess meanings for extension fields.

In scope:

- Manual hostname/IP connection, configurable port and optional password.
- Protocol-v2 authentication, screen updates, refresh, options and metadata.
- Fit/actual-size viewing, panning, fullscreen and capability-gated remote input.
- Device audio decoding, bounded playback, output selection and audio visualizers.
- Upright PNG captures and Photo-JPEG/PCM16 QuickTime MOV recording.
- Saved preferences, recent devices, connection/device information and statistics.
- Cancellable background work, bounded queues and actionable failure messages.
- Explicit opt-in read-only MCP teacher with local Force manual retrieval, device
  screenshots and viewer-only highlights; see `docs/mcp-mpc.md`.

Device discovery, cloud accounts, uploads, remote file access, microphone capture,
audio transmission, a web frontend and a device-side server are outside scope.

## 2. Architecture

| Module | Responsibility |
| --- | --- |
| `protocol.rs`, `auth.rs` | Bounded framing/parsing, message serialization and password proofs |
| `session.rs` | Socket ownership, connection lifecycle, commands, ACKs and metadata |
| `framebuffer.rs`, `viewport.rs` | Rectangle decoding/composition, upright images and viewing transforms |
| `input.rs` | Linux wire-key mapping and button/modifier/contact bookkeeping |
| `audio.rs`, `playout.rs`, `audio_output.rs` | Device decoding, bounded source-clock playback and optional CPAL output |
| `analyzer.rs`, `spectrogram.rs`, `vectorscope.rs`, `waveform.rs` | Background analysis and bounded source-time histories |
| `recording.rs` | Background capture, JPEG encoding, MOV writing and finalization |
| `statistics.rs`, `information.rs` | Per-attempt rates/history and read-only device details |
| `preferences.rs`, `preferences/viewer.rs` | Versioned JSON preferences and bounded recent-device history |
| `app.rs`, `app/`, `cli.rs`, `main.rs` | GPUI presentation/actions, launch options and application lifecycle |
| `teacher.rs`, `teacher/ipc.rs` | Observations, expiring local annotations, bounded input proposals and opt-in same-user Unix IPC (no remote lease/approval) |
| `control.rs` | Local source-bound lease/per-gesture approval, input ownership, bounded worker-driven gestures and lifecycle feedback |
| `manual.rs`, `mcp.rs`, `bin/mpc-mcp.rs` | Optional page-preserving manual index and stdio MCP adapter (feature `mcp`) |

One worker owns the TCP socket and serializes all writes with `write_all`.
Framebuffer deltas are applied in order, while presentation snapshots replace
older presentation state. ACKs must not depend on GUI paints or recording speed.
Audio output, analysis, recording, image preparation and settings writes have
separate background work; none may create an unbounded backlog.

Audio callbacks must not allocate, wait for a producer, or perform filesystem,
network or JPEG work. Playback-lock contention produces initialized silence and
increments a diagnostic counter. Normal quit drains pending preference writes
and finalizes recordings off the UI thread.

## 3. Connection lifecycle

```text
Idle -> Connecting -> WaitingForHello -> Authenticating
     -> WaitingForScreen -> Streaming
     -> RetryWait (transient network failure) or Failed (terminal failure)
```

- Trim and validate a nonempty host; accept explicit ports `1..=65535`.
- Default TCP port: **7447**. Resolve hostnames through the system resolver;
  numeric IPv4/IPv6 addresses bypass DNS.
- Default TCP connect deadline: **5 s**. Each initial handshake phase: **8 s**.
- Read/cancellation poll: **50 ms**; socket write timeout: **250 ms**.
- Ping interval: **2 s**; no-message timeout: **20 s**.
- Network retries start around **2 s**, use exponential backoff with ±25% jitter,
  and cap at **30 s**. A connection stable for 30 s resets the backoff.
- Authentication, invalid server proof, unsupported protocol, malformed required
  messages and local failures stop automatic retry. Connect retries explicitly.
- Stop cancels attempts/retry promptly and prevents stale worker publication.
- Each attempt uses fresh nonces, parser state, framebuffer identity, source-audio
  state and input bookkeeping. Queued input cannot cross configurations/sessions.
- DNS uses one bounded shared resolver worker. An OS resolver call cannot be
  cancelled, but must not block session cancellation or spawn unlimited workers.

There is **no protocol encryption**. Password authentication does not provide
confidentiality or post-login message integrity. Use a trusted LAN or external
VPN/tunnel; do not send an invented TLS handshake to the device service.

## 4. Transport and messages

A single TCP connection carries all traffic. Every message is framed as:

```text
u8 type
u32le payload_length
u8 payload[payload_length]
```

The length excludes the five-byte envelope. Accept IDs **1..16** and payloads up
to **64 MiB**. Invalid types/oversized lengths poison the assembler: terminate the
session instead of scanning for a guessed synchronization point. Buffer partial
messages in proportion to received bytes, not an advertised length.

Unless otherwise stated, integers are little-endian. A `short_string` is a
one-byte byte-length followed by up to 255 bytes, without a NUL. Optional metadata
uses bounded, lossy UTF-8 decoding. Parsers may accept trailing extension bytes;
exact-size checks are specified where required.

| ID | Message | Payload / handling |
| ---: | --- | --- |
| 1 | Hello | Required first server message; see authentication |
| 2 | Auth | Client nonce, password proof, codec mask and extension |
| 3 | AuthResult | Success proof or refusal reason |
| 4 | ServerInit | Screen configuration; may recur during streaming |
| 5 | FrameUpdate | Ordered delta rectangles |
| 6 | Ack | `u32le frame_id` |
| 7 | RequestFullRefresh | Empty |
| 8 | SetOptions | `u8 fps_limit`, `u8 codec_mask` |
| 9 | Ping | `u64le token`; respond with Pong |
| 10 | Pong | Echoed token; calculate RTT from the local monotonic clock |
| 11 | Bye | `short_string reason` |
| 12 | Input | Bounded batch of fixed-size input events |
| 13 | AudioControl | `u8 enabled`, `u8 codec_mask` |
| 14 | AudioFormat | Source rate, channel count and codec |
| 15 | AudioPacket | Source-frame index, frame count and encoded audio |
| 16 | DeviceInfo | Three short strings: model, serial, firmware |

Streaming ignores known but irrelevant incoming message types. Malformed optional
metadata/audio must not destroy an otherwise working screen session.
`SetOptions` sends codec mask `0x1f` and FPS clamped to `0..120`; zero requests the
server default. The UI offers server default, 15 FPS and 5 FPS.

## 5. Authentication

Hello contains at least 24 bytes:

```text
ASCII "RSCR" (4 bytes)
u16le protocol_version = 2
u16le flags
u8 server_nonce[16]
```

Bit `flags & 1` requires server-proof verification. Preserve other flags without
assigning unverified meanings. Reject invalid magic, short messages or versions
other than 2 with a useful diagnostic.

```text
key = PBKDF2-HMAC-SHA256(
    UTF8(password exactly as entered),
    salt = ASCII("remote-screen/v1"),
    iterations = 10000,
    output_length = 32
)
client_proof = HMAC-SHA256(key, ASCII("RSCR-client") || server_nonce || client_nonce)
server_proof = HMAC-SHA256(key, ASCII("RSCR-server") || server_nonce || client_nonce)
```

Nonces are 16 bytes each; the client nonce comes from OS randomness on every
attempt. Labels contain no NUL. Password whitespace is significant.

The client sends a 50-byte Auth payload: 16-byte client nonce, 32-byte client
proof, codec mask **`0x1f`**, extension **`0x01`**. The extension's server-side
meaning remains unverified (P1). The shared parser also accepts legacy 48/49-byte
forms, defaulting a missing mask to `0x1f` and missing extension to zero.

AuthResult success is a nonzero byte plus 32-byte proof; failure is zero plus a
short-string reason. Successful results require the proof bytes even when Hello
does not request verification. When requested, verify with constant-time equality
and refuse an incorrect proof. Require ServerInit before reporting streaming.
Manual connection permits an empty password and still performs the handshake.

Never log passwords, keys, proofs or authentication payloads. Derived keys and
CLI/session password buffers use `Zeroizing`; the UI text field does not guarantee
zeroization. Message Debug output shows only type and payload length.
Synthetic authentication examples are in `tests/fixtures/protocol-v2-vectors.json`.

## 6. Screen updates and viewing

### Configuration

```text
u16le native_width
u16le native_height
u8 pixel_format[7]
short_string screen_name
u8 rotation
u8 input_capabilities
[u8 audio_available]          # absent means false
```

Dimensions must be nonzero. PixelFormat is `[bpp, red_shift, red_bits, green_shift,
green_bits, blue_shift, blue_bits]`; bpp is 16/24/32, each channel has 1..8 bits,
and each shift+bits fits within bpp. Rotation is 0..3 clockwise quarter turns.
Input bits are touch `0x01`, mouse/wheel `0x02`, keyboard `0x04`.

A new configuration replaces/clears the framebuffer and resets dependent input
and audio state. The client caps decoded RGBA allocation at **64 MiB**.

### FrameUpdate

```text
u32le frame_id
u8 quality                    # zero is best
u16le rectangle_count
repeat rectangle_count times:
    u16le x, y, width, height
    u8 codec
    u8 quantization_drop_bits
    u32le encoded_byte_count
    u8 encoded_bytes[encoded_byte_count]
```

Coordinates address the native, unrotated framebuffer. Check all lengths/products
before reading/allocating. Apply in message order so later rectangles overwrite
earlier overlaps. Preserve pixels outside updates. Skip empty/out-of-bounds or
individually undecodable rectangles; structural frame corruption terminates.
ACK after processing, including empty/skipped frames.

The default aggregate decode/composite budget is **64 Mi rectangle pixels** per
update, including failed/overlapping attempts. Retain the valid prefix and skip
the remaining rectangles once exhausted. Library callers can configure the budget.

| Codec | Encoding | Validation |
| ---: | --- | --- |
| 0 | Raw | Exactly `width * height * bytes_per_pixel` |
| 1 | Raw LZ4 block | Not an LZ4 frame/container; exact decoded length |
| 2 | Zstandard | Bounded output/window and exact decoded length |
| 3 | Solid | Exactly one packed pixel |
| 4 | JPEG | Matching dimensions; standard 32-bit RGB/BGR layouts only |

Pixels are tightly packed and little-endian. Quantization expands each channel
with a single OR: for `drop = min(drop_bits, channel_bits - 1)`, OR
`value >> (channel_bits - drop)` into that channel when drop is nonzero.
For example, 8-bit value 128 with drop 7 becomes **192**, not 255.
Then expand sub-eight-bit channels by bit repetition and produce opaque RGBA.

### Viewing and capture orientation

Odd rotations swap upright width/height. Viewing, PNG and MOV use the same upright
orientation. Fit mode centers an aspect-preserving image; actual size uses one
image pixel per logical UI unit, with pan clamps on overflowing axes. Retina
behavior lacks detailed hands-on validation (V1). With control disabled, drag/scroll pans and
double-click toggles fit/actual size; Shift-scroll pans horizontally.

## 7. Remote input

Input is `u8 event_count` followed by up to 64 events. Each is exactly eight bytes:

```text
u8 kind, id
u16le x_or_key
u16le y
i16le value
```

| Kind | Meaning |
| ---: | --- |
| 0 / 1 / 2 | Touch down / move / up; contact ID 0..9 |
| 3 | Mouse move |
| 4 / 5 | Mouse button down / up; left/right/middle IDs 0/1/2 |
| 6 | Mouse wheel; signed value |
| 7 | Release all; remaining fields zero |
| 8 / 9 | Key down / up; Linux input-event keycode in `x_or_key` |

Split longer batches rather than truncating transitions. Map pointer positions
into **upright display-oriented device coordinates**, not inverse-rotated native
coordinates. Reject initial presses outside the image; clamp valid drags/releases
to image edges. Suppress duplicate integer-coordinate moves. Touch-only devices
currently use single-contact mouse emulation, not native multitouch.

Keys use Linux input-event codes, not Unicode or GUI enum values. US-layout
letters, digits, punctuation, navigation, F1..F10 and explicit keypad identities
are mapped in `input.rs`. Modifier codes are Shift 42, Control 29, Alt 56, Meta 125.
Recognized keypresses send down/up pairs; synthesized Shift must not release a
physically held Shift. Platform-normalized keypad events cannot recover physical
keypad identity from text alone. Non-US layouts and Option/repeat remain unverified (K1).

Control defaults off, restores the saved preference, and remains gated by the
handshake, device capabilities and focus. F12 toggles available control locally;
F11 toggles fullscreen; Command-comma opens Settings. Escape exits fullscreen
before forwarding. Ordinary letter shortcuts are suppressed while editing fields
or forwarding device keyboard input. Focus loss, dialogs, disable, configuration
changes and disconnect release held input and clear local bookkeeping. Queue
overload stops the session rather than silently losing a release transition.

### Assisted MCP input (phase 2)

The bridge remains read-only by default. A **local-only 60-second lease** and
separate local approval of each exact immutable proposal are required. IPC/MCP
cannot grant/approve; unknown fields such as `confirmed` are rejected. All gestures
are potentially destructive, not semantically classified as safe. Proposals expire
in 20 seconds; worker execution is capped at 5 seconds and lease expiry. Click holds
are 50–2000 ms; straight drags 100–3000 ms / at most 61 steps; scroll is nonzero
−120…120 wire units; text is 1–64 printable US ASCII characters. Keyboard/touch/
mouse capabilities are checked before construction and worker execution.

Each proposal references a valid observation and maps returned-image crop/resize
coordinates into upright input coordinates. Pre-proposal and in-gesture observations
cannot authorize later input; obtain a fresh screen after each terminal result.
Only one proposal/gesture exists at a time. The session worker schedules input,
records actual socket-written steps, and releases on completion/cancel. Input ACKs
and visual verification do not exist. Partial writes are uncertain and never
retried; broken connections cannot guarantee release. No blind sequences.

Human/agent arbitration covers the whole held gesture and in-flight local writes.
Viewer input revokes/cancels agent ownership and writes release before local input.
Sharing revocation, local Stop/F12, window deactivation, opening Settings, lease
expiry, disconnect/reconnect and reconfiguration revoke assisted control. Approval/reject/
Stop remain available in fullscreen; painting never intercepts input. Local feedback
shows pending/queued/running/completed/rejected/cancelled/uncertain states, actual
held/cursor/path/release, signed wheel amount and non-sensitive key/text labels.
It is excluded from captures/recordings. Physical device takeover is not detectable.
See `docs/mcp-mpc.md` for tool contracts and remaining live acceptance checks.

## 8. Device audio and analysis

AudioControl is two bytes: enabled and codec mask **`0x07`**. Do not send it unless
the device advertises audio. Playback and analysis may independently request the
source; analysis-only use must not require an output device.

AudioFormat is exactly `u32le sample_rate, u8 channels, u8 codec`: rates
**8000..384000**, channels **1..8**, codecs **0..2**. AudioPacket starts with
`u64le first_sample_index, u16le frame_count`, then encoded bytes. Frame count is
**1..8192 per channel**; source indices are sample-frame positions, not timestamps.
Format changes reset decoder/playback state. Decoded samples are interleaved f32,
normalized from signed PCM16 by division by 32768.

- **Codec 0 — PCM16:** exactly `2 * frames * channels` little-endian bytes.
- **Codec 1 — IMA ADPCM:** per-channel `i16 predictor, u8 step_index, u8 reserved`
  headers, followed by interleaved nibbles, low nibble first. Initial predictors
  form the first frame. Step indices are 0..88; packet states are independent.
  Standard step/index test tables live in `tests/fixtures/ima-adpcm-tables.json`.
- **Codec 2 — predictive/Rice:** channel-major blocks with `u8 control`,
  `u16le byte_length` and block bytes. Control `0x80` carries raw PCM16 for that
  channel; other bit-7 values and bit 6 are rejected. Compressed order is
  `(control >> 4) & 3`, k is `control & 15`, and order must be below frame count.
  Seed with `order` PCM16 samples, then read LSB-first Rice residuals. A run of
  24 one bits escapes to a 21-bit unsigned residual without an extra terminator.
  Zigzag-decode residuals and add order-0..3 finite-difference prediction. Reject
  exhaustion, overflow, out-of-i16 results and unused whole bytes. All channel
  blocks must consume the packet. Device-generated edge cases remain unverified (A1).

Playback uses a power-of-two source-frame ring, at most **16 MiB**, and 64 fixed
discontinuity markers. Trim duplicate/overlapping source positions; mark gaps.
Ring/marker overload drops packets and marks the next accepted discontinuity.
Defaults are **80 ms target**, **300 ms maximum-latency setting**, and **5 ms fades**.
The target is adjustable to **10..300 ms** without reopening the output stream.
Prefill, startup/boundary/tail fades, excessive-latency trim and refill after
underflow keep memory bounded and transitions explicit.

Four-point cubic resampling accounts for source/output rate mismatch. Smoothed
buffer-error correction adjusts the nominal ratio by up to ±0.4%. Mono duplicates
to output channels; mono output averages source channels; otherwise route matching
channels and leave extra outputs silent. Volume is the square of the slider value.
Input/output interval peaks use a stable -120 dBFS silence floor.

CPAL output is lazy. Missing saved devices fall back to system output; unsupported
rate/buffer settings warn and use supported defaults. Explicit buffer choices
start at **64 frames**; retired 16/32-frame preferences load as Default. Driver
failure must leave screen viewing available. No microphone is opened.

The analyzer offers a 2048-sample Hann FFT/50% overlap spectrum, ten-second spectral
history, projected waterfall, one-second peak-preserving waveform and optional
200 ms stereo density cloud. A bounded best-effort worker uses source-sample time,
marks gaps and keeps FFT/image work out of audio callbacks. Only analyzer display
settings persist, never source audio or analysis history. See README for display
ranges, queue limits and usage. Hardware latency, hot-plug and long drift runs
remain A2 checks; synthetic tests are not audible validation.

## 9. UI, preferences and statistics

Use a centered connection card while disconnected and a responsive single toolbar
while connected. Provide labeled address/password/port fields, masked password
with show/hide, recent devices, clear attempt status and cancellation. Hide
editable credentials during an attempt/session. Capture actions move into overflow
at small widths instead of wrapping. Settings has General, Audio, Streaming and
AI Teacher tabs; its tab buttons wrap in narrow dialogs. Opening Settings releases
remote input and revokes assisted control; it confines focus and closes with Escape.
The AI Teacher tab explicitly enables sharing and grants/revokes a timed lease via
local switches, never saved or auto-renewed. Per-gesture approval is still required
after closing Settings; an active lease can always be switched off.

Preferences are app-scoped JSON under `mpc-remote-screen-viewer-rs/` in macOS
Application Support or Linux `$XDG_CONFIG_HOME` (fallback `~/.config`). This legacy
directory is retained across the project rename to preserve existing settings:

- `viewer-settings.json`: version 1, host/port, fit/actual, stats/port visibility,
  control, FPS, analyzer visibility/mode/height/stereo toggle and recent devices.
- `screenshot-settings.json`: default screenshot folder.
- `audio-settings.json`: playback preference, device/rate/buffer, volume and target
  buffering; older files default the target to 80 ms.

Writes are atomic; a coalescing worker saves the latest viewer state and drains
on quit. Viewer reads are capped at **512 KiB**. Missing fields default; malformed,
unknown/unsupported or invalid settings report errors without rewriting on load.
No passwords, held input state or analysis data are saved.

Keep at most **10** successfully connected endpoints, newest success first.
Deduplicate hostnames case-insensitively and equivalent numeric IPs; ports remain
separate. Late metadata enriches rows without erasing known labels. Selecting a
row fills host/port, clears the password and does **not** connect; selection is
disabled during connection/retry. Clearing recents retains the last address.

Device Info displays connection/protocol, optional model/serial/firmware, native
and upright dimensions, rotation/pixel masks, input/audio capabilities and network
and audio diagnostics. Unknown values are explicit; protocol version appears only
after valid Hello.

FPS counts processed FrameUpdates, not paints. Receive bandwidth includes complete
message payloads plus five envelope bytes, not TCP/IP overhead. Sample on the
network worker at intervals of at least one actual second; retain **120** intervals.
Delayed work creates a longer interval, not backfill. Disconnect clears live rates;
retry retains counters/history until the next attempt resets them. Nothing in
network history is saved.

## 10. Captures and recording

PNG captures include full upright device pixels, not window chrome or the cropped
viewport. Toolbar/P asks for a default folder on first use (or when missing), then
uses noncolliding filenames. Settings can change the folder. File → Save Screenshot
uses a per-file chooser and refuses overwrite. Encode/write in the background;
report success path or actual error. Success notices expire after three seconds;
errors remain until replaced/dismissed.

Recording is available only with a screen. Choose a new MOV path, never overwrite,
and show elapsed time/Stop until background finalization completes. Disconnect
and normal quit finalize too. Output policies:

- QuickTime MOV (`qt  `), Photo-JPEG (`jpeg`) video at quality **80**, optional
  decoded-source PCM16 (`sowt`) audio before playback volume. Source audio is
  requested by playback or an open analyzer; recording itself does not enable it.
- Dirty-image capture on a **66 ms** timer; no full-image backlog. Empty/skipped
  updates do not create samples. The initial fixed upright canvas letterboxes
  later size changes. First video starts at zero; last image holds through Stop.
- Movie/video timescale **1000**; audio timescale is the source rate. Use `co64`
  offsets and version-2 audio descriptions for rates above 65535 Hz.
- A **128-packet** nonblocking audio queue; overflow is an explicit error. Trim
  duplicate/overlapping source samples, insert bounded silence for gaps, and
  re-anchor toggle/reset epochs to elapsed time. Rate/channel changes fail and
  attempt finalization; codec-only changes continue.
- Cap audio gaps at **60 s**, metadata at **one million chunks**, and track sample
  counts at `u32::MAX`. Report limits instead of unbounded growth.
- Finalize/sync before reporting Saved. Retain files on failure and report partial
  paths; a disk-write failure may leave an unplayable movie. No crash recovery.

FFmpeg is not a runtime dependency. Tests may use installed FFprobe/FFmpeg as
optional independent container/decoder validators. QuickTime/macOS playback,
real-device A/V sync and failure recovery remain R1 validation items.

## 11. Launch and packaging

Supported inputs: `--host`, `--port`, `--password`, `--audio`, `--teacher`, `--help`/`-h`, and
`REMOTE_SCREEN_PASSWORD`. Saved values are defaults; explicit CLI address options
win. `--password` overrides `REMOTE_SCREEN_PASSWORD`. A restored host never
auto-connects; auto-connect requires explicit nonempty `--host` and nonempty
password. Help exits without loading settings. Prefer masked UI/environment input
over CLI passwords, which can appear in process listings/history.

`--teacher` explicitly enables the read-only local teacher bridge; this opt-in is
never persisted. The View menu and a visible indicator (also in fullscreen) can
disable sharing. The separate optional `mpc-mcp` executable uses stdio and requires
an approved Force v3.10.1 PDF via `--manual`; `--socket`/`MPC_TEACHER_SOCKET` select
local IPC only. Bounded phase-2 input proposals require a local lease and local
approval for every gesture; no audio tools exist. Highlights and lifecycle feedback
reference upright device pixels and are local paint, not framebuffer modifications.
Full security/tool/gesture contracts and later phases are in `docs/mcp-mpc.md`.

`scripts/bundle-macos.sh` creates a development `.app` with its own bundle identity,
Local Network usage explanation and ad-hoc signing. `--universal` builds Apple
Silicon and Intel when both Rust targets are installed. Full Xcode is needed for
GPUI's Metal build. Bundle Local Network permission denial/allow, signing, Retina,
native menus/dialogs and universal output lack targeted validation; the successful
macOS user test does not establish coverage of these checks.

## 12. Validation and known gaps

```sh
python3 scripts/check-fixtures.py
cargo fmt --all -- --check
cargo test --locked --no-default-features
cargo test --locked --features audio-output
cargo test --locked --features mcp
cargo test --locked --features desktop,mcp --all-targets
cargo clippy --locked --features desktop,mcp --all-targets -- -D warnings
```

Core tests use synthetic fixtures and their own loopback mock peers. Never point
automated tests at real hardware without authorization. `examples/mock_server.rs`
provides an explicit local preview and optional quiet PCM16 tone.

Automated coverage includes fragmented/coalesced envelopes, crypto proofs and
negative authentication, optional extensions, malformed lengths, all screen codecs,
rotations/quantization, ACKs independent of paints, reconnect/cancel/deadlines,
input mapping/release safety, audio codec edges, bounded queues/drift/allocation-free
rendering, preference validation/atomic writes, PNG/MOV content/timing/errors,
and teacher IPC/observation bounds, manual page retrieval, native MCP images,
synthetic stdio tool dispatch, locally approved assisted input, takeover/release
ordering, stale-evidence/deadline rejection and partial-write uncertainty.

| Check | Current validation gap |
| --- | --- |
| P1 | Server-side meanings of Auth `0x01` and unassigned Hello flags are unverified. |
| P2 | Real-device model/firmware/network/password/input/audio evidence is limited to user-reported smoke tests. |
| K1 | GUI/device layouts, keypad identity, Option/repeat, trackpad and focus/release cases lack comprehensive hardware coverage. |
| A1 | Predictive/Rice escape and predictor-order edge cases lack device-generated packet validation. |
| A2 | CoreAudio devices/hot-plug, audible fades/latency, multichannel routing and ≥30-minute drift remain unverified. |
| R1 | QuickTime playback, real-device A/V timing, long movies and failure behavior lack hardware validation. |
| V1 | Detailed macOS/Retina layout, scaling, gestures, minimum sizes, native menus/dialogs and capture UI remain unverified. |
| C1 | Preference restoration, recent selection, startup precedence and quit/error paths lack detailed macOS interaction validation. |
| S1 | Inspector/graphs/scrolling/input occlusion lack detailed macOS validation. |
| T1 | Basic live Pi teaching and user-confirmed highlight visibility succeeded. Fullscreen/revocation/reconfiguration and macOS teacher IPC still need hands-on tests; absent device model/firmware prevents exact manual compatibility confirmation. Playback captures can return `Framebuffer busy; retry`; bounded capture backoff is not automatic. |
| T2 | Phase-2 assisted control has synthetic/loopback and desktop compile/lint coverage only. Actual Force effects and live approval/Stop/fullscreen/fit/pan/rotation rendering remain unverified; phase-1 teaching evidence is not phase-2 acceptance. |

User-reported testing has succeeded on macOS and Omarchy Linux. Earlier
connection, remote-control and screenshot-folder smoke tests also succeeded.
Device model, firmware, OS versions and per-platform feature coverage were not recorded.
Keep these reports separate from automated results and independently observed
hardware validation. A subsequent live Pi session exercised read-only teaching
through a complete one-bar drum pattern, with note positions verified from
screenshots and drum voices/overlay visibility confirmed by the user. All device
input was performed by the user; no agent audio analysis occurred. See
`docs/mcp-mpc.md` for this
session's evidence and limitations, and `IMPLEMENTATION.md` for development status.
