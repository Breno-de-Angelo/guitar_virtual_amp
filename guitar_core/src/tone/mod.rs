//! Tone from a recording: finds settings of the app's own pedals that make
//! a dry guitar sound like a target recording (a separated guitar stem, no
//! DI). Design and literature: `docs/papers/tone-estimation/` (Section 10).
//!
//! The estimate is analysis by synthesis with the real chain as the forward
//! model:
//! 1. a dry signal for the target's notes: the user's DI when it exists, or
//!    the transcribed notes played by a plucked-string model ([`pluck`]);
//! 2. the factory presets as a baseline and starting points;
//! 3. a gradient-free search ([`cmaes`]) over a template chain
//!    ([`template`]: input level, compressor, distortion, amp, reverb), one
//!    run per amp voicing, rendering through `backend::offline::render_at`
//!    and scoring with a distance that forgives a smooth EQ
//!    ([`features`]);
//! 4. that EQ turned into a minimum-phase impulse response for the Cabinet
//!    pedal ([`matching`]), placed last in the chain.
//!
//! Blocking and CPU-heavy: call it from a worker thread. Not yet wired to
//! the UI; `eval_tone` (ignored test) measures it.

pub mod cmaes;
pub mod features;
pub mod matching;
pub mod pluck;
pub mod template;

#[cfg(test)]
mod eval;

use crate::backend::offline::{RENDER_RATE, render_at};
use crate::shared::pedals::{AmpVoicing, CabinetParams, PedalDescription, PedalInstance};
use features::{Comparison, Target};
use template::Settings;

/// Search budget and parallelism.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Longest excerpt analysed, in seconds (the loudest stretch is used).
    pub max_secs: f64,
    pub population: usize,
    pub generations: usize,
    pub threads: usize,
    pub seed: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_secs: 12.0,
            population: 16,
            generations: 30,
            threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
            seed: 1,
        }
    }
}

/// A chain plus the matching IR that goes after it.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// What it is: a factory preset name or "search".
    pub label: String,
    pub pedals: Vec<PedalInstance>,
    /// RMS level the dry signal was played at.
    pub level: f32,
    /// Distance to the target (dB, EQ forgiven).
    pub distance: f32,
    pub eq_db: Vec<f32>,
    pub eq_hz: Vec<f32>,
}

impl Candidate {
    /// The matching filter as an impulse response at [`RENDER_RATE`].
    pub fn ir(&self) -> Vec<f32> {
        matching::ir_from_eq(&self.eq_db, &self.eq_hz, RENDER_RATE)
    }

    /// The full chain for a preset, with the matching IR (written at
    /// `ir_path`) in a Cabinet pedal at the end.
    pub fn chain_with_ir(&self, ir_path: &str) -> Vec<PedalInstance> {
        let mut chain = self.pedals.clone();
        chain.push(PedalInstance {
            enabled: true,
            description: PedalDescription::Cabinet(CabinetParams {
                ir_path: Some(ir_path.to_string()),
                mix: 1.0,
                source: None,
            }),
        });
        chain
    }

    /// `dry` through the chain and the matching filter, as the estimate
    /// would sound (without the Cabinet pedal's one-block latency).
    pub fn render(&self, dry: &[f32]) -> Vec<f32> {
        matching::convolve(&render_at(self.pedals.clone(), dry, self.level), &self.ir())
    }
}

/// Result of [`estimate`].
#[derive(Clone, Debug)]
pub struct Estimate {
    /// The search result.
    pub best: Candidate,
    /// The closest factory preset (the baseline).
    pub preset: Candidate,
    /// Template settings of the search result.
    pub settings: Settings,
    /// Excerpt used, in samples of the input: `start..end`.
    pub excerpt: (usize, usize),
    pub evaluations: usize,
}

/// Factory presets that make sense as a static guitar tone (the looper
/// records and replays, the pitch shifter changes the notes).
fn preset_candidates() -> Vec<crate::shared::preset::Preset> {
    crate::shared::factory_presets::factory_presets()
        .into_iter()
        .filter(|p| {
            !p.pedals.iter().any(|i| {
                matches!(
                    i.description,
                    PedalDescription::Looper(_) | PedalDescription::PitchShift(_)
                )
            })
        })
        .collect()
}

/// Start and end of the `secs`-long window of `x` with the most energy.
pub fn loudest_window(x: &[f32], secs: f64, rate: u32) -> (usize, usize) {
    let len = ((secs * rate as f64) as usize).min(x.len());
    if len == x.len() {
        return (0, len);
    }
    let hop = (rate / 10) as usize;
    let block: Vec<f32> = x
        .chunks(hop)
        .map(|c| c.iter().map(|v| v * v).sum())
        .collect();
    let span = len / hop;
    let mut best = (0, f32::MIN);
    let mut sum: f32 = block[..span.min(block.len())].iter().sum();
    for start in 0..block.len().saturating_sub(span) {
        if sum > best.1 {
            best = (start, sum);
        }
        sum += block[start + span] - block[start];
    }
    let s = best.0 * hop;
    (s, (s + len).min(x.len()))
}

fn compare(target: &Target, pedals: Vec<PedalInstance>, dry: &[f32], level: f32) -> Comparison {
    target.compare(&render_at(pedals, dry, level))
}

/// Finds the settings that make `dry` (48 kHz mono, aligned with the
/// target) sound like `target` (48 kHz mono).
pub fn estimate(target: &[f32], dry: &[f32], opts: &Options) -> Estimate {
    let len = target.len().min(dry.len());
    let (start, end) = loudest_window(&target[..len], opts.max_secs, RENDER_RATE);
    let (target, dry) = (&target[start..end], &dry[start..end]);
    let goal = Target::new(target, RENDER_RATE);

    // Baseline: every factory preset at the default level, in parallel.
    let presets = preset_candidates();
    let level = 0.1;
    let mut scored: Vec<Candidate> = std::thread::scope(|s| {
        let handles: Vec<_> = presets
            .iter()
            .map(|p| {
                let goal = &goal;
                s.spawn(move || {
                    let c = compare(goal, p.pedals.clone(), dry, level);
                    Candidate {
                        label: p.name.clone(),
                        pedals: p.pedals.clone(),
                        level,
                        distance: c.distance,
                        eq_db: c.eq_db,
                        eq_hz: c.eq_hz,
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    scored.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    let preset = scored[0].clone();
    let mut evaluations = scored.len();

    // Search: one CMA-ES run per voicing.
    let mut best: Option<(Settings, f64)> = None;
    for (k, voicing) in [AmpVoicing::Clean, AmpVoicing::Crunch, AmpVoicing::Lead]
        .into_iter()
        .enumerate()
    {
        let f = |x: &[f64]| -> f64 {
            let s = Settings::new(x.to_vec(), voicing);
            compare(&goal, s.pedals(), dry, s.level()).distance as f64
        };
        let found = cmaes::minimize(
            &f,
            &Settings::start(voicing).x,
            cmaes::Settings {
                population: opts.population,
                generations: opts.generations,
                sigma: 0.3,
                seed: opts.seed.wrapping_add(k as u64),
                threads: opts.threads,
            },
        );
        evaluations += found.evaluations;
        if best.as_ref().is_none_or(|b| found.value < b.1) {
            best = Some((Settings::new(found.x, voicing), found.value));
        }
    }
    let (settings, _) = best.expect("three voicings searched");
    let c = compare(&goal, settings.pedals(), dry, settings.level());
    let best = Candidate {
        label: "search".to_string(),
        pedals: settings.pedals(),
        level: settings.level(),
        distance: c.distance,
        eq_db: c.eq_db,
        eq_hz: c.eq_hz,
    };
    Estimate {
        best,
        preset,
        settings,
        excerpt: (start, end),
        evaluations,
    }
}

/// [`estimate`] with the dry signal played from the target's notes
/// (seconds from the start of `target`).
pub fn estimate_from_notes(target: &[f32], notes: &[pluck::Note], opts: &Options) -> Estimate {
    let dry = pluck::render(notes, target.len(), RENDER_RATE);
    estimate(target, &dry, opts)
}
