//! The chain the tone search fits: compressor -> distortion -> amp ->
//! delay -> reverb, played at a searched input level. The delay time is not
//! searched: it comes from [`super::analysis::echo_delay`], and without an
//! echo there is no delay pedal. Every parameter lives on
//! [0, 1] for the optimiser and maps to a pedal range. A pedal whose amount
//! is near zero is left out of the chain. The amp's tone stack is not
//! searched: after the clipping it is linear, so the final matching EQ
//! covers it.

use crate::shared::pedals::{
    AmpParams, AmpVoicing, CompressorParams, DelayParams, DistortionParams, PedalDescription,
    PedalInstance, ReverbParams,
};

/// One searched parameter.
#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub name: &'static str,
    pub lo: f32,
    pub hi: f32,
    /// Interpolate on a log scale (for levels and thresholds).
    pub log: bool,
}

impl Param {
    pub fn value(&self, x: f64) -> f32 {
        let x = x.clamp(0.0, 1.0) as f32;
        if self.log {
            self.lo * (self.hi / self.lo).powf(x)
        } else {
            self.lo + (self.hi - self.lo) * x
        }
    }

    pub fn unit(&self, v: f32) -> f64 {
        let v = v.clamp(self.lo.min(self.hi), self.lo.max(self.hi));
        (if self.log {
            (v / self.lo).ln() / (self.hi / self.lo).ln()
        } else {
            (v - self.lo) / (self.hi - self.lo)
        }) as f64
    }
}

pub const LEVEL: usize = 0;
pub const COMP_AMOUNT: usize = 1;
pub const COMP_THRESHOLD: usize = 2;
pub const DRIVE_MIX: usize = 3;
pub const DRIVE: usize = 4;
pub const DRIVE_TONE: usize = 5;
pub const AMP_GAIN: usize = 6;
pub const REVERB_MIX: usize = 7;
pub const REVERB_ROOM: usize = 8;
pub const REVERB_DAMPING: usize = 9;
pub const DELAY_MIX: usize = 10;
pub const DELAY_FEEDBACK: usize = 11;

pub const PARAMS: [Param; 12] = [
    Param {
        name: "input level (RMS)",
        lo: 0.01,
        hi: 0.4,
        log: true,
    },
    Param {
        name: "compressor amount",
        lo: 0.0,
        hi: 1.0,
        log: false,
    },
    Param {
        name: "compressor threshold",
        lo: 0.02,
        hi: 0.5,
        log: true,
    },
    Param {
        name: "drive mix",
        lo: 0.0,
        hi: 1.0,
        log: false,
    },
    Param {
        name: "drive",
        lo: 0.5,
        hi: 10.0,
        log: true,
    },
    Param {
        name: "drive tone",
        lo: 0.15,
        hi: 1.0,
        log: false,
    },
    Param {
        name: "amp gain",
        lo: 0.5,
        hi: 5.0,
        log: true,
    },
    Param {
        name: "reverb mix",
        lo: 0.0,
        hi: 0.5,
        log: false,
    },
    Param {
        name: "reverb room",
        lo: 0.0,
        hi: 0.95,
        log: false,
    },
    Param {
        name: "reverb damping",
        lo: 0.0,
        hi: 0.95,
        log: false,
    },
    Param {
        name: "delay mix",
        lo: 0.0,
        hi: 0.5,
        log: false,
    },
    Param {
        name: "delay feedback",
        lo: 0.0,
        hi: 0.7,
        log: false,
    },
];

/// Below this amount (on [0, 1]) a pedal is left out.
const OFF: f64 = 0.04;
/// Largest compression ratio, at amount 1.
const MAX_RATIO: f32 = 8.0;

/// A point of the search: the unit-cube vector plus the amp voicing, which
/// is searched separately because it's discrete, and the delay time, which
/// is measured.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub x: Vec<f64>,
    pub voicing: AmpVoicing,
    pub delay_ms: Option<f32>,
}

impl Settings {
    pub fn new(x: Vec<f64>, voicing: AmpVoicing) -> Self {
        Self {
            x,
            voicing,
            delay_ms: None,
        }
    }

    pub fn with_delay(mut self, delay_ms: Option<f32>) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    /// A neutral start: clean level, a little of everything.
    pub fn start(voicing: AmpVoicing) -> Self {
        let mut x = vec![0.5; PARAMS.len()];
        x[LEVEL] = PARAMS[LEVEL].unit(0.1);
        x[COMP_AMOUNT] = 0.2;
        x[DRIVE_MIX] = 0.3;
        x[REVERB_MIX] = 0.15;
        x[DELAY_MIX] = 0.3;
        x[DELAY_FEEDBACK] = 0.3;
        Self::new(x, voicing)
    }

    pub fn value(&self, i: usize) -> f32 {
        PARAMS[i].value(self.x[i])
    }

    /// RMS level the dry signal is played at.
    pub fn level(&self) -> f32 {
        self.value(LEVEL)
    }

    /// The pedals, in chain order.
    pub fn pedals(&self) -> Vec<PedalInstance> {
        let mut chain = Vec::new();
        let on = |d: PedalDescription| PedalInstance {
            enabled: true,
            description: d,
        };
        if self.x[COMP_AMOUNT] > OFF {
            chain.push(on(PedalDescription::Compressor(CompressorParams {
                threshold: self.value(COMP_THRESHOLD),
                ratio: 1.0 + (MAX_RATIO - 1.0) * self.value(COMP_AMOUNT),
                attack_ms: 10.0,
                release_ms: 150.0,
                makeup_gain: 1.0,
            })));
        }
        if self.x[DRIVE_MIX] > OFF {
            chain.push(on(PedalDescription::Distortion(DistortionParams {
                drive: self.value(DRIVE),
                tone: self.value(DRIVE_TONE),
                level: 1.0,
                mix: self.value(DRIVE_MIX),
                bias: 0.0,
            })));
        }
        chain.push(on(PedalDescription::Amp(AmpParams {
            gain: self.value(AMP_GAIN),
            voicing: self.voicing,
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
        })));
        if let Some(delay_ms) = self.delay_ms
            && self.x[DELAY_MIX] > OFF
        {
            chain.push(on(PedalDescription::Delay(DelayParams {
                delay_ms,
                feedback: self.value(DELAY_FEEDBACK),
                gain: 1.0,
                mix: self.value(DELAY_MIX),
                taps: 1,
                damping: 0.3,
            })));
        }
        if self.x[REVERB_MIX] > OFF {
            chain.push(on(PedalDescription::Reverb(ReverbParams {
                room_size: self.value(REVERB_ROOM),
                mix: self.value(REVERB_MIX),
                damping: self.value(REVERB_DAMPING),
            })));
        }
        chain
    }

    /// One line per parameter, for logs.
    pub fn describe(&self) -> String {
        let mut s = format!("voicing {:?}", self.voicing);
        match self.delay_ms {
            Some(ms) => s.push_str(&format!(", delay {ms:.0} ms")),
            None => s.push_str(", no delay"),
        }
        for (i, p) in PARAMS.iter().enumerate() {
            s.push_str(&format!(", {} {:.2}", p.name, self.value(i)));
        }
        s
    }
}

/// The template settings closest to a preset's pedals: amounts of pedals
/// the preset lacks are 0, and parameters that don't apply (a missing
/// pedal's knobs) are `None`. The input level is the renderer's default.
pub fn from_preset(pedals: &[PedalInstance]) -> (Vec<Option<f64>>, AmpVoicing) {
    let mut x: Vec<Option<f64>> = vec![None; PARAMS.len()];
    let unit = |i: usize, v: f32| Some(PARAMS[i].unit(v));
    x[LEVEL] = unit(LEVEL, 0.1);
    x[COMP_AMOUNT] = Some(0.0);
    x[DRIVE_MIX] = Some(0.0);
    x[REVERB_MIX] = Some(0.0);
    x[DELAY_MIX] = Some(0.0);
    x[AMP_GAIN] = unit(AMP_GAIN, 1.0);
    let mut voicing = AmpVoicing::Clean;
    for p in pedals.iter().filter(|p| p.enabled) {
        match &p.description {
            PedalDescription::Compressor(c) => {
                x[COMP_AMOUNT] = Some(((c.ratio - 1.0) / (MAX_RATIO - 1.0)).clamp(0.0, 1.0) as f64);
                x[COMP_THRESHOLD] = unit(COMP_THRESHOLD, c.threshold);
            }
            PedalDescription::Distortion(d) => {
                x[DRIVE_MIX] = unit(DRIVE_MIX, d.mix);
                x[DRIVE] = unit(DRIVE, d.drive);
                x[DRIVE_TONE] = unit(DRIVE_TONE, d.tone);
            }
            PedalDescription::Amp(a) => {
                x[AMP_GAIN] = unit(AMP_GAIN, a.gain);
                voicing = a.voicing;
            }
            PedalDescription::Reverb(r) => {
                x[REVERB_MIX] = unit(REVERB_MIX, r.mix);
                x[REVERB_ROOM] = unit(REVERB_ROOM, r.room_size);
                x[REVERB_DAMPING] = unit(REVERB_DAMPING, r.damping);
            }
            PedalDescription::Delay(d) => {
                x[DELAY_MIX] = unit(DELAY_MIX, d.mix);
                x[DELAY_FEEDBACK] = unit(DELAY_FEEDBACK, d.feedback);
            }
            _ => {}
        }
    }
    (x, voicing)
}

/// A diagonal Gaussian over the unit-cube settings, fitted on presets
/// (MAP-style regularisation of the search, after Yu et al., WASPAA 2025).
#[derive(Clone, Debug)]
pub struct Prior {
    pub mean: Vec<f64>,
    pub sd: Vec<f64>,
}

/// Smallest standard deviation per parameter, so a parameter all presets
/// agree on isn't pinned.
const MIN_SD: f64 = 0.15;

impl Prior {
    pub fn fit(presets: &[Vec<PedalInstance>]) -> Self {
        let rows: Vec<Vec<Option<f64>>> = presets.iter().map(|p| from_preset(p).0).collect();
        let n = PARAMS.len();
        let mut mean = vec![0.5; n];
        let mut sd = vec![0.5; n];
        for i in 0..n {
            let v: Vec<f64> = rows.iter().filter_map(|r| r[i]).collect();
            if v.len() < 2 {
                continue;
            }
            let m = v.iter().sum::<f64>() / v.len() as f64;
            let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64;
            mean[i] = m;
            sd[i] = var.sqrt().max(MIN_SD);
        }
        Self { mean, sd }
    }

    /// Mean squared z-score of `x`: 0 at the mean, ~1 for a typical preset.
    pub fn penalty(&self, x: &[f64]) -> f64 {
        x.iter()
            .zip(&self.mean)
            .zip(&self.sd)
            .map(|((v, m), s)| ((v - m) / s).powi(2))
            .sum::<f64>()
            / x.len().max(1) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_chain_maps_back_to_its_settings() {
        let mut s = Settings::start(AmpVoicing::Lead);
        s.x[COMP_AMOUNT] = 0.5;
        s.delay_ms = Some(300.0);
        let (x, voicing) = from_preset(&s.pedals());
        assert_eq!(voicing, AmpVoicing::Lead);
        for i in [COMP_AMOUNT, COMP_THRESHOLD, DRIVE_MIX, DRIVE, DRIVE_TONE, AMP_GAIN, REVERB_MIX, DELAY_MIX] {
            assert!((x[i].unwrap() - s.x[i]).abs() < 1e-4, "{}", PARAMS[i].name);
        }
    }

    #[test]
    fn the_prior_prefers_typical_settings() {
        let presets: Vec<Vec<PedalInstance>> = crate::shared::factory_presets::factory_presets()
            .into_iter()
            .map(|p| p.pedals)
            .collect();
        let prior = Prior::fit(&presets);
        let typical = prior.mean.clone();
        let extreme: Vec<f64> = typical.iter().map(|m| if *m < 0.5 { 1.0 } else { 0.0 }).collect();
        assert!(prior.penalty(&typical) < 1e-9);
        assert!(prior.penalty(&extreme) > 1.0);
    }

    #[test]
    fn unit_and_value_are_inverse() {
        for p in PARAMS {
            for x in [0.0, 0.3, 1.0] {
                assert!((p.unit(p.value(x)) - x).abs() < 1e-5, "{}", p.name);
            }
        }
    }

    #[test]
    fn pedals_below_the_threshold_are_left_out() {
        let mut s = Settings::start(AmpVoicing::Crunch);
        s.x[COMP_AMOUNT] = 0.0;
        s.x[DRIVE_MIX] = 0.0;
        s.x[REVERB_MIX] = 0.0;
        let chain = s.pedals();
        assert_eq!(chain.len(), 1);
        assert!(matches!(chain[0].description, PedalDescription::Amp(_)));
        s.x[DRIVE_MIX] = 0.5;
        assert_eq!(s.pedals().len(), 2);
    }
}
