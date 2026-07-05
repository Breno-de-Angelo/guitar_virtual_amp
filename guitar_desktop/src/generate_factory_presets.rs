//! One-off/maintenance tool: (re)generates the factory preset pack under
//! `guitar_core/assets/presets/factory/`. Presets are built as typed `Preset`
//! values here (rather than hand-written JSON) so the schema is guaranteed
//! valid; run with `cargo run -p guitar_desktop --bin generate_factory_presets`
//! whenever the factory pack needs to change.

use guitar_core::shared::pedals::{
    AmpParams, AmpVoicing, CabinetParams, ChorusParams, CompressorParams, DelayParams,
    DistortionParams, EqParams, LooperCommand, LooperParams, NoiseGateParams, OctaverParams,
    PedalDescription, PhaserParams, PitchShifterParams, ReverbParams, TremoloParams, WahWahParams,
};
use guitar_core::shared::preset::{sanitize_file_stem, save_to_file, Preset};

fn preset(
    name: &str,
    tags: &[&str],
    song: Option<&str>,
    artist: Option<&str>,
    pedals: Vec<PedalDescription>,
) -> Preset {
    Preset {
        name: name.to_string(),
        author: Some("Factory".to_string()),
        pedals,
        tags: tags.iter().map(|t| t.to_string()).collect(),
        song: song.map(|s| s.to_string()),
        artist: artist.map(|a| a.to_string()),
    }
}

fn factory_presets() -> Vec<Preset> {
    vec![
        preset(
            "Clean Jazz",
            &["clean", "jazz"],
            None,
            None,
            vec![
                PedalDescription::Compressor(CompressorParams {
                    threshold: 0.6,
                    ratio: 2.0,
                    ..CompressorParams::default()
                }),
                PedalDescription::Amp(AmpParams {
                    gain: 0.8,
                    voicing: AmpVoicing::Clean,
                    bass: 2.0,
                    mid: -1.0,
                    treble: -2.0,
                }),
                PedalDescription::Reverb(ReverbParams {
                    room_size: 0.3,
                    mix: 0.2,
                    damping: 0.5,
                }),
            ],
        ),
        preset(
            "Blues Crunch",
            &["blues", "crunch"],
            None,
            Some("inspired by Stevie Ray Vaughan"),
            vec![
                PedalDescription::Amp(AmpParams {
                    gain: 1.8,
                    voicing: AmpVoicing::Crunch,
                    bass: 1.0,
                    mid: 3.0,
                    treble: 1.0,
                }),
                PedalDescription::Delay(DelayParams {
                    delay_ms: 350.0,
                    feedback: 0.25,
                    gain: 0.3,
                    mix: 0.2,
                    taps: 2,
                    damping: 0.4,
                }),
            ],
        ),
        preset(
            "Rock Lead",
            &["rock", "lead"],
            Some("Sweet Child O' Mine (style)"),
            Some("inspired by Guns N' Roses"),
            vec![
                PedalDescription::Amp(AmpParams {
                    gain: 3.0,
                    voicing: AmpVoicing::Lead,
                    bass: 0.0,
                    mid: 4.0,
                    treble: 2.0,
                }),
                PedalDescription::Delay(DelayParams {
                    delay_ms: 400.0,
                    feedback: 0.35,
                    gain: 0.35,
                    mix: 0.25,
                    taps: 3,
                    damping: 0.3,
                }),
                PedalDescription::Reverb(ReverbParams {
                    room_size: 0.5,
                    mix: 0.25,
                    damping: 0.3,
                }),
            ],
        ),
        preset(
            "Metal High Gain",
            &["metal", "high-gain"],
            None,
            None,
            vec![
                PedalDescription::NoiseGate(NoiseGateParams {
                    threshold: 0.05,
                    ..NoiseGateParams::default()
                }),
                PedalDescription::Distortion(DistortionParams {
                    drive: 7.0,
                    tone: 0.4,
                    level: 0.9,
                    mix: 1.0,
                    bias: 0.0,
                }),
                PedalDescription::Amp(AmpParams {
                    gain: 3.5,
                    voicing: AmpVoicing::Lead,
                    bass: 3.0,
                    mid: -3.0,
                    treble: 2.0,
                }),
                PedalDescription::Cabinet(CabinetParams {
                    ir_path: None,
                    mix: 1.0,
                }),
            ],
        ),
        preset(
            "Ambient Ping-Pong",
            &["ambient", "delay"],
            None,
            None,
            vec![
                PedalDescription::Chorus(ChorusParams::default()),
                PedalDescription::Delay(DelayParams {
                    delay_ms: 600.0,
                    feedback: 0.55,
                    gain: 0.6,
                    mix: 0.5,
                    taps: 6,
                    damping: 0.2,
                }),
                PedalDescription::Reverb(ReverbParams {
                    room_size: 0.8,
                    mix: 0.5,
                    damping: 0.2,
                }),
            ],
        ),
        preset(
            "Funk Wah",
            &["funk", "wah"],
            None,
            None,
            vec![
                PedalDescription::Compressor(CompressorParams {
                    ratio: 3.0,
                    ..CompressorParams::default()
                }),
                PedalDescription::WahWah(WahWahParams::default()),
            ],
        ),
        preset(
            "Classic Rock Rhythm",
            &["rock", "rhythm"],
            None,
            None,
            vec![
                PedalDescription::Amp(AmpParams {
                    gain: 2.0,
                    voicing: AmpVoicing::Crunch,
                    bass: 1.0,
                    mid: 1.0,
                    treble: 0.0,
                }),
                PedalDescription::Eq(EqParams {
                    low_gain_db: 1.0,
                    mid_gain_db: 0.0,
                    high_gain_db: 1.0,
                }),
            ],
        ),
        preset(
            "Shoegaze Wall",
            &["shoegaze", "wall-of-sound"],
            None,
            None,
            vec![
                PedalDescription::Distortion(DistortionParams {
                    drive: 5.0,
                    tone: 0.6,
                    level: 0.7,
                    mix: 0.8,
                    bias: 0.0,
                }),
                PedalDescription::Chorus(ChorusParams {
                    depth_ms: 8.0,
                    mix: 0.6,
                    ..ChorusParams::default()
                }),
                PedalDescription::Delay(DelayParams {
                    delay_ms: 450.0,
                    feedback: 0.4,
                    gain: 0.4,
                    mix: 0.3,
                    taps: 4,
                    damping: 0.3,
                }),
                PedalDescription::Reverb(ReverbParams {
                    room_size: 0.9,
                    mix: 0.6,
                    damping: 0.15,
                }),
            ],
        ),
        preset(
            "Country Twang",
            &["country", "twang"],
            None,
            None,
            vec![
                PedalDescription::Compressor(CompressorParams {
                    threshold: 0.4,
                    ratio: 3.5,
                    attack_ms: 3.0,
                    release_ms: 80.0,
                    makeup_gain: 1.6,
                }),
                PedalDescription::Eq(EqParams {
                    low_gain_db: -2.0,
                    mid_gain_db: 1.0,
                    high_gain_db: 4.0,
                }),
                PedalDescription::Delay(DelayParams {
                    delay_ms: 120.0,
                    feedback: 0.1,
                    gain: 0.2,
                    mix: 0.15,
                    taps: 1,
                    damping: 0.3,
                }),
            ],
        ),
        preset(
            "Surf Rock",
            &["surf", "spring-reverb"],
            None,
            None,
            vec![
                PedalDescription::Tremolo(TremoloParams {
                    rate_hz: 4.0,
                    depth: 0.3,
                }),
                PedalDescription::Reverb(ReverbParams {
                    room_size: 0.7,
                    mix: 0.55,
                    damping: 0.25,
                }),
            ],
        ),
        preset(
            "Funk Envelope",
            &["funk", "auto-wah"],
            None,
            None,
            vec![
                PedalDescription::Compressor(CompressorParams {
                    ratio: 5.0,
                    attack_ms: 2.0,
                    ..CompressorParams::default()
                }),
                PedalDescription::WahWah(WahWahParams {
                    lfo_rate: 3.5,
                    lfo_depth: 0.8,
                    mix: 0.7,
                    ..WahWahParams::default()
                }),
            ],
        ),
        preset(
            "Doom Sludge",
            &["doom", "sludge", "low-tuned"],
            None,
            None,
            vec![
                PedalDescription::Distortion(DistortionParams {
                    drive: 8.0,
                    tone: 0.3,
                    level: 0.9,
                    mix: 1.0,
                    bias: 0.0,
                }),
                PedalDescription::LowPass(guitar_core::shared::pedals::LowPassParams {
                    frequency: 3500.0,
                }),
                PedalDescription::Cabinet(CabinetParams {
                    ir_path: None,
                    mix: 1.0,
                }),
            ],
        ),
        preset(
            "Pop Clean",
            &["pop", "clean"],
            None,
            None,
            vec![
                PedalDescription::Chorus(ChorusParams {
                    mix: 0.3,
                    ..ChorusParams::default()
                }),
                PedalDescription::Compressor(CompressorParams::default()),
                PedalDescription::Eq(EqParams {
                    low_gain_db: 0.0,
                    mid_gain_db: -1.0,
                    high_gain_db: 2.0,
                }),
            ],
        ),
        preset(
            "Reggae Skank",
            &["reggae", "skank"],
            None,
            None,
            vec![
                PedalDescription::Phaser(PhaserParams::default()),
                PedalDescription::Delay(DelayParams {
                    delay_ms: 300.0,
                    feedback: 0.3,
                    gain: 0.3,
                    mix: 0.25,
                    taps: 2,
                    damping: 0.4,
                }),
            ],
        ),
        preset(
            "Acoustic Simulation",
            &["acoustic", "unplugged"],
            None,
            None,
            vec![
                PedalDescription::Eq(EqParams {
                    low_gain_db: 1.0,
                    mid_gain_db: -2.0,
                    high_gain_db: 3.0,
                }),
                PedalDescription::Reverb(ReverbParams {
                    room_size: 0.35,
                    mix: 0.2,
                    damping: 0.4,
                }),
            ],
        ),
        preset(
            "Lo-Fi Tape",
            &["lo-fi", "tape"],
            None,
            None,
            vec![
                PedalDescription::LowPass(guitar_core::shared::pedals::LowPassParams {
                    frequency: 4000.0,
                }),
                PedalDescription::PitchShift(PitchShifterParams {
                    semitones: 0.1,
                    mix: 0.3,
                    grain_size_ms: 80.0,
                }),
                PedalDescription::NoiseGate(NoiseGateParams::default()),
            ],
        ),
        preset(
            "Synth Lead",
            &["synth", "octave"],
            None,
            None,
            vec![
                PedalDescription::Octaver(OctaverParams {
                    octave_down_mix: 0.6,
                    dry_mix: 0.5,
                    tone: 0.6,
                }),
                PedalDescription::Delay(DelayParams {
                    delay_ms: 250.0,
                    feedback: 0.3,
                    gain: 0.3,
                    mix: 0.25,
                    taps: 2,
                    damping: 0.3,
                }),
                PedalDescription::Reverb(ReverbParams {
                    room_size: 0.6,
                    mix: 0.3,
                    damping: 0.3,
                }),
            ],
        ),
        preset(
            "Rockabilly Slapback",
            &["rockabilly", "slapback"],
            None,
            None,
            vec![
                PedalDescription::Amp(AmpParams {
                    gain: 1.6,
                    voicing: AmpVoicing::Crunch,
                    bass: 0.0,
                    mid: 2.0,
                    treble: 2.0,
                }),
                PedalDescription::Delay(DelayParams {
                    delay_ms: 110.0,
                    feedback: 0.05,
                    gain: 0.4,
                    mix: 0.3,
                    taps: 1,
                    damping: 0.2,
                }),
            ],
        ),
        preset(
            "Metal Djent",
            &["metal", "djent", "tight"],
            None,
            None,
            vec![
                PedalDescription::NoiseGate(NoiseGateParams {
                    threshold: 0.08,
                    attack_ms: 0.5,
                    release_ms: 60.0,
                    hold_ms: 20.0,
                }),
                PedalDescription::Distortion(DistortionParams {
                    drive: 6.0,
                    tone: 0.35,
                    level: 0.85,
                    mix: 1.0,
                    bias: 0.0,
                }),
                PedalDescription::Eq(EqParams {
                    low_gain_db: 2.0,
                    mid_gain_db: -5.0,
                    high_gain_db: 3.0,
                }),
                PedalDescription::Cabinet(CabinetParams {
                    ir_path: None,
                    mix: 1.0,
                }),
            ],
        ),
        preset(
            "Ambient Pad Looper",
            &["ambient", "looper", "pad"],
            None,
            None,
            vec![
                PedalDescription::Chorus(ChorusParams {
                    mix: 0.4,
                    ..ChorusParams::default()
                }),
                PedalDescription::Reverb(ReverbParams {
                    room_size: 0.85,
                    mix: 0.55,
                    damping: 0.2,
                }),
                PedalDescription::Looper(LooperParams {
                    max_loop_seconds: 30.0,
                    command: LooperCommand::Idle,
                }),
            ],
        ),
    ]
}

fn main() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::path::Path::new(manifest_dir)
        .join("..")
        .join("guitar_core")
        .join("assets")
        .join("presets")
        .join("factory");

    for preset in factory_presets() {
        let file_name = format!("{}.json", sanitize_file_stem(&preset.name).to_lowercase());
        let path = out_dir.join(file_name);
        save_to_file(&preset, &path).expect("failed to write factory preset");
        println!("wrote {}", path.display());
    }
}
