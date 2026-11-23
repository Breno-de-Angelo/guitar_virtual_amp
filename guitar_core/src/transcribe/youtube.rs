//! YouTube as an audio source for transcription, through yt-dlp (public
//! domain/Unlicense) run as an external program: search without an API
//! key, and download a video's audio track as m4a (which Symphonia and
//! Demucs both read). `scripts/install_ytdlp.sh` installs it next to Demucs.
//!
//! YouTube's Terms of Service only allow downloading where YouTube offers
//! it or with permission from YouTube and the rights holders; the user is
//! responsible for what they download. Audio is kept in the app's private
//! cache (`<data dir>/guitar_virtual_amp/youtube_audio/`) for playback
//! against the tab, never in the tab repository.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;

use crate::shared::paths::app_data_dir;
use crate::shared::tab::library::{clean_title, words};

/// The `yt-dlp` executable: `GVA_YTDLP`, else the app's own environment
/// (`<data dir>/guitar_virtual_amp/tools/ytdlp-venv`), else `yt-dlp` on `PATH`.
pub fn find_ytdlp() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("GVA_YTDLP") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let bundled = app_data_dir().map(|d| {
        let venv = d.join("tools").join("ytdlp-venv");
        if cfg!(windows) {
            venv.join("Scripts").join("yt-dlp.exe")
        } else {
            venv.join("bin").join("yt-dlp")
        }
    });
    if let Some(path) = bundled.filter(|p| p.is_file()) {
        return Some(path);
    }
    let name = if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

pub fn audio_cache_dir() -> Option<PathBuf> {
    app_data_dir().map(|d| d.join("youtube_audio"))
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Thumbnail {
    pub url: String,
    #[serde(default)]
    pub width: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Video {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub thumbnails: Vec<Thumbnail>,
    #[serde(default)]
    pub live_status: Option<String>,
}

impl Video {
    pub fn url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }

    /// A small thumbnail (YouTube's standard one, always present for an id).
    pub fn thumbnail_url(&self) -> String {
        format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", self.id)
    }

    pub fn channel_name(&self) -> &str {
        self.channel.as_deref().unwrap_or("")
    }

    fn is_live(&self) -> bool {
        matches!(
            self.live_status.as_deref(),
            Some("is_live" | "is_upcoming" | "post_live")
        )
    }

    /// `(artist, title)` for the tab: "Artist - Topic" channels (YouTube
    /// Music's official audio) name the artist; else "Artist - Title"
    /// titles, ignoring bracketed tags and "| extra" tails; else the channel.
    pub fn guess_artist_title(&self) -> (String, String) {
        let channel = self.channel_name();
        let title = strip_decorations(&self.title);
        if let Some(artist) = channel.strip_suffix(" - Topic") {
            return (artist.trim().to_string(), clean_title(&title));
        }
        match title.split_once(" - ") {
            Some((artist, rest)) if !artist.trim().is_empty() => {
                (artist.trim().to_string(), clean_title(rest))
            }
            _ => (channel.trim().to_string(), clean_title(&title)),
        }
    }
}

/// A video title without `[tags]`, `(notes)` and anything after " | ".
fn strip_decorations(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut depth = 0i32;
    for c in title.chars() {
        match c {
            '(' | '[' | '【' => depth += 1,
            ')' | ']' | '】' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let cut = out.split(" | ").next().unwrap_or("").to_string();
    cut.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// YouTube video IDs: 11 characters of `[A-Za-z0-9_-]`.
pub fn is_valid_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The video ID in a pasted link (watch, youtu.be, shorts, music, embed)
/// or a bare ID. `None` for anything else (e.g. a search query).
pub fn parse_video_id(input: &str) -> Option<String> {
    let input = input.trim();
    if is_valid_id(input) {
        return Some(input.to_string());
    }
    let rest = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.trim_start_matches("www.").trim_start_matches("m.");
    let candidate = match host {
        "youtu.be" => path.split(['?', '&', '#', '/']).next(),
        "youtube.com" | "music.youtube.com" => {
            if let Some(query) = path.strip_prefix("watch?") {
                query.split('&').find_map(|kv| kv.strip_prefix("v="))
            } else {
                ["shorts/", "embed/", "live/", "v/"]
                    .iter()
                    .find_map(|p| path.strip_prefix(p))
                    .and_then(|r| r.split(['?', '&', '#', '/']).next())
            }
        }
        _ => None,
    }?;
    let id: String = candidate
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    is_valid_id(&id).then_some(id)
}

fn command(ytdlp: &Path) -> Command {
    let mut cmd = Command::new(ytdlp);
    cmd.arg("--no-warnings")
        .arg("--no-playlist")
        .stdin(Stdio::null());
    cmd
}

/// Runs a short yt-dlp query, returning stdout (kills it after `timeout`).
/// Both pipes are drained on their own threads while it runs: yt-dlp can
/// print more than a pipe buffer holds, and would block forever otherwise.
fn run_quick(mut cmd: Command, timeout: Duration) -> Result<String, String> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("não foi possível iniciar o yt-dlp: {e}"))?;
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    );
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("o YouTube demorou demais para responder (verifique a conexão)".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    if !status.success() {
        let line = err
            .lines()
            .rev()
            .find(|l| l.contains("ERROR"))
            .unwrap_or(err.trim());
        return Err(format!("yt-dlp: {}", line.trim()));
    }
    Ok(out)
}

/// Only the fields `Video` reads, as one JSON object per line (a full
/// `--dump-json` of one video is over 100 KB of formats and thumbnails).
const VIDEO_FIELDS: &str = "%(.{id,title,channel,duration,live_status})j";

fn parse_videos(json_lines: &str) -> Vec<Video> {
    json_lines
        .lines()
        .filter_map(|l| serde_json::from_str::<Video>(l).ok())
        .filter(|v| is_valid_id(&v.id) && !v.is_live() && v.duration.is_some_and(|d| d > 0.0))
        .collect()
}

/// Up to `count` videos for `query` (no API key; yt-dlp's `ytsearch`).
pub fn search(ytdlp: &Path, query: &str, count: usize) -> Result<Vec<Video>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("digite o que procurar".into());
    }
    let mut cmd = command(ytdlp);
    cmd.arg("--flat-playlist")
        .arg("--print")
        .arg(VIDEO_FIELDS)
        .arg(format!("ytsearch{}:{query}", count.clamp(1, 30)));
    Ok(parse_videos(&run_quick(cmd, Duration::from_secs(60))?))
}

/// Details of one video (for a pasted link).
pub fn video_info(ytdlp: &Path, id: &str) -> Result<Video, String> {
    if !is_valid_id(id) {
        return Err("link do YouTube inválido".into());
    }
    let mut cmd = command(ytdlp);
    cmd.arg("--skip-download")
        .arg("--print")
        .arg(VIDEO_FIELDS)
        .arg(format!("https://www.youtube.com/watch?v={id}"));
    parse_videos(&run_quick(cmd, Duration::from_secs(60))?)
        .into_iter()
        .next()
        .ok_or_else(|| "vídeo não encontrado ou ao vivo".into())
}

/// How likely `video` is the studio recording of the song, higher first.
pub fn match_score(video: &Video, title: &str, artist: &str, duration_hint: Option<f64>) -> f64 {
    let video_words = words(&format!("{} {}", video.title, video.channel_name()));
    let share = |text: &str| {
        let w = words(text);
        if w.is_empty() {
            1.0
        } else {
            w.iter().filter(|x| video_words.contains(x)).count() as f64 / w.len() as f64
        }
    };
    let mut score = 2.0 * share(&clean_title(title)) + share(artist);
    if video.channel_name().ends_with(" - Topic") {
        score += 1.0; // YouTube Music's official audio
    }
    let lower = video.title.to_lowercase();
    let wanted = format!("{title} {artist}").to_lowercase();
    for bad in [
        "live",
        "ao vivo",
        "en vivo",
        "cover",
        "karaoke",
        "lesson",
        "aula",
        "tutorial",
        "reaction",
        "slowed",
        "sped up",
        "nightcore",
        "8d",
        "remix",
        "instrumental",
        "backing track",
    ] {
        if lower.contains(bad) && !wanted.contains(bad) {
            score -= 2.5;
        }
    }
    if lower.contains("official audio")
        || lower.contains("áudio oficial")
        || lower.contains("audio oficial")
    {
        score += 0.5;
    }
    if let (Some(hint), Some(d)) = (duration_hint, video.duration) {
        let diff = (d - hint).abs();
        score -= if diff <= 3.0 {
            0.0
        } else {
            (diff / 10.0).min(3.0)
        };
    }
    score
}

/// Videos sorted by [`match_score`].
pub fn rank(
    mut videos: Vec<Video>,
    title: &str,
    artist: &str,
    duration_hint: Option<f64>,
) -> Vec<Video> {
    videos.sort_by(|a, b| {
        match_score(b, title, artist, duration_hint).total_cmp(&match_score(
            a,
            title,
            artist,
            duration_hint,
        ))
    });
    videos
}

/// `[download]  41.3% of ...` -> 0.413.
pub(crate) fn parse_download_progress(line: &str) -> Option<f32> {
    let rest = line.trim_start().strip_prefix("[download]")?.trim_start();
    let percent = rest.split('%').next()?.trim();
    percent
        .parse::<f32>()
        .ok()
        .map(|p| (p / 100.0).clamp(0.0, 1.0))
}

/// Downloads the audio of video `id` into `dir` as `<id>.m4a` (reusing a
/// previous download). Blocks; `cancel` kills yt-dlp.
pub fn download_audio(
    ytdlp: &Path,
    id: &str,
    dir: &Path,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    if !is_valid_id(id) {
        return Err("link do YouTube inválido".into());
    }
    let target = dir.join(format!("{id}.m4a"));
    if target.is_file() {
        progress(1.0);
        return Ok(target);
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("não foi possível criar {}: {e}", dir.display()))?;
    let mut child = command(ytdlp)
        .arg("--newline")
        .arg("--progress")
        .arg("-f")
        .arg("bestaudio[ext=m4a]/bestaudio")
        .arg("-x")
        .arg("--audio-format")
        .arg("m4a")
        .arg("-o")
        .arg(dir.join("%(id)s.%(ext)s"))
        .arg("--print")
        .arg("after_move:filepath")
        .arg(format!("https://www.youtube.com/watch?v={id}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("não foi possível iniciar o yt-dlp: {e}"))?;
    let stdout = child.stdout.take().ok_or("sem saída do yt-dlp")?;
    let mut stderr = child.stderr.take().ok_or("sem saída do yt-dlp")?;
    let (tx, rx) = crossbeam::channel::unbounded::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let err_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
        text
    });
    let mut last_line = String::new();
    let status = loop {
        for line in rx.try_iter() {
            if let Some(p) = parse_download_progress(&line) {
                progress(p * 0.95);
            } else if !line.trim().is_empty() {
                last_line = line;
            }
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelado".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let _ = reader.join();
    for line in rx.try_iter() {
        if parse_download_progress(&line).is_none() && !line.trim().is_empty() {
            last_line = line;
        }
    }
    let errors = err_reader.join().unwrap_or_default();
    if !status.success() {
        let line = errors
            .lines()
            .rev()
            .find(|l| l.contains("ERROR"))
            .unwrap_or(errors.trim());
        return Err(format!("o download falhou: {}", line.trim()));
    }
    let path = PathBuf::from(last_line.trim());
    let path = if path.is_file() { path } else { target };
    if !path.is_file() {
        return Err("o yt-dlp terminou mas o arquivo de áudio não apareceu".into());
    }
    progress(1.0);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(title: &str, channel: &str, duration: f64) -> Video {
        Video {
            id: "abcdefghijk".into(),
            title: title.into(),
            channel: Some(channel.into()),
            duration: Some(duration),
            ..Default::default()
        }
    }

    #[test]
    fn parses_ids_from_links() {
        for input in [
            "https://www.youtube.com/watch?v=M4uG1MmZVl0",
            "https://youtube.com/watch?feature=share&v=M4uG1MmZVl0&t=10",
            "https://youtu.be/M4uG1MmZVl0?si=xyz",
            "https://music.youtube.com/watch?v=M4uG1MmZVl0&list=RD",
            "https://m.youtube.com/shorts/M4uG1MmZVl0",
            "M4uG1MmZVl0",
        ] {
            assert_eq!(
                parse_video_id(input).as_deref(),
                Some("M4uG1MmZVl0"),
                "{input}"
            );
        }
        assert_eq!(parse_video_id("legiao urbana tempo perdido"), None);
        assert_eq!(
            parse_video_id("https://evil.example/watch?v=M4uG1MmZVl0"),
            None
        );
        assert_eq!(parse_video_id("https://youtu.be/../../etc"), None);
        assert!(!is_valid_id("M4uG1MmZVl0; rm"));
    }

    #[test]
    fn search_output_is_parsed_and_live_streams_dropped() {
        let lines = concat!(
            r#"{"id": "M4uG1MmZVl0", "title": "Tempo Perdido", "channel": "Legião Urbana - Topic", "duration": 303.0, "thumbnails": [{"url": "https://i.ytimg.com/x.jpg"}]}"#,
            "\n",
            r#"{"id": "livestream1", "title": "Radio 24/7", "channel": "x", "duration": null, "live_status": "is_live"}"#,
            "\nnot json\n",
            r#"{"id": "bad", "title": "t", "duration": 10}"#,
        );
        let videos = parse_videos(lines);
        assert_eq!(videos.len(), 1);
        assert_eq!(
            videos[0].guess_artist_title(),
            ("Legião Urbana".into(), "Tempo Perdido".into())
        );
        assert_eq!(
            videos[0].url(),
            "https://www.youtube.com/watch?v=M4uG1MmZVl0"
        );
        let tagged = video(
            "[No Copyright Music] Seven - Acoustic Folk | Travel Music",
            "Oak Studios",
            157.0,
        );
        assert_eq!(
            tagged.guess_artist_title(),
            ("Seven".into(), "Acoustic Folk".into())
        );
        let plain = video("Legião Urbana - Tempo Perdido (Ao Vivo) [HD]", "x", 1.0);
        assert_eq!(
            plain.guess_artist_title(),
            ("Legião Urbana".into(), "Tempo Perdido".into())
        );
        let bare = video("Iara's Song | Chill Music", "Oak Studios", 1.0);
        assert_eq!(
            bare.guess_artist_title(),
            ("Oak Studios".into(), "Iara's Song".into())
        );
    }

    #[test]
    fn ranking_prefers_the_studio_recording() {
        let candidates = vec![
            video(
                "Legião Urbana - Tempo Perdido (Ao Vivo Especial)",
                "Musicalidade",
                235.0,
            ),
            video("Tempo Perdido - cover no violão", "Fulano", 300.0),
            video("Tempo Perdido", "Legião Urbana - Topic", 303.0),
            video("Legião Urbana - Tempo Perdido (Letra)", "legendas", 229.0),
        ];
        let ranked = rank(
            candidates,
            "Tempo Perdido - Remastered",
            "Legião Urbana",
            Some(302.0),
        );
        assert_eq!(ranked[0].channel_name(), "Legião Urbana - Topic");
        // The lyrics video is the studio audio too; cover and live go last.
        assert_eq!(ranked[1].channel_name(), "legendas");
        let bottom: Vec<&str> = ranked[2..].iter().map(|v| v.channel_name()).collect();
        assert!(
            bottom.contains(&"Fulano") && bottom.contains(&"Musicalidade"),
            "{bottom:?}"
        );
        // Asking for the live version keeps it in the running.
        let live = video("Legião Urbana - Tempo Perdido (Ao Vivo)", "x", 235.0);
        assert!(
            match_score(&live, "Tempo Perdido ao vivo", "Legião Urbana", None)
                > match_score(&live, "Tempo Perdido", "Legião Urbana", None)
        );
    }

    /// A stand-in yt-dlp that prints far more than a pipe buffer before
    /// exiting: `run_quick` must not deadlock on it.
    #[cfg(unix)]
    #[test]
    fn large_outputs_do_not_block_the_process() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("gva_fake_ytdlp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("yt-dlp");
        let line = r#"{"id": "M4uG1MmZVl0", "title": "Tempo Perdido", "channel": "Legião Urbana - Topic", "duration": 303.0}"#;
        let padding = "x".repeat(300_000);
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho '{line}'\necho '{padding}'\necho '{padding}' >&2\n"),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let video = video_info(&fake, "M4uG1MmZVl0").unwrap();
        assert_eq!(video.title, "Tempo Perdido");
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_download_progress_lines() {
        assert_eq!(
            parse_download_progress("[download]  41.3% of    2.42MiB at 7.83MiB/s ETA 00:00"),
            Some(0.413)
        );
        assert_eq!(
            parse_download_progress("[download] 100% of    2.42MiB in 00:00:00"),
            Some(1.0)
        );
        assert_eq!(
            parse_download_progress("[ExtractAudio] Destination: x.m4a"),
            None
        );
        assert_eq!(parse_download_progress("/home/x/abc.m4a"), None);
    }

    #[test]
    fn existing_downloads_are_reused_without_running_ytdlp() {
        let dir = std::env::temp_dir().join(format!("gva_yt_cache_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("abcdefghijk.m4a"), b"x").unwrap();
        let path = download_audio(
            Path::new("/nonexistent/yt-dlp"),
            "abcdefghijk",
            &dir,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(path, dir.join("abcdefghijk.m4a"));
        assert!(
            download_audio(
                Path::new("/nonexistent/yt-dlp"),
                "../../etc/x",
                &dir,
                &mut |_| {},
                &AtomicBool::new(false)
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
