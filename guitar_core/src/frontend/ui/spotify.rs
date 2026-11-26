//! "Spotify" page: connect a Spotify account, browse its playlists, start
//! a track on the user's Spotify app, and jump to the matching tab from the
//! user's own tab folder.
//!
//! Spotify keeps playing on the user's device (Spotify Connect); this page
//! is a remote control. The tab player isn't synchronized to it: the
//! Developer Policy forbids synchronizing Spotify recordings with visual
//! media and overlapping them with other audio, and the player API carries
//! those policies. See ROADMAP.md "Spotify & play-along" for the options.
//!
//! Every request runs on a worker thread and reports back through a
//! channel polled once per frame. Nothing from Spotify is written to disk
//! except the sign-in tokens and the user's own track-to-tab links.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam::channel::{Receiver, Sender, unbounded};
use eframe::egui::{self, Color32, RichText, Ui, Vec2};

use crate::frontend::ui::tone3000::remote_image;
use crate::frontend::ui::transcribe::repository_dir;
use crate::shared::settings::{AppSettings, save_settings};
use crate::shared::tab::library::{LibraryIndex, clean_title, library_cache_path, read_tab_info, scan_library};
use crate::spotify::types::{User, pick_image};
use crate::spotify::{self, PlaybackState, Playlist, SpotifyClient, SpotifyError, Track};

const SPOTIFY_GREEN: Color32 = Color32::from_rgb(30, 215, 96);
/// Playback state refresh while the page is open.
const PLAYBACK_POLL: Duration = Duration::from_secs(5);
/// Shorter refresh right after a play/pause/skip command.
const AFTER_COMMAND_POLL: Duration = Duration::from_millis(800);
const ROW_HEIGHT: f32 = 40.0;

/// What the page asks the app to do.
pub enum SpotifyAction {
    OpenTab(PathBuf),
    /// Open the transcription form for this track (the user picks the audio).
    Transcribe { title: String, artist: String, track_id: String, duration_secs: Option<f64> },
}

fn transcribe_action(track: &Track) -> Option<SpotifyAction> {
    Some(SpotifyAction::Transcribe {
        title: clean_title(&track.name),
        artist: track.artist_names().first().cloned().unwrap_or_default(),
        track_id: track.id.clone()?,
        duration_secs: (track.duration_ms > 0).then(|| track.duration_ms as f64 / 1000.0),
    })
}

enum Job {
    SignedIn(Result<(), SpotifyError>),
    User(Result<User, SpotifyError>),
    Playlists(Result<Vec<Playlist>, SpotifyError>),
    Tracks(String, Result<Vec<Track>, SpotifyError>),
    Playback(Result<Option<Box<PlaybackState>>, SpotifyError>),
    Command(Result<(), SpotifyError>),
    Library(Vec<PathBuf>, LibraryIndex),
}

fn open_external(url: &str) {
    if let Err(e) = webbrowser::open(url) {
        log::warn!("could not open {url}: {e}");
    }
}

fn link(ui: &mut Ui, text: impl Into<egui::WidgetText>, url: &str) {
    if ui.link(text).on_hover_text(url).clicked() {
        open_external(url);
    }
}

fn spotify_mark(ui: &mut Ui) {
    ui.label(RichText::new("Spotify").strong().color(SPOTIFY_GREEN));
}

fn format_ms(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Web search for a tab of the song (the app doesn't fetch tabs itself).
fn tab_search_url(track: &Track) -> String {
    let query = format!(
        "{} {} guitar pro tab",
        track.artists_label(),
        clean_title(&track.name)
    );
    format!(
        "https://duckduckgo.com/?q={}",
        crate::tone3000::client::url_encode(&query)
    )
}

pub struct SpotifyPanel {
    client: Option<SpotifyClient>,
    tx: Sender<Job>,
    rx: Receiver<Job>,
    /// Authorize URL and cancel flag of a sign-in in progress.
    signing_in: Option<(String, Arc<AtomicBool>)>,
    user: Option<User>,
    playlists: Option<Vec<Playlist>>,
    loading_playlists: bool,
    selected_playlist: Option<String>,
    tracks: Option<(String, Vec<Track>)>,
    loading_tracks: bool,
    filter: String,
    playback: Option<PlaybackState>,
    /// When `playback` was received, to advance the progress bar locally.
    playback_at: Option<Instant>,
    next_poll: Option<Instant>,
    polling: bool,
    library: LibraryIndex,
    /// Folders the index was built from: the user's tab folder and the
    /// transcription repository.
    library_dirs: Vec<PathBuf>,
    scanning: bool,
    /// Library match per track ID (`None` = no match), cleared on rescan.
    match_cache: HashMap<String, Option<PathBuf>>,
    message: Option<(String, bool)>,
    started: bool,
}

impl Default for SpotifyPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl SpotifyPanel {
    pub fn new() -> Self {
        let client =
            spotify::client_id().map(|id| SpotifyClient::new(id, spotify::redirect_port()));
        let (tx, rx) = unbounded();
        Self {
            client,
            tx,
            rx,
            signing_in: None,
            user: None,
            playlists: None,
            loading_playlists: false,
            selected_playlist: None,
            tracks: None,
            loading_tracks: false,
            filter: String::new(),
            playback: None,
            playback_at: None,
            next_poll: None,
            polling: false,
            library: LibraryIndex::default(),
            library_dirs: Vec::new(),
            scanning: false,
            match_cache: HashMap::new(),
            message: None,
            started: false,
        }
    }

    fn spawn(&self, job: impl FnOnce(SpotifyClient) -> Job + Send + 'static) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job(client));
        });
    }

    fn set_message(&mut self, text: impl Into<String>, is_error: bool) {
        self.message = Some((text.into(), is_error));
    }

    fn handle_error(&mut self, error: SpotifyError) {
        if error == SpotifyError::NotSignedIn {
            // A request still in flight when the user disconnected.
            if !self.started {
                return;
            }
            self.forget_account();
            self.set_message("Sessão do Spotify expirada: conecte de novo.", true);
        } else if error != SpotifyError::Cancelled {
            self.set_message(error.to_string(), true);
        }
    }

    /// Drops everything loaded from the account (the policy requires it
    /// on disconnect; nothing of it was written to disk).
    fn forget_account(&mut self) {
        self.user = None;
        self.playlists = None;
        self.tracks = None;
        self.selected_playlist = None;
        self.playback = None;
        self.started = false;
    }

    fn start_sign_in(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        match client.begin_sign_in() {
            Ok(pending) => {
                let cancel = Arc::new(AtomicBool::new(false));
                self.signing_in = Some((pending.url.clone(), cancel.clone()));
                open_external(&pending.url);
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Job::SignedIn(pending.wait(&client, &cancel)));
                });
            }
            Err(e) => self.set_message(e.to_string(), true),
        }
    }

    fn load_account(&mut self) {
        self.started = true;
        self.loading_playlists = true;
        self.spawn(|c| Job::User(c.current_user()));
        self.spawn(|c| Job::Playlists(c.playlists()));
        self.request_playback(Duration::ZERO);
    }

    fn request_playback(&mut self, delay: Duration) {
        self.next_poll = Some(Instant::now() + delay);
    }

    fn poll_playback_if_due(&mut self) {
        let due = self.next_poll.is_some_and(|t| Instant::now() >= t);
        if !due || self.polling {
            return;
        }
        self.polling = true;
        self.next_poll = None;
        self.spawn(|c| Job::Playback(c.playback().map(|s| s.map(Box::new))));
    }

    fn command(
        &mut self,
        job: impl FnOnce(&SpotifyClient) -> Result<(), SpotifyError> + Send + 'static,
    ) {
        self.spawn(move |c| Job::Command(job(&c)));
    }

    fn load_tracks(&mut self, playlist_id: String) {
        self.selected_playlist = Some(playlist_id.clone());
        self.tracks = None;
        self.loading_tracks = true;
        self.spawn(move |c| {
            let result = c.playlist_tracks(&playlist_id);
            Job::Tracks(playlist_id, result)
        });
    }

    fn scan_library_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.scanning = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let files: Vec<PathBuf> = dirs.iter().flat_map(|d| scan_library(d)).collect();
            let info = read_tab_info(&files, library_cache_path().as_deref());
            let _ = tx.send(Job::Library(dirs, LibraryIndex::with_info(files, info)));
        });
    }

    fn process_jobs(&mut self) {
        while let Ok(job) = self.rx.try_recv() {
            match job {
                Job::SignedIn(result) => {
                    self.signing_in = None;
                    match result {
                        Ok(()) => {
                            self.set_message("Conectado ao Spotify.", false);
                            self.load_account();
                        }
                        Err(e) => self.handle_error(e),
                    }
                }
                Job::User(Ok(user)) => self.user = Some(user),
                Job::User(Err(e)) => self.handle_error(e),
                Job::Playlists(result) => {
                    self.loading_playlists = false;
                    match result {
                        Ok(list) => self.playlists = Some(list),
                        Err(e) => self.handle_error(e),
                    }
                }
                Job::Tracks(id, result) => {
                    if self.selected_playlist.as_deref() != Some(id.as_str()) {
                        continue;
                    }
                    self.loading_tracks = false;
                    match result {
                        Ok(tracks) => self.tracks = Some((id, tracks)),
                        Err(SpotifyError::Api { status: 403, .. }) => self.set_message(
                            "O Spotify só libera as faixas de playlists que você criou ou em que colabora.",
                            true,
                        ),
                        Err(e) => self.handle_error(e),
                    }
                }
                Job::Playback(result) => {
                    self.polling = false;
                    match result {
                        Ok(state) => {
                            self.playback = state.map(|s| *s);
                            self.playback_at = Some(Instant::now());
                        }
                        Err(e) => self.handle_error(e),
                    }
                    self.request_playback(PLAYBACK_POLL);
                }
                Job::Command(result) => {
                    if let Err(e) = result {
                        self.handle_error(e);
                    }
                    self.request_playback(AFTER_COMMAND_POLL);
                }
                Job::Library(dirs, index) => {
                    self.scanning = false;
                    if self.library_dirs == dirs {
                        self.library = index;
                        self.match_cache.clear();
                    }
                }
            }
        }
    }

    /// The tab for `track`: the user's manual link first, then the best
    /// library match by file name.
    fn tab_for(&mut self, track: &Track, settings: &AppSettings) -> Option<(PathBuf, bool)> {
        let id = track.id.as_deref()?;
        if let Some(path) = settings.spotify_tab_links.get(id) {
            return Some((path.clone(), true));
        }
        let library = &self.library;
        self.match_cache
            .entry(id.to_string())
            .or_insert_with(|| {
                library
                    .best_match(&track.name, &track.artist_names())
                    .cloned()
            })
            .clone()
            .map(|p| (p, false))
    }

    pub fn ui(&mut self, ui: &mut Ui, settings: &mut AppSettings) -> Option<SpotifyAction> {
        self.process_jobs();
        let mut action = None;

        ui.horizontal(|ui| {
            spotify_mark(ui);
            ui.label(
                RichText::new("Toque músicas das suas playlists e abra a tab correspondente.")
                    .weak(),
            );
        });
        if let Some((text, is_error)) = &self.message {
            let color = if *is_error {
                Color32::LIGHT_RED
            } else {
                ui.visuals().weak_text_color()
            };
            ui.colored_label(color, text);
        }

        let Some(client) = self.client.clone() else {
            not_configured_ui(ui);
            return None;
        };
        if !client.is_signed_in() {
            self.forget_account();
            self.signed_out_ui(ui);
            return None;
        }
        if !self.started {
            self.load_account();
        }
        let wanted: Vec<PathBuf> = settings.tab_library_dir.iter().cloned().chain(repository_dir(settings)).collect();
        if wanted != self.library_dirs {
            self.library_dirs = wanted.clone();
            self.library = LibraryIndex::default();
            self.match_cache.clear();
            self.scan_library_dirs(wanted);
        }
        self.poll_playback_if_due();

        self.account_row(ui, &client);
        self.library_row(ui, settings);
        ui.separator();
        if let Some(a) = self.now_playing(ui, settings) {
            action = Some(a);
        }
        ui.separator();

        let available = (ui.available_height() - 24.0).max(160.0);
        ui.horizontal_top(|ui| {
            let list_width = (ui.available_width() * 0.3).clamp(160.0, 320.0);
            ui.allocate_ui(egui::vec2(list_width, available), |ui| {
                ui.set_min_height(available);
                self.playlists_ui(ui);
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_min_height(available);
                if let Some(a) = self.tracks_ui(ui, settings, available) {
                    action = Some(a);
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Dados, capas e reprodução fornecidos pelo")
                    .small()
                    .weak(),
            );
            link(
                ui,
                RichText::new("Spotify").small().color(SPOTIFY_GREEN),
                "https://www.spotify.com",
            );
        });
        action
    }

    fn signed_out_ui(&mut self, ui: &mut Ui) {
        ui.add_space(8.0);
        if let Some((url, cancel)) = &self.signing_in {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Aguardando a autorização no navegador...");
            });
            let url = url.clone();
            ui.horizontal(|ui| {
                link(ui, "Abrir a página de login de novo", &url);
                if ui.button("Cancelar").clicked() {
                    cancel.store(true, Ordering::Relaxed);
                }
            });
            return;
        }
        if ui
            .button(RichText::new("Conectar com o Spotify").strong())
            .clicked()
        {
            self.start_sign_in();
        }
        ui.add_space(6.0);
        ui.label(RichText::new(
            "Abre o navegador para você autorizar o app. Permissões pedidas: ler suas playlists e \
             ver/controlar o que está tocando. Tocar exige Spotify Premium e o app do Spotify aberto \
             em algum aparelho (celular, computador, caixa de som).",
        ).weak());
    }

    fn account_row(&mut self, ui: &mut Ui, client: &SpotifyClient) {
        ui.horizontal_wrapped(|ui| {
            let name = self
                .user
                .as_ref()
                .map(|u| u.display_name.clone().unwrap_or_else(|| u.id.clone()))
                .unwrap_or_else(|| "...".into());
            ui.label(format!("Conectado como {name}"));
            if ui.button("🔄 Atualizar").clicked() {
                self.load_account();
                if let Some(id) = self.selected_playlist.clone() {
                    self.load_tracks(id);
                }
            }
            if ui
                .button("Desconectar")
                .on_hover_text("Apaga a sessão deste computador. Para revogar o acesso de vez: spotify.com/account/apps")
                .clicked()
            {
                client.sign_out();
                self.forget_account();
                self.set_message("Desconectado do Spotify.", false);
            }
        });
    }

    fn library_row(&mut self, ui: &mut Ui, settings: &mut AppSettings) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Pasta de tabs:");
            match &settings.tab_library_dir {
                Some(dir) => {
                    ui.monospace(dir.display().to_string());
                }
                None => {
                    ui.label(RichText::new("nenhuma (as tabs são procuradas pelo nome do arquivo, ex.: \"Artista - Música.gp5\")").weak());
                }
            }
            if self.scanning {
                ui.spinner();
            } else {
                ui.label(RichText::new(format!("{} tabs (pasta + transcritas pelo app)", self.library.len())).weak());
            }
            #[cfg(not(target_os = "android"))]
            if ui.button("📂 Escolher pasta...").clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                settings.tab_library_dir = Some(dir);
                save_settings(settings);
            }
            if !self.scanning && ui.button("Reescanear").on_hover_text("Inclui as tabs transcritas pelo app").clicked() {
                self.scan_library_dirs(self.library_dirs.clone());
            }
        });
    }

    fn now_playing(&mut self, ui: &mut Ui, settings: &AppSettings) -> Option<SpotifyAction> {
        let mut action = None;
        let Some(state) = self.playback.clone() else {
            ui.label(RichText::new("Nada tocando no Spotify agora.").weak());
            return None;
        };
        let Some(track) = state.item.clone().filter(|t| t.kind == "track") else {
            ui.label(
                RichText::new("Tocando algo que não é uma música (anúncio ou podcast).").weak(),
            );
            return None;
        };
        ui.horizontal(|ui| {
            if let Some(url) = pick_image(&track.album.images, 64) {
                remote_image(ui, url, Vec2::splat(64.0));
            }
            ui.vertical(|ui| {
                ui.label(RichText::new(&track.name).strong());
                ui.label(format!("{} · {}", track.artists_label(), track.album.name));
                ui.horizontal(|ui| {
                    if ui.button("⏮").on_hover_text("Anterior").clicked() {
                        self.command(|c| c.previous());
                    }
                    let playing = state.is_playing;
                    if ui.button(if playing { "⏸" } else { "▶" }).clicked() {
                        if playing {
                            self.command(|c| c.pause());
                        } else {
                            self.command(|c| c.resume());
                        }
                    }
                    if ui.button("⏭").on_hover_text("Próxima").clicked() {
                        self.command(|c| c.next());
                    }
                    let elapsed = if state.is_playing {
                        self.playback_at
                            .map(|t| t.elapsed().as_millis() as u64)
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    let progress =
                        (state.progress_ms.unwrap_or(0) + elapsed).min(track.duration_ms);
                    ui.monospace(format!(
                        "{} / {}",
                        format_ms(progress),
                        format_ms(track.duration_ms)
                    ));
                    if let Some(device) = &state.device {
                        ui.label(RichText::new(format!("em {}", device.name)).weak());
                    }
                    if let Some(url) = &track.external_urls.spotify {
                        link(ui, "Abrir no Spotify", url);
                    }
                });
            });
            ui.separator();
            if let Some((path, _)) = self.tab_for(&track, settings) {
                if ui
                    .button(RichText::new("🎼 Abrir tab desta música").strong())
                    .on_hover_text(path.display().to_string())
                    .clicked()
                {
                    action = Some(SpotifyAction::OpenTab(path));
                }
            } else {
                if ui
                    .button("⚡ Transcrever")
                    .on_hover_text("Gera a tab a partir de um arquivo de áudio desta música que você tenha")
                    .clicked()
                {
                    action = transcribe_action(&track);
                }
                link(ui, "🔎 Procurar tab na web", &tab_search_url(&track));
            }
        });
        action
    }

    fn playlists_ui(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Playlists").strong());
        if self.loading_playlists {
            ui.spinner();
            return;
        }
        let Some(playlists) = self.playlists.clone() else {
            return;
        };
        if playlists.is_empty() {
            ui.label(RichText::new("Nenhuma playlist.").weak());
            return;
        }
        let user_id = self.user.as_ref().map(|u| u.id.clone()).unwrap_or_default();
        egui::ScrollArea::vertical().id_salt("spotify_playlists").auto_shrink([false, false]).show(ui, |ui| {
            for playlist in &playlists {
                let readable = user_id.is_empty() || playlist.tracks_readable_by(&user_id);
                let selected = self.selected_playlist.as_deref() == Some(playlist.id.as_str());
                let count = playlist.track_count().map(|n| format!(" ({n})")).unwrap_or_default();
                let text = RichText::new(format!("{}{count}", playlist.name));
                let text = if readable { text } else { text.weak() };
                let response = ui.add_enabled(readable, egui::Button::selectable(selected, text));
                let response = if readable {
                    response
                } else {
                    response.on_disabled_hover_text(
                        "O Spotify só libera as faixas de playlists que você criou ou em que colabora.",
                    )
                };
                if response.clicked() {
                    self.load_tracks(playlist.id.clone());
                }
            }
        });
    }

    fn tracks_ui(
        &mut self,
        ui: &mut Ui,
        settings: &mut AppSettings,
        height: f32,
    ) -> Option<SpotifyAction> {
        let mut action = None;
        let Some(playlist_id) = self.selected_playlist.clone() else {
            ui.label(RichText::new("Escolha uma playlist.").weak());
            return None;
        };
        let playlist = self
            .playlists
            .as_ref()
            .and_then(|l| l.iter().find(|p| p.id == playlist_id))
            .cloned();
        ui.horizontal_wrapped(|ui| {
            if let Some(p) = &playlist {
                if let Some(url) = pick_image(&p.images, 40) {
                    remote_image(ui, url, Vec2::splat(28.0));
                }
                ui.label(RichText::new(&p.name).strong());
                if let Some(url) = &p.external_urls.spotify {
                    link(ui, "Abrir no Spotify", url);
                }
            }
            ui.separator();
            ui.label("Filtrar:");
            ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(160.0));
        });
        if self.loading_tracks {
            ui.spinner();
            return None;
        }
        let (_, tracks) = self.tracks.clone()?;
        let filter = self.filter.to_lowercase();
        let visible: Vec<&Track> = tracks
            .iter()
            .filter(|t| {
                filter.is_empty()
                    || t.name.to_lowercase().contains(&filter)
                    || t.artists_label().to_lowercase().contains(&filter)
            })
            .collect();
        if visible.is_empty() {
            ui.label(RichText::new("Nenhuma música.").weak());
            return None;
        }
        let context_uri = playlist.map(|p| p.uri);
        let playing_id = self
            .playback
            .as_ref()
            .and_then(|s| s.item.as_ref())
            .and_then(|t| t.id.clone());
        egui::ScrollArea::vertical()
            .id_salt("spotify_tracks")
            .auto_shrink([false, false])
            .max_height((height - 32.0).max(120.0))
            .show_rows(ui, ROW_HEIGHT, visible.len(), |ui, range| {
                for track in &visible[range] {
                    ui.horizontal(|ui| {
                        ui.set_height(ROW_HEIGHT - 4.0);
                        if ui.button("▶").on_hover_text("Tocar no Spotify").clicked() {
                            let context = context_uri.clone();
                            let uri = track.uri.clone();
                            self.command(move |c| c.play_track(context.as_deref(), &uri));
                        }
                        if let Some(url) = pick_image(&track.album.images, 40) {
                            remote_image(ui, url, Vec2::splat(32.0));
                        }
                        let is_playing = playing_id.is_some() && playing_id == track.id;
                        let title = RichText::new(&track.name);
                        let title = if is_playing {
                            title.strong().color(SPOTIFY_GREEN)
                        } else {
                            title.strong()
                        };
                        ui.vertical(|ui| {
                            ui.label(title);
                            ui.label(
                                RichText::new(format!(
                                    "{} · {}",
                                    track.artists_label(),
                                    format_ms(track.duration_ms)
                                ))
                                .small()
                                .weak(),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if let Some(a) = self.track_tab_controls(ui, track, settings) {
                                action = Some(a);
                            }
                        });
                    });
                }
            });
        action
    }

    /// Right-aligned tab controls of a track row (laid out right to left).
    fn track_tab_controls(
        &mut self,
        ui: &mut Ui,
        track: &Track,
        settings: &mut AppSettings,
    ) -> Option<SpotifyAction> {
        let mut action = None;
        if let Some(url) = &track.external_urls.spotify {
            link(ui, "↗", url);
        }
        let id = track.id.clone()?;
        let found = self.tab_for(track, settings);
        #[cfg(not(target_os = "android"))]
        {
            let label = if found.is_some() {
                "Trocar tab..."
            } else {
                "Associar tab..."
            };
            if ui
                .small_button(label)
                .on_hover_text("Escolher o arquivo Guitar Pro desta música")
                .clicked()
            {
                let mut dialog = rfd::FileDialog::new().add_filter(
                    "Guitar Pro",
                    crate::shared::tab::import::SUPPORTED_EXTENSIONS,
                );
                if let Some(dir) = &settings.tab_library_dir {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(path) = dialog.pick_file() {
                    settings.spotify_tab_links.insert(id.clone(), path);
                    save_settings(settings);
                }
            }
        }
        match found {
            Some((path, manual)) => {
                if manual
                    && ui
                        .small_button("✖")
                        .on_hover_text("Remover a associação manual")
                        .clicked()
                {
                    settings.spotify_tab_links.remove(&id);
                    save_settings(settings);
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if ui
                    .button("🎼 Abrir tab")
                    .on_hover_text(format!(
                        "{name}{}",
                        if manual {
                            " (associada por você)"
                        } else {
                            " (pelo nome do arquivo)"
                        }
                    ))
                    .clicked()
                {
                    action = Some(SpotifyAction::OpenTab(path));
                }
            }
            None => {
                link(ui, "🔎", &tab_search_url(track));
                if ui
                    .small_button("⚡ Transcrever")
                    .on_hover_text("Gera a tab a partir de um arquivo de áudio desta música que você tenha")
                    .clicked()
                {
                    action = transcribe_action(track);
                }
            }
        }
        action
    }
}

fn not_configured_ui(ui: &mut Ui) {
    ui.add_space(8.0);
    ui.label(
        RichText::new("A integração com o Spotify não está configurada nesta compilação.").strong(),
    );
    ui.label("Para ativar:");
    ui.label(format!(
        "1. Crie um app em {} (marque \"Web API\").",
        spotify::DASHBOARD_URL
    ));
    ui.label(format!(
        "2. Em Redirect URIs, adicione exatamente: {}",
        spotify::redirect_uri(spotify::redirect_port())
    ));
    ui.label("3. Copie o Client ID para a variável de ambiente SPOTIFY_CLIENT_ID (ou para [env] em .cargo/config.toml) e reinicie o app.");
    ui.label(RichText::new(
        "Não use o Client Secret: o app usa PKCE e não precisa dele. Apps em modo de desenvolvimento \
         exigem que o dono tenha Premium e aceitam até 5 usuários cadastrados no painel.",
    ).weak());
    link(
        ui,
        "Abrir o painel de desenvolvedor do Spotify",
        spotify::DASHBOARD_URL,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::types::Artist;

    fn render(panel: &mut SpotifyPanel, settings: &mut AppSettings) {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                panel.ui(ui, settings);
            });
        });
    }

    #[test]
    fn renders_without_a_client_id() {
        let mut panel = SpotifyPanel {
            client: None,
            ..SpotifyPanel::new()
        };
        render(&mut panel, &mut AppSettings::default());
    }

    #[test]
    fn renders_signed_out() {
        let client = SpotifyClient::with_session("id".into(), 0, None, None);
        let mut panel = SpotifyPanel {
            client: Some(client),
            ..SpotifyPanel::new()
        };
        render(&mut panel, &mut AppSettings::default());
        assert!(!panel.started);
    }

    #[test]
    fn manual_links_win_over_library_matches() {
        let mut panel = SpotifyPanel {
            client: None,
            ..SpotifyPanel::new()
        };
        panel.library = LibraryIndex::new(vec![PathBuf::from("/tabs/AC DC - Back In Black.gp5")]);
        let track = Track {
            id: Some("t1".into()),
            name: "Back In Black".into(),
            artists: vec![Artist {
                name: "AC/DC".into(),
            }],
            kind: "track".into(),
            ..Default::default()
        };
        let mut settings = AppSettings::default();
        assert_eq!(
            panel.tab_for(&track, &settings),
            Some((PathBuf::from("/tabs/AC DC - Back In Black.gp5"), false))
        );
        settings
            .spotify_tab_links
            .insert("t1".into(), PathBuf::from("/mine.gp"));
        assert_eq!(
            panel.tab_for(&track, &settings),
            Some((PathBuf::from("/mine.gp"), true))
        );
    }

    #[test]
    fn tab_search_url_is_encoded() {
        let track = Track {
            name: "Ainda É Cedo - Remastered".into(),
            artists: vec![Artist {
                name: "Legião Urbana".into(),
            }],
            ..Default::default()
        };
        let url = tab_search_url(&track);
        assert!(url.starts_with("https://duckduckgo.com/?q="));
        assert!(!url.contains(' '));
        assert!(!url.contains("Remastered"));
    }
}
