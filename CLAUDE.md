# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A real-time guitar effects processor written in Rust. Audio comes in through an input device (guitar interface / mic), runs through a chain of user-configurable DSP pedals, and is played back through an output device with minimal latency. The UI (egui/eframe) shows a live oscilloscope and FFT view alongside pedal controls. Ships to desktop (Linux/Windows/macOS via cpal) and Android (native activity, armv7).

## Workspace layout

Cargo workspace with three members:

- `guitar_core` — all shared logic: audio capture/playback (`backend`), DSP and pedal implementations (`backend/dsp`, `backend/pedals`), the egui UI (`frontend`), and shared config/types (`shared`). Both binaries are thin wrappers around this crate.
- `guitar_desktop` — desktop entry point (`src/main.rs`) plus a manual device-switching test binary (`src/bin`/`src/test_device_switch.rs`, run as `test_device_switch`).
- `guitar_android` — cdylib entry point for Android's `NativeActivity` (`android_main` in `src/lib.rs`), gated behind `#![cfg(target_os = "android")]`.

## Commands

Build/run/test desktop (the common workflow):
```
cargo run -p guitar_desktop                  # run the desktop app
cargo build --workspace --exclude guitar_android
cargo test --workspace --exclude guitar_android
```

**Important:** always run tests/builds at the workspace level (or explicitly include `guitar_desktop`), not with `cargo test -p guitar_core` alone. `guitar_core`'s `eframe` dependency has `default-features = false` (only `glow`+`default_fonts`), so built in isolation, `winit` has no windowing backend (x11/wayland) and fails with `no PlatformIcon in platform_impl`. `guitar_desktop` depends on `eframe` with default features enabled, and Cargo's feature unification pulls that into `guitar_core` when both crates are built together — which is why workspace-level commands work.

Run a single test:
```
cargo test --workspace --exclude guitar_android <test_name>
```

Android build (uses `cargo-apk`, configured via `[package.metadata.android]` in `guitar_android/Cargo.toml`, targeting `armv7-linux-androideabi`, min SDK 26 / target SDK 35):
```
cargo apk build -p guitar_android
```

## Architecture

### Audio pipeline (`guitar_core::backend::capture`)

`start_audio_processing` sets up two cpal streams:
- **Input stream**: reads samples from the default (or user-selected) input device, downmixes to mono (first channel of each frame), converts to `i16`, and pushes onto a bounded crossbeam channel (`input_tx`/`input_rx`).
- **Output stream**: on every callback, drains any pending `PedalDescription` updates from `pedal_rx` and rebuilds the `PedalChain` from scratch (`PedalChain::from_description`), then pulls one sample at a time from `input_rx`, runs it through the chain (`process_sample`), writes it to all output channels, and forwards it to the UI via `audio_tx` for visualization.
- Both I16 and F32 cpal sample formats are handled explicitly on both streams.
- Sample rate / buffer sizing is fixed by `shared::config::GLOBAL_CONFIG` (48kHz, not dynamically negotiated with the device — `find_best_config` just uses the device's default config).

Device switching (desktop UI) tears down the old input stream and spawns a new one on a background thread, communicating back via a one-shot crossbeam channel so the UI thread never blocks on device (re)negotiation.

### Pedal chain (`guitar_core::backend::pedals`)

- `Pedal` trait: `apply_effect(&mut self, input: i16) -> i16` — pedals are stateful, mono, sample-at-a-time processors.
- `PedalChain` holds `Vec<Box<dyn Pedal>>` and is **rebuilt wholesale** from a `Vec<PedalDescription>` any time the chain changes (added/removed/reordered pedal) — there's no in-place mutation of a live pedal's parameters object; a parameter tweak sends the whole updated description vec across the channel and the audio thread reconstructs all pedal instances (each pedal's constructor resets its own internal DSP state, e.g. delay lines, filter memory).
- Each pedal (`amp.rs`, `delay.rs`, `distortion.rs`, `flanger.rs`, `low_pass.rs`, `reverb.rs`, `wah_wah.rs`) is self-contained: owns its `*Params` struct (defined in `shared::pedals`) plus whatever internal DSP state it needs, and implements `Pedal`.
- Generic DSP building blocks (`backend::dsp::all_pass`, `comb_filter`) are used by pedals like `reverb` that need delay-line primitives; `backend::ring_buffer::RingBuffer<const N, T>` is a fixed-capacity ring buffer used for delay lines and is the one module in the codebase with unit tests.

### NAM pedal and TONE3000 (`backend::pedals::nam`, `tone3000`, `frontend::ui::tone3000`)
- `NamPedal` runs `.nam` captures via `nam-rs`. It overrides `Pedal::process_block` so the model runs on the output callback's whole block with no added latency (the output callback calls `PedalChain::process_block`; pedals that don't override it get a per-sample loop). Parsed model files live in a process-wide cache: the UI calls `nam::preload` (via `preload_nam_models` in `gui.rs`) before every `pedal_tx.send`, so the audio thread only builds the network from memory. Knob changes go through `update_in_place`; only a new path rebuilds the pedal.
- `tone3000::Tone3000Client` is a blocking client (ureq): OAuth PKCE with a loopback redirect, device-code flow, token refresh, list/tone/model endpoints and downloads. Only call it from worker threads. The access token is only sent to `https://*.tone3000.com`.
- `Tone3000Panel` (egui window) owns the client and runs every request on a spawned thread, reporting back through a channel. Pedals ask for it by returning `PedalAction::Tone3000(..)`; a finished download comes back from `Tone3000Panel::show` and `apply_loaded_tone` puts it in the chain.
- TONE3000 API terms apply: no bulk download, caching or bundling of catalog content, only the bounded list endpoints (no search) for independent integrations, and creator/license metadata (`Tone3000Source`) must stay with the file.

### Adding a new pedal

Touch four places: add a `*Params` struct + `Default` impl and a new `PedalDescription` variant (with its `name()`) in `shared/pedals.rs`; add the DSP implementation in `backend/pedals/<name>.rs` implementing `Pedal`, and register it in `backend/pedals/mod.rs`; wire the match arm in `PedalChain::from_description` (`backend/pedals/pedal.rs`); add slider UI in `frontend/ui/pedals.rs`'s `render_pedal_ui` match.

### Tab player (`shared::tab`, `backend::song_player`, `frontend::ui::tab_*`)
- `shared::tab::import` reads Guitar Pro files through the `guitarpro` crate and flattens them into `shared::tab::model::Score` (string 1 = highest string for every format; GPIF files number strings from the lowest one and are flipped on import). It works around gaps in guitarpro 0.5.0: GP3-5 instruments are guessed from track names/tuning, and GP7 tunings are re-read from the raw GPIF.
- `shared::tab::timeline` expands repeats/alternate endings into bar occurrences, converts ticks to seconds through the tempo map ("score time"), generates synth note events, and holds `SyncMap` (score time <-> backing-track time).
- `backend::song_player::SongPlayer` is the realtime engine (rustysynth + decoded backing track from `backend::audio_file`). It's an `Arc<Mutex<_>>` that the output callback only `try_lock`s, mixed in after the pedal chain (the visualizer/tuner channel still only gets the guitar). It's passed into `build_output_stream`/`switch_output_device` so it survives stream rebuilds.
- `frontend::ui::tab_view` lays out and paints the tab; `frontend::ui::tab_player::TabPlayerPanel` is the "Tabs" page (file loading on background threads, mixer, sync controls). Per-tab audio path + manual sync, the SoundFont path and the last tab are persisted in `AppSettings`.

### Practice mode and tone suggestions (`frontend::lib::practice`, `shared::tab::tone`)
- `PracticeSession` judges the selected track's notes against the guitar signal on the UI thread. `AudioApp::update` hands every drained `audio_rx` sample (post-chain, guitar only) to `TabPlayerPanel::feed_input`, so the visualizer channel is sized to ~1 s (`gui::VISUALIZER_CHANNEL_SAMPLES`): dropped samples would break the sample-index/score-time anchors. Each beat is analysed with Hann-windowed Goertzel filters on its pitches' harmonics, after `configured_latency_secs` + the user's extra latency, searching ±tolerance. Rules: power share above a strictness threshold, beat semitone neighbors, and (single notes only) the pitches a fourth/fifth away; chords skip the fourth/fifth check because their implied root out-scores the played notes. Results are `NoteKey -> bool` marks drawn by `tab_view`.
- `shared::tab::tone::guess_tone` maps a track (GM program, name, tuning, palm-mute share) to a factory preset name; `preset_fits_song` finds user presets by `song`/`artist`. `TabPlayerPanel::ui` returns the chosen chain and `gui.rs` loads it like a preset.

### Spotify (`spotify`, `frontend::ui::spotify`, `shared::tab::library`)
- `spotify::SpotifyClient` is a blocking ureq client (worker threads only) using Authorization Code + PKCE with the loopback listener/PKCE helpers from `tone3000::client` (`wait_for_callback`, `new_pkce`, `random_token` are `pub(crate)` for this). Endpoints follow the February 2026 Web API changes (`/playlists/{id}/items`, `item` field). 401 refreshes once, 429 honors `Retry-After` (capped), 5xx retries twice.
- `SpotifyPanel` (the "🎧 Spotify" page) lists playlists/tracks, sends play/pause/skip to the user's Spotify Connect device and polls `/me/player` every 5 s while visible. It returns `SpotifyAction::OpenTab`, which `gui.rs` passes to `TabPlayerPanel::open_tab`.
- Tabs come only from the user's files: `shared::tab::library` scans `AppSettings::tab_library_dir` and matches by file name (`LibraryIndex::best_match`); manual links are `AppSettings::spotify_tab_links` (track ID -> path).
- Spotify Developer Policy constraints shape the design: no syncing the tab cursor or synth to Spotify playback, no mixing Spotify audio with other audio, no analysing Spotify content, no combining with other services' content (so no tab downloads), show cover art + metadata + links back to Spotify, and delete account data on disconnect. Check the policy before adding Spotify features.

### Transcription and tab repository (`transcribe`, `shared::tab::{export, repository}`, `frontend::ui::transcribe`)
- `transcribe::run(&Request, progress, cancel)` is the blocking pipeline (worker threads only): decode + SHA-256 -> optional Demucs separation (`separation`, external `demucs` CLI found via `GVA_DEMUCS`, `<data dir>/guitar_virtual_amp/tools/demucs-venv`, or `PATH`; installed by `scripts/install_demucs.sh`) -> rhythm (`track_rhythm`) -> notes per stem (`transcribe_stem`) -> `arrange` -> `repository::save`.
- Optional models live in `<data dir>/guitar_virtual_amp/tools/models/` (`scripts/install_transcription_models.sh`; `transcribe::installed_models()`): `guitar_gaps_crnn.onnx` (Kong-style high-resolution CRNN with the GAPS guitar weights of Riley et al., exported by `scripts/export_guitar_crnn.py`; `GVA_GUITAR_MODEL` overrides the path, `GVA_GUITAR_ENGINE=basic-pitch` forces the fallback) and `beat_this_small0.onnx` + `beat_this_logmel.onnx` (Beat This!; `GVA_BEAT_MODEL_DIR`). Without them the pipeline falls back to Basic Pitch and the DSP tracker.
- Rhythm: `beat_net` (Beat This! on the mix, 50 fps logits in 30 s chunks; peaks, then `regularize` fills missed beats and drops doubled ones; `meter` picks 3/4 or 4/4 and the bar phase from the downbeat logits, 4/4 favored) or `beats` (spectral flux, autocorrelation tempo with a 120 BPM prior, Ellis DP beats, low-band downbeat phase, 4/4 only; on the drum stem when there is one). `beats::bar_grid` extends either to whole bars around the notes.
- Notes: guitar with `guitar_crnn` (16 kHz, 10 s windows with 50% overlap run in parallel by `parallel_map`, Kong's regression post-processing: onset/offset peaks with sub-frame shift, frame threshold, notes < 30 ms dropped) or Basic Pitch; bass always Basic Pitch (`basic_pitch`, ONNX bundled with `include_bytes!`; output 0 = onsets, 1 = notes; frames are 256 samples apart inside each window). `prune_weak` drops weak simultaneous detections (and, for Basic Pitch, overtones). Every model runs through `tract-onnx` 0.23.
- `arrange`: `BeatGrid` (beat times + beats per bar) -> `quantize` (chords = onsets within 35 ms; `choose_subdivisions` picks 16ths or 8th triplets per beat by Viterbi with a straight-time prior; positions are ticks, 960 per beat) -> `fret_events` (Viterbi over fingerings: `FingeringCosts` for stretch, position, high frets, open strings, muted strings inside chords and hand moves, with the hand position carried through open strings, plus the `StringPrior` learned from GuitarSet players 00-02; weights fitted with `eval_fingering`) -> `build_track` (`notate`: tuplets only inside partly used triplet beats, ties for long notes) -> `build_score` (one tempo per bar, numerator = beats per bar).
- The repository is GP5 files (`export::score_to_gp5`, through the `guitarpro` crate's legacy writer; tempos are rounded per bar) plus `.gvtab.json` sidecars (`TabMetadata`): audio hash (dedup/replace), engines, `bar_audio_secs`. `TabPlayerPanel` reads the sidecar on load and plays the source audio with `TabMetadata::sync_map` (exact per-bar sync regardless of GP5 tempo rounding).
- `transcribe::youtube` wraps the external `yt-dlp` (found via `GVA_YTDLP`, `<data dir>/guitar_virtual_amp/tools/ytdlp-venv`, or `PATH`; `scripts/install_ytdlp.sh`): `search` (`ytsearchN:` flat JSON, no API key), `video_info`, `rank` (title/artist words, " - Topic" channels, penalties for live/cover/karaoke..., Spotify duration), `download_audio` (m4a into `youtube_audio/`, reused). Video IDs are validated (11 chars `[A-Za-z0-9_-]`) before reaching the command line; no shell is involved. `transcribe::run_youtube` = download (first 15% of progress) + `run`, recording `source.youtube_id` in the sidecar (`repository::find_by_youtube_id` skips repeats).
- `tract` crates are built with `opt-level = 3` in dev (root `Cargo.toml`), otherwise debug-mode transcription is minutes long.
- Quality benchmarks are ignored tests in `transcribe/eval.rs`: `eval_guitarset` (GuitarSet; prints the final tab F1 and the raw model F1), `eval_transcription` (SoundFont renders of GP files), `eval_fingering` (true notes through `fret_events`, compared with the human strings; `GVA_FIT_FINGERING=1` refits the weights). `GVA_EVAL_CACHE=<dir>` caches each stem's notes so benchmarks of the later stages skip the models. Run them before and after changing thresholds or the arrangement logic. GuitarSet players 03-05 are the held-out set for fingering: don't fit anything on them.

### Shared config (`guitar_core::shared`)

- `config::GLOBAL_CONFIG` — the single source of truth for sample rate (48000.0 Hz) and buffer sizing constants, referenced by both DSP code (e.g. filter cutoff math) and the audio pipeline.
- `pedals::PedalDescription` — the serializable-by-value description of a pedal + its params, used as the message type sent from UI thread to audio thread over the `pedal_tx`/`pedal_rx` channel, and iterated via `strum::EnumIter` to drive the "Add <Pedal>" buttons.

### UI (`guitar_core::frontend`)

- `gui::AudioApp` implements `eframe::App`. Holds the live sample ring (a `VecDeque<f64>`, drained from `audio_rx` each frame) for the oscilloscope, and a `Vec<PedalDescription>` as the UI-side source of truth for the pedal chain — any mutation sends the full vec down `pedal_tx` to trigger an audio-thread chain rebuild.
- `frontend::lib::fft::compute_fft` computes the spectrum shown in the FFT plot, driven off the same sample buffer.
- `frontend::ui::pedals::render_pedal_ui` dispatches per-pedal-type slider UI and returns a `PedalAction` (`None`/`Updated`/`Deleted`) that `AudioApp::update` uses to know whether to push a chain update or remove the pedal from the vec.
- Device selection/refresh in the UI runs on background threads (see Audio pipeline section) to avoid blocking the egui frame loop.

Note: a fair amount of in-code comments and log/print strings are in Portuguese (the primary dev's language) — expect to encounter this reading `capture.rs` and `gui.rs` in particular.
