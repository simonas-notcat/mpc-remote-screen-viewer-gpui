# MPC Remote Screen Viewer GPUI — implementation status

This describes the current implementation, not a broad compatibility claim.
`SPEC.md` documents client behavior and known validation gaps; `README.md`
provides build instructions and user-facing workflows.

## Implemented features

| Area | Status |
| --- | --- |
| Protocol/authentication | Bounded protocol-v2 framing and all 16 message types; PBKDF2/HMAC proofs, OS nonces and constant-time server-proof checking. Synthetic fixtures and fragmented/coalesced-message tests. |
| Session | Background socket worker, serialized writes, bounded commands, cancellable connection/retry, typed terminal failures, exponential jittered network backoff, deadlines, liveness, ACKs, options and metadata. Loopback-tested. |
| Screen | Raw/LZ4/Zstd/solid/JPEG decoding, persistent delta composition, pixel conversion/quantization, four upright rotations, safe reconfiguration, allocation and aggregate decode budgets. |
| GPUI workspace | Connection card, responsive toolbar, modal General/Audio/Streaming settings, status/notifications, fit/actual size, pan, fullscreen, native menu/dialog integration and background PNG captures. User-reported testing succeeded on macOS and Omarchy Linux; detailed V1 interaction checks remain open. |
| Remote control | Capability-gated mouse/wheel, touch-only single-contact emulation, US/Linux key mappings, modifiers, synthesized Shift, local shortcut precedence and release-all safety. Core/loopback tests plus user-reported device smoke test; broader K1 checks remain. |
| Device audio | PCM16, IMA ADPCM and predictive/Rice decoding; source-index queue/markers, cubic resampling, drift correction, prefill/fades/trims. Lazy CPAL device/rate/buffer selection, saved settings, square-law volume and peak meters. A1/A2 hardware checks remain. |
| Audio analyzer | Spectrum, 2D spectrogram, projected waterfall, peak-preserving waveform, optional stereo density cloud and source peak/RMS meters. Bounded best-effort worker and source-time histories; display settings persist. |
| Recording | Background 66 ms dirty capture, quality-80 Photo-JPEG and optional source PCM16 MOV, co64 offsets, fixed upright canvas, bounded audio queue, explicit limits/errors and finalization. Synthetic container/codec tests; R1 device/macOS checks remain. |
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
  Skipped/empty updates are ACKed without needless image conversion.
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

CI runs on Ubuntu and covers stable Rust formatting/tests/Clippy and optional
audio output, plus Rust 1.88 core tests and audio-output compilation. Desktop
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

Local Network permission denial/allow, bundle signing and universal output are
not established by the general user-reported smoke tests.
