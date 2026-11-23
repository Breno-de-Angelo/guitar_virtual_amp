//! Real-time budget check for the DSP hot path (see ROADMAP.md Phase 3 request
//! to "test and assure the current implementation is satisfying real-time
//! requirements").
//!
//! The cpal output callback (`guitar_core::backend::capture::build_output_stream`)
//! runs `PedalChain::process_block` on each callback's samples; at
//! `GLOBAL_CONFIG`'s 48kHz the chain has ~20.83us per sample before the
//! callback risks under-running the audio device's buffer. Blocks here are
//! `BLOCK_FRAMES` long, the app's default buffer size. This binary builds
//! the worst realistic case -- every pedal type in the chain at once, each
//! with default params -- and measures wall-clock time per sample against
//! that budget. Run with `cargo run --release -p guitar_desktop --bin
//! bench_pedal_chain` (use `--release`; DSP code is not fast enough in debug
//! builds to be representative of real playback).
//!
//! Set `NAM_MODEL=/path/to/capture.nam` to also measure a NAM Model pedal
//! running that capture, and how long rebuilding it takes on the audio
//! thread once the file is preloaded.

use std::time::Instant;

use guitar_core::backend::pedals::pedal::PedalChain;
use guitar_core::shared::config::GLOBAL_CONFIG;
use guitar_core::shared::pedals::{
    AmpParams, CabinetParams, ChorusParams, CompressorParams, DelayParams, DistortionParams,
    EqParams, FlangerParams, LooperParams, LowPassParams, NamParams, NoiseGateParams, OctaverParams,
    PedalDescription, PedalInstance, PhaserParams, PitchShifterParams, ReverbParams,
    TremoloParams, WahWahParams,
};

const WARMUP_SAMPLES: usize = 48_000;
const BLOCK_FRAMES: usize = 128;
const MEASURED_SAMPLES: usize = 48_000 * 5;

fn full_chain() -> Vec<PedalInstance> {
    vec![
        PedalInstance::new(PedalDescription::NoiseGate(NoiseGateParams::default())),
        PedalInstance::new(PedalDescription::Compressor(CompressorParams::default())),
        PedalInstance::new(PedalDescription::Amp(AmpParams::default())),
        PedalInstance::new(PedalDescription::Distortion(DistortionParams::default())),
        PedalInstance::new(PedalDescription::WahWah(WahWahParams::default())),
        PedalInstance::new(PedalDescription::Eq(EqParams::default())),
        PedalInstance::new(PedalDescription::Octaver(OctaverParams::default())),
        PedalInstance::new(PedalDescription::PitchShift(PitchShifterParams::default())),
        PedalInstance::new(PedalDescription::Chorus(ChorusParams::default())),
        PedalInstance::new(PedalDescription::Flanger(FlangerParams::default())),
        PedalInstance::new(PedalDescription::Phaser(PhaserParams::default())),
        PedalInstance::new(PedalDescription::Tremolo(TremoloParams::default())),
        PedalInstance::new(PedalDescription::LowPass(LowPassParams::default())),
        PedalInstance::new(PedalDescription::Delay(DelayParams::default())),
        PedalInstance::new(PedalDescription::Reverb(ReverbParams::default())),
        PedalInstance::new(PedalDescription::Looper(LooperParams::default())),
        PedalInstance::new(PedalDescription::Cabinet(CabinetParams::default())),
    ]
}

fn bench(label: &str, description: Vec<PedalInstance>) {
    let mut chain = PedalChain::from_description(description);
    // Square-ish test signal, fed in callback-sized blocks. Warm up first so
    // steady-state cost is measured, not first-call allocation noise.
    let signal: Vec<i16> = (0..BLOCK_FRAMES)
        .map(|i| if (i / 16) % 2 == 0 { 8000 } else { -8000 })
        .collect();
    let mut block = signal.clone();
    for _ in 0..WARMUP_SAMPLES / BLOCK_FRAMES {
        block.copy_from_slice(&signal);
        chain.process_block(&mut block);
    }
    let blocks = MEASURED_SAMPLES / BLOCK_FRAMES;
    let start = Instant::now();
    for _ in 0..blocks {
        block.copy_from_slice(&signal);
        chain.process_block(&mut block);
    }
    let elapsed = start.elapsed();
    std::hint::black_box(&block);
    let per_sample_ns = elapsed.as_nanos() as f64 / (blocks * BLOCK_FRAMES) as f64;
    let budget_ns = 1_000_000_000.0 / GLOBAL_CONFIG.sample_rate as f64;
    let headroom_pct = (1.0 - per_sample_ns / budget_ns) * 100.0;

    println!(
        "{label}: {per_sample_ns:.1} ns/sample (budget @ {}Hz: {budget_ns:.1} ns) -> {headroom_pct:.1}% headroom",
        GLOBAL_CONFIG.sample_rate
    );
}

fn main() {
    bench("empty chain", Vec::new());
    bench(
        "single Amp",
        vec![PedalInstance::new(PedalDescription::Amp(
            AmpParams::default(),
        ))],
    );
    bench("full chain (all 17 pedal types)", full_chain());

    if let Ok(path) = std::env::var("NAM_MODEL") {
        match guitar_core::backend::pedals::nam::preload(&path) {
            Ok(info) => println!("NAM model: {:?} ({}, {} Hz)", info.name, info.architecture, info.sample_rate),
            Err(e) => {
                eprintln!("NAM_MODEL could not be loaded: {e}");
                return;
            }
        }
        let nam_chain = vec![PedalInstance::new(PedalDescription::Nam(NamParams {
            model_path: Some(path),
            ..Default::default()
        }))];

        let start = Instant::now();
        let rebuilds = 20;
        for _ in 0..rebuilds {
            std::hint::black_box(PedalChain::from_description(nam_chain.clone()));
        }
        println!(
            "NAM pedal rebuild from cache: {:.2} ms",
            start.elapsed().as_secs_f64() * 1000.0 / rebuilds as f64
        );

        bench("single NAM Model", nam_chain.clone());
        let mut with_nam = full_chain();
        with_nam.extend(nam_chain);
        bench("full chain + NAM Model", with_nam);
    }
}
