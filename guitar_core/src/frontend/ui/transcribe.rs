//! "Transcrever áudio" form shown on the Tabs page. The audio comes from a
//! local file or from YouTube (search or pasted link, downloaded with
//! yt-dlp); `crate::transcribe` then runs on a worker thread with a
//! progress bar. The result lands in the tab repository and is handed back
//! to the tab player to open.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam::channel::{Receiver, unbounded};
use eframe::egui::{self, Color32, Key, RichText, Ui, Vec2};

use crate::backend::audio_file::AUDIO_EXTENSIONS;
use crate::frontend::ui::tone3000::remote_image;
use crate::shared::settings::{AppSettings, save_settings};
use crate::shared::tab::repository::default_repository_dir;
use crate::transcribe::fingering_net::Fingering;
use crate::transcribe::guitar_crnn::GuitarModel;
use crate::transcribe::separation::{self, Separator};
use crate::transcribe::youtube::{self, Video};
use crate::transcribe::{self, InstalledModels, Output, Progress, Request};

const SEARCH_RESULTS: usize = 10;

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
    searching: Option<Receiver<Result<Vec<Video>, String>>>,
    results: Vec<Video>,
    selected: Option<usize>,
    job: Option<Job>,
    error: Option<String>,
    #[cfg(target_os = "android")]
    path_input: String,
}

impl Default for TranscribePanel {
    fn default() -> Self {
        Self::new()
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
    separator: Option<&Separator>,
    models: &InstalledModels,
    choice: GuitarModel,
    fingering: Fingering,
) -> String {
    let separation = separator.map_or("nenhuma", Separator::label);
    let notes = if choice == GuitarModel::Electric && models.electric_crnn {
        "CRNN elétrica (ajuste local)"
    } else if models.guitar_crnn {
        "CRNN GAPS"
    } else {
        "Basic Pitch"
    };
    let beats = match models.beat_tracker {
        Some(transcribe::beat_net::FINAL_ENGINE_NAME) => "Beat This! final0",
        Some(_) => "Beat This! small0",
        None => "detector simples",
    };
    let fingering = if fingering == Fingering::Learned && models.fingering_tagger {
        "aprendido"
    } else {
        "Viterbi"
    };
    format!("Separação: {separation} · notas: {notes} · tempos: {beats} · dedilhado: {fingering}")
}

fn format_secs(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

impl TranscribePanel {
    pub fn new() -> Self {
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
            searching: None,
            results: Vec::new(),
            selected: None,
            job: None,
            error: None,
            #[cfg(target_os = "android")]
            path_input: String::new(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.job.is_some()
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
        if self.prefill(title, artist, spotify_track_id, duration_hint) {
            self.start_search();
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

    fn start_search(&mut self) {
        let Some(ytdlp) = self.ytdlp.clone() else {
            return;
        };
        let query = self.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let (title, artist, hint) = (self.title.clone(), self.artist.clone(), self.duration_hint);
        let (tx, rx) = crossbeam::channel::bounded(1);
        std::thread::spawn(move || {
            let result = match youtube::parse_video_id(&query) {
                Some(id) => youtube::video_info(&ytdlp, &id).map(|v| vec![v]),
                None => youtube::search(&ytdlp, &query, SEARCH_RESULTS).map(|videos| {
                    // Rank against what we know about the song, else the query.
                    let (t, a) = if title.trim().is_empty() {
                        (query.clone(), String::new())
                    } else {
                        (title, artist)
                    };
                    youtube::rank(videos, &t, &a, hint)
                }),
            };
            let _ = tx.send(result);
        });
        self.results.clear();
        self.selected = None;
        self.error = None;
        self.searching = Some(rx);
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
        let Some(rx) = &self.searching else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.searching = None;
        match result {
            Ok(videos) if videos.is_empty() => {
                self.error = Some("Nada encontrado no YouTube.".into())
            }
            Ok(videos) => {
                self.results = videos;
                self.select(0);
            }
            Err(e) => self.error = Some(e),
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
        let Some(output_dir) = repository_dir(settings) else {
            self.error = Some("Sem pasta para o repositório de tabs.".into());
            return;
        };
        let video = match self.source {
            Source::YouTube => match (
                self.ytdlp.clone(),
                self.selected.and_then(|i| self.results.get(i)).cloned(),
            ) {
                (Some(ytdlp), Some(video)) => Some((ytdlp, video)),
                _ => {
                    self.error = Some("Escolha um vídeo.".into());
                    return;
                }
            },
            Source::File => None,
        };
        let audio_path = match (&video, &self.audio_path) {
            (Some(_), _) => PathBuf::new(),
            (None, Some(path)) => path.clone(),
            (None, None) => {
                self.error = Some("Escolha um arquivo de áudio.".into());
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
        self.poll_search();
        if let Some(job) = &self.job {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let label = job
                        .progress
                        .map(|p| p.stage.label())
                        .unwrap_or("Preparando");
                    ui.label(
                        RichText::new(format!("Transcrevendo \"{}\": {label}...", job.title))
                            .strong(),
                    );
                    if ui.button("Cancelar").clicked() {
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
                ui.label(RichText::new("🤖 Transcrever áudio").strong());
                ui.label(RichText::new("gera a tab a partir de uma gravação, tudo neste computador").weak());
            });
            ui.horizontal(|ui| {
                ui.label("Origem:");
                ui.selectable_value(&mut self.source, Source::YouTube, "▶ YouTube");
                ui.selectable_value(&mut self.source, Source::File, "📁 Arquivo");
            });
            match self.source {
                Source::File => self.file_source_ui(ui),
                Source::YouTube => self.youtube_source_ui(ui),
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Artista:");
                ui.add(egui::TextEdit::singleline(&mut self.artist).desired_width(180.0));
                ui.label("Música:");
                ui.add(egui::TextEdit::singleline(&mut self.title).desired_width(220.0));
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.guitar, "Guitarra");
                ui.checkbox(&mut self.bass, "Baixo");
                ui.separator();
                let label = match &self.separator {
                    Some(separator) => format!("Separar instrumentos ({})", separator.label()),
                    None => "Separar instrumentos".to_string(),
                };
                ui.add_enabled_ui(self.separator.is_some(), |ui| {
                    ui.checkbox(&mut self.separate, label)
                        .on_hover_text("Isola guitarra e baixo antes de transcrever. Melhor em gravações com banda.");
                });
                match &self.separator {
                    None => {
                        ui.label(
                            RichText::new("Separador não instalado: rode scripts/install_bs_roformer.sh (ou install_demucs.sh)")
                                .color(Color32::YELLOW),
                        )
                        .on_hover_text("Sem separação, guitarra e baixo são transcritos da mixagem inteira (pior com banda).");
                    }
                    Some(Separator::Demucs(_)) => {
                        ui.label(RichText::new("scripts/install_bs_roformer.sh separa melhor a guitarra").small().weak())
                            .on_hover_text(
                                "O BS-RoFormer SW (6 instrumentos) isola a guitarra melhor que o Demucs; \
                                 o app passa a usá-lo quando está instalado. Precisa de ~4 GB de memória na placa de vídeo.",
                            );
                    }
                    Some(Separator::BsRoformer(_)) => {}
                }
            });
            if self.models.electric_crnn {
                ui.horizontal(|ui| {
                    ui.label("Modelo de notas:");
                    let mut choice = settings.transcription_guitar_model;
                    ui.selectable_value(&mut choice, GuitarModel::Gaps, "GAPS (padrão)")
                        .on_hover_text("O modelo de guitarra do install_transcription_models.sh (Riley et al., violão clássico).");
                    ui.selectable_value(&mut choice, GuitarModel::Electric, "Elétrica (ajuste local)")
                        .on_hover_text(
                            "guitar_electric_crnn.onnx da pasta de modelos: o mesmo modelo ajustado em guitarra \
                             elétrica com amplificador. Treinado com dados sem licença: só para uso local.",
                        );
                    if choice != settings.transcription_guitar_model {
                        settings.transcription_guitar_model = choice;
                        save_settings(settings);
                    }
                });
            }
            if self.models.fingering_tagger {
                ui.horizontal(|ui| {
                    ui.label("Dedilhado:");
                    let mut choice = settings.transcription_fingering;
                    ui.selectable_value(&mut choice, Fingering::Learned, "Aprendido (padrão)")
                        .on_hover_text(
                            "fingering_tagger.onnx da pasta de modelos: escolhe a corda de cada nota como \
                             guitarristas tocam (mais acertos em solos e guitarra elétrica). Treinado com dados \
                             sem licença: só para uso local.",
                        );
                    ui.selectable_value(&mut choice, Fingering::Viterbi, "Viterbi")
                        .on_hover_text("O método antigo: menos esticadas e mudanças de posição.");
                    if choice != settings.transcription_fingering {
                        settings.transcription_fingering = choice;
                        save_settings(settings);
                    }
                });
            }
            ui.label(
                RichText::new(engines_summary(
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
                    RichText::new("Modelos de guitarra/tempo não instalados: rode scripts/install_transcription_models.sh")
                        .color(Color32::YELLOW),
                )
                .on_hover_text(
                    "Um modelo treinado só com guitarra (notas mais precisas) e o detector de tempos Beat This! \
                     (andamento, compassos e 3/4). Sem eles a transcrição usa o Basic Pitch e o detector simples.",
                );
            }
            if let Some(dir) = repository_dir(settings) {
                ui.label(RichText::new(format!("Salva em {}", dir.display())).small().weak());
            }
            if let Some(error) = &self.error {
                ui.colored_label(Color32::LIGHT_RED, error);
            }
            ui.horizontal(|ui| {
                let label = if self.source == Source::YouTube { "Baixar e transcrever" } else { "Transcrever" };
                if ui.add_enabled(self.ready(), egui::Button::new(RichText::new(label).strong())).clicked() {
                    self.start(settings);
                }
                if ui.button("Fechar").clicked() {
                    self.open = false;
                }
            });
            ui.label(
                RichText::new(
                    "Primeira versão: compasso 4/4, sem tercinas nem técnicas (bends, slides). Revise a tab: \
                     notas erradas e escolhas de corda diferentes do original são esperadas.",
                )
                .small()
                .weak(),
            );
        });
        finished
    }

    fn file_source_ui(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Áudio:");
            match &self.audio_path {
                Some(p) => {
                    ui.monospace(
                        p.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    );
                }
                None => {
                    ui.label(RichText::new("nenhum").weak());
                }
            }
            #[cfg(not(target_os = "android"))]
            if ui.button("Escolher...").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Áudio", AUDIO_EXTENSIONS)
                    .pick_file()
            {
                self.set_audio(path);
            }
            #[cfg(target_os = "android")]
            {
                ui.text_edit_singleline(&mut self.path_input);
                if ui.button("Usar caminho").clicked() {
                    self.set_audio(PathBuf::from(self.path_input.trim()));
                }
            }
        });
    }

    fn youtube_source_ui(&mut self, ui: &mut Ui) {
        if self.ytdlp.is_none() {
            ui.label(
                RichText::new(
                    "yt-dlp não instalado: rode scripts/install_ytdlp.sh e reabra este formulário.",
                )
                .color(Color32::YELLOW),
            );
            return;
        }
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("artista e música, ou cole um link do YouTube")
                    .desired_width(320.0),
            );
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if self.searching.is_some() {
                ui.spinner();
            } else if ui.button("🔎 Buscar").clicked() || enter {
                self.start_search();
            }
        });
        let mut clicked = None;
        egui::ScrollArea::vertical()
            .id_salt("yt_results")
            .max_height(220.0)
            .show(ui, |ui| {
                for (i, video) in self.results.iter().enumerate() {
                    let selected = self.selected == Some(i);
                    let row = ui.horizontal(|ui| {
                        remote_image(ui, &video.thumbnail_url(), Vec2::new(64.0, 36.0));
                        ui.vertical(|ui| {
                            let title = RichText::new(&video.title);
                            ui.label(if selected { title.strong() } else { title });
                            let duration = video.duration.map(format_secs).unwrap_or_default();
                            ui.label(
                                RichText::new(format!("{} · {duration}", video.channel_name()))
                                    .small()
                                    .weak(),
                            );
                        });
                    });
                    let response = ui.interact(
                        row.response.rect,
                        ui.id().with(("yt_row", i)),
                        egui::Sense::click(),
                    );
                    if selected {
                        ui.painter().rect_stroke(
                            row.response.rect.expand(2.0),
                            3.0,
                            (1.0, ui.visuals().selection.stroke.color),
                            egui::StrokeKind::Outside,
                        );
                    }
                    if response.clicked() {
                        clicked = Some(i);
                    }
                    if response.double_clicked()
                        && let Some(url) = self.results.get(i).map(Video::url)
                    {
                        let _ = webbrowser::open(&url);
                    }
                }
            });
        if let Some(i) = clicked {
            self.select(i);
        }
        if !self.results.is_empty() {
            ui.label(
                RichText::new(
                    "Clique para escolher, duplo clique abre no navegador. Os termos do YouTube só permitem baixar com \
                     autorização dele e dos donos dos direitos: use para estudo pessoal. O áudio fica só no cache do app.",
                )
                .small()
                .weak(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_renders_and_prefills_from_spotify_then_file_name() {
        let mut panel = TranscribePanel::new();
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
        let mut blank = TranscribePanel::new();
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
        let mut panel = TranscribePanel::new();
        panel.results = vec![video.clone()];
        panel.select(0);
        assert_eq!(
            (panel.artist.as_str(), panel.title.as_str()),
            ("Legião Urbana", "Tempo Perdido")
        );
        panel.ytdlp = Some(PathBuf::from("/x/yt-dlp"));
        panel.source = Source::YouTube;
        assert!(panel.ready());

        let mut from_spotify = TranscribePanel::new();
        from_spotify.spotify_track_id = Some("t".into());
        from_spotify.title = "Tempo Perdido - Remastered".into();
        from_spotify.artist = "Legião Urbana".into();
        from_spotify.results = vec![video];
        from_spotify.select(0);
        assert_eq!(from_spotify.title, "Tempo Perdido - Remastered");
    }

    #[test]
    fn engines_summary_names_the_separator_and_models() {
        let none = InstalledModels::default();
        assert_eq!(
            engines_summary(None, &none, GuitarModel::Electric, Fingering::Learned),
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
            engines_summary(Some(&roformer), &all, GuitarModel::Electric, Fingering::Learned),
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
            engines_summary(Some(&demucs), &small, GuitarModel::Electric, Fingering::Viterbi),
            "Separação: Demucs · notas: CRNN GAPS · tempos: Beat This! small0 · dedilhado: Viterbi"
        );
    }

    #[test]
    fn model_choice_renders_when_the_electric_model_is_installed() {
        let mut panel = TranscribePanel::new();
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
        let mut panel = TranscribePanel::new();
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
