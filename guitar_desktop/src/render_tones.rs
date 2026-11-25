//! Renders guitar recordings through the factory presets, for training and
//! testing transcription on amp tones (offline, no audio device):
//!
//! ```text
//! cargo run --release -p guitar_desktop --bin render_tones -- \
//!     --out DIR [--preset NAME]... [--drive 0.5,1,1.5] [--rate HZ] <wav files or folders>...
//! ```
//!
//! Every input file is written to `DIR/<preset>[@<drive>]/<file stem>.wav`
//! (mono, 16-bit, 48 kHz unless `--rate`), normalized to -1 dBFS peak. Without `--preset`,
//! all factory presets with an amp or distortion pedal are used. `--drive`
//! scales the drive/gain of those pedals for more tones per preset.
//! Existing outputs are skipped.

use std::path::{Path, PathBuf};

use guitar_core::backend::audio_file::decode_file;
use guitar_core::backend::offline::{RENDER_RATE, factory_preset, render, scale_drive};
use guitar_core::shared::factory_presets::factory_presets;
use guitar_core::shared::pedals::PedalDescription;
use guitar_core::transcribe::resample::resample;

fn wavs(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for e in entries {
            wavs(&e, out);
        }
    } else if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
    {
        out.push(path.to_path_buf());
    }
}

fn write(path: &Path, samples: &[f32], rate: u32) -> Result<(), String> {
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs())).max(1e-6);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let tmp = path.with_extension("wav.part");
    let mut w = hound::WavWriter::create(&tmp, spec).map_err(|e| e.to_string())?;
    for s in samples {
        w.write_sample((s / peak * 0.89 * 32767.0) as i16)
            .map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut out, mut presets, mut drives, mut inputs) =
        (None, Vec::new(), vec![1.0f32], Vec::new());
    let mut rate = RENDER_RATE;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--preset" => presets.extend(args.next()),
            "--rate" => {
                rate = args
                    .next()
                    .and_then(|r| r.parse().ok())
                    .unwrap_or(RENDER_RATE)
            }
            "--drive" => {
                drives = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|d| d.trim().parse().ok())
                    .collect()
            }
            "-h" | "--help" => {
                println!(
                    "{}",
                    include_str!("render_tones.rs")
                        .lines()
                        .take(14)
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                return;
            }
            _ => wavs(Path::new(&a), &mut inputs),
        }
    }
    let Some(out) = out else {
        eprintln!("--out DIR is required (see --help)");
        std::process::exit(2);
    };
    if presets.is_empty() {
        presets = factory_presets()
            .into_iter()
            .filter(|p| {
                p.pedals.iter().any(|i| {
                    matches!(
                        i.description,
                        PedalDescription::Amp(_) | PedalDescription::Distortion(_)
                    )
                })
            })
            .map(|p| p.name)
            .collect();
    }
    let mut done = 0;
    for name in &presets {
        let Some(preset) = factory_preset(name) else {
            eprintln!("no factory preset named {name}");
            std::process::exit(2);
        };
        for &drive in &drives {
            let folder = if (drive - 1.0).abs() < 1e-6 {
                name.to_lowercase().replace(' ', "_")
            } else {
                format!("{}@{drive}", name.to_lowercase().replace(' ', "_"))
            };
            let dir = out.join(folder);
            std::fs::create_dir_all(&dir).expect("output folder");
            for input in &inputs {
                let target = dir.join(input.file_name().unwrap());
                if target.exists() {
                    continue;
                }
                let audio = match decode_file(input) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("{}: {e}", input.display());
                        continue;
                    }
                };
                let mut pedals = preset.pedals.clone();
                scale_drive(&mut pedals, drive);
                let rendered = render(
                    pedals,
                    &resample(&audio.samples, audio.sample_rate, RENDER_RATE),
                );
                let rendered = resample(&rendered, RENDER_RATE, rate);
                if let Err(e) = write(&target, &rendered, rate) {
                    eprintln!("{}: {e}", target.display());
                }
                done += 1;
            }
            eprintln!("{} @ {drive}: {} files", name, inputs.len());
        }
    }
    eprintln!("rendered {done} files into {}", out.display());
}
