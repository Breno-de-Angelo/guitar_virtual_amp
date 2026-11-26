//! UI language (English or Brazilian Portuguese).
//!
//! Texts are written in both languages at the place they're shown:
//! `tr("Open tab...", "Abrir tab...")` for plain strings and
//! `trf!("{n} bars", "{n} compassos")` for formatted ones. The language is a
//! process-wide setting the app sets from `UiPrefs::language` at launch and
//! when the user changes it; everything drawn after that frame follows it.
//!
//! Only translate what the user reads. Strings that are stored, matched or
//! used as identifiers (settings values, preset and tuning names, file
//! names, track tags, `Page::key`) stay as they are, with a separate
//! display function when they're shown.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    English,
    Portuguese,
}

/// The language choice saved in the settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LanguagePref {
    /// The operating system's language: Portuguese for any `pt` locale,
    /// English otherwise.
    #[default]
    System,
    English,
    Portuguese,
}

impl LanguagePref {
    pub fn resolve(self) -> Language {
        match self {
            LanguagePref::System => system_language(),
            LanguagePref::English => Language::English,
            LanguagePref::Portuguese => Language::Portuguese,
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set_language(language: Language) {
    CURRENT.store(language as u8, Ordering::Relaxed);
}

pub fn language() -> Language {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Language::Portuguese,
        _ => Language::English,
    }
}

pub fn is_portuguese() -> bool {
    language() == Language::Portuguese
}

/// The text for the current language.
pub fn tr(en: &'static str, pt: &'static str) -> &'static str {
    if is_portuguese() { pt } else { en }
}

/// `format!` with one template per language:
/// `trf!("{count} bars", "{count} compassos")`, or with positional
/// arguments after the two templates, shared by both:
/// `trf!("Bar {}", "Compasso {}", bar + 1)`.
#[macro_export]
macro_rules! trf {
    ($en:literal, $pt:literal $(,)?) => {
        if $crate::shared::i18n::is_portuguese() { format!($pt) } else { format!($en) }
    };
    ($en:literal, $pt:literal, $($arg:tt)+) => {
        if $crate::shared::i18n::is_portuguese() { format!($pt, $($arg)+) } else { format!($en, $($arg)+) }
    };
}

/// Language from a locale name such as `pt_BR.UTF-8`, `pt-PT` or `en-US`.
pub fn from_locale(locale: &str) -> Language {
    let lang = locale.split(['_', '-', '.', '@']).next().unwrap_or("");
    if lang.eq_ignore_ascii_case("pt") { Language::Portuguese } else { Language::English }
}

/// The operating system's language. Always English in unit tests, so they
/// don't depend on the machine's locale.
fn system_language() -> Language {
    if cfg!(test) {
        return Language::English;
    }
    sys_locale::get_locale().map(|l| from_locale(&l)).unwrap_or(Language::English)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_map_to_languages() {
        assert_eq!(from_locale("pt_BR.UTF-8"), Language::Portuguese);
        assert_eq!(from_locale("pt-PT"), Language::Portuguese);
        assert_eq!(from_locale("PT"), Language::Portuguese);
        assert_eq!(from_locale("en-US"), Language::English);
        assert_eq!(from_locale("es_ES"), Language::English);
        assert_eq!(from_locale(""), Language::English);
    }

    #[test]
    fn system_is_english_in_tests() {
        assert_eq!(LanguagePref::System.resolve(), Language::English);
        assert_eq!(LanguagePref::Portuguese.resolve(), Language::Portuguese);
    }
}
