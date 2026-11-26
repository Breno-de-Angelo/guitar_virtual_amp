//! Spotify integration: sign in, list the user's playlists and their
//! tracks, and remote-control playback on the user's Spotify app (Spotify
//! Connect). Spotify's audio never passes through this app: the Web API
//! doesn't provide it, and the Developer Policy forbids mixing it with other
//! audio or analysing it.
//!
//! Endpoints follow the OpenAPI spec at
//! <https://developer.spotify.com/reference/web-api/open-api-schema.yaml>
//! (February 2026 changes applied: `/playlists/{id}/items` and the `item`
//! field instead of the removed `/tracks` variants).
//!
//! Configuration:
//! - `SPOTIFY_CLIENT_ID`: Client ID of an app created at
//!   <https://developer.spotify.com/dashboard>. Read at runtime, falling back
//!   to a value baked in at compile time (e.g. via `.cargo/config.toml`'s
//!   `[env]`). The app uses Authorization Code with PKCE, so there's no
//!   client secret anywhere.
//! - `SPOTIFY_REDIRECT_PORT`: loopback port, default [`DEFAULT_REDIRECT_PORT`].
//!   The dashboard must list `http://127.0.0.1:<port>/callback` exactly
//!   (Spotify rejects `localhost`).
//!
//! Development-mode apps need a Premium owner and only work for up to five
//! allow-listed users; playback control needs Premium.

pub mod client;
pub mod types;

use std::fmt;

use crate::shared::i18n::tr;
use crate::trf;

pub use client::{PendingSignIn, SpotifyClient};
pub use types::{Device, PlaybackState, Playlist, Track};

pub const ACCOUNTS_BASE: &str = "https://accounts.spotify.com";
pub const API_BASE: &str = "https://api.spotify.com/v1";
pub const DASHBOARD_URL: &str = "https://developer.spotify.com/dashboard";
pub const DEFAULT_REDIRECT_PORT: u16 = 47824;

/// Only what the features need:
/// - `playlist-read-private` / `playlist-read-collaborative`: list the
///   user's playlists and read the tracks of the ones they own or share.
/// - `user-read-playback-state`: what's playing, on which device.
/// - `user-modify-playback-state`: play/pause/skip.
pub const SCOPES: &[&str] = &[
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-read-playback-state",
    "user-modify-playback-state",
];

pub fn client_id() -> Option<String> {
    std::env::var("SPOTIFY_CLIENT_ID")
        .ok()
        .or_else(|| option_env!("SPOTIFY_CLIENT_ID").map(str::to_string))
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

pub fn redirect_port() -> u16 {
    std::env::var("SPOTIFY_REDIRECT_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(DEFAULT_REDIRECT_PORT)
}

pub fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/callback")
}

#[derive(Clone, Debug, PartialEq)]
pub enum SpotifyError {
    /// No session, or the refresh token was revoked/expired.
    NotSignedIn,
    Cancelled,
    OAuth(String),
    /// An API error response. `reason` is Spotify's player error code
    /// (`NO_ACTIVE_DEVICE`, `PREMIUM_REQUIRED`...) when there is one.
    Api {
        status: u16,
        message: String,
        reason: Option<String>,
    },
    RateLimited {
        retry_after_secs: u64,
    },
    Http(String),
    Other(String),
}

impl SpotifyError {
    pub fn is_no_active_device(&self) -> bool {
        matches!(self, SpotifyError::Api { status: 404, reason, message }
            if reason.as_deref() == Some("NO_ACTIVE_DEVICE") || message.to_lowercase().contains("no active device"))
    }
}

impl fmt::Display for SpotifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpotifyError::NotSignedIn => f.write_str(tr("not connected to Spotify", "não conectado ao Spotify")),
            SpotifyError::Cancelled => f.write_str(tr("cancelled", "cancelado")),
            SpotifyError::OAuth(message) => {
                f.write_str(&trf!("could not connect: {message}", "falha ao conectar: {message}"))
            }
            SpotifyError::Api {
                reason: Some(reason),
                ..
            } if reason == "PREMIUM_REQUIRED" => f.write_str(tr(
                "controlling playback requires Spotify Premium",
                "controlar a reprodução exige Spotify Premium",
            )),
            error @ SpotifyError::Api { .. } if error.is_no_active_device() => f.write_str(tr(
                "no active Spotify device: open Spotify (phone, computer...) and play something once",
                "nenhum aparelho com Spotify ativo: abra o Spotify (celular, computador...) e toque algo uma vez",
            )),
            SpotifyError::Api {
                status: 403,
                message,
                ..
            } => f.write_str(&trf!(
                "Spotify refused the request (403): {message}",
                "o Spotify recusou o pedido (403): {message}"
            )),
            SpotifyError::Api {
                status, message, ..
            } => f.write_str(&trf!(
                "Spotify error ({status}): {message}",
                "erro do Spotify ({status}): {message}"
            )),
            SpotifyError::RateLimited { retry_after_secs } => f.write_str(&trf!(
                "Spotify request limit reached; try again in {retry_after_secs} s",
                "limite de pedidos do Spotify atingido; tente de novo em {retry_after_secs} s"
            )),
            SpotifyError::Http(message) => {
                f.write_str(&trf!("network error: {message}", "erro de rede: {message}"))
            }
            SpotifyError::Other(message) => write!(f, "{message}"),
        }
    }
}
impl std::error::Error for SpotifyError {}
