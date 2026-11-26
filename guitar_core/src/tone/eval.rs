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
//! WAVs (target, estimate from notes, best preset) for listening,
//! `GVA_TONE_FLOOR` sets the search distance floor (dB), `GVA_TONE_PLUCK=0`
//! skips the pluck dry signal, `GVA_TONE_SAMPLES=<di.wav>:<notes.txt>,...`
//! adds the sampler dry signal, `GVA_TONE_STEMS=<eval_mix_bsrofo_sw.tsv>`
//! adds BS-RoFormer stems of the Marshall renders mixed over MUSDB18
//! backing ("sep-Marshall": estimated from the stem, scored against the
//! clean render), `GVA_TONE_STEMS_ONLY=1` keeps only items with a stem.

use super::cmaes::Rng;
use super::template::{PARAMS, Settings};
use super::sampler::Sampler;
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
    super::sampler::read_notes(&std::fs::read_to_string(path).unwrap_or_default())
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
    // Half the targets have a delay of 150-700 ms, clearly audible.
    let delay = (rng.uniform() < 0.5).then(|| 150.0 + 550.0 * rng.uniform() as f32);
    x[DELAY_MIX] = 0.3 + 0.6 * rng.uniform();
    Settings::new(x, voicing).with_delay(delay)
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
    let sampler = sampler_from_env();
    for item in items.split(',') {
        let (wav, notes) = item.split_once(':').expect("<di.wav>:<notes.txt>");
        let di = load_48k(Path::new(wav));
        let notes = load_notes(Path::new(notes));
        let pseudo = pluck::render(&notes, di.len(), RENDER_RATE);
        let (s, e) = super::loudest_window(&di, 30.0, RENDER_RATE);
        let t = features::Target::new(&di[s..e], RENDER_RATE);
        let c = t.compare(&pseudo[s..e]);
        eprintln!(
            "{wav}: pluck plain {:.2} dB, with EQ {:.2} dB, {} notes",
            features::plain_distance(&di[s..e], &pseudo[s..e], RENDER_RATE),
            c.distance,
            notes.len()
        );
        if let Some(sampler) = &sampler {
            let x = sampler.render(&notes, di.len());
            eprintln!(
                "    sampler plain {:.2} dB, with EQ {:.2} dB",
                features::plain_distance(&di[s..e], &x[s..e], RENDER_RATE),
                t.compare(&x[s..e]).distance
            );
        }
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

/// Echo scores ([`super::analysis::echo_delay`] without its threshold) on
/// EGDB: amp renders and the DI with no delay, and the DI through delays of
/// known time and mix, against the pluck dry signal. For setting
/// `analysis::ECHO_THRESHOLD`. Same `GVA_TONE_MANIFEST` as `eval_tone`.
#[test]
#[ignore]
fn echo_scores() {
    let Ok(manifest) = std::env::var("GVA_TONE_MANIFEST") else {
        eprintln!("set GVA_TONE_MANIFEST");
        return;
    };
    let mut items: BTreeMap<String, BTreeMap<String, PathBuf>> = BTreeMap::new();
    for line in std::fs::read_to_string(&manifest).unwrap().lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() >= 3 {
            let kind = f[2].rsplit('/').next().unwrap_or(f[2]).to_string();
            items.entry(f[1].to_string()).or_default().insert(kind, PathBuf::from(f[0]));
        }
    }
    let mut rng = Rng::new(11);
    let (mut none, mut with) = (Vec::new(), Vec::new());
    for (notes_path, group) in items.iter().step_by(3).take(16) {
        let Some(di_path) = group.get("DI") else { continue };
        let di = load_48k(di_path);
        let notes = load_notes(Path::new(notes_path));
        let dry = pluck::render(&notes, di.len(), RENDER_RATE);
        let (s, e) = super::loudest_window(&di, 12.0, RENDER_RATE);
        let score = |x: &[f32]| super::analysis::echo_score(&x[s..e], &dry[s..e], RENDER_RATE);
        for kind in ["DI", "Marshall", "Mesa", "Plexi"] {
            if let Some(p) = group.get(kind) {
                let mut x = load_48k(p);
                x.resize(di.len(), 0.0);
                none.push(score(&x));
            }
        }
        for _ in 0..2 {
            let ms = 150.0 + 550.0 * rng.uniform() as f32;
            let mix = 0.15 + 0.35 * rng.uniform() as f32;
            let delay = crate::shared::pedals::PedalInstance {
                enabled: true,
                description: crate::shared::pedals::PedalDescription::Delay(crate::shared::pedals::DelayParams {
                    delay_ms: ms,
                    feedback: 0.3,
                    gain: 1.0,
                    mix,
                    taps: 1,
                    damping: 0.3,
                }),
            };
            let x = render_at(vec![delay], &di, 0.1);
            let r = score(&x);
            eprintln!("delay {ms:.0} ms mix {mix:.2}: found {:.0} ms, score {:.3}", r.0, r.1);
            with.push((ms, r));
        }
    }
    {
        let get = |r: &(f32, f32)| *r;
        let mut n: Vec<f32> = none.iter().map(|r| r.1).collect();
        n.sort_by(f32::total_cmp);
        let quantile = |q: f64| n[((n.len() - 1) as f64 * q) as usize];
        eprintln!("no delay, {} scores: median {:.3}, 95th pct {:.3}, max {:.3}", n.len(), quantile(0.5), quantile(0.95), quantile(1.0));
        for thr in [super::analysis::ECHO_THRESHOLD, quantile(0.95), quantile(1.0)] {
            let hits = with.iter().filter(|(ms, r)| {
                let (found, v) = get(r);
                v >= thr && (found - ms).abs() <= 0.05 * ms
            }).count();
            let fa = n.iter().filter(|&&v| v >= thr).count();
            eprintln!("  threshold {thr:.3}: right delay {hits}/{}, false alarms {fa}/{}", with.len(), n.len());
        }
    }
}

/// `GVA_TONE_SAMPLES=<di.wav>:<notes.txt>,...`: single-note DI recordings
/// for the sampler dry signal (Guitar-TECHS `allsinglenotes`).
fn sampler_from_env() -> Option<Sampler> {
    let spec = std::env::var("GVA_TONE_SAMPLES").ok()?;
    let pairs: Vec<(PathBuf, PathBuf)> = spec
        .split(',')
        .filter_map(|p| p.split_once(':'))
        .map(|(a, b)| (PathBuf::from(a), PathBuf::from(b)))
        .collect();
    let refs: Vec<(&Path, &Path)> = pairs.iter().map(|(a, b)| (a.as_path(), b.as_path())).collect();
    match Sampler::load(&refs) {
        Ok(s) => Some(s),
        Err(e) => panic!("GVA_TONE_SAMPLES: {e}"),
    }
}

/// `x` shifted to line up with `reference` (RMS envelopes cross-correlated
/// over +-0.25 s): Guitar-TECHS's microphone tracks can be up to 100 ms off
/// their DI.
fn align_to(reference: &[f32], x: Vec<f32>) -> Vec<f32> {
    let hop = 48;
    let env = |v: &[f32]| -> Vec<f32> {
        v.chunks(hop).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt()).collect()
    };
    let (a, b) = (env(reference), env(&x));
    let max_lag = (0.25 * RENDER_RATE as f32) as i64 / hop as i64;
    let corr = |lag: i64| -> f32 {
        (0..a.len() as i64)
            .filter_map(|i| {
                let j = i + lag;
                (j >= 0 && (j as usize) < b.len()).then(|| a[i as usize] * b[j as usize])
            })
            .sum()
    };
    let lag = (-max_lag..=max_lag).max_by(|&p, &q| corr(p).total_cmp(&corr(q))).unwrap_or(0) * hop as i64;
    let mut out = vec![0f32; x.len()];
    for (i, o) in out.iter_mut().enumerate() {
        let j = i as i64 + lag;
        if j >= 0 && (j as usize) < x.len() {
            *o = x[j as usize];
        }
    }
    out
}

/// One target: the audio the estimator sees, and the audio its estimate is
/// scored against (the same, except for separated stems, which are scored
/// against the clean amp render they were separated from).
struct Case {
    group: String,
    seen: Vec<f32>,
    truth: Vec<f32>,
}

#[test]
#[ignore]
fn eval_tone() {
    let Ok(manifest) = std::env::var("GVA_TONE_MANIFEST") else {
        eprintln!("set GVA_TONE_MANIFEST (e.g. prepared/eval_egdb.tsv)");
        return;
    };
    let env = |k: &str, d: f64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let limit = env("GVA_TONE_LIMIT", 8.0) as usize;
    let oracle = std::env::var("GVA_TONE_ORACLE").map_or(true, |v| v != "0");
    let with_pluck = std::env::var("GVA_TONE_PLUCK").map_or(true, |v| v != "0");
    let out_dir = std::env::var("GVA_TONE_OUT").ok().map(PathBuf::from);
    if let Some(d) = &out_dir {
        std::fs::create_dir_all(d).unwrap();
    }
    let sampler = sampler_from_env();
    let opts = Options {
        max_secs: env("GVA_TONE_SECS", 12.0),
        generations: env("GVA_TONE_GENS", 30.0) as usize,
        floor_db: env("GVA_TONE_FLOOR", features::FLOOR_DB as f64) as f32,
        ..Options::default()
    };

    // notes file -> kind (DI, Marshall, ..., sep-Marshall) -> audio.
    let mut items: BTreeMap<String, BTreeMap<String, PathBuf>> = BTreeMap::new();
    let mut read = |path: &str, prefix: &str| {
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 3 {
                continue;
            }
            // EGDB: DI, Marshall, Mesa, Plexi; Guitar-TECHS: DI and the miked amp.
            let kind = match f[2].rsplit('/').next().unwrap_or(f[2]) {
                k if k.ends_with("directinput") => "DI".to_string(),
                k if k.ends_with("micamp") => "Micamp".to_string(),
                k => k.to_string(),
            };
            let kind = format!("{prefix}{kind}");
            items.entry(f[1].to_string()).or_default().insert(kind, PathBuf::from(f[0]));
        }
    };
    read(&manifest, "");
    if let Ok(stems) = std::env::var("GVA_TONE_STEMS") {
        read(&stems, "sep-");
    }
    let stems_only = std::env::var("GVA_TONE_STEMS_ONLY").is_ok_and(|v| v == "1");
    let mut items: Vec<_> = items
        .into_iter()
        .filter(|(_, g)| g.contains_key("DI") && (!stems_only || g.keys().any(|k| k.starts_with("sep-"))))
        .collect();
    let step = (items.len() / limit.max(1)).max(1);
    items = items.into_iter().step_by(step).take(limit).collect();

    let mut methods = vec!["dry DI", "best preset", "preset + match"];
    if with_pluck {
        methods.push("search, pluck");
    }
    if sampler.is_some() {
        methods.push("search, sampler");
    }
    if oracle {
        methods.push("search, true DI");
    }

    let mut scores: BTreeMap<String, Vec<Vec<f32>>> = BTreeMap::new();
    let mut param_err: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    // (dry signal, target has a delay) -> (right, total): delay time within
    // 5% when there is one, no delay found when there isn't.
    let mut delay_hits: BTreeMap<(&str, bool), (usize, usize)> = BTreeMap::new();
    let mut rng = Rng::new(7);
    for (n, (notes_path, group)) in items.iter().enumerate() {
        let di = load_48k(&group["DI"]);
        let notes = load_notes(Path::new(notes_path));
        let item = Path::new(notes_path).file_stem().unwrap().to_string_lossy().to_string();
        let mut drys: Vec<(&str, Vec<f32>)> = Vec::new();
        if with_pluck {
            drys.push(("pluck", pluck::render(&notes, di.len(), RENDER_RATE)));
        }
        if let Some(s) = &sampler {
            drys.push(("sampler", s.render(&notes, di.len())));
        }

        let truth = random_settings(&mut rng);
        let cab = random_cab(&mut rng);
        let chain = matching::convolve(&render_at(truth.pedals(), &di, truth.level()), &cab);
        let mut cases = vec![Case { group: "chain".into(), seen: chain.clone(), truth: chain }];
        let fit = |mut x: Vec<f32>| {
            x.resize(di.len(), 0.0);
            x
        };
        for kind in ["Marshall", "Mesa", "Plexi", "Micamp"] {
            if let Some(p) = group.get(kind) {
                let wet = align_to(&di, fit(load_48k(p)));
                if let Some(stem) = group.get(&format!("sep-{kind}")) {
                    cases.push(Case { group: format!("sep-{kind}"), seen: fit(load_48k(stem)), truth: wet.clone() });
                }
                cases.push(Case { group: kind.to_string(), seen: wet.clone(), truth: wet });
            }
        }

        for case in &cases {
            let t0 = std::time::Instant::now();
            let mut row = Vec::new();
            let mut first: Option<super::Estimate> = None;
            let score = |est: &super::Estimate| {
                let (s, e) = est.excerpt;
                (
                    features::plain_distance(&case.truth[s..e], &est.best.render(&di[s..e]), RENDER_RATE),
                    est.excerpt,
                )
            };
            for (name, dry) in &drys {
                let est = estimate(&case.seen, dry, &opts);
                if first.is_none() {
                    let (s, e) = est.excerpt;
                    let goal = &case.truth[s..e];
                    let dry = &di[s..e];
                    row.push(features::plain_distance(goal, dry, RENDER_RATE));
                    row.push(features::plain_distance(
                        goal,
                        &render_at(est.preset.pedals.clone(), dry, est.preset.level),
                        RENDER_RATE,
                    ));
                    row.push(features::plain_distance(goal, &est.preset.render(dry), RENDER_RATE));
                }
                row.push(score(&est).0);
                let true_delay = truth.delay_ms.filter(|_| case.group == "chain");
                let entry = delay_hits.entry((name, true_delay.is_some())).or_default();
                entry.1 += 1;
                let right = match (true_delay, est.echo) {
                    (Some(t), Some((ms, _))) => (ms - t).abs() <= 0.05 * t,
                    (None, None) => true,
                    _ => false,
                };
                if right {
                    entry.0 += 1;
                }
                if case.group == "chain" {
                    let err = est.settings.x.iter().zip(&truth.x).map(|(a, b)| (a - b).abs()).sum::<f64>()
                        / PARAMS.len() as f64;
                    param_err.entry(name).or_default().push(err);
                }
                if let Some(d) = &out_dir {
                    let (s, e) = est.excerpt;
                    let tag = format!("{item}_{}", case.group);
                    write_wav(d, &format!("{tag}_estimate_{name}.wav"), &est.best.render(&di[s..e]));
                    if first.is_none() {
                        write_wav(d, &format!("{tag}_target.wav"), &case.truth[s..e]);
                        write_wav(d, &format!("{tag}_preset.wav"), &est.preset.render(&di[s..e]));
                    }
                }
                first.get_or_insert(est);
            }
            if oracle {
                let est = estimate(&case.seen, &di, &opts);
                row.push(score(&est).0);
            }
            eprintln!(
                "[{}/{}] {item} {}: {} (preset: {}) {:.0}s",
                n + 1,
                items.len(),
                case.group,
                row.iter().map(|v| format!("{v:.2}")).collect::<Vec<_>>().join(" / "),
                first.as_ref().map_or("-".into(), |e| e.preset.label.clone()),
                t0.elapsed().as_secs_f64()
            );
            scores.entry(case.group.clone()).or_default().push(row);
        }
    }

    eprintln!("\nMean distance to the target (dB, lower is better), true DI through each estimate:");
    eprintln!("| target | {} |", methods.join(" | "));
    eprintln!("|---{}|", "|---".repeat(methods.len()));
    for (group, rows) in &scores {
        let cols = rows[0].len();
        let means: Vec<String> = (0..cols)
            .map(|c| format!("{:.2}", rows.iter().map(|r| r[c]).sum::<f32>() / rows.len() as f32))
            .collect();
        eprintln!("| {group} (n={}) | {} |", rows.len(), means.join(" | "));
    }
    for ((name, has), (ok, n)) in &delay_hits {
        let what = if *has { "delay time within 5%" } else { "no delay found where none" };
        eprintln!("{name}: {what} {ok}/{n}");
    }
    for (name, errs) in &param_err {
        eprintln!(
            "chain targets, {name}: mean |parameter error| {:.3} (unit scale)",
            errs.iter().sum::<f64>() / errs.len() as f64
        );
    }
}
