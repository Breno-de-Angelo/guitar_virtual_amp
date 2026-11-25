//! Neural beat and downbeat tracking with "Beat This!" (Foscarin, Schlüter
//! and Widmer, ISMIR 2024; code and weights MIT, CPJKU/beat_this), the
//! current state of the art across genres and the best option for solo
//! guitar, where the spectral-flux tracker in [`super::beats`] has no drums
//! to lock to. Runs the `final0` checkpoint, or the smaller `small0` when
//! that's the only one installed (ONNX conversions from
//! `nilsbc/beat-this-onnx`), with their log-mel front end in tract. On
//! GuitarSet accompaniment final0 gets beat CMLt 0.92 against 0.80.
//! `scripts/install_transcription_models.sh` downloads them into the models
//! folder; without them the pipeline keeps the DSP tracker.
//!
//! The network gives beat and downbeat logits at 50 frames per second. Beats
//! are its peaks (the paper's "minimal" post-processing, no DBN), repaired
//! into a regular sequence: missing beats are filled in and doubled ones
//! dropped, since the tab needs every beat. Bars come from the downbeat
//! logits through a bar-pointer Viterbi over 2 to 7 beats per bar, so the
//! meter can change within a song (Beat This! itself was trained on music
//! with time-signature changes and assumes no meter).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};

use tract_onnx::prelude::*;

use super::guitar_crnn::models_dir;

pub const SAMPLE_RATE: u32 = 22_050;
pub const FRAME_RATE: f64 = 50.0;
const CHUNK: usize = 1500;
const BORDER: usize = 6;
const MEL_BINS: usize = 128;
pub const FINAL_MODEL_FILE: &str = "beat_this_final0.onnx";
pub const SMALL_MODEL_FILE: &str = "beat_this_small0.onnx";
pub const FRONTEND_FILE: &str = "beat_this_logmel.onnx";
pub const FINAL_ENGINE_NAME: &str = "beat-this final0 (tract)";
pub const SMALL_ENGINE_NAME: &str = "beat-this small0 (tract)";

/// An installed Beat This! checkpoint and its front end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeatModels {
    pub front: PathBuf,
    pub net: PathBuf,
    /// Recorded in the tab's metadata.
    pub name: &'static str,
}

/// final0 when it's in `dir` with the front end, else small0.
fn models_in(dir: &Path) -> Option<BeatModels> {
    let front = dir.join(FRONTEND_FILE);
    if !front.is_file() {
        return None;
    }
    [
        (FINAL_MODEL_FILE, FINAL_ENGINE_NAME),
        (SMALL_MODEL_FILE, SMALL_ENGINE_NAME),
    ]
    .into_iter()
    .map(|(file, name)| (dir.join(file), name))
    .find(|(net, _)| net.is_file())
    .map(|(net, name)| BeatModels { front, net, name })
}

/// The models installed next to the guitar model (`GVA_BEAT_MODEL_DIR`
/// overrides the folder).
pub fn find_models() -> Option<BeatModels> {
    let dir = match std::env::var("GVA_BEAT_MODEL_DIR") {
        Ok(d) => PathBuf::from(d),
        // Not next to `GVA_GUITAR_MODEL`: that may point at a custom model.
        Err(_) => models_dir()?,
    };
    models_in(&dir)
}

type Model = Arc<TypedRunnableModel>;

fn load(path: &Path, shape: &[usize]) -> Result<Model, String> {
    tract_onnx::onnx()
        .model_for_path(path)
        .and_then(|m| m.with_input_fact(0, f32::fact(shape).into()))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|e| format!("não foi possível carregar o detector de tempos: {e}"))
}

fn network(path: &Path) -> Result<Model, String> {
    static CACHE: OnceLock<Mutex<Vec<(PathBuf, Model)>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| "cache do modelo corrompido")?;
    if let Some((_, m)) = cache.iter().find(|(p, _)| p == path) {
        return Ok(m.clone());
    }
    let m = load(path, &[1, CHUNK, MEL_BINS])?;
    cache.push((path.to_path_buf(), m.clone()));
    Ok(m)
}

/// Log-mel frames `(frames, 128)` of mono audio at [`SAMPLE_RATE`].
fn logmel(front: &Path, audio: &[f32]) -> Result<Vec<[f32; MEL_BINS]>, String> {
    // The front end has a free input length; fix it to this clip's.
    let len = audio.len().max(2048);
    let mut padded = audio.to_vec();
    padded.resize(len, 0.0);
    let model = load(front, &[1, len])?;
    let input = Tensor::from_shape(&[1, len], &padded).map_err(|e| e.to_string())?;
    let out = model
        .run(tvec!(input.into()))
        .map_err(|e| format!("erro no detector de tempos: {e}"))?;
    let view = out[0]
        .to_plain_array_view::<f32>()
        .map_err(|e| e.to_string())?;
    let frames = view.shape()[0];
    Ok((0..frames)
        .map(|f| std::array::from_fn(|b| view[[f, b]]))
        .collect())
}

/// Frame-wise beat and downbeat logits.
#[derive(Clone, Debug, Default)]
pub struct Activations {
    pub beat: Vec<f32>,
    pub downbeat: Vec<f32>,
}

/// Runs the network in 30 s chunks that overlap by [`BORDER`] frames, the
/// first chunk winning in overlaps, as in the reference inference.
pub fn activations(
    front: &Path,
    net: &Path,
    audio: &[f32],
    cancel: &AtomicBool,
) -> Result<Activations, String> {
    let spect = logmel(front, audio)?;
    let model = network(net)?;
    let n = spect.len();
    let step = CHUNK - 2 * BORDER;
    let mut starts: Vec<isize> = (0..)
        .map(|k| k as isize * step as isize - BORDER as isize)
        .take_while(|s| *s < n as isize - BORDER as isize)
        .collect();
    if starts.is_empty() {
        starts.push(-(BORDER as isize));
    }
    if n > step {
        *starts.last_mut().unwrap() = n as isize - (CHUNK - BORDER) as isize;
    }
    let mut act = Activations {
        beat: vec![-1000.0; n],
        downbeat: vec![-1000.0; n],
    };
    let outputs = super::parallel_map(
        starts.len(),
        4,
        &|c| {
            let start = starts[c];
            let mut chunk = vec![0f32; CHUNK * MEL_BINS];
            for k in 0..CHUNK {
                let f = start + k as isize;
                if (0..n as isize).contains(&f) {
                    chunk[k * MEL_BINS..(k + 1) * MEL_BINS].copy_from_slice(&spect[f as usize]);
                }
            }
            let input =
                Tensor::from_shape(&[1, CHUNK, MEL_BINS], &chunk).map_err(|e| e.to_string())?;
            let out = model
                .run(tvec!(input.into()))
                .map_err(|e| format!("erro no detector de tempos: {e}"))?;
            let read = |k: usize| -> Result<Vec<f32>, String> {
                let view = out[k]
                    .to_plain_array_view::<f32>()
                    .map_err(|e| e.to_string())?;
                Ok((0..CHUNK).map(|i| view[[0, i]]).collect())
            };
            Ok((read(0)?, read(1)?))
        },
        &mut |_| {},
        cancel,
    )?;
    // Earlier chunks win where chunks overlap.
    for (&start, (beat, down)) in starts.iter().zip(&outputs).rev() {
        for k in BORDER..CHUNK - BORDER {
            let f = start + k as isize;
            if (0..n as isize).contains(&f) {
                act.beat[f as usize] = beat[k];
                act.downbeat[f as usize] = down[k];
            }
        }
    }
    Ok(act)
}

/// Frames that are the maximum within ±3 frames (±60 ms) and above 0.5
/// probability, with runs of adjacent peaks merged into their mean.
pub fn peaks(logits: &[f32]) -> Vec<f64> {
    let n = logits.len();
    let raw: Vec<usize> = (0..n)
        .filter(|&i| {
            let lo = i.saturating_sub(3);
            let hi = (i + 3).min(n - 1);
            logits[i] > 0.0 && (lo..=hi).all(|j| logits[j] <= logits[i])
        })
        .collect();
    let mut out: Vec<f64> = Vec::new();
    let mut group: Vec<usize> = Vec::new();
    for i in raw {
        if group.last().is_some_and(|&last| i - last > 1) {
            out.push(group.iter().sum::<usize>() as f64 / group.len() as f64);
            group.clear();
        }
        group.push(i);
    }
    if !group.is_empty() {
        out.push(group.iter().sum::<usize>() as f64 / group.len() as f64);
    }
    out
}

fn median(values: &[f64]) -> Option<f64> {
    let mut v = values.to_vec();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

/// Makes beat frames regular: drops beats much closer than the local beat
/// period (keeping the stronger one) and fills gaps of about k periods with
/// k-1 evenly spaced beats.
pub fn regularize(beats: &[f64], logits: &[f32]) -> Vec<f64> {
    if beats.len() < 4 {
        return beats.to_vec();
    }
    let strength = |f: f64| logits.get(f.round() as usize).copied().unwrap_or(-1000.0);
    let local_period = |beats: &[f64], i: usize| -> f64 {
        let lo = i.saturating_sub(8);
        let hi = (i + 8).min(beats.len() - 1);
        let gaps: Vec<f64> = beats[lo..=hi].windows(2).map(|w| w[1] - w[0]).collect();
        median(&gaps).unwrap_or(1.0)
    };
    let mut kept: Vec<f64> = Vec::with_capacity(beats.len());
    for &b in beats {
        if let Some(&last) = kept.last() {
            let period = local_period(beats, kept.len());
            if b - last < 0.6 * period {
                if strength(b) > strength(last) {
                    *kept.last_mut().unwrap() = b;
                }
                continue;
            }
        }
        kept.push(b);
    }
    let mut out = Vec::with_capacity(kept.len() * 2);
    for i in 0..kept.len() {
        if i > 0 {
            let gap = kept[i] - kept[i - 1];
            let period = local_period(&kept, i);
            let k = (gap / period).round() as usize;
            if k >= 2 && gap > 1.5 * period {
                for j in 1..k {
                    out.push(kept[i - 1] + gap * j as f64 / k as f64);
                }
            }
        }
        out.push(kept[i]);
    }
    out
}

/// Tracked beats with bar information.
#[derive(Clone, Debug, PartialEq)]
pub struct BeatTrack {
    /// Beat times in seconds.
    pub beats: Vec<f64>,
    /// Indices into `beats` of the downbeats, ascending.
    pub downbeats: Vec<usize>,
}

impl BeatTrack {
    pub fn bpm(&self) -> Option<f64> {
        let gaps: Vec<f64> = self.beats.windows(2).map(|w| w[1] - w[0]).collect();
        median(&gaps).map(|p| 60.0 / p.max(1e-3))
    }
}

/// Bar lengths the decoder considers, with their cost per bar (in nats):
/// 4/4 is the default, 3/4 is common, 2/4 bars are the usual inserted half
/// bars, 5, 6 and 7 are rarer.
const METERS: [(usize, f64); 6] = [(4, 0.0), (3, 0.4), (2, 1.6), (6, 1.8), (5, 2.4), (7, 2.4)];
/// Cost of a bar whose length differs from the previous bar's.
const METER_CHANGE: f64 = 3.0;

/// Downbeat probability at each beat: the best logit within ±2 frames
/// (downbeat peaks can sit a frame off), through a sigmoid.
fn downbeat_probs(beat_frames: &[f64], downbeat_logits: &[f32]) -> Vec<f64> {
    beat_frames
        .iter()
        .map(|&f| {
            let i = f.round() as isize;
            let logit = (i - 2..=i + 2)
                .filter_map(|j| usize::try_from(j).ok())
                .filter_map(|j| downbeat_logits.get(j))
                .fold(-1000f32, |m, v| m.max(*v))
                .clamp(-30.0, 30.0);
            1.0 / (1.0 + (-logit as f64).exp())
        })
        .collect()
}

/// Downbeats among the beats: a bar-pointer Viterbi over beats whose state
/// is (bar length, position in the bar). The network's probability `p` is
/// scored against the song's mean `q`: a downbeat pays `-ln(p / q)`, any
/// other beat `-ln((1 - p) / (1 - q))`, so beats the network can't tell
/// apart cost nothing either way and the meter prior decides. Each bar pays
/// its length's cost from [`METERS`], plus [`METER_CHANGE`] when the length
/// changes. The first bar may start mid-bar (a pickup). Returns beat
/// indices.
pub fn decode_downbeats(probs: &[f64]) -> Vec<usize> {
    let n = probs.len();
    if n == 0 {
        return Vec::new();
    }
    let states: Vec<(usize, usize, f64)> = METERS
        .iter()
        .flat_map(|&(m, cost)| (0..m).map(move |k| (m, k, cost)))
        .collect();
    let clamp = |p: f64| p.clamp(0.02, 0.98);
    let q = (probs.iter().map(|&p| clamp(p)).sum::<f64>() / n as f64).clamp(0.05, 0.5);
    let emit = |i: usize, k: usize| {
        let p = clamp(probs[i]);
        if k == 0 {
            -(p / q).ln()
        } else {
            -((1.0 - p) / (1.0 - q)).ln()
        }
    };
    let mut cost: Vec<f64> = states.iter().map(|&(_, k, c)| c + emit(0, k)).collect();
    let mut back: Vec<Vec<usize>> = Vec::with_capacity(n);
    back.push(vec![usize::MAX; states.len()]);
    for i in 1..n {
        let mut next = vec![f64::INFINITY; states.len()];
        let mut from = vec![usize::MAX; states.len()];
        for (s, &(m, k, bar_cost)) in states.iter().enumerate() {
            if k > 0 {
                // Inside a bar: only from the previous position.
                let prev = states
                    .iter()
                    .position(|&(pm, pk, _)| pm == m && pk == k - 1)
                    .unwrap();
                next[s] = cost[prev];
                from[s] = prev;
            } else {
                // A new bar: from the last beat of any bar.
                for (prev, &(pm, pk, _)) in states.iter().enumerate() {
                    if pk + 1 != pm {
                        continue;
                    }
                    let change = if pm == m { 0.0 } else { METER_CHANGE };
                    let c = cost[prev] + bar_cost + change;
                    if c < next[s] {
                        next[s] = c;
                        from[s] = prev;
                    }
                }
            }
            next[s] += emit(i, k);
        }
        cost = next;
        back.push(from);
    }
    let mut s = (0..states.len())
        .min_by(|a, b| cost[*a].total_cmp(&cost[*b]))
        .unwrap();
    let mut downbeats = Vec::new();
    for i in (0..n).rev() {
        if states[s].1 == 0 {
            downbeats.push(i);
        }
        s = back[i][s];
        if s == usize::MAX {
            break;
        }
    }
    downbeats.reverse();
    downbeats
}

/// Beats and bars of `audio` (mono, [`SAMPLE_RATE`]).
pub fn track(
    front: &Path,
    net: &Path,
    audio: &[f32],
    cancel: &AtomicBool,
) -> Result<BeatTrack, String> {
    let act = activations(front, net, audio, cancel)?;
    Ok(track_from_activations(&act))
}

pub fn track_from_activations(act: &Activations) -> BeatTrack {
    let frames = regularize(&peaks(&act.beat), &act.beat);
    let downbeats = decode_downbeats(&downbeat_probs(&frames, &act.downbeat));
    BeatTrack {
        beats: frames.iter().map(|f| f / FRAME_RATE).collect(),
        downbeats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final0_is_preferred_over_small0() {
        let dir = std::env::temp_dir().join(format!("gva_beat_pick_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let touch = |file: &str| std::fs::write(dir.join(file), b"").unwrap();
        touch(SMALL_MODEL_FILE);
        touch(FINAL_MODEL_FILE);
        // No front end, no tracker.
        assert_eq!(models_in(&dir), None);
        touch(FRONTEND_FILE);
        let found = models_in(&dir).unwrap();
        assert_eq!(
            (found.net, found.name),
            (dir.join(FINAL_MODEL_FILE), FINAL_ENGINE_NAME)
        );
        std::fs::remove_file(dir.join(FINAL_MODEL_FILE)).unwrap();
        let found = models_in(&dir).unwrap();
        assert_eq!(
            (found.front, found.net, found.name),
            (
                dir.join(FRONTEND_FILE),
                dir.join(SMALL_MODEL_FILE),
                SMALL_ENGINE_NAME
            )
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Logits with a peak of `height` every `period` frames from `offset`.
    fn pulses(n: usize, period: usize, offset: usize, height: f32) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let d = (i as isize - offset as isize).rem_euclid(period as isize) as usize;
                let d = d.min(period - d) as f32;
                height - 4.0 * d
            })
            .collect()
    }

    #[test]
    fn peaks_are_local_maxima_above_half_probability() {
        let mut l = vec![-5.0f32; 40];
        l[10] = 2.0;
        l[11] = 2.0; // a flat top: one peak in the middle
        l[25] = -0.5; // below 0.5 probability
        l[30] = 1.0;
        l[32] = 3.0; // 30 isn't a maximum within ±3 frames
        assert_eq!(peaks(&l), vec![10.5, 32.0]);
    }

    #[test]
    fn regularizing_fills_missing_and_drops_doubled_beats() {
        let mut beats: Vec<f64> = (0..20).map(|i| 10.0 + i as f64 * 25.0).collect();
        beats.remove(7); // a missed beat
        beats.insert(12, beats[11] + 4.0); // a double detection
        let logits = vec![1.0f32; 1000];
        let fixed = regularize(&beats, &logits);
        assert_eq!(fixed.len(), 20, "{fixed:?}");
        for (i, b) in fixed.iter().enumerate() {
            assert!(
                (b - (10.0 + i as f64 * 25.0)).abs() < 4.5,
                "beat {i} at {b}"
            );
        }
    }

    /// Bar lengths between consecutive downbeats.
    fn lengths(downbeats: &[usize]) -> Vec<usize> {
        downbeats.windows(2).map(|w| w[1] - w[0]).collect()
    }

    #[test]
    fn bars_follow_the_downbeat_logits() {
        let beats: Vec<f64> = (0..24).map(|i| 5.0 + i as f64 * 25.0).collect();
        let n = 700;
        // Downbeats on beats 1, 5, 9... (4/4 after a one-beat pickup).
        let four = decode_downbeats(&downbeat_probs(&beats, &pulses(n, 100, 30, 3.0)));
        assert_eq!(four[0], 1, "{four:?}");
        assert!(lengths(&four).iter().all(|&l| l == 4), "{four:?}");
        // Every third beat from beat 2: 3/4.
        let three = decode_downbeats(&downbeat_probs(&beats, &pulses(n, 75, 55, 3.0)));
        assert_eq!(three[0], 2, "{three:?}");
        assert!(lengths(&three).iter().all(|&l| l == 3), "{three:?}");
        // No downbeat information at all: 4/4.
        let none = decode_downbeats(&downbeat_probs(&beats, &vec![-10.0; n]));
        assert!(lengths(&none).iter().all(|&l| l == 4), "{none:?}");
    }

    #[test]
    fn the_meter_can_change_mid_song() {
        // Four bars of 4/4, one 2/4 bar, then four bars of 3/4 (the kind of
        // turnaround the old single-meter choice wrote as shifted 4/4 bars).
        let mut lens = vec![4, 4, 4, 4, 2];
        lens.extend([3, 3, 3, 3]);
        let mut probs = Vec::new();
        for &l in &lens {
            probs.push(0.9);
            probs.extend(std::iter::repeat_n(0.1, l - 1));
        }
        let downbeats = decode_downbeats(&probs);
        assert_eq!(downbeats[0], 0);
        assert_eq!(lengths(&downbeats), lens[..lens.len() - 1].to_vec());
        assert_eq!(*downbeats.last().unwrap(), probs.len() - 3);
    }

    #[test]
    fn one_unclear_downbeat_does_not_change_the_meter() {
        // 4/4 with weak downbeats and one strong beat 3 in the middle.
        let mut probs: Vec<f64> = (0..40)
            .map(|i| if i % 4 == 0 { 0.7 } else { 0.2 })
            .collect();
        probs[18] = 0.8;
        probs[16] = 0.3;
        let downbeats = decode_downbeats(&probs);
        assert!(lengths(&downbeats).iter().all(|&l| l == 4), "{downbeats:?}");
    }
}
