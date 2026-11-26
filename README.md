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
- **Stompbox pedalboard**: each pedal is a card with knobs, a bypass LED and a footswitch,
  colored by effect family. Add pedals from a menu grouped by family (they land in the usual
  signal-chain order), drag them to reorder, duplicate, reset or collapse them, flip between two
  sounds with **A/B**, and undo/redo every edit (Ctrl+Z / Ctrl+Shift+Z).
- **20 factory presets** (clean jazz, blues crunch, metal djent, shoegaze wall, surf rock, …),
  plus saving and loading your own, with search, song/artist tags and a **setlist** you walk with
  the top-bar arrows, PageUp/PageDown or a MIDI footswitch.
- **Tuner** based on the YIN pitch-detection algorithm: needle, string targets for 15 tunings
  (drop D, half step down, open G, 7-string, bass...) or the open tab's tuning, adjustable A4
  reference, and a fretboard showing where the note is. Sample-accurate **metronome** with an
  accented first beat, click volume and tap tempo, and one-click delay/tremolo sync to
  quarter, eighth, dotted-eighth or triplet notes.
- **Master section**: input and output level meters with clip lights, input gain, master volume
  and mute in the top bar; the app warns when the input clips.
- **Recorder**: record the processed guitar (or everything you hear) to WAV with one button.
- **MIDI footswitches and expression pedals** (desktop): MIDI learn for pedal bypass, next/previous
  preset, setlist slots by program change, mute, tap tempo, metronome, looper and master volume.
- **Analyzer** page: a triggered oscilloscope (a held note stands still) and a log-frequency
  spectrum in dBFS with peak hold and freeze, of the dry guitar or the processed one.
- **Dark, light or system theme** and UI scaling; on narrow screens (phones) the navigation moves
  to a bottom bar and controls grow to touch size.
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
  accuracy score and streak, a per-bar heat map, the bars that need work (one click loops them)
  and your best result per tab. **Loop** any range of bars (Shift+click a bar to extend it), add
  a count-in, and let the **speed trainer** start slow and speed up after every good pass.
  **Latency calibration** on the Settings page measures the real round trip with a pulse through
  your speakers and microphone (or a loopback cable). It checks pitch (chords included) within a timing tolerance,
  compensating the audio latency. Octave errors and the same note on another string count
  as right; a note held from the previous beat also counts (there's no onset detection yet).
- **Tone suggestion** for the selected tab track: your own presets tagged with the song or
  artist, plus a factory preset picked from the track's instrument, name, tuning and palm
  muting. It's a starting point, not a copy of the recorded tone.
- **Automatic tablature** from your own audio files, offline: BS-RoFormer or Demucs separates
  guitar and bass, guitars on different sides of the stereo mix become separate tracks, a
  guitar-trained model (or Basic Pitch) transcribes the notes, the pitch contour marks bends,
  vibrato, slides and hammer-ons, Beat This! finds beats and bars (the meter may change), and
  the app picks rhythms, strings and frets, then saves a Guitar Pro 5 file into a local tab
  repository. See below.
- **Spotify** page: connect your account, browse your playlists, play a track on your Spotify
  app (Spotify Connect, Premium required) and open the matching tab from your own folder of
  Guitar Pro files (matched by file name, or linked by hand). See below.
- **Device selection and buffer-size control** from the UI (32 to 4096 frames), to trade
  latency against stability on your hardware.

## Using the app

The pages are on the left (or at the bottom on a phone): **Pedals**, **Presets**, **Tuner**
(with the metronome), **Tabs**, **Spotify**, **Analyzer** and **Settings** (audio devices and
buffer, levels, latency calibration, theme, MIDI, recordings). Without a working audio device
the app still opens, with a banner and a Retry button.

Keyboard shortcuts (F1 shows them in the app):

| Keys | Action |
| --- | --- |
| Ctrl+Z / Ctrl+Shift+Z (Ctrl+Y) | Undo / redo pedal edits |
| 1 … 9 | Bypass / enable pedal 1-9 |
| PageUp / PageDown | Previous / next preset (setlist) |
| Ctrl+S | Save the loaded user preset |
| M / T / K / L | Mute, tap tempo, metronome on/off, looper record/play/overdub |
| Ctrl+R | Start / stop recording |
| Space | Play / pause the tab (Tabs page) |
| Ctrl+1 … Ctrl+7 | Switch page |

Knobs: drag up/down (Shift for fine steps), double-click to reset, right-click to type a value.

MIDI (Settings → MIDI controller): pick the input port, press **Learn** next to an action and
press the footswitch. Footswitch CCs toggle on press (value ≥ 64); an expression pedal bound to
master volume uses every value. Recordings go to `<data dir>/guitar_virtual_amp/recordings/`.

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
| `guitar_desktop` | Desktop app, plus helper binaries (device-switch test, preset generator, DSP benchmark, batch `transcribe`, `render_tones` for training data) |
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

1. A separator splits the guitar, bass and drum stems, using the GPU when PyTorch sees one:
   [BS-RoFormer SW](https://huggingface.co/FractalSurfer/BS-RoFormer-SW-6stem), run through
   [MSST](https://github.com/ZFTurbo/Music-Source-Separation-Training), when installed (its
   guitar stem transcribes better: note F1 0.81 against 0.74 on band mixes), else
   [Demucs](https://github.com/facebookresearch/demucs) `htdemucs_6s` (MIT).
   `GVA_SEPARATOR=demucs|bs-roformer` forces one. Optional: without a separator, the
   instruments are transcribed from the full mix.
2. Beats and downbeats come from [Beat This!](https://github.com/CPJKU/beat_this) (MIT) on
   the mix (the `final0` checkpoint, or `small0` when it's missing); bars of 2 to 7 beats are
   decoded from its downbeats, so a song can switch meter or insert a 2/4 bar. Without it, a simpler tracker uses the drum stem (or the mix) and assumes
   4/4.
3. With separation, the stereo guitar stem is split by pan position (left, center, right). Parts
   that play different things (say, rhythm on the left and lead in the middle) become separate
   guitar tracks; parts that play the same notes (one guitar panned halfway, a double-tracked
   part, a stereo delay) stay one.
4. Guitar notes come from the guitar transcription model of
   [Riley et al. (GAPS, ISMIR 2024)](https://arxiv.org/abs/2408.08653) (MIT), bass notes (and
   guitar notes when that model isn't installed) from
   [Basic Pitch](https://github.com/spotify/basic-pitch) (Apache-2.0, bundled in
   `guitar_core/assets/models/`). All models run in pure Rust with
   [tract](https://github.com/sonos/tract). Basic Pitch's pitch contour (a third of a semitone
   resolution) then marks bends and releases, vibrato, slides and hammer-ons/pull-offs on
   single-note lines, and merges bends the note model split in two.
5. Notes struck together become chords; every beat is written as 16ths or 8th-note triplets,
   whichever fits the timing; strings and frets come from a cost on stretch, hand moves and
   muted strings plus how often guitarists use each string for a pitch (learned from
   GuitarSet), or from the learned fingering model when it's installed (6-string guitars,
   see below); slides and hammer-ons stay on one string and bent notes off open strings; a
   tuning is picked from the lowest notes (standard, Eb, drop D, drop C, 7 and 8 strings).
6. The result goes to the tab repository, `<data dir>/guitar_virtual_amp/tab_repository/`:
   `Artist - Title.gp5` (opens in Guitar Pro, TuxGuitar, MuseScore) plus `Artist - Title.gvtab.json`
   with the source audio's SHA-256, the engines used and the time of every bar, which the tab
   player uses to play the original recording in sync. Audio is never copied into the repository.

Install a separator and yt-dlp once (need [uv](https://docs.astral.sh/uv/); BS-RoFormer also
needs git, yt-dlp needs ffmpeg). Demucs is about 5 GB with the CUDA build of PyTorch;
BS-RoFormer adds a ~700 MB checkpoint and needs about 4 GB of free GPU memory. Both take
`--cpu` for a smaller CPU-only build. Rerun `install_ytdlp.sh` when YouTube downloads start
failing (YouTube changes often):

```bash
scripts/install_bs_roformer.sh            # preferred separator (local use only, see License)
scripts/install_demucs.sh                 # and/or Demucs (the fallback)
scripts/install_ytdlp.sh
scripts/install_transcription_models.sh   # guitar model + Beat This! (~200 MB)
```
The last one downloads the Beat This! ONNX files (final0 and small0) and the guitar model
checkpoint, which it exports to ONNX with PyTorch (reusing the Demucs environment when there
is one).

**Tone estimate (experimental).** With **Estimar o timbre** ticked in the Transcribe form (or
`--tone` in batch mode; needs a separator), after the tab the app searches its own pedals for a
chain that makes each guitar track sound like the recording: compressor, distortion, amp gain and
voicing, delay (its time measured from the echoes), reverb, and a final matching filter loaded in a
Cabinet pedal. For each track it saves two user presets named after the song and track, which
show up under 💡 Timbre on the Tabs page: the search result, and the nearest factory preset plus
the matching filter. It takes 15-30 s per guitar track. It's a starting point to adjust by ear: on
EGDB recordings through amp simulations the search is closer to the target than the nearest
factory preset, but on separated stems the factory preset plus the matching filter is about as
close, and no listening test has been done. Details in
`docs/papers/tone-estimation/` and `ROADMAP.md`.

Batch mode, to fill the repository offline (skips files already transcribed):

```bash
cargo run --release -p guitar_desktop --bin transcribe -- ~/Music/
cargo run --release -p guitar_desktop --bin transcribe -- --youtube "Legião Urbana Tempo Perdido" --youtube https://youtu.be/<id>
cargo run --release -p guitar_desktop --bin transcribe -- --help
```

Measured quality (note onset within 80 ms and exact pitch, F1, on the final tab), with the
optional models installed:

| Benchmark | F1 |
|---|---|
| [GuitarSet](https://zenodo.org/records/3371780), 60 real solo acoustic recordings (tempo right in 59, bar lines on the beat in 57) | 0.83 |
| Same recordings through the "Blues Crunch" preset | 0.76 |
| Same recordings through the "Metal High Gain" preset (8% octave errors, mostly an octave low) | 0.24 |
| Isolated guitar tracks rendered from Guitar Pro files | 0.85 |
| SoundFont-rendered full mixes without separation | 0.68 |
| Two GuitarSet guitars in one stereo mix (rhythm + lead), lead part | 0.85-0.88 (0.40-0.43 as one track) |

Given the right notes, 75% land on the same string as the GuitarSet player (held-out
players) with the default fingering; the learned fingering model below raises the final tab's
string/fret agreement on those players from 68% to 78%. Expect to fix the tab by hand: no palm mute or harmonics, guitars playing different
parts from the same spot of the mix stay one track, and heavy distortion breaks the default
guitar model (see the metal row). The benchmarks are ignored tests in
`guitar_core/src/transcribe/eval.rs` (`eval_guitarset`, `eval_transcription`,
`eval_fingering`, `eval_real`, `eval_guitar_pairs`, `eval_manifest`).

**Electric guitar model (optional, local).** `scripts/train/` fine-tunes the guitar model on
electric guitar (EGDB, Guitar-TECHS, GuitarSet players 00-02, rendered through the app's amp
presets). Copy the exported `.onnx` to `<data dir>/guitar_virtual_amp/tools/models/guitar_electric_crnn.onnx`
and the Transcribe form offers GAPS or electric (batch CLI: `--guitar-model electric`);
`GVA_GUITAR_MODEL` overrides both. On GuitarSet players 03-05 through the "Metal High Gain"
preset, the final tab's F1 goes from 0.43 to 0.69. Raw note F1 of an earlier run (4000 steps),
original model -> fine-tuned, on recordings it never saw:

| Test set | Original | Fine-tuned |
|---|---|---|
| GuitarSet players 03-05, clean / "Blues Crunch" / "Metal High Gain" | 0.87 / 0.80 / 0.29 | 0.88 / 0.84 / 0.63 |
| EGDB, Plexi amp (unseen tone) | 0.67 | 0.81 |
| GOAT test split (ISMIR 2025), DI / 5 amp tones | 0.76 / 0.48-0.63 | 0.81 / 0.68-0.75 |
| IDMT-SMT-Guitar licks (bends, slides, vibrato, harmonics) | 0.86 | 0.88 |
| Guitar-TECHS musical excerpts, DI / miked amp | 0.83 / 0.80 | 0.81 / 0.78 |
| Band mixes (labelled guitar over MUSDB18 drums, bass, vocals), EGDB Marshall | 0.75 | 0.81 |
| Band mixes, Guitar-TECHS excerpts | 0.71 / 0.64 | 0.63 / 0.62 |

It isn't the default yet: EGDB states no license, so ask its authors before distributing a
model trained on it. See `scripts/train/README.md`.

**Learned fingering (optional, local).** `scripts/train/fingering/` trains a small Transformer
(0.57M parameters) that picks each note's string from the notes around it. Copy its
`model.onnx` to `<data dir>/guitar_virtual_amp/tools/models/fingering_tagger.onnx` (or set
`GVA_FINGERING_MODEL`) and it becomes the default for 6-string guitars, with the Viterbi as a
choice in the Transcribe form (batch CLI: `--fingering learned|viterbi`). On true notes it puts
0.62 of GuitarSet solo notes on the player's string (Viterbi 0.55) and 0.81 on EGDB (0.68). It
is trained on EGDB too, so the same license caveat applies.

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

This is a personal open-source project, with no hosted service or store release planned. See
[ROADMAP.md](ROADMAP.md): next up are a mobile-friendly UI redesign and MIDI/footswitch control,
plus more play-along and transcription work.

## License

Copyright (C) 2025-2026 Breno de Angelo.

This program is free software: you can redistribute it and/or modify it under the terms of the
GNU General Public License as published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version. See [LICENSE](LICENSE).

Bundled and optional third-party models keep their own licenses: Basic Pitch (Apache-2.0, see
`guitar_core/assets/models/`), the GAPS guitar model and Beat This! (MIT), Demucs and MSST (MIT).
The BS-RoFormer SW checkpoint states no license (it comes from a community mirror): it is
downloaded for local, personal use only and must not be bundled or redistributed.
