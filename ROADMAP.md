# Roadmap: Guitar Virtual Amp → Full Pedal Effects Platform

Goal: turn this repo into a low-latency, cross-platform (desktop + Android, eventually iOS)
guitar effects processor with a large pedal/preset library and, eventually, a social layer
for sharing presets tied to songs/artists. This document breaks that goal into phases and
tasks sized for delegating to parallel coding agents.

Status snapshot as of 2026-11-25 (see CLAUDE.md for full architecture):
- Since the July snapshot below: an 18th pedal (NAM captures, with TONE3000 browsing and
  downloads), the Guitar Pro tab player with synth and backing track, practice mode, tone
  suggestions, the Spotify page, and automatic tablature from audio files or YouTube (Phase 7).
  The app now talks to the network: TONE3000, Spotify and yt-dlp, each from worker threads only.
  There is still no backend service and no account system of our own.
- Transcription research (models, datasets, benchmarks, DGX Spark training) is tracked in
  `docs/transcription_roadmap.md` and `docs/papers/tablature-transcription/`. Estimating a
  recording's guitar tone (amp, cab, effects) is reviewed in `docs/papers/tone-estimation/`.
- CI installs `libasound2-dev` (it had never built on the runner without it) and is clippy-clean;
  `cargo fmt --check` is still off.
- Phases 4 (UI redesign), 5 (social) and 6 (launch) have not started.

July 2026 snapshot:
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

## Phase 7 — Play-along: Spotify, tabs and practice

### Done
- **Practice mode** (`frontend/lib/practice.rs`): per-note right/wrong marks on the tab, accuracy
  and streak, latency compensation, tolerance and strictness knobs (persisted in
  `AppSettings::practice`). Checked offline against real Guitar Pro files rendered with a GM
  SoundFont, clean and through the "Metal High Gain" chain: ~100% for the right notes and
  0-30% for single-note parts shifted a semitone or a fourth (chord parts shifted a fourth
  keep some shared notes and score higher).
- **Tone suggestion** (`shared/tab/tone.rs`): user presets by `song`/`artist`, else a factory
  preset from the track's GM program, name, tuning and palm muting.
- **Spotify page** (`spotify/`, `frontend/ui/spotify.rs`): PKCE sign-in, playlists and tracks,
  play/pause/skip on the user's Spotify Connect device, now-playing with cover art, and tab
  lookup in the user's own Guitar Pro folder (`shared/tab/library.rs`) or a manual link.

### Spotify Developer Policy constraints (read before extending)
The [policy](https://developer.spotify.com/policy) and the player endpoints' policy tags rule
out, unless Spotify grants written permission: synchronizing a recording with visual content
(a tab cursor following Spotify), overlapping Spotify audio with other audio (the synth or the
processed guitar mixed with it inside the app), analysing Spotify content (tone or note
extraction), games (scoring is borderline: keep practice mode independent of Spotify), and
combining Spotify with other services' content (tabs downloaded from a tab site). Dev-mode
apps: Premium owner, 5 allow-listed users, `GET /search` capped at 10 results.

### Future features (noted while building this phase)
- **Spotify <-> tab sync** [decision needed]: blocked by the synchronization policy. Options:
  ask Spotify for permission/extended quota; keep sync for the user's own audio files (works
  today via the backing track); or a manual "start both together" flow with no automatic sync.
- **Onset detection** in practice mode, so a note still ringing from the previous beat doesn't
  count for a re-picked one, plus per-note "early/late by N ms" feedback and a rhythm-only
  score for muted chugs.
- **Dry input tap**: send the pre-chain guitar signal to the UI for practice analysis (reverb,
  delay and chorus tails currently lower the scores; the strictness knob compensates).
- **Automatic latency calibration** (click out, detect it back in) instead of the manual
  "latência extra".
- **Practice loops and speed trainer**: A-B loop of bars, start slow and raise the speed when
  accuracy passes a threshold; practice history per tab and a "hardest bars" heat map.
- **Expressive technique checks**: bends, slides and vibrato (pitch trajectory), harmonics,
  dead/ghost notes (currently skipped).
- **Pre-practice tuning check** with the tuner, warning when the guitar doesn't match the tab's
  tuning (drop D, half step down...).
- **Bass and drum practice**: bass works with a longer window for low notes; drums need
  onset-only judging.
- **Speaker bleed handling** for microphone users (the practiced track is muted in the synth,
  but the rest of the song still reaches the mic).
- **Tone from the tab file**: Guitar Pro 6+ files store RSE amp/effect settings per track;
  mapping them to pedals would give a real per-song chain instead of a GM-program guess.
- **Tone from the recording** [research, started: `guitar_core/src/tone/`, no UI yet]: estimate a
  preset for each guitar part of the user's own audio (separated stem, no DI) by fitting the app's
  chain through `backend::offline::render_at` with gradient-free search, a pseudo-DI resynthesised
  from the notes, and a final match IR. Literature, design and evaluation protocol:
  `docs/papers/tone-estimation/` (Section 10). Spotify audio stays out of scope (policy).
  - Built (step 1 of the paper's plan): pluck-model pseudo-DI fitted on Guitar-TECHS DI, CMA-ES
    per amp voicing over compressor / distortion / amp gain / reverb / input level, distance with
    an EQ allowance, minimum-phase match IR for a final Cabinet pedal, factory-preset baseline.
  - `eval_tone` on 12 EGDB items (isolated guitar, labelled notes, 12 s excerpts, ~15 s per
    estimate on 28 threads). Mean log-band distance of the true DI through each estimate to the
    target, dB (lower is better):
    | target | dry DI | best factory preset | preset + match | search + match, notes | same, true DI |
    |---|---|---|---|---|---|
    | random app chain + cab EQ | 6.60 | 9.29 | 6.64 | 5.30 | 0.66 |
    | EGDB Marshall | 7.52 | 7.55 | 6.84 | 6.49 | 3.11 |
    | EGDB Mesa | 7.94 | 8.90 | 6.52 | 5.83 | 2.67 |
    | EGDB Plexi | 7.61 | 8.16 | 7.63 | 6.26 | 2.32 |
    The search from notes beats every baseline on average and in 7-8 of 12 items per group, but the
    true-DI column shows most of the error is the pseudo-DI (the pluck model is 9.9 dB from real DI
    even with an EQ allowed). Settings are not recovered (mean parameter error 0.41 on a 0-1 scale,
    voicing right 7/12), which is expected when several settings sound alike.
  - Next: a better dry signal (learned DI renderer or effect remover; the player's DI through a
    pre-chain tap), then separated stems, delay/modulation from notes and tempo, a learned initial
    guess, listening tests, and the UI (save as a preset tagged with the song).
- **Tab library**: match by the title/artist inside the file, not only the file name; cache the
  index; Android folder picker (no `rfd` backend there).
- **Spotify extras**: Liked Songs (`GET /me/tracks`, needs `user-library-read`), open the tab
  automatically when the playing track changes, device picker (transfer playback), search.
- **Licensed tab source**: Songsterr and Ultimate Guitar have no public API; a licensed provider
  would have to live outside the Spotify-integrated surface (policy: no combining services).

### Automatic tablature (built: `transcribe/`, `shared/tab/repository.rs`)
- Offline pipeline: separation (BS-RoFormer SW through MSST, `scripts/install_bs_roformer.sh`,
  preferred when installed; else Demucs `htdemucs_6s`, `scripts/install_demucs.sh`; both
  external Python) -> Beat This! beats/downbeats/meter (final0, else small0) -> guitar CRNN
  (GAPS weights, or the local electric fine-tune when chosen) for the guitar, Basic Pitch for
  the bass, all in pure Rust (tract 0.23) -> chord grouping, per-beat 16th/triplet
  quantization, string/fret by the learned fingering tagger (local model, 6 strings) or the
  Viterbi with a learned string prior, tuning -> GP5 + `.gvtab.json` sidecar in the local tab
  repository. GUI on the Tabs and Spotify pages, batch CLI `transcribe`. The guitar CRNN and
  Beat This! are optional downloads (`scripts/install_transcription_models.sh`); without them
  the pipeline uses Basic Pitch and the DSP beat tracker in 4/4. The table below predates
  BS-RoFormer, final0 and the learned fingering (their numbers are under "Future work on
  transcription" and in `docs/transcription_roadmap.md`). Tab catalogs
  (Songsterr, Ultimate Guitar) stay link-only: their terms forbid pulling their content into
  the app.
- Measured (`transcribe/eval.rs`; note F1 = onset within 80 ms + pitch, on the final tab):
  | | before | Basic Pitch + new rhythm | guitar CRNN + new rhythm |
  |---|---|---|---|
  | GuitarSet, 60 excerpts: F1 median | 0.70 | 0.73 | 0.83 (model alone 0.84 at 50 ms) |
  | GuitarSet: tempo right / bar lines aligned | 44/60, 4/60 | 59/60, 56/60 | 59/60, 56/60 |
  | GP renders, isolated guitar: F1 median | 0.82 | 0.81 | 0.85 |
  | GP renders, full mix without Demucs | 0.52 | 0.52 | 0.68 |
  Fingering alone (true notes, share on the human's string): GuitarSet players 03-05, never
  used for fitting, 65.9% -> 75.4%; 10 human Guitar Pro tabs 58.5% -> 65.4% (the weights were
  fitted on those tabs + GuitarSet 00-02, and the string prior comes from GuitarSet 00-02, so
  the `eval_guitarset` string/fret median, 71% -> 90%, is optimistic). The guitar CRNN
  (trained on classical guitar) makes octave errors on very low, heavily distorted riffs: the
  8-string SoundFont render of "Meshuggah - Bleed" drops from 0.24 to 0.02. A 4-minute song
  without Demucs takes ~48 s (before: 42 s, mostly in the old per-sample resampler, now
  polyphase) and ~2.3 GB peak: each CRNN window in flight takes ~600 MB in tract, so it runs on
  at most 4 threads.
- Sources: Riley et al., "GAPS" (ISMIR 2024) and "High Resolution Guitar Transcription via
  Domain Adaptation" (ICASSP 2024); Kong et al., high-resolution piano transcription (2021);
  Foscarin et al., "Beat This!" (ISMIR 2024); TART (2025, onset clustering, 30 ms minimum note,
  16th quantization); Fretting-Transformer (2025) and MIDI-to-Tab (ISMIR 2024) for fingering
  metrics; spotify/basic-pitch#190 for the Basic Pitch frame hop.
- Techniques, meter changes, several guitars (built: `transcribe/techniques.rs`,
  `beat_net::decode_downbeats`, `transcribe/stereo.rs`):
  - Bends (with release), vibrato, slides and hammer-ons/pull-offs from Basic Pitch's contour
    output on single-note lines (rules after Chen et al., ISMIR 2015, and TENT, TISMIR 2019;
    split bends merged back), exported to GP5. Checked on synthetic tones only: GuitarSet has
    19 annotated glides in the 60-excerpt set, too few to score. GuitarSet F1 unchanged (0.83);
    on the crunch render, techniques on or off give the same 0.76.
  - Bars of 2-7 beats from Beat This!'s downbeats by a bar-pointer Viterbi, so the meter can
    change mid-song: GuitarSet bar lines aligned 56/60 -> 57/60.
  - Pan split of the Demucs guitar stem (ADRess-style soft masks), parts that play the same
    notes merged: on GuitarSet rhythm + lead stereo mixes (`eval_guitar_pairs`, 11 mixes per
    layout), rhythm hard left + lead hard right gives 2 tracks in 10/11, rhythm F1 0.59 -> 0.71,
    lead 0.40 -> 0.85; rhythm left + lead center 11/11, 0.55 -> 0.74 and 0.43 -> 0.88. A single
    guitar half-panned or with a 300 ms stereo echo stays one track in 11/11, F1 unchanged.
  - Distortion benchmark (`GVA_EVAL_TONE`): GuitarSet through the factory presets. Clean 0.83,
    "Blues Crunch" 0.76, "Metal High Gain" 0.24 (Basic Pitch 0.32), with octave errors
    3.8% -> 8.1% of notes, almost all an octave low. A rule raising notes struck with a twelfth
    above them (distortion's difference tone) made clean and crunch worse and metal no better,
    so it was dropped: this needs a model trained on distorted guitar.
  - Real-recording benchmark (`eval_real`) is written but unmeasured: no GP8 files with
    embedded audio on hand.
- Real data and an electric-guitar model (built: `scripts/train/`, `eval_manifest`,
  `backend/offline.rs`, `render_tones`): EGDB, Guitar-TECHS, IDMT-SMT-Guitar, GOAT, GuitarSet
  and MUSDB18 backings turned into labelled benchmarks, label latency removed per recording
  (`align_labels.py`: Guitar-TECHS 25-40 ms late, IDMT ~30 ms early, GOAT up to ±40 ms), and
  the GAPS CRNN fine-tuned on electric guitar in many tones (4000 steps, RTX 4070 Laptop, ~1.5
  h). On held-out data: metal render of GuitarSet 0.29 -> 0.63, EGDB Plexi 0.67 -> 0.81, GOAT
  amp tones ~0.50 -> ~0.72, clean GuitarSet 0.87 -> 0.88; Guitar-TECHS excerpts 0.02 lower and
  their band mixes 0.02-0.08 lower. Optional via `GVA_GUITAR_MODEL` until EGDB's license is
  clear. Techniques measured on real labels: vibrato ~20-40% of labelled notes found, bends
  35-56%, slides ~30% (often marked as legato), false alarms on plain notes 0-4%.
- YouTube as audio source (built: `transcribe/youtube.rs`): search or link, ranked
  preselection (duration from Spotify), download with yt-dlp into a private cache, then the
  same pipeline. YouTube's ToS forbid downloading without permission, and Google Play bans
  apps that download from YouTube: keep it out of the Android build/store listing, and
  revisit before any public release.

### Future work on transcription
- **YouTube**: batch from a Spotify playlist (search + transcribe every track missing a
  tab); show the match confidence and let the user confirm before downloading in batch
  mode; trim intros/outros of music videos against the Spotify duration.
- **Real band-recording benchmark**: the tab-to-audio aligner is built
  (`scripts/train/align_tabs.py`); collect 30-50 song + human tab pairs and hand-check them, or
  GP8 files with embedded audio for `eval_real`. GOAT (DI electric guitar with
  technique-annotated tabs, ISMIR 2025, non-commercial) is on hand and is now the technique
  benchmark.
- **Rhythm**: compound meters are written as 2/4 or 6/4 with triplets, not 6/8; 16th-note
  triplets and 32nds.
- **Techniques**: palm mute and harmonics; tune the hammer-on attack threshold and the bend
  rules on GOAT / Guitar-TECHS; a learned technique classifier (TART, 2026) if rules plateau.
- **Several guitar tracks**: guitars in the same spot of the mix stay one track (needs a
  separator for multiple guitars); BS-RoFormer SW (MSST) is now the preferred separator
  (checkpoint license unknown: local use only; find a licensed RoFormer guitar checkpoint before
  shipping one);
  **drums** (onset classification on the drum stem) and **vocal melody** tracks.
- **Fingering**: a learned tagger (`transcribe/fingering_net.rs`, a local model file) is now the
  default for 6-string guitars when installed: true-note string accuracy GuitarSet solo 0.55 ->
  0.62, EGDB 0.68 -> 0.81; final-tab string/fret on GuitarSet 03-05 68% -> 78% (GAPS notes).
  It is trained on EGDB, so it can't ship until EGDB's authors allow it or it is retrained
  without EGDB. Shipped builds still use the Viterbi, whose string prior and weights come from
  GuitarSet + 10 tabs. Next: more players (DadaGP pre-training, research access on request),
  then audio conditioning (TART's AudioFret reports +4.8 points from timbre); MIDI-to-Tab and
  Fretting-Transformer have no released weights.
- **Electric-guitar model**: it is already a local user option (`guitar_electric_crnn.onnx`;
  Metal High Gain final-tab F1 0.69 vs 0.43 for GAPS). Ship it (EGDB license, or retrain without EGDB on Guitar-TECHS +
  GuitarSet renders), add NAM-capture renders, and train on band mixes (Demucs guitar stems of
  labelled mixes) to fix the band-mix regression on Guitar-TECHS. A GOAT-trained research
  model can't ship (non-commercial).
- **Native Demucs** (ONNX via `ort` or demucs.cpp) so separation needs no Python and could
  run on Android; today Android can only transcribe from the mix.
- **Tab editor** to fix transcriptions in the app, and **repository sync**: share tabs by
  audio hash + Spotify track ID, with per-tab provenance already in the sidecar.

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
