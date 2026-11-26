//! `storyboard.toml`: the music and the video's scenes, in order. See
//! README.md for the format; every field here is documented there too.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::riff::{BASS, Drums, GUITAR, Riff};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storyboard {
    pub video: Video,
    pub music: Music,
    #[serde(default)]
    pub riffs: BTreeMap<String, RiffSpec>,
    #[serde(default)]
    pub drums: BTreeMap<String, DrumSpec>,
    #[serde(rename = "scene")]
    pub scenes: Vec<Scene>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Video {
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// Output file, relative to the storyboard.
    #[serde(default = "default_output")]
    pub output: String,
    /// `"scene id@seconds"`: that frame is also saved as a PNG next to the video.
    pub poster: Option<String>,
    /// Fade through the background between scenes, seconds (the music doesn't fade).
    #[serde(default = "default_fade")]
    pub fade: f32,
    /// Text shown small in the corner of app scenes ("" = none).
    #[serde(default)]
    pub watermark: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Music {
    pub bpm: f64,
    /// General MIDI SoundFont for the bass and drums (and the tab player).
    pub soundfont: Option<String>,
    /// Guitar DI recordings, `"di.wav:notes.txt"`, one per take (player).
    #[serde(default)]
    pub guitars: Vec<String>,
    /// Loudness of the guitars and of the band, dBFS RMS, before the limiter.
    #[serde(default = "default_guitar_db")]
    pub guitar_db: f32,
    #[serde(default = "default_band_db")]
    pub band_db: f32,
    /// Load a speaker IR into every chain with an amp or a drive pedal, as
    /// a player would (factory presets come without one).
    #[serde(default = "default_true")]
    pub cabinet: bool,
    /// Snap delay times to the song's dotted eighths / eighths / quarters.
    #[serde(default = "default_true")]
    pub sync_delays: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiffSpec {
    /// Steps per whole note: 8 = each token is an eighth note.
    #[serde(default = "default_step")]
    pub step: u32,
    pub notes: String,
    /// "guitar" (default) or "bass".
    #[serde(default)]
    pub instrument: Instrument,
    /// Notes ring until the same string plays again.
    #[serde(default)]
    pub ring: bool,
    /// General MIDI program (bass sound, or the tab's instrument).
    pub program: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Instrument {
    #[default]
    Guitar,
    Bass,
}

#[derive(Debug, Deserialize)]
pub struct DrumSpec {
    #[serde(default = "default_step")]
    pub step: u32,
    /// GS drum kit (program on channel 10): 0 standard, 8 room, 16 power.
    #[serde(default)]
    pub kit: u8,
    /// One line per drum: kick, snare, side, hat, open_hat, pedal_hat,
    /// crash, ride, ride_bell, china, splash, tom_hi, tom_mid, tom_low.
    #[serde(flatten)]
    pub lanes: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// A title card: big centered text, no app.
    Title,
    /// The app in a desktop window, caption above.
    #[default]
    Desktop,
    /// The app at phone size, caption beside it.
    Phone,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    pub id: String,
    /// Length in bars of the song (4/4).
    pub bars: f32,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    /// Lines listed under the subtitle (title layout) or the caption (phone).
    #[serde(default)]
    pub bullets: Vec<String>,
    /// Small print at the bottom of a title card (credits).
    #[serde(default)]
    pub footnote: String,
    /// App page to open: Pedals, Presets, Tuner, Tabs, Spotify, Analyzer, Settings.
    pub page: Option<String>,
    /// Factory preset loaded at the start (also the tone of title scenes).
    pub preset: Option<String>,
    /// Start with every pedal bypassed.
    #[serde(default)]
    pub bypass_all: bool,
    #[serde(default)]
    pub theme: Theme,
    /// Desktop window size in app points, `[width, height]`.
    pub window: Option<[f32; 2]>,
    /// Open the scene's parts (guitar, backing, bass, drums) as a tab.
    #[serde(default)]
    pub tab: bool,
    /// What the guitar plays into the app.
    pub guitar: Option<Guitar>,
    /// A second take of the guitar part through the same pedals, panned
    /// opposite (double tracking).
    #[serde(default)]
    pub double: bool,
    /// More guitars, through their own presets (not shown).
    #[serde(default)]
    pub backing: Vec<Backing>,
    /// Bass riff and drum pattern, looped.
    pub bass: Option<String>,
    pub drums: Option<String>,
    /// Actions done before the first frame (instantly, no cursor).
    #[serde(default)]
    pub setup: Vec<Action>,
    #[serde(default)]
    pub actions: Vec<Action>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guitar {
    /// A riff from `[riffs]`, looped.
    pub riff: Option<String>,
    /// Or one string being tuned: `"5:0"`.
    pub tune: Option<String>,
    /// Tuning glide, cents from the note: `[from, to]`.
    #[serde(default = "default_cents")]
    pub cents: [f32; 2],
    /// Which recorded player plays it (0 or 1).
    #[serde(default)]
    pub take: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backing {
    pub riff: String,
    pub preset: String,
    /// Two takes panned left and right, or one in the center.
    #[serde(default = "default_true")]
    pub double: bool,
    /// Relative to the main guitar.
    #[serde(default)]
    pub gain_db: f32,
}

/// One thing that happens at scene time `at` seconds (or `beat`, counted
/// from 0 at the scene start). Set exactly one of the verbs.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    #[serde(default)]
    pub at: f32,
    pub beat: Option<f32>,
    /// Clicks the widget with this label (the cursor glides there first;
    /// the click lands at `at`).
    pub click: Option<String>,
    /// Drags the widget with this label by `dx`/`dy` points over `secs`.
    pub drag: Option<String>,
    /// Moves the cursor onto a widget without clicking.
    pub hover: Option<String>,
    /// Presses a key: egui names, "Space", "F1", "PageDown", "K"...
    pub key: Option<String>,
    /// Switches page directly (no click).
    pub page: Option<String>,
    /// Scrolls by this many points (positive = down) where the cursor is.
    pub scroll: Option<f32>,
    /// Zooms the view onto `[x, y, width, height]` (app points) over `secs`.
    pub zoom: Option<[f32; 4]>,
    /// Zooms back out over `secs`.
    #[serde(default)]
    pub unzoom: bool,
    /// The n-th widget with that label, when several match (0 = first).
    #[serde(default)]
    pub nth: usize,
    /// Match labels that contain the text instead of equal it.
    #[serde(default)]
    pub contains: bool,
    #[serde(default)]
    pub dx: f32,
    #[serde(default)]
    pub dy: f32,
    #[serde(default = "default_secs")]
    pub secs: f32,
}

fn default_width() -> u32 {
    1920
}
fn default_height() -> u32 {
    1080
}
fn default_fps() -> u32 {
    30
}
fn default_output() -> String {
    "out/promo.mp4".into()
}
fn default_fade() -> f32 {
    0.3
}
fn default_guitar_db() -> f32 {
    -19.0
}
fn default_band_db() -> f32 {
    -21.0
}
fn default_true() -> bool {
    true
}
fn default_step() -> u32 {
    8
}
fn default_cents() -> [f32; 2] {
    [-40.0, 0.0]
}
fn default_secs() -> f32 {
    0.8
}

/// `~/x` -> `$HOME/x`.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

impl Storyboard {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut board: Storyboard = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let beat = 60.0 / board.music.bpm as f32;
        for s in &mut board.scenes {
            for a in s.actions.iter_mut().chain(s.setup.iter_mut()) {
                if let Some(b) = a.beat {
                    a.at = b * beat;
                }
            }
        }
        board.check()?;
        Ok(board)
    }

    pub fn bar_secs(&self) -> f64 {
        4.0 * 60.0 / self.music.bpm
    }

    pub fn duration(&self, scene: &Scene) -> f32 {
        (scene.bars as f64 * self.bar_secs()) as f32
    }

    pub fn riff(&self, name: &str) -> Riff {
        let r = &self.riffs[name];
        let tuning: &[u8] = match r.instrument {
            Instrument::Guitar => &GUITAR,
            Instrument::Bass => &BASS,
        };
        Riff::parse(self.music.bpm, r.step, &r.notes, tuning, r.ring).expect("checked when loading")
    }

    pub fn drum_pattern(&self, name: &str) -> Drums {
        let d = &self.drums[name];
        let lanes: Vec<(String, String)> = d.lanes.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        Drums::parse(self.music.bpm, d.step, &lanes).expect("checked when loading")
    }

    fn check(&self) -> Result<(), String> {
        for (name, r) in &self.riffs {
            let tuning: &[u8] = if r.instrument == Instrument::Bass { &BASS } else { &GUITAR };
            Riff::parse(self.music.bpm, r.step, &r.notes, tuning, r.ring).map_err(|e| format!("riff '{name}': {e}"))?;
        }
        for (name, d) in &self.drums {
            let lanes: Vec<(String, String)> = d.lanes.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            Drums::parse(self.music.bpm, d.step, &lanes).map_err(|e| format!("drums '{name}': {e}"))?;
        }
        let mut ids = std::collections::HashSet::new();
        for s in &self.scenes {
            let at = |what: &str| format!("scene '{}': {what}", s.id);
            if !ids.insert(&s.id) {
                return Err(at("duplicate id"));
            }
            if s.bars <= 0.0 {
                return Err(at("bars must be > 0"));
            }
            let duration = self.duration(s);
            if let Some(p) = &s.page
                && guitar_core::frontend::gui::Page::from_label(p).is_none()
            {
                return Err(at(&format!("unknown page '{p}'")));
            }
            let presets = s.preset.iter().chain(s.backing.iter().map(|b| &b.preset));
            for name in presets {
                if guitar_core::backend::offline::factory_preset(name).is_none() {
                    return Err(at(&format!("unknown factory preset '{name}'")));
                }
            }
            let riffs = s.guitar.as_ref().and_then(|g| g.riff.as_ref()).into_iter().chain(s.backing.iter().map(|b| &b.riff)).chain(s.bass.iter());
            for riff in riffs {
                if !self.riffs.contains_key(riff) {
                    return Err(at(&format!("unknown riff '{riff}'")));
                }
            }
            if let Some(b) = &s.bass
                && self.riffs[b].instrument != Instrument::Bass
            {
                return Err(at(&format!("riff '{b}' is not a bass riff")));
            }
            if let Some(d) = &s.drums
                && !self.drums.contains_key(d)
            {
                return Err(at(&format!("unknown drum pattern '{d}'")));
            }
            if let Some(t) = s.guitar.as_ref().and_then(|g| g.tune.as_ref()) {
                crate::riff::parse_position(t, 6).map_err(|e| at(&e))?;
            }
            if s.layout == Layout::Title && !(s.actions.is_empty() && s.setup.is_empty()) {
                return Err(at("title scenes have no app to act on"));
            }
            if s.tab && s.guitar.as_ref().and_then(|g| g.riff.as_ref()).is_none() {
                return Err(at("tab = true needs a guitar riff"));
            }
            for a in s.actions.iter().chain(&s.setup) {
                let verbs = [a.click.is_some(), a.drag.is_some(), a.hover.is_some(), a.key.is_some(), a.page.is_some(), a.scroll.is_some(), a.zoom.is_some(), a.unzoom];
                if verbs.iter().filter(|v| **v).count() != 1 {
                    return Err(at(&format!("action at {}: set exactly one of click, drag, hover, key, page, scroll, zoom, unzoom", a.at)));
                }
                if a.at < 0.0 || a.at > duration {
                    return Err(at(&format!("action at {}s is outside the scene ({duration:.1}s)", a.at)));
                }
                if let Some(k) = &a.key
                    && eframe::egui::Key::from_name(k).is_none()
                {
                    return Err(at(&format!("unknown key '{k}'")));
                }
            }
        }
        Ok(())
    }
}

/// Replaces `{pedal_count}`, `{preset_count}` and `{page_count}` with the
/// app's current numbers, so captions stay true as features are added.
pub fn expand(text: &str) -> String {
    use strum::IntoEnumIterator;
    text.replace("{pedal_count}", &guitar_core::shared::pedals::PedalDescription::iter().count().to_string())
        .replace("{preset_count}", &guitar_core::shared::factory_presets::factory_presets().len().to_string())
        .replace("{page_count}", &guitar_core::frontend::gui::Page::ALL.len().to_string())
}
