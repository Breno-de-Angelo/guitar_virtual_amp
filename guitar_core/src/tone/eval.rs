//! Tone estimation benchmark (ignored test), on EGDB: each item has the DI,
//! three amp-sim renders of the same performance (Marshall, Mesa, Plexi:
//! equipment outside the app's chain) and note labels.
//!
//! Targets per item: the DI through a random template chain plus a random
//! cabinet-like EQ ("chain", settings known), and the three amp renders.
//! The estimator sees only the target and a dry signal: the labelled notes
//! played by the pluck model (the real use case), or, as an upper bound,
//! the true DI. Every method is scored by rendering the true DI through its
//! estimate and comparing with the target, with no EQ allowance
//! ([`features::plain_distance`], mean absolute dB over bands and frames,
//! two resolutions). Lower is better; 0 = identical.
//!
//! ```text
//! GVA_TONE_MANIFEST=~/.cache/gva_test/prepared/eval_egdb.tsv \
//!   cargo test --release --workspace --exclude guitar_android eval_tone -- --ignored --nocapture
//! ```
//! `GVA_TONE_LIMIT` items (default 8), `GVA_TONE_GENS` CMA-ES generations
//! (default 30), `GVA_TONE_SECS` excerpt length (default 12),
//! `GVA_TONE_ORACLE=0` skips the true-DI search, `GVA_TONE_OUT=<dir>` writes
//! WAVs (target, estimate from notes, best preset) for listening.

use super::cmaes::Rng;
use super::template::{PARAMS, Settings};
use super::{Options, estimate, features, matching, pluck};
use crate::backend::offline::{RENDER_RATE, render_at};
use crate::shared::pedals::AmpVoicing;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn load_48k(path: &Path) -> Vec<f32> {
    let a =
        crate::backend::audio_file::decode_file(path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    crate::transcribe::resample::resample(&a.samples, a.sample_rate, RENDER_RATE)
}

fn load_notes(path: &Path) -> Vec<pluck::Note> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            Some(pluck::Note {
                start: f.first()?.parse().ok()?,
                end: f.get(1)?.parse().ok()?,
                pitch: f.get(2)?.parse::<f32>().ok()?.round() as u8,
                velocity: f.get(3).and_then(|v| v.parse::<f32>().ok()).unwrap_or(90.0) / 127.0,
            })
        })
        .collect()
}

/// A random chain of the template: what the in-chain targets are made of.
fn random_settings(rng: &mut Rng) -> Settings {
    let voicing = [AmpVoicing::Clean, AmpVoicing::Crunch, AmpVoicing::Lead]
        [(rng.uniform() * 3.0) as usize % 3];
    let mut x: Vec<f64> = (0..PARAMS.len())
        .map(|_| 0.1 + 0.8 * rng.uniform())
        .collect();
    use super::template::*;
    if rng.uniform() < 0.5 {
        x[COMP_AMOUNT] = 0.0;
    }
    if rng.uniform() < 0.4 {
        x[DRIVE_MIX] = 0.0;
    }
    if rng.uniform() < 0.5 {
        x[REVERB_MIX] = 0.0;
    }
    Settings::new(x, voicing)
}

/// A random cabinet-like response: high-pass near 80 Hz, a roll-off above
/// 3-7 kHz, and smooth bumps of up to ±4 dB.
fn random_cab(rng: &mut Rng) -> Vec<f32> {
    let cutoff = 3000.0 + 4000.0 * rng.uniform() as f32;
    let bumps: Vec<f32> = (0..6)
        .map(|_| (rng.uniform() as f32 * 2.0 - 1.0) * 4.0)
        .collect();
    let hz: Vec<f32> = (0..32)
        .map(|i| 50.0 * (16_000f32 / 50.0).powf(i as f32 / 31.0))
        .collect();
    let db: Vec<f32> = hz
        .iter()
        .enumerate()
        .map(|(i, &f)| {
            let low = if f < 80.0 {
                -12.0 * (80.0 / f).log2()
            } else {
                0.0
            };
            let high = if f > cutoff {
                -24.0 * (f / cutoff).log2()
            } else {
                0.0
            };
            let bump = bumps[(i * bumps.len() / hz.len()).min(bumps.len() - 1)];
            (low + high + bump).max(-40.0)
        })
        .collect();
    matching::ir_from_eq(&db, &hz, RENDER_RATE)
}

fn write_wav(dir: &Path, name: &str, x: &[f32]) {
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RENDER_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(dir.join(name), spec).unwrap();
    for v in x {
        w.write_sample((v / peak * 0.7 * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();
}

/// How far the pluck model's rendering of the notes is from the real DI of
/// the same performance, overall and once an EQ is allowed, plus the mean
/// long-term spectrum difference (DI minus pluck) per band. Pass items as
/// `GVA_PLUCK_ITEMS=<di.wav>:<notes.txt>,...`.
#[test]
#[ignore]
fn pluck_vs_di() {
    let Ok(items) = std::env::var("GVA_PLUCK_ITEMS") else {
        eprintln!("set GVA_PLUCK_ITEMS=<di.wav>:<notes.txt>,...");
        return;
    };
    let mut mean_eq: Option<(Vec<f32>, Vec<f32>)> = None;
    let mut count = 0;
    for item in items.split(',') {
        let (wav, notes) = item.split_once(':').expect("<di.wav>:<notes.txt>");
        let di = load_48k(Path::new(wav));
        let notes = load_notes(Path::new(notes));
        let pseudo = pluck::render(&notes, di.len(), RENDER_RATE);
        let (s, e) = super::loudest_window(&di, 30.0, RENDER_RATE);
        let t = features::Target::new(&di[s..e], RENDER_RATE);
        let c = t.compare(&pseudo[s..e]);
        eprintln!(
            "{wav}: plain {:.2} dB, with EQ {:.2} dB, {} notes",
            features::plain_distance(&di[s..e], &pseudo[s..e], RENDER_RATE),
            c.distance,
            notes.len()
        );
        let acc = mean_eq.get_or_insert((vec![0.0; c.eq_db.len()], c.eq_hz.clone()));
        acc.0.iter_mut().zip(&c.eq_db).for_each(|(a, v)| *a += v);
        count += 1;
    }
    if let Some((eq, hz)) = mean_eq {
        let line: Vec<String> = eq
            .iter()
            .zip(&hz)
            .map(|(v, f)| format!("{f:.0}:{:.1}", v / count as f32))
            .collect();
        eprintln!("mean EQ (DI - pluck, dB): {}", line.join(" "));
    }
}

/// Fits the pluck model's settings to real DI recordings (same item format
/// as [`pluck_vs_di`]) by CMA-ES on the EQ-forgiven distance, then prints
/// the settings and the remaining mean EQ (DI minus pluck) to put in
/// `pluck.rs`. Fit on Guitar-TECHS, not on EGDB (the benchmark).
#[test]
#[ignore]
fn fit_pluck() {
    let Ok(items) = std::env::var("GVA_PLUCK_ITEMS") else {
        eprintln!("set GVA_PLUCK_ITEMS=<di.wav>:<notes.txt>,...");
        return;
    };
    let data: Vec<(features::Target, Vec<pluck::Note>, usize, usize)> = items
        .split(',')
        .map(|item| {
            let (wav, notes) = item.split_once(':').expect("<di.wav>:<notes.txt>");
            let di = load_48k(Path::new(wav));
            let (s, e) = super::loudest_window(&di, 20.0, RENDER_RATE);
            let notes: Vec<pluck::Note> = load_notes(Path::new(notes))
                .into_iter()
                .filter(|n| {
                    n.end > s as f64 / RENDER_RATE as f64 && n.start < e as f64 / RENDER_RATE as f64
                })
                .map(|mut n| {
                    n.start -= s as f64 / RENDER_RATE as f64;
                    n.end -= s as f64 / RENDER_RATE as f64;
                    n
                })
                .collect();
            (features::Target::new(&di[s..e], RENDER_RATE), notes, s, e)
        })
        .collect();
    let space = [
        super::template::Param {
            name: "pluck position",
            lo: 0.05,
            hi: 0.5,
            log: false,
        },
        super::template::Param {
            name: "release (s)",
            lo: 0.02,
            hi: 0.5,
            log: true,
        },
        super::template::Param {
            name: "low E T60 (s)",
            lo: 0.5,
            hi: 40.0,
            log: true,
        },
        super::template::Param {
            name: "brightness",
            lo: 0.002,
            hi: 0.9,
            log: true,
        },
        super::template::Param {
            name: "velocity brightness",
            lo: 0.0,
            hi: 0.9,
            log: false,
        },
    ];
    let model = |x: &[f64]| pluck::Model {
        pluck_position: space[0].value(x[0]),
        release_secs: space[1].value(x[1]),
        low_e_t60: space[2].value(x[2]),
        brightness: space[3].value(x[3]),
        velocity_brightness: space[4].value(x[4]),
        di_eq: false,
    };
    let cost = |m: &pluck::Model| -> (f64, Vec<f32>, Vec<f32>) {
        let mut total = 0.0;
        let mut eq: Vec<f32> = Vec::new();
        let mut hz = Vec::new();
        for (t, notes, s, e) in &data {
            let x = pluck::render_with(m, notes, e - s, RENDER_RATE);
            let c = t.compare(&x);
            total += c.distance as f64;
            if eq.is_empty() {
                eq = vec![0.0; c.eq_db.len()];
                hz = c.eq_hz.clone();
            }
            eq.iter_mut()
                .zip(&c.eq_db)
                .for_each(|(a, v)| *a += v / data.len() as f32);
        }
        (total / data.len() as f64, eq, hz)
    };
    let start = pluck::Model {
        di_eq: false,
        ..pluck::Model::default()
    };
    eprintln!("default: {:.2} dB", cost(&start).0);
    let x0: Vec<f64> = vec![
        space[0].unit(start.pluck_position),
        space[1].unit(start.release_secs),
        space[2].unit(start.low_e_t60),
        space[3].unit(start.brightness),
        space[4].unit(start.velocity_brightness),
    ];
    let found = super::cmaes::minimize(
        &|x: &[f64]| cost(&model(x)).0,
        &x0,
        super::cmaes::Settings {
            population: 12,
            generations: 25,
            sigma: 0.25,
            seed: 3,
            threads: 12,
        },
    );
    let m = model(&found.x);
    let (d, eq, hz) = cost(&m);
    eprintln!("fitted: {d:.2} dB, {m:?}");
    let table: Vec<String> = hz
        .iter()
        .zip(&eq)
        .map(|(f, v)| format!("({f:.0}.0, {v:.1})"))
        .collect();
    eprintln!("DI_EQ: [{}]", table.join(", "));
    let fixed = pluck::Model { di_eq: true, ..m };
    let mut plain = 0.0;
    for (t, notes, s, e) in &data {
        let _ = t;
        let x = pluck::render_with(&fixed, notes, e - s, RENDER_RATE);
        plain += t.compare(&x).distance;
    }
    eprintln!(
        "(EQ-forgiven distance with the DI EQ applied: {:.2} dB)",
        plain / data.len() as f32
    );
}

const METHODS: [&str; 5] = [
    "dry DI",
    "best preset",
    "preset + match",
    "search + match (notes)",
    "search + match (true DI)",
];

#[test]
#[ignore]
fn eval_tone() {
    let Ok(manifest) = std::env::var("GVA_TONE_MANIFEST") else {
        eprintln!("set GVA_TONE_MANIFEST (e.g. prepared/eval_egdb.tsv)");
        return;
    };
    let env = |k: &str, d: f64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let limit = env("GVA_TONE_LIMIT", 8.0) as usize;
    let oracle = std::env::var("GVA_TONE_ORACLE").map_or(true, |v| v != "0");
    let out_dir = std::env::var("GVA_TONE_OUT").ok().map(PathBuf::from);
    if let Some(d) = &out_dir {
        std::fs::create_dir_all(d).unwrap();
    }
    let opts = Options {
        max_secs: env("GVA_TONE_SECS", 12.0),
        generations: env("GVA_TONE_GENS", 30.0) as usize,
        ..Options::default()
    };

    // notes file -> group suffix (DI, Marshall, ...) -> audio.
    let mut items: BTreeMap<String, BTreeMap<String, PathBuf>> = BTreeMap::new();
    for line in std::fs::read_to_string(&manifest).unwrap().lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 3 {
            continue;
        }
        let kind = f[2].rsplit('/').next().unwrap_or(f[2]).to_string();
        items
            .entry(f[1].to_string())
            .or_default()
            .insert(kind, PathBuf::from(f[0]));
    }
    let mut items: Vec<_> = items
        .into_iter()
        .filter(|(_, g)| g.contains_key("DI"))
        .collect();
    let step = (items.len() / limit.max(1)).max(1);
    items = items.into_iter().step_by(step).take(limit).collect();

    // group -> method -> scores
    let mut scores: BTreeMap<String, Vec<Vec<f32>>> = BTreeMap::new();
    let mut param_err = Vec::new();
    let mut voicing_hits = (0, 0);
    let mut rng = Rng::new(7);
    for (n, (notes_path, group)) in items.iter().enumerate() {
        let di = load_48k(&group["DI"]);
        let notes = load_notes(Path::new(notes_path));
        let pseudo = pluck::render(&notes, di.len(), RENDER_RATE);
        let item = Path::new(notes_path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let truth = random_settings(&mut rng);
        let cab = random_cab(&mut rng);
        let mut targets: Vec<(String, Vec<f32>)> = vec![(
            "chain".to_string(),
            matching::convolve(&render_at(truth.pedals(), &di, truth.level()), &cab),
        )];
        for kind in ["Marshall", "Mesa", "Plexi"] {
            if let Some(p) = group.get(kind) {
                let mut wet = load_48k(p);
                wet.resize(di.len(), 0.0);
                targets.push((kind.to_string(), wet));
            }
        }

        for (kind, target) in &targets {
            let t0 = std::time::Instant::now();
            let est = estimate(target, &pseudo, &opts);
            let (s, e) = est.excerpt;
            let (goal, dry) = (&target[s..e], &di[s..e]);
            let score = |x: &[f32]| features::plain_distance(goal, x, RENDER_RATE);
            let mut row = vec![
                score(dry),
                score(&render_at(est.preset.pedals.clone(), dry, est.preset.level)),
                score(&est.preset.render(dry)),
                score(&est.best.render(dry)),
            ];
            if oracle {
                let o = estimate(target, &di, &opts);
                let (os, oe) = o.excerpt;
                row.push(features::plain_distance(
                    &target[os..oe],
                    &o.best.render(&di[os..oe]),
                    RENDER_RATE,
                ));
            }
            if kind == "chain" {
                let err: f64 = est
                    .settings
                    .x
                    .iter()
                    .zip(&truth.x)
                    .map(|(a, b)| (a - b).abs())
                    .sum::<f64>()
                    / PARAMS.len() as f64;
                param_err.push(err);
                voicing_hits.1 += 1;
                if est.settings.voicing == truth.voicing {
                    voicing_hits.0 += 1;
                }
                eprintln!("    truth:    {}", truth.describe());
                eprintln!("    estimate: {}", est.settings.describe());
            }
            eprintln!(
                "[{}/{}] {item} {kind}: {} (preset: {}) {:.0}s",
                n + 1,
                items.len(),
                row.iter()
                    .map(|v| format!("{v:.2}"))
                    .collect::<Vec<_>>()
                    .join(" / "),
                est.preset.label,
                t0.elapsed().as_secs_f64()
            );
            if let Some(d) = &out_dir {
                write_wav(d, &format!("{item}_{kind}_target.wav"), goal);
                write_wav(
                    d,
                    &format!("{item}_{kind}_estimate.wav"),
                    &est.best.render(dry),
                );
                write_wav(
                    d,
                    &format!("{item}_{kind}_preset.wav"),
                    &est.preset.render(dry),
                );
            }
            scores.entry(kind.clone()).or_default().push(row);
        }
    }

    eprintln!(
        "\nMean distance to the target (dB, lower is better), true DI through each estimate:"
    );
    let cols = if oracle {
        METHODS.len()
    } else {
        METHODS.len() - 1
    };
    eprintln!("{:<10} {}", "target", METHODS[..cols].join(" | "));
    for (kind, rows) in &scores {
        let means: Vec<String> = (0..cols)
            .map(|c| {
                format!(
                    "{:.2}",
                    rows.iter().map(|r| r[c]).sum::<f32>() / rows.len() as f32
                )
            })
            .collect();
        eprintln!("{kind:<10} {} (n={})", means.join(" | "), rows.len());
    }
    if !param_err.is_empty() {
        eprintln!(
            "chain targets: mean |parameter error| {:.3} (unit scale), voicing right {}/{}",
            param_err.iter().sum::<f64>() / param_err.len() as f64,
            voicing_hits.0,
            voicing_hits.1
        );
    }
}
