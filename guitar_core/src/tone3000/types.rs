//! Response types for the TONE3000 API v1 (https://www.tone3000.com/api).
//!
//! Enum-like fields (gear, format, license, size) are kept as strings: the
//! API is versioned but still adds and renames values (`full-rig` became
//! `amp-cab`, `ir` moved from gear to format), and a strict enum would turn
//! a new value into a failed request. Most fields default when missing for
//! the same reason.

use serde::{Deserialize, Deserializer};

use crate::shared::i18n::tr;
use crate::shared::pedals::Tone3000Source;

/// Accepts an ID sent as a JSON number or string. The API docs list IDs as
/// numbers, but user IDs come back as UUID strings, so nothing assumes the
/// documented type.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawId {
    Number(u64),
    Text(String),
}

fn id_as_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match Option::<RawId>::deserialize(deserializer)? {
        Some(RawId::Number(n)) => n.to_string(),
        Some(RawId::Text(text)) => text,
        None => String::new(),
    })
}

fn null_as_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// Tone and model IDs are used in URLs and stored in presets as numbers;
/// this still accepts them as numeric strings.
fn id_as_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    match Option::<RawId>::deserialize(deserializer)? {
        Some(RawId::Number(n)) => Ok(n),
        Some(RawId::Text(text)) => text
            .trim()
            .parse()
            .map_err(|_| serde::de::Error::custom(format!("expected a numeric id, got {text:?}"))),
        None => Ok(0),
    }
}

/// `#[serde(default)]` only covers missing fields; the API also sends
/// `null` for fields it documents as always present (e.g. `sizes`).
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct EmbeddedUser {
    #[serde(default, deserialize_with = "id_as_string")]
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub username: String,
    /// Only set for verified creators.
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub is_verified: bool,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub url: String,
}

impl EmbeddedUser {
    /// Name to show for a creator: display name if verified, else username.
    pub fn shown_name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.username)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Tone {
    #[serde(deserialize_with = "id_as_u64")]
    pub id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub gear: String,
    #[serde(default)]
    pub images: Option<Vec<String>>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub format: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub license: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub sizes: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub user: EmbeddedUser,
    #[serde(default, deserialize_with = "null_as_default")]
    pub models_count: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub a1_models_count: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub a2_models_count: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub irs_count: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub url: String,
}

impl Tone {
    pub fn image_url(&self) -> Option<&str> {
        self.images.as_ref()?.first().map(String::as_str)
    }

    /// Human label for the gear type, as the design guidelines ask lists to
    /// show it (e.g. "Amp + Cab").
    pub fn gear_label(&self) -> String {
        gear_label(&self.gear)
    }

    pub fn format_label(&self) -> String {
        format_label(&self.format)
    }

    /// Architecture to request models for. `List Models` defaults to A2 and
    /// returns nothing for tones that only have A1 captures, so ask for A1
    /// explicitly in that case. `None` for non-NAM formats.
    pub fn preferred_architecture(&self) -> Option<&'static str> {
        if self.format != "nam" {
            return None;
        }
        if self.a2_models_count == 0 && self.a1_models_count > 0 {
            Some("1")
        } else {
            Some("2")
        }
    }
}

pub fn gear_label(gear: &str) -> String {
    match gear {
        "amp" => "Amp".into(),
        "amp-cab" | "full-rig" => "Amp + Cab".into(),
        "pedal" => "Pedal".into(),
        "outboard" => "Outboard".into(),
        "cab" => "Cab".into(),
        "space" => "Space".into(),
        "experimental" => "Experimental".into(),
        "ir" => "IR".into(),
        other => other.to_string(),
    }
}

pub fn format_label(format: &str) -> String {
    match format {
        "nam" => "NAM".into(),
        "ir" => "IR".into(),
        other => other.to_uppercase(),
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Model {
    #[serde(deserialize_with = "id_as_u64")]
    pub id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub model_url: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default, deserialize_with = "id_as_u64")]
    pub tone_id: u64,
}

/// Paginated list (`PaginatedResponse<T>`) and the plain `{ data: [...] }`
/// shape used by trending/latest both parse into this.
#[derive(Clone, Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Page<T> {
    #[serde(default = "Vec::new", deserialize_with = "null_as_vec")]
    pub data: Vec<T>,
    #[serde(default)]
    pub total: Option<u64>,
}

/// Which of the bounded list endpoints to read. These are the ones the API
/// terms allow independent integrations to use (search is excluded).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToneList {
    Favorited,
    Created,
    Downloaded,
    Trending,
    Latest,
}

impl ToneList {
    pub const ALL: [ToneList; 5] = [
        ToneList::Favorited,
        ToneList::Created,
        ToneList::Downloaded,
        ToneList::Trending,
        ToneList::Latest,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ToneList::Favorited => tr("Favorites", "Favoritos"),
            ToneList::Created => tr("Created", "Criados"),
            ToneList::Downloaded => tr("Downloaded", "Baixados"),
            ToneList::Trending => tr("Trending", "Em alta"),
            ToneList::Latest => tr("Latest", "Recentes"),
        }
    }

    pub(crate) fn path(self) -> &'static str {
        match self {
            ToneList::Favorited => "/api/v1/tones/favorited",
            ToneList::Created => "/api/v1/tones/created",
            ToneList::Downloaded => "/api/v1/tones/downloaded",
            ToneList::Trending => "/api/v1/tones/trending",
            ToneList::Latest => "/api/v1/tones/latest",
        }
    }

    pub(crate) fn is_paginated(self) -> bool {
        matches!(
            self,
            ToneList::Favorited | ToneList::Created | ToneList::Downloaded
        )
    }
}

/// Builds the attribution record stored in pedal params from API data.
pub fn source_for(tone: &Tone, model: &Model) -> Tone3000Source {
    Tone3000Source {
        tone_id: tone.id,
        model_id: model.id,
        tone_title: tone.title.clone(),
        model_name: model.name.clone(),
        creator: tone.user.shown_name().to_string(),
        creator_username: tone.user.username.clone(),
        gear: tone.gear.clone(),
        format: tone.format.clone(),
        license: tone.license.clone(),
        tone_url: tone.url.clone(),
        image_url: tone.image_url().map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tone_with_missing_optional_fields() {
        let json = r#"{
            "id": 42, "title": "Plexi", "gear": "amp", "format": "nam",
            "license": "t3k", "user": {"id": 1, "username": "someone"},
            "a1_models_count": 3, "a2_models_count": 0,
            "brand_new_field": {"ignored": true}
        }"#;
        let tone: Tone = serde_json::from_str(json).unwrap();
        assert_eq!(tone.id, 42);
        assert_eq!(tone.user.shown_name(), "someone");
        assert_eq!(tone.image_url(), None);
        assert_eq!(tone.preferred_architecture(), Some("1"));
        assert_eq!(tone.gear_label(), "Amp");
    }

    /// Regression: the live API sends user IDs as UUID strings, which
    /// failed every list because each tone embeds its creator.
    #[test]
    fn accepts_uuid_user_ids_and_string_tone_ids() {
        let json = r#"{
            "id": "2698", "user_id": "d320f38b-b4d1-408e-8c46-e4fdb9adab11",
            "user": {"id": "d320f38b-b4d1-408e-8c46-e4fdb9adab11", "username": "akka5"}
        }"#;
        let tone: Tone = serde_json::from_str(json).unwrap();
        assert_eq!(tone.id, 2698);
        assert_eq!(tone.user.id, "d320f38b-b4d1-408e-8c46-e4fdb9adab11");

        let user: EmbeddedUser =
            serde_json::from_str(r#"{"id": 17, "username": "x", "display_name": null}"#).unwrap();
        assert_eq!(user.id, "17");

        let model: Model =
            serde_json::from_str(r#"{"id": 5, "tone_id": "2698", "name": "Clean"}"#).unwrap();
        assert_eq!((model.id, model.tone_id), (5, 2698));

        assert!(serde_json::from_str::<Tone>(r#"{"id": "not-a-number"}"#).is_err());
    }

    /// Shape of a real `/tones/trending` entry: `sizes` and `description`
    /// can be `null`, and extra fields must be ignored.
    #[test]
    fn accepts_nulls_in_documented_fields() {
        let json = r#"{
            "id": 1, "user_id": "u", "title": null, "description": null, "images": null,
            "license": "t3k", "models_count": 3, "sizes": null, "a1_models_count": null,
            "a2_models_count": 3, "makes": [], "tags": [], "is_favorite": false,
            "user": {"id": "u", "username": "x", "avatar_url": null, "display_name": null,
                     "is_verified": null, "url": null},
            "url": "https://www.tone3000.com/tones/x-1", "format": "nam", "gear": "amp"
        }"#;
        let tone: Tone = serde_json::from_str(json).unwrap();
        assert!(tone.sizes.is_empty());
        assert_eq!(tone.title, "");
        assert_eq!(tone.preferred_architecture(), Some("2"));

        let page: Page<Tone> = serde_json::from_str(r#"{"data": null}"#).unwrap();
        assert!(page.data.is_empty());
    }

    #[test]
    fn verified_creator_shows_display_name() {
        let user = EmbeddedUser {
            username: "amalgamaudio".into(),
            display_name: Some("Amalgam Audio".into()),
            ..Default::default()
        };
        assert_eq!(user.shown_name(), "Amalgam Audio");
    }

    #[test]
    fn parses_paginated_and_plain_lists() {
        let paginated =
            r#"{"data": [{"id": 1}], "page": 1, "page_size": 10, "total": 1, "total_pages": 1}"#;
        let plain = r#"{"data": [{"id": 2}, {"id": 3}]}"#;
        let a: Page<Tone> = serde_json::from_str(paginated).unwrap();
        let b: Page<Tone> = serde_json::from_str(plain).unwrap();
        assert_eq!(a.data.len(), 1);
        assert_eq!(b.data.len(), 2);
    }

    #[test]
    fn ir_tones_request_no_architecture() {
        let tone = Tone {
            format: "ir".into(),
            ..Default::default()
        };
        assert_eq!(tone.preferred_architecture(), None);
    }
}
