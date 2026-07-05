//! Real-time budget check for the DSP hot path (see ROADMAP.md Phase 3 request
//! to "test and assure the current implementation is satisfying real-time
//! requirements").
//!
//! `PedalChain::process_sample` runs once per sample inside the cpal output
//! callback (`guitar_core::backend::capture::build_output_stream`); at
//! `GLOBAL_CONFIG`'s 48kHz it has ~20.83us to process one sample before the
//! callback risks under-running the audio device's buffer. This binary builds
//! the worst realistic case -- every pedal type in the chain at once, each
//! with default params -- and measures wall-clock time per sample against
//! that budget. Run with `cargo run --release -p guitar_desktop --bin
//! bench_pedal_chain` (use `--release`; DSP code is not fast enough in debug
//! builds to be representative of real playback).

use std::time::Instant;

use guitar_core::backend::pedals::pedal::PedalChain;
use guitar_core::shared::config::GLOBAL_CONFIG;
use guitar_core::shared::pedals::{
    AmpParams, CabinetParams, ChorusParams, CompressorParams, DelayParams, DistortionParams,
    EqParams, FlangerParams, LooperParams, LowPassParams, NoiseGateParams, OctaverParams,
    PedalDescription, PedalInstance, PhaserParams, PitchShifterParams, ReverbParams,
    TremoloParams, WahWahParams,
};

const WARMUP_SAMPLES: usize = 48_000;
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

    // Warm up: fill delay lines / filter state so steady-state cost is measured,
    // not first-call allocation/branch-prediction noise.
    let mut sample: i16 = 1000;
    for i in 0..WARMUP_SAMPLES {
        sample = chain.process_sample(if i % 2 == 0 { sample } else { -sample });
    }

    let start = Instant::now();
    for i in 0..MEASURED_SAMPLES {
        sample = chain.process_sample(if i % 2 == 0 { sample } else { -sample });
    }
    let elapsed = start.elapsed();
    std::hint::black_box(sample);

    let per_sample_ns = elapsed.as_nanos() as f64 / MEASURED_SAMPLES as f64;
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
}
