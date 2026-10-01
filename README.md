# Guitar Virtual Amp

A real-time guitar effects processor written in Rust. Plug a guitar into an audio interface,
build a pedalboard on screen, and hear the processed signal with low latency. It runs on
desktop (Linux, Windows, macOS) and Android.

## Features

- **17 effect pedals**, chained in any order: amp, cabinet, chorus, compressor, delay,
  distortion, EQ, flanger, looper, low-pass filter, noise gate, octaver, phaser,
  pitch shifter, reverb, tremolo and wah.
- **Cabinet simulator** that convolves the signal with a speaker impulse response (any `.wav` IR),
  using partitioned convolution so latency stays at one block regardless of IR length.
- **20 factory presets** (clean jazz, blues crunch, metal djent, shoegaze wall, surf rock, …),
  plus saving and loading your own.
- **Tuner** based on the YIN pitch-detection algorithm, and a sample-accurate **metronome**.
- **Live oscilloscope and FFT** view of the output signal.
- **Device selection and buffer-size control** from the UI (32 to 4096 frames), to trade
  latency against stability on your hardware.

## How it works

Audio runs at 48 kHz through two [cpal](https://github.com/RustAudio/cpal) streams. The input
stream reads from the audio interface, downmixes to mono and hands samples to the output stream
over a lock-free channel. The output stream runs each sample through the pedal chain and plays it.
The [egui](https://github.com/emilk/egui) UI talks to the audio thread only through channels, so
editing the pedalboard never blocks audio.

Every pedal is a stateful, mono, sample-at-a-time processor implementing one trait:

```rust
pub trait Pedal: Send {
    fn apply_effect(&mut self, input: i16) -> i16;
    // ...plus an optional in-place parameter update that preserves DSP state
}
```

## Project layout

| Crate | Purpose |
| --- | --- |
| `guitar_core` | Audio pipeline, DSP and pedals, egui UI, presets and settings |
| `guitar_desktop` | Desktop app, plus helper binaries (device-switch test, preset generator, DSP benchmark) |
| `guitar_android` | Android `NativeActivity` entry point |

## Build and run

Requires a recent stable Rust toolchain. On Linux, cpal needs the ALSA development headers
(`libasound2-dev` on Debian/Ubuntu).

```bash
cargo run --release -p guitar_desktop
```

Run the tests (always at workspace level; see `CLAUDE.md` for why):

```bash
cargo test --workspace --exclude guitar_android
```

Check that the full pedal chain fits the real-time budget (about 20.8 µs per sample at 48 kHz):

```bash
cargo run --release -p guitar_desktop --bin bench_pedal_chain
```

Android (armv7, min SDK 26), using [cargo-apk](https://github.com/rust-mobile/cargo-apk):

```bash
cargo apk build -p guitar_android
```

## Roadmap

See [ROADMAP.md](ROADMAP.md): a mobile-friendly UI redesign, MIDI/footswitch control and online
preset sharing are planned.
