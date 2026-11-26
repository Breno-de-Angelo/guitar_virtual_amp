//! The user's own tab library: a folder of Guitar Pro files, matched to
//! songs (e.g. a Spotify track) by file name.
//!
//! The app doesn't download tablature from anywhere: there's no licensed
//! tab API to use, and the Spotify Developer Policy forbids mixing Spotify
//! content with other services' content. Tabs come from files the user owns.
//! Matching is by file name ("Artist - Title.gp5" and similar), which is how
//! most tab collections are named, and by the title and artist stored inside
//! the file (read once and cached, see [`read_tab_info`]); a manual link
//! covers the rest.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::import::SUPPORTED_EXTENSIONS;

/// Limits so pointing the library at `/` can't hang the scan.
const MAX_DEPTH: usize = 8;
const MAX_FILES: usize = 50_000;

/// Recursively lists the Guitar Pro files under `dir`, sorted. Hidden
/// folders and symlinked folders are skipped.
pub fn scan_library(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((folder, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_FILES {
                found.sort();
                return found;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if kind.is_dir() {
                if !hidden && depth + 1 < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if !hidden && is_tab_file(&path) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn is_tab_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn fold_char(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'ø' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ý' | 'ÿ' => 'y',
        other => other,
    }
}

/// Lowercase, accent-free words of `text`, with `'` dropped so "Don't" and
/// "Dont" compare equal.
pub fn words(text: &str) -> Vec<String> {
    let folded: String = text
        .to_lowercase()
        .chars()
        .map(fold_char)
        .filter(|c| *c != '\'' && *c != '’')
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    folded.split_whitespace().map(str::to_string).collect()
}

/// A track title without the decorations streaming services add:
/// "Song - Remastered 2011", "Song (feat. X)", "Song [Live]".
pub fn clean_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut depth = 0i32;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let cut = out.find(" - ").map(|i| out[..i].to_string()).unwrap_or(out);
    cut.trim().to_string()
}

/// Lowest [`match_score`] treated as "this is the song's tab".
pub const MIN_MATCH_SCORE: f64 = 0.55;

/// How well a tab file's name fits a song, 0..=1. Every title word must
/// appear; artist words raise the score; extra words lower it.
pub fn match_score(file_stem: &str, title: &str, artists: &[String]) -> f64 {
    score_words(&words(file_stem), title, artists)
}

fn score_words(file_words: &[String], title: &str, artists: &[String]) -> f64 {
    let title_words = words(&clean_title(title));
    if title_words.is_empty() || file_words.is_empty() {
        return 0.0;
    }
    if !title_words.iter().all(|w| file_words.contains(w)) {
        return 0.0;
    }
    let artist_words: Vec<String> = artists.iter().flat_map(|a| words(a)).collect();
    let artist_hits = artist_words
        .iter()
        .filter(|w| file_words.contains(w))
        .count();
    let artist_part = if artist_words.is_empty() {
        0.0
    } else {
        artist_hits as f64 / artist_words.len() as f64
    };
    let explained = title_words.len() + artist_hits;
    let extra = file_words.len().saturating_sub(explained) as f64;
    (0.6 + 0.4 * artist_part - 0.1 * extra).clamp(0.05, 1.0)
}

/// Song title and artist stored inside a tab file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub title: String,
    pub artist: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct CacheEntry {
    modified_secs: u64,
    size: u64,
    /// `None` when the file didn't parse (so it isn't retried every scan).
    info: Option<TabInfo>,
}

/// `<data dir>/guitar_virtual_amp/tab_library_cache.json`.
pub fn library_cache_path() -> Option<PathBuf> {
    crate::shared::paths::app_data_dir().map(|d| d.join("tab_library_cache.json"))
}

fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((modified, meta.len()))
}

fn parse_info(path: &Path) -> Option<TabInfo> {
    let score = super::import::load_score_file(path).ok()?;
    Some(TabInfo { title: score.title, artist: score.artist })
}

/// Title and artist of each file, parsed in parallel for files the cache
/// at `cache_path` doesn't know (or that changed since), then saves the
/// cache. Blocking: worker threads only.
pub fn read_tab_info(files: &[PathBuf], cache_path: Option<&Path>) -> Vec<Option<TabInfo>> {
    let mut cache: BTreeMap<String, CacheEntry> = cache_path
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    let stamps: Vec<Option<(u64, u64)>> = files.iter().map(|f| file_stamp(f)).collect();
    let stale: Vec<usize> = (0..files.len())
        .filter(|&i| {
            let key = files[i].display().to_string();
            match (cache.get(&key), stamps[i]) {
                (Some(entry), Some((m, s))) => entry.modified_secs != m || entry.size != s,
                _ => true,
            }
        })
        .collect();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    let chunk = stale.len().div_ceil(threads.max(1)).max(1);
    let parsed: Vec<(usize, Option<TabInfo>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = stale
            .chunks(chunk)
            .map(|indices| scope.spawn(move || indices.iter().map(|&i| (i, parse_info(&files[i]))).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    let changed = !parsed.is_empty();
    for (i, info) in parsed {
        let (modified_secs, size) = stamps[i].unwrap_or((0, 0));
        cache.insert(files[i].display().to_string(), CacheEntry { modified_secs, size, info });
    }
    if changed && let Some(path) = cache_path {
        // Drop entries for files that are gone.
        let keep: std::collections::HashSet<String> = files.iter().map(|f| f.display().to_string()).collect();
        let pruned: BTreeMap<&String, &CacheEntry> = cache.iter().filter(|(k, _)| keep.contains(*k)).collect();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(&pruned) {
            let _ = std::fs::write(path, json);
        }
    }
    files.iter().map(|f| cache.get(&f.display().to_string()).and_then(|e| e.info.clone())).collect()
}

/// Library files with their names (and stored title/artist) already split
/// into words, so matching every visible track each frame stays cheap.
#[derive(Clone, Debug, Default)]
pub struct LibraryIndex {
    /// `(file, file name words, "artist title" words from inside the file)`.
    entries: Vec<(PathBuf, Vec<String>, Vec<String>)>,
}

impl LibraryIndex {
    pub fn new(files: Vec<PathBuf>) -> Self {
        let info = vec![None; files.len()];
        Self::with_info(files, info)
    }

    /// Like [`LibraryIndex::new`], also matching the title and artist read
    /// from each file (`info[i]` belongs to `files[i]`).
    pub fn with_info(files: Vec<PathBuf>, info: Vec<Option<TabInfo>>) -> Self {
        let entries = files
            .into_iter()
            .zip(info.into_iter().chain(std::iter::repeat(None)))
            .map(|(path, info)| {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let stem_words = words(&stem);
                let info_words = info.map(|i| words(&format!("{} {}", i.artist, i.title))).unwrap_or_default();
                (path, stem_words, info_words)
            })
            .collect();
        Self { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The best tab for the song, if one scores at least [`MIN_MATCH_SCORE`].
    pub fn best_match(&self, title: &str, artists: &[String]) -> Option<&PathBuf> {
        self.entries
            .iter()
            .map(|(path, stem, info)| (path, score_words(stem, title, artists).max(score_words(info, title, artists))))
            .filter(|(_, score)| *score >= MIN_MATCH_SCORE)
            .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(path, _)| path)
    }
}

/// Tab files that fit the song, best first.
pub fn find_matches<'a>(
    files: &'a [PathBuf],
    title: &str,
    artists: &[String],
) -> Vec<(&'a PathBuf, f64)> {
    let mut matches: Vec<(&PathBuf, f64)> = files
        .iter()
        .filter_map(|path| {
            let stem = path.file_stem()?.to_string_lossy();
            let score = match_score(&stem, title, artists);
            (score > 0.0).then_some((path, score))
        })
        .collect();
    matches.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_title_and_artist_match_badly_named_files() {
        let files = vec![PathBuf::from("/tabs/track01.gp5"), PathBuf::from("/tabs/other.gp5")];
        let info = vec![Some(TabInfo { title: "Back In Black".into(), artist: "AC/DC".into() }), None];
        let index = LibraryIndex::with_info(files.clone(), info);
        assert_eq!(index.best_match("Back In Black", &artists(&["AC/DC"])), Some(&files[0]));
        assert!(LibraryIndex::new(files).best_match("Back In Black", &artists(&["AC/DC"])).is_none());
    }

    #[test]
    fn tab_info_is_cached_by_modification_time() {
        let dir = std::env::temp_dir().join(format!("gva_lib_cache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.gp5");
        std::fs::write(&broken, b"not a guitar pro file").unwrap();
        let cache = dir.join("cache.json");
        assert_eq!(read_tab_info(std::slice::from_ref(&broken), Some(&cache)), vec![None]);
        let saved = std::fs::read_to_string(&cache).unwrap();
        assert!(saved.contains("broken.gp5"));
        // A cached entry is used as is: plant one and see it come back.
        let (m, s) = file_stamp(&broken).unwrap();
        let planted = BTreeMap::from([(
            broken.display().to_string(),
            CacheEntry { modified_secs: m, size: s, info: Some(TabInfo { title: "T".into(), artist: "A".into() }) },
        )]);
        std::fs::write(&cache, serde_json::to_string(&planted).unwrap()).unwrap();
        let info = read_tab_info(std::slice::from_ref(&broken), Some(&cache));
        assert_eq!(info[0].as_ref().map(|i| i.title.as_str()), Some("T"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn artists(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn words_ignore_case_accents_and_punctuation() {
        assert_eq!(words("Ainda É Cedo!"), vec!["ainda", "e", "cedo"]);
        assert_eq!(words("Don't Stop Me Now"), words("dont stop me now"));
        assert_eq!(
            words("Guns N' Roses - Sweet_Child"),
            vec!["guns", "n", "roses", "sweet", "child"]
        );
    }

    #[test]
    fn titles_lose_streaming_decorations() {
        assert_eq!(
            clean_title("Smells Like Teen Spirit - Remastered 2021"),
            "Smells Like Teen Spirit"
        );
        assert_eq!(clean_title("Stay (feat. Someone) [Live]"), "Stay");
        assert_eq!(clean_title("Back In Black"), "Back In Black");
    }

    #[test]
    fn artist_and_title_beat_title_only() {
        let title = "Back In Black - 2003 Remaster";
        let band = artists(&["AC/DC"]);
        let full = match_score("ACDC - Back In Black", title, &band);
        let exact = match_score("AC DC - Back In Black", title, &band);
        let only_title = match_score("Back in Black (cover)", title, &band);
        assert!(exact > only_title, "{exact} vs {only_title}");
        assert!(full > 0.0);
        assert_eq!(match_score("Highway to Hell", title, &band), 0.0);
    }

    #[test]
    fn best_match_comes_first() {
        let files = vec![
            PathBuf::from("/tabs/Metallica - One.gp5"),
            PathBuf::from("/tabs/U2 - One.gp4"),
            PathBuf::from("/tabs/Metallica - Fade To Black.gp5"),
        ];
        let matches = find_matches(&files, "One", &artists(&["Metallica"]));
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].0, &files[0]);

        let index = LibraryIndex::new(files.clone());
        assert_eq!(
            index.best_match("One - Remastered", &artists(&["Metallica"])),
            Some(&files[0])
        );
        // A different band's song with the same title isn't good enough.
        let only_u2 = LibraryIndex::new(vec![files[1].clone()]);
        assert_eq!(only_u2.best_match("One", &artists(&["Metallica"])), None);
        // Neither is a longer title that merely contains the word.
        let longer = LibraryIndex::new(vec![PathBuf::from("/t/One Step Closer.gp5")]);
        assert_eq!(longer.best_match("One", &[]), None);
        assert!(
            LibraryIndex::new(vec![PathBuf::from("/t/One.gp5")])
                .best_match("One", &[])
                .is_some()
        );
    }

    #[test]
    fn scan_finds_tab_files_recursively_and_skips_hidden() {
        let root = std::env::temp_dir().join(format!("gva_tab_library_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("rock/ac dc")).unwrap();
        std::fs::create_dir_all(root.join(".cache")).unwrap();
        for file in [
            "rock/ac dc/Back In Black.GP5",
            "One.gp",
            "notes.txt",
            ".cache/x.gp5",
        ] {
            std::fs::write(root.join(file), b"").unwrap();
        }
        let found = scan_library(&root);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names.contains(&"Back In Black.GP5".to_string()));
        assert!(names.contains(&"One.gp".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }
}
