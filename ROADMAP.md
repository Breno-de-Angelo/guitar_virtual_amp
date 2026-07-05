# Roadmap: Guitar Virtual Amp → Full Pedal Effects Platform

Goal: turn this repo into a low-latency, cross-platform (desktop + Android, eventually iOS)
guitar effects processor with a large pedal/preset library and, eventually, a social layer
for sharing presets tied to songs/artists. This document breaks that goal into phases and
tasks sized for delegating to parallel coding agents.

Status snapshot as of 2026-07-04 (see CLAUDE.md for full architecture):
- Mono, sample-at-a-time `i16` pipeline (plus an opt-in `MultiChannelPedalChain` path for
  stereo/multi-channel devices). Input/output are two cpal streams bridged by
  `crossbeam::bounded` channels; `rtrb` was investigated as a lock-free replacement and
  rejected for now (see `backend/capture.rs` module doc), with configured round-trip
  latency measured and exposed via `AudioSetup::configured_latency_secs`.
- 17 pedals: Amp (Clean/Crunch/Lead voicings + bass/mid/treble tone stack), Delay, Reverb,
  LowPass, Flanger, WahWah, Distortion, Noise Gate, Compressor, Tremolo, Chorus, EQ,
  Phaser, Octaver, Pitch Shifter, Looper, and Cabinet/IR (partitioned FFT convolution via
  `backend/dsp/convolution.rs`, `.wav` IR loading via `hound`). Tuner (YIN pitch detection,
  `frontend/lib/pitch.rs`) and metronome/tap-tempo (`frontend/lib/metronome.rs`) utilities
  ship as UI panels alongside the oscilloscope/FFT view, each with a BPM-sync button that
  writes computed rate/delay values into any Delay/Tremolo pedals already in the chain.
- Full serde support on `PedalDescription`/params, a `Preset` file format
  (`shared/preset.rs`) with save/load/list/delete/rename, a 20-entry factory preset pack
  (`guitar_core/assets/presets/factory/`), optional `song`/`artist` metadata on presets
  (the seam Phase 5's social layer will build on), and import/export via `rfd` native
  dialogs (desktop) / manual path entry (Android, where `rfd` has no backend).
- CI (`.github/workflows/ci.yml`) runs build/test/clippy/fmt. Android targets both
  `armv7-linux-androideabi` and `aarch64-linux-android`.
- No MIDI / footswitch / external controller support.
- No backend service, no accounts, nothing network-facing at all.
- Phase 3 (hardware-like UX polish) is done: per-pedal bypass (`PedalInstance::enabled`,
  a bypass LED/footswitch toggle in `frontend/ui/pedals.rs`, honored by `PedalChain::process_sample`
  skipping disabled pedals without dropping them from the chain), a live "buffer: Xms, CPU: Y%"
  latency/CPU overlay in the side panel driven by `AudioSetup::configured_latency_secs` /
  `last_output_callback_ns`, drag-to-reorder pedals via a `☰` handle, and settings persistence
  (`shared/settings.rs`, `~/.config/guitar_virtual_amp/settings.json`) for last-used input/output
  device and last-loaded preset; window size/position persistence comes from enabling eframe's
  `persistence` feature rather than app code. A real-time budget check
  (`guitar_desktop/src/bench_pedal_chain.rs`, run via `cargo run --release -p guitar_desktop --bin
  bench_pedal_chain`) shows the DSP hot path itself is nowhere near the bottleneck: a chain with
  all 17 pedal types stacked costs ~430ns/sample against a 20.8us budget at 48kHz (~98% headroom).
  The real latency knob is the host callback buffer size, which used to be a fixed
  `GLOBAL_CONFIG.buffer_size` (1024 frames) never negotiated with the device; it's now tunable
  live from a "Buffer:" selector in the UI (`shared::config::BUFFER_SIZE_OPTIONS_FRAMES`, 32-4096
  frames), clamped to what the device actually reports supporting
  (`backend::capture::resolve_stream_config`), rebuilding both streams in the background and
  persisting the choice via `AppSettings::buffer_frames`.

Because of this, "vast preset library" and "social sharing" are not incremental features —
they require a backend service and client networking layer that don't exist yet. The
roadmap sequences foundation → content → distribution → platform so agents can work in
parallel without blocking on each other.

---

## How to use this file with agents

Each task below is scoped to be handed to one agent with roughly this framing:
"Read CLAUDE.md and ROADMAP.md section X.Y. Implement <task>. Touch only <files>.
Acceptance criteria: <criteria>." Tasks inside the same phase marked **[P]** (parallel)
touch disjoint files/crates and can be dispatched simultaneously. Tasks marked **[S]**
(sequential) depend on a previous task's output and must wait.

Recommended dispatch pattern: one agent per **[P]** task via the `Agent` tool with
`isolation: worktree` so parallel agents don't collide on the working tree, then review
and merge each before starting tasks that depend on it.

---

## Phase 4 — Style Guide & Mobile-Friendly UI Redesign

The current layout (`frontend/gui.rs`) is a single fixed `SidePanel` + `CentralPanel`
carrying every control at once, with the oscilloscope and FFT plots alone consuming
roughly half the vertical space — workable on a wide desktop window, cramped to the point
of unusable on a phone-sized Android screen. This phase is a deliberate UX/visual reset
before Phase 5 adds a whole new social-platform surface (browse/share/profile pages) on
top of it: better to fix the foundation once than build three more pages on the current
layout and redo them all later.

### 4.1 [S] Style guide / design system
- Produce a short design doc (`docs/style-guide.md` or similar) fixing: color palette
  (dark/light), typography scale, spacing/padding constants, an `egui::Style`/`Visuals`
  configuration to apply app-wide, and iconography conventions (bypass LED color, active
  vs. inactive states, etc). This is a genuine design decision, not just an implementation
  task — flag it to the user for sign-off on direction before 4.2+ build against it, since
  every other task in this phase depends on the choices made here.
- Acceptance: a committed style doc plus a reusable `frontend::style` module (theme
  constants / `egui::Style` builder) that 4.2–4.6 import instead of hardcoding colors.

### 4.2 [P, depends on 4.1] Multi-page navigation shell
- Replace the single side-panel-plus-central-panel layout with page-based navigation
  (e.g. bottom tab bar or nav rail: "Live" / "Chain" / "Tuner & Metronome" / "Presets" /
  "Settings") so no single screen has to cram the pedal chain, oscilloscope, FFT, tuner,
  metronome, and device pickers in at once. This is the main mobile-friendliness fix.
- Must degrade gracefully to a wide desktop layout too (e.g. nav rail on the side instead
  of a bottom bar past some width threshold), not just target phone aspect ratios.

### 4.3 [P, depends on 4.1] Collapsible/dedicated oscilloscope + FFT view
- Move the oscilloscope/FFT plots off the always-visible main screen and onto their own
  page (or a collapsible panel), reusing `frontend::lib::fft::compute_fft` and the existing
  sample buffer unchanged — this is a layout change, not a DSP change. Frees up the bulk of
  a phone screen for the pedal chain during normal play.

### 4.4 [P, depends on 4.1] Custom pedal visual widget
- Replace the current `ui.group()`-per-pedal box rendering (`frontend/ui/pedals.rs`) with a
  custom-painted stompbox-style widget (via `egui::Painter`): pedal-shaped body, a visible
  bypass LED/footswitch graphic, and knob-style controls in place of plain sliders where it
  reads better at a glance. Keep `render_pedal_ui`'s existing per-pedal parameter logic and
  `PedalAction` contract — this task is purely the visual layer around it.

### 4.5 [P, depends on 4.1] Touch-friendly control sizing
- Audit tap target sizes (knobs, buttons, sliders) against Android touch-target guidelines
  (~48dp minimum) and adjust `frontend/ui` widget sizing accordingly; verify on an Android
  emulator or device at a real phone resolution, not just desktop with a resized window.

### 4.6 [P, depends on 4.2] MIDI / footswitch control
- *(Moved from the old Phase 3.1 — a natural fit once the "Live" page and pedal visuals
  from 4.2/4.4 exist to represent footswitch state.)* Bind MIDI CC/PC messages (via
  `midir` crate) to pedal bypass toggles and preset switching, so a physical MIDI
  footswitch can drive the app hands-free like a real pedalboard.

Acceptance for the phase as a whole: a working build (desktop + Android) showing the new
navigation, pedal visuals, and layout; manual verification on a phone-sized viewport (real
device/emulator, not just a narrowed desktop window) that the oscilloscope/FFT no longer
dominates the screen by default.

---

## Phase 5 — Social Platform (Preset Sharing)

This phase requires a real backend service and is the largest scope increase in the
project. Backend and client work can proceed in parallel once the API contract (5.1) is fixed.

### 5.1 [S] API contract design
- Decide and document (new `docs/api.md` or similar): auth model (email/password vs.
  OAuth), preset upload/download endpoints, search/browse by song/artist/genre/tags
  (the `Preset.song`/`Preset.artist`/`Preset.tags` fields already exist for this), rating/
  like counts, comments. This single design doc unblocks 5.2 and 5.3 to run in parallel
  against a shared contract.
- This is a genuine product decision (hosting cost, moderation policy for user-uploaded
  content, whether accounts are required to browse vs. only to upload) — flag to the user
  for a go/no-go and scope call before agents build against it.

### 5.2 [P, depends on 5.1] Backend service
- New top-level crate or separate repo/service (recommend `axum` + `sqlx`/Postgres, kept
  outside the `guitar_core` workspace since it has nothing to do with real-time audio).
  Implements the 5.1 contract: accounts, preset CRUD, search, ratings/comments.
- Needs basic content moderation (reporting, rate limiting on uploads) before any public
  launch — flag as a hard requirement, not a nice-to-have, given user-generated content.

### 5.3 [P, depends on 5.1] Client networking layer
- New module in `guitar_core` (e.g. `guitar_core/src/backend/api_client.rs`) using a
  minimal HTTP client (`ureq` or `reqwest` — prefer `ureq` for smaller dependency
  footprint on Android). Wraps the 5.1 endpoints; UI work (5.4) builds on this.
- Must run network calls off the UI thread (same background-thread pattern already used
  for device switching in `capture.rs`) so browsing/searching never blocks the egui frame
  loop or, worse, the audio callback.

### 5.4 [S, depends on 5.3] Browse/share UI
- New `guitar_core/src/frontend/ui/community.rs` — browse presets by song/artist/tag,
  preview/load one into the live chain, upload the current chain as a shared preset
  (reusing the existing `song`/`artist` metadata), like/rate. Should land as a new page in
  the Phase 4 navigation shell rather than bolted onto the old single-screen layout.

### 5.5 [P] Account/profile UI
- Sign up/login/logout, view your uploaded presets. Can be built in parallel with 5.4
  against the same 5.3 client layer.

---

## Phase 6 — Launch Readiness

Final phase; mostly sequential since it's packaging/release engineering, not feature work.

- **6.1 [P] Desktop packaging** — installers/bundles for Windows (`.msi`/`.exe` via
  `cargo-wix` or similar), macOS (`.app` + notarization), Linux (AppImage or distro packages).
- **6.2 [P] Google Play listing + signing pipeline** — production keystore (distinct from
  the checked-in debug keystore in `guitar_android/Cargo.toml` — **do not ship the debug
  key to production**), Play Console listing, permissions review (RECORD_AUDIO is already
  declared).
- **6.3 [P] Crash reporting / telemetry (opt-in)** — minimal crash reporter
  (e.g. `sentry` crate) gated behind explicit user opt-in, given this handles live audio
  from a personal instrument/mic.
- **6.4 [S] iOS feasibility spike** — eframe/egui iOS support is not first-class; this
  needs a dedicated research spike (not a build task) to decide whether iOS ships via
  egui's experimental iOS backend, a from-scratch SwiftUI shell calling into `guitar_core`
  via FFI, or is deferred. Flag to the user as a decision point before committing agent
  time to a full iOS port.
- **6.5 [P] Docs & marketing site** — landing page, pedal/preset showcase, download links.

---

## Suggested parallel dispatch order

1. Phase 3 is done (see status snapshot above).
2. Phase 4 starts with the 4.1 style-guide sign-off (a design decision, flag to the user),
   then 4.2–4.6 can run in parallel against it.
3. Phase 5 is a deliberate go/no-go checkpoint with the user (backend hosting, moderation,
   accounts are real product/cost decisions) — don't auto-dispatch 5.2+ without that
   conversation. Sequence it after Phase 4 so there's a UI worth adding social pages to.
4. Phase 6 starts once there's a build worth shipping; 6.4 (iOS) is a research spike to
   schedule early since it may change client architecture decisions retroactively.
