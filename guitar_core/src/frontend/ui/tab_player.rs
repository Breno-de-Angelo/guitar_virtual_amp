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

use crossbeam::channel::{bounded, Receiver};
use eframe::egui::{self, Color32, Key, RichText, Ui};
use rustysynth::SoundFont;

use crate::frontend::lib::tunings::{Tuning, label as tuning_label};
use crate::frontend::ui::tab_view::TabOverlay;

use crate::backend::audio_file::{self, DecodedAudio, AUDIO_EXTENSIONS};
use crate::backend::song_player::SongPlayer;
use crate::frontend::lib::practice::{expected_beats, PracticeSession, PracticeSettings};
use crate::frontend::ui::tab_view::{tab_view, CursorPos, TabLayout};
use crate::frontend::ui::transcribe::TranscribePanel;
use crate::shared::config::GLOBAL_CONFIG;
use crate::shared::factory_presets::factory_presets;
use crate::shared::pedals::PedalInstance;
use crate::shared::preset::{self, Preset};
use crate::shared::settings::{save_settings, AppSettings, PracticePrefs, PresetRef, TabSyncSettings};
use crate::shared::tab::tone::{ESTIMATED_TAG, guess_tone, preset_fits_song, preset_fits_track};
use crate::shared::tab::import::{load_score_file, SUPPORTED_EXTENSIONS};
use crate::shared::tab::repository;
use crate::shared::tab::model::Score;
use crate::shared::tab::timeline::{SyncMap, Timeline};

/// Common SoundFont install locations, tried in order when none is configured.
const SOUNDFONT_CANDIDATES: &[&str] = &[
    "/usr/share/sounds/sf2/FluidR3_GM.sf2",
    "/usr/share/soundfonts/FluidR3_GM.sf2",
    "/usr/share/sounds/sf2/default-GM.sf2",
    "/usr/share/soundfonts/default.sf2",
    "/usr/share/sounds/sf2/TimGM6mb.sf2",
    "/usr/share/sounds/sf2/GeneralUser_GS.sf2",
    "/usr/share/soundfonts/GeneralUser_GS.sf2",
    "/opt/homebrew/share/soundfonts/default.sf2",
    "/usr/local/share/soundfonts/default.sf2",
];

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

type Pending<T> = Option<Receiver<Result<T, String>>>;

fn spawn<T: Send + 'static>(job: impl FnOnce() -> Result<T, String> + Send + 'static) -> Receiver<Result<T, String>> {
    let (tx, rx) = bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send(job());
    });
    rx
}

fn poll<T>(pending: &mut Pending<T>) -> Option<Result<T, String>> {
    let result = pending.as_ref()?.try_recv().ok()?;
    *pending = None;
    Some(result)
}

fn format_time(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| extensions.contains(&e.to_ascii_lowercase().as_str()))
}

fn find_default_soundfont() -> Option<PathBuf> {
    // A user-provided SoundFont dropped into the app's data dir wins.
    if let Some(dir) = dirs::data_dir().map(|d| d.join("guitar_virtual_amp/soundfonts"))
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        let mut found: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| has_extension(p, &["sf2"]))
            .collect();
        found.sort();
        if let Some(first) = found.into_iter().next() {
            return Some(first);
        }
    }
    SOUNDFONT_CANDIDATES.iter().map(PathBuf::from).find(|p| p.is_file())
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
            player,
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
            transcriber: TranscribePanel::new(),
            sidecar_sync: None,
            practice_loop: PracticeLoop::default(),
            count_in_bars: 0,
            show_heatmap: true,
            tuner_requested: false,
            was_playing: false,
            #[cfg(target_os = "android")]
            path_input: String::new(),
        };
        let soundfont = settings
            .soundfont_path
            .clone()
            .filter(|p| p.is_file())
            .or_else(find_default_soundfont);
        if let Some(path) = soundfont {
            panel.load_soundfont(path);
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
            self.set_message(format!("Treino de velocidade: {:.0}%", self.speed * 100.0), false);
        } else if let Some(a) = accuracy
            && !good
        {
            self.set_message(format!("Passada com {:.0}%: repita nesta velocidade.", a * 100.0), false);
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
            .map(|t| format!("{} ({} notas, afinação {})", t.name, t.notes, t.tuning_name))
            .collect();
        self.set_message(
            format!(
                "Tab gerada{}: {} · {:.0} bpm · {}",
                if output.replaced { " de novo" } else { "" },
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

    fn load_soundfont(&mut self, path: PathBuf) {
        self.loading_soundfont = Some(spawn(move || {
            let mut file = std::fs::File::open(&path)
                .map_err(|e| format!("Não foi possível abrir o SoundFont: {e}"))?;
            let sf = SoundFont::new(&mut file).map_err(|e| format!("SoundFont inválido: {e:?}"))?;
            Ok((path, Arc::new(sf)))
        }));
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

        let title = if score.artist.is_empty() {
            score.title.clone()
        } else {
            format!("{} - {}", score.artist, score.title)
        };
        self.set_message(format!("Aberto: {title}"), false);
        settings.last_tab = Some(tab.path.clone());
        save_settings(settings);
        self.tab = Some(tab);
    }

    fn poll_background(&mut self, settings: &mut AppSettings) {
        match poll(&mut self.loading_tab) {
            Some(Ok(tab)) => self.on_tab_loaded(tab, settings),
            Some(Err(e)) => self.set_message(e, true),
            None => {}
        }
        match poll(&mut self.loading_soundfont) {
            Some(Ok((path, sf))) => {
                self.player.set_soundfont(sf);
                self.soundfont_name =
                    path.file_name().map(|n| n.to_string_lossy().into_owned());
                if settings.soundfont_path.as_ref() != Some(&path) {
                    settings.soundfont_path = Some(path);
                    save_settings(settings);
                }
            }
            Some(Err(e)) => self.set_message(e, true),
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
                self.load_soundfont(path);
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
            self.set_message("Modo prática: a faixa selecionada foi mutada no sintetizador.", false);
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

        if self.tab.is_some()
            && !ui.ctx().wants_keyboard_input()
            && ui.input(|i| i.key_pressed(Key::Space))
        {
            self.toggle_play();
        }

        self.file_row(ui);
        if let Some((text, is_error)) = &self.message {
            let color = if *is_error { Color32::LIGHT_RED } else { ui.visuals().weak_text_color() };
            ui.colored_label(color, text);
        }
        if let Some(output) = self.transcriber.ui(ui, settings) {
            self.on_transcribed(output, settings);
        }
        if self.tab.is_none() {
            ui.add_space(20.0);
            ui.label(
                "Abra (ou arraste para a janela) um arquivo Guitar Pro: .gp3, .gp4, .gp5, .gpx ou .gp.",
            );
            return None;
        }
        let tone = self.tone_row(ui);
        self.tuning_row(ui);
        ui.separator();
        let playing = self.player.status().playing();
        if self.was_playing && !playing && self.practice_on {
            self.record_run(settings);
        }
        self.was_playing = playing;
        self.transport_row(ui);
        if self.practice_loop.on {
            ui.separator();
            self.loop_row(ui);
        }
        if self.practice_on {
            ui.separator();
            self.practice_row(ui, settings);
        }
        if self.show_mixer {
            ui.separator();
            self.mixer(ui);
            ui.separator();
            self.backing_section(ui, settings);
        }
        ui.separator();
        self.tab_area(ui);
        tone
    }

    /// Tone suggestions for the selected track.
    fn tone_row(&mut self, ui: &mut Ui) -> Option<(Vec<PedalInstance>, PresetRef)> {
        let tab = self.tab.as_ref()?;
        let track = tab.score.tracks.get(self.selected_track);
        let guess = track.and_then(guess_tone);
        let track_name = track.map(|t| t.name.as_str()).unwrap_or_default();
        let presets: Vec<&(Preset, PathBuf)> =
            self.song_presets.iter().filter(|(p, _)| preset_fits_track(p, track_name)).collect();
        if guess.is_none() && presets.is_empty() {
            return None;
        }
        let mut chosen = None;
        ui.horizontal_wrapped(|ui| {
            ui.label("💡 Timbre:")
                .on_hover_text("Ponto de partida para o timbre. Não há como copiar o timbre exato da gravação.");
            for (preset, path) in presets {
                let estimated = preset.tags.iter().any(|t| t == ESTIMATED_TAG);
                let (label, hover) = if estimated {
                    (
                        format!("🎛 {}", preset.name),
                        "Timbre estimado da gravação (experimental): um ponto de partida, ajuste de ouvido",
                    )
                } else {
                    (format!("⭐ {}", preset.name), "Seu preset salvo para esta música/artista")
                };
                if ui.button(label).on_hover_text(hover).clicked() {
                    chosen = Some((preset.pedals.clone(), PresetRef::User(path.clone())));
                }
            }
            if let Some(guess) = &guess {
                let response = ui.button(format!("Aplicar \"{}\"", guess.factory_preset)).on_hover_text(guess.reason.as_str());
                if response.clicked()
                    && let Some(preset) = factory_presets().into_iter().find(|p| p.name == guess.factory_preset)
                {
                    chosen = Some((preset.pedals, PresetRef::Factory(preset.name)));
                }
                ui.label(RichText::new(guess.reason.as_str()).weak());
            }
        });
        if chosen.is_some() {
            self.set_message("Timbre aplicado à cadeia de pedais.", false);
        }
        chosen
    }

    fn file_row(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            #[cfg(not(target_os = "android"))]
            if ui.button("📂 Abrir tab...").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Guitar Pro", SUPPORTED_EXTENSIONS)
                    .pick_file()
            {
                self.load_tab(path);
            }
            #[cfg(target_os = "android")]
            {
                ui.label("Arquivo:");
                ui.text_edit_singleline(&mut self.path_input);
                if ui.button("Abrir tab").clicked() {
                    self.load_tab(PathBuf::from(self.path_input.trim()));
                }
            }
            if ui
                .add_enabled(!self.transcriber.is_running(), egui::Button::new("⚡ Transcrever áudio..."))
                .on_hover_text("Gera uma tab (guitarra e baixo) a partir de um arquivo de música")
                .clicked()
            {
                self.transcriber.show_form(None, None, None, None);
            }
            if self.loading_tab.is_some() {
                ui.spinner();
            }
            if let Some(tab) = &self.tab {
                let s = &tab.score;
                let title = if s.title.is_empty() { "(sem título)" } else { &s.title };
                ui.label(RichText::new(title).strong());
                if !s.artist.is_empty() {
                    ui.label(format!("— {}", s.artist));
                }
                ui.label(
                    RichText::new(format!("{:.0} bpm · {} compassos", s.initial_tempo, s.bars.len()))
                        .weak(),
                );
            }
        });
    }

    fn transport_row(&mut self, ui: &mut Ui) {
        let Some((score, timeline)) = self.tab.as_ref().map(|t| (t.score.clone(), t.timeline.clone())) else {
            return;
        };
        let player = self.player.clone();
        let status = player.status();
        let playing = status.playing();
        let backing_on = self.use_backing && self.backing.is_some();
        ui.horizontal_wrapped(|ui| {
            let play_label = if playing { "⏸ Pausar" } else { "▶ Tocar" };
            if ui.button(play_label).on_hover_text("Espaço").clicked() {
                self.toggle_play();
            }
            if ui.button("⏹").on_hover_text("Parar e voltar ao início").clicked() {
                self.player.stop();
            }
            ui.monospace(format!(
                "{} / {}",
                format_time(status.score_secs()),
                format_time(timeline.duration_secs)
            ));
            ui.separator();

            ui.label("Faixa:");
            let tracks = &score.tracks;
            let current = tracks.get(self.selected_track).map(|t| t.name.as_str()).unwrap_or("");
            egui::ComboBox::from_id_salt("tab_track_select")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (i, track) in tracks.iter().enumerate() {
                        let name = if track.name.is_empty() { format!("Faixa {}", i + 1) } else { track.name.clone() };
                        ui.selectable_value(&mut self.selected_track, i, name);
                    }
                });
            ui.checkbox(&mut self.follow_cursor, "Seguir cursor");
            ui.separator();

            let speed = ui.add_enabled(
                !backing_on,
                egui::Slider::new(&mut self.speed, 0.25..=1.5).text("velocidade").fixed_decimals(2),
            );
            if backing_on {
                speed.on_disabled_hover_text("A velocidade só vale para o áudio sintetizado.");
            } else if speed.changed() {
                self.player.set_speed(self.speed as f64);
            }
            ui.toggle_value(&mut self.show_mixer, "🔉 Mixer");
            let mut practice = self.practice_on;
            if ui
                .toggle_value(&mut practice, "🎯 Prática")
                .on_hover_text("Confere, nota a nota, se você tocou a faixa selecionada corretamente")
                .changed()
            {
                self.set_practice(practice);
            }
            let mut looping = self.practice_loop.on;
            if ui
                .toggle_value(&mut looping, "🔁 Loop")
                .on_hover_text("Repete um trecho. Com o loop ligado, Shift+clique num compasso marca o fim.")
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
            egui::ComboBox::from_id_salt("tab_count_in")
                .selected_text(match self.count_in_bars {
                    0 => "sem contagem".to_string(),
                    1 => "contagem: 1 compasso".to_string(),
                    n => format!("contagem: {n} compassos"),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.count_in_bars, 0, "sem contagem");
                    ui.selectable_value(&mut self.count_in_bars, 1, "1 compasso");
                    ui.selectable_value(&mut self.count_in_bars, 2, "2 compassos");
                })
                .response
                .on_hover_text("Cliques antes de começar a tocar");
            if status.counting_in() {
                ui.colored_label(Color32::from_rgb(255, 170, 60), "contando...");
            }
        });
    }

    /// The selected track's tuning, with a shortcut to the tuner.
    fn tuning_row(&mut self, ui: &mut Ui) {
        let Some(tuning) = self.selected_tuning().and_then(|t| Tuning::from_tab(&t)) else { return };
        let standard = tuning.strings == Tuning::standard().strings;
        ui.horizontal_wrapped(|ui| {
            let text = if standard {
                "🎸 Afinação padrão (E A D G B E)".to_string()
            } else {
                format!("🎸 Afinação: {} ({})", tuning.name, tuning_label(&tuning.strings))
            };
            let color = if standard { ui.visuals().weak_text_color() } else { Color32::from_rgb(240, 200, 70) };
            ui.label(RichText::new(text).color(color));
            if ui
                .small_button("🎵 Afinar")
                .on_hover_text("Abre o afinador com as cordas desta faixa")
                .clicked()
            {
                self.tuner_requested = true;
            }
        });
    }

    /// Loop range and speed trainer.
    fn loop_row(&mut self, ui: &mut Ui) {
        let Some(bar_count) = self.tab.as_ref().map(|t| t.score.bars.len()) else { return };
        let backing_on = self.use_backing && self.backing.is_some();
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("🔁 Loop").strong());
            let (mut first, mut last) = (self.practice_loop.bars.0 + 1, self.practice_loop.bars.1 + 1);
            ui.label("compassos");
            changed |= ui.add(egui::DragValue::new(&mut first).range(1..=bar_count)).changed();
            ui.label("a");
            changed |= ui.add(egui::DragValue::new(&mut last).range(1..=bar_count)).changed();
            if changed {
                self.practice_loop.bars = (first.min(last) - 1, first.max(last) - 1);
            }
            ui.label(RichText::new(format!("· passadas: {}", self.practice_loop.good_passes)).weak())
                .on_hover_text("Passadas boas (com o modo prática, acima da exigência do treino)");
            ui.separator();
            ui.add_enabled_ui(!backing_on, |ui| {
                let trainer = ui
                    .checkbox(&mut self.practice_loop.trainer, "Treino de velocidade")
                    .on_hover_text("Começa devagar e acelera a cada passada boa");
                if trainer.changed() && self.practice_loop.trainer {
                    self.speed = self.practice_loop.start_speed;
                    self.player.set_speed(self.speed as f64);
                    self.practice_loop.good_passes = 0;
                }
                if self.practice_loop.trainer {
                    let pl = &mut self.practice_loop;
                    ui.add(egui::DragValue::new(&mut pl.start_speed).range(0.25..=1.5).speed(0.01).custom_formatter(|v, _| format!("de {:.0}%", v * 100.0)));
                    ui.add(egui::DragValue::new(&mut pl.step).range(0.01..=0.25).speed(0.005).custom_formatter(|v, _| format!("+{:.0}%", v * 100.0)));
                    ui.add(egui::DragValue::new(&mut pl.target_speed).range(0.25..=1.5).speed(0.01).custom_formatter(|v, _| format!("até {:.0}%", v * 100.0)));
                    ui.add(egui::DragValue::new(&mut pl.pass_accuracy).range(0.5..=1.0).speed(0.01).custom_formatter(|v, _| format!("passa com {:.0}%", v * 100.0)))
                        .on_hover_text("Com o modo prática ligado, só acelera se a passada tiver esta precisão");
                    ui.label(RichText::new(format!("agora {:.0}%", self.speed * 100.0)).strong());
                }
            })
            .response
            .on_disabled_hover_text("O treino de velocidade só funciona com o áudio sintetizado (desligue o áudio original).");
        });
        if changed {
            self.apply_loop();
        }
    }

    fn practice_row(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        self.ensure_practice();
        let Some((_, _, session)) = &mut self.practice else { return };
        let mut prefs_changed = false;
        ui.horizontal_wrapped(|ui| {
            if session.beat_count() == 0 {
                ui.colored_label(Color32::YELLOW, "Esta faixa não tem notas para conferir (bateria ou vazia).");
                return;
            }
            let stats = session.stats();
            let accuracy = stats
                .note_accuracy()
                .map(|a| format!("{:.0}%", a * 100.0))
                .unwrap_or_else(|| "—".to_string());
            ui.label(RichText::new(format!("🎯 {accuracy}")).strong())
                .on_hover_text("Notas certas / notas conferidas");
            ui.label(format!(
                "notas {}/{} · tempos {}/{} · sequência {} (melhor {})",
                stats.notes_hit,
                stats.notes_judged,
                stats.beats_hit,
                stats.beats_judged,
                stats.streak,
                stats.best_streak
            ));
            if ui.button("Zerar").clicked() {
                session.reset();
            }
            ui.checkbox(&mut self.show_heatmap, "mapa de acertos")
                .on_hover_text("Pinta cada compasso de vermelho (errou muito) a verde (acertou)");
            ui.separator();
            let prefs = &mut self.practice_prefs;
            prefs_changed |= ui
                .add(egui::DragValue::new(&mut prefs.extra_latency_ms).range(-200.0..=500.0).suffix(" ms"))
                .on_hover_text(
                    "Latência extra, somada à do buffer de áudio. Aumente se notas tocadas no tempo aparecem como erradas.",
                )
                .changed();
            ui.label("latência extra");
            prefs_changed |= ui
                .add(egui::Slider::new(&mut prefs.tolerance_ms, 30.0..=300.0).suffix(" ms").text("tolerância"))
                .on_hover_text("Quanto antes ou depois do tempo a nota ainda vale")
                .changed();
            prefs_changed |= ui
                .add(egui::Slider::new(&mut prefs.strictness, 0.0..=1.0).fixed_decimals(2).text("exigência"))
                .on_hover_text("Baixe se notas certas aparecem em vermelho (ex.: muita distorção ou reverb)")
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
        let track_name = tab.score.tracks.get(self.selected_track).map(|t| t.name.clone()).unwrap_or_default();
        let record = settings.practice_log.get(&tab_key(&tab.path));
        let hardest = self
            .practice
            .as_ref()
            .map(|(_, _, s)| hardest_bars(&s.bar_results(), 2))
            .unwrap_or_default();
        let mut loop_bar = None;
        ui.horizontal_wrapped(|ui| {
            if let Some(record) = record {
                let best = record.best_accuracy.get(&track_name).map(|a| format!("{:.0}%", a * 100.0));
                ui.label(
                    RichText::new(format!(
                        "Histórico: melhor {} · {} sessões",
                        best.unwrap_or_else(|| "—".into()),
                        record.runs
                    ))
                    .weak(),
                );
                ui.separator();
            }
            let weak: Vec<&(usize, f32)> = hardest.iter().filter(|(_, a)| *a < 0.9).take(4).collect();
            if !weak.is_empty() {
                ui.label("Compassos mais difíceis:");
                for (bar, accuracy) in weak {
                    if ui
                        .small_button(format!("{} ({:.0}%)", bar + 1, accuracy * 100.0))
                        .on_hover_text("Repetir este compasso em loop")
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
            for (i, track) in tab.score.tracks.iter().enumerate() {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        let name = if track.name.is_empty() { format!("Faixa {}", i + 1) } else { track.name.clone() };
                        if ui.selectable_label(self.selected_track == i, name).clicked() {
                            self.selected_track = i;
                        }
                        tracks_changed |= ui
                            .toggle_value(&mut self.muted[i], "M")
                            .on_hover_text("Mudo")
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
            let synth = ui.add(egui::Slider::new(&mut self.synth_gain, 0.0..=2.0).text("volume do sintetizador"));
            if synth.changed() {
                self.player.set_gains(self.synth_gain, self.backing_gain);
            }
            ui.separator();
            if self.loading_soundfont.is_some() {
                ui.spinner();
                ui.label("Carregando SoundFont...");
            } else if let Some(name) = &self.soundfont_name {
                ui.label(format!("SoundFont: {name}"));
            } else {
                ui.colored_label(Color32::YELLOW, "Sem SoundFont: o sintetizador está mudo.")
                    .on_hover_text(
                        "Escolha um arquivo .sf2 (por exemplo FluidR3_GM.sf2 ou GeneralUser GS).",
                    );
            }
            #[cfg(not(target_os = "android"))]
            if ui.button("Escolher SoundFont...").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("SoundFont", &["sf2"]).pick_file()
            {
                self.load_soundfont(path);
            }
            #[cfg(target_os = "android")]
            if ui.button("Usar caminho como SoundFont").clicked() {
                self.load_soundfont(PathBuf::from(self.path_input.trim()));
            }
        });
    }

    fn backing_section(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        let Some(timeline) = self.tab.as_ref().map(|t| t.timeline.clone()) else { return };
        let mut sync_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Áudio original:").strong());
            #[cfg(not(target_os = "android"))]
            if ui.button("🎵 Abrir áudio...").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Áudio", AUDIO_EXTENSIONS)
                    .pick_file()
            {
                self.load_backing_file(path);
            }
            #[cfg(target_os = "android")]
            if ui.button("Usar caminho como áudio").clicked() {
                self.load_backing_file(PathBuf::from(self.path_input.trim()));
            }
            if self.loading_backing.is_some() {
                ui.spinner();
                ui.label("Decodificando...");
            }
            match &self.backing {
                None => {
                    ui.label(RichText::new("nenhum (mp3, ogg, flac, wav, m4a)").weak());
                }
                Some(backing) => {
                    let name = match &backing.source {
                        BackingSource::File(p) => p
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        BackingSource::Embedded => "embutido no arquivo GP".to_string(),
                    };
                    ui.label(format!("{name} ({})", format_time(backing.duration_secs)));
                    if ui.checkbox(&mut self.use_backing, "tocar").changed() {
                        self.player.set_use_backing(self.use_backing);
                    }
                    let vol = ui.add(egui::Slider::new(&mut self.backing_gain, 0.0..=2.0).text("volume"));
                    if vol.changed() {
                        self.player.set_gains(self.synth_gain, self.backing_gain);
                    }
                    if ui.button("Remover").clicked() {
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
            ui.label(RichText::new("Sincronização: compassos detectados na transcrição.").weak());
        } else if self.backing.is_some() && embedded_sync {
            ui.label(RichText::new("Sincronização: pontos de sincronia do próprio arquivo (Guitar Pro 8).").weak());
        } else if self.backing.is_some() {
            let player = self.player.clone();
            let status = player.status();
            ui.horizontal_wrapped(|ui| {
                ui.label("Sincronia:");
                sync_changed |= ui
                    .add(egui::DragValue::new(&mut self.sync_offset).speed(0.01).range(-600.0..=600.0).suffix(" s").fixed_decimals(2))
                    .on_hover_text("Tempo no áudio em que o compasso 1 começa")
                    .changed();
                ui.label("tempo do áudio ×");
                sync_changed |= ui
                    .add(egui::DragValue::new(&mut self.tempo_ratio).speed(0.001).range(0.5..=2.0).fixed_decimals(3))
                    .on_hover_text("Andamento da gravação dividido pelo andamento da tab")
                    .changed();
                if ui
                    .button("⏮ Ouvir do início do áudio")
                    .on_hover_text("Toca o áudio desde 0:00 para marcar onde o compasso 1 começa")
                    .clicked()
                {
                    self.player.seek_audio(0.0);
                    self.player.play();
                }
                if ui
                    .button("Compasso 1 começa agora")
                    .on_hover_text("Clique no instante em que o compasso 1 começa na gravação")
                    .clicked()
                {
                    self.sync_offset = status.audio_secs();
                    sync_changed = true;
                }
                if ui
                    .button("Ajustar andamento aqui")
                    .on_hover_text(
                        "Com o compasso 1 já marcado: clique no início de um compasso mais adiante para corrigir a diferença de andamento",
                    )
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
                if ui.button("Zerar").clicked() {
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
