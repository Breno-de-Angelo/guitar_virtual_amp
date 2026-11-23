# Guitar Virtual Amp

A real-time guitar effects processor written in Rust. Plug a guitar into an audio interface,
build a pedalboard on screen, and hear the processed signal with low latency. It runs on
desktop (Linux, Windows, macOS) and Android.

## Features

- **18 effect pedals**, chained in any order: amp, cabinet, chorus, compressor, delay,
  distortion, EQ, flanger, looper, low-pass filter, NAM model, noise gate, octaver, phaser,
  pitch shifter, reverb, tremolo and wah.
- **NAM Model pedal** that runs [Neural Amp Modeler](https://www.neuralampmodeler.com/) captures
  (`.nam` WaveNet and LSTM captures, A1 and most A2 files) through the pure-Rust [nam-rs](https://github.com/OpenSauce/nam-rs)
  runtime, with input/output gain and loudness normalization.
- **TONE3000 integration** (Powered by [TONE3000](https://www.tone3000.com)): sign in, pick NAM
  captures and IRs from your favorites, uploads, downloads, trending and latest tones, or browse
  the whole catalog, and load them straight into the NAM Model or Cabinet pedal. See below.
- **Cabinet simulator** that convolves the signal with a speaker impulse response (any `.wav` IR),
  using partitioned convolution so latency stays at one block regardless of IR length.
- **20 factory presets** (clean jazz, blues crunch, metal djent, shoegaze wall, surf rock, …),
  plus saving and loading your own.
- **Tuner** based on the YIN pitch-detection algorithm, and a sample-accurate **metronome**.
- **Live oscilloscope and FFT** view of the output signal.
- **Tab player** (an open Songsterr-style view): opens Guitar Pro files (GP3, GP4, GP5, GPX, GP7/8),
  draws the tablature with a playback cursor, and plays it with a SoundFont synthesizer
  (per-track mute/solo, speed control) and/or an original recording (mp3, ogg, flac, wav, m4a)
  aligned to the score. Guitar Pro 8 embedded audio and sync points are used automatically;
  for other files you mark where bar 1 starts and, optionally, correct the tempo. The song
  plays through the same output as your processed guitar, so you can mute a track and play it
  yourself. The synth needs a `.sf2` SoundFont (e.g. FluidR3_GM, GeneralUser GS): it picks up
  common system install paths or any file in `<data dir>/guitar_virtual_amp/soundfonts/`, or
  you can choose one in the UI.
- **Practice mode** in the tab player: while the song plays, it listens to your guitar and
  colors each note of the selected track green (right) or red (wrong or missing), with an
  accuracy score and streak. It checks pitch (chords included) within a timing tolerance,
  compensating the audio latency. Octave errors and the same note on another string count
  as right; a note held from the previous beat also counts (there's no onset detection yet).
- **Tone suggestion** for the selected tab track: your own presets tagged with the song or
  artist, plus a factory preset picked from the track's instrument, name, tuning and palm
  muting. It's a starting point, not a copy of the recorded tone.
- **Automatic tablature** from your own audio files, offline and open source: Demucs separates
  guitar and bass, a guitar-trained model (or Basic Pitch) transcribes the notes, Beat This!
  finds beats, bars and the meter, and the app picks rhythms, strings and frets, then saves a
  Guitar Pro 5 file into a local tab repository. See below.
- **Spotify** page: connect your account, browse your playlists, play a track on your Spotify
  app (Spotify Connect, Premium required) and open the matching tab from your own folder of
  Guitar Pro files (matched by file name, or linked by hand). See below.
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
# also measure a NAM capture:
NAM_MODEL=/path/to/capture.nam cargo run --release -p guitar_desktop --bin bench_pedal_chain
```

Android (armv7, min SDK 26), using [cargo-apk](https://github.com/rust-mobile/cargo-apk):

```bash
cargo apk build -p guitar_android
```

## TONE3000

The NAM Model and Cabinet pedals each have a **Browse TONE3000** button. It signs you in to
[TONE3000](https://www.tone3000.com) in the system browser (OAuth with PKCE and a redirect to
`http://127.0.0.1:47823/callback`), lets you pick a tone, and downloads the chosen model into the
pedal. Devices without a browser can sign in with a code at `tone3000.com/activate` instead.
Without TONE3000 you can still load any `.nam` or `.wav` file from disk with **Load .nam...** /
**Load IR...**.

Setup:

1. Create API keys in your TONE3000 account settings.
2. Set the **publishable** key in `TONE3000_CLIENT_ID`, either when building
   (`TONE3000_CLIENT_ID=... cargo build --release`) or when starting the app. Never put the secret
   key (`t3k_cs_...`) in the app.
3. If you register redirect URIs in the TONE3000 settings, add `http://127.0.0.1:47823/callback`.
   `TONE3000_REDIRECT_PORT` changes the port.

Downloads go to `<data dir>/guitar_virtual_amp/tone3000/models/<tone id>/`, next to a `.json` file
with the tone's creator and license. The sign-in session is in `tone3000/session.json`, readable
only by your user. Each tone keeps the license its creator chose (T3K, CC BY, CC BY-NC, ...), and
use of the API follows the [TONE3000 API terms](https://www.tone3000.com/api/terms): models are
downloaded only when you ask for one, and none are bundled with the app.

## Automatic tablature (transcription)

**🤖 Transcrever áudio...** on the Tabs page (or **🤖 Transcrever** next to a Spotify track
without a tab) turns a recording into a tab with a guitar and a bass track. The audio comes from
a file (mp3, flac, ogg, wav, m4a) or from **YouTube**: search by artist and song (or paste a
link), pick a result, and the app downloads the audio track with
[yt-dlp](https://github.com/yt-dlp/yt-dlp). From a Spotify track the search starts by itself and
the result whose length matches the Spotify track best is preselected (official "Artist - Topic"
audio ranks first; live, cover, karaoke and lesson videos last). Downloads are kept in
`<data dir>/guitar_virtual_amp/youtube_audio/` for playback in sync with the tab, never in the
repository. YouTube's Terms of Service only allow downloading where YouTube offers it or with
permission from YouTube and the rights holders: use it for content you have the right to
download. Everything else runs on your computer:

1. [Demucs](https://github.com/facebookresearch/demucs) `htdemucs_6s` (MIT) separates the
   guitar, bass and drum stems, using the GPU when PyTorch sees one. Optional: without it, the
   instruments are transcribed from the full mix.
2. Beats, downbeats and the meter (4/4 or 3/4) come from
   [Beat This!](https://github.com/CPJKU/beat_this) (MIT) on the mix. Without it, a simpler
   tracker uses the drum stem (or the mix) and assumes 4/4.
3. Guitar notes come from the guitar transcription model of
   [Riley et al. (GAPS, ISMIR 2024)](https://arxiv.org/abs/2408.08653) (MIT), bass notes (and
   guitar notes when that model isn't installed) from
   [Basic Pitch](https://github.com/spotify/basic-pitch) (Apache-2.0, bundled in
   `guitar_core/assets/models/`). All models run in pure Rust with
   [tract](https://github.com/sonos/tract).
4. Notes struck together become chords; every beat is written as 16ths or 8th-note triplets,
   whichever fits the timing; strings and frets come from a cost on stretch, hand moves and
   muted strings plus how often guitarists use each string for a pitch (learned from
   GuitarSet); a tuning is picked from the lowest notes (standard, Eb, drop D, drop C, 7 and 8
   strings).
5. The result goes to the tab repository, `<data dir>/guitar_virtual_amp/tab_repository/`:
   `Artist - Title.gp5` (opens in Guitar Pro, TuxGuitar, MuseScore) plus `Artist - Title.gvtab.json`
   with the source audio's SHA-256, the engines used and the time of every bar, which the tab
   player uses to play the original recording in sync. Audio is never copied into the repository.

Install Demucs and yt-dlp once (need [uv](https://docs.astral.sh/uv/); yt-dlp also needs
ffmpeg; Demucs is about 5 GB with the CUDA build of PyTorch, `--cpu` for a smaller CPU-only one).
Rerun `install_ytdlp.sh` when YouTube downloads start failing (YouTube changes often):

```bash
scripts/install_demucs.sh
scripts/install_ytdlp.sh
scripts/install_transcription_models.sh   # guitar model + Beat This! (~115 MB)
```
The last one downloads the Beat This! ONNX files and the guitar model checkpoint, which it
exports to ONNX with PyTorch (reusing the Demucs environment when there is one).

Batch mode, to fill the repository offline (skips files already transcribed):

```bash
cargo run --release -p guitar_desktop --bin transcribe -- ~/Music/
cargo run --release -p guitar_desktop --bin transcribe -- --youtube "Legião Urbana Tempo Perdido" --youtube https://youtu.be/<id>
cargo run --release -p guitar_desktop --bin transcribe -- --help
```

Measured quality (note onset within 80 ms and exact pitch, F1, on the final tab), with the
optional models installed: 0.83 median on 60 real solo acoustic guitar recordings from
[GuitarSet](https://zenodo.org/records/3371780) (0.70 with the previous pipeline; tempo right in 59 of 60, bar
lines on the right beat in 56 of 60); 0.85 on isolated guitar tracks rendered from Guitar Pro
files; 0.68 on SoundFont-rendered full mixes without separation. Given the right notes, 75% land
on the same string as the GuitarSet player (held-out players). Not measured yet on real band
recordings. Expect to fix the tab by hand: no techniques (bends, slides, hammer-ons), one guitar
track even when the song has several, only 4/4 and 3/4, and the guitar model makes octave
errors on very low distorted riffs (7/8-string metal). The benchmarks are ignored tests in
`guitar_core/src/transcribe/eval.rs` (`eval_guitarset`, `eval_transcription`,
`eval_fingering`).

## Spotify

The **🎧 Spotify** page is a remote control for your Spotify app plus a bridge to your tabs.
Spotify's audio never goes through this app: the Web API doesn't provide it, and the
[Developer Policy](https://developer.spotify.com/policy) forbids mixing it with other audio,
synchronizing it with visual content and analysing it. That's also why the tab cursor doesn't
follow Spotify and tabs aren't downloaded from anywhere: they come from a folder of Guitar Pro
files you own ("Artist - Title.gp5" names match automatically).

Setup:

1. Create an app at the [Spotify developer dashboard](https://developer.spotify.com/dashboard)
   and tick **Web API**.
2. Add the redirect URI `http://127.0.0.1:47824/callback` exactly (Spotify refuses `localhost`).
   `SPOTIFY_REDIRECT_PORT` changes the port.
3. Put the **Client ID** in `SPOTIFY_CLIENT_ID`, when building or when starting the app (or in
   `[env]` of `.cargo/config.toml`). The app uses Authorization Code with PKCE, so there is no
   client secret.

Development-mode apps need a Premium owner and work for up to five users you add in the
dashboard. Playback control needs Premium, and the Spotify app must be open on some device.
Only playlists you own or collaborate on show their tracks (a Spotify rule). The session is in
`<data dir>/guitar_virtual_amp/spotify/session.json`, readable only by your user; **Desconectar**
deletes it, and [spotify.com/account/apps](https://www.spotify.com/account/apps) revokes access.

## Roadmap

See [ROADMAP.md](ROADMAP.md): a mobile-friendly UI redesign, MIDI/footswitch control and online
preset sharing are planned.
