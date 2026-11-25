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
//! `GVA_EVAL_DEMUCS=1` to also run the full mix through the separator
//! (BS-RoFormer when installed, else Demucs; `GVA_SEPARATOR` forces one),
//! `GVA_EVAL_PITCHES=1` to print pitch histograms (octave errors show up
//! there) and `GVA_EVAL_CACHE=<dir>` to reuse transcribed notes. Every
//! benchmark runs the GAPS guitar CRNN unless `GVA_EVAL_GUITAR_MODEL=electric`
//! (the local electric fine-tune, when installed).

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

/// `GVA_EVAL_GUITAR_MODEL=electric` runs the electric fine-tune.
fn eval_guitar_model() -> super::guitar_crnn::GuitarModel {
    match std::env::var("GVA_EVAL_GUITAR_MODEL").as_deref() {
        Ok("electric") => super::guitar_crnn::GuitarModel::Electric,
        _ => super::guitar_crnn::GuitarModel::Gaps,
    }
}

/// `GVA_EVAL_FINGERING=learned` uses the learned fingering tagger when
/// installed; benchmarks use the Viterbi otherwise.
fn eval_fingering_choice() -> super::fingering_net::Fingering {
    match std::env::var("GVA_EVAL_FINGERING").as_deref() {
        Ok("learned") => super::fingering_net::Fingering::Learned,
        _ => super::fingering_net::Fingering::Viterbi,
    }
}

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

/// Share of the true notes that have no estimate at their pitch but one an
/// octave (or two) away, onset within `tol`: octave errors.
fn octave_errors(truth: &[(f64, u8, u8, i16)], est: &[(f64, u8, u8, i16)], tol: f64) -> f64 {
    octave_errors_by_direction(truth, est, tol).iter().sum()
}

/// [`octave_errors`] split into `[estimate too high, too low]`.
fn octave_errors_by_direction(
    truth: &[(f64, u8, u8, i16)],
    est: &[(f64, u8, u8, i16)],
    tol: f64,
) -> [f64; 2] {
    let near = |t: &(f64, u8, u8, i16), d: i16| {
        est.iter()
            .any(|e| e.1 as i16 - t.1 as i16 == d && (e.0 - t.0).abs() <= tol)
    };
    let missed: Vec<_> = truth.iter().filter(|t| !near(t, 0)).collect();
    let up = missed.iter().filter(|t| near(t, 12) || near(t, 24)).count();
    let down = missed
        .iter()
        .filter(|t| !(near(t, 12) || near(t, 24)) && (near(t, -12) || near(t, -24)))
        .count();
    let n = truth.len().max(1) as f64;
    [up as f64 / n, down as f64 / n]
}

/// `audio` through the pedal chain of the factory preset `name` (at the
/// app's 48 kHz, in i16 like the live signal): a GuitarSet recording with
/// an amp tone, as in EGDB's amp renders.
fn through_preset(audio: &super::Mono, name: &str) -> Vec<f32> {
    let preset = crate::backend::offline::factory_preset(name)
        .unwrap_or_else(|| panic!("no factory preset named {name}"));
    crate::backend::offline::render(preset.pedals, &audio.at(RENDER_RATE))
}

const RENDER_RATE: u32 = crate::backend::offline::RENDER_RATE;

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
    write_wav_at(path, samples, RATE);
}

fn write_wav_at(path: &Path, samples: &[f32], rate: u32) {
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs())).max(1e-6);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
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
    /// `(onset, pitch)` of notes whose annotated pitch contour glides (over
    /// 35 ms or more) 0.75 to 4 semitones above where it starts and stays
    /// up for 80 ms: bends or slides. GuitarSet has no technique labels,
    /// and an instant jump is a new note, not a glide.
    glides: Vec<(f64, u8)>,
}

/// `(times, frequencies)` of a JAMS `pitch_contour` annotation (columnar
/// or one observation per entry); unvoiced frames have frequency <= 0.
fn contour_points(data: &serde_json::Value) -> (Vec<f64>, Vec<f64>) {
    let freq = |v: &serde_json::Value| {
        let voiced = v["voiced"].as_bool().unwrap_or(true);
        let f = v["frequency"]
            .as_f64()
            .or_else(|| v.as_f64())
            .unwrap_or(0.0);
        if voiced { f } else { 0.0 }
    };
    if let (Some(t), Some(v)) = (data["time"].as_array(), data["value"].as_array()) {
        return (
            t.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect(),
            v.iter().map(freq).collect(),
        );
    }
    let obs = data.as_array().cloned().unwrap_or_default();
    (
        obs.iter()
            .map(|o| o["time"].as_f64().unwrap_or(0.0))
            .collect(),
        obs.iter().map(|o| freq(&o["value"])).collect(),
    )
}

/// Held, gradual rise of the contour within `start..end` above its start,
/// in semitones: the level a run of at least 80 ms stays above, reached
/// over 35 ms or more (0 if none).
fn contour_rise(times: &[f64], freqs: &[f64], start: f64, end: f64) -> f64 {
    let a = times.partition_point(|t| *t < start + 0.02);
    let b = times.partition_point(|t| *t < end - 0.04);
    let midi: Vec<f64> = (a..b.max(a))
        .filter(|&i| freqs[i] > 0.0)
        .map(|i| 69.0 + 12.0 * (freqs[i] / 440.0).log2())
        .collect();
    let hop = match times {
        [t0, t1, ..] => (t1 - t0).max(1e-4),
        _ => return 0.0,
    };
    let run = (0.08 / hop).ceil() as usize;
    if midi.len() < run.max(6) {
        return 0.0;
    }
    let med = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let first = med(&midi[..5]);
    // The highest level that a whole run stays above.
    let peak = (0..=midi.len() - run)
        .map(|i| midi[i..i + run].iter().copied().fold(f64::MAX, f64::min))
        .fold(f64::MIN, f64::max);
    let rise = peak - first;
    if rise <= 0.0 {
        return 0.0;
    }
    let Some(hi) = midi.iter().position(|m| *m >= first + 0.8 * rise) else {
        return 0.0;
    };
    let lo = midi[..hi]
        .iter()
        .rposition(|m| *m <= first + 0.2 * rise)
        .unwrap_or(hi);
    if (hi - lo) as f64 * hop >= 0.035 {
        rise
    } else {
        0.0
    }
}

fn guitarset_truth(path: &Path) -> Option<GuitarSetTruth> {
    const OPEN: [u8; 6] = [40, 45, 50, 55, 59, 64]; // low E to high e
    let text = std::fs::read_to_string(path).ok()?;
    let doc = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    let mut notes = Vec::new();
    let (mut tempo, mut downbeats) = (0.0, Vec::new());
    // `(string, (times, frequencies))` of every string's pitch contour.
    type Contour = (u8, (Vec<f64>, Vec<f64>));
    let mut contours: Vec<Contour> = Vec::new();
    for a in doc["annotations"].as_array().into_iter().flatten() {
        match a["namespace"].as_str() {
            Some("pitch_contour") => {
                let idx: usize = a["annotation_metadata"]["data_source"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                contours.push(((6 - idx.min(5)) as u8, contour_points(&a["data"])));
            }
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
    let glides = notes
        .iter()
        .filter(|n| {
            contours.iter().any(|(string, (t, f))| {
                *string == n.3 && (0.75..4.0).contains(&contour_rise(t, f, n.0, n.1))
            })
        })
        .map(|n| (n.0, n.2))
        .collect();
    Some(GuitarSetTruth {
        notes,
        tempo,
        downbeats,
        glides,
    })
}

/// Real recordings: GuitarSet (CC BY 4.0, https://zenodo.org/records/3371780),
/// solo acoustic guitar with per-string note annotations. Point
/// `GVA_GUITARSET` at a folder with `annotation/*.jams` and
/// `audio/*_mic.wav` (from `annotation.zip` and `audio_mono-mic.zip`).
/// `GVA_EVAL_LIMIT` caps the number of excerpts (default 60, spread evenly).
/// `GVA_EVAL_TONE=<factory preset name>` (e.g. "Metal High Gain") plays
/// each recording through that preset's pedal chain first, for distorted
/// guitar with known notes; the octave-error share shows up there.
/// `GVA_EVAL_PLAYERS=03,04,05` keeps only those players' excerpts (for
/// models trained on the others).
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
    let tone = std::env::var("GVA_EVAL_TONE")
        .ok()
        .filter(|t| !t.is_empty());
    let players: Option<Vec<String>> = std::env::var("GVA_EVAL_PLAYERS")
        .ok()
        .map(|v| v.split(',').map(|p| p.trim().to_string()).collect());
    let mut jams: Vec<PathBuf> = std::fs::read_dir(root.join("annotation"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            players
                .as_ref()
                .is_none_or(|ps| ps.iter().any(|x| name.starts_with(x.as_str())))
        })
        .collect();
    jams.sort();
    let step = (jams.len() / limit.max(1)).max(1);
    let work = std::env::temp_dir().join(format!("gva_eval_gs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut octaves = Vec::new();
    let mut octave_dirs = (0.0, 0.0);
    let (mut f1s, mut raw_f1s, mut places, mut tempo_ok, mut downbeat_ok, mut total) =
        (Vec::new(), Vec::new(), Vec::new(), 0, 0, 0);
    // Glides: (annotated, bends + slides detected, detected that are
    // annotated, annotated that are detected).
    let mut glides = (0usize, 0usize, 0usize, 0usize);
    // Techniques detected: (bends, vibrato, slides, hammer-ons/pull-offs).
    let mut found = (0usize, 0usize, 0usize, 0usize);
    for path in jams.iter().step_by(step).take(limit) {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        let mut audio = root.join("audio").join(format!("{stem}_mic.wav"));
        let Some(gs) = guitarset_truth(path) else {
            continue;
        };
        if let Some(name) = &tone {
            let rendered = through_preset(&super::Mono::load(&audio).unwrap(), name);
            audio = work.join(format!("{stem}.wav"));
            write_wav_at(&audio, &rendered, RENDER_RATE);
        }
        let truth: Vec<_> = gs.notes.iter().map(|n| (n.0, n.2, n.3, n.4)).collect();
        let (tempo, downbeats) = (gs.tempo, gs.downbeats.clone());
        let request = Request {
            audio_path: audio.clone(),
            title: stem.clone(),
            artist: "GuitarSet".into(),
            separate: demucs,
            guitar_model: eval_guitar_model(),
            fingering: eval_fingering_choice(),
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
        let gliding: Vec<(f64, u8)> = output
            .guitar_notes
            .iter()
            .zip(&output.guitar_techniques)
            .filter(|(_, t)| t.bend > 0 || t.slide_out)
            .map(|(n, _)| (n.start, n.pitch))
            .collect();
        let near = |a: &(f64, u8), b: &(f64, u8)| a.1 == b.1 && (a.0 - b.0).abs() <= 0.08;
        glides.0 += gs.glides.len();
        glides.1 += gliding.len();
        glides.2 += gliding
            .iter()
            .filter(|b| gs.glides.iter().any(|t| near(t, b)))
            .count();
        glides.3 += gs
            .glides
            .iter()
            .filter(|t| gliding.iter().any(|b| near(t, b)))
            .count();
        for t in &output.guitar_techniques {
            found.0 += (t.bend > 0) as usize;
            found.1 += t.vibrato as usize;
            found.2 += t.slide_out as usize;
            found.3 += t.legato_out as usize;
        }
        let (p, r, f, place) = compare(&truth, &est, 0.08);
        let octave = octave_errors(&truth, &est, 0.08);
        let [up, down] = octave_errors_by_direction(&truth, &est, 0.08);
        octave_dirs.0 += up;
        octave_dirs.1 += down;
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
            "{stem:28} P {p:.2} R {r:.2} F1 {f:.2} (modelo {raw_f:.2})  oitava {:.0}%  corda/casa {:.0}%  tempo {:.0}/{tempo:.0}{}  compassos {:.0}%",
            octave * 100.0,
            place * 100.0,
            meta.tempo_bpm,
            if tempo_hit { " ok" } else { "" },
            db_share * 100.0
        );
        f1s.push(f);
        octaves.push(octave);
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
        "GUITARSET{} {total} trechos: F1 mediana {:.2} (modelo {:.2}), erros de oitava {:.1}% das notas ({:.1}% acima, {:.1}% abaixo), corda/casa mediana {:.0}%, tempo certo {}/{total}, compassos alinhados {}/{total}",
        tone.as_ref().map(|t| format!(" [{t}]")).unwrap_or_default(),
        median(&mut f1s),
        median(&mut raw_f1s),
        100.0 * octaves.iter().sum::<f64>() / octaves.len().max(1) as f64,
        100.0 * octave_dirs.0 / octaves.len().max(1) as f64,
        100.0 * octave_dirs.1 / octaves.len().max(1) as f64,
        median(&mut places) * 100.0,
        tempo_ok,
        downbeat_ok
    );
    eprintln!(
        "TÉCNICAS {} notas com glissando no contorno anotado, {} bends+slides detectados: precisão {:.2}, revocação {:.2}; detectados: {} bends, {} vibratos, {} slides, {} ligados",
        glides.0,
        glides.1,
        glides.2 as f64 / glides.1.max(1) as f64,
        glides.3 as f64 / glides.0.max(1) as f64,
        found.0,
        found.1,
        found.2,
        found.3
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
                guitar_model: eval_guitar_model(),
                fingering: eval_fingering_choice(),
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
                techniques: Vec::new(),
                timing: Vec::new(),
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

/// Techniques of a picked note: (bend, slide, hammer-on/pull-off, vibrato).
type Flags = [bool; 4];
const FLAG_NAMES: [&str; 4] = ["bend", "slide", "ligado", "vibrato"];

/// `(time, pitch, techniques)` of every picked note of `track`.
fn tab_flags(score: &Score, track: usize, map: &dyn Fn(f64) -> f64) -> Vec<(f64, u8, Flags)> {
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
                    // Links to the next note may sit on the last tied piece.
                    let linked = |f: fn(&crate::shared::tab::model::TabNote) -> bool| {
                        f(n) || voice.iter().any(|b| {
                            b.start > beat.start
                                && b.notes
                                    .iter()
                                    .any(|m| m.tie && m.string == n.string && f(m))
                        })
                    };
                    let flags = [n.bend, linked(|m| m.slide), linked(|m| m.hammer), n.vibrato];
                    out.push((map(occ.tick_to_secs(beat.start)), pitch, flags));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// Bar lines in audio time with each bar's length in quarter notes.
fn bar_lines(score: &Score, map: &dyn Fn(f64) -> f64) -> Vec<(f64, f64)> {
    Timeline::build(score)
        .occurrences
        .iter()
        .map(|o| {
            let bar = &score.bars[o.bar];
            let quarters = bar.numerator as f64 * 4.0 / bar.denominator.max(1) as f64;
            (map(o.start_secs), quarters)
        })
        .collect()
}

/// Real band recordings: Guitar Pro 8 files with the song's audio embedded
/// and synced to the tab (`GVA_EVAL_REAL=<folder>`), so the tab is the
/// ground truth for a real mix. Community tabs aren't perfect and sync
/// points are only exact at bar lines, so expect lower numbers than on
/// GuitarSet. Uses Demucs when installed (`GVA_EVAL_DEMUCS=0` turns it
/// off); `GVA_EVAL_SECS` limits each song (default: all of it).
///
/// Reports, per song: F1 against the best-matching guitar track and the
/// share of all guitar notes found, octave errors, bar lines within 70 ms
/// of the tab's (and how many of those have the tab's meter), and
/// precision/recall of each technique on the notes found.
#[test]
#[ignore]
fn eval_real() {
    let Ok(dir) = std::env::var("GVA_EVAL_REAL") else {
        eprintln!("set GVA_EVAL_REAL");
        return;
    };
    let separate = std::env::var("GVA_EVAL_DEMUCS").map_or(true, |v| v != "0");
    let secs: f64 = std::env::var("GVA_EVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(f64::MAX);
    let work = std::env::temp_dir().join(format!("gva_eval_real_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    let (mut f1s, mut recalls, mut octaves, mut bar_hits, mut meter_hits) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    // Per technique: (true, detected, detected & true) on matched notes.
    let mut tech = [(0usize, 0usize, 0usize); 4];
    for file in files {
        let Ok(score) = load_score_file(&file) else {
            continue;
        };
        let name: String = file
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .chars()
            .take(32)
            .collect();
        let (Some(bytes), false) = (score.backing_audio.clone(), score.sync_points.is_empty())
        else {
            eprintln!("{name:32} sem áudio embutido ou pontos de sincronia");
            continue;
        };
        let timeline = Timeline::build(&score);
        let Some(truth_sync) =
            crate::shared::tab::timeline::SyncMap::from_file_points(&score.sync_points, &timeline)
        else {
            continue;
        };
        let guitars: Vec<usize> = (0..score.tracks.len())
            .filter(|&i| {
                let t = &score.tracks[i];
                !t.percussion && (24..=31).contains(&t.program) && t.tuning.len() >= 6
            })
            .collect();
        if guitars.is_empty() {
            continue;
        }
        let decoded = match crate::backend::audio_file::decode_bytes(bytes) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{name:32} ERRO ao ler o áudio: {e}");
                continue;
            }
        };
        let len = ((secs * decoded.sample_rate as f64) as usize).min(decoded.samples.len());
        let wav = work.join(format!("{name}.wav"));
        write_wav_at(&wav, &decoded.samples[..len], decoded.sample_rate);
        let duration = len as f64 / decoded.sample_rate.max(1) as f64;
        let request = Request {
            audio_path: wav,
            title: name.clone(),
            artist: "eval".into(),
            separate,
            guitar_model: eval_guitar_model(),
            fingering: eval_fingering_choice(),
            guitar: true,
            bass: false,
            output_dir: work.join("repo"),
            spotify_track_id: None,
            youtube_video_id: None,
        };
        let output = match run(&request, &mut |_| {}, &AtomicBool::new(false)) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("{name:32} ERRO {e}");
                continue;
            }
        };
        let tab = load_score_file(&output.tab_path).unwrap();
        let meta = load_metadata(&output.tab_path).unwrap();
        let sync = meta.sync_map(&Timeline::build(&tab)).unwrap();
        let in_range = |n: &(f64, u8, u8, i16)| n.0 >= 0.0 && n.0 < duration - 0.5;
        let est: Vec<_> = tab_notes(&tab, 0, &|t| sync.score_to_audio(t))
            .into_iter()
            .filter(in_range)
            .collect();
        let to_audio = |t: f64| truth_sync.score_to_audio(t);
        // The guitar track our single track matches best.
        let (best, (_, _, f, _)) = guitars
            .iter()
            .map(|&g| {
                let truth: Vec<_> = tab_notes(&score, g, &to_audio)
                    .into_iter()
                    .filter(in_range)
                    .collect();
                (g, compare(&truth, &est, 0.08))
            })
            .max_by(|a, b| a.1.2.total_cmp(&b.1.2))
            .unwrap();
        let all: Vec<_> = guitars
            .iter()
            .flat_map(|&g| tab_notes(&score, g, &to_audio))
            .filter(in_range)
            .collect();
        let (_, union_recall, _, _) = compare(&all, &est, 0.08);
        let truth: Vec<_> = tab_notes(&score, best, &to_audio)
            .into_iter()
            .filter(in_range)
            .collect();
        let octave = octave_errors(&truth, &est, 0.08);
        // Bar lines and meters.
        let true_bars: Vec<(f64, f64)> = bar_lines(&score, &to_audio)
            .into_iter()
            .filter(|b| b.0 >= 0.0 && b.0 < duration)
            .collect();
        let our_bars: Vec<(f64, f64)> = bar_lines(&tab, &|t| sync.score_to_audio(t))
            .into_iter()
            .filter(|b| b.0 >= 0.0 && b.0 < duration)
            .collect();
        // (our bar, the tab's bar at the same time), each (start, quarters).
        type BarPair<'a> = (&'a (f64, f64), &'a (f64, f64));
        let matched: Vec<BarPair> = our_bars
            .iter()
            .filter_map(|o| {
                true_bars
                    .iter()
                    .find(|t| (t.0 - o.0).abs() < 0.07)
                    .map(|t| (o, t))
            })
            .collect();
        let bar_share = matched.len() as f64 / our_bars.len().max(1) as f64;
        let meter_share = matched
            .iter()
            .filter(|(o, t)| (o.1 - t.1).abs() < 1e-6)
            .count() as f64
            / matched.len().max(1) as f64;
        // Techniques on the notes found.
        let true_flags: Vec<_> = tab_flags(&score, best, &to_audio);
        let our_flags: Vec<_> = tab_flags(&tab, 0, &|t| sync.score_to_audio(t));
        for t in true_flags
            .iter()
            .filter(|t| t.0 >= 0.0 && t.0 < duration - 0.5)
        {
            let Some(o) = our_flags
                .iter()
                .find(|o| o.1 == t.1 && (o.0 - t.0).abs() <= 0.08)
            else {
                continue;
            };
            for k in 0..4 {
                tech[k].0 += t.2[k] as usize;
                tech[k].1 += o.2[k] as usize;
                tech[k].2 += (t.2[k] && o.2[k]) as usize;
            }
        }
        eprintln!(
            "{name:32} F1 {f:.2} (faixa {best} \"{}\")  todas as guitarras R {union_recall:.2}  oitava {:.0}%  compassos {:.0}% (fórmula certa {:.0}%)",
            score.tracks[best].name,
            octave * 100.0,
            bar_share * 100.0,
            meter_share * 100.0
        );
        f1s.push(f);
        recalls.push(union_recall);
        octaves.push(octave);
        bar_hits.push(bar_share);
        meter_hits.push(meter_share);
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    let n = f1s.len();
    eprintln!(
        "REAL {n} músicas: F1 mediana {:.2}, revocação de todas as guitarras {:.2}, erros de oitava {:.1}%, compassos alinhados {:.0}%, fórmula de compasso certa {:.0}%",
        median(&mut f1s),
        median(&mut recalls),
        100.0 * octaves.iter().sum::<f64>() / n.max(1) as f64,
        100.0 * median(&mut bar_hits),
        100.0 * median(&mut meter_hits)
    );
    for (k, (t, d, both)) in tech.iter().enumerate() {
        eprintln!(
            "TÉCNICA {:8} {t} na tab, {d} detectadas: precisão {:.2}, revocação {:.2}",
            FLAG_NAMES[k],
            *both as f64 / (*d).max(1) as f64,
            *both as f64 / (*t).max(1) as f64
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}

fn write_stereo_wav(path: &Path, left: &[f32], right: &[f32], rate: u32) {
    let peak = left
        .iter()
        .chain(right)
        .fold(0f32, |m, s| m.max(s.abs()))
        .max(1e-6);
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for (l, r) in left.iter().zip(right) {
        w.write_sample((l / peak * 0.9 * 32767.0) as i16).unwrap();
        w.write_sample((r / peak * 0.9 * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();
}

/// Two guitars in one stereo mix, from GuitarSet: a player's comping
/// (rhythm) and solo (lead) takes of the same song, at equal loudness,
/// either on opposite sides ("L/R") or rhythm left and lead in the middle
/// ("L/C"); plus one guitar that must stay one track: the solo panned
/// halfway left ("1 lado"), and in the middle with a 300 ms echo on the
/// right ("1 eco"). Reports how many guitar tracks come out and, for each
/// true part, the F1 of the best-matching track. Runs through Demucs, like a
/// real song (the pan split works on its guitar stem); compare with
/// `GVA_SPLIT_GUITARS=0` for the single-track baseline. `GVA_EVAL_LIMIT`
/// songs (default 12).
#[test]
#[ignore]
fn eval_guitar_pairs() {
    let Ok(root) = std::env::var("GVA_GUITARSET") else {
        eprintln!("set GVA_GUITARSET");
        return;
    };
    let root = PathBuf::from(root);
    let limit: usize = std::env::var("GVA_EVAL_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let work = std::env::temp_dir().join(format!("gva_eval_pairs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut comps: Vec<PathBuf> = std::fs::read_dir(root.join("annotation"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with("_comp.jams"))
        .collect();
    comps.sort();
    // GuitarSet has 30 songs per player: a stride coprime with 30 varies
    // both the player and the song.
    let gcd = |mut a: usize, mut b: usize| {
        while b > 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let mut step = (comps.len() / limit.max(1)).max(1);
    while gcd(step, 30) != 1 {
        step += 1;
    }
    // Per layout: (name, tracks out, rhythm F1s, lead F1s).
    type Layout<'a> = (&'a str, Vec<usize>, Vec<f64>, Vec<f64>);
    let mut results: Vec<Layout> = ["L/R", "L/C", "1 lado", "1 eco"]
        .into_iter()
        .map(|l| (l, vec![], vec![], vec![]))
        .collect();
    let load = |stem: &str| {
        let audio = super::Mono::load(&root.join("audio").join(format!("{stem}_mic.wav"))).unwrap();
        let rms = (audio.samples.iter().map(|s| s * s).sum::<f32>()
            / audio.samples.len().max(1) as f32)
            .sqrt()
            .max(1e-6);
        let samples: Vec<f32> = audio.samples.iter().map(|s| s / rms * 0.1).collect();
        (samples, audio.rate)
    };
    for comp in comps.iter().step_by(step).take(limit) {
        let comp_stem = comp.file_stem().unwrap().to_string_lossy().into_owned();
        let solo_stem = comp_stem.replace("_comp", "_solo");
        let (Some(rhythm_truth), Some(lead_truth)) = (
            guitarset_truth(comp),
            guitarset_truth(&root.join("annotation").join(format!("{solo_stem}.jams"))),
        ) else {
            continue;
        };
        let ((rhythm, rate), (lead, _)) = (load(&comp_stem), load(&solo_stem));
        let n = rhythm.len().min(lead.len());
        let truth = |t: &GuitarSetTruth| -> Vec<(f64, u8, u8, i16)> {
            t.notes
                .iter()
                .filter(|x| x.0 < n as f64 / rate as f64 - 0.5)
                .map(|x| (x.0, x.2, x.3, x.4))
                .collect()
        };
        let (rhythm_notes, lead_notes) = (truth(&rhythm_truth), truth(&lead_truth));
        for (layout, tracks_out, rhythm_f1s, lead_f1s) in results.iter_mut() {
            let echo = (0.3 * rate as f64) as usize;
            let (left, right): (Vec<f32>, Vec<f32>) = match *layout {
                "L/R" => (rhythm[..n].to_vec(), lead[..n].to_vec()),
                "L/C" => (
                    (0..n).map(|i| rhythm[i] + 0.707 * lead[i]).collect(),
                    (0..n).map(|i| 0.707 * lead[i]).collect(),
                ),
                "1 lado" => (
                    (0..n).map(|i| 0.9 * lead[i]).collect(),
                    (0..n).map(|i| 0.4 * lead[i]).collect(),
                ),
                _ => (
                    (0..n).map(|i| 0.707 * lead[i]).collect(),
                    (0..n)
                        .map(|i| {
                            0.707 * lead[i] + if i >= echo { 0.5 * lead[i - echo] } else { 0.0 }
                        })
                        .collect(),
                ),
            };
            let single = layout.starts_with('1');
            let wav = work.join(format!("{comp_stem} {}.wav", layout.replace('/', "")));
            write_stereo_wav(&wav, &left, &right, rate);
            let request = Request {
                audio_path: wav,
                title: comp_stem.clone(),
                artist: layout.to_string(),
                separate: true,
                guitar_model: eval_guitar_model(),
                fingering: eval_fingering_choice(),
                guitar: true,
                bass: false,
                output_dir: work.join("repo"),
                spotify_track_id: None,
                youtube_video_id: None,
            };
            let output = match run(&request, &mut |_| {}, &AtomicBool::new(false)) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("{comp_stem} {layout}: ERRO {e}");
                    continue;
                }
            };
            let tab = load_score_file(&output.tab_path).unwrap();
            let meta = load_metadata(&output.tab_path).unwrap();
            let sync = meta.sync_map(&Timeline::build(&tab)).unwrap();
            let ours: Vec<Vec<_>> = (0..tab.tracks.len())
                .map(|k| tab_notes(&tab, k, &|t| sync.score_to_audio(t)))
                .collect();
            let best = |truth: &[(f64, u8, u8, i16)]| {
                ours.iter()
                    .map(|est| compare(truth, est, 0.08).2)
                    .fold(0.0, f64::max)
            };
            let (fr, fl) = if single {
                (0.0, best(&lead_notes))
            } else {
                (best(&rhythm_notes), best(&lead_notes))
            };
            eprintln!(
                "{comp_stem:26} {layout}: {} faixa(s) [{}]  base F1 {fr:.2}  solo F1 {fl:.2}",
                tab.tracks.len(),
                meta.tracks
                    .iter()
                    .map(|t| t.stem.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            tracks_out.push(tab.tracks.len());
            rhythm_f1s.push(fr);
            lead_f1s.push(fl);
        }
    }
    let median = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    for (layout, tracks_out, rhythm_f1s, lead_f1s) in &results {
        let expected = if layout.starts_with('1') { 1 } else { 2 };
        let right = tracks_out.iter().filter(|&&t| t == expected).count();
        eprintln!(
            "PARES {layout} {} mixes: {expected} faixa(s) em {right}, F1 mediana base {:.2}, solo {:.2}",
            tracks_out.len(),
            median(rhythm_f1s),
            median(lead_f1s)
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// A labelled note of a manifest benchmark: onset, offset, pitch and,
/// when the dataset has it, the technique it is played with.
type LabelledNote = (f64, f64, u8, Option<String>);

/// Notes from a text file of `onset offset pitch [velocity [technique]]`
/// lines.
fn manifest_notes(path: &Path) -> Option<Vec<LabelledNote>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut notes: Vec<LabelledNote> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let v: Vec<&str> = l.split_whitespace().collect();
            Some((
                v.first()?.parse().ok()?,
                v.get(1)?.parse().ok()?,
                v.get(2)?.parse().ok()?,
                v.get(4).map(|t| t.to_string()),
            ))
        })
        .collect();
    notes.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(notes)
}

/// Recordings with note labels from other datasets (EGDB, Guitar-TECHS,
/// IDMT-SMT-Guitar...), listed in `GVA_EVAL_MANIFEST`: one line per
/// recording, tab-separated `audio path, notes file, group[, technique]`.
/// Notes files have `onset offset midi_pitch` lines (seconds);
/// `scripts/train/prepare_data.py` writes both. Reports per group the
/// median F1 of the final tab (80 ms) and of the raw model notes (50 ms),
/// octave errors, and, for recordings labelled with a technique (`bend`,
/// `vibrato`, `slide`, `legato`, or `none`), the share of matched notes of
/// 0.3 s or more that our pipeline marks with each technique. Notes files
/// with a technique per note (IDMT) are scored note by note instead
/// ("NOTA" lines): a slide counts when the note or the one before it
/// slides. `GVA_EVAL_GROUP=<substring>` runs
/// only matching groups; `GVA_EVAL_SECS` cuts long recordings;
/// `GVA_EVAL_DEMUCS=1` separates first (for band mixes).
#[test]
#[ignore]
fn eval_manifest() {
    let Ok(manifest) = std::env::var("GVA_EVAL_MANIFEST") else {
        eprintln!("set GVA_EVAL_MANIFEST");
        return;
    };
    let only = std::env::var("GVA_EVAL_GROUP").ok();
    let separate = std::env::var("GVA_EVAL_DEMUCS").is_ok_and(|v| v == "1");
    let secs: f64 = std::env::var("GVA_EVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(f64::MAX);
    let work = std::env::temp_dir().join(format!("gva_eval_manifest_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    // Per group: (final F1s, raw F1s, octave errors).
    type Scores = (Vec<f64>, Vec<f64>, Vec<f64>);
    let mut groups: std::collections::BTreeMap<String, Scores> = Default::default();
    // Per (group, labelled technique): (matched notes, bend, vibrato, slide, legato).
    let mut techs: std::collections::BTreeMap<(String, String), [usize; 5]> = Default::default();
    // Per (group, note label): (labelled, found, bend, vibrato, slide, legato).
    let mut note_techs: std::collections::BTreeMap<(String, String), [usize; 6]> =
        Default::default();
    let text = std::fs::read_to_string(&manifest).expect("manifest");
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let cols: Vec<&str> = line.split('\t').collect();
        let (Some(audio), Some(notes), Some(group)) = (cols.first(), cols.get(1), cols.get(2))
        else {
            continue;
        };
        if only.as_ref().is_some_and(|o| !group.contains(o.as_str())) {
            continue;
        }
        let Some(truth) = manifest_notes(Path::new(notes)) else {
            eprintln!("{notes}: sem notas");
            continue;
        };
        let mut audio_path = PathBuf::from(audio);
        if secs < f64::MAX {
            let a = super::Mono::load(&audio_path).unwrap();
            let n = ((secs * a.rate as f64) as usize).min(a.samples.len());
            audio_path = work.join(format!("{}.wav", groups.len() * 1000 + techs.len()));
            write_wav_at(&audio_path, &a.samples[..n], a.rate);
        }
        let request = Request {
            audio_path: audio_path.clone(),
            title: Path::new(audio)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            artist: group.to_string(),
            separate,
            guitar_model: eval_guitar_model(),
            fingering: eval_fingering_choice(),
            guitar: true,
            bass: false,
            output_dir: work.join("repo"),
            spotify_track_id: None,
            youtube_video_id: None,
        };
        let output = match run(&request, &mut |_| {}, &AtomicBool::new(false)) {
            Ok(o) => o,
            Err(e) => {
                eprintln!("{audio}: ERRO {e}");
                continue;
            }
        };
        let duration_secs = super::Mono::load(&audio_path)
            .map(|a| a.duration())
            .unwrap_or(0.0);
        // Techniques per labelled note: the estimate at the same onset and
        // pitch, and the estimate just before it (slides leave from it).
        for t in truth.iter().filter(|t| t.0 < duration_secs - 0.3) {
            let Some(label) = &t.3 else {
                continue;
            };
            let entry = note_techs
                .entry((group.to_string(), label.clone()))
                .or_default();
            entry[0] += 1;
            let notes = &output.guitar_notes;
            let Some(k) = notes
                .iter()
                .position(|n| n.pitch == t.2 && (n.start - t.0).abs() <= 0.08)
            else {
                continue;
            };
            let tech = output.guitar_techniques[k];
            let before = k
                .checked_sub(1)
                .map(|j| output.guitar_techniques[j].slide_out)
                .unwrap_or(false);
            entry[1] += 1;
            entry[2] += (tech.bend > 0) as usize;
            entry[3] += tech.vibrato as usize;
            entry[4] += (tech.slide_out || before) as usize;
            entry[5] += tech.legato_out as usize;
        }
        let duration = super::Mono::load(&audio_path)
            .map(|a| a.duration())
            .unwrap_or(0.0);
        let truth: Vec<(f64, u8, u8, i16)> = truth
            .iter()
            .filter(|n| n.0 < duration - 0.3)
            .map(|n| (n.0, n.2, 0, 0))
            .collect();
        let tab = load_score_file(&output.tab_path).unwrap();
        let meta = load_metadata(&output.tab_path).unwrap();
        let sync = meta.sync_map(&Timeline::build(&tab)).unwrap();
        let est: Vec<_> = tab_notes(&tab, 0, &|t| sync.score_to_audio(t))
            .into_iter()
            .filter(|n| n.0 >= 0.0 && n.0 < duration - 0.3)
            .collect();
        let raw: Vec<_> = output
            .guitar_notes
            .iter()
            .map(|n| (n.start, n.pitch, 0u8, 0i16))
            .collect();
        let (_, _, f, _) = compare(&truth, &est, 0.08);
        let (_, _, raw_f, _) = compare(&truth, &raw, 0.05);
        let octave = octave_errors(&truth, &raw, 0.05);
        eprintln!(
            "{group:24} {:40} F1 {f:.2} (modelo {raw_f:.2}) oitava {:.0}%",
            Path::new(audio).file_name().unwrap().to_string_lossy(),
            octave * 100.0
        );
        let g = groups.entry(group.to_string()).or_default();
        g.0.push(f);
        g.1.push(raw_f);
        g.2.push(octave);
        if let Some(label) = cols.get(3).filter(|l| !l.is_empty()) {
            let t = techs
                .entry((group.to_string(), label.to_string()))
                .or_default();
            for (n, tech) in output.guitar_notes.iter().zip(&output.guitar_techniques) {
                // Short fragments can't show a bend or vibrato.
                let matched = truth
                    .iter()
                    .any(|x| x.1 == n.pitch && (x.0 - n.start).abs() <= 0.08);
                if !matched || n.end - n.start < 0.3 {
                    continue;
                }
                t[0] += 1;
                t[1] += (tech.bend > 0) as usize;
                t[2] += tech.vibrato as usize;
                t[3] += tech.slide_out as usize;
                t[4] += tech.legato_out as usize;
            }
        }
    }
    let median = |v: &[f64]| {
        let mut v = v.to_vec();
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    for (group, (f, raw, oct)) in &groups {
        eprintln!(
            "GRUPO {group:24} {} gravações: F1 mediana {:.2} (modelo {:.2}), erros de oitava {:.1}%",
            f.len(),
            median(f),
            median(raw),
            100.0 * oct.iter().sum::<f64>() / oct.len().max(1) as f64
        );
    }
    for ((group, label), t) in &note_techs {
        let pct = |k: usize| 100.0 * t[k] as f64 / t[1].max(1) as f64;
        eprintln!(
            "NOTA {group:24} rótulo {label:8} {} notas, {} achadas: bend {:.0}%, vibrato {:.0}%, slide {:.0}%, ligado {:.0}%",
            t[0],
            t[1],
            pct(2),
            pct(3),
            pct(4),
            pct(5)
        );
    }
    for ((group, label), t) in &techs {
        let pct = |k: usize| 100.0 * t[k] as f64 / t[0].max(1) as f64;
        eprintln!(
            "TÉCNICA {group:24} rótulo {label:8} {} notas: bend {:.0}%, vibrato {:.0}%, slide {:.0}%, ligado {:.0}%",
            t[0],
            pct(1),
            pct(2),
            pct(3),
            pct(4)
        );
    }
    let _ = std::fs::remove_dir_all(&work);
}
