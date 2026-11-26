//! "Tabs" page: open a Guitar Pro file, see its tablature with a playback
//! cursor, and play it with the SoundFont synth (per-track mute/solo) and/or
//! an original recording synchronized to the score.
//!
//! File parsing, audio decoding and SoundFont loading run on background
//! threads; results are polled once per frame.
//!
//! Practice mode (`frontend::lib::practice`) listens to the guitar input
//! while the song plays and colors each note of the selected track green
//! (played right) or red (wrong or missing).
//!
//! The "Timbre" row suggests a pedal chain for the selected track (see
//! `shared::tab::tone`); applying it is returned to the app from [`TabPlayerPanel::ui`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam::channel::{bounded, Receiver};
use eframe::egui::{self, Color32, Key, RichText, Ui};
use rustysynth::SoundFont;

use crate::frontend::lib::tunings::{Tuning, label as tuning_label};
use crate::frontend::style::{self, Palette};
use crate::frontend::ui::soundfont::{self, CardAction};
use crate::frontend::ui::tab_start::{push_recent, TabStartPage};
use crate::frontend::ui::tab_view::TabOverlay;

use crate::backend::audio_file::{self, DecodedAudio, AUDIO_EXTENSIONS};
use crate::backend::song_player::SongPlayer;
use crate::frontend::lib::practice::{expected_beats, PracticeSession, PracticeSettings};
use crate::frontend::ui::tab_view::{tab_view, CursorPos, TabLayout};
use crate::frontend::ui::transcribe::TranscribePanel;
use crate::shared::config::GLOBAL_CONFIG;
use crate::shared::i18n::tr;
use crate::trf;
use crate::shared::factory_presets::factory_presets;
use crate::shared::pedals::PedalInstance;
use crate::shared::preset::{self, Preset};
use crate::shared::settings::{save_settings, AppSettings, PracticePrefs, PresetRef, TabSyncSettings};
use crate::shared::tab::tone::{ESTIMATED_TAG, guess_tone, preset_fits_song, preset_fits_track};
use crate::shared::tab::import::{load_score_file, SUPPORTED_EXTENSIONS};
use crate::shared::tab::repository;
use crate::shared::tab::model::Score;
use crate::shared::tab::timeline::{SyncMap, Timeline};

/// How often to look for a SoundFont again while the Tabs page says
/// there's none (a listing of a few folders, see `ui::soundfont`).
const SOUNDFONT_RECHECK: Duration = Duration::from_secs(4);

struct LoadedTab {
    path: PathBuf,
    score: Arc<Score>,
    timeline: Arc<Timeline>,
}

#[derive(Clone, Debug, PartialEq)]
enum BackingSource {
    File(PathBuf),
    /// Audio embedded in a Guitar Pro 8 file.
    Embedded,
}

struct Backing {
    source: BackingSource,
    duration_secs: f64,
}

pub(crate) type Pending<T> = Option<Receiver<Result<T, String>>>;

pub(crate) fn spawn<T: Send + 'static>(job: impl FnOnce() -> Result<T, String> + Send + 'static) -> Receiver<Result<T, String>> {
    let (tx, rx) = bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send(job());
    });
    rx
}

pub(crate) fn poll<T>(pending: &mut Pending<T>) -> Option<Result<T, String>> {
    let result = pending.as_ref()?.try_recv().ok()?;
    *pending = None;
    Some(result)
}

fn format_time(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Width of `text` drawn as a button label.
fn text_width(ui: &Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    ui.fonts(|f| f.layout_no_wrap(text.to_owned(), font, Color32::WHITE).size().x)
}

/// In a `horizontal_wrapped` row, starts a new row unless `width` points
/// still fit on this one. egui wraps plain widgets by itself, but not
/// widgets drawn in a child `Ui` (sliders, combo boxes, groups): those run
/// off the right edge instead.
fn keep_together(ui: &mut Ui, width: f32) {
    let row_started = ui.cursor().left() > ui.max_rect().left() + 0.5;
    if row_started && ui.available_size_before_wrap().x < width {
        ui.end_row();
    }
}

/// A vertical separator between groups of a wrapped row. Left out on
/// phones, where rows wrap after almost every group and would end in a
/// dangling line.
fn row_separator(ui: &mut Ui) {
    if ui.ctx().screen_rect().width() >= style::NARROW_WIDTH {
        ui.separator();
    }
}

/// A slider with its label, kept on one row of a wrapped layout.
fn labeled_slider(ui: &mut Ui, slider: egui::Slider, label: &str) -> egui::Response {
    labeled_slider_enabled(ui, true, slider, label)
}

fn labeled_slider_enabled(ui: &mut Ui, enabled: bool, slider: egui::Slider, label: &str) -> egui::Response {
    let spacing = ui.spacing();
    // Rail, value field (about `interact_size.x` plus padding) and label.
    let width = spacing.slider_width + spacing.interact_size.x + 2.0 * spacing.button_padding.x + 2.0 * spacing.item_spacing.x + text_width(ui, label);
    keep_together(ui, width);
    ui.add_enabled(enabled, slider.text(label))
}

/// A small button for the info row; full size on touch screens.
fn chip(ui: &mut Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    if ui.ctx().screen_rect().width() < style::NARROW_WIDTH { ui.button(text) } else { ui.small_button(text) }
}

/// A compact card for the Loop, Practice and Mixer panels.
fn panel_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let palette = Palette::of(ui.ctx());
    ui.add_space(style::space::S);
    style::card_frame(&palette)
        .inner_margin(egui::Margin::same(style::space::S as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.contains(&e.to_ascii_lowercase().as_str()))
}

fn tab_key(path: &Path) -> String {
    path.display().to_string()
}

pub struct TabPlayerPanel {
    player: SongPlayer,
    tab: Option<LoadedTab>,
    loading_tab: Pending<LoadedTab>,
    loading_soundfont: Pending<(PathBuf, Arc<SoundFont>)>,
    loading_backing: Pending<(BackingSource, Arc<DecodedAudio>)>,
    soundfont_name: Option<String>,
    message: Option<(String, bool)>,
    selected_track: usize,
    layout: Option<(usize, f32, TabLayout)>,
    follow_cursor: bool,
    speed: f32,
    synth_gain: f32,
    backing_gain: f32,
    muted: Vec<bool>,
    soloed: Vec<bool>,
    backing: Option<Backing>,
    use_backing: bool,
    /// Manual sync (used for user-loaded audio files).
    sync_offset: f64,
    tempo_ratio: f64,
    /// Sync from the file itself (Guitar Pro 8 embedded audio).
    file_sync: Option<SyncMap>,
    show_mixer: bool,
    practice_on: bool,
    /// Built for `(tab path, track index)`; rebuilt when either changes.
    practice: Option<(PathBuf, usize, PracticeSession)>,
    practice_prefs: PracticePrefs,
    /// User presets whose song/artist metadata names the open tab's song.
    song_presets: Vec<(Preset, PathBuf)>,
    loading_song_presets: Pending<Vec<(Preset, PathBuf)>>,
    transcriber: TranscribePanel,
    /// Audio file and bar-accurate sync from a transcribed tab's sidecar.
    sidecar_sync: Option<(PathBuf, SyncMap)>,
    practice_loop: PracticeLoop,
    /// Count-in before playback, in bars (0 = off).
    count_in_bars: u32,
    show_heatmap: bool,
    /// The user asked to tune to this tab (see [`TabPlayerPanel::take_tuner_request`]).
    tuner_requested: bool,
    /// Playing state last frame, to notice the end of a run.
    was_playing: bool,
    /// What the page shows while no tab is open.
    start: TabStartPage,
    /// Looking for a SoundFont on a worker thread (`ui::soundfont`).
    soundfont_search: Pending<Option<PathBuf>>,
    last_soundfont_search: Option<Instant>,
    /// The SoundFont being loaded was found by the search, not picked.
    auto_soundfont: Option<PathBuf>,
    /// Found SoundFonts that failed to load, not to be picked again.
    rejected_soundfonts: Vec<PathBuf>,
    /// For the app's toast (see [`TabPlayerPanel::take_notice`]).
    notice: Option<String>,
    #[cfg(target_os = "android")]
    path_input: String,
}

/// A-B loop over bars plus the speed trainer.
#[derive(Clone, Debug, PartialEq)]
struct PracticeLoop {
    on: bool,
    /// First and last bar, 0-based, inclusive.
    bars: (usize, usize),
    trainer: bool,
    /// Speed the trainer starts at, then raises by `step` after each good
    /// pass until `target`.
    start_speed: f32,
    step: f32,
    target_speed: f32,
    /// Accuracy a pass needs (with practice mode on) to speed up.
    pass_accuracy: f32,
    /// `PlayerStatus::loop_passes` already handled.
    passes_seen: u32,
    good_passes: u32,
}

impl Default for PracticeLoop {
    fn default() -> Self {
        Self {
            on: false,
            bars: (0, 0),
            trainer: false,
            start_speed: 0.6,
            step: 0.05,
            target_speed: 1.0,
            pass_accuracy: 0.9,
            passes_seen: 0,
            good_passes: 0,
        }
    }
}

/// Score-time range covering bars `first..=last`: the occurrence of `first`
/// nearest to `near_secs`, through the first occurrence of `last` after it.
fn loop_secs(timeline: &Timeline, (first, last): (usize, usize), near_secs: f64) -> Option<(f64, f64)> {
    let (first, last) = (first.min(last), first.max(last));
    let start = timeline.closest_occurrence(first, near_secs)?;
    let start_occ = &timeline.occurrences[start];
    let end = timeline.occurrences[start..].iter().find(|o| o.bar == last)?;
    Some((start_occ.start_secs, end.end_secs))
}

/// Bars sorted from least to most accurate (only bars with judged notes).
fn hardest_bars(results: &std::collections::BTreeMap<usize, (u32, u32)>, min_notes: u32) -> Vec<(usize, f32)> {
    let mut bars: Vec<(usize, f32)> = results
        .iter()
        .filter(|(_, (_, judged))| *judged >= min_notes)
        .map(|(&bar, &(hit, judged))| (bar, hit as f32 / judged as f32))
        .collect();
    bars.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    bars
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// User presets made for this song (or artist), read on a worker thread.
fn find_song_presets(title: String, artist: String) -> Result<Vec<(Preset, PathBuf)>, String> {
    let Some(dir) = preset::user_preset_dir() else { return Ok(Vec::new()) };
    Ok(preset::list_presets(&dir)
        .into_iter()
        .filter_map(|listing| {
            let loaded = preset::load_from_file(&listing.path).ok()?;
            preset_fits_song(&loaded, &title, &artist).then_some((loaded, listing.path))
        })
        .collect())
}

impl TabPlayerPanel {
    pub fn new(player: SongPlayer, settings: &AppSettings) -> Self {
        let mut panel = Self {
            player: player.clone(),
            tab: None,
            loading_tab: None,
            loading_soundfont: None,
            loading_backing: None,
            soundfont_name: None,
            message: None,
            selected_track: 0,
            layout: None,
            follow_cursor: true,
            speed: 1.0,
            synth_gain: 1.0,
            backing_gain: 1.0,
            muted: Vec::new(),
            soloed: Vec::new(),
            backing: None,
            use_backing: true,
            sync_offset: 0.0,
            tempo_ratio: 1.0,
            file_sync: None,
            show_mixer: true,
            practice_on: false,
            practice: None,
            practice_prefs: settings.practice.clone(),
            song_presets: Vec::new(),
            loading_song_presets: None,
            transcriber: TranscribePanel::new(player.clone()),
            sidecar_sync: None,
            practice_loop: PracticeLoop::default(),
            count_in_bars: 0,
            show_heatmap: true,
            tuner_requested: false,
            was_playing: false,
            start: TabStartPage::default(),
            soundfont_search: None,
            last_soundfont_search: None,
            auto_soundfont: None,
            rejected_soundfonts: Vec::new(),
            notice: None,
            #[cfg(target_os = "android")]
            path_input: String::new(),
        };
        match settings.soundfont_path.clone().filter(|p| p.is_file()) {
            Some(path) => panel.load_soundfont(path, false),
            None => panel.start_soundfont_search(),
        }
        if let Some(path) = settings.last_tab.clone().filter(|p| p.is_file()) {
            panel.load_tab(path);
        }
        panel
    }

    fn set_message(&mut self, text: impl Into<String>, is_error: bool) {
        self.message = Some((text.into(), is_error));
    }

    /// Opens a tab file (e.g. one picked on the Spotify page).
    pub fn open_tab(&mut self, path: PathBuf) {
        self.load_tab(path);
    }

    /// Tuning of the open tab's selected track (string 1 = highest first),
    /// for the tuner's "tune to the tab" target.
    pub fn selected_tuning(&self) -> Option<Vec<u8>> {
        let track = self.tab.as_ref()?.score.tracks.get(self.selected_track)?;
        (!track.percussion && !track.tuning.is_empty()).then(|| track.tuning.clone())
    }

    /// Replaces the practice knobs (e.g. after a latency measurement).
    pub fn set_practice_prefs(&mut self, prefs: &PracticePrefs) {
        self.practice_prefs = prefs.clone();
    }

    /// A message for the app's toast, once (e.g. which SoundFont was found).
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// `true` once after the user pressed "tune to this tab".
    pub fn take_tuner_request(&mut self) -> bool {
        std::mem::take(&mut self.tuner_requested)
    }

    /// Applies the loop range to the player (or clears it).
    fn apply_loop(&mut self) {
        let Some(tab) = &self.tab else { return };
        let range = if self.practice_loop.on {
            loop_secs(&tab.timeline, self.practice_loop.bars, self.player.status().score_secs())
        } else {
            None
        };
        self.player.set_loop(range);
    }

    /// Loops `first..=last` (0-based bars).
    fn loop_bars(&mut self, first: usize, last: usize) {
        self.practice_loop.on = true;
        self.practice_loop.bars = (first.min(last), first.max(last));
        self.practice_loop.passes_seen = self.player.status().loop_passes();
        self.apply_loop();
    }

    /// Seconds between count-in clicks: one beat of the bar at the cursor,
    /// as heard (synth speed or the recording's tempo).
    fn count_in_beat_secs(&self) -> f64 {
        let Some(tab) = &self.tab else { return 0.5 };
        let status = self.player.status();
        let Some(occ) = tab.timeline.occurrence_at(status.score_secs()).map(|i| &tab.timeline.occurrences[i]) else {
            return 0.5;
        };
        let numerator = tab.score.bars.get(occ.bar).map(|b| b.numerator.max(1) as f64).unwrap_or(4.0);
        let beat = (occ.end_secs - occ.start_secs) / numerator;
        if self.use_backing && self.backing.is_some() {
            let sync = self.active_sync();
            (sync.score_to_audio(occ.start_secs + beat) - sync.score_to_audio(occ.start_secs)).abs()
        } else {
            beat / self.speed.max(0.05) as f64
        }
    }

    fn count_in_beats(&self) -> u32 {
        let Some(tab) = &self.tab else { return 0 };
        let bar = tab.timeline.occurrence_at(self.player.status().score_secs()).map(|i| tab.timeline.occurrences[i].bar);
        let numerator = bar.and_then(|b| tab.score.bars.get(b)).map(|b| b.numerator.max(1)).unwrap_or(4);
        numerator * self.count_in_bars
    }

    /// Handles finished loop passes: the speed trainer and per-pass marks.
    fn on_loop_passes(&mut self) {
        let passes = self.player.status().loop_passes();
        if passes == self.practice_loop.passes_seen {
            return;
        }
        self.practice_loop.passes_seen = passes;
        let (first, last) = self.practice_loop.bars;
        // Accuracy of the pass that just ended, from practice mode.
        let accuracy = self.practice.as_ref().filter(|_| self.practice_on).and_then(|(_, _, session)| {
            let results = session.bar_results();
            let (hit, judged) = results
                .range(first..=last)
                .fold((0, 0), |(h, j), (_, (bh, bj))| (h + bh, j + bj));
            (judged > 0).then(|| hit as f32 / judged as f32)
        });
        let backing_on = self.use_backing && self.backing.is_some();
        let good = accuracy.is_none_or(|a| a >= self.practice_loop.pass_accuracy);
        if good {
            self.practice_loop.good_passes += 1;
        }
        if self.practice_loop.trainer && !backing_on && good && self.speed < self.practice_loop.target_speed {
            self.speed = (self.speed + self.practice_loop.step).min(self.practice_loop.target_speed);
            self.player.set_speed(self.speed as f64);
            self.set_message(trf!("Speed trainer: {:.0}%", "Treino de velocidade: {:.0}%", self.speed * 100.0), false);
        } else if let Some(a) = accuracy
            && !good
        {
            self.set_message(trf!("Pass at {:.0}%: repeat at this speed.", "Passada com {:.0}%: repita nesta velocidade.", a * 100.0), false);
        }
        if let Some((_, _, session)) = &mut self.practice {
            session.clear_marks_in(first..=last);
        }
    }

    /// Records a finished practice run in the history.
    fn record_run(&mut self, settings: &mut AppSettings) {
        let Some(tab) = &self.tab else { return };
        let Some((_, track_index, session)) = &self.practice else { return };
        let stats = session.stats();
        let Some(accuracy) = stats.note_accuracy().filter(|_| stats.notes_judged >= 8) else { return };
        let track_name = tab.score.tracks.get(*track_index).map(|t| t.name.clone()).unwrap_or_default();
        let record = settings.practice_log.entry(tab_key(&tab.path)).or_default();
        record.runs += 1;
        record.last_run_unix = unix_now();
        let best = record.best_accuracy.entry(track_name).or_insert(0.0);
        *best = best.max(accuracy as f32);
        save_settings(settings);
    }

    /// Opens the transcription form, prefilled (e.g. from a Spotify track).
    pub fn start_transcription_for(
        &mut self,
        title: String,
        artist: String,
        spotify_track_id: Option<String>,
        duration_secs: Option<f64>,
    ) {
        self.transcriber.show_form(Some(title), Some(artist), spotify_track_id, duration_secs);
    }

    fn on_transcribed(&mut self, output: crate::transcribe::Output, settings: &mut AppSettings) {
        if let Some(id) = &output.metadata.spotify_track_id {
            settings.spotify_tab_links.insert(id.clone(), output.tab_path.clone());
            save_settings(settings);
        }
        let tracks: Vec<String> = output
            .metadata
            .tracks
            .iter()
            .map(|t| trf!("{} ({} notes, tuning {})", "{} ({} notas, afinação {})", t.name, t.notes, crate::frontend::lib::tunings::display_name(&t.tuning_name)))
            .collect();
        self.set_message(
            trf!(
                "Tab created{}: {} · {:.0} bpm · {}",
                "Tab gerada{}: {} · {:.0} bpm · {}",
                if output.replaced { tr(" again", " de novo") } else { "" },
                tracks.join(", "),
                output.metadata.tempo_bpm,
                output.tab_path.display()
            ),
            false,
        );
        self.load_tab(output.tab_path);
    }

    fn sidecar_sync_for(&self, path: &Path) -> Option<&SyncMap> {
        self.sidecar_sync.as_ref().filter(|(p, _)| p == path).map(|(_, s)| s)
    }

    fn load_tab(&mut self, path: PathBuf) {
        self.loading_tab = Some(spawn(move || {
            let score = load_score_file(&path)?;
            let timeline = Timeline::build(&score);
            Ok(LoadedTab { path, score: Arc::new(score), timeline: Arc::new(timeline) })
        }));
    }

    /// Loads a SoundFont; `auto` when the search found it (the user didn't pick it).
    fn load_soundfont(&mut self, path: PathBuf, auto: bool) {
        self.auto_soundfont = auto.then(|| path.clone());
        self.loading_soundfont = Some(spawn(move || {
            let mut file = std::fs::File::open(&path)
                .map_err(|e| trf!("Couldn't open the SoundFont: {e}", "Não foi possível abrir o SoundFont: {e}"))?;
            let sf = SoundFont::new(&mut file).map_err(|e| trf!("Invalid SoundFont: {e:?}", "SoundFont inválido: {e:?}"))?;
            Ok((path, Arc::new(sf)))
        }));
    }

    fn start_soundfont_search(&mut self) {
        self.last_soundfont_search = Some(Instant::now());
        let skip = self.rejected_soundfonts.clone();
        self.soundfont_search = Some(spawn(move || Ok(soundfont::find_soundfont(&skip))));
    }

    /// While a tab is open without a SoundFont, looks again every few
    /// seconds (one may have just been downloaded). Stops once one loads.
    fn keep_looking_for_soundfont(&mut self, ctx: &egui::Context) {
        if self.tab.is_none() || self.soundfont_name.is_some() || self.loading_soundfont.is_some() {
            return;
        }
        if self.soundfont_search.is_none() && self.last_soundfont_search.is_none_or(|t| t.elapsed() >= SOUNDFONT_RECHECK) {
            self.start_soundfont_search();
        }
        ctx.request_repaint_after(if self.soundfont_search.is_some() { Duration::from_millis(200) } else { SOUNDFONT_RECHECK });
    }

    fn close_tab(&mut self, settings: &mut AppSettings) {
        self.player.stop();
        self.player.set_loop(None);
        self.player.set_backing(None, SyncMap::default());
        self.tab = None;
        self.loading_tab = None;
        self.backing = None;
        self.practice = None;
        self.practice_loop = PracticeLoop::default();
        self.layout = None;
        self.message = None;
        settings.last_tab = None;
        save_settings(settings);
    }

    fn load_backing_file(&mut self, path: PathBuf) {
        self.loading_backing = Some(spawn(move || {
            let audio = audio_file::decode_file(&path)?;
            Ok((BackingSource::File(path), Arc::new(audio)))
        }));
    }

    fn manual_sync(&self) -> SyncMap {
        SyncMap::Linear { offset_secs: self.sync_offset, tempo_ratio: self.tempo_ratio }
    }

    fn active_sync(&self) -> SyncMap {
        match (&self.backing, &self.file_sync) {
            (Some(Backing { source: BackingSource::Embedded, .. }), Some(sync)) => sync.clone(),
            (Some(Backing { source: BackingSource::File(path), .. }), _) => {
                self.sidecar_sync_for(path).cloned().unwrap_or_else(|| self.manual_sync())
            }
            _ => self.manual_sync(),
        }
    }

    fn track_enabled(&self) -> Vec<bool> {
        let any_solo = self.soloed.iter().any(|s| *s);
        self.muted
            .iter()
            .zip(&self.soloed)
            .map(|(&m, &s)| if any_solo { s } else { !m })
            .collect()
    }

    /// Saves the manual sync/audio choice for the current tab.
    fn persist_tab_sync(&self, settings: &mut AppSettings) {
        let Some(tab) = &self.tab else { return };
        let audio_path = match &self.backing {
            Some(Backing { source: BackingSource::File(p), .. }) => Some(p.clone()),
            _ => None,
        };
        settings.tab_sync.insert(
            tab_key(&tab.path),
            TabSyncSettings { audio_path, offset_secs: self.sync_offset, tempo_ratio: self.tempo_ratio },
        );
        save_settings(settings);
    }

    fn on_tab_loaded(&mut self, tab: LoadedTab, settings: &mut AppSettings) {
        let score = &tab.score;
        self.player.set_song(tab.timeline.clone(), tab.timeline.channel_setup(score), score.tracks.len());
        self.player.set_backing(None, SyncMap::default());
        self.selected_track = score.tracks.iter().position(|t| !t.percussion).unwrap_or(0);
        self.muted = vec![false; score.tracks.len()];
        self.soloed = vec![false; score.tracks.len()];
        self.layout = None;
        self.practice = None;
        self.practice_loop = PracticeLoop::default();
        self.song_presets.clear();
        let (title, artist) = (score.title.clone(), score.artist.clone());
        self.loading_song_presets = Some(spawn(move || find_song_presets(title, artist)));
        self.backing = None;
        self.file_sync = SyncMap::from_file_points(&score.sync_points, &tab.timeline);

        let saved = settings.tab_sync.get(&tab_key(&tab.path)).cloned().unwrap_or_default();
        self.sync_offset = saved.offset_secs;
        self.tempo_ratio = saved.tempo_ratio;
        // A transcribed tab knows its recording and where every bar starts.
        self.sidecar_sync = repository::load_metadata(&tab.path).and_then(|meta| {
            let audio = meta.source.path.clone().filter(|p| p.is_file())?;
            Some((audio, meta.sync_map(&tab.timeline)?))
        });
        if let Some(bytes) = score.backing_audio.clone() {
            self.loading_backing = Some(spawn(move || {
                let audio = audio_file::decode_bytes(bytes)?;
                Ok((BackingSource::Embedded, Arc::new(audio)))
            }));
        } else if let Some(path) = saved.audio_path.filter(|p| p.is_file()) {
            self.load_backing_file(path);
        } else if let Some((path, _)) = self.sidecar_sync.clone() {
            self.load_backing_file(path);
        }

        let title = if score.title.trim().is_empty() {
            tab.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else if score.artist.is_empty() {
            score.title.clone()
        } else {
            format!("{} - {}", score.artist, score.title)
        };
        self.set_message(trf!("Opened: {title}", "Aberto: {title}"), false);
        settings.last_tab = Some(tab.path.clone());
        push_recent(&mut settings.recent_tabs, &tab.path);
        save_settings(settings);
        self.tab = Some(tab);
    }

    fn poll_background(&mut self, settings: &mut AppSettings) {
        match poll(&mut self.loading_tab) {
            Some(Ok(tab)) => self.on_tab_loaded(tab, settings),
            Some(Err(e)) => self.set_message(e, true),
            None => {}
        }
        if let Some(Ok(Some(path))) = poll(&mut self.soundfont_search)
            && self.soundfont_name.is_none()
            && self.loading_soundfont.is_none()
        {
            self.load_soundfont(path, true);
        }
        match poll(&mut self.loading_soundfont) {
            Some(Ok((path, sf))) => {
                self.player.set_soundfont(sf);
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if self.auto_soundfont.take().is_some() {
                    let text = trf!("SoundFont found: {name}", "SoundFont encontrado: {name}");
                    self.set_message(text.clone(), false);
                    self.notice = Some(text);
                }
                self.soundfont_name = Some(name);
                if settings.soundfont_path.as_ref() != Some(&path) {
                    settings.soundfont_path = Some(path);
                    save_settings(settings);
                }
            }
            Some(Err(e)) => match self.auto_soundfont.take() {
                Some(path) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.set_message(trf!("Skipped the SoundFont {name}: {e}", "SoundFont {name} ignorado: {e}"), true);
                    self.rejected_soundfonts.push(path);
                }
                None => self.set_message(e, true),
            },
            None => {}
        }
        if let Some(Ok(found)) = poll(&mut self.loading_song_presets) {
            self.song_presets = found;
        }
        match poll(&mut self.loading_backing) {
            Some(Ok((source, audio))) => {
                self.backing = Some(Backing { source, duration_secs: audio.duration_secs() });
                let sync = self.active_sync();
                self.player.set_backing(Some(audio), sync);
                self.player.set_use_backing(self.use_backing);
                self.persist_tab_sync(settings);
            }
            Some(Err(e)) => self.set_message(e, true),
            None => {}
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for path in dropped {
            if has_extension(&path, SUPPORTED_EXTENSIONS) {
                self.load_tab(path);
            } else if has_extension(&path, AUDIO_EXTENSIONS) {
                if self.tab.is_some() {
                    self.load_backing_file(path);
                }
            } else if has_extension(&path, &["sf2"]) {
                self.load_soundfont(path, false);
            }
        }
    }

    /// Practice settings in score seconds. `audio_latency_secs` is the
    /// stream's round-trip latency; the synth's speed setting stretches it.
    fn practice_settings(&self, audio_latency_secs: f64) -> PracticeSettings {
        let backing_on = self.use_backing && self.backing.is_some();
        let speed = if backing_on { 1.0 } else { self.speed as f64 };
        let prefs = &self.practice_prefs;
        PracticeSettings {
            latency_secs: (audio_latency_secs + prefs.extra_latency_ms as f64 / 1000.0) * speed,
            tolerance_secs: prefs.tolerance_ms as f64 / 1000.0 * speed,
            strictness: prefs.strictness as f64,
        }
    }

    /// Makes sure the practice session matches the open tab and track.
    fn ensure_practice(&mut self) {
        if !self.practice_on {
            self.practice = None;
            return;
        }
        let Some(tab) = &self.tab else { return };
        let up_to_date = matches!(&self.practice, Some((path, track, _)) if *path == tab.path && *track == self.selected_track);
        if up_to_date {
            return;
        }
        let Some(track) = tab.score.tracks.get(self.selected_track) else { return };
        let rate = match self.player.status().output_rate() {
            0 => GLOBAL_CONFIG.sample_rate as f64,
            rate => rate as f64,
        };
        let beats = expected_beats(track, &tab.timeline);
        let settings = self.practice_settings(0.0);
        self.practice = Some((tab.path.clone(), self.selected_track, PracticeSession::new(beats, rate, settings)));
    }

    /// Hands the guitar samples that arrived this frame to practice mode.
    /// Call every frame, also while another page is shown.
    pub fn feed_input(&mut self, samples: &[f32], audio_latency_secs: f32) {
        let rate = match self.player.status().output_rate() {
            0 => GLOBAL_CONFIG.sample_rate as u32,
            rate => rate,
        };
        self.transcriber.feed_input(samples, rate);
        // Loop passes count on every page, not just while Tabs is shown.
        self.on_loop_passes();
        if !self.practice_on {
            return;
        }
        self.ensure_practice();
        let settings = self.practice_settings(audio_latency_secs as f64);
        let status = self.player.status();
        let (score_secs, playing) = (status.score_secs(), status.playing() && !status.counting_in());
        if let Some((_, _, session)) = &mut self.practice {
            session.settings = settings;
            session.feed(samples, score_secs, playing);
        }
    }

    fn set_practice(&mut self, on: bool) {
        self.practice_on = on;
        if !on {
            self.practice = None;
            return;
        }
        // The synth playing the same part would be "heard" as the guitar
        // through a microphone, so mute it.
        if let Some(muted) = self.muted.get_mut(self.selected_track)
            && !*muted
        {
            *muted = true;
            self.player.set_track_enabled(self.track_enabled());
            self.set_message(
                tr(
                    "Practice mode: the selected track is muted in the synth.",
                    "Modo prática: a faixa selecionada foi mutada no sintetizador.",
                ),
                false,
            );
        }
        self.ensure_practice();
    }

    fn toggle_play(&self) {
        if self.player.status().playing() {
            self.player.pause();
        } else if self.count_in_bars > 0 {
            self.player.play_with_count_in(self.count_in_beats(), self.count_in_beat_secs());
        } else {
            self.player.play();
        }
    }

    /// Draws the page. Returns a pedal chain to load when the user applies
    /// a tone suggestion.
    pub fn ui(&mut self, ui: &mut Ui, settings: &mut AppSettings) -> Option<(Vec<PedalInstance>, PresetRef)> {
        self.poll_background(settings);
        self.handle_dropped_files(ui.ctx());
        self.keep_looking_for_soundfont(ui.ctx());

        if self.tab.is_some()
            && !ui.ctx().wants_keyboard_input()
            && ui.input(|i| i.key_pressed(Key::Space))
        {
            self.toggle_play();
        }

        if self.tab.is_none() {
            let mut open = None;
            egui::ScrollArea::vertical().id_salt("tab_start_scroll").auto_shrink([false, false]).show(ui, |ui| {
                self.file_row(ui, settings);
                self.info_row(ui);
                self.transcriber_ui(ui, settings);
                ui.add_space(style::space::M);
                open = self.start.ui(ui, settings);
            });
            if let Some(path) = open {
                self.load_tab(path);
            }
            return None;
        }
        let playing = self.player.status().playing();
        if self.was_playing && !playing && self.practice_on {
            self.record_run(settings);
        }
        self.was_playing = playing;

        // The controls scroll on their own so the tab keeps most of the
        // height, also on a phone with every panel open.
        let max_controls = (ui.available_height() * 0.6).max(160.0);
        let mut tone = None;
        egui::ScrollArea::vertical()
            .id_salt("tab_controls_scroll")
            .max_height(max_controls)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                self.file_row(ui, settings);
                tone = self.info_row(ui);
                self.transcriber_ui(ui, settings);
                ui.add_space(style::space::XS);
                self.transport_row(ui);
                if self.soundfont_name.is_none() && self.loading_soundfont.is_none() {
                    ui.add_space(style::space::S);
                    match soundfont::missing_soundfont_card(ui, self.soundfont_search.is_some()) {
                        CardAction::Pick(path) => self.load_soundfont(path, false),
                        #[cfg(target_os = "android")]
                        CardAction::UsePathField => self.load_soundfont(PathBuf::from(self.path_input.trim()), false),
                        _ => {}
                    }
                }
                if self.practice_loop.on {
                    panel_card(ui, |ui| self.loop_row(ui));
                }
                if self.practice_on {
                    panel_card(ui, |ui| self.practice_row(ui, settings));
                }
                if self.show_mixer {
                    panel_card(ui, |ui| {
                        self.mixer(ui);
                        ui.separator();
                        self.backing_section(ui, settings);
                    });
                }
            });
        ui.add_space(style::space::XS);
        ui.separator();
        self.tab_area(ui);
        tone
    }

    /// The transcription form or progress, and what it finished.
    fn transcriber_ui(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        if let Some(output) = self.transcriber.ui(ui, settings) {
            self.on_transcribed(output, settings);
        }
        if let Some(path) = self.transcriber.take_open_request() {
            self.load_tab(path);
        }
    }

    /// One wrapped row of small items under the header: the last message,
    /// tone suggestions and the track's tuning. Returns a tone to apply.
    fn info_row(&mut self, ui: &mut Ui) -> Option<(Vec<PedalInstance>, PresetRef)> {
        if self.message.is_none() && self.tab.is_none() {
            return None;
        }
        let palette = Palette::of(ui.ctx());
        let mut chosen = None;
        ui.horizontal_wrapped(|ui| {
            let mut items = 0;
            if let Some((text, is_error)) = &self.message {
                let color = if *is_error { palette.danger } else { palette.text_dim };
                ui.label(RichText::new(text).small().color(color));
                items += 1;
            }
            if self.tab.is_some() {
                chosen = self.tone_chips(ui, &mut items);
                self.tuning_chips(ui, &mut items);
            }
        });
        if chosen.is_some() {
            self.set_message(tr("Tone applied to the pedal chain.", "Timbre aplicado à cadeia de pedais."), false);
        }
        chosen
    }

    /// Tone suggestions for the selected track, as chips in the info row.
    fn tone_chips(&self, ui: &mut Ui, items: &mut usize) -> Option<(Vec<PedalInstance>, PresetRef)> {
        let tab = self.tab.as_ref()?;
        let track = tab.score.tracks.get(self.selected_track);
        let guess = track.and_then(guess_tone);
        let track_name = track.map(|t| t.name.as_str()).unwrap_or_default();
        let presets: Vec<&(Preset, PathBuf)> =
            self.song_presets.iter().filter(|(p, _)| preset_fits_track(p, track_name)).collect();
        if guess.is_none() && presets.is_empty() {
            return None;
        }
        if *items > 0 {
            row_separator(ui);
        }
        *items += 1;
        let palette = Palette::of(ui.ctx());
        let mut chosen = None;
        // Keep the label with (about) its first chip.
        keep_together(ui, text_width(ui, tr("💡 Tone:", "💡 Timbre:")) + 120.0);
        ui.add(egui::Label::new(RichText::new(tr("💡 Tone:", "💡 Timbre:")).small().color(palette.text_dim)).extend()).on_hover_text(tr(
            "A starting point for the tone. There's no way to copy the recording's exact tone.",
            "Ponto de partida para o timbre. Não há como copiar o timbre exato da gravação.",
        ));
        for (preset, path) in presets {
            let estimated = preset.tags.iter().any(|t| t == ESTIMATED_TAG);
            let (label, hover) = if estimated {
                (
                    format!("✨ {}", preset.name),
                    tr(
                        "Tone estimated from the recording (experimental): a starting point, adjust by ear",
                        "Timbre estimado da gravação (experimental): um ponto de partida, ajuste de ouvido",
                    ),
                )
            } else {
                (format!("⭐ {}", preset.name), tr("Your preset saved for this song/artist", "Seu preset salvo para esta música/artista"))
            };
            if chip(ui, label).on_hover_text(hover).clicked() {
                chosen = Some((preset.pedals.clone(), PresetRef::User(path.clone())));
            }
        }
        if let Some(guess) = &guess {
            let response = chip(ui, trf!("Apply \"{}\"", "Aplicar \"{}\"", guess.factory_preset)).on_hover_text(guess.reason.as_str());
            if response.clicked()
                && let Some(preset) = factory_presets().into_iter().find(|p| p.name == guess.factory_preset)
            {
                chosen = Some((preset.pedals, PresetRef::Factory(preset.name)));
            }
        }
        chosen
    }

    /// The selected track's tuning, with a shortcut to the tuner.
    fn tuning_chips(&mut self, ui: &mut Ui, items: &mut usize) {
        let Some(tuning) = self.selected_tuning().and_then(|t| Tuning::from_tab(&t)) else { return };
        if *items > 0 {
            row_separator(ui);
        }
        *items += 1;
        let palette = Palette::of(ui.ctx());
        let standard = tuning.strings == Tuning::standard().strings;
        let text = if standard {
            tr("🎸 Standard tuning (E A D G B E)", "🎸 Afinação padrão (E A D G B E)").to_string()
        } else {
            trf!("🎸 Tuning: {} ({})", "🎸 Afinação: {} ({})", tuning.display_name(), tuning_label(&tuning.strings))
        };
        let color = if standard { palette.text_dim } else { palette.warn };
        ui.add(egui::Label::new(RichText::new(text).small().color(color)).extend());
        if chip(ui, tr("🎵 Tune", "🎵 Afinar"))
            .on_hover_text(tr("Opens the tuner with this track's strings", "Abre o afinador com as cordas desta faixa"))
            .clicked()
        {
            self.tuner_requested = true;
        }
    }

    fn file_row(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        let palette = Palette::of(ui.ctx());
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            #[cfg(not(target_os = "android"))]
            if ui.button(tr("📂 Open tab...", "📂 Abrir tab...")).clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Guitar Pro", SUPPORTED_EXTENSIONS)
                    .pick_file()
            {
                self.load_tab(path);
            }
            #[cfg(target_os = "android")]
            {
                ui.label(tr("File:", "Arquivo:"));
                ui.text_edit_singleline(&mut self.path_input);
                if ui.button(tr("Open tab", "Abrir tab")).clicked() {
                    self.load_tab(PathBuf::from(self.path_input.trim()));
                }
            }
            if ui
                .add_enabled(!self.transcriber.is_running(), egui::Button::new(tr("⚡ Transcribe audio...", "⚡ Transcrever áudio...")))
                .on_hover_text(tr(
                    "Makes a tab (guitar and bass) from a music file",
                    "Gera uma tab (guitarra e baixo) a partir de um arquivo de música",
                ))
                .clicked()
            {
                self.transcriber.show_form(None, None, None, None);
            }
            if self.loading_tab.is_some() {
                ui.spinner();
            }
            if let Some(tab) = &self.tab {
                row_separator(ui);
                let s = &tab.score;
                let title = if s.title.is_empty() { tr("(untitled)", "(sem título)") } else { &s.title };
                // Whole labels move to the next row instead of breaking
                // mid-text; only a title longer than the page is cut.
                let row = ui.max_rect().width();
                keep_together(ui, (text_width(ui, title) * style::text::HEADING / style::text::BODY).min(row));
                ui.add(egui::Label::new(RichText::new(title).size(style::text::HEADING).strong().color(palette.text)).truncate());
                if !s.artist.is_empty() {
                    let artist = format!("— {}", s.artist);
                    keep_together(ui, text_width(ui, &artist).min(row));
                    ui.add(egui::Label::new(RichText::new(artist).color(palette.text)).truncate());
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(trf!("{:.0} bpm · {} bars", "{:.0} bpm · {} compassos", s.initial_tempo, s.bars.len()))
                            .color(palette.text_dim),
                    )
                    .extend(),
                );
                close = ui
                    .button("✖")
                    .on_hover_text(tr("Close the tab (back to the list of tabs)", "Fechar a tab (volta para a lista de tabs)"))
                    .clicked();
            }
        });
        if close {
            self.close_tab(settings);
        }
    }

    fn transport_row(&mut self, ui: &mut Ui) {
        let Some((score, timeline)) = self.tab.as_ref().map(|t| (t.score.clone(), t.timeline.clone())) else {
            return;
        };
        let player = self.player.clone();
        let status = player.status();
        let playing = status.playing();
        let backing_on = self.use_backing && self.backing.is_some();
        let palette = Palette::of(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().slider_width = 90.0;
            let play_label = if playing { tr("⏸ Pause", "⏸ Pausar") } else { tr("▶ Play", "▶ Tocar") };
            if ui.button(play_label).on_hover_text(tr("Space", "Espaço")).clicked() {
                self.toggle_play();
            }
            if ui.button("⏹").on_hover_text(tr("Stop and go back to the start", "Parar e voltar ao início")).clicked() {
                self.player.stop();
            }
            ui.monospace(format!(
                "{} / {}",
                format_time(status.score_secs()),
                format_time(timeline.duration_secs)
            ));
            row_separator(ui);

            let tracks = &score.tracks;
            let current = tracks.get(self.selected_track).map(|t| t.name.as_str()).unwrap_or("");
            let track_label = tr("Track:", "Faixa:");
            let combo_width = (text_width(ui, current) + 2.0 * ui.spacing().icon_width + 2.0 * ui.spacing().button_padding.x).clamp(80.0, 180.0);
            keep_together(ui, text_width(ui, track_label) + ui.spacing().item_spacing.x + combo_width);
            ui.label(track_label);
            egui::ComboBox::from_id_salt("tab_track_select")
                .selected_text(current)
                .width(combo_width)
                .truncate()
                .show_ui(ui, |ui| {
                    for (i, track) in tracks.iter().enumerate() {
                        let name = if track.name.is_empty() { trf!("Track {}", "Faixa {}", i + 1) } else { track.name.clone() };
                        ui.selectable_value(&mut self.selected_track, i, name);
                    }
                });

            let speed = labeled_slider_enabled(
                ui,
                !backing_on,
                egui::Slider::new(&mut self.speed, 0.25..=1.5).fixed_decimals(2),
                tr("speed", "velocidade"),
            );
            if backing_on {
                speed.on_disabled_hover_text(tr(
                    "Speed only applies to the synthesized audio.",
                    "A velocidade só vale para o áudio sintetizado.",
                ));
            } else if speed.changed() {
                self.player.set_speed(self.speed as f64);
            }
            ui.checkbox(&mut self.follow_cursor, tr("Follow cursor", "Seguir cursor"));

            let count_in = match self.count_in_bars {
                0 => tr("no count-in", "sem contagem").to_string(),
                1 => tr("count-in: 1 bar", "contagem: 1 compasso").to_string(),
                n => trf!("count-in: {n} bars", "contagem: {n} compassos"),
            };
            let count_in_width = text_width(ui, &count_in) + 2.0 * ui.spacing().icon_width + 2.0 * ui.spacing().button_padding.x;
            keep_together(ui, count_in_width);
            egui::ComboBox::from_id_salt("tab_count_in")
                .selected_text(count_in)
                .width(count_in_width)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.count_in_bars, 0, tr("no count-in", "sem contagem"));
                    ui.selectable_value(&mut self.count_in_bars, 1, tr("1 bar", "1 compasso"));
                    ui.selectable_value(&mut self.count_in_bars, 2, tr("2 bars", "2 compassos"));
                })
                .response
                .on_hover_text(tr("Clicks before playback starts", "Cliques antes de começar a tocar"));
            if status.counting_in() {
                ui.colored_label(palette.accent, tr("counting in...", "contando..."));
            }
            row_separator(ui);

            ui.toggle_value(&mut self.show_mixer, "🔉 Mixer");
            let mut practice = self.practice_on;
            if ui
                .toggle_value(&mut practice, tr("🎯 Practice", "🎯 Prática"))
                .on_hover_text(tr(
                    "Checks, note by note, whether you played the selected track right",
                    "Confere, nota a nota, se você tocou a faixa selecionada corretamente",
                ))
                .changed()
            {
                self.set_practice(practice);
            }
            let mut looping = self.practice_loop.on;
            if ui
                .toggle_value(&mut looping, "🔁 Loop")
                .on_hover_text(tr(
                    "Repeats a section. With the loop on, Shift+click a bar to set its end.",
                    "Repete um trecho. Com o loop ligado, Shift+clique num compasso marca o fim.",
                ))
                .changed()
            {
                if looping {
                    let bar = timeline
                        .occurrence_at(status.score_secs())
                        .map(|i| timeline.occurrences[i].bar)
                        .unwrap_or(0);
                    self.loop_bars(bar, bar);
                } else {
                    self.practice_loop.on = false;
                    self.apply_loop();
                }
            }
        });
    }

    /// Loop range and speed trainer.
    fn loop_row(&mut self, ui: &mut Ui) {
        let Some(bar_count) = self.tab.as_ref().map(|t| t.score.bars.len()) else { return };
        let backing_on = self.use_backing && self.backing.is_some();
        let palette = Palette::of(ui.ctx());
        let mut changed = false;
        let trainer_off_hint = tr(
            "The speed trainer only works with the synthesized audio (turn off the original audio).",
            "O treino de velocidade só funciona com o áudio sintetizado (desligue o áudio original).",
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("🔁 Loop").strong());
            let (mut first, mut last) = (self.practice_loop.bars.0 + 1, self.practice_loop.bars.1 + 1);
            ui.label(tr("bars", "compassos"));
            changed |= ui.add(egui::DragValue::new(&mut first).range(1..=bar_count)).changed();
            ui.label(tr("to", "a"));
            changed |= ui.add(egui::DragValue::new(&mut last).range(1..=bar_count)).changed();
            if changed {
                self.practice_loop.bars = (first.min(last) - 1, first.max(last) - 1);
            }
            ui.label(RichText::new(trf!("· passes: {}", "· passadas: {}", self.practice_loop.good_passes)).color(palette.text_dim))
                .on_hover_text(tr(
                    "Good passes (with practice mode, above the trainer's pass mark)",
                    "Passadas boas (com o modo prática, acima da exigência do treino)",
                ));
            row_separator(ui);
            let trainer = ui
                .add_enabled(!backing_on, egui::Checkbox::new(&mut self.practice_loop.trainer, tr("Speed trainer", "Treino de velocidade")))
                .on_hover_text(tr("Starts slow and speeds up after each good pass", "Começa devagar e acelera a cada passada boa"))
                .on_disabled_hover_text(trainer_off_hint);
            if trainer.changed() && self.practice_loop.trainer {
                self.speed = self.practice_loop.start_speed;
                self.player.set_speed(self.speed as f64);
                self.practice_loop.good_passes = 0;
            }
            if self.practice_loop.trainer {
                let pl = &mut self.practice_loop;
                let enabled = !backing_on;
                ui.add_enabled(enabled, egui::DragValue::new(&mut pl.start_speed).range(0.25..=1.5).speed(0.01).custom_formatter(|v, _| trf!("from {:.0}%", "de {:.0}%", v * 100.0)))
                    .on_disabled_hover_text(trainer_off_hint);
                ui.add_enabled(enabled, egui::DragValue::new(&mut pl.step).range(0.01..=0.25).speed(0.005).custom_formatter(|v, _| format!("+{:.0}%", v * 100.0)))
                    .on_disabled_hover_text(trainer_off_hint);
                ui.add_enabled(enabled, egui::DragValue::new(&mut pl.target_speed).range(0.25..=1.5).speed(0.01).custom_formatter(|v, _| trf!("to {:.0}%", "até {:.0}%", v * 100.0)))
                    .on_disabled_hover_text(trainer_off_hint);
                ui.add_enabled(enabled, egui::DragValue::new(&mut pl.pass_accuracy).range(0.5..=1.0).speed(0.01).custom_formatter(|v, _| trf!("pass at {:.0}%", "passa com {:.0}%", v * 100.0)))
                    .on_hover_text(tr(
                        "With practice mode on, only speeds up if the pass reaches this accuracy",
                        "Com o modo prática ligado, só acelera se a passada tiver esta precisão",
                    ))
                    .on_disabled_hover_text(trainer_off_hint);
                ui.label(RichText::new(trf!("now {:.0}%", "agora {:.0}%", self.speed * 100.0)).strong());
            }
        });
        if changed {
            self.apply_loop();
        }
    }

    fn practice_row(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        self.ensure_practice();
        let palette = Palette::of(ui.ctx());
        let Some((_, _, session)) = &mut self.practice else { return };
        let mut prefs_changed = false;
        ui.horizontal_wrapped(|ui| {
            if session.beat_count() == 0 {
                ui.colored_label(
                    palette.warn,
                    tr(
                        "This track has no notes to check (drums or empty).",
                        "Esta faixa não tem notas para conferir (bateria ou vazia).",
                    ),
                );
                return;
            }
            let stats = session.stats();
            let accuracy = stats
                .note_accuracy()
                .map(|a| format!("{:.0}%", a * 100.0))
                .unwrap_or_else(|| "—".to_string());
            ui.label(RichText::new(format!("🎯 {accuracy}")).strong())
                .on_hover_text(tr("Notes right / notes checked", "Notas certas / notas conferidas"));
            ui.label(trf!(
                "notes {}/{} · beats {}/{} · streak {} (best {})",
                "notas {}/{} · tempos {}/{} · sequência {} (melhor {})",
                stats.notes_hit,
                stats.notes_judged,
                stats.beats_hit,
                stats.beats_judged,
                stats.streak,
                stats.best_streak
            ));
            if ui.button(tr("Reset", "Zerar")).clicked() {
                session.reset();
            }
            ui.checkbox(&mut self.show_heatmap, tr("hit map", "mapa de acertos"))
                .on_hover_text(tr(
                    "Colors each bar from red (many misses) to green (all right)",
                    "Pinta cada compasso de vermelho (errou muito) a verde (acertou)",
                ));
            row_separator(ui);
            ui.spacing_mut().slider_width = 100.0;
            let prefs = &mut self.practice_prefs;
            let latency_label = tr("extra latency", "latência extra");
            keep_together(ui, ui.spacing().interact_size.x * 1.5 + ui.spacing().item_spacing.x + text_width(ui, latency_label));
            prefs_changed |= ui
                .add(egui::DragValue::new(&mut prefs.extra_latency_ms).range(-200.0..=500.0).suffix(" ms"))
                .on_hover_text(tr(
                    "Extra latency, added to the audio buffer's. Raise it if notes played in time show up as wrong.",
                    "Latência extra, somada à do buffer de áudio. Aumente se notas tocadas no tempo aparecem como erradas.",
                ))
                .changed();
            ui.label(latency_label);
            prefs_changed |= labeled_slider(ui, egui::Slider::new(&mut prefs.tolerance_ms, 30.0..=300.0).suffix(" ms"), tr("tolerance", "tolerância"))
                .on_hover_text(tr("How early or late a note still counts", "Quanto antes ou depois do tempo a nota ainda vale"))
                .changed();
            prefs_changed |= labeled_slider(ui, egui::Slider::new(&mut prefs.strictness, 0.0..=1.0).fixed_decimals(2), tr("strictness", "exigência"))
                .on_hover_text(tr(
                    "Lower it if right notes show up red (e.g. lots of distortion or reverb)",
                    "Baixe se notas certas aparecem em vermelho (ex.: muita distorção ou reverb)",
                ))
                .changed();
        });
        if prefs_changed {
            settings.practice = self.practice_prefs.clone();
            save_settings(settings);
        }
        self.practice_history_row(ui, settings);
    }

    /// Best result so far and the bars that need work.
    fn practice_history_row(&mut self, ui: &mut Ui, settings: &AppSettings) {
        let Some(tab) = &self.tab else { return };
        let palette = Palette::of(ui.ctx());
        let track_name = tab.score.tracks.get(self.selected_track).map(|t| t.name.clone()).unwrap_or_default();
        let record = settings.practice_log.get(&tab_key(&tab.path));
        let hardest = self
            .practice
            .as_ref()
            .map(|(_, _, s)| hardest_bars(&s.bar_results(), 2))
            .unwrap_or_default();
        let weak: Vec<&(usize, f32)> = hardest.iter().filter(|(_, a)| *a < 0.9).take(4).collect();
        if record.is_none() && weak.is_empty() {
            return;
        }
        let mut loop_bar = None;
        ui.horizontal_wrapped(|ui| {
            if let Some(record) = record {
                let best = record.best_accuracy.get(&track_name).map(|a| format!("{:.0}%", a * 100.0));
                ui.label(
                    RichText::new(trf!(
                        "History: best {} · {} sessions",
                        "Histórico: melhor {} · {} sessões",
                        best.unwrap_or_else(|| "—".into()),
                        record.runs
                    ))
                    .color(palette.text_dim),
                );
                ui.separator();
            }
            if !weak.is_empty() {
                ui.label(tr("Hardest bars:", "Compassos mais difíceis:"));
                for (bar, accuracy) in weak {
                    if chip(ui, format!("{} ({:.0}%)", bar + 1, accuracy * 100.0))
                        .on_hover_text(tr("Loop this bar", "Repetir este compasso em loop"))
                        .clicked()
                    {
                        loop_bar = Some(*bar);
                    }
                }
            }
        });
        if let Some(bar) = loop_bar {
            self.loop_bars(bar, bar);
        }
    }

    fn mixer(&mut self, ui: &mut Ui) {
        let Some(tab) = &self.tab else { return };
        let mut tracks_changed = false;
        ui.horizontal_wrapped(|ui| {
            let spacing = ui.spacing().clone();
            let toggle_width = text_width(ui, "M") + 2.0 * spacing.button_padding.x;
            for (i, track) in tab.score.tracks.iter().enumerate() {
                let name = if track.name.is_empty() { trf!("Track {}", "Faixa {}", i + 1) } else { track.name.clone() };
                // Name, M and S in a group frame (margin 6 and stroke 1 per side).
                keep_together(ui, text_width(ui, &name) + 2.0 * spacing.button_padding.x + 2.0 * toggle_width + 2.0 * spacing.item_spacing.x + 14.0);
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(self.selected_track == i, name).clicked() {
                            self.selected_track = i;
                        }
                        tracks_changed |= ui
                            .toggle_value(&mut self.muted[i], "M")
                            .on_hover_text(tr("Mute", "Mudo"))
                            .changed();
                        tracks_changed |= ui
                            .toggle_value(&mut self.soloed[i], "S")
                            .on_hover_text("Solo")
                            .changed();
                    });
                });
            }
        });
        if tracks_changed {
            self.player.set_track_enabled(self.track_enabled());
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().slider_width = 100.0;
            let synth = labeled_slider(ui, egui::Slider::new(&mut self.synth_gain, 0.0..=2.0), tr("synth volume", "volume do sintetizador"));
            if synth.changed() {
                self.player.set_gains(self.synth_gain, self.backing_gain);
            }
            row_separator(ui);
            if self.loading_soundfont.is_some() {
                ui.spinner();
                ui.label(tr("Loading SoundFont...", "Carregando SoundFont..."));
            } else if let Some(name) = &self.soundfont_name {
                ui.label(format!("SoundFont: {name}"));
            }
            #[cfg(not(target_os = "android"))]
            if ui.button(tr("Choose SoundFont...", "Escolher SoundFont...")).clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("SoundFont", &["sf2"]).pick_file()
            {
                self.load_soundfont(path, false);
            }
            #[cfg(target_os = "android")]
            if ui.button(tr("Use path as SoundFont", "Usar caminho como SoundFont")).clicked() {
                self.load_soundfont(PathBuf::from(self.path_input.trim()), false);
            }
        });
    }

    fn backing_section(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        let Some(timeline) = self.tab.as_ref().map(|t| t.timeline.clone()) else { return };
        let mut sync_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(tr("Original audio:", "Áudio original:")).strong());
            #[cfg(not(target_os = "android"))]
            if ui.button(tr("🎵 Open audio...", "🎵 Abrir áudio...")).clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter(tr("Audio", "Áudio"), AUDIO_EXTENSIONS)
                    .pick_file()
            {
                self.load_backing_file(path);
            }
            #[cfg(target_os = "android")]
            if ui.button(tr("Use path as audio", "Usar caminho como áudio")).clicked() {
                self.load_backing_file(PathBuf::from(self.path_input.trim()));
            }
            if self.loading_backing.is_some() {
                ui.spinner();
                ui.label(tr("Decoding...", "Decodificando..."));
            }
            match &self.backing {
                None => {
                    ui.label(RichText::new(tr("none (mp3, ogg, flac, wav, m4a)", "nenhum (mp3, ogg, flac, wav, m4a)")).weak());
                }
                Some(backing) => {
                    let name = match &backing.source {
                        BackingSource::File(p) => p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        BackingSource::Embedded => tr("embedded in the GP file", "embutido no arquivo GP").to_string(),
                    };
                    ui.label(format!("{name} ({})", format_time(backing.duration_secs)));
                    if ui.checkbox(&mut self.use_backing, tr("play", "tocar")).changed() {
                        self.player.set_use_backing(self.use_backing);
                    }
                    ui.spacing_mut().slider_width = 100.0;
                    let vol = labeled_slider(ui, egui::Slider::new(&mut self.backing_gain, 0.0..=2.0), "volume");
                    if vol.changed() {
                        self.player.set_gains(self.synth_gain, self.backing_gain);
                    }
                    if ui.button(tr("Remove", "Remover")).clicked() {
                        self.backing = None;
                        self.player.set_backing(None, SyncMap::default());
                        sync_changed = true;
                    }
                }
            }
        });

        let embedded_sync =
            matches!(&self.backing, Some(Backing { source: BackingSource::Embedded, .. }))
                && self.file_sync.is_some();
        let transcribed_sync = matches!(&self.backing, Some(Backing { source: BackingSource::File(p), .. }) if self.sidecar_sync_for(p).is_some());
        if transcribed_sync {
            ui.label(RichText::new(tr("Sync: bars detected by the transcription.", "Sincronização: compassos detectados na transcrição.")).weak());
        } else if self.backing.is_some() && embedded_sync {
            ui.label(RichText::new(tr(
                "Sync: the file's own sync points (Guitar Pro 8).",
                "Sincronização: pontos de sincronia do próprio arquivo (Guitar Pro 8).",
            )).weak());
        } else if self.backing.is_some() {
            let player = self.player.clone();
            let status = player.status();
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Sync:", "Sincronia:"));
                sync_changed |= ui
                    .add(egui::DragValue::new(&mut self.sync_offset).speed(0.01).range(-600.0..=600.0).suffix(" s").fixed_decimals(2))
                    .on_hover_text(tr("Time in the audio where bar 1 starts", "Tempo no áudio em que o compasso 1 começa"))
                    .changed();
                ui.label(tr("audio tempo ×", "tempo do áudio ×"));
                sync_changed |= ui
                    .add(egui::DragValue::new(&mut self.tempo_ratio).speed(0.001).range(0.5..=2.0).fixed_decimals(3))
                    .on_hover_text(tr("Recording tempo divided by the tab's tempo", "Andamento da gravação dividido pelo andamento da tab"))
                    .changed();
                if ui
                    .button(tr("⏮ Play from audio start", "⏮ Ouvir do início do áudio"))
                    .on_hover_text(tr(
                        "Plays the audio from 0:00 so you can mark where bar 1 starts",
                        "Toca o áudio desde 0:00 para marcar onde o compasso 1 começa",
                    ))
                    .clicked()
                {
                    self.player.seek_audio(0.0);
                    self.player.play();
                }
                if ui
                    .button(tr("Bar 1 starts now", "Compasso 1 começa agora"))
                    .on_hover_text(tr(
                        "Click at the moment bar 1 starts in the recording",
                        "Clique no instante em que o compasso 1 começa na gravação",
                    ))
                    .clicked()
                {
                    self.sync_offset = status.audio_secs();
                    sync_changed = true;
                }
                if ui
                    .button(tr("Fit tempo here", "Ajustar andamento aqui"))
                    .on_hover_text(tr(
                        "With bar 1 already marked: click at the start of a later bar to correct the tempo difference",
                        "Com o compasso 1 já marcado: clique no início de um compasso mais adiante para corrigir a diferença de andamento",
                    ))
                    .clicked()
                {
                    let audio_now = status.audio_secs();
                    let score_now = status.score_secs();
                    // The bar boundary nearest to where the cursor is now.
                    let nearest = timeline
                        .occurrences
                        .iter()
                        .map(|o| o.start_secs)
                        .min_by(|a, b| (a - score_now).abs().total_cmp(&(b - score_now).abs()));
                    let elapsed_audio = audio_now - self.sync_offset;
                    if let Some(bar_start) = nearest
                        && bar_start > 1.0
                        && elapsed_audio > 1.0
                    {
                        self.tempo_ratio = (bar_start / elapsed_audio).clamp(0.5, 2.0);
                        sync_changed = true;
                    }
                }
                if ui.button(tr("Reset", "Zerar")).clicked() {
                    self.sync_offset = 0.0;
                    self.tempo_ratio = 1.0;
                    sync_changed = true;
                }
            });
        }
        if sync_changed {
            self.player.set_sync(self.active_sync());
            self.persist_tab_sync(settings);
        }
    }

    fn tab_area(&mut self, ui: &mut Ui) {
        let Some(tab) = &self.tab else { return };
        let Some(track) = tab.score.tracks.get(self.selected_track) else { return };
        // Rows above (combo boxes can't wrap) may have widened the page past
        // the window: lay the tab out for what's actually visible.
        let visible = ui.clip_rect().right() - ui.cursor().left();
        let width = ui.available_width().min(visible.max(200.0));
        let stale = match &self.layout {
            Some((t, w, _)) => *t != self.selected_track || (*w - width).abs() > 1.0,
            None => true,
        };
        if stale {
            self.layout = Some((self.selected_track, width, TabLayout::build(&tab.score, track, width)));
        }
        let Some((_, _, layout)) = &self.layout else { return };

        let status = self.player.status();
        let score_secs = status.score_secs();
        let cursor = tab.timeline.occurrence_at(score_secs).map(|i| {
            let occ = &tab.timeline.occurrences[i];
            CursorPos { bar: occ.bar, tick: occ.secs_to_tick(score_secs.max(occ.start_secs)) }
        });
        let follow = self.follow_cursor && status.playing();
        let marks = match &self.practice {
            Some((_, track_index, session)) if self.practice_on && *track_index == self.selected_track => {
                Some(session.marks())
            }
            _ => None,
        };
        let heat: Option<std::collections::HashMap<usize, f32>> = match &self.practice {
            Some((_, track_index, session)) if self.practice_on && self.show_heatmap && *track_index == self.selected_track => {
                Some(session.bar_results().into_iter().map(|(bar, (hit, judged))| (bar, hit as f32 / judged.max(1) as f32)).collect())
            }
            _ => None,
        };
        let overlay = TabOverlay {
            marks,
            bar_heat: heat.as_ref(),
            loop_bars: self.practice_loop.on.then_some(self.practice_loop.bars),
        };
        let response = tab_view(ui, &tab.score, track, layout, cursor, follow, overlay);
        let shift = ui.input(|i| i.modifiers.shift);
        if let Some(bar) = response.clicked_bar {
            if shift && self.practice_loop.on {
                let first = self.practice_loop.bars.0;
                self.loop_bars(first.min(bar), first.max(bar));
            } else if let Some(occ) = tab.timeline.closest_occurrence(bar, score_secs) {
                self.player.seek(tab.timeline.occurrences[occ].start_secs);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_minutes_and_seconds() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(65.9), "1:05");
        assert_eq!(format_time(-3.0), "0:00");
    }

    #[test]
    fn solo_overrides_mute() {
        let mut panel = TabPlayerPanel::new(SongPlayer::new(), &AppSettings::default());
        panel.muted = vec![false, true, false];
        panel.soloed = vec![false, false, false];
        assert_eq!(panel.track_enabled(), vec![true, false, true]);
        panel.soloed = vec![false, true, false];
        assert_eq!(panel.track_enabled(), vec![false, true, false]);
    }

    #[test]
    fn practice_mode_and_tone_row_render_with_a_tab() {
        use crate::shared::tab::model::{MasterBar, ScoreTrack, TabBeat, TabNote, TrackBar};
        let beat = TabBeat {
            duration: 960,
            value: 4,
            notes: vec![TabNote { string: 6, fret: 0, velocity: 100, ..Default::default() }],
            ..Default::default()
        };
        let track = ScoreTrack {
            name: "Distortion Guitar".into(),
            program: 30,
            tuning: vec![64, 59, 55, 50, 45, 40],
            bars: vec![TrackBar { voices: vec![vec![beat]] }],
            ..Default::default()
        };
        let score = Score {
            initial_tempo: 120.0,
            bars: vec![MasterBar { numerator: 4, denominator: 4, ..Default::default() }],
            tracks: vec![track],
            ..Default::default()
        };
        let timeline = Timeline::build(&score);
        let mut panel = TabPlayerPanel::new(SongPlayer::new(), &AppSettings::default());
        panel.tab = Some(LoadedTab { path: PathBuf::from("/t/x.gp5"), score: Arc::new(score), timeline: Arc::new(timeline) });
        panel.muted = vec![false];
        panel.soloed = vec![false];
        panel.set_practice(true);
        assert_eq!(panel.muted, vec![true], "practiced track is muted in the synth");
        panel.feed_input(&[0.0; 800], 0.01);
        assert_eq!(panel.practice.as_ref().map(|p| p.2.beat_count()), Some(1));

        let ctx = egui::Context::default();
        let mut settings = AppSettings::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(panel.ui(ui, &mut settings).is_none());
            });
        });
        panel.set_practice(false);
        assert!(panel.practice.is_none());
    }

    #[test]
    fn transcribed_tabs_open_with_their_recording_in_sync() {
        use crate::shared::tab::model::{MasterBar, ScoreTrack, TabBeat, TabNote, TrackBar};
        use crate::shared::tab::repository::{save, SourceAudio, TabMetadata};
        let dir = std::env::temp_dir().join(format!("gva_tab_sidecar_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("song.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 8000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        for _ in 0..8000 * 6 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
        let beat = TabBeat {
            duration: 3840,
            value: 1,
            notes: vec![TabNote { string: 6, fret: 0, velocity: 90, ..Default::default() }],
            ..Default::default()
        };
        let score = Score {
            initial_tempo: 120.0,
            bars: vec![MasterBar { numerator: 4, denominator: 4, ..Default::default() }; 2],
            tracks: vec![ScoreTrack {
                tuning: vec![64, 59, 55, 50, 45, 40],
                bars: vec![TrackBar { voices: vec![vec![beat]] }, TrackBar::default()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let meta = TabMetadata {
            title: "Song".into(),
            source: SourceAudio { path: Some(wav.clone()), sha256: "h".into(), ..Default::default() },
            bar_audio_secs: vec![0.5, 2.6, 4.7],
            ..Default::default()
        };
        let tab_path = save(&dir.join("repo"), &score, &meta).unwrap();

        let mut settings = AppSettings::default();
        let mut panel = TabPlayerPanel::new(SongPlayer::new(), &settings);
        panel.open_tab(tab_path);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while (panel.tab.is_none() || panel.backing.is_none()) && std::time::Instant::now() < deadline {
            panel.poll_background(&mut settings);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(panel.backing.is_some(), "recording loaded");
        match panel.active_sync() {
            SyncMap::Points(points) => {
                assert_eq!(points.len(), 3);
                assert!((points[1].0 - 2.0).abs() < 1e-9 && (points[1].1 - 2.6).abs() < 1e-9);
            }
            other => panic!("expected bar sync, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn four_bar_score() -> Score {
        use crate::shared::tab::model::{MasterBar, ScoreTrack, TabBeat, TabNote, TrackBar};
        let beat = TabBeat {
            duration: 3840,
            value: 1,
            notes: vec![TabNote { string: 5, fret: 2, velocity: 100, ..Default::default() }],
            ..Default::default()
        };
        Score {
            initial_tempo: 120.0,
            bars: vec![MasterBar { numerator: 4, denominator: 4, ..Default::default() }; 4],
            tracks: vec![ScoreTrack {
                name: "Guitar".into(),
                tuning: vec![64, 59, 55, 50, 45, 38],
                bars: vec![TrackBar { voices: vec![vec![beat]] }; 4],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn loop_ranges_cover_whole_bars_and_hardest_bars_sort_by_accuracy() {
        let timeline = Timeline::build(&four_bar_score());
        // 120 bpm 4/4: 2 s per bar.
        assert_eq!(loop_secs(&timeline, (1, 2), 0.0), Some((2.0, 6.0)));
        assert_eq!(loop_secs(&timeline, (2, 1), 0.0), Some((2.0, 6.0)), "order doesn't matter");
        assert_eq!(loop_secs(&timeline, (9, 9), 0.0), None);
        let results = std::collections::BTreeMap::from([(0, (9, 10)), (1, (2, 4)), (2, (1, 1)), (3, (0, 3))]);
        assert_eq!(hardest_bars(&results, 2), vec![(3, 0.0), (1, 0.5), (0, 0.9)]);
    }

    #[test]
    fn loop_and_trainer_drive_the_player() {
        let score = four_bar_score();
        let timeline = Timeline::build(&score);
        let player = SongPlayer::new();
        player.set_output_rate(1000);
        player.set_song(Arc::new(Timeline::build(&score)), Vec::new(), 1);
        let mut panel = TabPlayerPanel::new(player.clone(), &AppSettings::default());
        panel.tab = Some(LoadedTab { path: PathBuf::from("/t/l.gp5"), score: Arc::new(score), timeline: Arc::new(timeline) });
        panel.muted = vec![false];
        panel.soloed = vec![false];
        assert_eq!(panel.selected_tuning().and_then(|t| Tuning::from_tab(&t)).map(|t| t.name), Some("Drop D".into()));
        panel.loop_bars(1, 1);
        assert!((player.status().score_secs() - 2.0).abs() < 1e-9);
        panel.practice_loop.trainer = true;
        panel.speed = 0.6;
        player.set_speed(0.6);
        player.play();
        let mut buf = vec![0.0; 1000];
        for _ in 0..4 {
            player.try_render(&mut buf);
        }
        panel.on_loop_passes();
        assert!(panel.speed > 0.6, "a pass without practice mode counts as good");
        assert_eq!(panel.practice_loop.good_passes, 1);
        panel.count_in_bars = 1;
        assert_eq!(panel.count_in_beats(), 4);
        let beat = panel.count_in_beat_secs();
        assert!((beat - 0.5 / panel.speed as f64).abs() < 1e-6, "{beat}");

        let ctx = egui::Context::default();
        let mut settings = AppSettings::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                panel.ui(ui, &mut settings);
            });
        });
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert!(has_extension(Path::new("/a/Song.GP5"), SUPPORTED_EXTENSIONS));
        assert!(!has_extension(Path::new("/a/song.txt"), SUPPORTED_EXTENSIONS));
    }
}
