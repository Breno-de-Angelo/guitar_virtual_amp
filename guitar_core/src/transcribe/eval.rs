//! Quality benchmark for the transcription pipeline: renders Guitar Pro
//! files with a SoundFont, transcribes the audio, and compares the result
//! with the original tab. Ignored by default (needs files that aren't in
//! the repo); run with
//!
//! ```text
//! GVA_EVAL_TABS=/path/to/gp/files GVA_SOUNDFONT=/path/to/gm.sf2 \
//!   cargo test --release --workspace --exclude guitar_android eval_transcription -- --ignored --nocapture
//! ```
//!
//! Optional: `GVA_EVAL_SECS` (seconds per song, default 40),
//! `GVA_EVAL_DEMUCS=1` to also run the full mix through Demucs,
//! `GVA_EVAL_PITCHES=1` to print pitch histograms (octave errors show up
//! there) and `GVA_EVAL_CACHE=<dir>` to reuse transcribed notes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

use super::{Request, run};
use crate::shared::tab::import::load_score_file;
use crate::shared::tab::model::Score;
use crate::shared::tab::repository::load_metadata;
use crate::shared::tab::timeline::Timeline;

const RATE: u32 = 44_100;

/// `GVA_EVAL_CACHE=<dir>` keeps each stem's transcribed notes (keyed by the
/// audio and the engine settings), so benchmarks of the later stages
/// (beats, quantization, fingering) don't rerun the models.
fn cache_file(audio: &super::Mono, engine: &super::Engine) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let dir = PathBuf::from(std::env::var("GVA_EVAL_CACHE").ok()?);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    audio.rate.hash(&mut h);
    audio.samples.len().hash(&mut h);
    for s in audio.samples.iter().step_by(97) {
        s.to_bits().hash(&mut h);
    }
    format!("{engine:?}").hash(&mut h);
    Some(dir.join(format!("{:016x}.notes", h.finish())))
}

pub(super) fn cached_notes(
    audio: &super::Mono,
    engine: &super::Engine,
) -> Option<Vec<super::NoteEstimate>> {
    let text = std::fs::read_to_string(cache_file(audio, engine)?).ok()?;
    text.lines()
        .map(|line| {
            let v: Vec<&str> = line.split(' ').collect();
            Some(super::NoteEstimate {
                start: v.first()?.parse().ok()?,
                end: v.get(1)?.parse().ok()?,
                pitch: v.get(2)?.parse().ok()?,
                amplitude: v.get(3)?.parse().ok()?,
            })
        })
        .collect()
}

pub(super) fn cache_notes(
    audio: &super::Mono,
    engine: &super::Engine,
    notes: &[super::NoteEstimate],
) {
    let Some(path) = cache_file(audio, engine) else {
        return;
    };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let text: String = notes
        .iter()
        .map(|n| format!("{} {} {} {}\n", n.start, n.end, n.pitch, n.amplitude))
        .collect();
    let _ = std::fs::write(path, text);
}

/// `(time, pitch, string, fret)` of every picked note of `track`.
fn tab_notes(score: &Score, track: usize, map: &dyn Fn(f64) -> f64) -> Vec<(f64, u8, u8, i16)> {
    let timeline = Timeline::build(score);
    let t = &score.tracks[track];
    let mut out = Vec::new();
    for occ in &timeline.occurrences {
        let Some(bar) = t.bars.get(occ.bar) else {
            continue;
        };
        for voice in &bar.voices {
            for beat in voice {
                for n in beat.notes.iter().filter(|n| !n.tie && !n.dead) {
                    let open = t.tuning.get(n.string as usize - 1).copied().unwrap_or(40) as i32;
                    let pitch = (open + t.capo + n.fret as i32).clamp(0, 127) as u8;
                    out.push((map(occ.tick_to_secs(beat.start)), pitch, n.string, n.fret));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// Note F1 (onset within `tol`, same pitch) and, among matches, the share
/// on the same string and fret.
fn compare(
    truth: &[(f64, u8, u8, i16)],
    est: &[(f64, u8, u8, i16)],
    tol: f64,
) -> (f64, f64, f64, f64) {
    let mut used = vec![false; est.len()];
    let (mut tp, mut same_place) = (0usize, 0usize);
    for t in truth {
        if let Some(j) =
            (0..est.len()).find(|&j| !used[j] && est[j].1 == t.1 && (est[j].0 - t.0).abs() <= tol)
        {
            used[j] = true;
            tp += 1;
            if est[j].2 == t.2 && est[j].3 == t.3 {
                same_place += 1;
            }
        }
    }
    let p = tp as f64 / est.len().max(1) as f64;
    let r = tp as f64 / truth.len().max(1) as f64;
    let f = if p + r > 0.0 {
        2.0 * p * r / (p + r)
    } else {
        0.0
    };
    (p, r, f, same_place as f64 / tp.max(1) as f64)
}

fn render(score: &Score, only: Option<usize>, secs: f64, sf: &Arc<SoundFont>) -> Vec<f32> {
    let timeline = Timeline::build(score);
    let mut synth = Synthesizer::new(sf, &SynthesizerSettings::new(RATE as i32)).unwrap();
    for (i, t) in score.tracks.iter().enumerate() {
        if !t.percussion {
            synth.process_midi_message(
                timeline.track_channels[i] as i32,
                0xC0,
                t.program as i32,
                0,
            );
        }
    }
    let n = (secs * RATE as f64) as usize;
    let mut out = Vec::with_capacity(n);
    let mut events = timeline
        .events
        .iter()
        .filter(|e| only.is_none_or(|o| e.track as usize == o))
        .peekable();
    let (mut l, mut r) = (vec![0f32; 64], vec![0f32; 64]);
    while out.len() < n {
        let t = out.len() as f64 / RATE as f64;
        while let Some(e) = events.peek() {
            if e.secs > t {
                break;
            }
            if e.velocity > 0 {
                synth.note_on(e.channel as i32, e.key as i32, e.velocity as i32);
            } else {
                synth.note_off(e.channel as i32, e.key as i32);
            }
            events.next();
        }
        synth.render(&mut l, &mut r);
        out.extend(l.iter().zip(&r).map(|(a, b)| (a + b) * 0.5));
    }
    out
}

fn write_wav(path: &Path, samples: &[f32]) {
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs())).max(1e-6);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for s in samples {
        w.write_sample((s / peak * 0.9 * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();
}

/// GuitarSet annotations of one excerpt.
struct GuitarSetTruth {
    /// `(onset, offset, pitch, string, fret)`, sorted by onset.
    notes: Vec<(f64, f64, u8, u8, i16)>,
    tempo: f64,
    downbeats: Vec<f64>,
}

fn guitarset_truth(path: &Path) -> Option<GuitarSetTruth> {
    const OPEN: [u8; 6] = [40, 45, 50, 55, 59, 64]; // low E to high e
    let text = std::fs::read_to_string(path).ok()?;
    let doc = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    let mut notes = Vec::new();
    let (mut tempo, mut downbeats) = (0.0, Vec::new());
    for a in doc["annotations"].as_array().into_iter().flatten() {
        match a["namespace"].as_str() {
            Some("note_midi") => {
                let idx: usize = a["annotation_metadata"]["data_source"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                for n in a["data"].as_array().into_iter().flatten() {
                    let pitch = n["value"].as_f64().unwrap_or(0.0).round() as u8;
                    let fret = pitch as i16 - OPEN[idx.min(5)] as i16;
                    let time = n["time"].as_f64().unwrap_or(0.0);
                    notes.push((
                        time,
                        time + n["duration"].as_f64().unwrap_or(0.1),
                        pitch,
                        (6 - idx.min(5)) as u8,
                        fret,
                    ));
                }
            }
            Some("tempo") => tempo = a["data"][0]["value"].as_f64().unwrap_or(0.0),
            Some("beat_position") => {
                for b in a["data"].as_array().into_iter().flatten() {
                    if b["value"]["position"].as_i64() == Some(1) {
                        downbeats.push(b["time"].as_f64().unwrap_or(0.0));
                    }
                }
            }
            _ => {}
        }
    }
    notes.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(GuitarSetTruth {
        notes,
        tempo,
        downbeats,
    })
}

/// Real recordings: GuitarSet (CC BY 4.0, https://zenodo.org/records/3371780),
/// solo acoustic guitar with per-string note annotations. Point
/// `GVA_GUITARSET` at a folder with `annotation/*.jams` and
/// `audio/*_mic.wav` (from `annotation.zip` and `audio_mono-mic.zip`).
/// `GVA_EVAL_LIMIT` caps the number of excerpts (default 60, spread evenly).
#[test]
#[ignore]
fn eval_guitarset() {
    let Ok(root) = std::env::var("GVA_GUITARSET") else {
        eprintln!("set GVA_GUITARSET");
        return;
    };
    let root = PathBuf::from(root);
    let limit: usize = std::env::var("GVA_EVAL_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let demucs = std::env::var("GVA_EVAL_DEMUCS").is_ok_and(|v| v == "1");
    let mut jams: Vec<PathBuf> = std::fs::read_dir(root.join("annotation"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    jams.sort();
    let step = (jams.len() / limit.max(1)).max(1);
    let work = std::env::temp_dir().join(format!("gva_eval_gs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let (mut f1s, mut raw_f1s, mut places, mut tempo_ok, mut downbeat_ok, mut total) =
        (Vec::new(), Vec::new(), Vec::new(), 0, 0, 0);
    for path in jams.iter().step_by(step).take(limit) {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let audio = root.join("audio").join(format!("{stem}_mic.wav"));
        let Some(gs) = guitarset_truth(path) else {
            continue;
        };
        let truth: Vec<_> = gs.notes.iter().map(|n| (n.0, n.2, n.3, n.4)).collect();
        let (tempo, downbeats) = (gs.tempo, gs.downbeats);
        let request = Request {
            audio_path: audio.clone(),
            title: stem.clone(),
            artist: "GuitarSet".into(),
            separate: demucs,
            guitar: true,
            bass: false,
            output_dir: work.join("repo"),
            spotify_track_id: None,
            youtube_video_id: None,
        };
        let Ok(output) = run(&request, &mut |_| {}, &AtomicBool::new(false)) else {
            eprintln!("{stem}: ERRO");
            continue;
        };
        let tab = load_score_file(&output.tab_path).unwrap();
        let meta = load_metadata(&output.tab_path).unwrap();
        let sync = meta.sync_map(&Timeline::build(&tab)).unwrap();
        let est = tab_notes(&tab, 0, &|t| sync.score_to_audio(t));
        let (p, r, f, place) = compare(&truth, &est, 0.08);
        // Notes straight from the model, before quantization (50 ms, as
        // in the papers).
        let raw: Vec<_> = output
            .guitar_notes
            .iter()
            .map(|n| (n.start, n.pitch, 0u8, 0i16))
            .collect();
        let (_, _, raw_f, _) = compare(&truth, &raw, 0.05);
        let ratio = meta.tempo_bpm / tempo.max(1.0);
        let tempo_hit = [0.5, 1.0, 2.0]
            .iter()
            .any(|k| (ratio / k - 1.0).abs() < 0.04);
        // Downbeats: share of our bar starts within 70 ms of an annotated one.
        let bars: Vec<f64> = meta
            .bar_audio_secs
            .iter()
            .copied()
            .filter(|t| *t >= 0.0)
            .collect();
        let db_hits = bars
            .iter()
            .filter(|t| downbeats.iter().any(|d| (*d - **t).abs() < 0.07))
            .count();
        let db_share = db_hits as f64 / bars.len().max(1) as f64;
        eprintln!(
            "{stem:28} P {p:.2} R {r:.2} F1 {f:.2} (modelo {raw_f:.2})  corda/casa {:.0}%  tempo {:.0}/{tempo:.0}{}  compassos {:.0}%",
            place * 100.0,
            meta.tempo_bpm,
            if tempo_hit { " ok" } else { "" },
            db_share * 100.0
        );
        f1s.push(f);
        raw_f1s.push(raw_f);
        places.push(place);
        total += 1;
        tempo_ok += tempo_hit as usize;
        downbeat_ok += (db_share > 0.5) as usize;
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    eprintln!(
        "GUITARSET {total} trechos: F1 mediana {:.2} (modelo {:.2}), corda/casa mediana {:.0}%, tempo certo {}/{total}, compassos alinhados {}/{total}",
        median(&mut f1s),
        median(&mut raw_f1s),
        median(&mut places) * 100.0,
        tempo_ok,
        downbeat_ok
    );
    let _ = std::fs::remove_dir_all(&work);
}

#[test]
#[ignore]
fn eval_transcription() {
    let (Ok(dir), Ok(sf_path)) = (
        std::env::var("GVA_EVAL_TABS"),
        std::env::var("GVA_SOUNDFONT"),
    ) else {
        eprintln!("set GVA_EVAL_TABS and GVA_SOUNDFONT");
        return;
    };
    let secs: f64 = std::env::var("GVA_EVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40.0);
    let demucs = std::env::var("GVA_EVAL_DEMUCS").is_ok_and(|v| v == "1");
    let sf = Arc::new(SoundFont::new(&mut std::fs::File::open(sf_path).unwrap()).unwrap());
    let work = std::env::temp_dir().join(format!("gva_eval_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    let mut summary: Vec<(String, f64)> = Vec::new();
    for file in files {
        let Ok(score) = load_score_file(&file) else {
            continue;
        };
        let Some(gi) = score
            .tracks
            .iter()
            .position(|t| !t.percussion && (24..=31).contains(&t.program))
        else {
            continue;
        };
        let duration = Timeline::build(&score).duration_secs.min(secs);
        let truth: Vec<_> = tab_notes(&score, gi, &|t| t)
            .into_iter()
            .filter(|n| n.0 < duration - 0.5)
            .collect();
        if truth.len() < 5 {
            continue;
        }
        let name: String = file
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .chars()
            .take(28)
            .collect();
        let mut cases: Vec<(&str, Vec<f32>, bool)> = vec![(
            "guitarra isolada",
            render(&score, Some(gi), duration, &sf),
            false,
        )];
        let mix = render(&score, None, duration, &sf);
        cases.push(("mix sem separação", mix.clone(), false));
        if demucs {
            cases.push(("mix + Demucs", mix, true));
        }
        for (case, audio, separate) in cases {
            let wav = work.join(format!("{name} {case}.wav"));
            write_wav(&wav, &audio);
            let request = Request {
                audio_path: wav,
                title: name.clone(),
                artist: case.into(),
                separate,
                guitar: true,
                bass: false,
                output_dir: work.join("repo"),
                spotify_track_id: None,
                youtube_video_id: None,
            };
            let started = std::time::Instant::now();
            let output = match run(&request, &mut |_| {}, &AtomicBool::new(false)) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("{name:28} {case:18} ERRO {e}");
                    continue;
                }
            };
            let elapsed = started.elapsed().as_secs_f64();
            let tab = load_score_file(&output.tab_path).unwrap();
            let meta = load_metadata(&output.tab_path).unwrap();
            let timeline = Timeline::build(&tab);
            let sync = meta.sync_map(&timeline).unwrap();
            let est: Vec<_> = tab_notes(&tab, 0, &|t| sync.score_to_audio(t))
                .into_iter()
                .filter(|n| n.0 < duration - 0.5)
                .collect();
            let (p, r, f, place) = compare(&truth, &est, 0.08);
            if std::env::var("GVA_EVAL_PITCHES").is_ok() {
                let hist = |v: &[(f64, u8, u8, i16)]| {
                    let mut h = std::collections::BTreeMap::new();
                    for n in v {
                        *h.entry(n.1).or_insert(0) += 1;
                    }
                    h
                };
                eprintln!("  verdade {:?}\n  tab     {:?}", hist(&truth), hist(&est));
            }
            eprintln!(
                "{name:28} {case:18} notas {:4}/{:4}  P {p:.2} R {r:.2} F1 {f:.2}  mesma corda/casa {:.0}%  {:.0} bpm  {:.1}s",
                est.len(),
                truth.len(),
                place * 100.0,
                meta.tempo_bpm,
                elapsed
            );
            summary.push((case.to_string(), f));
        }
    }
    for case in ["guitarra isolada", "mix sem separação", "mix + Demucs"] {
        let mut values: Vec<f64> = summary
            .iter()
            .filter(|s| s.0 == case)
            .map(|s| s.1)
            .collect();
        if values.is_empty() {
            continue;
        }
        values.sort_by(f64::total_cmp);
        eprintln!(
            "MEDIANA F1 {case}: {:.2} ({} músicas)",
            values[values.len() / 2],
            values.len()
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// `(onset, pitch, string, fret)` notes of one part, sorted, plus tuning.
type FingeringCase = (Vec<(f64, u8, u8, i16)>, Vec<u8>);

/// Share of `cases`' notes that `fret_events_with` puts on the human's
/// string, given the true notes grouped into chords (onsets within 35 ms)
/// and the true tuning.
fn fingering_accuracy(
    cases: &[FingeringCase],
    costs: &super::arrange::FingeringCosts,
) -> (usize, usize) {
    use super::arrange::{GridEvent, InstrumentSpec, fret_events_with};
    let (mut same, mut total) = (0, 0);
    for (truth, tuning) in cases {
        let spec = InstrumentSpec {
            name: String::new(),
            tuning: tuning.clone(),
            tuning_name: String::new(),
            max_fret: 24,
            max_notes: tuning.len(),
            program: 25,
            fingering_prior: true,
        };
        let mut groups: Vec<Vec<&(f64, u8, u8, i16)>> = Vec::new();
        for n in truth {
            match groups.last_mut() {
                Some(g) if n.0 - g[0].0 <= 0.1 && !g.iter().any(|m| m.1 == n.1) => g.push(n),
                _ => groups.push(vec![n]),
            }
        }
        let events: Vec<GridEvent> = groups
            .iter()
            .map(|g| GridEvent {
                tick: (g[0].0 * 1920.0) as i64,
                end_tick: (g[0].0 * 1920.0) as i64 + 240,
                secs: g[0].0,
                pitches: g.iter().map(|n| (n.1, 0.8)).collect(),
            })
            .collect();
        let prior = super::arrange::StringPrior::guitarset();
        let frets = fret_events_with(&events, &spec, costs, Some(prior));
        for (g, f) in groups.iter().zip(&frets) {
            for n in g {
                total += 1;
                if f.iter().any(|(p, s, _, _)| *p == n.1 && *s == n.2) {
                    same += 1;
                }
            }
        }
    }
    (same, total)
}

/// Fingering only: the true notes of each excerpt (so transcription errors
/// don't count) go through chord grouping and `fret_events` with the true
/// tuning; reports the share of notes on the same string as the human
/// player/transcriber. Sets: GuitarSet players 00-02 (where the string
/// prior comes from), 03-05 (held out), and the Guitar Pro files in
/// `GVA_EVAL_TABS`. `GVA_FIT_FINGERING=1` runs a coordinate search over the
/// cost weights on the tabs and prints the best weights.
#[test]
#[ignore]
fn eval_fingering() {
    use super::arrange::FingeringCosts;
    let (mut fit, mut held, mut tabs): (
        Vec<FingeringCase>,
        Vec<FingeringCase>,
        Vec<FingeringCase>,
    ) = (Vec::new(), Vec::new(), Vec::new());
    if let Ok(root) = std::env::var("GVA_GUITARSET") {
        let mut jams: Vec<PathBuf> = std::fs::read_dir(PathBuf::from(root).join("annotation"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        jams.sort();
        for path in &jams {
            let Some(gs) = guitarset_truth(path) else {
                continue;
            };
            let truth: Vec<_> = gs.notes.iter().map(|n| (n.0, n.2, n.3, n.4)).collect();
            let case = (truth, vec![64, 59, 55, 50, 45, 40]);
            if path.file_name().unwrap().as_encoded_bytes()[1] <= b'2' {
                fit.push(case);
            } else {
                held.push(case);
            }
        }
    }
    if let Ok(dir) = std::env::var("GVA_EVAL_TABS") {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        for file in files {
            let Ok(song) = load_score_file(&file) else {
                continue;
            };
            for (i, t) in song.tracks.iter().enumerate() {
                if t.percussion || !(24..=31).contains(&t.program) || t.tuning.len() < 6 {
                    continue;
                }
                let tuning: Vec<u8> = t
                    .tuning
                    .iter()
                    .map(|p| (*p as i32 + t.capo) as u8)
                    .collect();
                tabs.push((tab_notes(&song, i, &|x| x), tuning));
            }
        }
    }
    let pct = |(s, t): (usize, usize)| 100.0 * s as f64 / t.max(1) as f64;
    let report = |label: &str, costs: &FingeringCosts| {
        for (name, set) in [
            ("GuitarSet 00-02 (ajuste)", &fit),
            ("GuitarSet 03-05 (teste)", &held),
            ("tabs Guitar Pro", &tabs),
        ] {
            let r = fingering_accuracy(set, costs);
            eprintln!(
                "DEDILHADO {label} {name}: {:.1}% na mesma corda ({} notas)",
                pct(r),
                r.1
            );
        }
    };
    let mut best = FingeringCosts::default();
    report("atual", &best);
    if std::env::var("GVA_FIT_FINGERING").is_ok_and(|v| v == "1") {
        // GuitarSet 03-05 stays unseen (the prior comes from 00-02 too).
        let objective = |c: &FingeringCosts| {
            pct(fingering_accuracy(&fit, c)) + pct(fingering_accuracy(&tabs, c))
        };
        let mut best_score = objective(&best);
        for round in 0..8 {
            for field in 0..7 {
                let get = |c: &FingeringCosts| match field {
                    0 => c.span,
                    1 => c.position,
                    2 => c.high,
                    3 => c.open,
                    4 => c.gap,
                    5 => c.moving,
                    _ => c.prior,
                };
                let set = |c: &mut FingeringCosts, v: f64| match field {
                    0 => c.span = v,
                    1 => c.position = v,
                    2 => c.high = v,
                    3 => c.open = v,
                    4 => c.gap = v,
                    5 => c.moving = v,
                    _ => c.prior = v,
                };
                let current = get(&best);
                let step = if current.abs() > 1e-9 {
                    current.abs()
                } else {
                    0.1
                };
                for factor in [-2.0, -1.0, -0.5, -0.25, -0.1, 0.1, 0.25, 0.5, 1.0, 2.0] {
                    let mut c = best;
                    set(&mut c, current + factor * step);
                    let score = objective(&c);
                    if score > best_score + 1e-6 {
                        best_score = score;
                        best = c;
                    }
                }
            }
            eprintln!("rodada {round}: {best:?} -> {best_score:.2}");
        }
        report("ajustado", &best);
    }
}
