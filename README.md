# MPC Remote Screen Viewer GPUI

<img width="1894" height="2084" alt="screenshot-2026-11-14_18-50-53-mpc-remote-screen-viewer" src="https://github.com/user-attachments/assets/b8954496-f485-4de4-b0cf-87e2687a3008" />

https://github.com/user-attachments/assets/9c56c9e5-3571-4472-ab2e-afee434daf87

`mpc-remote-screen-viewer-gpui` is a Rust desktop client for MPC remote screen
viewing, control, device audio and recording. The UI uses **Zed's GPUI**.
See [`SPEC.md`](SPEC.md) for client behavior and validation requirements.
Synthetic test data lives in `tests/fixtures/`.

## Implemented features

- Bounded, incremental protocol-v2 framing and parsers for all 16 message types.
- PBKDF2/HMAC authentication, fresh OS nonces, constant-time server-proof checking.
- Background TCP session, interruptible connection/retry, deadlines, ping/pong,
  options, full refresh, frame ACKs, metadata, reconfiguration and byte counters.
- Raw, raw-block LZ4, Zstd, solid and JPEG rectangle decoding; delta composition,
  pixel conversion, quantization reconstruction and all four upright rotations.
- GPUI connection UI with masked password, configurable port, status, fit/actual
  size, drag/wheel panning, fullscreen, native menus, and background PNG capture.
- Capability-gated remote control (F12): mouse/wheel, keyboard/modifiers,
  single-contact touch emulation, and release-all safety.
- Device Info panel, live FPS/bandwidth and bounded network-rate history graphs.
- Saved viewer settings and a bounded recent-device picker; passwords are never saved.
- Saved default screenshot folder with first-use folder selection.
- Optional stdio MCP Force teacher (read-only by default): local manual search/page images,
  device observations, viewer-only highlights and locally approved assisted gestures.
- Device audio: PCM16, IMA ADPCM and predictive/Rice decoding, bounded
  playback, cubic rate conversion, drift correction and discontinuity fades.
- Lazy output-device/rate/buffer selection, saved audio preferences, square-law
  volume and input/output peak meters. Audio failures do not stop screen viewing.
- Background MOV recording: Photo-JPEG video and optional decoded-device PCM16 audio.
- macOS bundle metadata and native/universal development-bundle script.

Input layout/keypad and native multitouch limitations remain documented. Recording
validation R1, audio codec-2 and audible/hardware checks A1/A2 remain open. See
[`IMPLEMENTATION.md`](IMPLEMENTATION.md) for current behavior and validation gaps.

User-reported **testing succeeded on macOS and Omarchy Linux**. Earlier
real-device connection, remote-control and screenshot-folder tests also succeeded.
This is not the complete hardware/firmware or platform acceptance matrix.

## Build and test

Use current stable Rust. Core tests need no GUI, audio hardware or device.
All network tests connect **only to their own loopback mock peers**.

```sh
python3 scripts/check-fixtures.py
cargo test --locked --no-default-features
cargo clippy --locked --features desktop --all-targets -- -D warnings
```

GPUI is pinned to Zed's published `gpui 0.2.2` API. `gpui-component 0.5.1` provides
text editing/IME/selection and ordinary controls on that same GPUI release.
These are pre-1.0 APIs; upgrading should be deliberate. `Cargo.lock` is checked in.
The `libc` pin preserves an alias needed by GPUI's older transitive `xattr` on
Linux. Desktop/audio-output builds on Linux also need ALSA development headers
(`libasound2-dev` on Debian/Ubuntu; `alsa-lib` on Arch) and `pkg-config`.
The desktop app has been successfully tested by the user on **Omarchy Linux**
(Arch-based). Other Linux distributions remain unverified.

### macOS (initial product target)

Install full Xcode, launch it to install macOS components, install its command-line
tools, and select it as the active developer directory. GPUI's Metal build needs
full Xcode, not just standalone command-line tools.

```sh
cargo run --locked --features desktop
scripts/bundle-macos.sh
```

The script creates `target/bundle/MPC Remote Screen Viewer GPUI.app`, with a
stable bundle identifier and Local Network explanation. Its ad-hoc signature
is for development, not distribution.
For a universal bundle, first install Rust's `aarch64-apple-darwin` and
`x86_64-apple-darwin` targets, then run `scripts/bundle-macos.sh --universal`.

**User testing on macOS succeeded.** Detailed native-menu/dialog, Retina,
Local Network permission, signing and universal-output checks remain unverified.
If permission is denied, inspect System Settings → Privacy & Security → Local
Network for the Rust app. Terminal-launched tests cannot establish bundle permission
behavior.

### Synthetic screen preview

For an explicit, local-only development test (not a service implementation):

```sh
cargo run --locked --example mock_server
```

Then launch the GPUI UI, enter `127.0.0.1`, expand **Advanced**, use port `17447`,
and enter the **fake test password** `demo`. Or use:

```sh
cargo run --locked --features desktop -- --host 127.0.0.1 --port 17447
```

That command intentionally does not auto-connect without a password. Enter the fake
password in the UI and press Connect. The default mock advertises neither input
nor audio. To offer a **quiet synthetic 440 Hz tone** for an explicit playback test:

```sh
cargo run --locked --example mock_server -- --audio
```

Connect and click the speaker button (or launch the viewer with `--audio`). This is a synthetic
PCM16 test, not evidence of real-device codec/timing correctness. Start with low volume.
Do not point automated tests at a real MPC without authorization.

## AI teacher (MCP, read-only by default)

See [`docs/mcp-mpc.md`](docs/mcp-mpc.md) for the architecture, phased plan,
full tool contracts and MCP-client configuration. State/screen capture, Force manual
search/read and local highlights work in read-only mode. Phase 2 adds bounded
click/long-press, drag, scroll, key and ASCII-text input, plus cancellation.
The main-menu **Allow assistance** toggle is global approval: no per-gesture prompts,
permission timer or focus requirement. It starts off on each app launch. No audio tools exist.

Supply `manuals/Force - User Guide - v3.10.1.pdf` locally and install Poppler
(`pdftotext`; `pdftoppm` for diagram/page images). The manual is indexed in memory;
no cloud indexing or extracted-manual files are created.

```sh
cargo build --locked --release --features desktop,mcp
./target/release/mpc-remote-screen-viewer-gpui --teacher
# Configure your MCP client to launch this stdio server (use absolute paths):
./target/release/mpc-mcp --manual '/absolute/path/Force - User Guide - v3.10.1.pdf'
```

The viewer's bridge is off by default. `--teacher` or **AI teacher sharing** in the
main menu explicitly enables read-only sharing through a private same-user Unix
socket on Linux/macOS. Click **AI teacher · read only · ×**
to disable immediately; fullscreen also has the indicator. Sharing is never restored from preferences. Screen/manual
content requested by your agent may reach its configured model. The enabled socket
is accessible to other processes running as your OS user, not just a particular
agent. Device credentials and serial numbers are not exported.

For device input, switch on **… → Allow assistance** (Linux) or **View → Allow
assistance** (native menu). This also enables sharing if needed. While checked,
agent gestures run directly without further approval—even with the viewer unfocused.
It stays on through Settings, reconnects and screen changes, until you turn it off,
disable sharing, or close the app. It is not saved across launches.

Switch it off or press **Stop assistance** to cancel queued/running input and
request release. **Cancel gesture** / `mpc_cancel` stop only the current action and
leave global assistance on. Local viewer input takes over after agent release,
without resetting the toggle. Old gestures never replay after source changes.
Cursor/held/path/scroll feedback reflects worker writes, not model intentions.
A socket write is **not** visual verification: inspect a fresh screen after every
gesture and never automatically retry uncertain input. Text is limited to 64
printable ASCII characters; do not send secrets. Physical takeover is not detectable.

Highlights have a distinct AI-teacher label, follow fit/pan/rotation, expire within
10 seconds, and never appear in device screenshots or recordings. Old observations
cannot annotate a replaced/reconfigured/disconnected device. Manual compatibility
is explicitly unverified until the agent/user confirms model and software version.

Core/MCP tests use synthetic pixels and manuals, never a real device:

```sh
cargo test --locked --features mcp
cargo clippy --locked --features desktop,mcp --all-targets -- -D warnings
```

For Pi, use the [project-local setup instructions](docs/mcp-mpc.md#pi-configuration)
with direct tool exposure, then trust the project and reload/restart Pi. Keep the
manual PDF and machine-specific `.pi/mcp.json` local; both are ignored by Git.

The stdio smoke test exercises MCP initialization, all twelve tool definitions,
disabled-input rejection and PDF page rendering when Poppler is available
(required in the MCP CI job). Loopback tests cover globally authorized gesture writes,
cancellation, takeover and reconfiguration; **phase-2 GUI/Force effects remain
unverified**. A prior phase-1 live Pi session exercised its six tools; the user confirmed the overlay appeared
and completed a one-bar techno pattern through guided, user-performed input.
See [live teaching evidence and limitations](docs/mcp-mpc.md#live-read-only-teaching-smoke-test).
The agent verified note positions from screenshots; the user identified the sounds.
Model/firmware metadata was unavailable, and audio analysis was not performed.
During playback, captures sometimes return `Framebuffer busy; retry`; retry reads
with a bounded delay. Fullscreen/revocation/reconfiguration and macOS teaching
behavior still need hands-on coverage.

## Workspace

When disconnected, a centered, scrollable connection card provides labeled address
and password fields, an **Advanced** port field, and recent devices. Enter submits
from any connection field. Connecting/retrying shows the current phase and a
**Cancel connection** button; credentials cannot be edited until stopped.
Transient network failures retry with exponential backoff and ±25% jitter, capped
at 30 seconds; a connection stable for 30 seconds resets the backoff. Authentication,
server-proof, malformed required protocol, and unsupported-version failures stop automatically
retrying. Edit the connection fields and choose **Connect** to retry explicitly.

Once connected, the screen takes the available workspace beneath one fixed-height
toolbar: device/connection menu, **Fit / 1:1**, remote control, speaker, screenshot,
recording and fullscreen. At smaller widths capture actions move into the **…**
overflow menu rather than wrapping. Active control is highlighted; active recording
has a red Stop control and shows elapsed time in the status bar. The status bar
shows connection health; detailed metrics remain available in the information
inspector, including playback-lock contention callbacks. Normal quit drains pending
viewer and audio settings writes.
Empty or fully skipped updates are ACKed without rebuilding the displayed image.
Fullscreen hides the toolbar/status bar and provides an Exit fullscreen overlay.

As an additional decoding safeguard, an update has a default aggregate budget of
64 Mi rectangle pixels (including overlaps and failed decode attempts). Once
exhausted, its remaining rectangles are skipped; its valid prefix is retained and
the update is still ACKed. Library callers can set `Config::frame_pixel_budget`.

**… → Settings…** (Command-comma) opens one modal dialog with **General**, **Audio**
and **Streaming** tabs. Volume, live input/output levels, output-device selection
and target buffering live in Audio; sample rate and output buffer are under
**Advanced output options**. Preferences save immediately. AI assistance is a
separate main-menu toggle, not a Settings option; permission is never saved. Dialogs release held remote
input and keep field editing separate from device keyboard forwarding. Escape
closes the dialog. Notifications overlay the workspace instead of resizing it.

Basic user testing succeeded on macOS and Omarchy Linux; detailed Retina and
interaction checks remain open.

## Current shortcuts

| Shortcut | Action |
| --- | --- |
| F11 / F | Fullscreen |
| Escape | Exit fullscreen |
| S | Toggle statistics |
| R | Full refresh |
| P | PNG screenshot in the saved folder (asks on first use) |
| F12 | Toggle remote control |
| Command-comma | Settings |
| Command-Q | Quit |

Letter shortcuts do not apply while editing a text field or forwarding remote
keyboard input. F11/F12 remain local. With control off, double-click the screen
to toggle fit/actual size. Drag or scroll to pan in actual size; Shift-scroll pans
horizontally.

## Viewer settings and recent devices

The app restores the last valid host/port, fit/actual size, statistics/port-field
visibility, remote-control preference, FPS limit and analyzer display settings.
Defaults remain fit mode, statistics visible, port hidden, control off and
server-default FPS. Once enabled,
**remote control stays enabled across launches**, but input is still gated by the
connected device's capabilities and screen focus. Held keys/buttons are never saved.
FPS can be selected before connecting and is applied on login and reconnect.

**Recent devices** keeps up to 10 successfully connected host/port pairs, newest
success first, with available screen/model/serial/firmware labels. Hostnames match
case-insensitively and equivalent numeric IP addresses deduplicate; different ports
remain separate. Late metadata updates the existing row; absent/empty metadata
retains earlier labels. Selection fills the address, reveals a nondefault port and
**clears the password without connecting**. Stop any current connection/retry first.
The connection card also offers **Clear**, which leaves the last host intact.
These ordering, limit and selection rules are the client's recent-device policy.

Viewer settings use versioned `viewer-settings.json` alongside screenshot/audio
preferences in the Rust app's own directory. Writes are atomically replaced on one
coalescing background worker; normal quit drains pending writes. Invalid/incomplete
port edits retain the last valid port. Missing fields receive defaults; malformed,
oversized or unsupported-version settings show an error and use defaults, without
rewriting the file merely on load. A subsequent setting change can replace it.
Settings are scoped to this app.

Explicit `--host`/`--port` override saved values; omitted options use saved defaults.
Restoring a host never auto-connects, even with `REMOTE_SCREEN_PASSWORD` set:
auto-connect still requires an explicit nonempty `--host` and a nonempty password.
Passwords remain UI/environment/CLI-only and are never included in recent devices.

## Device information and statistics

Click the status-bar information icon, the device menu's **Device information**, or
**View → Device Information…** for a live, scrollable panel:
model/serial/firmware, connection and protocol, native/upright screen dimensions,
rotation/pixel masks, input/audio capabilities, network totals/quality/RTT, and
source/output audio details with buffering, levels and dropout counters. Missing
metadata is shown explicitly; protocol is not reported as negotiated until Hello
validates. Toggling the panel releases held remote input.

The statistics row (**S** / **View → Show Statistics**) includes processed-frame FPS
and receive kbit/s. Rates are sampled by the network worker, not by UI repaints;
a static screen settles to zero FPS. Bandwidth includes all complete received
messages plus their five-byte envelopes, including audio and metadata, but not
TCP/IP overhead or incomplete messages.

The panel graphs retain at most **120 samples**, each spanning **at least one actual
second**. Delayed work produces one longer interval, not artificial backfill; graph
widths reflect those durations. First rates include handshake traffic/time. A drop
clears live rates; counters/history remain during retry wait and reset at the next
attempt. Nothing is saved to disk. These bounded sampling and graph policies are
part of this client. The panel is build-checked; macOS rendering, scrolling and
input occlusion remain unverified (S1).

## Device audio

Click the speaker button or use **View → Play Device Audio**. `--audio` enables playback on
connection. The service must advertise audio support; no audio requests are sent
to devices that lack it. Initial preference is off; subsequent launches restore it.

**Settings → Audio** selects system/default or a named output. Its **Advanced
output options** offer supported sample-rate choices and output buffer sizes of
**64 frames or higher**, plus **Default**. The experimental 16/32-frame options have been removed; saved smaller
selections fall back to Default without losing other audio preferences.
Unsupported sizes are not offered or forced on the driver.
The **Target buffering** slider adjusts the playback target from **10–300 ms** in
1 ms steps, with a **Reset to 80 ms** action. The 10 ms floor preserves two 5 ms
fades. This is separate from the output device's buffer size in frames.
Settings apply live and persist alongside screenshot preferences in
`audio-settings.json`; older settings files retain the 80 ms default.
Missing saved devices fall back to system
output; unsupported rate/buffer settings show warnings and use supported defaults.
**Rescan / Retry** refreshes devices and retries output after a driver error or a
system-default change. Output initialization is lazy and errors leave the screen
connection running. No microphone or computer loopback mix is captured.

The volume slider uses square-law gain; double-click it or use **Reset volume**
to restore full volume. Settings → Audio shows input/output interval peaks in dBFS,
with silence floored at -120 dBFS. Device Info shows source format, queue latency
and dropout counters.
The bounded queue defaults to an 80 ms target and a 300 ms maximum-latency
setting (the trim threshold remains at least 1.5× the selected target).
Changing the target fades out briefly, trims or prefills the existing queue, then
fades in without reopening the output device. Lower values reduce latency but
increase dropout risk; packet timing and hardware buffers also affect actual
latency. Detailed playback/meter behavior and hot-plug remain unverified on
real devices/CoreAudio.

### Audio analyzer

Click the spectrum-bars **Audio visualizers** button in the top toolbar to toggle
the docked analyzer and peak/RMS meters. It stays available while disconnected and
highlights while the panel is open. **View → Audio Analyzer** (or **… → Audio analyzer**
on Linux) also opens it. Switch views with **Spectrum**, **2D Spectrogram**,
**3D Waterfall**, or **Waveform** in the panel header. Toggle **Stereo width** to show
a vectorscope to the **right of the chart**, before the meters. It is independent of
the view selection; toggling it does not change the selected view or history.
Drag the top divider to resize; **Close** stops analysis.
On first launch the panel is hidden. Its visibility, selected view, height and
Stereo width toggle are saved between launches, separately from playback preference.
It can request device audio with playback off, so silent analysis needs no output device.

The 64-band logarithmic spectrum covers 20 Hz to the smaller of 20 kHz and Nyquist,
with a fixed −90 to 0 dBFS range. Analysis uses a 2048-sample Hann FFT with 50% overlap;
the two-channel power average avoids antiphase cancellation. Meters show raw source
peak and RMS levels before playback volume (mono shows one meter; multichannel sources
show the first two channels). Compact vertical meters span −60 to 0 dBFS: the fill
shows RMS and a thin marker shows peak, turning red near full scale. Hover the meters
for exact readings. These are sample peaks, not true-peak or LUFS measurements.

The **Stereo width** vectorscope draws a glowing green **density cloud** from the
last **200 ms** of unwindowed source audio, before playback volume. Frequent sample
positions glow brighter; older dots fade away instead of being joined by lines.
A vertical cloud is mono; a wider cloud indicates more stereo spread; a horizontal
cloud indicates antiphase content that can cancel in mono. Left/right-only signals
follow the corresponding diagonals. Mono sources stay vertical, and multichannel
sources use the first L/R pair. The cloud has reference grid lines and L / Mono / R
guides, but no outer border, meter beneath it, or hover tooltip. Image edges are
clipped to keep texture-atlas bleed from appearing as a rectangular frame. Both
axes use the same scale and remain square when resized.

Gentle **display auto-gain** makes quiet audio easier to see, with a maximum boost
of about **+18 dB**, 500 ms gain recovery, and immediate reduction for loud transients.
One gain is applied to both channels and the entire cloud, preserving balance and
shape. Digital silence and samples below −80 dBFS do not generate dots. Auto-gain
never changes playback, the spectrum, or peak/RMS meters.

The cloud resets on source gaps and stream/format changes. It also fades during
delivery pauses and clears after 200 ms without analysis.
Only display settings are saved; source audio and analysis history are never saved.
The 200 ms cloud persistence, auto-gain limits/recovery and noise gate are constants
in `src/vectorscope.rs`, not additional UI settings.

The **2D Spectrogram** shows the last **10 seconds**, with time moving left to right
(newest at the right), logarithmic frequency increasing upwards, and color representing
−90 to 0 dBFS (dark → purple → orange → yellow). The **3D Waterfall** shows the same
history as a fixed-perspective stack of ridges: frequency increases left to right,
older audio recedes, and ridge height represents level; each ridge is tinted by its
peak level. Ridges peak-merge into six fixed source-time buckets per second,
so old ridges do not change identity when the history scrolls. It is a
projected 3D view, not an orbitable 3D scene. Switching views retains history.
Unobserved history stays dark.

The **Waveform** shows the last **1 second** of unwindowed source amplitude, newest
at the right, before playback volume. Stereo has separate **L** (blue) and **R**
(green) lanes; mono has one lane, and multichannel sources use the first L/R pair.
Each lane has a fixed **−1 to +1** scale and a zero line, with no automatic gain or
channel mixing. Min/max buckets (2,000 per second) preserve short transients and
are peak-merged into display pixels when resized. Silence draws a thin line;
unobserved audio and source gaps stay blank rather than being connected. The
bounded history is retained across view changes and delivery pauses, and resets
on stream/format changes. Time is driven by source samples, not the UI frame rate.

A bounded, best-effort background worker keeps analysis out of playback callbacks.
Its nonblocking input queue absorbs up to 500 ms / 256 packets of delivery bursts
(with room for one larger protocol-sized packet). It never waits on playback or
allows an unbounded backlog. FFT buffers and band mappings are reused; mono needs
only one FFT. The UI rasterizes/uploads the heatmap only while the 2D view is selected,
and the density image only while **Stereo width** is visible. Stereo history keeps
bounded, shared sample chunks and counts each accepted source frame once, never
double-counting overlapping FFTs or packets. Spectral history uses source-sample
time, not UI frame rate, and merges high-rate FFTs into at most 30 audio columns/second
using per-band peaks. Source-clock gaps and dropped analysis blocks reset the partial
FFT and stereo cloud, and insert a blank interval while preserving earlier spectral
history. Duplicate/overlapping packets do not rewind either the spectral or waveform
history. Only a new stream or format clears history. After 250 ms without analysis,
live levels clear but the spectrogram and waveform are retained until source audio
resumes. The UI polls at roughly 30 Hz.
The meter tooltip also includes diagnostics: **Drops** counts packets rejected by
the local analysis queue; **Gaps** counts source-position discontinuities seen by
the worker (which can include those local drops). Both counters last for the current
analyzer instance. Rising Drops points to local overload; rising Gaps with no Drops
points to missing/discontinuous input, not a rendering-only problem. Genuine missing
audio stays blank.

For live use, prefer an optimized build:
`cargo run --locked --release --features desktop`.
A manual CPU benchmark (not a GPU/live-device test) is available with
`cargo test --locked --lib analyzer::tests::benchmark_analysis -- --ignored --nocapture`.

## Recording

Click the recording button (or **… → Record…** in compact windows) or
**File → Start / Stop Recording…** while connected, choose a new `.mov` path,
then click the active Stop button to finalize. The chooser starts in
the screenshot folder (or home); existing files are never overwritten. Wait for
“Saved recording” before using the file. Disconnect and quit also finalize.

Movies contain full upright device pixels, not window chrome/cropping: Photo-JPEG
at quality 80 in QuickTime MOV, with dirty-image capture on a 66 ms timer. Static
images are held until the next update/Stop. Source size changes are letterboxed
into the initial canvas. JPEG and disk work run on a dedicated worker, without an
image backlog. No FFmpeg installation is needed; optional tests use it if present.

Recording includes decoded device PCM16 whenever playback **or an open analyzer**
requests device audio, independent of output volume or playback-device failure.
For silent video, turn playback off and close the analyzer; the recording-start
notice currently checks playback only. Devices without audio produce silent video.
Starting device audio mid-recording adds an aligned audio track. Duplicate/overlapping
source samples are trimmed, gaps filled with silence, and toggles re-anchor to
elapsed recording time. Rate/channel changes stop recording with an error and attempt to finalize
already written samples; codec-only changes are allowed. Queue overload, gaps over
60 seconds and metadata/duration limits also surface errors instead of silently
losing data. Files are retained on failure; a failed disk write may leave an
unplayable partial movie. Crash recovery is not implemented.

QuickTime/macOS playback, device lip-sync and the R1 hardware validation matrix
remain unverified.

## Captures and credentials

PNG captures use the full **upright device image**, not window chrome or the
currently cropped/panned display. Encoding/disk writes run in the background.
The Screenshot button and P shortcut ask for a default folder on first use, then
save there automatically with noncolliding filenames. Change it in **Settings →
General → Screenshot folder → Choose folder…**. Cancelling the folder prompt saves nothing; a missing saved
folder prompts again. Successful “Saved …png” notifications disappear after three
seconds; errors remain visible and newer notifications are not cleared by an old
timer.

The folder survives restarts in the app's existing settings directory, kept for
compatibility:
`mpc-remote-screen-viewer-rs/screenshot-settings.json`, under macOS
`~/Library/Application Support/` or Linux `$XDG_CONFIG_HOME` (fallback `~/.config`).
Audio preferences are saved separately in `audio-settings.json` in that directory;
viewer settings and recent devices use `viewer-settings.json`.

File → Save Screenshot… still opens a per-file chooser and refuses to overwrite
an existing path, even if the native dialog allowed it; select a new filename.

Passwords are never saved, put into recent hosts, or logged. `--password` overrides
`REMOTE_SCREEN_PASSWORD`; the exact password is not trimmed. Prefer the masked
UI/environment to `--password`, which can appear in process listings/history.
The device protocol is **not encrypted**: use a trusted LAN or an external VPN/tunnel.

Fixtures are synthetic, not real captures. Passing these tests does **not** prove
the full real-MPC compatibility matrix, UI correctness, keyboard handling,
audio timing or recording quality.
