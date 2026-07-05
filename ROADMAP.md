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
  `backend/dsp/convolution.rs`, `.wav` IR loading via `hound`). No tuner, no metronome/tap
  tempo yet.
- Full serde support on `PedalDescription`/params, a `Preset` file format
  (`shared/preset.rs`) with save/load/list/delete/rename, a 20-entry factory preset pack
  (`guitar_core/assets/presets/factory/`), optional `song`/`artist` metadata on presets
  (the seam Phase 4's social layer will build on), and import/export via `rfd` native
  dialogs (desktop) / manual path entry (Android, where `rfd` has no backend).
- CI (`.github/workflows/ci.yml`) runs build/test/clippy/fmt. Android targets both
  `armv7-linux-androideabi` and `aarch64-linux-android`.
- No MIDI / footswitch / external controller support.
- No backend service, no accounts, nothing network-facing at all.

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

## Phase 1 — DSP & Pedal Library Expansion (remaining)

Everything in this phase except the two items below has shipped (see status snapshot).
These are the two still-open items, independent **[P]** tasks (of each other and of Phase 3):

- **1.8 Tuner utility (not a chain pedal)** — pitch detection (autocorrelation or YIN) off
  the live sample buffer already available to the FFT view (`frontend/lib/fft.rs`);
  surfaced as a UI panel, not a `Pedal` impl.
- **1.9 Metronome / tap tempo utility** — feeds delay/tremolo rate params so time-based
  effects can sync to a tempo; needed before "vast preset library" presets can specify
  tempo-synced delay times meaningfully.

Acceptance: unit test on the detection/timing logic in isolation (e.g. known sine wave →
correct detected pitch/frequency), UI panel wired in alongside the existing
oscilloscope/FFT view.

---

## Phase 3 — Hardware-Like UX Polish

Independent of Phase 1's remaining items; can run in parallel.

- **3.1 [P] MIDI / footswitch control** — bind MIDI CC/PC messages (via `midir` crate) to
  pedal bypass toggles and preset switching, so a physical MIDI footswitch can drive the
  app hands-free like a real pedalboard. This is a major differentiator for "feels like
  hardware."
- **3.2 [P] Per-pedal bypass (true/soft bypass toggle)** — currently every pedal in the
  chain always processes; add an `enabled: bool` to each pedal/description and a bypass
  button in the UI, without removing the pedal from the chain (matches real pedalboard
  workflow of stomping a switch instead of unplugging a pedal).
- **3.3 [P] Latency/CPU meter overlay** — surfaces the measurements already computed in
  `backend/capture.rs` (`AudioSetup::configured_latency_secs`) in the UI (a small
  "buffer: Xms, CPU: Y%" readout), so users/testers can see if they're near
  hardware-competitive latency (~5-10ms round trip is the bar to hit).
- **3.4 [P] Drag-to-reorder pedal chain in UI** — currently pedals are added to the end of
  a `Vec`; confirm and add drag handles (egui supports this) so reordering doesn't require
  delete+re-add.
- **3.5 [P] Settings persistence** — remember last-used input/output device, window size,
  and last-loaded preset across restarts (small serde-backed config file, reuses the
  existing serde/`Preset`/`dirs` infrastructure).

---

## Phase 4 — Social Platform (Preset Sharing)

This phase requires a real backend service and is the largest scope increase in the
project — sequence it after Phase 1's remaining items land so there's something worth
sharing. Backend and client work can proceed in parallel once the API contract (4.1) is fixed.

### 4.1 [S] API contract design
- Decide and document (new `docs/api.md` or similar): auth model (email/password vs.
  OAuth), preset upload/download endpoints, search/browse by song/artist/genre/tags
  (the `Preset.song`/`Preset.artist`/`Preset.tags` fields already exist for this), rating/
  like counts, comments. This single design doc unblocks 4.2 and 4.3 to run in parallel
  against a shared contract.
- This is a genuine product decision (hosting cost, moderation policy for user-uploaded
  content, whether accounts are required to browse vs. only to upload) — flag to the user
  for a go/no-go and scope call before agents build against it.

### 4.2 [P, depends on 4.1] Backend service
- New top-level crate or separate repo/service (recommend `axum` + `sqlx`/Postgres, kept
  outside the `guitar_core` workspace since it has nothing to do with real-time audio).
  Implements the 4.1 contract: accounts, preset CRUD, search, ratings/comments.
- Needs basic content moderation (reporting, rate limiting on uploads) before any public
  launch — flag as a hard requirement, not a nice-to-have, given user-generated content.

### 4.3 [P, depends on 4.1] Client networking layer
- New module in `guitar_core` (e.g. `guitar_core/src/backend/api_client.rs`) using a
  minimal HTTP client (`ureq` or `reqwest` — prefer `ureq` for smaller dependency
  footprint on Android). Wraps the 4.1 endpoints; UI work (4.4) builds on this.
- Must run network calls off the UI thread (same background-thread pattern already used
  for device switching in `capture.rs`) so browsing/searching never blocks the egui frame
  loop or, worse, the audio callback.

### 4.4 [S, depends on 4.3] Browse/share UI
- New `guitar_core/src/frontend/ui/community.rs` — browse presets by song/artist/tag,
  preview/load one into the live chain, upload the current chain as a shared preset
  (reusing the existing `song`/`artist` metadata), like/rate.

### 4.5 [P] Account/profile UI
- Sign up/login/logout, view your uploaded presets. Can be built in parallel with 4.4
  against the same 4.3 client layer.

---

## Phase 5 — Launch Readiness

Final phase; mostly sequential since it's packaging/release engineering, not feature work.

- **5.1 [P] Desktop packaging** — installers/bundles for Windows (`.msi`/`.exe` via
  `cargo-wix` or similar), macOS (`.app` + notarization), Linux (AppImage or distro packages).
- **5.2 [P] Google Play listing + signing pipeline** — production keystore (distinct from
  the checked-in debug keystore in `guitar_android/Cargo.toml` — **do not ship the debug
  key to production**), Play Console listing, permissions review (RECORD_AUDIO is already
  declared).
- **5.3 [P] Crash reporting / telemetry (opt-in)** — minimal crash reporter
  (e.g. `sentry` crate) gated behind explicit user opt-in, given this handles live audio
  from a personal instrument/mic.
- **5.4 [S] iOS feasibility spike** — eframe/egui iOS support is not first-class; this
  needs a dedicated research spike (not a build task) to decide whether iOS ships via
  egui's experimental iOS backend, a from-scratch SwiftUI shell calling into `guitar_core`
  via FFI, or is deferred. Flag to the user as a decision point before committing agent
  time to a full iOS port.
- **5.5 [P] Docs & marketing site** — landing page, pedal/preset showcase, download links.

---

## Suggested parallel dispatch order

1. Kick off **1.8** (Tuner) and **1.9** (Metronome) together — independent files, no
   shared match-arm contention (neither is a `Pedal`/`PedalDescription`).
2. **Phase 3** tasks can start any time, fully parallel with Phase 1's remaining items.
3. Phase 4 is a deliberate go/no-go checkpoint with the user (backend hosting, moderation,
   accounts are real product/cost decisions) — don't auto-dispatch 4.2+ without that
   conversation.
4. Phase 5 starts once there's a build worth shipping; 5.4 (iOS) is a research spike to
   schedule early since it may change client architecture decisions retroactively.
