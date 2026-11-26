//! TONE3000 browser window and the TONE3000 bits shown inside pedals.
//!
//! Layout follows the TONE3000 API design requirements
//! (https://www.tone3000.com/api, "Design Requirements"):
//! - The entry point is the pedal that will hold the tone (NAM Model or
//!   Cabinet), via its "Browse TONE3000" button.
//! - Users who aren't signed in see a partnership splash first, then sign in
//!   through the TONE3000 Select flow.
//! - After sign-in the first view is a tone list (Favorites / Created /
//!   Downloaded tabs plus Trending / Latest), with the signed-in user shown
//!   and a persistent "Browse TONE3000" button for the full catalog.
//! - Tone details show image, title, gear type, format, creator, a model
//!   selector and the description.
//! - Pedals holding a TONE3000 tone show its image, the T3K mark, title, gear
//!   type, format and creator.
//!
//! Logos: [`tone3000_logo`] and [`t3k_mark`] draw text placeholders. Before
//! shipping, replace them with the official assets from TONE3000's brand kit
//! (the full logo must be seen before the T3K mark).
//!
//! All network calls run on worker threads and report back over a channel;
//! nothing here blocks the UI thread on the network.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam::channel::{Receiver, Sender, unbounded};
use eframe::egui::{self, Color32, RichText, Ui, Vec2};

use crate::backend::pedals::nam;
use crate::shared::config::GLOBAL_CONFIG;
use crate::shared::i18n::tr;
use crate::shared::pedals::Tone3000Source;
use crate::tone3000::{
    self, BrowserFlowResult, DeviceCode, EmbeddedUser, Model, Prompt, Tone, Tone3000Client,
    Tone3000Error, ToneList, client::fetch_public_image, types::source_for,
};
use crate::trf;

const APP_NAME: &str = "Guitar Virtual Amp";
const T3K_ORANGE: Color32 = Color32::from_rgb(255, 92, 0);

/// Which kind of pedal a tone is for. Each maps to one TONE3000 format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToneTarget {
    /// NAM Model pedal, `format=nam`.
    Nam,
    /// Cabinet pedal, `format=ir`.
    Ir,
}

impl ToneTarget {
    fn format(self) -> &'static str {
        match self {
            ToneTarget::Nam => "nam",
            ToneTarget::Ir => "ir",
        }
    }

    fn pedal_name(self) -> &'static str {
        match self {
            ToneTarget::Nam => "NAM Model",
            ToneTarget::Ir => "Cabinet",
        }
    }
}

/// What a pedal asks the TONE3000 window to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Tone3000Request {
    /// Open the browser for this pedal.
    Browse(ToneTarget),
    /// The pedal references a TONE3000 tone whose file is missing (e.g. a
    /// preset from another machine): fetch it again via the Load Tone flow.
    Reload {
        target: ToneTarget,
        tone_id: u64,
        /// Model to preselect once the tone's models are listed.
        model_id: Option<u64>,
    },
}

/// A downloaded tone, ready to be put into the pedal at `pedal_index`.
#[derive(Clone, Debug)]
pub struct LoadedTone {
    pub pedal_index: usize,
    pub target: ToneTarget,
    pub path: String,
    pub source: Tone3000Source,
}

/// Opens `url` in the system browser, logging failures. Used for links so
/// they work the same on desktop and Android.
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

/// Full TONE3000 logo (text placeholder, see module docs).
pub fn tone3000_logo(ui: &mut Ui) {
    ui.label(
        RichText::new("TONE3000")
            .heading()
            .strong()
            .color(T3K_ORANGE),
    );
}

/// Compact T3K mark (text placeholder, see module docs).
pub fn t3k_mark(ui: &mut Ui) {
    ui.label(
        RichText::new(" T3K ")
            .small()
            .strong()
            .color(Color32::BLACK)
            .background_color(T3K_ORANGE),
    );
}

/// The "Browse TONE3000" button pedals show. Returns `true` when clicked.
pub fn browse_button(ui: &mut Ui) -> bool {
    ui.horizontal(|ui| {
        t3k_mark(ui);
        ui.button(tr("Browse TONE3000", "Explorar TONE3000")).clicked()
    })
    .inner
}

fn license_label(license: &str) -> String {
    match license {
        "t3k" => tr("T3K license", "Licença T3K").into(),
        "cco" | "cc0" => "CC0".into(),
        other => other.to_uppercase().replace("CC-", "CC "),
    }
}

/// Tone card shown inside a pedal that holds a TONE3000 tone.
pub fn source_block(ui: &mut Ui, source: &Tone3000Source) {
    ui.horizontal(|ui| {
        if let Some(url) = &source.image_url {
            remote_image(ui, url, Vec2::splat(44.0));
        }
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                t3k_mark(ui);
                ui.label(RichText::new(&source.tone_title).strong());
            });
            if !source.model_name.is_empty() && source.model_name != source.tone_title {
                ui.label(RichText::new(&source.model_name).small());
            }
            ui.label(
                RichText::new(trf!(
                    "{} · {} · by {}",
                    "{} · {} · por {}",
                    tone3000::types::gear_label(&source.gear),
                    tone3000::types::format_label(&source.format),
                    source.creator
                ))
                .small(),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new(license_label(&source.license)).small());
                if !source.tone_url.is_empty() {
                    link(
                        ui,
                        RichText::new(tr("View on TONE3000", "Ver no TONE3000")).small(),
                        &source.tone_url,
                    );
                }
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Remote images (tone pictures, avatars)
// ---------------------------------------------------------------------------

enum ImageEntry {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

struct DecodedImage {
    size: [usize; 2],
    rgba: Vec<u8>,
}

struct ImageCacheInner {
    entries: HashMap<String, ImageEntry>,
    tx: Sender<(String, Option<DecodedImage>)>,
    rx: Receiver<(String, Option<DecodedImage>)>,
}

/// Per-context image cache, stored in egui's memory so pedals and the
/// browser window share it without threading it through every call.
#[derive(Clone)]
struct ImageCache(Arc<Mutex<ImageCacheInner>>);

impl Default for ImageCache {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self(Arc::new(Mutex::new(ImageCacheInner {
            entries: HashMap::new(),
            tx,
            rx,
        })))
    }
}

fn decode_thumbnail(bytes: &[u8]) -> Option<DecodedImage> {
    let image = image::load_from_memory(bytes)
        .ok()?
        .thumbnail(256, 256)
        .to_rgba8();
    let (width, height) = image.dimensions();
    Some(DecodedImage {
        size: [width as usize, height as usize],
        rgba: image.into_raw(),
    })
}

/// Shows the image at `url` in a `size` box, fetching it in the background
/// the first time. A placeholder is drawn while loading or on failure.
pub fn remote_image(ui: &mut Ui, url: &str, size: Vec2) {
    let cache = ui.ctx().data_mut(|data| {
        data.get_temp_mut_or_default::<ImageCache>(egui::Id::new("t3k_images"))
            .clone()
    });
    let mut inner = cache.0.lock().unwrap();

    while let Ok((key, decoded)) = inner.rx.try_recv() {
        let entry = match decoded {
            Some(image) => {
                let color = egui::ColorImage::from_rgba_unmultiplied(image.size, &image.rgba);
                ImageEntry::Ready(
                    ui.ctx()
                        .load_texture(&key, color, egui::TextureOptions::LINEAR),
                )
            }
            None => ImageEntry::Failed,
        };
        inner.entries.insert(key, entry);
    }

    match inner.entries.get(url) {
        Some(ImageEntry::Ready(texture)) => {
            let sized = egui::load::SizedTexture::new(texture.id(), texture.size_vec2());
            ui.add(
                egui::Image::from_texture(sized)
                    .fit_to_exact_size(size)
                    .corner_radius(4.0),
            );
        }
        Some(ImageEntry::Loading) => {
            ui.allocate_ui(size, |ui| {
                ui.set_min_size(size);
                ui.centered_and_justified(|ui| ui.spinner());
            });
        }
        Some(ImageEntry::Failed) => {
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, Color32::from_gray(60));
        }
        None => {
            inner.entries.insert(url.to_string(), ImageEntry::Loading);
            let tx = inner.tx.clone();
            let key = url.to_string();
            std::thread::spawn(move || {
                let decoded = fetch_public_image(&key)
                    .ok()
                    .and_then(|bytes| decode_thumbnail(&bytes));
                let _ = tx.send((key, decoded));
            });
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, Color32::from_gray(60));
        }
    }
}

// ---------------------------------------------------------------------------
// Browser window
// ---------------------------------------------------------------------------

enum Job {
    BrowserDone(Result<BrowserFlowResult, Tone3000Error>),
    DeviceCode(Result<DeviceCode, Tone3000Error>),
    DeviceDone(Result<(), Tone3000Error>),
    User(Result<EmbeddedUser, Tone3000Error>),
    List(ToneList, Result<Vec<Tone>, Tone3000Error>),
    Tone(u64, Result<Tone, Tone3000Error>),
    Models(u64, Result<Vec<Model>, Tone3000Error>),
    Downloaded(Result<DownloadOutcome, Tone3000Error>),
}

struct DownloadOutcome {
    pedal_index: usize,
    target: ToneTarget,
    path: String,
    source: Tone3000Source,
    warning: Option<String>,
}

enum ListState {
    Loading,
    Loaded(Vec<Tone>),
    Failed(String),
}

struct Details {
    tone_id: u64,
    tone: Option<Tone>,
    models: Option<Result<Vec<Model>, String>>,
    selected: usize,
    downloading: bool,
    /// Result of the last download, shown under the button.
    outcome: Option<Result<String, String>>,
}

impl Details {
    fn new(tone_id: u64, tone: Option<Tone>) -> Self {
        Self {
            tone_id,
            tone,
            models: None,
            selected: 0,
            downloading: false,
            outcome: None,
        }
    }
}

enum View {
    Splash,
    WaitingBrowser { url: String },
    DeviceCode(Option<DeviceCode>),
    Library,
    Details(Box<Details>),
}

pub struct Tone3000Panel {
    client: Option<Tone3000Client>,
    open: bool,
    /// Pedal the next downloaded tone goes into, and which format it takes.
    target: Option<(usize, ToneTarget)>,
    view: View,
    tab: ToneList,
    user: Option<EmbeddedUser>,
    /// A `/user` request was sent for the current session. Kept separate
    /// from `user` so a failed request isn't retried every frame.
    user_requested: bool,
    lists: HashMap<ToneList, ListState>,
    message: Option<String>,
    /// Model to preselect in the details view (set by `Reload`).
    preferred_model: Option<u64>,
    cancel: Arc<AtomicBool>,
    tx: Sender<Job>,
    rx: Receiver<Job>,
}

impl Default for Tone3000Panel {
    fn default() -> Self {
        Self::new()
    }
}

impl Tone3000Panel {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        let client =
            tone3000::client_id().map(|id| Tone3000Client::new(id, tone3000::redirect_port()));
        Self {
            client,
            open: false,
            target: None,
            view: View::Splash,
            tab: ToneList::Favorited,
            user: None,
            user_requested: false,
            lists: HashMap::new(),
            message: None,
            preferred_model: None,
            cancel: Arc::new(AtomicBool::new(false)),
            tx,
            rx,
        }
    }

    /// Handles a request coming from the pedal at `pedal_index`.
    pub fn handle_request(&mut self, pedal_index: usize, request: Tone3000Request) {
        self.open = true;
        self.message = None;
        self.preferred_model = None;
        match request {
            Tone3000Request::Browse(target) => {
                if self.target.map(|(_, t)| t) != Some(target) {
                    // Lists are filtered by format, so a different pedal
                    // type needs them rebuilt.
                    self.lists.clear();
                }
                self.target = Some((pedal_index, target));
                let signed_in = self
                    .client
                    .as_ref()
                    .is_some_and(Tone3000Client::is_signed_in);
                match &self.view {
                    View::WaitingBrowser { .. } | View::DeviceCode(_) => {}
                    _ if signed_in => self.view = View::Library,
                    _ => self.view = View::Splash,
                }
            }
            Tone3000Request::Reload {
                target,
                tone_id,
                model_id,
            } => {
                self.target = Some((pedal_index, target));
                self.preferred_model = model_id;
                self.start_browser_flow(Prompt::LoadTone {
                    tone_id,
                    format: Some(target.format().into()),
                });
            }
        }
    }

    fn new_cancel(&mut self) -> Arc<AtomicBool> {
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.cancel.clone()
    }

    fn spawn<F>(&self, job: F)
    where
        F: FnOnce(Tone3000Client) -> Job + Send + 'static,
    {
        let Some(client) = self.client.clone() else {
            return;
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job(client));
        });
    }

    fn start_browser_flow(&mut self, prompt: Prompt) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let cancel = self.new_cancel();
        match client.begin_browser_flow(&prompt) {
            Ok(flow) => {
                let url = flow.url.clone();
                if let Err(e) = webbrowser::open(&url) {
                    self.message = Some(trf!(
                        "Could not open the browser ({e}). Open the link below yourself.",
                        "Não foi possível abrir o navegador ({e}). Abra o link abaixo manualmente."
                    ));
                }
                self.view = View::WaitingBrowser { url };
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Job::BrowserDone(flow.wait(&client, &cancel)));
                });
            }
            Err(e) => self.message = Some(e.to_string()),
        }
    }

    fn start_device_flow(&mut self) {
        self.new_cancel();
        self.view = View::DeviceCode(None);
        self.spawn(|client| Job::DeviceCode(client.request_device_code()));
    }

    fn after_sign_in(&mut self) {
        self.lists.clear();
        self.user = None;
        self.user_requested = true;
        self.spawn(|client| Job::User(client.user()));
    }

    fn forget_user(&mut self) {
        self.user = None;
        self.user_requested = false;
        self.lists.clear();
    }

    fn open_details(&mut self, tone_id: u64, tone: Option<Tone>) {
        match &tone {
            Some(tone) => {
                let tone = tone.clone();
                self.spawn(move |client| Job::Models(tone.id, client.models(&tone)));
            }
            None => self.spawn(move |client| Job::Tone(tone_id, client.tone(tone_id))),
        }
        self.view = View::Details(Box::new(Details::new(tone_id, tone)));
    }

    /// Any call can find out the session is gone; send the user back to the
    /// splash in that case instead of showing a raw error.
    fn handle_error(&mut self, error: Tone3000Error) {
        match error {
            Tone3000Error::Cancelled => {}
            Tone3000Error::NotSignedIn => {
                self.forget_user();
                self.view = View::Splash;
                self.message = Some(
                    tr(
                        "Your TONE3000 session ended. Sign in again to continue.",
                        "Sua sessão no TONE3000 terminou. Entre de novo para continuar.",
                    )
                    .into(),
                );
            }
            other => self.message = Some(other.to_string()),
        }
    }

    fn process_jobs(&mut self) -> Option<LoadedTone> {
        let mut loaded = None;
        while let Ok(job) = self.rx.try_recv() {
            match job {
                Job::BrowserDone(Ok(result)) => {
                    let signed_in = self
                        .client
                        .as_ref()
                        .is_some_and(Tone3000Client::is_signed_in);
                    if signed_in {
                        self.after_sign_in();
                    }
                    match result.tone_id {
                        Some(tone_id) if !result.canceled => self.open_details(tone_id, None),
                        _ if signed_in => self.view = View::Library,
                        _ => self.view = View::Splash,
                    }
                }
                Job::BrowserDone(Err(e)) => {
                    if matches!(self.view, View::WaitingBrowser { .. }) {
                        self.view = if self
                            .client
                            .as_ref()
                            .is_some_and(Tone3000Client::is_signed_in)
                        {
                            View::Library
                        } else {
                            View::Splash
                        };
                    }
                    self.handle_error(e);
                }
                Job::DeviceCode(Ok(code)) => {
                    if matches!(self.view, View::DeviceCode(None)) {
                        let cancel = self.cancel.clone();
                        let poll_code = code.clone();
                        self.spawn(move |client| {
                            Job::DeviceDone(client.poll_device_code(&poll_code, &cancel))
                        });
                        self.view = View::DeviceCode(Some(code));
                    }
                }
                Job::DeviceCode(Err(e)) => {
                    self.view = View::Splash;
                    self.handle_error(e);
                }
                Job::DeviceDone(Ok(())) => {
                    self.after_sign_in();
                    self.view = View::Library;
                }
                Job::DeviceDone(Err(e)) => {
                    if matches!(self.view, View::DeviceCode(_)) {
                        self.view = View::Splash;
                    }
                    self.handle_error(e);
                }
                Job::User(Ok(user)) => self.user = Some(user),
                Job::User(Err(e)) => self.handle_error(e),
                Job::List(list, result) => match result {
                    Ok(tones) => {
                        self.lists.insert(list, ListState::Loaded(tones));
                    }
                    Err(e) => {
                        self.lists.insert(list, ListState::Failed(e.to_string()));
                        if e == Tone3000Error::NotSignedIn {
                            self.handle_error(e);
                        }
                    }
                },
                Job::Tone(tone_id, result) => {
                    let matches_view =
                        matches!(&self.view, View::Details(d) if d.tone_id == tone_id);
                    match result {
                        Ok(tone) if matches_view => self.open_details(tone_id, Some(tone)),
                        Ok(_) => {}
                        Err(e) => self.handle_error(e),
                    }
                }
                Job::Models(tone_id, result) => {
                    if let View::Details(details) = &mut self.view
                        && details.tone_id == tone_id
                    {
                        details.models = Some(result.clone().map_err(|e| e.to_string()));
                        details.selected = result
                            .as_ref()
                            .ok()
                            .and_then(|models| {
                                let wanted = self.preferred_model?;
                                models.iter().position(|m| m.id == wanted)
                            })
                            .unwrap_or(0);
                    }
                    if let Err(e) = result
                        && e == Tone3000Error::NotSignedIn
                    {
                        self.handle_error(e);
                    }
                }
                Job::Downloaded(result) => {
                    if let View::Details(details) = &mut self.view {
                        details.downloading = false;
                        details.outcome = Some(match &result {
                            Ok(outcome) => Ok(match &outcome.warning {
                                Some(warning) => trf!("Loaded. {warning}", "Carregado. {warning}"),
                                None => tr("Loaded into the pedal.", "Carregado no pedal.").to_string(),
                            }),
                            Err(e) => Err(e.to_string()),
                        });
                    }
                    match result {
                        Ok(outcome) => {
                            loaded = Some(LoadedTone {
                                pedal_index: outcome.pedal_index,
                                target: outcome.target,
                                path: outcome.path,
                                source: outcome.source,
                            });
                        }
                        Err(e) => {
                            if e == Tone3000Error::NotSignedIn {
                                self.handle_error(e);
                            }
                        }
                    }
                }
            }
        }
        loaded
    }

    /// Draws the window (if open) and returns a tone the user just loaded.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<LoadedTone> {
        let loaded = self.process_jobs();
        if !self.open {
            return loaded;
        }

        let mut open = self.open;
        egui::Window::new("TONE3000")
            .open(&mut open)
            .default_width(460.0)
            .default_height(560.0)
            .collapsible(false)
            .show(ctx, |ui| self.ui(ui));
        if !open {
            self.close();
        }
        loaded
    }

    fn close(&mut self) {
        self.open = false;
        self.cancel.store(true, Ordering::Relaxed);
        if matches!(self.view, View::WaitingBrowser { .. } | View::DeviceCode(_)) {
            self.view = View::Splash;
        }
    }

    fn ui(&mut self, ui: &mut Ui) {
        if self.client.is_none() {
            not_configured_ui(ui);
            return;
        }
        if let Some(message) = self.message.clone() {
            ui.horizontal(|ui| {
                ui.colored_label(Color32::YELLOW, message);
                if ui.small_button("✖").clicked() {
                    self.message = None;
                }
            });
            ui.separator();
        }

        match &self.view {
            View::Splash => self.splash_ui(ui),
            View::WaitingBrowser { url } => {
                let url = url.clone();
                self.waiting_ui(ui, &url);
            }
            View::DeviceCode(code) => {
                let code = code.clone();
                self.device_code_ui(ui, code.as_ref());
            }
            View::Library => self.library_ui(ui),
            View::Details(_) => self.details_ui(ui),
        }
    }

    fn splash_ui(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            tone3000_logo(ui);
            ui.add_space(8.0);
            ui.label(trf!(
                "{APP_NAME} has partnered with TONE3000 to give you access to a massive library \
                 of Neural Amp Modeler (NAM) captures and IRs of real analog gear, created by a \
                 global community of musicians.",
                "{APP_NAME} fez uma parceria com o TONE3000 para dar a você acesso a uma enorme \
                 biblioteca de capturas do Neural Amp Modeler (NAM) e IRs de equipamentos \
                 analógicos reais, criadas por uma comunidade global de músicos."
            ));
            ui.add_space(12.0);
            if ui.button(RichText::new(tr("Continue", "Continuar")).strong()).clicked() {
                let format = self.target.map(|(_, t)| t.format().to_string());
                self.start_browser_flow(Prompt::SelectTone { format });
            }
            ui.add_space(6.0);
            if ui
                .link(tr(
                    "No browser on this device? Sign in with a code",
                    "Sem navegador neste dispositivo? Entre com um código",
                ))
                .clicked()
            {
                self.start_device_flow();
            }
            ui.add_space(12.0);
            link(ui, "tone3000.com", tone3000::WEBSITE_URL);
        });
    }

    fn waiting_ui(&mut self, ui: &mut Ui, url: &str) {
        ui.vertical_centered(|ui| {
            tone3000_logo(ui);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr(
                    "Finish in the browser window that just opened.",
                    "Termine na janela do navegador que acabou de abrir.",
                ));
            });
            ui.add_space(8.0);
            if ui.button(tr("Open the TONE3000 page again", "Abrir a página do TONE3000 de novo")).clicked() {
                open_external(url);
            }
            ui.add_space(4.0);
            if ui.button(tr("Cancel", "Cancelar")).clicked() {
                self.cancel.store(true, Ordering::Relaxed);
                self.view = if self
                    .client
                    .as_ref()
                    .is_some_and(Tone3000Client::is_signed_in)
                {
                    View::Library
                } else {
                    View::Splash
                };
            }
        });
    }

    fn device_code_ui(&mut self, ui: &mut Ui, code: Option<&DeviceCode>) {
        ui.vertical_centered(|ui| {
            tone3000_logo(ui);
            ui.add_space(8.0);
            match code {
                None => {
                    ui.spinner();
                }
                Some(code) => {
                    ui.label(tr("On your phone or computer, go to", "No celular ou computador, acesse"));
                    link(
                        ui,
                        RichText::new(&code.verification_uri).strong(),
                        &code.verification_uri,
                    );
                    ui.label(tr("and enter this code:", "e digite este código:"));
                    ui.label(
                        RichText::new(&code.user_code)
                            .monospace()
                            .size(28.0)
                            .strong(),
                    );
                    if let Some(complete) = &code.verification_uri_complete
                        && ui.button(tr("Open on this device", "Abrir neste dispositivo")).clicked()
                    {
                        open_external(complete);
                    }
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr("Waiting for approval...", "Aguardando aprovação..."));
                    });
                }
            }
            if ui.button(tr("Cancel", "Cancelar")).clicked() {
                self.cancel.store(true, Ordering::Relaxed);
                self.view = View::Splash;
            }
        });
    }

    fn header_ui(&mut self, ui: &mut Ui) {
        // A session restored from disk hasn't fetched the profile yet.
        if !self.user_requested {
            self.user_requested = true;
            self.spawn(|client| Job::User(client.user()));
        }
        ui.horizontal(|ui| {
            tone3000_logo(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(tr("Sign out", "Sair")).clicked() {
                    if let Some(client) = &self.client {
                        client.sign_out();
                    }
                    self.forget_user();
                    self.view = View::Splash;
                }
                if let Some(user) = &self.user {
                    ui.label(user.shown_name());
                    if let Some(avatar) = &user.avatar_url {
                        remote_image(ui, avatar, Vec2::splat(20.0));
                    }
                } else if self.user_requested {
                    ui.spinner();
                }
            });
        });
    }

    fn library_ui(&mut self, ui: &mut Ui) {
        self.header_ui(ui);
        let target = self.target.map(|(_, t)| t);
        if let Some(target) = target {
            ui.label(
                RichText::new(trf!(
                    "Showing {} tones for the {} pedal.",
                    "Mostrando timbres {} para o pedal {}.",
                    tone3000::types::format_label(target.format()),
                    target.pedal_name()
                ))
                .small(),
            );
        }
        if browse_button(ui) {
            let format = target.map(|t| t.format().to_string());
            self.start_browser_flow(Prompt::SelectTone { format });
        }
        ui.separator();

        ui.horizontal_wrapped(|ui| {
            for list in ToneList::ALL {
                ui.selectable_value(&mut self.tab, list, list.label());
            }
            if ui.small_button("⟳").on_hover_text(tr("Reload", "Recarregar")).clicked() {
                self.lists.remove(&self.tab);
            }
        });

        let tab = self.tab;
        if let std::collections::hash_map::Entry::Vacant(entry) = self.lists.entry(tab) {
            entry.insert(ListState::Loading);
            // Only Trending needs the filter: the user's own lists are
            // small and already filtered by format below.
            let gear =
                (tab == ToneList::Trending && target == Some(ToneTarget::Ir)).then_some("cab");
            self.spawn(move |client| Job::List(tab, client.list_tones(tab, gear)));
        }

        let mut clicked: Option<Tone> = None;
        match self.lists.get(&tab) {
            Some(ListState::Loading) | None => {
                ui.spinner();
            }
            Some(ListState::Failed(error)) => {
                ui.colored_label(Color32::RED, error);
            }
            Some(ListState::Loaded(tones)) => {
                let wanted = target.map(ToneTarget::format);
                let visible: Vec<&Tone> = tones
                    .iter()
                    .filter(|tone| wanted.is_none_or(|format| tone.format == format))
                    .collect();
                let hidden = tones.len() - visible.len();
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height() - 40.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        if visible.is_empty() {
                            ui.label(tr("No tones here yet.", "Nenhum timbre aqui ainda."));
                        }
                        for tone in visible {
                            if tone_card(ui, tone) {
                                clicked = Some(tone.clone());
                            }
                        }
                        if hidden > 0 {
                            ui.label(
                                RichText::new(trf!(
                                    "{hidden} tones in other formats are hidden.",
                                    "{hidden} timbres em outros formatos estão ocultos."
                                ))
                                .small()
                                .weak(),
                            );
                        }
                    });
            }
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label(tr("Looking for more?", "Quer mais?"));
            if ui.link(tr("Discover more on TONE3000", "Descubra mais no TONE3000")).clicked() {
                let format = target.map(|t| t.format().to_string());
                self.start_browser_flow(Prompt::SelectTone { format });
            }
        });

        if let Some(tone) = clicked {
            self.open_details(tone.id, Some(tone));
        }
    }

    fn details_ui(&mut self, ui: &mut Ui) {
        self.header_ui(ui);
        let mut back = false;
        let mut download: Option<(Tone, Model)> = None;
        let target = self.target;

        let View::Details(details) = &mut self.view else {
            return;
        };
        if ui.button(tr("⬅ Back", "⬅ Voltar")).clicked() {
            back = true;
        }

        match &details.tone {
            None => {
                ui.spinner();
            }
            Some(tone) => {
                ui.horizontal(|ui| {
                    if let Some(url) = tone.image_url() {
                        remote_image(ui, url, Vec2::splat(96.0));
                    }
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&tone.title).heading());
                        ui.label(format!("{} · {}", tone.gear_label(), tone.format_label()));
                        creator_line(ui, &tone.user);
                        ui.label(RichText::new(license_label(&tone.license)).small());
                        if !tone.url.is_empty() {
                            link(ui, tr("View on TONE3000", "Ver no TONE3000"), &tone.url);
                        }
                    });
                });
                ui.separator();

                let compatible = target.is_some_and(|(_, t)| t.format() == tone.format);
                match &details.models {
                    None => {
                        ui.spinner();
                    }
                    Some(Err(error)) => {
                        ui.colored_label(Color32::RED, error);
                    }
                    Some(Ok(models)) if models.is_empty() => {
                        ui.label(tr(
                            "This tone has no models this app can load.",
                            "Este timbre não tem modelos que o app consiga carregar.",
                        ));
                    }
                    Some(Ok(models)) => {
                        details.selected = details.selected.min(models.len() - 1);
                        egui::ComboBox::new("t3k_model", tr("Model", "Modelo"))
                            .selected_text(model_label(&models[details.selected]))
                            .width(260.0)
                            .show_ui(ui, |ui| {
                                for (i, model) in models.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut details.selected,
                                        i,
                                        model_label(model),
                                    );
                                }
                            });

                        if !compatible {
                            let pedal = match tone.format.as_str() {
                                "nam" => Some(ToneTarget::Nam),
                                "ir" => Some(ToneTarget::Ir),
                                _ => None,
                            };
                            ui.colored_label(
                                Color32::YELLOW,
                                match pedal {
                                    Some(pedal) => trf!(
                                        "This is a {} tone. Open TONE3000 from a {} pedal to load it.",
                                        "Este é um timbre {}. Abra o TONE3000 a partir de um pedal {} para carregá-lo.",
                                        tone.format_label(),
                                        pedal.pedal_name()
                                    ),
                                    None => trf!(
                                        "{} tones aren't supported.",
                                        "Timbres {} não são suportados.",
                                        tone.format_label()
                                    ),
                                },
                            );
                        }
                        ui.horizontal(|ui| {
                            let enabled = compatible && !details.downloading;
                            if ui
                                .add_enabled(enabled, egui::Button::new(tr("Load into pedal", "Carregar no pedal")))
                                .clicked()
                            {
                                details.downloading = true;
                                details.outcome = None;
                                download = Some((tone.clone(), models[details.selected].clone()));
                            }
                            if details.downloading {
                                ui.spinner();
                            }
                        });
                        match &details.outcome {
                            Some(Ok(message)) => {
                                ui.colored_label(Color32::LIGHT_GREEN, message);
                            }
                            Some(Err(error)) => {
                                ui.colored_label(Color32::RED, error);
                            }
                            None => {}
                        }
                    }
                }

                if let Some(description) = tone.description.as_deref().filter(|d| !d.is_empty()) {
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(ui.available_height() - 8.0)
                        .show(ui, |ui| {
                            ui.label(description);
                        });
                }
            }
        }

        if let (Some((tone, model)), Some((pedal_index, target))) = (download, target) {
            self.spawn(move |client| {
                Job::Downloaded(download_job(&client, pedal_index, target, &tone, &model))
            });
        }
        if back {
            self.view = View::Library;
        }
    }
}

fn model_label(model: &Model) -> String {
    match model.size.as_deref() {
        Some(size) if !size.is_empty() => format!("{} ({size})", model.name),
        _ => model.name.clone(),
    }
}

fn creator_line(ui: &mut Ui, user: &EmbeddedUser) {
    ui.horizontal(|ui| {
        if let Some(avatar) = &user.avatar_url {
            remote_image(ui, avatar, Vec2::splat(16.0));
        }
        let name = user.shown_name();
        if user.url.is_empty() {
            ui.label(name);
        } else {
            link(ui, name, &user.url);
        }
        if user.is_verified {
            ui.label(RichText::new("✔").color(T3K_ORANGE))
                .on_hover_text(tr("Verified creator", "Criador verificado"));
        }
    });
}

/// One row in a tone list: image, title, gear type, format and creator.
/// Returns `true` when clicked.
fn tone_card(ui: &mut Ui, tone: &Tone) -> bool {
    let response = ui
        .group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                match tone.image_url() {
                    Some(url) => remote_image(ui, url, Vec2::splat(56.0)),
                    None => {
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::splat(56.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 4.0, Color32::from_gray(60));
                    }
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(&tone.title).strong());
                    ui.label(
                        RichText::new(format!("{} · {}", tone.gear_label(), tone.format_label()))
                            .small(),
                    );
                    creator_line(ui, &tone.user);
                });
            });
        })
        .response
        .interact(egui::Sense::click());
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn not_configured_ui(ui: &mut Ui) {
    tone3000_logo(ui);
    ui.label(tr(
        "The TONE3000 integration isn't configured in this build.",
        "A integração com o TONE3000 não está configurada nesta versão.",
    ));
    ui.label(tr(
        "Create API keys in your TONE3000 account settings, then set the publishable key in \
         the TONE3000_CLIENT_ID environment variable (at build time or when starting the app).",
        "Crie chaves de API nos ajustes da sua conta TONE3000 e coloque a chave publicável na \
         variável de ambiente TONE3000_CLIENT_ID (na compilação ou ao iniciar o app).",
    ));
    link(
        ui,
        tr("TONE3000 API documentation", "Documentação da API do TONE3000"),
        "https://www.tone3000.com/api",
    );
    ui.separator();
    ui.label(tr(
        "You can still load .nam and .wav files you downloaded yourself from the pedal.",
        "Você ainda pode carregar no pedal arquivos .nam e .wav que você mesmo baixou.",
    ));
}

/// Runs on a worker thread: downloads the model, then checks it loads so
/// the user hears about problems here rather than getting a silent pedal.
fn download_job(
    client: &Tone3000Client,
    pedal_index: usize,
    target: ToneTarget,
    tone: &Tone,
    model: &Model,
) -> Result<DownloadOutcome, Tone3000Error> {
    let path = client.download_model(tone, model)?;
    let path_str = path.display().to_string();
    let warning = match target {
        ToneTarget::Nam => {
            nam::invalidate(&path_str);
            let info = nam::preload(&path_str).map_err(Tone3000Error::Other)?;
            nam_warning(&info)
        }
        ToneTarget::Ir => ir_warning(&path)?,
    };
    Ok(DownloadOutcome {
        pedal_index,
        target,
        path: path_str,
        source: source_for(tone, model),
        warning,
    })
}

/// Warnings shown for a loaded NAM capture (also used by the pedal UI).
pub fn nam_warning(info: &nam::NamInfo) -> Option<String> {
    let mut warnings = Vec::new();
    if info.sample_rate_mismatch() {
        warnings.push(trf!(
            "This capture was made at {:.0} Hz and the app runs at {:.0} Hz, so it won't sound right.",
            "Esta captura foi feita a {:.0} Hz e o app roda a {:.0} Hz, então não vai soar certo.",
            info.sample_rate, GLOBAL_CONFIG.sample_rate
        ));
    }
    if info.includes_cab == Some(true) {
        warnings.push(
            tr(
                "This capture already includes a cabinet; bypass the Cabinet pedal to avoid two in series.",
                "Esta captura já inclui um gabinete; deixe o pedal Cabinet em bypass para não ter dois em série.",
            )
            .to_string(),
        );
    }
    (!warnings.is_empty()).then(|| warnings.join(" "))
}

fn ir_warning(path: &Path) -> Result<Option<String>, Tone3000Error> {
    let reader = hound::WavReader::open(path)
        .map_err(|e| Tone3000Error::Other(trf!(
            "downloaded IR isn't a readable WAV: {e}",
            "o IR baixado não é um WAV legível: {e}"
        )))?;
    let rate = reader.spec().sample_rate as f32;
    Ok(((rate - GLOBAL_CONFIG.sample_rate).abs() > 1.0).then(|| {
        trf!(
            "This IR is {rate:.0} Hz and the app runs at {:.0} Hz, so it will sound shifted. \
             Pick a 48 kHz version if the tone has one.",
            "Este IR é de {rate:.0} Hz e o app roda a {:.0} Hz, então vai soar deslocado. \
             Escolha uma versão de 48 kHz se o timbre tiver uma.",
            GLOBAL_CONFIG.sample_rate
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::ui::pedals::{PedalAction, render_pedal_ui};
    use crate::shared::pedals::{CabinetParams, NamParams, PedalDescription, PedalInstance};

    fn fake_tone(id: u64, format: &str) -> Tone {
        Tone {
            id,
            title: format!("Tone {id}"),
            description: Some("A description".into()),
            gear: "amp-cab".into(),
            format: format.into(),
            license: "cc-by".into(),
            user: EmbeddedUser {
                username: "creator".into(),
                is_verified: true,
                ..Default::default()
            },
            url: "https://www.tone3000.com/tones/x".into(),
            ..Default::default()
        }
    }

    fn fake_source() -> Tone3000Source {
        source_for(
            &fake_tone(7, "nam"),
            &Model {
                id: 70,
                name: "Crunch".into(),
                ..Default::default()
            },
        )
    }

    /// A panel with a client that has no session and no session file, so
    /// nothing touches the network or the user's real data directory.
    fn offline_panel() -> Tone3000Panel {
        let mut panel = Tone3000Panel::new();
        panel.client = Some(Tone3000Client::with_session(
            "pk_test".into(),
            0,
            None,
            None,
        ));
        panel
    }

    fn render(panel: &mut Tone3000Panel) {
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                panel.show(ctx);
            });
        }
    }

    #[test]
    fn every_view_renders_without_panicking() {
        let mut unconfigured = Tone3000Panel::new();
        unconfigured.client = None;
        unconfigured.open = true;
        render(&mut unconfigured);

        let mut panel = offline_panel();
        panel.handle_request(0, Tone3000Request::Browse(ToneTarget::Nam));
        assert!(
            matches!(panel.view, View::Splash),
            "signed-out users see the splash first"
        );
        render(&mut panel);

        panel.view = View::WaitingBrowser {
            url: "https://www.tone3000.com".into(),
        };
        render(&mut panel);

        panel.view = View::DeviceCode(None);
        render(&mut panel);
        panel.view = View::DeviceCode(Some(DeviceCode {
            device_code: "d".into(),
            user_code: "ABCD-EFGH".into(),
            verification_uri: "https://www.tone3000.com/activate".into(),
            verification_uri_complete: None,
            expires_in: 600,
            interval: 5,
        }));
        render(&mut panel);

        panel.view = View::Library;
        panel.user = Some(EmbeddedUser {
            username: "me".into(),
            ..Default::default()
        });
        panel.user_requested = true;
        panel.lists.insert(
            ToneList::Favorited,
            ListState::Loaded(vec![fake_tone(1, "nam"), fake_tone(2, "ir")]),
        );
        render(&mut panel);

        let mut details = Details::new(1, Some(fake_tone(1, "nam")));
        details.models = Some(Ok(vec![
            Model {
                id: 10,
                name: "Clean".into(),
                size: Some("standard".into()),
                ..Default::default()
            },
            Model {
                id: 11,
                name: "Lead".into(),
                ..Default::default()
            },
        ]));
        details.selected = 5; // out of range on purpose: must be clamped
        details.outcome = Some(Ok("Loaded.".into()));
        panel.view = View::Details(Box::new(details));
        render(&mut panel);

        let mut empty = Details::new(2, Some(fake_tone(2, "ir")));
        empty.models = Some(Ok(Vec::new()));
        panel.view = View::Details(Box::new(empty));
        render(&mut panel);
    }

    #[test]
    fn signed_in_errors_return_to_splash() {
        let mut panel = offline_panel();
        panel.view = View::Library;
        panel.handle_error(Tone3000Error::NotSignedIn);
        assert!(matches!(panel.view, View::Splash));
        assert!(panel.message.is_some());
    }

    #[test]
    fn reload_request_preselects_the_saved_model() {
        let mut panel = offline_panel();
        panel.preferred_model = Some(11);
        panel.view = View::Details(Box::new(Details::new(1, Some(fake_tone(1, "nam")))));
        panel
            .tx
            .send(Job::Models(
                1,
                Ok(vec![
                    Model {
                        id: 10,
                        ..Default::default()
                    },
                    Model {
                        id: 11,
                        ..Default::default()
                    },
                ]),
            ))
            .unwrap();
        panel.process_jobs();
        let View::Details(details) = &panel.view else {
            panic!("still in details")
        };
        assert_eq!(details.selected, 1);
    }

    #[test]
    fn pedals_with_tone3000_sources_render() {
        let ctx = egui::Context::default();
        let mut pedals = vec![
            PedalInstance::new(PedalDescription::Nam(NamParams {
                model_path: Some("/nonexistent/x.nam".into()),
                source: Some(fake_source()),
                ..Default::default()
            })),
            PedalInstance::new(PedalDescription::Cabinet(CabinetParams {
                ir_path: Some("/nonexistent/x.wav".into()),
                source: Some(fake_source()),
                ..Default::default()
            })),
            PedalInstance::new(PedalDescription::Nam(NamParams::default())),
        ];
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for pedal in &mut pedals {
                    assert!(matches!(render_pedal_ui(ui, pedal), PedalAction::None));
                }
            });
        });
    }
}
