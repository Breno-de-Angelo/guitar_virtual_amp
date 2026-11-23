//! Blocking TONE3000 API client. Every method does network I/O, so call them
//! from a worker thread, never from the UI or audio thread.
//!
//! Sign-in uses OAuth 2.0 with PKCE and a loopback redirect
//! (`http://127.0.0.1:<port>/callback`): the app opens the system browser,
//! TONE3000 redirects back to a one-shot local HTTP listener, and the code is
//! exchanged for tokens. Only the publishable key (`client_id`) is used; the
//! secret key must never ship inside the app (API terms). For devices that
//! can't open a browser there's also the device-code flow.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use super::storage;
use super::types::{EmbeddedUser, Model, Page, Tone, ToneList};
use super::{API_BASE, Tone3000Error};

/// Largest model file accepted. Standard `.nam` captures are well under
/// 10 MB; this leaves room for A2 multi-size files and long IRs.
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
/// How long to wait for the user to finish in the browser.
const BROWSER_FLOW_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Refresh this long before the access token actually expires.
const EXPIRY_MARGIN_SECS: u64 = 60;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix time (seconds) at which `access_token` expires.
    pub expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct OAuthErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// What the TONE3000 web flow should do after sign-in.
#[derive(Clone, Debug, PartialEq)]
pub enum Prompt {
    /// Let the user browse the catalog and pick a tone. `format` locks the
    /// catalog to `nam` or `ir`.
    SelectTone { format: Option<String> },
    /// Load a known tone (e.g. one referenced by a preset). If it's gone,
    /// TONE3000 lets the user pick a replacement.
    LoadTone {
        tone_id: u64,
        format: Option<String>,
    },
}

/// Result of the browser flow once TONE3000 redirects back.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserFlowResult {
    /// Tone the user selected (or the resolved tone for `LoadTone`).
    pub tone_id: Option<u64>,
    /// User closed the TONE3000 window. They may still be signed in.
    pub canceled: bool,
}

/// Parameters received on the loopback callback.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub tone_id: Option<u64>,
    pub canceled: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_device_expiry")]
    pub expires_in: u64,
    #[serde(default = "default_device_interval")]
    pub interval: u64,
}

fn default_device_expiry() -> u64 {
    600
}

fn default_device_interval() -> u64 {
    5
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub(crate) fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(decoded) => {
                        out.push(decoded);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// PKCE verifier and its S256 challenge.
pub(crate) struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

fn random_token(bytes: usize) -> Result<String, Tone3000Error> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| Tone3000Error::Other(format!("no randomness: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

pub(crate) fn new_pkce() -> Result<Pkce, Tone3000Error> {
    // 32 random bytes give a 43-character verifier, the RFC 7636 minimum.
    let verifier = random_token(32)?;
    Ok(Pkce {
        challenge: pkce_challenge(&verifier),
        verifier,
    })
}

pub(crate) fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

pub(crate) fn authorize_url(
    client_id: &str,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
    prompt: &Prompt,
) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("client_id", client_id.to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("response_type", "code".to_string()),
        ("code_challenge", pkce.challenge.clone()),
        ("code_challenge_method", "S256".to_string()),
        ("state", state.to_string()),
        // Close button that redirects back with `canceled=true`, so the app
        // isn't left waiting when the user gives up.
        ("menubar", "true".to_string()),
    ];
    match prompt {
        Prompt::SelectTone { format } => {
            params.push(("prompt", "select_tone".to_string()));
            params.push(("preview", "true".to_string()));
            if let Some(format) = format {
                params.push(("format", format.clone()));
            }
        }
        Prompt::LoadTone { tone_id, format } => {
            params.push(("prompt", "load_tone".to_string()));
            params.push(("tone_id", tone_id.to_string()));
            if let Some(format) = format {
                params.push(("format", format.clone()));
            }
        }
    }
    let query: Vec<String> = params
        .iter()
        .map(|(key, value)| format!("{key}={}", url_encode(value)))
        .collect();
    format!("{API_BASE}/api/v1/oauth/authorize?{}", query.join("&"))
}

/// Parses the request line of the loopback callback (`GET /callback?... HTTP/1.1`).
/// Returns `None` for any other path (browsers also ask for `/favicon.ico`).
pub(crate) fn parse_callback(request_line: &str) -> Option<CallbackParams> {
    let target = request_line.split_whitespace().nth(1)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        return None;
    }
    let mut params = CallbackParams::default();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = url_decode(value);
        match key {
            "code" => params.code = Some(value),
            "state" => params.state = Some(value),
            "tone_id" => params.tone_id = value.parse().ok(),
            "canceled" => params.canceled = value == "true",
            "error" => params.error = Some(value),
            _ => {}
        }
    }
    Some(params)
}

/// `true` only for HTTPS URLs on tone3000.com or a subdomain. The access
/// token is sent to these and nowhere else.
pub(crate) fn is_tone3000_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = authority
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host == "tone3000.com" || host.ends_with(".tone3000.com")
}

/// A browser sign-in that has its loopback listener bound and its URL
/// built, waiting for the user. Created on the UI thread (cheap), then moved
/// to a worker thread for [`PendingBrowserFlow::wait`].
pub struct PendingBrowserFlow {
    listener: TcpListener,
    pkce: Pkce,
    state: String,
    redirect_uri: String,
    pub url: String,
}

impl PendingBrowserFlow {
    /// Blocks until TONE3000 redirects back, the user cancels (`cancel` set),
    /// or the flow times out. On success the session is stored on `client`.
    pub fn wait(
        self,
        client: &Tone3000Client,
        cancel: &AtomicBool,
    ) -> Result<BrowserFlowResult, Tone3000Error> {
        let params = wait_for_callback(&self.listener, cancel)?;
        if params.state.as_deref() != Some(self.state.as_str()) {
            return Err(Tone3000Error::OAuth(
                "sign-in response did not match this request (state mismatch)".into(),
            ));
        }
        if let Some(error) = params.error {
            return Err(Tone3000Error::OAuth(error));
        }
        if let Some(code) = params.code {
            client.exchange_code(&code, &self.pkce.verifier, &self.redirect_uri)?;
        } else if !params.canceled {
            return Err(Tone3000Error::OAuth(
                "no authorization code returned".into(),
            ));
        }
        Ok(BrowserFlowResult {
            tone_id: params.tone_id,
            canceled: params.canceled,
        })
    }
}

const CALLBACK_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>TONE3000</title></head>\
<body style=\"font-family:sans-serif;text-align:center;padding-top:15vh\">\
<h2>Done</h2><p>You can close this tab and go back to Guitar Virtual Amp.</p></body></html>";

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn wait_for_callback(
    listener: &TcpListener,
    cancel: &AtomicBool,
) -> Result<CallbackParams, Tone3000Error> {
    listener
        .set_nonblocking(true)
        .map_err(|e| Tone3000Error::Io(e.to_string()))?;
    let deadline = Instant::now() + BROWSER_FLOW_TIMEOUT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Tone3000Error::Cancelled);
        }
        if Instant::now() > deadline {
            return Err(Tone3000Error::OAuth(
                "timed out waiting for the browser".into(),
            ));
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut request_line = String::new();
                let mut reader = BufReader::new(&stream);
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                match parse_callback(&request_line) {
                    Some(params) => {
                        respond(&mut stream, "200 OK", CALLBACK_PAGE);
                        return Ok(params);
                    }
                    None => respond(&mut stream, "404 Not Found", ""),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(Tone3000Error::Io(e.to_string())),
        }
    }
}

struct Inner {
    agent: ureq::Agent,
    client_id: String,
    redirect_port: u16,
    session: Mutex<Option<Session>>,
    session_path: Option<PathBuf>,
}

/// Cheap to clone; clones share the session.
#[derive(Clone)]
pub struct Tone3000Client {
    inner: Arc<Inner>,
}

impl Tone3000Client {
    pub fn new(client_id: String, redirect_port: u16) -> Self {
        let session_path = storage::session_path();
        let session = session_path.as_deref().and_then(storage::load_session);
        Self::with_session(client_id, redirect_port, session, session_path)
    }

    pub(crate) fn with_session(
        client_id: String,
        redirect_port: u16,
        session: Option<Session>,
        session_path: Option<PathBuf>,
    ) -> Self {
        let config = ureq::Agent::config_builder()
            // OAuth errors come back as 400 with a JSON body we need to read.
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(concat!("guitar_virtual_amp/", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            inner: Arc::new(Inner {
                agent: ureq::Agent::new_with_config(config),
                client_id,
                redirect_port,
                session: Mutex::new(session),
                session_path,
            }),
        }
    }

    pub fn is_signed_in(&self) -> bool {
        self.inner.session.lock().unwrap().is_some()
    }

    pub fn sign_out(&self) {
        *self.inner.session.lock().unwrap() = None;
        if let Some(path) = &self.inner.session_path {
            let _ = std::fs::remove_file(path);
        }
    }

    fn store_session(&self, session: Session) {
        if let Some(path) = &self.inner.session_path {
            if let Err(e) = storage::save_session(path, &session) {
                log::warn!("could not save TONE3000 session: {e}");
            }
        }
        *self.inner.session.lock().unwrap() = Some(session);
    }

    fn session_from(&self, token: TokenResponse, previous_refresh: Option<&str>) -> Session {
        Session {
            access_token: token.access_token,
            refresh_token: token
                .refresh_token
                .or_else(|| previous_refresh.map(str::to_string))
                .unwrap_or_default(),
            expires_at: now_secs() + token.expires_in.unwrap_or(3600),
        }
    }

    /// Binds the loopback listener and builds the authorize URL. Fails fast
    /// (before any browser opens) if the redirect port is taken.
    pub fn begin_browser_flow(&self, prompt: &Prompt) -> Result<PendingBrowserFlow, Tone3000Error> {
        // A just-cancelled flow releases the port within ~100 ms (its poll
        // interval), so retry briefly before giving up.
        let mut attempt = 0;
        let listener = loop {
            match TcpListener::bind(("127.0.0.1", self.inner.redirect_port)) {
                Ok(listener) => break listener,
                Err(e) if attempt < 6 && e.kind() == std::io::ErrorKind::AddrInUse => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(Tone3000Error::Io(format!(
                        "could not listen on 127.0.0.1:{} for the sign-in redirect: {e}",
                        self.inner.redirect_port
                    )));
                }
            }
        };
        let redirect_uri = format!("http://127.0.0.1:{}/callback", self.inner.redirect_port);
        let pkce = new_pkce()?;
        let state = random_token(16)?;
        let url = authorize_url(&self.inner.client_id, &redirect_uri, &pkce, &state, prompt);
        Ok(PendingBrowserFlow {
            listener,
            pkce,
            state,
            redirect_uri,
            url,
        })
    }

    /// POSTs to the token endpoint. OAuth errors keep their `error` code so
    /// callers can tell `authorization_pending` from a real failure.
    fn post_token_raw(&self, form: &[(&str, &str)]) -> Result<TokenResponse, TokenFailure> {
        let mut response = self
            .inner
            .agent
            .post(format!("{API_BASE}/api/v1/oauth/token"))
            .send_form(form.iter().copied())
            .map_err(|e| TokenFailure::Other(http_error(e)))?;
        if response.status().is_success() {
            return response
                .body_mut()
                .read_json()
                .map_err(|e| TokenFailure::Other(http_error(e)));
        }
        let status = response.status();
        match response.body_mut().read_json::<OAuthErrorBody>() {
            Ok(body) => Err(TokenFailure::OAuth(body)),
            Err(_) => Err(TokenFailure::Other(Tone3000Error::Http(format!(
                "token request failed ({status})"
            )))),
        }
    }

    fn post_token(&self, form: &[(&str, &str)]) -> Result<TokenResponse, Tone3000Error> {
        self.post_token_raw(form).map_err(|failure| match failure {
            TokenFailure::OAuth(body) if body.error == "invalid_grant" => {
                Tone3000Error::NotSignedIn
            }
            TokenFailure::OAuth(body) => {
                Tone3000Error::OAuth(body.error_description.unwrap_or(body.error))
            }
            TokenFailure::Other(e) => e,
        })
    }

    fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<(), Tone3000Error> {
        let token = self.post_token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("client_id", &self.inner.client_id),
        ])?;
        let session = self.session_from(token, None);
        self.store_session(session);
        Ok(())
    }

    pub fn request_device_code(&self) -> Result<DeviceCode, Tone3000Error> {
        let mut response = self
            .inner
            .agent
            .post(format!("{API_BASE}/api/v1/oauth/device_authorization"))
            .send_form([("client_id", self.inner.client_id.as_str())])
            .map_err(http_error)?;
        if !response.status().is_success() {
            let body: Result<OAuthErrorBody, _> = response.body_mut().read_json();
            return Err(match body {
                Ok(err) => Tone3000Error::OAuth(err.error_description.unwrap_or(err.error)),
                Err(_) => Tone3000Error::Http(format!(
                    "device code request failed ({})",
                    response.status()
                )),
            });
        }
        response.body_mut().read_json().map_err(http_error)
    }

    /// Polls until the user approves the code, following the RFC 8628
    /// `authorization_pending` / `slow_down` rules.
    pub fn poll_device_code(
        &self,
        code: &DeviceCode,
        cancel: &AtomicBool,
    ) -> Result<(), Tone3000Error> {
        let deadline = Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = code.interval.max(1);
        loop {
            // Sleep in short steps so cancelling is responsive.
            let wake = Instant::now() + Duration::from_secs(interval);
            while Instant::now() < wake {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Tone3000Error::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if Instant::now() > deadline {
                return Err(Tone3000Error::OAuth(
                    "the code expired; request a new one".into(),
                ));
            }
            let result = self.post_token_raw(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &code.device_code),
                ("client_id", &self.inner.client_id),
            ]);
            match result {
                Ok(token) => {
                    let session = self.session_from(token, None);
                    self.store_session(session);
                    return Ok(());
                }
                Err(TokenFailure::OAuth(body)) => match body.error.as_str() {
                    "authorization_pending" => {}
                    "slow_down" => interval += 5,
                    "access_denied" => {
                        return Err(Tone3000Error::OAuth("sign-in was declined".into()));
                    }
                    "expired_token" => {
                        return Err(Tone3000Error::OAuth(
                            "the code expired; request a new one".into(),
                        ));
                    }
                    _ => {
                        return Err(Tone3000Error::OAuth(
                            body.error_description.unwrap_or(body.error),
                        ));
                    }
                },
                Err(TokenFailure::Other(e)) => return Err(e),
            }
        }
    }

    /// Current access token, refreshed first if it's about to expire.
    fn access_token(&self, force_refresh: bool) -> Result<String, Tone3000Error> {
        let mut guard = self.inner.session.lock().unwrap();
        let session = guard.as_ref().ok_or(Tone3000Error::NotSignedIn)?.clone();
        if !force_refresh && session.expires_at > now_secs() + EXPIRY_MARGIN_SECS {
            return Ok(session.access_token);
        }
        // Hold the lock across the refresh so parallel requests don't each
        // spend the same refresh token.
        let refreshed = self.post_token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &session.refresh_token),
            ("client_id", &self.inner.client_id),
        ]);
        match refreshed {
            Ok(token) => {
                let new_session = self.session_from(token, Some(&session.refresh_token));
                if let Some(path) = &self.inner.session_path {
                    let _ = storage::save_session(path, &new_session);
                }
                let access = new_session.access_token.clone();
                *guard = Some(new_session);
                Ok(access)
            }
            Err(Tone3000Error::NotSignedIn) => {
                *guard = None;
                drop(guard);
                if let Some(path) = &self.inner.session_path {
                    let _ = std::fs::remove_file(path);
                }
                Err(Tone3000Error::NotSignedIn)
            }
            Err(other) => Err(other),
        }
    }

    fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, Tone3000Error> {
        let url = format!("{API_BASE}{path}");
        for attempt in 0..2 {
            let token = self.access_token(attempt > 0)?;
            let mut request = self
                .inner
                .agent
                .get(&url)
                .header("Authorization", &format!("Bearer {token}"));
            for (key, value) in query {
                request = request.query(*key, value);
            }
            let mut response = request.call().map_err(http_error)?;
            match response.status().as_u16() {
                200..=299 => return response.body_mut().read_json().map_err(http_error),
                // The token may have been revoked early; refresh once and retry.
                401 if attempt == 0 => continue,
                401 => return Err(Tone3000Error::NotSignedIn),
                429 => {
                    return Err(Tone3000Error::Http(
                        "TONE3000 rate limit reached; try again in a minute".into(),
                    ));
                }
                status => {
                    return Err(Tone3000Error::Http(format!(
                        "{path} returned HTTP {status}"
                    )));
                }
            }
        }
        Err(Tone3000Error::NotSignedIn)
    }

    pub fn user(&self) -> Result<EmbeddedUser, Tone3000Error> {
        self.get_json("/api/v1/user", &[])
    }

    /// `gear` narrows lists that support it (all but Latest), e.g. `cab`:
    /// the unfiltered Trending feed is NAM-only in practice, so IRs only
    /// show up when asking for cab gear.
    pub fn list_tones(
        &self,
        list: ToneList,
        gear: Option<&str>,
    ) -> Result<Vec<Tone>, Tone3000Error> {
        let mut query = if list.is_paginated() {
            vec![("page", "1".to_string()), ("page_size", "50".to_string())]
        } else {
            Vec::new()
        };
        if let Some(gear) = gear.filter(|_| list != ToneList::Latest) {
            query.push(("gear", gear.to_string()));
        }
        let page: Page<Tone> = self.get_json(list.path(), &query)?;
        Ok(page.data)
    }

    pub fn tone(&self, tone_id: u64) -> Result<Tone, Tone3000Error> {
        self.get_json(&format!("/api/v1/tones/{tone_id}"), &[])
    }

    pub fn models(&self, tone: &Tone) -> Result<Vec<Model>, Tone3000Error> {
        let mut query = vec![
            ("tone_id", tone.id.to_string()),
            ("page", "1".to_string()),
            ("page_size", "100".to_string()),
        ];
        if let Some(architecture) = tone.preferred_architecture() {
            query.push(("architecture", architecture.to_string()));
        }
        let page: Page<Model> = self.get_json("/api/v1/models", &query)?;
        Ok(page.data)
    }

    /// Downloads one model file into the app's TONE3000 folder and writes
    /// the attribution sidecar next to it. Returns the file path.
    pub fn download_model(&self, tone: &Tone, model: &Model) -> Result<PathBuf, Tone3000Error> {
        if model.model_url.is_empty() {
            return Err(Tone3000Error::Other(
                "this model has no download URL".into(),
            ));
        }
        let path = storage::model_file_path(tone, model)
            .ok_or_else(|| Tone3000Error::Io("no writable data directory on this device".into()))?;

        let mut request = self.inner.agent.get(&model.model_url);
        if is_tone3000_url(&model.model_url) {
            let token = self.access_token(false)?;
            request = request.header("Authorization", &format!("Bearer {token}"));
        }
        let mut response = request.call().map_err(http_error)?;
        if !response.status().is_success() {
            return Err(Tone3000Error::Http(format!(
                "download failed (HTTP {})",
                response.status()
            )));
        }
        let bytes = response
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .read_to_vec()
            .map_err(http_error)?;
        storage::write_model(&path, &bytes, &super::types::source_for(tone, model))
            .map_err(|e| Tone3000Error::Io(e.to_string()))?;
        Ok(path)
    }
}

/// Fetches a tone image or avatar. These are public, so no token is sent
/// and no client (or sign-in) is needed.
pub fn fetch_public_image(url: &str) -> Result<Vec<u8>, Tone3000Error> {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        ureq::Agent::new_with_config(config)
    });
    fetch_public_bytes(agent, url)
}

enum TokenFailure {
    OAuth(OAuthErrorBody),
    Other(Tone3000Error),
}

fn fetch_public_bytes(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, Tone3000Error> {
    if !url.starts_with("https://") {
        return Err(Tone3000Error::Other("only https images are loaded".into()));
    }
    let mut response = agent.get(url).call().map_err(http_error)?;
    if !response.status().is_success() {
        return Err(Tone3000Error::Http(format!(
            "image request failed ({})",
            response.status()
        )));
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_IMAGE_BYTES)
        .read_to_vec()
        .map_err(http_error)
}

fn http_error(e: ureq::Error) -> Tone3000Error {
    Tone3000Error::Http(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        // Appendix B of RFC 7636.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_has_valid_length_and_alphabet() {
        let pkce = new_pkce().unwrap();
        assert!((43..=128).contains(&pkce.verifier.len()));
        assert!(
            pkce.verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_eq!(pkce.challenge, pkce_challenge(&pkce.verifier));
    }

    #[test]
    fn authorize_url_carries_required_parameters() {
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "c".into(),
        };
        let url = authorize_url(
            "pk_123",
            "http://127.0.0.1:47823/callback",
            &pkce,
            "st",
            &Prompt::SelectTone {
                format: Some("nam".into()),
            },
        );
        assert!(url.starts_with("https://www.tone3000.com/api/v1/oauth/authorize?"));
        for expected in [
            "client_id=pk_123",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A47823%2Fcallback",
            "response_type=code",
            "code_challenge=c",
            "code_challenge_method=S256",
            "state=st",
            "prompt=select_tone",
            "format=nam",
        ] {
            assert!(url.contains(expected), "missing {expected} in {url}");
        }
    }

    #[test]
    fn load_tone_prompt_includes_tone_id() {
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "c".into(),
        };
        let url = authorize_url(
            "pk",
            "http://127.0.0.1:1/callback",
            &pkce,
            "s",
            &Prompt::LoadTone {
                tone_id: 42,
                format: None,
            },
        );
        assert!(url.contains("prompt=load_tone"));
        assert!(url.contains("tone_id=42"));
    }

    #[test]
    fn parses_callback_request_line() {
        let params =
            parse_callback("GET /callback?code=abc%2B1&state=xyz&tone_id=99 HTTP/1.1\r\n").unwrap();
        assert_eq!(params.code.as_deref(), Some("abc+1"));
        assert_eq!(params.state.as_deref(), Some("xyz"));
        assert_eq!(params.tone_id, Some(99));
        assert!(!params.canceled);

        let canceled = parse_callback("GET /callback?state=xyz&canceled=true HTTP/1.1").unwrap();
        assert!(canceled.canceled);
        assert_eq!(canceled.code, None);

        assert!(parse_callback("GET /favicon.ico HTTP/1.1").is_none());
    }

    #[test]
    fn url_decode_handles_truncated_escapes() {
        assert_eq!(url_decode("a%2"), "a%2");
        assert_eq!(url_decode("%41%42"), "AB");
        assert_eq!(url_decode("a+b"), "a b");
    }

    #[test]
    fn token_only_goes_to_tone3000_hosts() {
        assert!(is_tone3000_url(
            "https://www.tone3000.com/api/v1/models/1/download"
        ));
        assert!(is_tone3000_url("https://tone3000.com/x"));
        assert!(is_tone3000_url("https://cdn.tone3000.com:443/x"));
        assert!(!is_tone3000_url("http://www.tone3000.com/x"));
        assert!(!is_tone3000_url("https://tone3000.com.evil.example/x"));
        assert!(!is_tone3000_url("https://eviltone3000.com/x"));
        assert!(!is_tone3000_url("https://www.tone3000.com@evil.example/x"));
    }

    #[test]
    fn loopback_flow_rejects_state_mismatch() {
        let client = Tone3000Client::with_session("pk".into(), 0, None, None);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let flow = PendingBrowserFlow {
            listener,
            pkce: Pkce {
                verifier: "v".into(),
                challenge: "c".into(),
            },
            state: "expected".into(),
            redirect_uri: format!("http://127.0.0.1:{port}/callback"),
            url: String::new(),
        };
        let browser = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"GET /callback?code=abc&state=forged HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut body = String::new();
            let _ = std::io::Read::read_to_string(&mut stream, &mut body);
            body
        });
        let cancel = AtomicBool::new(false);
        let result = flow.wait(&client, &cancel);
        assert!(matches!(result, Err(Tone3000Error::OAuth(_))));
        assert!(browser.join().unwrap().starts_with("HTTP/1.1 200"));
        assert!(!client.is_signed_in());
    }

    #[test]
    fn loopback_flow_reports_cancel_without_code() {
        let client = Tone3000Client::with_session("pk".into(), 0, None, None);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let flow = PendingBrowserFlow {
            listener,
            pkce: Pkce {
                verifier: "v".into(),
                challenge: "c".into(),
            },
            state: "s1".into(),
            redirect_uri: format!("http://127.0.0.1:{port}/callback"),
            url: String::new(),
        };
        std::thread::spawn(move || {
            let mut favicon = TcpStream::connect(("127.0.0.1", port)).unwrap();
            favicon
                .write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n")
                .unwrap();
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"GET /callback?state=s1&canceled=true HTTP/1.1\r\n\r\n")
                .unwrap();
        });
        let cancel = AtomicBool::new(false);
        let result = flow.wait(&client, &cancel).unwrap();
        assert!(result.canceled);
        assert_eq!(result.tone_id, None);
    }

    #[test]
    fn requests_without_session_report_not_signed_in() {
        let client = Tone3000Client::with_session("pk".into(), 0, None, None);
        assert!(matches!(client.user(), Err(Tone3000Error::NotSignedIn)));
    }
}
