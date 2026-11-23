//! Response types, limited to the fields the app uses. Unknown fields are
//! ignored and most fields are optional, since Spotify omits or nulls
//! fields depending on the market, the item type and the app's access mode.

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Image {
    pub url: String,
    #[serde(default)]
    pub width: Option<u32>,
}

/// Smallest image at least `min_px` wide (Spotify lists them large first),
/// or the last one if none is that big.
pub fn pick_image(images: &[Image], min_px: u32) -> Option<&str> {
    images
        .iter()
        .filter(|i| i.width.is_none_or(|w| w >= min_px))
        .min_by_key(|i| i.width.unwrap_or(u32::MAX))
        .or(images.last())
        .map(|i| i.url.as_str())
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ExternalUrls {
    #[serde(default)]
    pub spotify: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Paging<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub total: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct User {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ItemsRef {
    #[serde(default)]
    pub total: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub uri: String,
    #[serde(default)]
    pub owner: Option<User>,
    #[serde(default)]
    pub collaborative: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    pub images: Vec<Image>,
    #[serde(default)]
    pub external_urls: ExternalUrls,
    /// Track count (`items` since February 2026, formerly `tracks`).
    #[serde(default)]
    pub items: Option<ItemsRef>,
}

impl Playlist {
    pub fn track_count(&self) -> Option<u32> {
        self.items.as_ref().and_then(|i| i.total)
    }

    /// Spotify only returns the tracks of playlists the user owns or
    /// collaborates on.
    pub fn tracks_readable_by(&self, user_id: &str) -> bool {
        self.collaborative || self.owner.as_ref().is_some_and(|o| o.id == user_id)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Artist {
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Album {
    #[serde(default)]
    pub name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub images: Vec<Image>,
}

/// A track (or, for `kind == "episode"`, a podcast episode, which the app
/// ignores).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Track {
    /// `None` for local files.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub artists: Vec<Artist>,
    #[serde(default)]
    pub album: Album,
    #[serde(default)]
    pub external_urls: ExternalUrls,
    #[serde(default)]
    pub is_local: bool,
    #[serde(rename = "type", default)]
    pub kind: String,
}

impl Track {
    pub fn artist_names(&self) -> Vec<String> {
        self.artists.iter().map(|a| a.name.clone()).collect()
    }

    pub fn artists_label(&self) -> String {
        self.artist_names().join(", ")
    }

    pub fn is_playable_track(&self) -> bool {
        self.kind == "track" && !self.is_local && self.id.is_some()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct PlaylistItem {
    /// `None` when the track is unavailable.
    #[serde(default)]
    pub item: Option<Track>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Device {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_restricted: bool,
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Devices {
    #[serde(default)]
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct PlaybackState {
    #[serde(default)]
    pub device: Option<Device>,
    #[serde(default)]
    pub is_playing: bool,
    #[serde(default)]
    pub progress_ms: Option<u64>,
    /// Track or episode; `None` during ads or when nothing is loaded.
    #[serde(default)]
    pub item: Option<Track>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ErrorDetail {
    #[serde(default)]
    pub status: u16,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// OAuth errors from the accounts service use a different shape.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct OAuthErrorBody {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_page_parses_with_nulls_and_new_items_field() {
        let json = r#"{
            "items": [
                {"id": "37i9dQZF1DXcBWIGoYBM5M", "name": "Rock", "uri": "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M",
                 "owner": {"id": "breno", "display_name": "Breno"}, "collaborative": false,
                 "images": null, "external_urls": {"spotify": "https://open.spotify.com/playlist/x"},
                 "items": {"href": "https://api.spotify.com/v1/playlists/x/items", "total": 12},
                 "public": true, "snapshot_id": "abc", "type": "playlist"}
            ],
            "next": null, "total": 1, "limit": 50, "offset": 0
        }"#;
        let page: Paging<Playlist> = serde_json::from_str(json).unwrap();
        let playlist = &page.items[0];
        assert_eq!(playlist.track_count(), Some(12));
        assert!(playlist.images.is_empty());
        assert!(playlist.tracks_readable_by("breno"));
        assert!(!playlist.tracks_readable_by("someone_else"));
    }

    #[test]
    fn playlist_items_parse_tracks_episodes_and_unavailable_entries() {
        let json = r#"{
            "items": [
                {"added_at": "2024-01-01T00:00:00Z", "is_local": false, "item": {
                    "id": "4uLU6hMCjMI75M1A2tKUQC", "name": "Back In Black", "uri": "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
                    "duration_ms": 255493, "type": "track", "is_local": false,
                    "artists": [{"name": "AC/DC", "id": "x"}],
                    "album": {"name": "Back In Black", "images": [{"url": "https://i.scdn.co/image/a", "width": 640, "height": 640}]},
                    "external_urls": {"spotify": "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC"}}},
                {"item": {"id": "ep1", "name": "A podcast", "uri": "spotify:episode:ep1", "type": "episode"}},
                {"item": null}
            ],
            "next": "https://api.spotify.com/v1/playlists/x/items?offset=50&limit=50"
        }"#;
        let page: Paging<PlaylistItem> = serde_json::from_str(json).unwrap();
        assert_eq!(page.items.len(), 3);
        let track = page.items[0].item.as_ref().unwrap();
        assert!(track.is_playable_track());
        assert_eq!(track.artists_label(), "AC/DC");
        assert!(!page.items[1].item.as_ref().unwrap().is_playable_track());
        assert!(page.items[2].item.is_none());
        assert!(page.next.is_some());
    }

    #[test]
    fn playback_state_parses() {
        let json = r#"{"device": {"id": "d1", "name": "Celular", "is_active": true, "type": "Smartphone"},
            "is_playing": true, "progress_ms": 1200, "currently_playing_type": "track",
            "item": {"id": "t", "name": "One", "uri": "spotify:track:t", "type": "track", "artists": [], "album": {"name": "x", "images": []}}}"#;
        let state: PlaybackState = serde_json::from_str(json).unwrap();
        assert!(state.is_playing);
        assert_eq!(state.device.unwrap().name, "Celular");
        assert_eq!(state.item.unwrap().name, "One");
    }

    #[test]
    fn picks_the_smallest_image_that_is_big_enough() {
        let images = vec![
            Image {
                url: "big".into(),
                width: Some(640),
            },
            Image {
                url: "mid".into(),
                width: Some(300),
            },
            Image {
                url: "small".into(),
                width: Some(64),
            },
        ];
        assert_eq!(pick_image(&images, 60), Some("small"));
        assert_eq!(pick_image(&images, 100), Some("mid"));
        assert_eq!(pick_image(&images, 1000), Some("small"));
        assert_eq!(pick_image(&[], 10), None);
    }
}
