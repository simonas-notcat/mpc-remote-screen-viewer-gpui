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
viewer-owned Teacher observation/approval controller
    | shared Session snapshot/framebuffer + bounded, locally approved plans
existing Session worker --- device protocol --- Force
    |
GPUI viewer + local-only teaching overlays
```

Use one device connection. The viewer hosts the observation/approval controller;
the MCP executable attaches through local IPC. Manual lookup works even when the
viewer is closed. Later a headless host can implement the same controller contract.
Neither GPUI nor an audio output device is a dependency of the MCP executable.
Existing socket ownership, framing, authentication, ACKs and audio decoding remain
in the current session worker.

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

Phase 1 implements a labeled, high-contrast teaching rectangle and a persistent
"AI teacher · read only" indicator with a local disable control (including in
fullscreen). Highlights expire, can be explicitly cleared, and disappear on
revocation, disconnect, source replacement or screen reconfiguration. Their label
explicitly identifies them as agent guidance, not actual MPC UI.

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
phase-1 tools remain available; six additional tools **propose** bounded input or
request cancellation. Sharing still starts **read only**, even when the viewer's
ordinary remote-control preference is on. No MCP/IPC operation grants a lease or
approves a proposal. `confirmed: true` and unknown arguments are rejected.

### Tools and bounds

Every proposal requires `observation_id` from a fresh `mpc_get_screen`. Targets and
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

### Local authorization and lifecycle

1. Open **Settings → AI Teacher**. Enable **AI teacher sharing**, then switch on
   **Allow assisted proposals · 60 s**. Close Settings to review/approve proposals
   in the teacher panel. The panel's quick-enable button is an alternative. Both
   controls use the same local grant/revoke logic and reflect the live lease, not a
   saved preference: the switch turns off on expiry/revocation and never auto-renews.
   Sharing must be enabled and a supported device must be streaming; release local
   held inputs first. This grants only a source-bound lease, not approval of input.
2. Inspect a new screen and propose **one** gesture. The tool returns
   `pending_approval`, `queued: false`, and `device_input_sent: false`. It never
   waits for approval or automatically executes/retries the proposal.
3. The viewer shows the immutable proposal's ID, generated action summary and risk
   warning. Choose **Approve this ONE gesture** or **Reject** locally. Approval is
   required for **every** gesture, including delete/overwrite/format/project
   replacement/record/transport and unknown workflows. Text/key contents are not
   displayed or echoed in feedback; text contents must not contain secrets.
4. Poll `mpc_get_state.assisted_control.action`: `pending_approval` → `queued` →
   `running` → `completed`, `cancelled`, `rejected`, or `uncertain`.
   `written_steps` changes only after successful socket writes; `release_written`
   means only that a release was written. Neither is a device acknowledgement or
   visual verification (`visually_verified` remains false).
5. After a terminal result, capture a **new** screen and verify the intended change.
   All pre-proposal and in-gesture observations are rejected for later input.
   Animated pixels alone do not invalidate observations. Do not queue sequences or
   automatically retry mutations, including after an IPC timeout or partial write.

Proposal approval/queue time is limited to **20 seconds**; worker execution has a
**5-second** wall-clock deadline, capped by lease/observation expiry. Approval also
expires no later than the referenced observation's TTL. The worker schedules
bounded steps alongside protocol traffic and checks cancellation even while waiting
for a framebuffer reader. Completion/cancellation writes release-all on the original
usable connection. Failed/partial writes stop the attempt, revoke the lease, and
report an uncertain outcome; best-effort cleanup cannot guarantee release on a
broken connection. A write already in flight when Stop is pressed cannot be recalled.

The GUI-independent `control.rs` layer uses the viewer's existing `input.rs` wire
constructors. The session worker alone owns the socket and runs gestures. It
rechecks source/capabilities before input and releases/cancels the agent gesture
**before** sending local viewer input. Local down/up/modifier ownership covers the
whole held gesture, including in-flight writes. Lease expiry, local Stop, F12
(takeover), window deactivation, opening Settings, sharing revocation, disconnect,
reconnect and screen reconfiguration revoke assisted control; it is never replayed
on a new source. Device-side physical takeover cannot be reliably detected.

The panel, approval/reject buttons and **Stop / take over** are available in normal
and fullscreen views. Agent cursor/crosshair, held ring, actual traversed drag path,
endpoint markers, signed scroll arrow and non-sensitive key/text label are painted
in the device canvas, above pixels and below modal UI, with no input hitboxes.
Pending/queued feedback is amber, running cyan, completed green and rejected/
cancelled/uncertain red; terminal graphics expire/fade. Raw captures and recordings
remain overlay-free. Disabling sharing clears guidance and requests release.

The **AI Teacher** Settings tab provides explicit sharing and timed control switches,
including explanations when control is unavailable. Turning the control switch off
remains possible while a lease is active, including pending gestures. Opening
Settings revokes the previous lease; explicitly switching on grants a new one while in the
modal. Close the modal and keep the viewer focused to approve proposals. No input
is approved by opening/closing Settings or rendering the switches.

### Phase-2 validation and remaining checks

Automated synthetic/loopback tests cover read-only defaults, exact local approval,
all five gesture encodings, cancellation during framebuffer contention, local
human takeover/release ordering, source/capability rechecks, deadlines, consumed
observations, crop/resize/all-rotation mapping, bounded paths/text, hidden text,
failed-write uncertainty, revocation, and no replay after reconfiguration. Desktop
unit tests cover the Settings/quick-enable availability gates and live lease/off
state, including sharing disabled, disconnected/unsupported devices, held local
input and pending-gesture cancellation. Stdio MCP tests enumerate all twelve tools,
dispatch the five proposal tools in read-only
mode, and retain manual/image/highlight coverage. No test connects to a real Force.

Desktop+MCP compilation/Clippy and the Rust 1.88 checks do **not** establish live
rendering or device-input correctness. Hands-on approval/stop/fullscreen feedback,
fit/pan/rotation appearance, actual Force gesture effects, cancellation on real
network failures, native macOS behavior and broader hardware/software coverage
remain unverified. Phase-1 live teaching evidence above is not phase-2 acceptance.

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

Start the viewer with `--teacher`, or enable **View → AI Teacher (read only)**
(**… → AI teacher (read only)** on Linux), or **Settings → AI Teacher → AI teacher
sharing**. Sharing/control switches are session-only, not saved preferences.
Connect to Force through the normal viewer UI. The AI-teacher indicator disables
sharing immediately when clicked; the option is not persisted across launches.

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
`"exposure": "direct"` so screenshots, highlights and proposal tools are available
without tool search. Direct exposure does not grant device control. From the project root, after building the binaries:

```sh
pi mcp add --local force-teacher --exposure direct --cwd "$PWD" \
  --description 'Default read-only Force teacher: cited manual, screen observations/highlights; assisted gestures require a local lease and approval every time. No audio tools.' \
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
