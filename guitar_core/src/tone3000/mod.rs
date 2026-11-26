//! TONE3000 (formerly ToneHunt) integration: sign-in, the user's tone lists,
//! and downloading NAM captures and IRs into the pedal chain.
//!
//! Powered by TONE3000 — https://www.tone3000.com. Use of the API is bound by
//! the TONE3000 API Terms of Service and Design Requirements
//! (https://www.tone3000.com/api/terms).
//!
//! Configuration (the publishable key is safe to ship; the secret key must
//! never be put in the app):
//! - `TONE3000_CLIENT_ID`: publishable key from TONE3000 settings. Read at
//!   runtime, falling back to the value baked in at compile time.
//! - `TONE3000_REDIRECT_PORT`: loopback port for the sign-in redirect,
//!   default [`DEFAULT_REDIRECT_PORT`]. If you register redirect URIs in the
//!   TONE3000 settings, register `http://127.0.0.1:<port>/callback`.

pub mod client;
pub mod storage;
pub mod types;

use std::fmt;

use crate::shared::i18n::tr;
use crate::trf;

pub use client::{BrowserFlowResult, DeviceCode, PendingBrowserFlow, Prompt, Tone3000Client};
pub use types::{EmbeddedUser, Model, Tone, ToneList};

pub const API_BASE: &str = "https://www.tone3000.com";
pub const WEBSITE_URL: &str = "https://www.tone3000.com";
pub const DEFAULT_REDIRECT_PORT: u16 = 47823;

/// Publishable key, or `None` if the integration isn't configured.
pub fn client_id() -> Option<String> {
    std::env::var("TONE3000_CLIENT_ID")
        .ok()
        .or_else(|| option_env!("TONE3000_CLIENT_ID").map(str::to_string))
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

pub fn redirect_port() -> u16 {
    std::env::var("TONE3000_REDIRECT_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(DEFAULT_REDIRECT_PORT)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tone3000Error {
    /// No session, or the refresh token expired. The user must sign in.
    NotSignedIn,
    Cancelled,
    OAuth(String),
    Http(String),
    Io(String),
    Other(String),
}

impl fmt::Display for Tone3000Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tone3000Error::NotSignedIn => f.write_str(tr("not signed in to TONE3000", "você não está conectado ao TONE3000")),
            Tone3000Error::Cancelled => f.write_str(tr("cancelled", "cancelado")),
            Tone3000Error::OAuth(message) => f.write_str(&trf!("sign-in failed: {message}", "falha no login: {message}")),
            Tone3000Error::Http(message) => f.write_str(&trf!("network error: {message}", "erro de rede: {message}")),
            Tone3000Error::Io(message) => f.write_str(&trf!("file error: {message}", "erro de arquivo: {message}")),
            Tone3000Error::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Tone3000Error {}
