//! Neural Amp Modeler (`.nam`) pedal, running captures with the `nam-rs`
//! inference crate.
//!
//! Loading has two costs: reading and parsing the JSON file (can be several
//! MB and tens of ms) and building the runtime network from it (allocation,
//! but no I/O). The pedal chain is built on the audio thread, so the parse
//! is moved off it: the UI calls [`preload`] before sending a chain that uses
//! a model, and [`NamPedal::new`] picks the parsed file up from the cache.
//! On a cache miss the constructor still parses the file itself, matching how
//! the `Cabinet` pedal loads its IR, so a chain always works even if nobody
//! preloaded it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use nam_rs::{Model, NamModel};

use crate::{
    backend::pedals::pedal::Pedal,
    shared::{
        config::GLOBAL_CONFIG,
        pedals::{NamParams, PedalDescription},
    },
};

/// Largest chunk handed to `Model::process_buffer` at once; longer blocks
/// are split. Sized for the biggest buffer the UI offers.
///
/// The pedal adds no latency: `process_block` runs the model on the audio
/// callback's own block. `nam-rs` is faster on bigger blocks, so CPU use
/// depends on the buffer size picked in the UI. Measured with
/// `bench_pedal_chain` on a standard WaveNet capture (i7-14700HX, one
/// core): 32 frames → 5.9 µs/sample, 64 → 4.3, 128 → 3.5, 256 → 3.2.
const MAX_CHUNK: usize = 4096;

/// Output level that normalization aims for, in dBFS RMS. Same target as the
/// reference NAM plugin, so models sound as loud here as they do there.
const NORMALIZE_TARGET_DB: f32 = -18.0;

/// Length of the fade-in after the model's warmup transient, in samples.
const FADE_IN_SAMPLES: usize = 2048;

/// What the UI shows about a loaded model.
#[derive(Clone, Debug, PartialEq)]
pub struct NamInfo {
    pub name: Option<String>,
    pub architecture: String,
    pub sample_rate: f64,
    pub loudness_db: Option<f32>,
    pub includes_cab: Option<bool>,
}

impl NamInfo {
    /// `true` when the model was captured at a rate other than the app's.
    /// `nam-rs` doesn't resample, so such a model sounds wrong here.
    pub fn sample_rate_mismatch(&self) -> bool {
        (self.sample_rate - GLOBAL_CONFIG.sample_rate as f64).abs() > 1.0
    }
}

struct LoadedNam {
    file: NamModel,
    info: NamInfo,
}

type Cache = HashMap<String, Result<Arc<LoadedNam>, String>>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn parse(path: &str) -> Result<Arc<LoadedNam>, String> {
    let file = NamModel::from_file(path).map_err(|e| crate::trf!("could not read model: {e}", "não foi possível ler o modelo: {e}"))?;
    // Build the runtime once here so unsupported architectures are reported
    // to the user instead of silently producing a pass-through pedal.
    Model::from_nam(&file).map_err(|e| crate::trf!("unsupported model: {e}", "modelo não suportado: {e}"))?;
    let metadata = file.metadata_typed();
    let info = NamInfo {
        name: metadata.name.clone(),
        architecture: file.architecture.clone(),
        sample_rate: file.expected_sample_rate(),
        loudness_db: metadata.loudness,
        includes_cab: metadata.includes_cab(),
    };
    Ok(Arc::new(LoadedNam { file, info }))
}

/// Parses `path` into the cache (if it isn't there yet) and returns its info.
/// Call this off the audio thread before sending a chain that uses the model.
pub fn preload(path: &str) -> Result<NamInfo, String> {
    if let Some(entry) = cache().lock().unwrap().get(path) {
        return entry
            .as_ref()
            .map(|loaded| loaded.info.clone())
            .map_err(Clone::clone);
    }
    // Parse without holding the lock so the audio thread is never blocked
    // behind file I/O.
    let parsed = parse(path);
    let result = parsed
        .as_ref()
        .map(|l| l.info.clone())
        .map_err(Clone::clone);
    cache().lock().unwrap().insert(path.to_string(), parsed);
    result
}

/// Info for an already-preloaded model, without touching the disk.
pub fn cached_info(path: &str) -> Option<Result<NamInfo, String>> {
    cache()
        .lock()
        .unwrap()
        .get(path)
        .map(|entry| entry.as_ref().map(|l| l.info.clone()).map_err(Clone::clone))
}

/// Forgets a path so the next [`preload`] reads the file again (used after a
/// download overwrites it).
pub fn invalidate(path: &str) {
    cache().lock().unwrap().remove(path);
}

/// Drops cached models that are no longer used by the chain, so swapping
/// models doesn't keep every one ever loaded in memory.
pub fn retain_only(paths: &[&str]) {
    cache()
        .lock()
        .unwrap()
        .retain(|path, _| paths.contains(&path.as_str()));
}

fn load_for_audio(path: &str) -> Option<Arc<LoadedNam>> {
    if let Some(entry) = cache().lock().unwrap().get(path) {
        return entry.as_ref().ok().cloned();
    }
    let parsed = parse(path);
    let loaded = parsed.as_ref().ok().cloned();
    cache().lock().unwrap().insert(path.to_string(), parsed);
    loaded
}

fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// One-pole high-pass at about 5 Hz. The reference NAM plugin runs one after
/// the model because captures can carry a DC offset.
struct DcBlocker {
    prev_in: f32,
    prev_out: f32,
    coeff: f32,
}

impl DcBlocker {
    fn new() -> Self {
        let cutoff_hz = 5.0;
        let coeff = 1.0 - (2.0 * std::f32::consts::PI * cutoff_hz / GLOBAL_CONFIG.sample_rate);
        Self {
            prev_in: 0.0,
            prev_out: 0.0,
            coeff,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let y = x - self.prev_in + self.coeff * self.prev_out;
        self.prev_in = x;
        self.prev_out = y;
        y
    }
}

struct Runtime {
    model: Model,
    loudness_db: Option<f32>,
    /// Output samples still inside the model's startup transient.
    warmup_remaining: usize,
    fade_position: usize,
    /// Preallocated so `process_block` doesn't allocate on the audio thread.
    scratch: Vec<f32>,
    dc_blocker: DcBlocker,
}

impl Runtime {
    /// DC blocker, output gain, then the warmup mute and fade-in.
    fn finish(&mut self, model_out: f32, output_gain: f32) -> i16 {
        let mut out = self.dc_blocker.process(model_out) * output_gain;
        if self.warmup_remaining > 0 {
            self.warmup_remaining -= 1;
            out = 0.0;
        } else if self.fade_position < FADE_IN_SAMPLES {
            out *= self.fade_position as f32 / FADE_IN_SAMPLES as f32;
            self.fade_position += 1;
        }
        (out * 32768.0).clamp(-32768.0, 32767.0) as i16
    }
}

pub struct NamPedal {
    model_path: Option<String>,
    runtime: Option<Runtime>,
    input_gain: f32,
    output_gain: f32,
}

impl NamPedal {
    pub fn new(params: NamParams) -> Self {
        let runtime = params
            .model_path
            .as_deref()
            .and_then(load_for_audio)
            .and_then(|loaded| {
                let model = Model::from_nam(&loaded.file).ok()?;
                Some(Runtime {
                    warmup_remaining: model.receptive_field(),
                    model,
                    loudness_db: loaded.info.loudness_db,
                    fade_position: 0,
                    scratch: vec![0.0; MAX_CHUNK],
                    dc_blocker: DcBlocker::new(),
                })
            });

        let mut pedal = Self {
            model_path: params.model_path.clone(),
            runtime,
            input_gain: 1.0,
            output_gain: 1.0,
        };
        pedal.set_gains(&params);
        pedal
    }

    fn set_gains(&mut self, params: &NamParams) {
        self.input_gain = db_to_linear(params.input_gain_db);
        let normalization_db = match (&self.runtime, params.normalize) {
            (Some(runtime), true) => runtime
                .loudness_db
                .map(|loudness| NORMALIZE_TARGET_DB - loudness)
                .unwrap_or(0.0),
            _ => 0.0,
        };
        self.output_gain = db_to_linear(params.output_gain_db + normalization_db);
    }
}

impl Pedal for NamPedal {
    /// Per-sample path (several times slower in `nam-rs`). The audio
    /// callback uses `process_block`; this exists for the trait and for
    /// callers that process sample by sample.
    fn apply_effect(&mut self, input: i16) -> i16 {
        let Some(runtime) = &mut self.runtime else {
            return input;
        };
        let model_out = runtime
            .model
            .process_sample(input as f32 / 32768.0 * self.input_gain);
        runtime.finish(model_out, self.output_gain)
    }

    fn process_block(&mut self, samples: &mut [i16]) {
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        for chunk in samples.chunks_mut(MAX_CHUNK) {
            let scratch = &mut runtime.scratch[..chunk.len()];
            for (x, &sample) in scratch.iter_mut().zip(chunk.iter()) {
                *x = sample as f32 / 32768.0 * self.input_gain;
            }
            runtime.model.process_buffer(scratch);
            for i in 0..chunk.len() {
                let model_out = runtime.scratch[i];
                chunk[i] = runtime.finish(model_out, self.output_gain);
            }
        }
    }

    /// Knob changes keep the running network (and its state); only a new
    /// model path needs a rebuild.
    fn update_in_place(&mut self, description: &PedalDescription) -> bool {
        let PedalDescription::Nam(params) = description else {
            return false;
        };
        if params.model_path != self.model_path {
            return false;
        }
        self.set_gains(params);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny one-layer WaveNet from the `nam-rs` docs: small enough to inline,
    /// real enough to exercise parsing and the block path.
    const TINY_MODEL: &str = r#"{
        "version": "0.5.4", "architecture": "WaveNet", "sample_rate": 48000,
        "config": {
            "layers": [{
                "input_size": 1, "condition_size": 1, "channels": 1, "head_size": 1,
                "kernel_size": 1, "dilations": [1], "activation": "ReLU",
                "gated": false, "head_bias": false
            }],
            "head": null, "head_scale": 1.0
        },
        "weights": [1.0, 2.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0],
        "metadata": {"name": "Tiny", "loudness": -18.0}
    }"#;

    fn write_tiny_model(name: &str) -> String {
        let dir = std::env::temp_dir().join("guitar_virtual_amp_nam_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, TINY_MODEL).unwrap();
        path.display().to_string()
    }

    fn params(path: Option<String>) -> NamParams {
        NamParams {
            model_path: path,
            ..Default::default()
        }
    }

    #[test]
    fn passes_audio_through_without_a_model() {
        let mut pedal = NamPedal::new(params(None));
        for sample in [0i16, 1000, -1000, 32000] {
            assert_eq!(pedal.apply_effect(sample), sample);
        }
    }

    #[test]
    fn missing_file_falls_back_to_pass_through() {
        let mut pedal = NamPedal::new(params(Some("/nonexistent/model.nam".into())));
        assert_eq!(pedal.apply_effect(1234), 1234);
        assert!(preload("/nonexistent/model.nam").is_err());
    }

    #[test]
    fn preload_reports_model_info() {
        let path = write_tiny_model("info.nam");
        let info = preload(&path).expect("tiny model loads");
        assert_eq!(info.name.as_deref(), Some("Tiny"));
        assert_eq!(info.architecture, "WaveNet");
        assert!(!info.sample_rate_mismatch());
        assert_eq!(cached_info(&path), Some(Ok(info)));
    }

    #[test]
    fn produces_signal_after_warmup() {
        let path = write_tiny_model("signal.nam");
        let mut pedal = NamPedal::new(params(Some(path)));
        let mut outputs: Vec<i16> = (0..8000)
            .map(|i| if (i / 50) % 2 == 0 { 8000 } else { -8000 })
            .collect();
        pedal.process_block(&mut outputs);
        assert!(outputs[4000..].iter().any(|&s| s != 0));
    }

    /// The pedal must output exactly what the model produces, with no delay,
    /// on both the per-sample and the block path.
    #[test]
    fn matches_direct_model_output_without_delay() {
        let path = write_tiny_model("parity.nam");
        let params = NamParams {
            model_path: Some(path),
            normalize: false,
            ..Default::default()
        };

        let n = 8 * 1024;
        let input: Vec<i16> = (0..n)
            .map(|i| ((i as f32 * 0.05).sin() * 12000.0) as i16)
            .collect();

        let file = NamModel::from_json_str(TINY_MODEL).unwrap();
        let mut model = Model::from_nam(&file).unwrap();
        let mut dc = DcBlocker::new();
        let expected: Vec<i16> = input
            .iter()
            .map(|&s| model.process_sample(s as f32 / 32768.0))
            .map(|y| (dc.process(y) * 32768.0).clamp(-32768.0, 32767.0) as i16)
            .collect();
        let settled = model.receptive_field() + FADE_IN_SAMPLES;

        let mut per_sample = NamPedal::new(params.clone());
        let sample_out: Vec<i16> = input.iter().map(|&s| per_sample.apply_effect(s)).collect();

        let mut per_block = NamPedal::new(params);
        let mut block_out = input.clone();
        for block in block_out.chunks_mut(100) {
            per_block.process_block(block);
        }

        for i in settled..n {
            for (path, out) in [("sample", &sample_out), ("block", &block_out)] {
                let diff = (out[i] as i32 - expected[i] as i32).abs();
                assert!(
                    diff <= 1,
                    "{path} path, sample {i}: got {}, expected {}",
                    out[i],
                    expected[i]
                );
            }
        }
    }

    #[test]
    fn gain_change_updates_in_place_but_new_path_rebuilds() {
        let path = write_tiny_model("update.nam");
        let mut pedal = NamPedal::new(params(Some(path.clone())));

        let louder = NamParams {
            model_path: Some(path),
            output_gain_db: 6.0,
            ..Default::default()
        };
        assert!(pedal.update_in_place(&PedalDescription::Nam(louder)));

        let other = params(Some(write_tiny_model("other.nam")));
        assert!(!pedal.update_in_place(&PedalDescription::Nam(other)));
    }
}
