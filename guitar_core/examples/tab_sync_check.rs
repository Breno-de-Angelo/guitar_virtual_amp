//! Checks a tab written by `scripts/train/align_tabs.py` the way the Tabs
//! page will use it: loads the tab with the app's importer, builds the
//! timeline, reads the `.gvtab.json` sidecar through `TabMetadata` and maps
//! the score to the recording with `TabMetadata::sync_map`.
//!
//! Optionally compares the app's note-ons of a track, mapped to recording
//! time, with an aligned notes file (`onset offset pitch ...` lines): for
//! each file note, the closest app note of the same pitch.
//!
//! Prints one JSON object per line: the playback order (bar index of every
//! occurrence, to compare with `gp_tabs.playback_order`), the sync summary,
//! then one per `<track index> <notes file>` pair.
//!
//! ```text
//! cargo run --release --example tab_sync_check -- check/<id>.gp5 [0 pairs/<id>/notes/1_Guitar.txt]...
//! ```

use std::path::Path;

use guitar_core::shared::tab::import::load_score_file;
use guitar_core::shared::tab::repository::load_metadata;
use guitar_core::shared::tab::timeline::Timeline;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(tab_path) = args.first().map(Path::new) else {
        eprintln!("usage: tab_sync_check <tab> [<track index> <notes file>]...");
        std::process::exit(2);
    };
    let score = load_score_file(tab_path).unwrap_or_else(|e| fail(&format!("tab: {e}")));
    let timeline = Timeline::build(&score);
    let order: Vec<usize> = timeline.occurrences.iter().map(|o| o.bar).collect();
    println!("{{\"playback_order\":{order:?},\"duration_secs\":{:.3}}}", timeline.duration_secs);
    let meta = load_metadata(tab_path).unwrap_or_else(|| fail("sidecar missing or not a TabMetadata"));
    let sync = meta
        .sync_map(&timeline)
        .unwrap_or_else(|| fail("sidecar gives fewer than 2 usable bar times"));
    let audio_ok = meta.source.path.as_ref().is_some_and(|p| p.is_file());
    let first_bar = sync.score_to_audio(0.0);
    let end = sync.score_to_audio(timeline.duration_secs);
    println!(
        "{{\"tab\":{:?},\"bars\":{},\"occurrences\":{},\"sidecar_bar_times\":{},\"audio_found\":{},\"embedded_audio\":{},\"first_bar_audio\":{:.3},\"end_audio\":{:.3},\"tracks\":{}}}",
        tab_path.display().to_string(),
        score.bars.len(),
        timeline.occurrences.len(),
        meta.bar_audio_secs.len(),
        audio_ok,
        score.backing_audio.is_some(),
        first_bar,
        end,
        score.tracks.len()
    );
    for pair in args[1..].chunks(2) {
        let [track, file] = pair else {
            fail("expected <track index> <notes file> pairs");
        };
        let track: u16 = track.parse().unwrap_or_else(|_| fail("track index"));
        let app: Vec<(f64, u8)> = timeline
            .events
            .iter()
            .filter(|e| e.track == track && e.velocity > 0)
            .map(|e| (sync.score_to_audio(e.secs), e.key))
            .collect();
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| fail(&format!("{file}: {e}")));
        let notes: Vec<(f64, u8)> = text
            .lines()
            .filter_map(|l| {
                let v: Vec<&str> = l.split_whitespace().collect();
                Some((v.first()?.parse().ok()?, v.get(2)?.parse().ok()?))
            })
            .collect();
        let mut diffs: Vec<f64> = notes
            .iter()
            .filter_map(|&(t, p)| {
                app.iter()
                    .filter(|a| a.1 == p)
                    .map(|a| (a.0 - t).abs())
                    .min_by(|a, b| a.total_cmp(b))
            })
            .collect();
        diffs.sort_by(|a, b| a.total_cmp(b));
        let within = |s: f64| diffs.iter().filter(|d| **d <= s).count();
        let median = diffs.get(diffs.len() / 2).copied().unwrap_or(f64::NAN);
        println!(
            "{{\"track\":{track},\"file\":{file:?},\"app_notes\":{},\"file_notes\":{},\"within_5ms\":{},\"within_30ms\":{},\"median_abs_ms\":{:.1}}}",
            app.len(),
            notes.len(),
            within(0.005),
            within(0.03),
            median * 1000.0
        );
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("tab_sync_check: {msg}");
    std::process::exit(1);
}
