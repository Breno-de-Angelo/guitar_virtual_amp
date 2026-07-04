# Roadmap: Guitar Virtual Amp → Full Pedal Effects Platform

Goal: turn this repo into a low-latency, cross-platform (desktop + Android, eventually iOS)
guitar effects processor with a large pedal/preset library and, eventually, a social layer
for sharing presets tied to songs/artists. This document breaks that goal into phases and
tasks sized for delegating to parallel coding agents.

Status snapshot as of 2026-07-04 (see CLAUDE.md for full architecture):
- Mono, sample-at-a-time `i16` pipeline. Input/output are two cpal streams bridged by a
  crossbeam channel, one message *per sample* (48k msgs/sec each direction).
- 7 pedals (Amp, Delay, Reverb, LowPass, Flanger, WahWah, Distortion), no cabinet/IR sim,
  no EQ, no compressor, no noise gate, no tuner.
- Zero persistence: no serde anywhere, no save/load, no presets, no settings file. Device
  choice is not remembered across restarts.
- No MIDI / footswitch / external controller support.
- Android target is armv7 only, min SDK 26; no arm64-v8a (most phones since ~2017 are
  arm64 — armv7-only will fail or run poorly on newer devices).
- No CI, one test file (`ring_buffer.rs`).
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

## Phase 0 — Foundation Hardening (blocks almost everything else)

Nothing else scales until presets can be serialized and the audio path is proven low-latency.
Do this phase mostly before Phase 2+, but Phase 0 tasks are themselves parallelizable.

### 0.1 [P] Add serde to all pedal params and `PedalDescription`
- Files: `guitar_core/src/shared/pedals.rs`, `guitar_core/Cargo.toml`
- Add `serde` (with `derive`) as a dependency. Derive `Serialize, Deserialize` on every
  `*Params` struct and on `PedalDescription`. Keep `Copy, Clone`.
- Acceptance: `cargo test --workspace --exclude guitar_android` passes; a round-trip
  serde_json test (serialize a `Vec<PedalDescription>` chain, deserialize, compare) exists
  in `guitar_core/src/shared/pedals.rs` under `#[cfg(test)]`.

### 0.2 [S, depends on 0.1] Preset file format + load/save module
- New file: `guitar_core/src/shared/preset.rs`
- Define `Preset { name: String, author: Option<String>, pedals: Vec<PedalDescription>, tags: Vec<String> }`.
- `save_to_file(path) -> Result<...>` / `load_from_file(path) -> Result<...>` using
  serde_json (human-diffable, easy to hand-author factory presets and ship as static assets).
- Add a `dirs` crate dependency to resolve a per-platform user preset directory
  (e.g. `~/.local/share/guitar_virtual_amp/presets/` on Linux, app-specific dir on Android
  via a path injected at startup since Android has no `$HOME`).
- Acceptance: unit tests for save/load round-trip; handles missing directory (creates it).

### 0.3 [P] Audio path latency/architecture audit + fix
- File: `guitar_core/src/backend/capture.rs`
- Current design sends one crossbeam message per sample in both directions and rebuilds
  the whole heap-allocated `PedalChain` (with `Box<dyn Pedal>` per pedal) inside the
  realtime output callback whenever `pedal_rx` has a pending message — the allocation
  itself is fine (happens off the hot per-sample path) but confirm rebuild + `try_recv`
  overhead per sample doesn't cause underruns at small buffer sizes.
- Investigate switching to a lock-free ring buffer (e.g. `rtrb` crate, single-producer
  single-consumer) instead of `crossbeam::bounded` for the input→output audio path —
  crossbeam channels are not guaranteed wait-free and can allocate/block under contention,
  which risks audible glitches versus dedicated hardware.
- Add a debug/perf overlay: measure round-trip latency (buffer_size / sample_rate) and
  actual callback duration, surfaced in the UI (ties into 1.x tuner/meter work).
- Acceptance: document findings in `guitar_core/src/backend/capture.rs` module doc;
  if `rtrb` swap is justified, implement it behind the same public API
  (`AudioSetup`, `start_audio_processing`) so no callers change.

### 0.4 [P] CI pipeline
- New file: `.github/workflows/ci.yml`
- Run `cargo build --workspace --exclude guitar_android`,
  `cargo test --workspace --exclude guitar_android`, `cargo clippy --workspace --exclude guitar_android -- -D warnings`,
  `cargo fmt --check` on push/PR. Cache cargo registry/target.
- Acceptance: workflow passes on a clean clone.

### 0.5 [P] Android arm64 target
- File: `guitar_android/Cargo.toml`
- Add `arm64-v8a` (`aarch64-linux-android`) to `build_targets` alongside armv7. Verify
  `cargo apk build -p guitar_android` produces both ABIs in one APK/AAB.
- Acceptance: build succeeds for both targets; note any per-ABI toolchain issues in a
  code comment or this roadmap if arm64 needs different NDK config.

### 0.6 [P] Stereo I/O support
- Files: `guitar_core/src/backend/capture.rs`, `guitar_core/src/backend/pedals/pedal.rs`
- Currently only the first channel of each input frame is read and the same mono sample
  is written to all output channels. Many audio interfaces are stereo (e.g. one input =
  guitar, one = mic/aux) and stereo effects (ping-pong delay, real reverb) need it
  eventually. Scope this task narrowly: make channel count configurable and pipe through,
  without yet implementing stereo-specific DSP (that's Phase 1).
- Acceptance: mono chain still works unchanged (default); a stereo passthrough test proves
  both channels are captured/played independently when a stereo device is selected.

---

## Phase 1 — DSP & Pedal Library Expansion

Parallelizable once 0.1 (serde on params) lands, since each new pedal follows the
"Adding a new pedal" recipe in CLAUDE.md (touch `shared/pedals.rs`, `backend/pedals/<name>.rs`,
`backend/pedals/mod.rs`, `backend/pedals/pedal.rs` match arm, `frontend/ui/pedals.rs`).
Each pedal below is an independent **[P]** task — one agent per pedal, but all touch the
same 4-5 shared files' match arms, so either serialize these or have agents patch in
sequence / rebase to avoid merge conflicts. Recommend batches of 2-3 in parallel with a
merge step between batches, or one agent doing all pedals in this phase sequentially in a
single worktree.

- **1.1 Noise Gate** — essential for high-gain tones on real amps/hardware; threshold,
  attack, release, hold params.
- **1.2 Compressor** — sustain/dynamics control; threshold, ratio, attack, release, makeup gain.
- **1.3 Parametric/Graphic EQ** — multi-band; needed as its own pedal plus as the tone
  stack for amp sims.
- **1.4 Cabinet/IR (impulse response) simulator** — convolution against loaded `.wav` IR
  files; this is the single biggest lever for sounding like real hardware rather than a
  synth. Needs an FFT-based (partitioned) convolution engine for real-time performance —
  reuse `rustfft` (already a dependency, used by the FFT view) rather than adding a new
  DSP dependency.
- **1.5 Amp sim expansion** — multiple amp voicings (clean/crunch/lead) as presets of an
  extended `Amp` pedal (tone stack + waveshaping stages), not just a gain multiplier.
- **1.6 Chorus, Phaser, Tremolo, Octaver, Pitch shifter** — standard pedalboard staples,
  one per agent.
- **1.7 Looper pedal** — record/overdub/play a loop in a ring buffer; distinct from other
  pedals since it needs transport state (recording/playing/overdubbing) exposed to the UI,
  not just a slider.
- **1.8 Tuner utility (not a chain pedal)** — pitch detection (autocorrelation or YIN) off
  the live sample buffer already available to the FFT view; surfaced as a UI panel, not a
  `Pedal` impl.
- **1.9 Metronome / tap tempo utility** — feeds delay/tremolo rate params so time-based
  effects can sync to a tempo; needed before "vast preset library" presets can specify
  tempo-synced delay times meaningfully.

Acceptance per pedal: unit test on the DSP struct in isolation (given known input, expected
output shape — e.g. gate silences below threshold, IR convolution matches a reference
tail), slider UI wired in `render_pedal_ui`, added to `PedalDescription` `EnumIter` so it
shows in "Add Pedal".

---

## Phase 2 — Preset System & Content

Depends on 0.1 + 0.2 (serde + preset file format).

### 2.1 [S] UI: Save/Load/Rename/Delete preset panel
- File: `guitar_core/src/frontend/gui.rs`, new `guitar_core/src/frontend/ui/presets.rs`
- List presets from the user preset directory (0.2), load one into the live chain
  (push through `pedal_tx` like any other chain mutation), save current chain as a named
  preset, delete/rename.
- Acceptance: manual verification via `/run` skill — save a chain, restart app, reload it.

### 2.2 [P] Factory preset pack — genre/amp starting points
- New directory: `guitar_core/assets/presets/factory/*.json`
- Author ~20-30 presets covering common genres (clean jazz, blues crunch, rock lead,
  metal high-gain, ambient ping-pong delay + reverb, funk wah, etc.) using whatever pedals
  exist at the time. This is content work, not code — can run as soon as 2.1's format is
  fixed, doesn't need to wait on all of Phase 1.
- Acceptance: every factory preset loads without error and produces audibly distinct tone
  (spot check a handful via `/run`).

### 2.3 [P] "Song/artist" preset metadata schema
- Extend `Preset` (2.2's format) with optional `song: Option<String>`, `artist: Option<String>`.
- This is the seam that Phase 4 (social/sharing) will build on — get the schema right now
  even though there's no sharing UI yet, so factory presets and any user-authored ones
  are already taggable.
- Acceptance: schema documented in `preset.rs` doc comment; a couple of factory presets in
  2.2 tagged with a well-known song/artist as an example.

### 2.4 [P] Preset import/export (single file share via filesystem)
- Add "export to file" / "import from file" using the OS file picker (`rfd` crate is the
  standard egui-ecosystem choice) so users can hand a `.json` preset file to another user
  before any network layer exists. This is the cheapest possible version of "sharing" and
  unblocks user value immediately, well before Phase 4's backend.
- Acceptance: export writes a valid preset file readable by 2.1's loader; import round-trips.

---

## Phase 3 — Hardware-Like UX Polish

Independent of Phase 1/2 content work; can run in parallel once Phase 0 lands.

- **3.1 [P] MIDI / footswitch control** — bind MIDI CC/PC messages (via `midir` crate) to
  pedal bypass toggles and preset switching, so a physical MIDI footswitch can drive the
  app hands-free like a real pedalboard. This is a major differentiator for "feels like
  hardware."
- **3.2 [P] Per-pedal bypass (true/soft bypass toggle)** — currently every pedal in the
  chain always processes; add an `enabled: bool` to each pedal/description and a bypass
  button in the UI, without removing the pedal from the chain (matches real pedalboard
  workflow of stomping a switch instead of unplugging a pedal).
- **3.3 [P] Latency/CPU meter overlay** — surfaces the measurements from task 0.3 in the
  UI (a small "buffer: Xms, CPU: Y%" readout), so users/testers can see if they're near
  hardware-competitive latency (~5-10ms round trip is the bar to hit).
- **3.4 [P] Drag-to-reorder pedal chain in UI** — currently pedals are likely added to the
  end of a `Vec`; confirm and add drag handles (egui supports this) so reordering doesn't
  require delete+re-add.
- **3.5 [P] Settings persistence** — remember last-used input/output device, window size,
  and last-loaded preset across restarts (small serde-backed config file, reuses 0.1/0.2
  infrastructure and the `dirs` crate).

---

## Phase 4 — Social Platform (Preset Sharing)

This phase requires a real backend service and is the largest scope increase in the
project — sequence it after Phase 0-2 land so there's something worth sharing. Backend
and client work can proceed in parallel once the API contract (4.1) is fixed.

### 4.1 [S] API contract design
- Decide and document (new `docs/api.md` or similar): auth model (email/password vs.
  OAuth), preset upload/download endpoints, search/browse by song/artist/genre/tags,
  rating/like counts, comments. This single design doc unblocks 4.2 and 4.3 to run in
  parallel against a shared contract.
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
  (reusing 2.3's song/artist metadata), like/rate.

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

1. Kick off **0.1, 0.3, 0.4, 0.5, 0.6** simultaneously (5 agents, disjoint files/concerns).
2. Once 0.1 merges: **0.2** (sequential), and start **Phase 1** pedals in small batches
   (2-3 agents at a time given shared match-arm files).
3. Once 0.2 merges: **2.1** then **2.2/2.3/2.4** in parallel.
4. **Phase 3** tasks can start any time after Phase 0 merges — fully parallel with Phase 1/2.
5. Phase 4 is a deliberate go/no-go checkpoint with the user (backend hosting, moderation,
   accounts are real product/cost decisions) — don't auto-dispatch 4.2+ without that
   conversation.
6. Phase 5 starts once there's a build worth shipping; 5.4 (iOS) is a research spike to
   schedule early since it may change client architecture decisions retroactively.
