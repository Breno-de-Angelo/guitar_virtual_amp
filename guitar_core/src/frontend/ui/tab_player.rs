//! "Tabs" page: open a Guitar Pro file, see its tablature with a playback
//! cursor, and play it with the SoundFont synth (per-track mute/solo) and/or
//! an original recording synchronized to the score.
//!
//! File parsing, audio decoding and SoundFont loading run on background
//! threads; results are polled once per frame.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam::channel::{bounded, Receiver};
use eframe::egui::{self, Color32, Key, RichText, Ui};
use rustysynth::SoundFont;

use crate::backend::audio_file::{self, DecodedAudio, AUDIO_EXTENSIONS};
use crate::backend::song_player::SongPlayer;
use crate::frontend::ui::tab_view::{tab_view, CursorPos, TabLayout};
use crate::shared::settings::{save_settings, AppSettings, TabSyncSettings};
use crate::shared::tab::import::{load_score_file, SUPPORTED_EXTENSIONS};
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
    #[cfg(target_os = "android")]
    path_input: String,
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
        self.backing = None;
        self.file_sync = SyncMap::from_file_points(&score.sync_points, &tab.timeline);

        let saved = settings.tab_sync.get(&tab_key(&tab.path)).cloned().unwrap_or_default();
        self.sync_offset = saved.offset_secs;
        self.tempo_ratio = saved.tempo_ratio;
        if let Some(bytes) = score.backing_audio.clone() {
            self.loading_backing = Some(spawn(move || {
                let audio = audio_file::decode_bytes(bytes)?;
                Ok((BackingSource::Embedded, Arc::new(audio)))
            }));
        } else if let Some(path) = saved.audio_path.filter(|p| p.is_file()) {
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

    fn toggle_play(&self) {
        if self.player.status().playing() {
            self.player.pause();
        } else {
            self.player.play();
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
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
        if self.tab.is_none() {
            ui.add_space(20.0);
            ui.label(
                "Abra (ou arraste para a janela) um arquivo Guitar Pro: .gp3, .gp4, .gp5, .gpx ou .gp.",
            );
            return;
        }
        ui.separator();
        self.transport_row(ui);
        if self.show_mixer {
            ui.separator();
            self.mixer(ui);
            ui.separator();
            self.backing_section(ui, settings);
        }
        ui.separator();
        self.tab_area(ui);
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
            ui.toggle_value(&mut self.show_mixer, "🎚 Mixer");
        });
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
        if self.backing.is_some() && embedded_sync {
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
        let response = tab_view(ui, &tab.score, track, layout, cursor, follow);
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
    fn extension_matching_is_case_insensitive() {
        assert!(has_extension(Path::new("/a/Song.GP5"), SUPPORTED_EXTENSIONS));
        assert!(!has_extension(Path::new("/a/song.txt"), SUPPORTED_EXTENSIONS));
    }
}
