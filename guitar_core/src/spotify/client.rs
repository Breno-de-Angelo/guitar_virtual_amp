//! Blocking Spotify Web API client. Every method does network I/O, so call
//! them from a worker thread, never from the UI or audio thread.
//!
//! Sign-in is Authorization Code with PKCE and a loopback redirect, sharing
//! the listener/PKCE helpers with the TONE3000 client. Tokens are stored in
//! the app data dir with owner-only permissions and refreshed before they
//! expire; a revoked refresh token sends the user back to sign-in.
//!
//! HTTP 429 is retried after the `Retry-After` delay (exponential backoff
//! when the header is missing), up to a short cap; 5xx is retried twice.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde_json::json;

use super::types::{
    Device, Devices, ErrorBody, OAuthErrorBody, Paging, PlaybackState, Playlist, PlaylistItem,
    Track, User,
};
use super::{ACCOUNTS_BASE, API_BASE, SCOPES, SpotifyError, redirect_uri};
use crate::shared::i18n::tr;
use crate::shared::paths::app_data_dir;
use crate::tone3000::Tone3000Error;
use crate::tone3000::client::{
    Pkce, Session, new_pkce, random_token, url_encode, wait_for_callback,
};
use crate::tone3000::storage::{load_session, save_session};
use crate::trf;

const EXPIRY_MARGIN_SECS: u64 = 60;
/// Longest `Retry-After` the client sleeps through before giving up.
const MAX_RETRY_WAIT_SECS: u64 = 10;
const MAX_ATTEMPTS: u32 = 4;
const PAGE_SIZE: u32 = 50;
const MAX_PLAYLISTS: u32 = 1000;
const MAX_PLAYLIST_TRACKS: u32 = 2000;
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

const CALLBACK_PAGE_EN: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Spotify</title></head>\
<body style=\"font-family:sans-serif;text-align:center;padding-top:15vh\">\
<h2>Done</h2><p>You can close this tab and go back to Guitar Virtual Amp.</p></body></html>";
const CALLBACK_PAGE_PT: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Spotify</title></head>\
<body style=\"font-family:sans-serif;text-align:center;padding-top:15vh\">\
<h2>Pronto</h2><p>Pode fechar esta aba e voltar ao Guitar Virtual Amp.</p></body></html>";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn session_path() -> Option<PathBuf> {
    app_data_dir().map(|dir| dir.join("spotify").join("session.json"))
}

/// Spotify IDs are base-62; anything else is refused before it can end up
/// in a URL path.
pub(crate) fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub(crate) fn authorize_url(
    client_id: &str,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
) -> String {
    let params = [
        ("client_id", client_id.to_string()),
        ("response_type", "code".to_string()),
        ("redirect_uri", redirect_uri.to_string()),
        ("code_challenge_method", "S256".to_string()),
        ("code_challenge", pkce.challenge.clone()),
        ("state", state.to_string()),
        ("scope", SCOPES.join(" ")),
    ];
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", url_encode(v)))
        .collect();
    format!("{ACCOUNTS_BASE}/authorize?{}", query.join("&"))
}

/// Body for "play `track_uri`", inside `context_uri` (a playlist) when given
/// so Spotify keeps playing the rest of the playlist afterwards.
pub(crate) fn play_body(context_uri: Option<&str>, track_uri: &str) -> serde_json::Value {
    match context_uri {
        Some(context) => json!({ "context_uri": context, "offset": { "uri": track_uri } }),
        None => json!({ "uris": [track_uri] }),
    }
}

/// Seconds to wait before retry number `attempt` (0-based) of a 429.
pub(crate) fn retry_delay_secs(retry_after: Option<&str>, attempt: u32) -> u64 {
    retry_after
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(1u64 << attempt.min(5))
}

fn from_tone3000(error: Tone3000Error) -> SpotifyError {
    match error {
        Tone3000Error::Cancelled => SpotifyError::Cancelled,
        Tone3000Error::OAuth(message) => SpotifyError::OAuth(message),
        other => SpotifyError::Other(other.to_string()),
    }
}

fn http_error(e: ureq::Error) -> SpotifyError {
    SpotifyError::Http(e.to_string())
}

fn api_error(status: u16, body: &[u8]) -> SpotifyError {
    match serde_json::from_slice::<ErrorBody>(body) {
        Ok(parsed) => SpotifyError::Api {
            status: if parsed.error.status == 0 {
                status
            } else {
                parsed.error.status
            },
            message: parsed.error.message,
            reason: parsed.error.reason,
        },
        Err(_) => SpotifyError::Api {
            status,
            message: format!("HTTP {status}"),
            reason: None,
        },
    }
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Put,
    Post,
}

/// A sign-in with its loopback listener bound, waiting for the browser.
pub struct PendingSignIn {
    listener: TcpListener,
    pkce: Pkce,
    state: String,
    redirect_uri: String,
    pub url: String,
}

impl PendingSignIn {
    /// Blocks until Spotify redirects back (or `cancel` is set / timeout).
    pub fn wait(self, client: &SpotifyClient, cancel: &AtomicBool) -> Result<(), SpotifyError> {
        let params =
            wait_for_callback(&self.listener, cancel, tr(CALLBACK_PAGE_EN, CALLBACK_PAGE_PT)).map_err(from_tone3000)?;
        if params.state.as_deref() != Some(self.state.as_str()) {
            return Err(SpotifyError::OAuth(
                tr(
                    "the response doesn't match this request (state)",
                    "a resposta não corresponde a este pedido (state)",
                )
                .into(),
            ));
        }
        if let Some(error) = params.error {
            return Err(match error.as_str() {
                "access_denied" => SpotifyError::OAuth(
                    tr("access denied on Spotify", "acesso negado no Spotify").into(),
                ),
                _ => SpotifyError::OAuth(error),
            });
        }
        let code = params
            .code
            .ok_or_else(|| SpotifyError::OAuth(
                    tr("Spotify didn't return a code", "o Spotify não devolveu um código").into(),
                ))?;
        client.exchange_code(&code, &self.pkce.verifier, &self.redirect_uri)
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
pub struct SpotifyClient {
    inner: Arc<Inner>,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl SpotifyClient {
    pub fn new(client_id: String, redirect_port: u16) -> Self {
        let session_path = session_path();
        let session = session_path.as_deref().and_then(load_session);
        Self::with_session(client_id, redirect_port, session, session_path)
    }

    pub(crate) fn with_session(
        client_id: String,
        redirect_port: u16,
        session: Option<Session>,
        session_path: Option<PathBuf>,
    ) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
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

    /// Forgets the tokens, on disk too. Revoking the app's access entirely
    /// is done by the user at spotify.com/account/apps.
    pub fn sign_out(&self) {
        *self.inner.session.lock().unwrap() = None;
        if let Some(path) = &self.inner.session_path {
            let _ = std::fs::remove_file(path);
        }
    }

    fn store_session(&self, session: Session) {
        if let Some(path) = &self.inner.session_path
            && let Err(e) = save_session(path, &session)
        {
            log::warn!("could not save Spotify session: {e}");
        }
        *self.inner.session.lock().unwrap() = Some(session);
    }

    pub fn begin_sign_in(&self) -> Result<PendingSignIn, SpotifyError> {
        let port = self.inner.redirect_port;
        let mut attempt = 0;
        let listener = loop {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => break listener,
                Err(e) if attempt < 6 && e.kind() == std::io::ErrorKind::AddrInUse => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(SpotifyError::Other(trf!(
                        "could not listen on 127.0.0.1:{port} for the sign-in callback: {e}",
                        "não foi possível escutar em 127.0.0.1:{port} para o retorno do login: {e}"
                    )));
                }
            }
        };
        let redirect_uri = redirect_uri(port);
        let pkce = new_pkce().map_err(from_tone3000)?;
        let state = random_token(16).map_err(from_tone3000)?;
        let url = authorize_url(&self.inner.client_id, &redirect_uri, &pkce, &state);
        Ok(PendingSignIn {
            listener,
            pkce,
            state,
            redirect_uri,
            url,
        })
    }

    fn post_token(&self, form: &[(&str, &str)]) -> Result<TokenResponse, SpotifyError> {
        let mut response = self
            .inner
            .agent
            .post(format!("{ACCOUNTS_BASE}/api/token"))
            .send_form(form.iter().copied())
            .map_err(http_error)?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(http_error)?;
        if (200..300).contains(&status) {
            return serde_json::from_slice(&body)
                .map_err(|e| SpotifyError::Other(trf!(
                    "invalid token response: {e}",
                    "resposta de token inválida: {e}"
                )));
        }
        match serde_json::from_slice::<OAuthErrorBody>(&body) {
            Ok(err) if err.error == "invalid_grant" => Err(SpotifyError::NotSignedIn),
            Ok(err) => Err(SpotifyError::OAuth(
                err.error_description.unwrap_or(err.error),
            )),
            Err(_) => Err(SpotifyError::OAuth(trf!(
                "token request failed (HTTP {status})",
                "pedido de token falhou (HTTP {status})"
            ))),
        }
    }

    fn session_from(token: TokenResponse, previous_refresh: Option<&str>) -> Session {
        Session {
            access_token: token.access_token,
            // Spotify may or may not rotate the refresh token.
            refresh_token: token
                .refresh_token
                .or_else(|| previous_refresh.map(str::to_string))
                .unwrap_or_default(),
            expires_at: now_secs() + token.expires_in.unwrap_or(3600),
        }
    }

    fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<(), SpotifyError> {
        let token = self.post_token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &self.inner.client_id),
            ("code_verifier", verifier),
        ])?;
        self.store_session(Self::session_from(token, None));
        Ok(())
    }

    fn access_token(&self, force_refresh: bool) -> Result<String, SpotifyError> {
        let mut guard = self.inner.session.lock().unwrap();
        let session = guard.as_ref().ok_or(SpotifyError::NotSignedIn)?.clone();
        if !force_refresh && session.expires_at > now_secs() + EXPIRY_MARGIN_SECS {
            return Ok(session.access_token);
        }
        // Lock held across the refresh so parallel requests don't race.
        match self.post_token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", &session.refresh_token),
            ("client_id", &self.inner.client_id),
        ]) {
            Ok(token) => {
                let fresh = Self::session_from(token, Some(&session.refresh_token));
                if let Some(path) = &self.inner.session_path {
                    let _ = save_session(path, &fresh);
                }
                let access = fresh.access_token.clone();
                *guard = Some(fresh);
                Ok(access)
            }
            Err(SpotifyError::NotSignedIn) => {
                *guard = None;
                drop(guard);
                if let Some(path) = &self.inner.session_path {
                    let _ = std::fs::remove_file(path);
                }
                Err(SpotifyError::NotSignedIn)
            }
            Err(other) => Err(other),
        }
    }

    /// Sends one API request with auth, refresh-on-401 and retries.
    /// Returns the body, or `None` for an empty (204) response.
    fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<Option<Vec<u8>>, SpotifyError> {
        let url = format!("{API_BASE}{path}");
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            let token = self.access_token(refreshed)?;
            let auth = format!("Bearer {token}");
            let agent = &self.inner.agent;
            let result = match method {
                Method::Get => agent.get(&url).header("Authorization", &auth).call(),
                Method::Put | Method::Post => {
                    let request = match method {
                        Method::Put => agent.put(&url),
                        _ => agent.post(&url),
                    }
                    .header("Authorization", &auth);
                    match body {
                        Some(json) => request.send_json(json),
                        None => request.send_empty(),
                    }
                }
            };
            let mut response = result.map_err(http_error)?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let bytes = response
                .body_mut()
                .with_config()
                .limit(MAX_RESPONSE_BYTES)
                .read_to_vec()
                .map_err(http_error)?;
            match status {
                200..=299 => return Ok((!bytes.is_empty() && status != 204).then_some(bytes)),
                401 if !refreshed => refreshed = true,
                401 => return Err(SpotifyError::NotSignedIn),
                429 => {
                    let wait = retry_delay_secs(retry_after.as_deref(), attempt);
                    if attempt + 1 >= MAX_ATTEMPTS || wait > MAX_RETRY_WAIT_SECS {
                        return Err(SpotifyError::RateLimited {
                            retry_after_secs: wait,
                        });
                    }
                    std::thread::sleep(Duration::from_secs(wait));
                }
                500 | 502 | 503 | 504 if attempt < 2 => {
                    std::thread::sleep(Duration::from_secs(1u64 << attempt));
                }
                _ => return Err(api_error(status, &bytes)),
            }
            attempt += 1;
            if attempt >= MAX_ATTEMPTS {
                return Err(SpotifyError::Http(trf!(
                    "{path}: no valid response after {MAX_ATTEMPTS} attempts",
                    "{path}: sem resposta válida após {MAX_ATTEMPTS} tentativas"
                )));
            }
        }
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, SpotifyError> {
        match self.call(Method::Get, path, None)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| SpotifyError::Other(trf!(
                    "unexpected response from {path}: {e}",
                    "resposta inesperada de {path}: {e}"
                ))),
            None => Ok(None),
        }
    }

    pub fn current_user(&self) -> Result<User, SpotifyError> {
        self.get_json("/me")?
            .ok_or_else(|| SpotifyError::Other(tr("empty profile", "perfil vazio").into()))
    }

    /// Playlists the user owns or follows (`GET /me/playlists`).
    pub fn playlists(&self) -> Result<Vec<Playlist>, SpotifyError> {
        let mut all = Vec::new();
        let mut offset = 0;
        while offset < MAX_PLAYLISTS {
            let page: Paging<Playlist> = self
                .get_json(&format!("/me/playlists?limit={PAGE_SIZE}&offset={offset}"))?
                .unwrap_or_default();
            let count = page.items.len() as u32;
            all.extend(page.items);
            if page.next.is_none() || count == 0 {
                break;
            }
            offset += count;
        }
        Ok(all)
    }

    /// Tracks of a playlist the user owns or collaborates on
    /// (`GET /playlists/{id}/items`). Episodes, local files and unavailable
    /// tracks are left out.
    pub fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>, SpotifyError> {
        if !is_valid_id(playlist_id) {
            return Err(SpotifyError::Other(
                tr("invalid playlist ID", "ID de playlist inválido").into(),
            ));
        }
        let mut tracks = Vec::new();
        let mut offset = 0;
        while offset < MAX_PLAYLIST_TRACKS {
            let path = format!(
                "/playlists/{playlist_id}/items?limit={PAGE_SIZE}&offset={offset}&additional_types=track"
            );
            let page: Paging<PlaylistItem> = self.get_json(&path)?.unwrap_or_default();
            let count = page.items.len() as u32;
            tracks.extend(
                page.items
                    .into_iter()
                    .filter_map(|i| i.item)
                    .filter(Track::is_playable_track),
            );
            if page.next.is_none() || count == 0 {
                break;
            }
            offset += count;
        }
        Ok(tracks)
    }

    /// What's playing now; `None` when nothing is (HTTP 204).
    pub fn playback(&self) -> Result<Option<PlaybackState>, SpotifyError> {
        self.get_json("/me/player")
    }

    pub fn devices(&self) -> Result<Vec<Device>, SpotifyError> {
        Ok(self
            .get_json::<Devices>("/me/player/devices")?
            .unwrap_or_default()
            .devices)
    }

    fn device_query(device_id: Option<&str>) -> String {
        device_id
            .map(|id| format!("?device_id={}", url_encode(id)))
            .unwrap_or_default()
    }

    /// Plays `track_uri` (inside `context_uri` when given). If no device is
    /// active, retries on the first available one.
    pub fn play_track(
        &self,
        context_uri: Option<&str>,
        track_uri: &str,
    ) -> Result<(), SpotifyError> {
        let body = play_body(context_uri, track_uri);
        match self.call(Method::Put, "/me/player/play", Some(&body)) {
            Err(error) if error.is_no_active_device() => {
                let device = self
                    .devices()?
                    .into_iter()
                    .find(|d| !d.is_restricted && d.id.is_some())
                    .ok_or(error)?;
                let path = format!(
                    "/me/player/play{}",
                    Self::device_query(device.id.as_deref())
                );
                self.call(Method::Put, &path, Some(&body)).map(|_| ())
            }
            other => other.map(|_| ()),
        }
    }

    pub fn resume(&self) -> Result<(), SpotifyError> {
        self.call(Method::Put, "/me/player/play", None).map(|_| ())
    }

    pub fn pause(&self) -> Result<(), SpotifyError> {
        self.call(Method::Put, "/me/player/pause", None).map(|_| ())
    }

    pub fn next(&self) -> Result<(), SpotifyError> {
        self.call(Method::Post, "/me/player/next", None).map(|_| ())
    }

    pub fn previous(&self) -> Result<(), SpotifyError> {
        self.call(Method::Post, "/me/player/previous", None)
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone3000::client::pkce_challenge;

    #[test]
    fn authorize_url_requests_pkce_and_only_the_needed_scopes() {
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: pkce_challenge("v"),
        };
        let url = authorize_url("abc123", "http://127.0.0.1:47824/callback", &pkce, "st");
        assert!(url.starts_with("https://accounts.spotify.com/authorize?"));
        assert!(url.contains("client_id=abc123"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge)));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A47824%2Fcallback"));
        assert!(url.contains("scope=playlist-read-private%20playlist-read-collaborative%20user-read-playback-state%20user-modify-playback-state"));
        assert!(!url.contains("client_secret"));
    }

    #[test]
    fn ids_are_validated_before_use_in_paths() {
        assert!(is_valid_id("37i9dQZF1DXcBWIGoYBM5M"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("../me"));
        assert!(!is_valid_id("abc?x=1"));
    }

    #[test]
    fn play_body_keeps_the_playlist_context() {
        assert_eq!(
            play_body(Some("spotify:playlist:p"), "spotify:track:t"),
            json!({"context_uri": "spotify:playlist:p", "offset": {"uri": "spotify:track:t"}})
        );
        assert_eq!(
            play_body(None, "spotify:track:t"),
            json!({"uris": ["spotify:track:t"]})
        );
    }

    #[test]
    fn retry_delay_honors_retry_after_then_backs_off() {
        assert_eq!(retry_delay_secs(Some("7"), 0), 7);
        assert_eq!(retry_delay_secs(Some(" 3 "), 2), 3);
        assert_eq!(retry_delay_secs(None, 0), 1);
        assert_eq!(retry_delay_secs(None, 2), 4);
        assert_eq!(retry_delay_secs(Some("soon"), 1), 2);
    }

    #[test]
    fn api_errors_keep_status_message_and_reason() {
        let body = br#"{"error": {"status": 404, "message": "Player command failed: No active device found", "reason": "NO_ACTIVE_DEVICE"}}"#;
        let error = api_error(404, body);
        assert!(error.is_no_active_device());
        let premium = api_error(403, br#"{"error": {"status": 403, "message": "Premium required", "reason": "PREMIUM_REQUIRED"}}"#);
        assert!(premium.to_string().contains("Premium"));
        assert_eq!(
            api_error(502, b"<html>bad gateway</html>"),
            SpotifyError::Api {
                status: 502,
                message: "HTTP 502".into(),
                reason: None
            }
        );
    }

    #[test]
    fn signed_out_client_refuses_requests_without_network() {
        let client = SpotifyClient::with_session("id".into(), 0, None, None);
        assert!(!client.is_signed_in());
        assert_eq!(client.playlists(), Err(SpotifyError::NotSignedIn));
        assert_eq!(
            client.playlist_tracks("../x"),
            Err(SpotifyError::Other("invalid playlist ID".into()))
        );
    }
}
