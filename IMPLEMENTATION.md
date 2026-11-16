# MPC Remote Screen Viewer GPUI — implementation status

This describes the current implementation, not a broad compatibility claim.
`SPEC.md` documents client behavior and known validation gaps; `README.md`
provides build instructions and user-facing workflows.

## Implemented features

| Area | Status |
| --- | --- |
| Protocol/authentication | Bounded protocol-v2 framing and all 16 message types; PBKDF2/HMAC proofs, OS nonces and constant-time server-proof checking. Synthetic fixtures and fragmented/coalesced-message tests. |
| Session | Background socket owner plus a one-job pixel decoder; serialized writes and responsive input/release during slow decoding or reader contention. Bounded commands, cancellable connection/retry, typed failures, backoff, deadlines, liveness, ordered ACKs, options and metadata. Loopback-tested. |
| Screen | Raw/LZ4/Zstd/solid/JPEG decoding, persistent delta composition, pixel conversion/quantization, four upright rotations, safe reconfiguration, allocation and aggregate decode budgets. |
| GPUI workspace | Connection card, responsive toolbar, modal General/Audio/Streaming settings, status/notifications, fit/actual size, pan, fullscreen, native menu/dialog integration and background PNG captures. User-reported testing succeeded on macOS and Omarchy Linux; detailed V1 interaction checks remain open. |
| Remote control | Capability-gated mouse/wheel, touch-only single-contact emulation, US/Linux key mappings, modifiers, synthesized Shift, local shortcut precedence and release-all safety. Core/loopback tests plus user-reported device smoke test; broader K1 checks remain. |
| Device audio | PCM16, IMA ADPCM and predictive/Rice decoding; source-index queue/markers, cubic resampling, drift correction, prefill/fades/trims. Lazy CPAL device/rate/buffer selection, saved settings, square-law volume and peak meters. A1/A2 hardware checks remain. |
| Audio analyzer | Spectrum, 2D spectrogram, projected waterfall, peak-preserving waveform, optional stereo density cloud and source peak/RMS meters. Bounded best-effort worker and source-time histories; display settings persist. |
| Recording | Background 66 ms dirty capture, quality-80 Photo-JPEG and optional source PCM16 MOV, co64 offsets, fixed upright canvas, bounded audio queue, explicit limits/errors and finalization. Synthetic container/codec tests; R1 device/macOS checks remain. |
| MCP teacher (phases 1–2) | Optional rmcp stdio executable; Force v3.10.1 manual, private opt-in observations/highlights, plus bounded assisted gestures authorized by one global main-menu toggle (no timer, focus reset or per-gesture approval). Worker-owned execution/release and local feedback; synthetic/loopback tests. Phase-1 live teaching succeeded; phase-2 GUI/Force effects remain unverified. No audio tools. See `docs/mcp-mpc.md`. |
| Preferences | Atomic app-scoped viewer/screenshot/audio JSON; bounded recent-device picker, default screenshot folder, control/FPS restoration and analyzer display state. Passwords and held input are never saved. |
| Information/statistics | Read-only Device Info, live FPS/bandwidth and 120-interval network-rate graphs with core/loopback tests. |
| Packaging | macOS plist and native/universal development-bundle script. User testing on macOS succeeded; bundle creation, permissions, signing and universal output remain unverified. |

## Architecture and boundedness

- One session worker owns the socket and all writes. Complete messages are consumed
  before another bounded read. The incremental assembler allocates only for bytes
  received, rejects payloads above 64 MiB and never attempts resynchronization.
- Connection cancellation polls at 50 ms. One bounded shared DNS worker avoids
  uncancellable resolver calls creating unlimited threads. Network retry caps at
  30 s; authentication/protocol/local failures remain visible for explicit retry.
- Framebuffers cap decoded allocation at 64 MiB. Decode/composite work defaults
  to a per-update budget of 64 Mi rectangle pixels, including failed attempts.
  One decoder helper accepts only one in-flight payload, moved without copying;
  deltas are applied in order and ACKed only after completion, including skipped/
  empty updates. While waiting, the socket owner services input/Stop/deadlines at
  50 ms intervals. Human takeover is admitted before advancing agent input. On
  teardown, socket release precedes joining even a non-interruptible codec call.
- Presentation keeps the latest image/state rather than queued repaint jobs.
  Framebuffer identity/content revision and session generation guards reject stale
  images. Old GPU images are evicted; RGBA-to-GPUI-BGRA preparation is off-thread.
- Input commands carry the target framebuffer identity. Queue overload stops the
  session instead of dropping a release. Focus loss, dialogs, disable, shutdown
  and reconfiguration clear held input; outside drag/releases clamp to the image.
- Playback uses a power-of-two ring (at most 16 MiB) and 64 discontinuity markers.
  Ring/marker overload drops a packet and marks the next accepted gap. Source
  indices deduplicate overlaps; resets use format epochs even for equal formats.
  The callback uses try-lock/silence, performs no allocation and records contention.
- Target buffering is 10–300 ms (default 80 ms), with fades and live trim/prefill
  adjustments without device reopen. Explicit output buffers start at 64 frames;
  retired 16/32-frame selections migrate to Default without losing other settings.
- CPAL output enumeration, stream lifecycle and serialized settings writes stay
  on one worker. Unavailable devices/settings fall back with warnings. Audio
  errors do not stop screen viewing, and no microphone is opened. Shutdown joins
  off the UI thread and drains pending saves, including changes during a save.
- Analyzer input is bounded to 500 ms / 256 packets, with space for one larger
  protocol-sized packet. FFT buffers/mappings are reused. Spectral history merges
  to at most 30 columns per source second; waveform and stereo histories are
  bounded. Gaps remain blank instead of fabricated continuity. Hidden views avoid
  unnecessary image uploads; playback callbacks never perform analysis work.
- Recording has no queued full-image backlog. JPEG/mux/disk work is off-thread;
  audio arrives before output gain through a 128-packet nonblocking queue, without
  holding the playback lock. Playback or an open analyzer requests the source;
  recording itself does not enable device audio. Limits are one million chunks,
  60 s audio gaps and u32 track sample counts. Errors retain partial paths and attempt finalization.
  Normal Stop/disconnect/quit drains accepted work before reporting success.
- Preferences are atomically replaced. Viewer reads cap at 512 KiB, missing fields
  default, invalid/unsupported schemas report errors, recents deduplicate/truncate
  to ten. A coalescing writer retains the latest state and drains on normal quit.
- Network rates are worker-sampled, independent of paints. Retain 120 intervals
  of at least one actual second; delays create longer samples without backfill.
  Count all complete message envelopes/payloads; idle streaming samples zero.
  Retry retains history until the next attempt resets it.

## MCP teacher (read-only default, assisted phase 2)

The viewer explicitly enables a same-user private Unix socket (`--teacher` or View
menu), with a visible revoke button including fullscreen. One bounded IPC worker
reads the existing session snapshot. It can queue/cancel bounded assisted gestures,
but never holds a session command sender or an IPC capability to enable assistance.
Observations retain at most 16 weak framebuffer references for 60 seconds. PNGs
are at most 2048 pixels per edge; live framebuffer locks cover only a bounded copy,
not rotation/PNG encoding. Local teaching rectangles map crop/resize coordinates
back to upright device pixels and are painted in the existing canvas, never into
captures/recordings. They clear on expiry, disconnection/reconfiguration or revocation.

The optional MCP executable has no GPUI/output-device dependency. Poppler import
keeps page-separated text and a bounded lexical passage index in memory, retaining
citations, printed labels and a PDF fingerprint. Optional page rendering and import
have subprocess deadlines/output limits. Manual tools work without the viewer.
MCP advertises twelve tools. Overlay writes are local non-destructive mutations;
input tools are honestly annotated as potentially destructive, non-idempotent
mutations. **Allow assistance** in the main menu is global approval and enables
sharing if needed. It has no permission timer, focus reset, Settings switch or
per-gesture approval UI. The viewer keeps the toggle on across focus/Settings,
local takeover and connection/source changes until off, sharing disable or shutdown;
it is not persisted. Only local UI can enable it; MCP cannot enable permission.

The GUI-independent `control.rs` layer shares `input.rs` wire construction. The
session worker alone executes gestures, records socket-written steps and releases
on completion/cancel. Queue and execution deadlines are 5 seconds, capped by the
referenced observation's expiry; these bound a gesture, not global authorization.
Failed writes are uncertain and never replayed. Local input cancels/releases agent
ownership first; held/in-flight local inputs block new gestures without resetting
the toggle. Source changes cancel old work and invalidate observations, not global
permission. All pre-action/in-action observations are consumed for subsequent input.
The normal/fullscreen panel provides **Cancel gesture** and **Stop assistance**;
the latter switches global authorization off. This safety row reserves space below
the workspace, outside the analyzer and notification layers. Non-interactive cursor,
held ring, actual drag path, signed scroll feedback and hidden-content key/text labels remain
outside captures/recordings and below modal UI.

Synthetic stdio tests cover MCP initialization/tool dispatch, manual search/read,
page PNGs, rotated/downscaled screen captures, highlights/clear, stale-source
rejection and disabled-input rejection. IPC/unit tests cover permission/framing
bounds, active/stale sockets, observation TTL, fit/pan transforms, overlay-free
captures, authorization/deadlines, consumed observations and failed-write uncertainty.
Loopback session tests cover all five gestures under one global toggle without
additional approval, off/cancel release, framebuffer contention, local takeover,
permission surviving source changes, and no old-work replay after reconfiguration.
Regression tests include a single large LZ4 rectangle, many expensive rectangles,
release/observation deadlines during decoding, release before joining a slow codec
on session shutdown, human ownership while readers block pixels, and cancellation
of queued agent work before its first write. Decoder
unit tests enforce one in-flight job, result/error propagation and reader-safe shutdown.
Tests never connect to real hardware. Socket-written is not device-acknowledged or
visually verified; broader phase-2 live rendering/input acceptance remains open.
Two separately authorized live MPC menu-navigation clicks were visually verified;
see `docs/mcp-mpc.md` for the limited evidence.

A subsequent live Pi session exercised all six tools against the running viewer,
including a manual page image and user-confirmed on-screen guidance. The user
completed a one-bar 128 BPM drum pattern with DrumSynth:Multi / Deep House, while
the agent checked successive screenshots. Pad identities were established by the
user's audition reports, not agent audio analysis or an assumed universal mapping.
The model/firmware fields were absent; exact guide compatibility remains unverified.
See `docs/mcp-mpc.md` for the pattern, manual citations and evidence boundaries.

During playback some captures returned `Framebuffer busy; retry`; later read
attempts succeeded without changing device settings. Capture backoff/waiting is
not automatic. Live fullscreen, revocation/reconfiguration and macOS IPC behavior
still need targeted hands-on validation.

## Test coverage and evidence

The automated suite covers:

- Synthetic messages for all 16 IDs, envelope fragmentation/coalescing, password
  proofs, optional extensions, malformed lengths and bounded parser allocation.
- Loopback authentication failures, proof flags, reconnect/new nonces, deadlines,
  cancellation, ACKs/options/ping, audio negotiation/toggle/capability and corrupt
  optional messages. Mock peers reject unrelated eager local HTTP probes.
- Screen codecs, packed pixels, quantization, delta overlap, rotation, skipped
  rectangles, image budgets and upright capture content.
- Input serialization, Linux mappings, modifier/Shift transitions, batch splitting,
  release safety and stale-command protection. Tests do not replace GUI/layout runs.
- PCM/ADPCM/predictive decoding, Rice parameters/orders/escape, source overlap/gaps,
  ring/marker bounds, rate conversion, injected ±1000 ppm drift, underflow/refill,
  gain/peaks, callback contention and allocation-free rendering.
- Analyzer tone/amplitude/channel behavior, source-time histories, discontinuities,
  delivery pauses, peak-preserving waveform and vectorscope density/gain behavior.
- Preference defaults, schema/size validation, atomic replacement, recents policies,
  CLI precedence, no restored-host auto-connect, serialized/coalesced saving and
  quit draining. Output fallback tests do not open an audio driver.
- MOV sample tables/timing, independently decoded JPEG dimensions/letterboxing,
  audio overlap/gaps/resets, pre-gain taps, static tails, disconnect finalization,
  format-change errors, queue bounds, overwrite refusal and offsets above 4 GiB.
  Optional FFprobe/FFmpeg checks inspect/decode generated movies, including high
  source rates. They are test validators, not runtime dependencies.
- Desktop presentation helpers, embedded asset lookup and settings/layout decisions;
  these checks do not measure actual GPU layout, focus or pointer interaction.

`scripts/check-fixtures.py` checks synthetic framing/authentication/expected values
and standard IMA ADPCM tables without hardware or networking. Rust tests consume
`tests/fixtures/` directly. Core tests use only their own loopback peers. The manual
mock optionally supplies a quiet PCM16 tone; it is not real-device validation.

CI runs on Ubuntu and covers stable Rust formatting/tests/Clippy, optional
audio output and an MCP job with Poppler for the synthetic stdio/PDF smoke test.
Rust 1.88 checks cover core tests plus audio-output and MCP compilation. Desktop
builds are not covered by CI. Linux/aarch64 desktop builds, Clippy and help smoke
checks have been performed during development; they do not establish
macOS support or hands-on UI correctness. Bundle plist/script syntax checks do
not establish successful bundle creation/signing. GPUI's transitive
`proc-macro-error2 2.0.1` currently emits a future-incompatibility warning.

## User-reported validation

The user reported successful testing on **macOS** and **Omarchy Linux** (Arch-based).
Earlier reports confirmed real-device connection, remote control and a working
saved screenshot-folder workflow. These are user-reported smoke tests, not
independently observed acceptance runs. Device model, firmware, OS versions and
per-platform layout/keypad, safety and feature coverage were not recorded.

The later MCP session separately provides agent-observed screenshots/manual reads
and a user-confirmed visible highlight and drum-voice identification. It validates
the read-only teaching loop, not autonomous input, acoustic quality, saved-project
persistence, or comprehensive platform/firmware compatibility.

## Known limitations and validation gaps

| Check | Current limitation or validation gap |
| --- | --- |
| P1 | Server-side meanings of Auth extension `0x01` and unknown Hello flags are unverified; bytes are preserved without invented semantics. |
| P2 | Device/firmware/network/password/input/audio evidence is limited to user-reported smoke tests. |
| K1 | macOS/device keyboard layouts, keypad identity, Option/repeat, trackpad, focus loss and outside releases lack comprehensive hardware coverage. GPUI normalization can lose keypad identity; native multitouch is not supported. |
| A1 | Codec-2 escape and predictor-order edge cases lack device-generated packet validation. Synthetic derivation tests alone are insufficient. |
| A2 | Audible/CoreAudio/hot-plug, latency, multichannel-routing and ≥30-minute drift remain unverified. Output devices with identical CPAL display names cannot currently be distinguished. |
| R1 | QuickTime playback, device A/V sync, toggle/format-change timing, long recordings and write/finalization failures lack hardware validation. No crash recovery; failed disk writes may leave unplayable partial files. |
| V1 | Detailed Mac/Retina rendering, minimum-size/compact toolbar, native menus/dialogs, scaling/panning/fullscreen and capture indicators remain unverified. |
| C1 | Settings restoration, recent selection/cleared passwords, CLI precedence, dialog focus and quit/error handling lack detailed macOS interaction validation. |
| S1 | Inspector scrolling/occlusion and graphs lack detailed macOS validation. |
| T1 | Basic live Pi teaching/highlight visibility succeeded; fullscreen/revocation/reconfiguration and macOS teacher IPC remain unverified. Device model/firmware was not supplied. Playback capture can return a transient framebuffer-busy error; automatic bounded capture backoff is not implemented. |

Local Network permission denial/allow, bundle signing and universal output are
not established by the general user-reported smoke tests.
