//! "Transcrever áudio" form shown on the Tabs page. The audio comes from a
//! local file or from YouTube (search or pasted link, downloaded with
//! yt-dlp); `crate::transcribe` then runs on a worker thread with a
//! progress bar. The result lands in the tab repository and is handed back
//! to the tab player to open.
//!
//! YouTube results can be heard in the app before choosing one (▶ on a
//! row: the audio is downloaded into the cache's preview folder and played
//! through `SongPlayer::start_preview`), more results can be loaded, and
//! videos that already have a tab are marked, with a button to open it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam::channel::{Receiver, unbounded};
use eframe::egui::{self, Color32, Key, RichText, Ui, Vec2};

use crate::backend::audio_file::{AUDIO_EXTENSIONS, DecodedAudio, decode_file};
use crate::backend::song_player::SongPlayer;
use crate::frontend::ui::guitar_calibration::GuitarCalibrationPanel;
use crate::frontend::ui::tone3000::remote_image;
use crate::shared::i18n::{self, Language, tr};
use crate::shared::settings::{AppSettings, save_settings};
use crate::shared::tab::library::{clean_title, words};
use crate::shared::tab::repository::{default_repository_dir, youtube_tabs};
use crate::transcribe::fingering_net::Fingering;
use crate::transcribe::guitar_crnn::GuitarModel;
use crate::transcribe::separation::{self, Separator};
use crate::transcribe::youtube::{self, Video};
use crate::transcribe::{self, InstalledModels, Output, Progress, Request};
use crate::trf;

const SEARCH_RESULTS: usize = 10;
/// A result whose length differs from the expected one (e.g. Spotify's) by
/// more than this is flagged: likely a live take or a video with an intro.
const DURATION_WARNING_SECS: f64 = 10.0;

/// One page of search results, plus the tabs already made from YouTube.
struct SearchPage {
    videos: Vec<Video>,
    existing: HashMap<String, PathBuf>,
}

struct Search {
    rx: Receiver<Result<SearchPage, String>>,
    /// Adds a page to the current results instead of replacing them.
    append: bool,
}

enum PreviewMsg {
    Progress(f32),
    Ready(Result<Arc<DecodedAudio>, String>),
}

/// A preview being downloaded and decoded.
struct PreviewLoad {
    id: String,
    rx: Receiver<PreviewMsg>,
    cancel: Arc<AtomicBool>,
    progress: f32,
}

/// What the user did in the results list.
enum RowAction {
    Select(usize),
    Preview(usize),
    Seek(f64),
    Open(PathBuf),
    More,
}

/// The clip loaded in the player.
struct LoadedPreview {
    id: String,
    duration: f64,
}

enum Msg {
    Progress(Progress),
    Done(Box<Result<Output, String>>),
}

struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    progress: Option<Progress>,
    title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    File,
    YouTube,
}

pub struct TranscribePanel {
    pub open: bool,
    source: Source,
    audio_path: Option<PathBuf>,
    title: String,
    artist: String,
    separate: bool,
    guitar: bool,
    bass: bool,
    spotify_track_id: Option<String>,
    /// Expected song length (e.g. from Spotify), to pick the right video.
    duration_hint: Option<f64>,
    separator: Option<Separator>,
    /// Optional models installed.
    models: InstalledModels,
    ytdlp: Option<PathBuf>,
    query: String,
    /// The query behind `results` (for loading more of them).
    results_query: String,
    searching: Option<Search>,
    results: Vec<Video>,
    /// Search positions already fetched for `results_query`.
    fetched: usize,
    more_results: bool,
    /// Tabs made from YouTube videos, by video ID.
    existing: HashMap<String, PathBuf>,
    selected: Option<usize>,
    /// Search for the prefilled song on the next frame (which knows the
    /// repository folder).
    pending_search: bool,
    focus_query: bool,
    repo_dir: Option<PathBuf>,
    player: SongPlayer,
    preview_load: Option<PreviewLoad>,
    preview: Option<LoadedPreview>,
    /// An existing tab the user chose to open instead of transcribing.
    open_request: Option<PathBuf>,
    job: Option<Job>,
    error: Option<String>,
    /// "My guitar": the player's guitar for tone estimates.
    calibration: GuitarCalibrationPanel,
    #[cfg(target_os = "android")]
    path_input: String,
}

impl Default for TranscribePanel {
    fn default() -> Self {
        Self::new(SongPlayer::new())
    }
}

/// Where transcriptions are saved.
pub fn repository_dir(settings: &AppSettings) -> Option<PathBuf> {
    settings
        .tab_repository_dir
        .clone()
        .or_else(default_repository_dir)
}

/// One line naming the separator and models a transcription would use.
fn engines_summary(
    language: Language,
    separator: Option<&Separator>,
    models: &InstalledModels,
    choice: GuitarModel,
    fingering: Fingering,
) -> String {
    let pt = language == Language::Portuguese;
    let t = |en: &'static str, pt_text: &'static str| if pt { pt_text } else { en };
    let separation = separator.map_or(t("none", "nenhuma"), Separator::label);
    let notes = if choice == GuitarModel::Electric && models.electric_crnn {
        t("electric CRNN (local fine-tune)", "CRNN elétrica (ajuste local)")
    } else if models.guitar_crnn {
        "CRNN GAPS"
    } else {
        "Basic Pitch"
    };
    let beats = match models.beat_tracker {
        Some(transcribe::beat_net::FINAL_ENGINE_NAME) => "Beat This! final0",
        Some(_) => "Beat This! small0",
        None => t("simple detector", "detector simples"),
    };
    let fingering = if fingering == Fingering::Learned && models.fingering_tagger {
        t("learned", "aprendido")
    } else {
        "Viterbi"
    };
    if pt {
        format!("Separação: {separation} · notas: {notes} · tempos: {beats} · dedilhado: {fingering}")
    } else {
        format!("Separation: {separation} · notes: {notes} · beats: {beats} · fingering: {fingering}")
    }
}

fn format_secs(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// A view count the way YouTube writes it in the UI language.
fn format_views(views: u64) -> String {
    views_text(views, i18n::language())
}

/// "1.6M views" in English, "1,6 mi de visualizações" in Portuguese.
fn views_text(views: u64, language: Language) -> String {
    let pt = language == Language::Portuguese;
    let short = |value: f64, unit: &str| {
        let text = if value < 10.0 {
            format!("{:.1}", (value * 10.0).floor() / 10.0)
        } else {
            format!("{}", value.floor())
        };
        let text = text.trim_end_matches(".0");
        if pt { format!("{} {unit}", text.replace('.', ",")) } else { format!("{text}{unit}") }
    };
    let v = views as f64;
    match (pt, v) {
        (true, v) if v >= 1e9 => format!("{} de visualizações", short(v / 1e9, "bi")),
        (true, v) if v >= 1e6 => format!("{} de visualizações", short(v / 1e6, "mi")),
        (true, v) if v >= 1e3 => format!("{} visualizações", short(v / 1e3, "mil")),
        (true, _) if views == 1 => "1 visualização".into(),
        (true, _) => format!("{views} visualizações"),
        (false, v) if v >= 1e9 => format!("{} views", short(v / 1e9, "B")),
        (false, v) if v >= 1e6 => format!("{} views", short(v / 1e6, "M")),
        (false, v) if v >= 1e3 => format!("{} views", short(v / 1e3, "K")),
        (false, _) if views == 1 => "1 view".into(),
        (false, _) => format!("{views} views"),
    }
}

/// Downloads (unless cached) and decodes a video's audio for preview.
fn load_preview(
    ytdlp: &std::path::Path,
    id: &str,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Arc<DecodedAudio>, String> {
    let cache = youtube::audio_cache_dir()
        .ok_or(tr("no data folder to keep the audio in", "sem pasta de dados para guardar o áudio"))?;
    let path = match youtube::cached_audio(&cache, id) {
        Some(path) => path,
        None => youtube::download_audio(
            ytdlp,
            id,
            &youtube::preview_dir(&cache),
            &mut |p| progress(0.9 * p),
            cancel,
        )?,
    };
    progress(0.9);
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelado".into());
    }
    decode_file(&path).map(Arc::new)
}

impl TranscribePanel {
    /// `player` plays the YouTube previews (the tab player's engine, so they
    /// come out of the same output as the song).
    /// Guitar samples since the last frame (the analysis stream, at
    /// `rate`), for recording the player's guitar.
    pub fn feed_input(&mut self, samples: &[f32], rate: u32) {
        self.calibration.feed_input(samples, rate);
    }

    pub fn new(player: SongPlayer) -> Self {
        Self {
            open: false,
            source: Source::File,
            audio_path: None,
            title: String::new(),
            artist: String::new(),
            separate: true,
            guitar: true,
            bass: true,
            spotify_track_id: None,
            duration_hint: None,
            separator: None,
            models: InstalledModels::default(),
            ytdlp: None,
            query: String::new(),
            results_query: String::new(),
            searching: None,
            results: Vec::new(),
            fetched: 0,
            more_results: false,
            existing: HashMap::new(),
            selected: None,
            pending_search: false,
            focus_query: false,
            repo_dir: None,
            player,
            preview_load: None,
            preview: None,
            open_request: None,
            job: None,
            error: None,
            calibration: GuitarCalibrationPanel::default(),
            #[cfg(target_os = "android")]
            path_input: String::new(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// A tab the user chose to open from the results (already transcribed).
    pub fn take_open_request(&mut self) -> Option<PathBuf> {
        self.open_request.take()
    }

    /// Opens the form, optionally prefilled (e.g. from a Spotify track).
    /// With a title and yt-dlp installed, it goes straight to a YouTube search.
    pub fn show_form(
        &mut self,
        title: Option<String>,
        artist: Option<String>,
        spotify_track_id: Option<String>,
        duration_hint: Option<f64>,
    ) {
        self.open = true;
        self.error = None;
        self.separator = separation::find_separator();
        self.models = crate::transcribe::installed_models();
        self.ytdlp = youtube::find_ytdlp();
        if !self.has_preview() {
            // Leftovers from a session that ended without closing the form.
            if let Some(cache) = youtube::audio_cache_dir() {
                youtube::clear_previews(&cache);
            }
        }
        self.pending_search = self.prefill(title, artist, spotify_track_id, duration_hint);
        self.focus_query = !self.pending_search && self.source == Source::YouTube;
    }

    fn has_preview(&self) -> bool {
        self.preview.is_some() || self.preview_load.is_some()
    }

    /// Stops and forgets the preview (the downloads stay until `close`).
    fn stop_preview(&mut self) {
        if let Some(load) = self.preview_load.take() {
            load.cancel.store(true, Ordering::Relaxed);
        }
        if self.preview.take().is_some() {
            self.player.stop_preview();
        }
    }

    /// Closes the form, deleting the preview downloads.
    fn close(&mut self) {
        self.open = false;
        let had_preview = self.has_preview();
        self.stop_preview();
        if (had_preview || !self.results.is_empty())
            && let Some(cache) = youtube::audio_cache_dir()
        {
            youtube::clear_previews(&cache);
        }
    }

    /// Fills the form; returns whether a YouTube search should start.
    fn prefill(
        &mut self,
        title: Option<String>,
        artist: Option<String>,
        spotify_track_id: Option<String>,
        duration_hint: Option<f64>,
    ) -> bool {
        let prefilled = title.is_some();
        if let Some(title) = title {
            self.title = title;
        }
        if let Some(artist) = artist {
            self.artist = artist;
        }
        if spotify_track_id.is_some() {
            self.spotify_track_id = spotify_track_id;
        }
        self.duration_hint = duration_hint;
        if self.ytdlp.is_some() && self.audio_path.is_none() {
            self.source = Source::YouTube;
        }
        if prefilled && self.ytdlp.is_some() {
            self.query = format!("{} {}", self.artist, self.title).trim().to_string();
            return true;
        }
        false
    }

    fn set_audio(&mut self, path: PathBuf) {
        let (artist, title) = transcribe::guess_artist_title(&path);
        // Keep what came from Spotify; fill the rest from the file name.
        if self.title.trim().is_empty() {
            self.title = title;
        }
        if self.artist.trim().is_empty() {
            self.artist = artist;
        }
        self.audio_path = Some(path);
    }

    /// Searches for the query (or looks up a pasted link). With `more`, the
    /// next page of the last search is added to the results.
    fn start_search(&mut self, more: bool) {
        let Some(ytdlp) = self.ytdlp.clone() else {
            return;
        };
        let query = if more { self.results_query.clone() } else { self.query.trim().to_string() };
        if query.is_empty() || self.searching.is_some() {
            return;
        }
        let skip = if more { self.fetched } else { 0 };
        let (title, artist) = self.ranking_names(&query);
        let hint = self.duration_hint;
        let repo_dir = self.repo_dir.clone();
        let (tx, rx) = crossbeam::channel::bounded(1);
        let q = query.clone();
        std::thread::spawn(move || {
            let result = match youtube::parse_video_id(&q) {
                Some(id) => youtube::video_info(&ytdlp, &id).map(|v| vec![v]),
                None => youtube::search_page(&ytdlp, &q, skip, SEARCH_RESULTS)
                    .map(|videos| youtube::rank(videos, &title, &artist, hint)),
            };
            let existing = repo_dir.as_deref().map(youtube_tabs).unwrap_or_default();
            let _ = tx.send(result.map(|videos| SearchPage { videos, existing }));
        });
        if !more {
            self.stop_preview();
            self.results.clear();
            self.selected = None;
            self.results_query = query;
            self.fetched = 0;
            self.more_results = false;
        }
        self.fetched = skip + SEARCH_RESULTS;
        self.error = None;
        self.searching = Some(Search { rx, append: more });
    }

    /// `(title, artist)` to rank results for `query` against: the form's
    /// names when the query contains the title (they came from Spotify or
    /// match what was typed), else the query itself, so a new search isn't
    /// ranked against the previous song.
    fn ranking_names(&self, query: &str) -> (String, String) {
        let title_words = words(&clean_title(&self.title));
        let query_words = words(query);
        if !title_words.is_empty() && title_words.iter().all(|w| query_words.contains(w)) {
            (self.title.clone(), self.artist.clone())
        } else {
            (query.to_string(), String::new())
        }
    }

    /// Plays the preview of result `index`, or pauses/resumes it.
    fn toggle_preview(&mut self, index: usize) {
        let Some(video) = self.results.get(index).cloned() else { return };
        if self.selected != Some(index) {
            self.select(index);
        }
        if let Some(loaded) = &self.preview
            && loaded.id == video.id
        {
            let playing = self.player.status().previewing();
            self.player.set_preview_playing(!playing);
            return;
        }
        if self.preview_load.as_ref().is_some_and(|l| l.id == video.id) {
            return;
        }
        self.stop_preview();
        let Some(ytdlp) = self.ytdlp.clone() else { return };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded();
        let (flag, id) = (cancel.clone(), video.id.clone());
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let mut report = |p| {
                let _ = progress_tx.send(PreviewMsg::Progress(p));
            };
            let result = load_preview(&ytdlp, &id, &mut report, &flag);
            let _ = tx.send(PreviewMsg::Ready(result));
        });
        self.error = None;
        self.preview_load = Some(PreviewLoad { id: video.id, rx, cancel, progress: 0.0 });
    }

    fn poll_preview(&mut self) {
        let Some(load) = &mut self.preview_load else { return };
        let mut ready = None;
        for msg in load.rx.try_iter() {
            match msg {
                PreviewMsg::Progress(p) => load.progress = p,
                PreviewMsg::Ready(result) => ready = Some(result),
            }
        }
        let Some(result) = ready else { return };
        let id = load.id.clone();
        self.preview_load = None;
        match result {
            Ok(audio) => {
                let duration = audio.duration_secs();
                self.player.start_preview(audio, 0.0);
                self.preview = Some(LoadedPreview { id, duration });
            }
            Err(e) if e == "cancelado" => {}
            Err(e) => self.error = Some(trf!("Couldn't play the video: {e}", "Não foi possível ouvir o vídeo: {e}")),
        }
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        if let Some(video) = self.results.get(index) {
            let (artist, title) = video.guess_artist_title();
            // Spotify's names win; otherwise use the video's.
            if self.spotify_track_id.is_none() {
                self.artist = artist;
                self.title = title;
            } else {
                if self.title.trim().is_empty() {
                    self.title = title;
                }
                if self.artist.trim().is_empty() {
                    self.artist = artist;
                }
            }
        }
    }

    fn poll_search(&mut self) {
        let Some(search) = &self.searching else { return };
        let Ok(result) = search.rx.try_recv() else { return };
        let append = search.append;
        self.searching = None;
        match result {
            Ok(page) => self.add_results(page, append),
            Err(e) => self.error = Some(e),
        }
    }

    fn add_results(&mut self, page: SearchPage, append: bool) {
        self.existing = page.existing;
        let link = youtube::parse_video_id(&self.results_query).is_some();
        let before = self.results.len();
        for video in page.videos {
            if !self.results.iter().any(|v| v.id == video.id) {
                self.results.push(video);
            }
        }
        let added = self.results.len() - before;
        self.more_results = !link && added > 0 && self.fetched < youtube::MAX_SEARCH_RESULTS;
        if self.results.is_empty() {
            self.error = Some(tr("Nothing found on YouTube.", "Nada encontrado no YouTube.").into());
        } else if !append {
            self.select(0);
        } else if added == 0 {
            self.error = Some(tr("No more results.", "Não há mais resultados.").into());
        }
    }

    fn ready(&self) -> bool {
        let source_ok = match self.source {
            Source::File => self.audio_path.is_some(),
            Source::YouTube => self.ytdlp.is_some() && self.selected.is_some(),
        };
        source_ok && (self.guitar || self.bass)
    }

    fn start(&mut self, settings: &AppSettings) {
        self.stop_preview();
        let Some(output_dir) = repository_dir(settings) else {
            self.error = Some(tr("No folder for the tab repository.", "Sem pasta para o repositório de tabs.").into());
            return;
        };
        let video = match self.source {
            Source::YouTube => match (
                self.ytdlp.clone(),
                self.selected.and_then(|i| self.results.get(i)).cloned(),
            ) {
                (Some(ytdlp), Some(video)) => Some((ytdlp, video)),
                _ => {
                    self.error = Some(tr("Choose a video.", "Escolha um vídeo.").into());
                    return;
                }
            },
            Source::File => None,
        };
        let audio_path = match (&video, &self.audio_path) {
            (Some(_), _) => PathBuf::new(),
            (None, Some(path)) => path.clone(),
            (None, None) => {
                self.error = Some(tr("Choose an audio file.", "Escolha um arquivo de áudio.").into());
                return;
            }
        };
        let request = Request {
            audio_path,
            title: self.title.trim().to_string(),
            artist: self.artist.trim().to_string(),
            separate: self.separate && self.separator.is_some(),
            guitar_model: settings.transcription_guitar_model,
            fingering: settings.transcription_fingering,
            tone: settings.transcription_tone && self.separate && self.separator.is_some(),
            guitar_colour: settings.tone_guitar.as_ref().map(|g| g.colour.clone()),
            guitar: self.guitar,
            bass: self.bass,
            output_dir,
            spotify_track_id: self.spotify_track_id.clone(),
            youtube_video_id: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded();
        let flag = cancel.clone();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let mut report = |p| {
                let _ = progress_tx.send(Msg::Progress(p));
            };
            let result = match video {
                Some((ytdlp, video)) => {
                    // A previewed video is already downloaded: keep that copy.
                    if let Some(cache) = youtube::audio_cache_dir() {
                        youtube::keep_preview(&cache, &video.id);
                        youtube::clear_previews(&cache);
                    }
                    transcribe::run_youtube(&ytdlp, &video.id, request, &mut report, &flag)
                }
                None => transcribe::run(&request, &mut report, &flag),
            };
            let _ = tx.send(Msg::Done(Box::new(result)));
        });
        self.error = None;
        self.job = Some(Job {
            rx,
            cancel,
            progress: None,
            title: self.title.clone(),
        });
    }

    /// Polls the running job; returns the finished transcription once.
    fn poll(&mut self) -> Option<Output> {
        let job = self.job.as_mut()?;
        let mut done = None;
        for msg in job.rx.try_iter() {
            match msg {
                Msg::Progress(p) => job.progress = Some(p),
                Msg::Done(result) => done = Some(result),
            }
        }
        match *done? {
            Ok(output) => {
                self.job = None;
                self.open = false;
                self.audio_path = None;
                self.title.clear();
                self.artist.clear();
                self.query.clear();
                self.results.clear();
                self.results_query.clear();
                self.more_results = false;
                self.selected = None;
                self.spotify_track_id = None;
                self.duration_hint = None;
                Some(output)
            }
            Err(e) => {
                self.job = None;
                if e != "cancelado" {
                    self.error = Some(e);
                }
                None
            }
        }
    }

    /// Draws the form (when open) and returns a finished transcription.
    pub fn ui(&mut self, ui: &mut Ui, settings: &mut AppSettings) -> Option<Output> {
        let finished = self.poll();
        self.repo_dir = repository_dir(settings);
        if std::mem::take(&mut self.pending_search) {
            self.start_search(false);
        }
        self.poll_search();
        self.poll_preview();
        if self.searching.is_some() || self.preview_load.is_some() || self.player.status().previewing() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(job) = &self.job {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let label = job
                        .progress
                        .map(|p| p.stage.label())
                        .unwrap_or(tr("Preparing", "Preparando"));
                    ui.label(
                        RichText::new(trf!("Transcribing \"{}\": {label}...", "Transcrevendo \"{}\": {label}...", job.title))
                            .strong(),
                    );
                    if ui.button(tr("Cancel", "Cancelar")).clicked() {
                        job.cancel.store(true, Ordering::Relaxed);
                    }
                });
                let fraction = job.progress.map(|p| p.overall).unwrap_or(0.0);
                ui.add(egui::ProgressBar::new(fraction).show_percentage());
            });
            return finished;
        }
        if !self.open {
            return finished;
        }
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("⚡ Transcribe audio", "⚡ Transcrever áudio")).strong());
                ui.label(
                    RichText::new(tr(
                        "makes the tab from a recording, all on this computer",
                        "gera a tab a partir de uma gravação, tudo neste computador",
                    ))
                    .weak(),
                );
            });
            ui.horizontal(|ui| {
                ui.label(tr("Source:", "Origem:"));
                ui.selectable_value(&mut self.source, Source::YouTube, "▶ YouTube");
                ui.selectable_value(&mut self.source, Source::File, tr("📁 File", "📁 Arquivo"));
            });
            match self.source {
                Source::File => self.file_source_ui(ui),
                Source::YouTube => self.youtube_source_ui(ui),
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(tr("Artist:", "Artista:"));
                ui.add(egui::TextEdit::singleline(&mut self.artist).desired_width(180.0));
                ui.label(tr("Song:", "Música:"));
                ui.add(egui::TextEdit::singleline(&mut self.title).desired_width(220.0));
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.guitar, tr("Guitar", "Guitarra"));
                ui.checkbox(&mut self.bass, tr("Bass", "Baixo"));
                ui.separator();
                let label = match &self.separator {
                    Some(separator) => trf!("Separate instruments ({})", "Separar instrumentos ({})", separator.label()),
                    None => tr("Separate instruments", "Separar instrumentos").to_string(),
                };
                ui.add_enabled_ui(self.separator.is_some(), |ui| {
                    ui.checkbox(&mut self.separate, label)
                        .on_hover_text(tr(
                            "Isolates guitar and bass before transcribing. Better for recordings with a band.",
                            "Isola guitarra e baixo antes de transcrever. Melhor em gravações com banda.",
                        ));
                });
                match &self.separator {
                    None => {
                        ui.label(
                            RichText::new(tr(
                                "Separator not installed: run scripts/install_bs_roformer.sh (or install_demucs.sh)",
                                "Separador não instalado: rode scripts/install_bs_roformer.sh (ou install_demucs.sh)",
                            ))
                                .color(Color32::YELLOW),
                        )
                        .on_hover_text(tr(
                            "Without separation, guitar and bass are transcribed from the whole mix (worse with a band).",
                            "Sem separação, guitarra e baixo são transcritos da mixagem inteira (pior com banda).",
                        ));
                    }
                    Some(Separator::Demucs(_)) => {
                        ui.label(
                            RichText::new(tr(
                                "scripts/install_bs_roformer.sh separates the guitar better",
                                "scripts/install_bs_roformer.sh separa melhor a guitarra",
                            ))
                            .small()
                            .weak(),
                        )
                            .on_hover_text(tr(
                                "BS-RoFormer SW (6 instruments) isolates the guitar better than Demucs; \
                                 the app uses it once it's installed. Needs ~4 GB of video card memory.",
                                "O BS-RoFormer SW (6 instrumentos) isola a guitarra melhor que o Demucs; \
                                 o app passa a usá-lo quando está instalado. Precisa de ~4 GB de memória na placa de vídeo.",
                            ));
                    }
                    Some(Separator::BsRoformer(_)) => {}
                }
            });
            if self.models.electric_crnn {
                ui.horizontal(|ui| {
                    ui.label(tr("Note model:", "Modelo de notas:"));
                    let mut choice = settings.transcription_guitar_model;
                    ui.selectable_value(&mut choice, GuitarModel::Gaps, tr("GAPS (default)", "GAPS (padrão)"))
                        .on_hover_text(tr(
                            "The guitar model from install_transcription_models.sh (Riley et al., classical guitar).",
                            "O modelo de guitarra do install_transcription_models.sh (Riley et al., violão clássico).",
                        ));
                    ui.selectable_value(&mut choice, GuitarModel::Electric, tr("Electric (local fine-tune)", "Elétrica (ajuste local)"))
                        .on_hover_text(tr(
                            "guitar_electric_crnn.onnx from the models folder: the same model fine-tuned on amplified \
                             electric guitar. Trained on unlicensed data: for local use only.",
                            "guitar_electric_crnn.onnx da pasta de modelos: o mesmo modelo ajustado em guitarra \
                             elétrica com amplificador. Treinado com dados sem licença: só para uso local.",
                        ));
                    if choice != settings.transcription_guitar_model {
                        settings.transcription_guitar_model = choice;
                        save_settings(settings);
                    }
                });
            }
            if self.models.fingering_tagger {
                ui.horizontal(|ui| {
                    ui.label(tr("Fingering:", "Dedilhado:"));
                    let mut choice = settings.transcription_fingering;
                    ui.selectable_value(&mut choice, Fingering::Learned, tr("Learned (default)", "Aprendido (padrão)"))
                        .on_hover_text(tr(
                            "fingering_tagger.onnx from the models folder: picks each note's string the way \
                             guitarists play it (more hits on solos and electric guitar). Trained on unlicensed \
                             data: for local use only.",
                            "fingering_tagger.onnx da pasta de modelos: escolhe a corda de cada nota como \
                             guitarristas tocam (mais acertos em solos e guitarra elétrica). Treinado com dados \
                             sem licença: só para uso local.",
                        ));
                    ui.selectable_value(&mut choice, Fingering::Viterbi, "Viterbi")
                        .on_hover_text(tr(
                            "The old method: fewer stretches and position shifts.",
                            "O método antigo: menos esticadas e mudanças de posição.",
                        ));
                    if choice != settings.transcription_fingering {
                        settings.transcription_fingering = choice;
                        save_settings(settings);
                    }
                });
            }
            ui.horizontal(|ui| {
                let can = self.guitar && self.separate && self.separator.is_some();
                let mut tone = settings.transcription_tone;
                ui.add_enabled(can, egui::Checkbox::new(&mut tone, tr("Estimate the tone (experimental)", "Estimar o timbre (experimental)")))
                    .on_hover_text(tr(
                        "After the tab, searches the app's pedals for a tone close to each guitar in the \
                         recording and saves it as a preset for this song (shown under 💡 Tone on the tabs page). \
                         Needs instrument separation; takes ~30 s per guitar. It's a starting point: \
                         adjust by ear.",
                        "Depois da tab, procura nos pedais do app um timbre parecido com cada guitarra da \
                         gravação e salva como preset desta música (aparece em 💡 Timbre na página de tabs). \
                         Precisa da separação de instrumentos; leva ~30 s por guitarra. É um ponto de \
                         partida: ajuste de ouvido.",
                    ));
                if tone != settings.transcription_tone {
                    settings.transcription_tone = tone;
                    save_settings(settings);
                }
            });
            if settings.transcription_tone || self.calibration.is_busy() {
                self.calibration.ui(ui, settings);
            }
            ui.label(
                RichText::new(engines_summary(
                    i18n::language(),
                    self.separate.then_some(self.separator.as_ref()).flatten(),
                    &self.models,
                    settings.transcription_guitar_model,
                    settings.transcription_fingering,
                ))
                .small()
                .weak(),
            );
            if !self.models.complete() {
                ui.label(
                    RichText::new(tr(
                        "Guitar/beat models not installed: run scripts/install_transcription_models.sh",
                        "Modelos de guitarra/tempo não instalados: rode scripts/install_transcription_models.sh",
                    ))
                        .color(Color32::YELLOW),
                )
                .on_hover_text(tr(
                    "A model trained on guitar only (more accurate notes) and the Beat This! beat detector \
                     (tempo, bars and 3/4). Without them the transcription uses Basic Pitch and the simple detector.",
                    "Um modelo treinado só com guitarra (notas mais precisas) e o detector de tempos Beat This! \
                     (andamento, compassos e 3/4). Sem eles a transcrição usa o Basic Pitch e o detector simples.",
                ));
            }
            if let Some(dir) = repository_dir(settings) {
                ui.label(RichText::new(trf!("Saved in {}", "Salva em {}", dir.display())).small().weak());
            }
            if let Some(error) = &self.error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            ui.horizontal(|ui| {
                let label = match self.source {
                    Source::YouTube
                        if self
                            .selected
                            .and_then(|i| self.results.get(i))
                            .is_some_and(|v| self.existing.contains_key(&v.id)) =>
                    {
                        tr("Transcribe again", "Transcrever de novo")
                    }
                    Source::YouTube => tr("Download and transcribe", "Baixar e transcrever"),
                    Source::File => tr("Transcribe", "Transcrever"),
                };
                if ui.add_enabled(self.ready(), egui::Button::new(RichText::new(label).strong())).clicked() {
                    self.start(settings);
                }
                if ui.button(tr("Close", "Fechar")).clicked() {
                    self.close();
                }
            });
            ui.label(
                RichText::new(tr(
                    "First version: 4/4 time, no triplets or techniques (bends, slides). Review the tab: \
                     wrong notes and string choices that differ from the original are expected.",
                    "Primeira versão: compasso 4/4, sem tercinas nem técnicas (bends, slides). Revise a tab: \
                     notas erradas e escolhas de corda diferentes do original são esperadas.",
                ))
                .small()
                .weak(),
            );
        });
        finished
    }

    fn file_source_ui(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(tr("Audio:", "Áudio:"));
            match &self.audio_path {
                Some(p) => {
                    ui.monospace(
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    );
                }
                None => {
                    ui.label(RichText::new(tr("none", "nenhum")).weak());
                }
            }
            #[cfg(not(target_os = "android"))]
            if ui.button(tr("Choose...", "Escolher...")).clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter(tr("Audio", "Áudio"), AUDIO_EXTENSIONS)
                    .pick_file()
            {
                self.set_audio(path);
            }
            #[cfg(target_os = "android")]
            {
                ui.text_edit_singleline(&mut self.path_input);
                if ui.button(tr("Use path", "Usar caminho")).clicked() {
                    self.set_audio(PathBuf::from(self.path_input.trim()));
                }
            }
        });
    }

    fn youtube_source_ui(&mut self, ui: &mut Ui) {
        if self.ytdlp.is_none() {
            ui.label(
                RichText::new(tr(
                    "yt-dlp not installed: run scripts/install_ytdlp.sh and reopen this form.",
                    "yt-dlp não instalado: rode scripts/install_ytdlp.sh e reabra este formulário.",
                ))
                .color(Color32::YELLOW),
            );
            return;
        }
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(tr("artist and song, or paste a YouTube link", "artista e música, ou cole um link do YouTube"))
                    .desired_width(320.0),
            );
            if std::mem::take(&mut self.focus_query) {
                field.request_focus();
            }
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            // A pasted link needs no extra click.
            let pasted_link = field.changed() && youtube::parse_video_id(&self.query).is_some();
            let new_search = self.searching.as_ref().is_some_and(|s| !s.append);
            if new_search {
                ui.spinner();
                ui.label(RichText::new(tr("searching YouTube...", "buscando no YouTube...")).weak());
            } else if ui.button(tr("🔎 Search", "🔎 Buscar")).clicked() || enter || pasted_link {
                self.start_search(false);
            }
        });
        let mut action = None;
        egui::ScrollArea::vertical()
            .id_salt("yt_results")
            .max_height(300.0)
            .show(ui, |ui| {
                for i in 0..self.results.len() {
                    if let Some(a) = self.result_row(ui, i) {
                        action = Some(a);
                    }
                }
                if self.more_results || self.searching.as_ref().is_some_and(|s| s.append) {
                    ui.horizontal(|ui| {
                        if self.searching.is_some() {
                            ui.spinner();
                        } else if ui.button(tr("More results", "Mais resultados")).clicked() {
                            action = Some(RowAction::More);
                        }
                    });
                }
            });
        match action {
            Some(RowAction::Select(i)) => self.select(i),
            Some(RowAction::Preview(i)) => self.toggle_preview(i),
            Some(RowAction::Seek(secs)) => self.player.seek_preview(secs),
            Some(RowAction::Open(path)) => {
                self.open_request = Some(path);
                self.close();
            }
            Some(RowAction::More) => self.start_search(true),
            None => {}
        }
        if !self.results.is_empty() {
            ui.label(
                RichText::new(tr(
                    "▶ plays the audio here to check the version before transcribing. YouTube's terms only \
                     allow downloading with permission from YouTube and the rights holders: use it for personal \
                     study. The audio stays in the app's cache.",
                    "▶ toca o áudio aqui para conferir a versão antes de transcrever. Os termos do YouTube só \
                     permitem baixar com autorização dele e dos donos dos direitos: use para estudo pessoal. \
                     O áudio fica só no cache do app.",
                ))
                .small()
                .weak(),
            );
        }
    }

    /// One search result: thumbnail, title, channel and details, a play
    /// button, and the preview's position while it plays.
    fn result_row(&self, ui: &mut Ui, i: usize) -> Option<RowAction> {
        let video = &self.results[i];
        let selected = self.selected == Some(i);
        let existing = self.existing.get(&video.id);
        let loading = self.preview_load.as_ref().filter(|l| l.id == video.id);
        let loaded = self.preview.as_ref().filter(|p| p.id == video.id);
        let mut action = None;
        let row = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
            ui.horizontal(|ui| {
                remote_image(ui, &video.thumbnail_url(), Vec2::new(80.0, 45.0));
                ui.vertical(|ui| {
                    let title = RichText::new(&video.title);
                    ui.label(if selected { title.strong() } else { title });
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let channel = if video.is_verified() {
                            format!("{} ✔", video.channel_name())
                        } else {
                            video.channel_name().to_string()
                        };
                        ui.label(RichText::new(channel).small().weak());
                        if let Some(d) = video.duration {
                            ui.label(RichText::new("·").small().weak());
                            match self.duration_hint.filter(|h| (d - h).abs() > DURATION_WARNING_SECS) {
                                Some(hint) => {
                                    ui.label(RichText::new(format_secs(d)).small().color(Color32::YELLOW))
                                        .on_hover_text(trf!(
                                            "The song on Spotify is {} long: this may be live or have an intro.",
                                            "A música no Spotify tem {}: pode ser ao vivo ou ter introdução.",
                                            format_secs(hint)
                                        ));
                                }
                                None => {
                                    ui.label(RichText::new(format_secs(d)).small().weak());
                                }
                            }
                        }
                        if let Some(views) = video.view_count {
                            ui.label(RichText::new(format!("· {}", format_views(views))).small().weak());
                        }
                        if video.is_official_audio() {
                            ui.label(
                                RichText::new(tr("official audio", "áudio oficial")).small().color(Color32::from_rgb(110, 190, 120)),
                            )
                            .on_hover_text(tr(
                                "The studio recording uploaded by the label: the best to transcribe.",
                                "A gravação de estúdio enviada pela gravadora: a melhor para transcrever.",
                            ));
                        }
                        if let Some(path) = existing {
                            ui.label(RichText::new(tr("✔ has a tab", "✔ já tem tab")).small().color(Color32::from_rgb(110, 170, 230)));
                            if ui.small_button(tr("Open tab", "Abrir tab")).on_hover_text(path.display().to_string()).clicked() {
                                action = Some(RowAction::Open(path.clone()));
                            }
                        }
                    });
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(load) = loading {
                        ui.label(RichText::new(format!("{:.0}%", load.progress * 100.0)).small().weak());
                        ui.spinner();
                    } else {
                        let playing = loaded.is_some() && self.player.status().previewing();
                        let (icon, tip) = if playing {
                            ("⏸", tr("Pause", "Pausar"))
                        } else if loaded.is_some() {
                            ("▶", tr("Resume", "Continuar"))
                        } else {
                            ("▶", tr("Listen here (downloads the audio, ~3 s)", "Ouvir aqui (baixa o áudio, ~3 s)"))
                        };
                        if ui.button(RichText::new(icon).size(16.0)).on_hover_text(tip).clicked() {
                            action = Some(RowAction::Preview(i));
                        }
                    }
                });
            });
            if let Some(loaded) = loaded {
                ui.horizontal(|ui| {
                    ui.add_space(86.0);
                    let mut pos = self.player.status().preview_secs().min(loaded.duration);
                    ui.spacing_mut().slider_width = 320.0;
                    let slider = ui.add(egui::Slider::new(&mut pos, 0.0..=loaded.duration).show_value(false));
                    if slider.changed() {
                        action = Some(RowAction::Seek(pos));
                    }
                    ui.label(
                        RichText::new(format!("{} / {}", format_secs(pos), format_secs(loaded.duration)))
                            .small()
                            .monospace(),
                    );
                });
            }
        });
        if selected {
            ui.painter().rect_stroke(
                row.response.rect.expand(2.0),
                3.0,
                (1.0, ui.visuals().selection.stroke.color),
                egui::StrokeKind::Outside,
            );
        }
        if action.is_none() && row.response.clicked() {
            action = Some(RowAction::Select(i));
        }
        if row.response.double_clicked() {
            action = Some(RowAction::Preview(i));
        }
        ui.add_space(4.0);
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_panel() -> TranscribePanel {
        TranscribePanel::new(SongPlayer::new())
    }

    #[test]
    fn form_renders_and_prefills_from_spotify_then_file_name() {
        let mut panel = new_panel();
        panel.ytdlp = Some(PathBuf::from("/x/yt-dlp"));
        panel.open = true;
        assert!(panel.prefill(
            Some("One".into()),
            Some("Metallica".into()),
            Some("t1".into()),
            Some(446.0)
        ));
        assert_eq!(panel.query, "Metallica One");
        assert_eq!(panel.source, Source::YouTube);
        panel.source = Source::File;
        panel.set_audio(PathBuf::from("/m/01 - Other - Name.mp3"));
        assert_eq!(
            (panel.artist.as_str(), panel.title.as_str()),
            ("Metallica", "One")
        );
        let mut blank = new_panel();
        blank.set_audio(PathBuf::from("/m/Band - Song.flac"));
        assert_eq!(
            (blank.artist.as_str(), blank.title.as_str()),
            ("Band", "Song")
        );

        let ctx = egui::Context::default();
        for source in [Source::File, Source::YouTube] {
            panel.source = source;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    assert!(panel.ui(ui, &mut AppSettings::default()).is_none());
                });
            });
        }
    }

    #[test]
    fn choosing_a_video_fills_names_unless_spotify_did() {
        let video = Video {
            id: "M4uG1MmZVl0".into(),
            title: "Tempo Perdido".into(),
            channel: Some("Legião Urbana - Topic".into()),
            duration: Some(303.0),
            ..Default::default()
        };
        let mut panel = new_panel();
        panel.results = vec![video.clone()];
        panel.select(0);
        assert_eq!(
            (panel.artist.as_str(), panel.title.as_str()),
            ("Legião Urbana", "Tempo Perdido")
        );
        panel.ytdlp = Some(PathBuf::from("/x/yt-dlp"));
        panel.source = Source::YouTube;
        assert!(panel.ready());

        let mut from_spotify = new_panel();
        from_spotify.spotify_track_id = Some("t".into());
        from_spotify.title = "Tempo Perdido - Remastered".into();
        from_spotify.artist = "Legião Urbana".into();
        from_spotify.results = vec![video];
        from_spotify.select(0);
        assert_eq!(from_spotify.title, "Tempo Perdido - Remastered");
    }

    fn yt(id: &str, title: &str) -> Video {
        Video {
            id: id.into(),
            title: title.into(),
            channel: Some("Metallica".into()),
            duration: Some(447.0),
            ..Default::default()
        }
    }

    fn render(panel: &mut TranscribePanel) {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(panel.ui(ui, &mut AppSettings::default()).is_none());
            });
        });
    }

    #[test]
    fn views_are_written_like_youtube_does() {
        let pt = |views| views_text(views, Language::Portuguese);
        assert_eq!(pt(1), "1 visualização");
        assert_eq!(pt(532), "532 visualizações");
        assert_eq!(pt(12_400), "12 mil visualizações");
        assert_eq!(pt(1_619_070), "1,6 mi de visualizações");
        assert_eq!(pt(2_000_000), "2 mi de visualizações");
        assert_eq!(pt(394_547_030), "394 mi de visualizações");
        assert_eq!(pt(1_290_000_000), "1,2 bi de visualizações");
        let en = |views| views_text(views, Language::English);
        assert_eq!(en(1), "1 view");
        assert_eq!(en(532), "532 views");
        assert_eq!(en(1_500), "1.5K views");
        assert_eq!(en(12_400), "12K views");
        assert_eq!(en(1_619_070), "1.6M views");
        assert_eq!(en(2_000_000), "2M views");
        assert_eq!(en(394_547_030), "394M views");
        assert_eq!(en(1_290_000_000), "1.2B views");
        assert_eq!(format_views(12_400), "12K views", "tests run in English");
    }

    #[test]
    fn more_results_are_appended_without_duplicates() {
        let mut panel = new_panel();
        panel.results_query = "metallica one".into();
        panel.fetched = SEARCH_RESULTS;
        let existing = HashMap::from([("bbbbbbbbbbb".to_string(), PathBuf::from("/r/One.gp5"))]);
        panel.add_results(
            SearchPage { videos: vec![yt("aaaaaaaaaaa", "One"), yt("bbbbbbbbbbb", "One (Live)")], existing },
            false,
        );
        assert_eq!(panel.selected, Some(0));
        assert!(panel.more_results);
        panel.select(1);
        panel.fetched += SEARCH_RESULTS;
        panel.add_results(
            SearchPage {
                videos: vec![yt("bbbbbbbbbbb", "One (Live)"), yt("ccccccccccc", "One (Demo)")],
                existing: panel.existing.clone(),
            },
            true,
        );
        let ids: Vec<&str> = panel.results.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"]);
        assert_eq!(panel.selected, Some(1), "loading more keeps the choice");
        // A page with nothing new ends the list.
        panel.add_results(SearchPage { videos: vec![yt("ccccccccccc", "One (Demo)")], existing: HashMap::new() }, true);
        assert!(!panel.more_results);
        assert!(panel.error.is_some());
        // A pasted link has no further pages.
        let mut link = new_panel();
        link.results_query = "https://youtu.be/aaaaaaaaaaa".into();
        link.add_results(SearchPage { videos: vec![yt("aaaaaaaaaaa", "One")], existing: HashMap::new() }, false);
        assert!(!link.more_results);
    }

    #[test]
    fn results_with_a_preview_and_an_existing_tab_render() {
        let mut panel = new_panel();
        panel.open = true;
        panel.source = Source::YouTube;
        panel.ytdlp = Some(PathBuf::from("/x/yt-dlp"));
        panel.duration_hint = Some(300.0);
        let mut official = yt("aaaaaaaaaaa", "One (Remastered)");
        official.description = Some("Provided to YouTube by Blackened Recordings".into());
        official.view_count = Some(34_000_000);
        official.channel_is_verified = Some(true);
        panel.results = vec![official, yt("bbbbbbbbbbb", "One (Live)")];
        panel.existing.insert("bbbbbbbbbbb".into(), PathBuf::from("/r/One.gp5"));
        panel.more_results = true;
        panel.select(0);
        let clip = Arc::new(DecodedAudio { samples: vec![0.0; 4800], sample_rate: 48_000 });
        panel.player.start_preview(clip, 0.0);
        panel.preview = Some(LoadedPreview { id: "aaaaaaaaaaa".into(), duration: 0.1 });
        render(&mut panel);
        // The play button on the loaded row pauses and resumes it.
        panel.toggle_preview(0);
        assert!(!panel.player.status().previewing());
        panel.toggle_preview(0);
        assert!(panel.player.status().previewing());
        // Choosing the video that already has a tab offers to redo it.
        panel.select(1);
        render(&mut panel);
        // Closing stops the preview.
        panel.close();
        assert!(!panel.player.status().previewing());
        assert!(panel.preview.is_none());
    }

    #[test]
    fn a_new_search_ranks_against_the_query_unless_it_names_the_song() {
        let mut panel = new_panel();
        // A previous choice filled the names; a different query must not be
        // ranked against them.
        panel.title = "Tempo Perdido".into();
        panel.artist = "Legião Urbana".into();
        panel.query = "metallica one".into();
        assert_eq!(panel.ranking_names("metallica one"), ("metallica one".into(), String::new()));
        // Spotify's names (with a decorated title) are used for their own search.
        panel.title = "One - Remastered".into();
        panel.artist = "Metallica".into();
        assert_eq!(panel.ranking_names("Metallica One"), ("One - Remastered".into(), "Metallica".into()));
        panel.ytdlp = Some(PathBuf::from("/nonexistent/yt-dlp"));
        panel.start_search(false);
        assert!(panel.searching.is_some());
        assert_eq!(panel.results_query, "metallica one");
        assert_eq!(panel.fetched, SEARCH_RESULTS);
        // Only one search at a time.
        panel.start_search(true);
        assert_eq!(panel.fetched, SEARCH_RESULTS);
    }

    #[test]
    fn engines_summary_names_the_separator_and_models() {
        let none = InstalledModels::default();
        assert_eq!(
            engines_summary(Language::English, None, &none, GuitarModel::Electric, Fingering::Learned),
            "Separation: none · notes: Basic Pitch · beats: simple detector · fingering: Viterbi"
        );
        assert_eq!(
            engines_summary(Language::Portuguese, None, &none, GuitarModel::Electric, Fingering::Learned),
            "Separação: nenhuma · notas: Basic Pitch · tempos: detector simples · dedilhado: Viterbi"
        );
        let all = InstalledModels {
            guitar_crnn: true,
            electric_crnn: true,
            beat_tracker: Some(transcribe::beat_net::FINAL_ENGINE_NAME),
            fingering_tagger: true,
        };
        let roformer = Separator::BsRoformer(separation::Roformer {
            python: "/p".into(),
            runner: "/r".into(),
            msst: "/m".into(),
            config: "/c".into(),
            checkpoint: "/k".into(),
        });
        assert_eq!(
            engines_summary(Language::English, Some(&roformer), &all, GuitarModel::Electric, Fingering::Learned),
            "Separation: BS-RoFormer · notes: electric CRNN (local fine-tune) · beats: Beat This! final0 \
             · fingering: learned"
        );
        assert_eq!(
            engines_summary(Language::Portuguese, Some(&roformer), &all, GuitarModel::Electric, Fingering::Learned),
            "Separação: BS-RoFormer · notas: CRNN elétrica (ajuste local) · tempos: Beat This! final0 \
             · dedilhado: aprendido"
        );
        let demucs = Separator::Demucs("/d".into());
        let small = InstalledModels {
            electric_crnn: false,
            beat_tracker: Some(transcribe::beat_net::SMALL_ENGINE_NAME),
            ..all
        };
        // The electric choice falls back to GAPS when that file is gone.
        assert_eq!(
            engines_summary(Language::English, Some(&demucs), &small, GuitarModel::Electric, Fingering::Viterbi),
            "Separation: Demucs · notes: CRNN GAPS · beats: Beat This! small0 · fingering: Viterbi"
        );
        assert_eq!(
            engines_summary(Language::Portuguese, Some(&demucs), &small, GuitarModel::Electric, Fingering::Viterbi),
            "Separação: Demucs · notas: CRNN GAPS · tempos: Beat This! small0 · dedilhado: Viterbi"
        );
    }

    #[test]
    fn model_choice_renders_when_the_electric_model_is_installed() {
        let mut panel = new_panel();
        panel.open = true;
        panel.models.electric_crnn = true;
        let mut settings = AppSettings {
            transcription_guitar_model: GuitarModel::Electric,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert!(panel.ui(ui, &mut settings).is_none());
            });
        });
        assert_eq!(settings.transcription_guitar_model, GuitarModel::Electric);
    }

    #[test]
    fn starting_without_audio_reports_an_error() {
        let mut panel = new_panel();
        panel.source = Source::File;
        panel.start(&AppSettings {
            tab_repository_dir: Some(PathBuf::from("/tmp/x")),
            ..Default::default()
        });
        assert!(panel.error.is_some());
        assert!(!panel.is_running());
        panel.source = Source::YouTube;
        panel.start(&AppSettings {
            tab_repository_dir: Some(PathBuf::from("/tmp/x")),
            ..Default::default()
        });
        assert!(!panel.is_running());
    }
}
