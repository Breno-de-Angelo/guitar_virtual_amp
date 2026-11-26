//! Pedal chains as a player would set them up for the song: a speaker IR
//! in the Cabinet (the factory presets ship without one, and an amp without
//! a speaker sounds fizzy), and delays in time with the band.

use std::path::{Path, PathBuf};

use guitar_core::shared::pedals::{CabinetParams, PedalDescription, PedalInstance};

use crate::guitar::RATE;

pub struct ToneSetup {
    /// Speaker IR every drive chain gets, if any.
    pub ir: Option<PathBuf>,
    /// Song tempo for delay sync, if any.
    pub bpm: Option<f64>,
}

/// Writes a 4x12 closed-back cabinet IR (minimum phase, from a typical
/// magnitude response: tight lows, a slight mid scoop, presence near 3 kHz
/// and the steep speaker roll-off above 5 kHz). Synthetic, so it's ours.
pub fn write_cabinet_ir(dir: &Path) -> Result<PathBuf, String> {
    // Voiced against real amp recordings (EGDB's Mesa and Marshall renders,
    // octave by octave) with the factory presets: less 125 Hz, more 2-4 kHz
    // bite, and no fizz above 6 kHz.
    let curve: [(f32, f32); 18] = [
        (20.0, -40.0),
        (60.0, -20.0),
        (90.0, -9.0),
        (130.0, -4.0),
        (180.0, -1.0),
        (250.0, 0.0),
        (400.0, -2.0),
        (800.0, -1.0),
        (1400.0, 1.0),
        (2200.0, 3.5),
        (3000.0, 4.5),
        (3800.0, 3.0),
        (4600.0, -1.0),
        (5500.0, -10.0),
        (6500.0, -20.0),
        (8000.0, -32.0),
        (10000.0, -44.0),
        (20000.0, -65.0),
    ];
    // 9 dB down overall, so the presence peak never clips the
    // pedal's 16-bit output when a high-gain amp drives it to full scale.
    let (hz, db): (Vec<f32>, Vec<f32>) = curve.iter().map(|(f, d)| (*f, d - 9.0)).unzip();
    let ir = guitar_core::tone::matching::ir_from_eq(&db, &hz, RATE as u32);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join("4x12 Closed Back.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: RATE as u32, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&path, spec).map_err(|e| e.to_string())?;
    for s in ir {
        w.write_sample(s).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())?;
    Ok(path)
}

impl ToneSetup {
    /// `pedals` with an IR in empty Cabinets (one is added after the last
    /// amp or drive pedal when there's none) and delays synced. Calling it
    /// again changes nothing.
    pub fn prepare(&self, mut pedals: Vec<PedalInstance>) -> Vec<PedalInstance> {
        if let Some(ir) = &self.ir {
            let ir = ir.display().to_string();
            let drive = |p: &PedalInstance| matches!(p.description, PedalDescription::Amp(_) | PedalDescription::Distortion(_));
            let mut has_cab = false;
            for p in &mut pedals {
                if let PedalDescription::Cabinet(c) = &mut p.description {
                    has_cab = true;
                    if c.ir_path.is_none() {
                        c.ir_path = Some(ir.clone());
                        c.mix = 1.0;
                    }
                }
            }
            if !has_cab && let Some(last) = pedals.iter().rposition(drive) {
                let cab = CabinetParams { ir_path: Some(ir), mix: 1.0, ..Default::default() };
                pedals.insert(last + 1, PedalInstance::new(PedalDescription::Cabinet(cab)));
            }
        }
        if let Some(bpm) = self.bpm {
            let beat = 60_000.0 / bpm as f32;
            for p in &mut pedals {
                if let PedalDescription::Delay(d) = &mut p.description {
                    let choices = [beat * 0.5, beat * 0.75, beat, beat * 1.5];
                    d.delay_ms = choices.iter().copied().min_by(|a, b| (a - d.delay_ms).abs().total_cmp(&(b - d.delay_ms).abs())).unwrap();
                }
            }
        }
        pedals
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guitar_core::shared::pedals::{AmpParams, DelayParams, EqParams};

    #[test]
    fn adds_a_cab_after_the_amp_once_and_syncs_delays() {
        let tone = ToneSetup { ir: Some("/x/cab.wav".into()), bpm: Some(100.0) };
        let chain = vec![
            PedalInstance::new(PedalDescription::Amp(AmpParams::default())),
            PedalInstance::new(PedalDescription::Eq(EqParams::default())),
            PedalInstance::new(PedalDescription::Delay(DelayParams { delay_ms: 430.0, ..Default::default() })),
        ];
        let once = tone.prepare(chain);
        assert!(matches!(&once[1].description, PedalDescription::Cabinet(c) if c.ir_path.as_deref() == Some("/x/cab.wav")));
        assert!(matches!(&once[3].description, PedalDescription::Delay(d) if (d.delay_ms - 450.0).abs() < 1e-3));
        assert_eq!(tone.prepare(once.clone()).len(), once.len());
    }
}

#[cfg(test)]
mod response {
    use super::*;
    use guitar_core::backend::offline::{factory_preset, render_at};

    fn write(path: &Path, x: &[f32]) {
        let spec = hound::WavSpec { channels: 1, sample_rate: RATE as u32, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        x.iter().for_each(|s| w.write_sample(*s).unwrap());
        w.finalize().unwrap();
    }

    /// `GVA_PROMO_DI=<di.wav>`: writes it through presets (with and without
    /// the cabinet) to the temp dir, for measuring the tones.
    #[test]
    #[ignore]
    fn render_presets() {
        let dir = std::env::temp_dir().join("gva_ir_test");
        let tone = ToneSetup { ir: Some(write_cabinet_ir(&dir).unwrap()), bpm: None };
        let di_path = std::env::var("GVA_PROMO_DI").expect("GVA_PROMO_DI");
        let di = guitar_core::backend::audio_file::decode_file(Path::new(&di_path)).unwrap().samples;
        let level = (di.iter().map(|s| s * s).sum::<f32>() / di.len() as f32).sqrt();
        for name in ["Blues Crunch", "Classic Rock Rhythm", "Metal High Gain", "Rock Lead", "Shoegaze Wall"] {
            let raw = factory_preset(name).unwrap().pedals;
            let stem = name.replace(' ', "_");
            write(&dir.join(format!("{stem}.wav")), &render_at(tone.prepare(raw.clone()), &di, level));
            write(&dir.join(format!("{stem}_nocab.wav")), &render_at(raw, &di, level));
        }
    }
}
