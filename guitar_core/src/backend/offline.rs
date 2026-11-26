//! Offline rendering through a pedal chain: plays recorded audio through a
//! preset exactly as the live signal would go (48 kHz, i16, 256-sample
//! blocks), for benchmarks and for training data with amp tones.

use super::pedals::pedal::PedalChain;
use crate::shared::pedals::{PedalDescription, PedalInstance};
use crate::shared::preset::Preset;

/// The live chain's sample rate.
pub const RENDER_RATE: u32 = 48_000;

/// The factory preset called `name` (case-insensitive).
pub fn factory_preset(name: &str) -> Option<Preset> {
    crate::shared::factory_presets::factory_presets()
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Scales the drive of every distortion and amp pedal by `factor`, for
/// more tones from one preset.
pub fn scale_drive(pedals: &mut [PedalInstance], factor: f32) {
    for p in pedals {
        match &mut p.description {
            PedalDescription::Distortion(d) => d.drive = (d.drive * factor).clamp(0.0, 10.0),
            PedalDescription::Amp(a) => a.gain = (a.gain * factor).clamp(0.0, 10.0),
            _ => {}
        }
    }
}

/// `input` (mono, [`RENDER_RATE`]) through `pedals`. The input is scaled to
/// about the RMS of a guitar DI (-20 dBFS) so noise gates and drive act as
/// they do live; peaks are clipped to i16. Returns mono at [`RENDER_RATE`].
pub fn render(pedals: Vec<PedalInstance>, input: &[f32]) -> Vec<f32> {
    render_at(pedals, input, 0.1)
}

/// [`render`] with the input scaled to RMS `level` (full scale = 1.0) instead
/// of -20 dBFS: how hard the guitar hits the chain, which sets the drive.
pub fn render_at(pedals: Vec<PedalInstance>, input: &[f32], level: f32) -> Vec<f32> {
    let rms = (input.iter().map(|s| s * s).sum::<f32>() / input.len().max(1) as f32)
        .sqrt()
        .max(1e-6);
    let mut samples: Vec<i16> = input
        .iter()
        .map(|s| (s / rms * level * 32767.0).clamp(-32767.0, 32767.0) as i16)
        .collect();
    let mut chain = PedalChain::from_description(pedals);
    for block in samples.chunks_mut(256) {
        chain.process_block(block);
    }
    samples.iter().map(|s| *s as f32 / 32768.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distortion_presets_add_harmonics() {
        let n = RENDER_RATE as usize;
        let sine: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * 110.0 * i as f32 / RENDER_RATE as f32).sin() * 0.1)
            .collect();
        let preset = factory_preset("metal high gain").unwrap();
        let out = render(preset.pedals, &sine);
        assert_eq!(out.len(), n);
        // Energy away from 110 Hz: a clean sine has none.
        let at = |f: f32| {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, v) in out.iter().enumerate().skip(n / 2) {
                let ph = std::f32::consts::TAU * f * i as f32 / RENDER_RATE as f32;
                re += v * ph.cos();
                im += v * ph.sin();
            }
            (re * re + im * im).sqrt()
        };
        assert!(
            at(330.0) > 0.01 * at(110.0),
            "{} vs {}",
            at(330.0),
            at(110.0)
        );
        let mut gentler = factory_preset("metal high gain").unwrap().pedals;
        scale_drive(&mut gentler, 0.5);
        assert_ne!(gentler, factory_preset("metal high gain").unwrap().pedals);
    }
}
