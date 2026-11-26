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
    #[cfg(target_os = "android")]
    path_input: String,
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
        if !self.practice_on {
            return;
        }
        self.ensure_practice();
        let settings = self.practice_settings(audio_latency_secs as f64);
        let status = self.player.status();
        let (score_secs, playing) = (status.score_secs(), status.playing());
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
        ui.separator();
        self.transport_row(ui);
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
        });
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
        let width = ui.available_width();
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
        let response = tab_view(ui, &tab.score, track, layout, cursor, follow, marks);
        if let Some(bar) = response.clicked_bar
            && let Some(occ) = tab.timeline.closest_occurrence(bar, score_secs)
        {
            self.player.seek(tab.timeline.occurrences[occ].start_secs);
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

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert!(has_extension(Path::new("/a/Song.GP5"), SUPPORTED_EXTENSIONS));
        assert!(!has_extension(Path::new("/a/song.txt"), SUPPORTED_EXTENSIONS));
    }
}
