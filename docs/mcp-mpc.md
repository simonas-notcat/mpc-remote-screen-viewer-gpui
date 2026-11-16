# MCP device teacher: Force first

## Goal and boundaries

Let a vision-capable agent teach the user to operate an Akai Force: explain the
current screen, retrieve the correct manual instructions, point to the next
control, and eventually carry out explicitly authorized gestures. The first
reference is the user-supplied `manuals/Force - User Guide - v3.10.1.pdf` (468 PDF
pages). Manual version is **v3.10.1**; applicability to a connected device must be
verified, not inferred from the filename or the existence of a connection.

The agent handles teaching and reasoning. The MCP server supplies evidence and
bounded capabilities. Do not put a second LLM in the server. Manual passages and
screen text are reference data, never instructions that override the user's goal.

The existing service provides screen pixels, generic touch/mouse/keyboard events,
and an exported audio stream. It does **not** provide a UI accessibility tree,
logical parameter access, or verified hardware-button/pad/encoder control. A
screen name from ServerInit is metadata, not proof of the currently open mode.

## Architecture

```text
agent / MCP client
    | stdio (official Rust rmcp SDK; stdout is protocol only)
mpc-mcp binary --- locally imported, versioned Force manual
    | permission-restricted Unix socket, bounded request/response framing
viewer-owned Teacher observation/input controller
    | shared Session snapshot/framebuffer + bounded, globally authorized gestures
existing Session worker --- device protocol --- Force
    |
GPUI viewer + local-only teaching overlays
```

Use one device connection. The viewer hosts the observation/input controller;
the MCP executable attaches through local IPC. Manual lookup works even when the
viewer is closed. Later a headless host can implement the same controller contract.
Neither GPUI nor an audio output device is a dependency of the MCP executable.
Existing socket ownership, framing, authentication, ACKs and audio decoding remain
in the current session worker. A bounded pixel-decoder helper accepts one frame at
a time, with no socket access or early ACK; the owner remains available for input
and release while decoding or waiting for framebuffer readers.

The bridge is **off by default**, explicitly enabled in the viewer, visibly marked,
and immediately revocable. Socket directory and socket are private to the current
OS user; there is no HTTP listener or remote bind. Local processes running as that
user can attach while enabled: this is a same-user boundary, not per-agent OAuth.
Screens and manual excerpts are sent to the configured model only when requested;
the user must trust that model/client. Revocation stops new sharing; it cannot
recall data already delivered or in transit. No device passwords enter MCP or IPC.

## Observation and gesture contract

- Capture full upright device pixels, not desktop screenshots, window chrome,
  overlays, or the viewer's crop/pan. Return native MCP image content plus JSON.
- Each capture has an opaque observation ID, generation, content revision, native
  upright dimensions, returned image dimensions, crop bounds, timestamp and source
  freshness/health. Coordinates for highlights are **returned-image pixels**;
  mapping to upright device coordinates is explicit in the response.
- Observations are bounded and expire. Reject unknown/expired IDs, disconnected
  sources and changed session/framebuffer identities. Do not require the entire
  image to be unchanged: meters/playheads animate. Report pixel-update age without
  treating a connected static screen as a dead stream.
- An action must reference its observation. Future actions report queued, written,
  and visually verified separately: `Session::command()` currently only confirms
  queue acceptance. Frame ACKs are not input acknowledgements.
- Work one gesture at a time, followed by a fresh observation; no blind sequences.
  Waits are bounded and preferably scoped to a region, not global pixel stability.

## Visual feedback (required for all agent gestures)

Feedback is a local GPUI overlay in device coordinates, transformed with the same
fit/actual-size/pan geometry as the screen. It never changes the device framebuffer,
remote input routing, exported screenshots, or recordings.

Read-only tools paint a labeled, high-contrast teaching rectangle. The selected
robot button (**Toggle MCP**) in the top toolbar indicates sharing is enabled;
click it again to revoke sharing/input and clear overlays. The adjacent **Allow
assistance** switch appears only while MCP is on. Fullscreen reuses the same pair
at the top; there are no duplicate status/footer/menu switches. Highlights expire,
can be explicitly cleared, and disappear on revocation, disconnect, source
replacement or screen reconfiguration. Their label identifies them as agent
guidance, not actual MPC UI.

Phase 2 adds a distinct agent cursor and gesture lifecycle events (implemented):

| Gesture | Required local feedback |
| --- | --- |
| Click / long press | Target crosshair, press ring/pulse, held-state indicator, release/fade |
| Drag | Start marker, moving cursor, visible path and current held position; end marker |
| Scroll | Target cursor plus directional arrows and signed amount |
| Keyboard / text | Small action label, without displaying sensitive text |
| Cancel / rejection | Immediate release indication; rejected actions must not look executed |

Feedback must be driven by the controller/worker's actual action lifecycle, not
just the model's intention or command enqueue. A bounded shared feedback model
should cover pending/running/completed/rejected/cancelled states. Draw above device
pixels and below modal UI; never intercept user input. A local stop/takeover control
remains available during gestures. Device-side physical takeover cannot be reliably
detected with the current protocol and must not be promised.

## Phase 1: read-only teacher

### MCP tools

| Tool | Contract |
| --- | --- |
| `mpc_get_state()` | Viewer availability, read-only mode, model/firmware, protocol/capabilities, screen geometry and health; manual identity/version and explicit compatibility warning. No credentials. |
| `mpc_get_screen(region?, max_size?)` | Bounded PNG image plus observation/coordinate metadata. Requires an enabled bridge and a streaming source with decoded pixels. |
| `mpc_search_manual(query, limit?)` | Ranked local full-text passages, headings, PDF page numbers, detected printed page labels and citations. Bounded query/results. |
| `mpc_read_manual(document_id, start_page, page_count?, include_image?)` | Full page text, citation and optional rendered PDF page image for diagrams. Page numbers are 1-based PDF indices, never ambiguous printed labels. |
| `mpc_highlight(observation_id, bounds, label?, duration_ms?)` | Show a bounded, expiring teaching rectangle in the local viewer. No input is sent to Force. |
| `mpc_clear_highlight()` | Remove the local teaching annotation. |

The MCP server advertises teaching instructions: inspect state, verify Force/manual
version, retrieve and cite relevant instructions, inspect the current screen,
explain one step, highlight it, let the user act, and observe again. State clearly
when a physical control is needed or when a screen/feature cannot be identified.
Do not invent semantic selectors, buttons, device state or audio observations.

### Manual implementation

Import the approved PDF at MCP startup using Poppler `pdftotext -layout`; keep the
page-separated index in memory. Use bounded lexical/BM25-style passage ranking,
not an external embedding service. Preserve page boundaries and printed labels,
record a SHA-256 document fingerprint, and include citations in all results. Search
is lexical rather than guaranteed semantic retrieval. Optional diagram rendering
uses `pdftoppm`, one bounded page at a time. Subprocess duration/output, PDF size,
result count, returned text and image dimensions are capped. No shell interpolation.

The PDF is user-provided reference material, not a generated fixture. Do not embed,
redistribute or commit extracted manual text/page images. Unit tests use synthetic
pages/PDFs; real-manual validation is a separate explicit local check.

### Acceptance

- MCP initializes over stdio, advertises only the six read-only/local-overlay tools,
  returns native image content, and reports errors without polluting stdout.
- Manual search/read works without a running viewer. Missing dependencies/PDFs and
  invalid ranges fail clearly. The Force manual identifies itself as v3.10.1.
- GUI opt-in, visible status, local revocation and overlay lifetime work without
  requiring remote control, playback, or audio analysis.
- Disconnect/reconnect/reconfigure cannot reuse an old highlight observation.
- Crop/resize/rotation and fit/pan mappings are tested. Overlay metadata is bounded;
  raw screen captures remain free of teaching graphics.
- IPC framing, permission checks, request/output bounds and shutdown are tested.
- No phase-1 handler has a route to remote Input, Refresh, Audio or device settings.

### Implemented phase-1 status

Phase 1 is implemented on `feature/mcp-read-only-teacher`, including the six tools,
opt-in/revocation, private IPC, native MCP images, crop/resize mapping, bounded
observations, expiring labeled local highlights, and manual page diagrams.

Validated locally: core/MCP tests, real stdio tool dispatch against synthetic
pixels/PDFs, Clippy with desktop+MCP, and Rust 1.88 MCP compilation. Explicit local
validation of the supplied **468-page** PDF passed import, fingerprinting, search
(Chop Mode, Track Mixer, Remote Screen) and page-248 PNG rendering.

A subsequent live Pi session exercised all six tools against the running viewer;
the user confirmed the highlight appeared and completed the one-bar drum lesson
below. This establishes a working read-only teaching path with user-performed
device input, not autonomous control, audio perception, or a broad hardware/macOS
acceptance matrix. Device model/software metadata was not supplied, so exact manual
compatibility remains unverified.

To repeat the explicit, offline-only supplied-manual check:

```sh
MPC_FORCE_MANUAL="$PWD/manuals/Force - User Guide - v3.10.1.pdf" \
  cargo test --locked --features mcp --test mcp \
  supplied_force_manual_import_search_and_page_image -- --ignored --nocapture
```

### Live read-only teaching smoke test

The user restarted Pi with the project MCP server loaded successfully. Observed
results, separate from the synthetic automated suite:

| Check | Evidence |
| --- | --- |
| Connection/state | `Streaming`, protocol v2, 1280×800 upright screenshots; no skipped rectangles reported during the observed session. |
| Manual retrieval | Search and complete-page reads worked with exact citations. A PDF page image was rendered through MCP (Clip Matrix Mode, PDF p. 118). |
| Local visual feedback | A ten-second labeled Cutoff highlight was requested on a Fabric XL screen; the user explicitly confirmed it appeared in the viewer. Highlight clearing also returned successfully. |
| Teaching loop | Inspect screen → retrieve instructions → highlight → user acts → inspect again worked through navigation and step entry. |
| Sound identification | The user auditioned and identified each drum voice. The agent did not receive or analyze audio. |
| Scope | No MCP device-input, transport, audio-toggle, or settings command was available or sent. All device changes were made by the user. |

The lesson used track 1's **DrumSynth:Multi / Deep House** preset, a
**one-bar, 4/4 sequence at 128 BPM**, and **1/16 steps**. After the user auditioned
each voice, screenshots independently verified the note positions for each lane:

| User-identified voice | Pad in this session | Active steps |
| --- | --- | --- |
| Kick | A01 | 1, 5, 9, 13 |
| Clap | A02 | 5, 13 |
| Hi-hat | A03 | 3, 7, 11, 15 |

These pad assignments are session evidence, **not a universal DrumSynth/kit MIDI
map**. Ask the user to audition unknown sounds rather than assume A01/A02/A03
always mean kick/clap/hat. Changing the selected pad changes the lane being viewed;
previously entered lanes remain. Existing sequences must not be shortened or
cleared without checking that the user wants to discard or hide any prior work.

Reference pages used for the workflow: Step Sequencer **173–175**, sequence length
and looping **318**, and DrumSynth:Multi **422** (all 1-based PDF indices). The
musical pattern is a teaching example, not a preset prescribed by the manual.

#### Limitations observed and checks still open

- The service returned `device: null` and the generic screen name `MPC`. These do
  not identify hardware or firmware; do not report manual compatibility as verified.
- During playback, some screen requests returned **`Framebuffer busy; retry`**.
  A later read succeeded without changing playback or connection settings. Clients
  should make only bounded, spaced read retries; ask the user to pause playback if
  contention persists. A bounded server-side capture wait/backoff is a follow-up,
  not implemented in phase 1. Never retry future mutations as if they were reads.
- A static screen's pixel-update age grew while the connection stayed healthy;
  advancing playback produced fresh revisions. Pixel age alone is not a liveness test.
- Only one rectangle can be shown at a time. A new highlight replaces the previous
  one. Use fresh observation IDs; annotation expiry and bounds are enforced.
- Live fullscreen/revocation/reconfiguration behavior, detailed fit/pan/rotation
  rendering, native macOS IPC, long-running robustness, and broader model/software
  coverage remain unverified. Unit tests do not substitute for these checks.
- No perceptual assessment of the completed mix, saved-project round trip, device
  automation, or audio-analysis acceptance was performed.

## Phase 2: safe assisted interaction

Implemented on `feature/mcp-assisted-interaction`, stacked on phase 1. The six
phase-1 tools remain available; six additional tools queue bounded input or request
cancellation. The top-toolbar robot (**Toggle MCP**) enables read-only sharing;
its adjacent **Allow assistance** switch is the single global input approval:
no per-gesture prompts, permission timer or focus requirement. It starts off on app
launch, independently of ordinary remote control. MCP/IPC cannot enable it;
`confirmed: true` and unknown arguments are rejected.

### Tools and bounds

Every gesture requires `observation_id` from a fresh `mpc_get_screen`. Targets and
drag endpoints are integer **returned-image pixels**, mapped through that exact
observation's crop/resize into upright protocol coordinates (never inverse-rotated).

| Tool | Additional arguments / bounds |
| --- | --- |
| `mpc_click` | `target: {x,y}`, `hold_ms?` (50–2000, default 100); long press uses a longer hold. Left button, or single-contact touch on touch-only devices. |
| `mpc_drag` | `start: {x,y}`, `end: {x,y}`, `duration_ms?` (100–3000, default 500); straight-line path, at most 61 bounded steps. |
| `mpc_scroll` | `target: {x,y}`, `amount` (nonzero −120…120 signed **protocol wheel units**, not browser pixels); requires mouse capability. |
| `mpc_press_key` | `key` (supported `input.rs` Linux wire identity), `modifiers?` (`[Shift, Control, Alt, Meta]`, default false); requires keyboard capability. |
| `mpc_type_text` | `text` (1–64 printable ASCII characters, US wire-key mapping); no Unicode, paste, newlines/control characters, or secrets. |
| `mpc_cancel` | No arguments. Cancels pending/queued input or requests release of a running gesture. **Not rollback**; poll state for the outcome. |

### Global authorization and lifecycle

1. Click the top-toolbar robot (**Toggle MCP**) to enable sharing, then switch on
   **Allow assistance** immediately next to it. The switch is hidden while MCP is
   off; MCP always starts read-only. This grants global input permission: gestures
   run without any further local prompt.
   The toggle stays on until explicitly switched off, sharing is disabled, or the
   app closes. It is not timed and is unaffected by window focus or Settings.
2. Inspect a fresh screen and submit **one** gesture. The tool returns `queued: true`
   and an action ID. `device_input_sent: false` describes queue acceptance, not a
   promise that execution waits: the worker can start immediately. There is no
   per-gesture approval method, button or `pending_approval` state.
3. Poll `mpc_get_state.assisted_control`: `enabled` reports global authorization;
   `action.phase` progresses `queued` → `running` → `completed`, `cancelled`,
   `rejected`, or `uncertain`. `written_steps` / `release_written` describe socket
   writes, not device acknowledgement or visual verification. Global approval
   authorizes input, not unrequested destructive work; follow the user's task.
4. After every terminal result, capture a **new** screen and verify the change.
   Pre-action and in-action observations cannot authorize later gestures. Animated
   pixels alone do not invalidate observations. Never blindly queue sequences or
   automatically retry uncertain mutations, including after an IPC timeout.

Switching **Allow assistance** off cancels queued work and requests automatic
release of held input while keeping sharing enabled. `mpc_cancel` stops only the
current gesture and leaves assistance enabled. Switching the robot (**Toggle MCP**)
off also disables sharing, resets/hides assistance and clears teaching overlays.
No permission is restored across app launches, but it remains on through focus changes, Settings, disconnect/
reconnect, reconfiguration, and local human takeover. Connection/source changes
still cancel old work and invalidate its observations; nothing is replayed.

Only the local toggle enables input; MCP has no operation to enable/disable global
permission. The optional MCP process still has no session command sender or GUI/
audio-output dependency. A same-user client can use the granted global permission,
so enable assistance only for a trusted client/model. No per-agent authentication
or semantic classification of generic clicks is provided.

Individual queue and execution deadlines remain **5 seconds**, capped by observation
expiry. These bound a gesture/release, **not** global permission. A one-job decoder
keeps slow codecs/conversion and framebuffer-reader waits off the socket owner.
The owner checks cancellation/deadlines and processes local takeover at 50 ms wait
intervals, releasing on completion/cancel without waiting for pixel decoding.
Deltas remain ordered and ACKed only after completion; teardown attempts socket
release before joining the decoder. Failed/partial writes report uncertainty and
are never replayed; a broken connection cannot guarantee release. An in-flight
write cannot be recalled.

The GUI-independent `control.rs` layer shares `input.rs` wire construction. The
session worker alone executes input and rechecks source/capabilities. It releases/
cancels the agent's entire gesture **before** sending local viewer input; held
human input and in-flight writes block new agent gestures without resetting global
authorization. Physical device takeover cannot be reliably detected.

The single robot/assistance pair is in the normal top toolbar or fullscreen top
strip. No bottom bar, status-bar toggle or duplicate menu switch is shown. Workspace
clipping keeps analyzer/notification layers away from the normal toolbar; the
fullscreen controls render above the workspace and below modal UI.
Feedback shows an agent crosshair, held ring, actual traversed
drag path/end markers, signed scroll arrow and non-sensitive key/text label above
pixels and below modal UI, without input hitboxes. Queued feedback is amber, running
cyan, completed green and rejected/cancelled/uncertain red. Terminal graphics fade
and clear even on static screens. Screenshots/recordings never include this feedback.

### Phase-2 validation and remaining checks

Synthetic/loopback tests cover off-by-default authorization, all five gestures
running under one global toggle without additional approval, off/cancel release,
permission surviving action completion/cancellation and source changes, no old-work
replay, local takeover ordering, framebuffer contention, capability/source rechecks,
stale observations, deadlines, bounded paths/text, hidden text, failed-write
uncertainty and off/on races. Stdio tests enumerate twelve tools and retain manual,
image, highlight and disabled-input rejection coverage. Decoder regressions cover
single-large/many-rectangle LZ4 workloads, off/cancel/deadline release and local
input during decoding, release before a codec join on shutdown, human-held
ownership during reader contention, and queued
human input cancelling the agent before its first socket write. One-job bounds,
protocol errors and reader-safe decoder shutdown have unit coverage. No automated
test contacts real Force.

A separately authorized live MPC smoke test opened its main menu and returned to
Main Mode using two `mpc_click` actions, with a new screenshot verifying each result.
No project edits or playback were initiated. Model/firmware metadata remained
absent: this is narrow click-navigation evidence, not Force/manual compatibility
or acceptance of every gesture.

Desktop/MCP tests, Clippy and Rust 1.88 checks do **not** establish live rendering.
Hands-on robot/switch/analyzer/notification/fullscreen behavior, fit/pan/rotation,
real-network failures and native macOS/broader hardware coverage remain unverified.
Phase-1 live teaching evidence above is not phase-2 acceptance.

## Phase 3: audio inspection

Add `mpc_get_audio_analysis(window_ms?, visualization?)` for source peak/RMS,
frequency bands with boundaries, stereo correlation, waveform/spectral histories,
time range, age and gap/drop diagnostics. Reuse current analyzer data before
playback gain. Return labeled plot images when useful, rather than requiring the
agent to read numbers from screenshots. Current peaks are sample peaks, not LUFS
or true-peak measurements. FFT plots alone do not justify perceptual claims.

Centralize audio demand: playback OR viewer analysis OR MCP subscribers. Do not
change speakers/output preferences or replace an existing analyzer tap. Analysis
must remain off playback callbacks with bounded best-effort queues.

## Phase 4: short audio clips

Add `mpc_capture_audio(duration_ms, format?)` and clip-based inspection. Capture
from decoded, pre-gain/pre-resampling source audio into a bounded separate buffer,
never consume the playback queue. Start with short PCM WAV clips. Expose native MCP
audio content only where supported; a base64 string in text is not audio perception.
Report source format, time/sample indices, discontinuities and incomplete captures.
Use bounded retention/TTL and explicit export; do not silently persist performances.
The exported source stream is not proof of access to isolated tracks or stems.

## Phase 5: verified navigation helpers

Promote repeatable observe-act-verify paths into model/software-specific recipes.
Only then add helpers such as `navigate_to_screen`. Every step must check expected
evidence and stop on uncertainty. Physical buttons/pads/encoders require a separately
verified protocol/MIDI integration; generic keyboard events are not a substitute.

## Validation strategy

Use existing loopback-only session tests and synthetic framebuffer/manual fixtures.
Add MCP protocol smoke tests, stale-observation and overlay lifetime tests, private
socket/oversized frame tests, and document search/page-boundary checks. Run core
and MCP tests without GUI/audio hardware; compile and lint desktop integration.
No automated test may connect to a real Force without explicit authorization.
Record actual GUI/Force smoke-test evidence separately from compilation.

## Running the teacher

Install Poppler (`pdftotext` and, for diagrams, `pdftoppm`). Build:

```sh
cargo build --locked --release --features desktop,mcp
```

Click the robot icon in the top toolbar (tooltip **Toggle MCP**) for read-only
sharing. When it is selected, the adjacent switch (tooltip **Allow assistance**)
authorizes global input. Click the robot again to disable sharing and reset/hide
assistance, or switch assistance off to stop input without disabling sharing. The
same pair appears at the top in fullscreen; no bottom/status/menu switches remain.
Both default off on launch; `--teacher` explicitly starts sharing, still read-only.
Neither is persisted. Connect to Force through the normal viewer UI.

Configure the MCP client to launch the binary using absolute paths:

```json
{
  "mcpServers": {
    "force-teacher": {
      "command": "/absolute/project/target/release/mpc-mcp",
      "args": [
        "--manual", "/absolute/project/manuals/Force - User Guide - v3.10.1.pdf"
      ]
    }
  }
}
```

Both processes default to a private `mpc-teacher-<uid>/viewer.sock` directory under
the OS temporary directory. `MPC_TEACHER_SOCKET` can select an absolute custom socket;
its existing parent must be owned by the current user with no group/other access.
An active viewer socket is never replaced. After an abnormal exit, only a verified
same-user stale socket may be removed. `mpc-mcp --help` does not import the PDF.

### Pi configuration

Pi reads project servers from `.pi/mcp.json` after project trust is granted, or
user-level servers from `~/.pi/agent/mcp.json`. For this twelve-tool server, use
`"exposure": "direct"` so screenshots, highlights and gesture tools are available
without tool search. Direct exposure does not grant device control. From the project root, after building the binaries:

```sh
pi mcp add --local force-teacher --exposure direct --cwd "$PWD" \
  --description 'Force teacher: cited manual, screen observations/highlights; the Allow assistance toolbar switch beside the MCP robot globally authorizes bounded input. No audio tools.' \
  -- "$PWD/target/release/mpc-mcp" \
  --manual "$PWD/manuals/Force - User Guide - v3.10.1.pdf"
```

For a development build, substitute `target/debug/mpc-mcp` and first run
`cargo build --locked --features mcp --bin mpc-mcp`. Do not use a nonexistent
release binary or put a shell command string in the `command` field.

Start Pi in this project and approve project trust if prompted, or use `/trust`
to grant it deliberately. After adding/changing the configuration, use `/reload`
or restart Pi. `/mcp` should list `force-teacher` and its twelve tools; after the
project is trusted, `pi mcp list` also validates the connection. An untrusted
project's `.pi/mcp.json` is intentionally ignored. No device password belongs in
this config, and starting the MCP executable does not enable viewer sharing.

The manual PDF and generated machine-specific `.pi/mcp.json` stay local and are
ignored by Git. The JSON example above uses placeholders rather than committing
one developer's executable/manual paths. The live session confirmed that Pi loaded
the configured server after restart; it does not verify every Pi/client version.
