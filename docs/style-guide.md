# Style guide

The look of the app, as implemented in `guitar_core/src/frontend/style.rs`. Pages and widgets
take their colors and sizes from that module instead of hardcoding them. This is ROADMAP task
4.1. The direction below was chosen without a design review, so treat it as a first proposal:
changing a value in `style.rs` changes it everywhere.

## Direction

A dark pedalboard: near-black background, dark gray cards, and one warm amber accent
(an amp's pilot light) for selection and the active page. Effect families get their own stripe
color so a chain can be read at a glance. A light theme mirrors it for daylight use, and
"System" follows the OS.

## Colors (`style::Palette`)

| Role | Dark | Light | Used for |
|---|---|---|---|
| `accent` | `#FFAA3C` | `#CD6E00` | selection, active page, knob arcs without a family color |
| `background` | `#161619` | `#F4F3F0` | page background |
| `panel` | `#1E1E22` | `#EAE8E4` | top bar, navigation |
| `card` / `card_stroke` | `#28282D` / `#3C3C44` | `#FCFCFA` / `#CDCAC4` | pedal cards, page sections |
| `ok` / `warn` / `danger` | green / yellow / red | darker versions | meters, tuner, errors |
| `led_on` / `led_off` | `#FF4632` / `#462828` | `#E6281E` / `#C8B4B4` | bypass LEDs |

Effect families (`Palette::category`, from `PedalCategory`): Dynamics blue, Filter & EQ violet,
Drive red-orange, Amp & Cabinet amber, Modulation teal, Pitch pink, Delay & Reverb indigo,
Looper gray. The light theme uses the same hues at 80% brightness.

Level meters are green up to -12 dBFS, yellow to -3 dBFS and red above, with a clip light that
stays on for 2 s.

## Type and spacing

- Text: body 13.5 pt, small 11 pt, headings 18 pt, big readouts (tuner note, BPM) 44 pt.
  egui's default fonts only; check new glyphs exist in them (arrows like `→`, `↑` and symbols
  like `✓`, `●`, `♭` don't: use `➡`, `⬆`, `✔`, `⚫`, `b`).
- Spacing scale `style::space`: 4, 8, 12, 16, 24 pt. Item spacing 8 × 6, button padding 8 × 4.
- Corners: cards 8, widgets 5, menus 6.

## Layout

- Window width under `NARROW_WIDTH` (720 pt): navigation is a bottom bar, controls are at least
  `TOUCH_TARGET` (44 pt) tall, pedal cards take the full width. Wider: a navigation rail on the
  left and pedal cards in a grid of 212 pt columns.
- egui can't wrap widgets whose size it only learns while drawing them (frames, groups). Lay
  such things out in explicit rows (`style::grid_rows`, the pedalboard's card rows) instead of
  `horizontal_wrapped`.
- Pages start with a heading, then sections in `style::card_frame`.

## Controls

- Knobs (`widgets::Knob`): 270° sweep, value arc in the pedal's family color (bipolar ranges
  fill from the center), label below; the value replaces the label while hovering or dragging.
  Drag up/down to turn (Shift: 5× finer), double-click resets, right-click to type a value.
  Frequencies and times use a logarithmic taper.
- Bypass: a red LED (lit = on) in the card header plus a footswitch at the bottom; both toggle.
  A bypassed card is dimmed and its family stripe fades.
- Destructive actions (delete a preset, clear the chain) ask for confirmation inline; chain
  edits are undoable instead (Ctrl+Z).
- Transient feedback goes to a toast in the bottom-right corner for 3 s; persistent problems
  (no audio device) get a banner under the top bar.
