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

### Adding a new pedal

Touch four places: add a `*Params` struct + `Default` impl and a new `PedalDescription` variant (with its `name()`) in `shared/pedals.rs`; add the DSP implementation in `backend/pedals/<name>.rs` implementing `Pedal`, and register it in `backend/pedals/mod.rs`; wire the match arm in `PedalChain::from_description` (`backend/pedals/pedal.rs`); add slider UI in `frontend/ui/pedals.rs`'s `render_pedal_ui` match.

### Shared config (`guitar_core::shared`)

- `config::GLOBAL_CONFIG` — the single source of truth for sample rate (48000.0 Hz) and buffer sizing constants, referenced by both DSP code (e.g. filter cutoff math) and the audio pipeline.
- `pedals::PedalDescription` — the serializable-by-value description of a pedal + its params, used as the message type sent from UI thread to audio thread over the `pedal_tx`/`pedal_rx` channel, and iterated via `strum::EnumIter` to drive the "Add <Pedal>" buttons.

### UI (`guitar_core::frontend`)

- `gui::AudioApp` implements `eframe::App`. Holds the live sample ring (a `VecDeque<f64>`, drained from `audio_rx` each frame) for the oscilloscope, and a `Vec<PedalDescription>` as the UI-side source of truth for the pedal chain — any mutation sends the full vec down `pedal_tx` to trigger an audio-thread chain rebuild.
- `frontend::lib::fft::compute_fft` computes the spectrum shown in the FFT plot, driven off the same sample buffer.
- `frontend::ui::pedals::render_pedal_ui` dispatches per-pedal-type slider UI and returns a `PedalAction` (`None`/`Updated`/`Deleted`) that `AudioApp::update` uses to know whether to push a chain update or remove the pedal from the vec.
- Device selection/refresh in the UI runs on background threads (see Audio pipeline section) to avoid blocking the egui frame loop.

Note: a fair amount of in-code comments and log/print strings are in Portuguese (the primary dev's language) — expect to encounter this reading `capture.rs` and `gui.rs` in particular.
