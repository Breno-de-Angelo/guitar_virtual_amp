# App improvements, November 2026 (branch `feat/app-improvements`)

`ROADMAP.md` was being rewritten by the transcription/tone research work at the same time, so
this branch doesn't touch it. This note lists what the branch did against the roadmap, so the
status can be folded into `ROADMAP.md` later. Tablature transcription and tone estimation
(`transcribe/`, `tone/`, `shared/tab/tone.rs`, `frontend/ui/transcribe.rs`) were left alone.

## Roadmap items done

- **4.1 Style guide**: `docs/style-guide.md` and `frontend::style` (palette, spacing, text
  sizes, touch sizes, `grid_rows`). The direction was chosen without a review; it's a proposal.
- **4.2 Navigation shell**: pages Pedals / Presets / Tuner / Tabs / Spotify / Analyzer / Settings,
  a rail on wide windows and a bottom bar under 720 pt.
- **4.3 Analyzer off the main screen**: its own page, with a triggered scope and a log-frequency
  dBFS spectrum.
- **4.4 Pedal widget**: stompbox cards with knobs, LED and footswitch (`ui::pedals`,
  `ui::widgets`).
- **4.5 Touch sizing**: 44 pt controls, full-width cards and adaptive tuner chips on narrow
  windows. Checked on a 390 × 844 render, not on a device or emulator.
- **4.6 MIDI**: `backend::midi` (midir, desktop only) and `shared::midi` with MIDI learn.
- **Practice loops and speed trainer**, **hardest bars heat map**, **practice history per tab**.
- **Pre-practice tuning check**: the Tabs page shows the track's tuning and opens the tuner on it.
- **Automatic latency calibration**: Settings → Latency (a pulse out through the speakers, detected
  back in).
- **Dry input tap**: the tuner, practice mode and analyzer get the guitar before the pedals by
  default (`AudioControls::analysis_dry`).
- **Tab library**: matches by the title/artist inside the files, with a cache
  (`tab_library_cache.json`).

## Also new

Master section (meters, input gain, master volume, mute, clip warning), WAV recorder, preset
setlist with next/previous and search, song/artist tags in the save form, A/B compare, undo/redo
for every chain change, keyboard shortcuts (F1), metronome accent/volume and note-value delay
sync, tuner with 15 tunings, A4 reference and fretboard, count-in, the app opening without an
audio device, presets and settings persisted on Android, preset name in the window title, last
page remembered, dropping `.json` / `.nam` / `.wav` files on the window.

Fixes: the CPU meter stopped updating after a device switch (the new stream got its own
counter); the UI could block on `pedal_tx.send` with no stream draining it; the tuner and
practice mode heard the metronome click.

## Known limitations

- The last beats of each loop pass aren't judged in practice mode (the jump back clears the
  analysis history before they are).
- MIDI footswitch CCs toggle on values ≥ 64, so latching switches that alternate 127/0 act on
  every other press.
- The practice-mode sliders on the Tabs page still save settings on every change (unchanged code).
- Some Tabs-page rows are wider than a narrow window (combo boxes don't wrap); the tab itself
  fits.
- Android: compiles (`cargo check --target x86_64-linux-android`), not run on a device. MIDI is
  desktop only.

## Testing

`cargo test --workspace --exclude guitar_android` (319 tests) and clippy are clean. The pages
were also rendered offscreen with `egui_kittest` + wgpu at 1280 × 820 and 390 × 844, dark and
light, including clicks and a knob drag: `tools/ui_snapshots/` (its own Cargo workspace, so
regular builds don't compile wgpu).
