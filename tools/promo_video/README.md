# Promo video

Renders the app's promo video (1080p, 30 fps, H.264 + stereo AAC) from `storyboard.toml`. The
video is one piece of music (E minor, 100 bpm) with a scene per section, cut on the bar lines:

| Scene | Bars | What you see | What you hear |
|---|---|---|---|
| tuner | 2 | The A string settles in tune, the metronome counts in | One string, then four clicks |
| intro | 4 | Title card | Clean arpeggios through Ambient Ping-Pong, soft bass and drums |
| pedalboard | 4 | Every pedal bypassed, then **Enable all** on the downbeat | A bar of dry DI alone, then Metal High Gain with the band |
| presets | 4 | A new preset clicked on every downbeat | Blues Crunch, Classic Rock Rhythm, Shoegaze Wall, Metal High Gain |
| nam | 2 | A NAM Model pedal added, TONE3000 button | The riff continues |
| analyzer | 2 | Scope and spectrum after the pedals | A palm-muted metal riff |
| tabs | 4 | Practice mode turns each note green | The melody through Rock Lead over the band |
| phone | 4 | The phone layout, light theme | Clean arpeggios through Pop Clean |
| outro | 3 | Title card and credits | One big chord through Shoegaze Wall |

Each app scene is the real app, rendered offscreen like `tools/ui_snapshots`. The guitar goes
through the app's real audio path (`backend::capture::OfflineOutput`), so the guitar tone is
the app's own pedals, and clicks that change the chain change the sound. The meters, tuner,
analyzer and practice scoring also react to what is heard.

```bash
cd tools/promo_video
cargo run                                   # out/guitar_virtual_amp.mp4 (+ poster .png)
cargo run -- --stills /tmp/s --no-video     # PNGs (one per second) and the .wav, no video
cargo run -- --only tuner,tabs --stills /tmp/s --every 0.5
PROMO_STEMS=/tmp/stems cargo run -- --no-video   # also the guitars, band and each DI apart
```

It needs ffmpeg (with libx264) on the PATH and a GPU driver (Vulkan or GL), like
`ui_snapshots`. On Linux the build needs the ALSA headers. A full render takes about two
minutes. The app gets a scratch `XDG_CONFIG_HOME`/`XDG_DATA_HOME`, so your settings, presets,
tab library and Spotify/TONE3000 sessions never appear in the video. This folder is its own
Cargo workspace, like `ui_snapshots`.

## Sounds

None of these are in the repository. Paths go in `[music]` in the storyboard.

- **Guitar**: real DI recordings of every fret of every string by two players, from
  [Guitar-TECHS](https://zenodo.org/records/14963133) (CC BY 4.0, credited in the video):
  `P1_singlenotes` and `P2_singlenotes` with their note labels, as prepared for
  `scripts/train/` (`~/.cache/gva_test/...`). `guitar.rs` cuts them per string and fret,
  tunes each note to A440, and plays riffs with strummed chords (down on the beat, up off
  it), palm mutes, vibrato and small timing and velocity differences. Player 2 is the double
  take. Both recordings are dark, so they get a high shelf, the way a brighter guitar would
  sound. Without them the tool falls back to the plucked-string model (`tone::pluck`), which
  sounds much plainer.
- **Bass and drums**: [GeneralUser GS](https://github.com/mrbumpy409/GeneralUser-GS) (free
  for any use) played by rustysynth, the tab player's synth. Download it once:
  `curl -L -o ~/.cache/gva_promo/GeneralUser-GS.sf2 https://github.com/mrbumpy409/GeneralUser-GS/raw/main/GeneralUser-GS.sf2`.
  The tool also gives it to the app, so the tab player shows the SoundFont. In the tab
  scene, the tab player still plays and scores, but its synth is muted. The band is mixed
  with the rest of the video instead: same synth, same SoundFont, in stereo.
- **Speaker IR**: the factory presets come without one, and an amp without a speaker sounds
  fizzy. With `cabinet = true`, every chain with an amp or drive pedal gets
  `4x12 Closed Back.wav`, a synthetic minimum-phase IR from `tone.rs`, as a player would
  load one. A Cabinet is added after the last drive pedal when the preset has none. The IR
  was voiced octave by octave against EGDB's Mesa and Marshall renders. Pedal-card scenes load
  the preset as a user preset with the IR, so the card shows it. Delays snap to the song's
  eighths, dotted eighths or quarters (`sync_delays`).

## The mix

- The main guitar is the app's output. With `double = true`, a second take through the same
  chain is panned opposite. The relay forwards every chain the app sends to both, so bypass
  and preset clicks reach both.
- Each scene's guitars are leveled to `guitar_db`, bar by bar, keeping a quarter of the level
  differences, like a fader ride. The band is leveled to `band_db`. The guitar bus gets a
  100 Hz low cut, -2.5 dB at 260 Hz and +2.5 dB at 3 kHz. A soft limiter ends the chain.
- Delay and reverb tails ring 3 s into the next scene. The band is delayed by each scene's
  Cabinet latency (1024 samples per IR), so the guitar sits on the beat.
- `cargo test render_presets -- --ignored` with `GVA_PROMO_DI=<di.wav>` writes that DI through
  a few presets, with and without the IR, to the temp dir, for measuring tones.

## Known app issues the video works around

- **Every chain edit rebuilds every pedal except the Looper and NAM.**
  `PedalChain::apply_description` keeps a pedal only if `update_in_place` succeeds, and only
  those two implement it. A rebuild resets amps and filters, cuts delay and reverb tails, and
  the Cabinet reloads its IR and outputs about 21 ms of silence. While someone plays, a knob
  drag (one rebuild per frame) stutters. The video avoids knob drags during the music and
  lands chain changes about 50 ms before a downbeat.
- **Factory presets have no speaker IR**, see above.

## Adding a feature to the video

Add a `[[scene]]` to `storyboard.toml` where it fits the song, with a length in bars, a page,
a preset and parts to play. Script the clicks in beats. Check it with
`cargo run -- --only <id> --stills /tmp/s --every 0.5` before rendering the whole video. A
4-bar scene keeps the Em-C-G-D progression whole:

```toml
[[scene]]
id = "looper"
bars = 4
title = "Looper"
subtitle = "Record a part, then play over it."
page = "Pedals"
preset = "Ambient Pad Looper"
guitar = { riff = "arpeggio" }
bass = "bass_whole"
drums = "soft"
actions = [
    { beat = 0, key = "L" },     # record from the downbeat
    { beat = 8, key = "L" },     # play
]
```

`{pedal_count}`, `{preset_count}` and `{page_count}` in any text are filled in from the code.

### Scene fields

| Field | Meaning |
|---|---|
| `id`, `bars` | Unique name (for `--only` and stills) and length in bars. |
| `layout` | `desktop` (default): the app in a window under the caption. `phone`: the 390 × 844 phone layout beside the caption. `title`: a title card, no app. |
| `title`, `subtitle`, `bullets`, `footnote` | Caption text. Bullets show on `title` and `phone` scenes, the footnote on title cards. |
| `page` | Pedals, Presets, Tuner, Tabs, Spotify, Analyzer or Settings. |
| `preset` | Factory preset loaded at the start. Title scenes play through it. |
| `bypass_all` | Start with every pedal bypassed. |
| `theme`, `window` | `dark`/`light`, and the desktop window size in points (default `[1280, 760]`). |
| `guitar` | What goes into the app: `{ riff = "name", take = 0 }`, or one string being tuned, `{ tune = "5:0", cents = [-35, 0] }`. |
| `double` | A second take of the guitar part, panned opposite, through the same pedals. |
| `backing` | More guitars through their own presets, not shown: `[{ riff, preset, double = true, gain_db = -6 }]`. |
| `bass`, `drums` | A bass riff and a drum pattern, looped. |
| `tab` | Opens the scene's parts as a Guitar Pro file in the tab player: lead, backing, bass and drums tracks. |
| `setup` | Actions done before the first frame, without the cursor (`click`, `key`, `page`). |
| `actions` | Scripted input during the scene. |

### Actions

Each action has `beat` (from 0 at the scene start; `at` in seconds also works) and one verb:

| Verb | Effect |
|---|---|
| `click = "label"` | The cursor glides to the widget for 0.7 s, then clicks it. The release, which is when egui acts, lands on the beat. |
| `drag = "label", dx = 0, dy = -40, secs = 1` | Press on the widget, move by `dx`/`dy` points, release. Knobs turn when dragged up, but see the known issues. |
| `hover = "label"` | Moves the cursor there without clicking. |
| `key = "K"` | A key press (egui key names: `Space`, `F1`, `PageDown`, `M`, `K`, `L`, ...). |
| `page = "Tuner"` | Switches page without a click. |
| `scroll = 200` | Mouse wheel, in points (positive scrolls down). |
| `zoom = [x, y, w, h], secs = 0.8` | Zooms onto that part of the app, in points, widened to the window's aspect ratio. The app is re-rendered at the zoomed scale, so text stays sharp. |
| `unzoom = true` | Zooms back out. |

Labels are the widgets' exact text, icons included (`"➕ Add pedal"`, `"🎯 Prática"`); add
`contains = true` to match part of a label and `nth = 1` for the second match. A label that
isn't on screen stops the render with an error naming the scene and action. Take zoom
coordinates from a still: points are pixels ÷ 1.095 at the default size, measured from the
window's top-left corner.

### Riffs and drums

`[riffs.<name>]`: `notes` in tab notation, one token per step (`step = 8`: eighths, the
default; 16: sixteenths), in 4/4:

- `6:0` is string 6 fret 0, and `6:0+5:2+4:2` is a strummed chord.
- Suffixes: `m` palm mute, `~` vibrato, `!` accent.
- `.` is a rest, `-` holds the previous token, and `|` is only a bar line.

`ring = true` lets each note ring until its string plays again (arpeggios).
`instrument = "bass"` uses a 4-string bass (string 4 = low E), and `program` sets the GM
sound (33 finger, 34 pick).

`[drums.<name>]` has one line per drum (`kick`, `snare`, `side`, `hat`, `open_hat`, `ride`,
`crash`, `tom_hi`, `tom_mid`, `tom_low`, ...), one token per step: `x` hit, `X` accent, `o`
soft, `.` nothing. `kit` picks the GS kit (0 standard, 8 room, 16 power). Patterns loop, so a
4-bar pattern can end in a fill.

## Other gaps

- The Transcribe form shows install warnings and the scratch data path on machines without
  the optional models, so there is no transcription scene.
- The tab player's labels are in Portuguese.
