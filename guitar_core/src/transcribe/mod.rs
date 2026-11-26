//! Automatic tablature from an audio file, fully offline with open-source
//! models:
//!
//! 1. decode the file (Symphonia) and hash it;
//! 2. separate guitar and bass with BS-RoFormer SW or Demucs `htdemucs_6s`,
//!    when installed ([`separation`]); otherwise transcribe from the mix;
//! 3. track beats and downbeats with Beat This! ([`beat_net`]; bars of 2 to
//!    7 beats, the meter may change mid-song), or with the spectral-flux
//!    tracker in 4/4 ([`beats`]) when that model isn't installed;
//! 4. transcribe the guitar with the guitar-trained CRNN ([`guitar_crnn`],
//!    optional) or Basic Pitch, and the bass with Basic Pitch
//!    ([`basic_pitch`]); read bends, vibrato, slides and hammer-ons from
//!    Basic Pitch's pitch contour ([`techniques`]);
//! 5. group chords, quantize each beat to 16ths or 8th triplets, pick
//!    strings/frets (Viterbi over hand positions with a prior learned from
//!    guitarists) and lay out bars ([`arrange`]);
//! 6. save a Guitar Pro 5 file plus sidecar into the tab repository
//!    (`shared::tab::repository`).
//!
//! The optional models come from `scripts/install_transcription_models.sh`.
//! With separation, the stereo guitar stem is also split by pan position
//! ([`stereo`]): guitars on different sides of the mix that play different
//! parts become separate tracks.
//!
//! Everything is blocking; run it on a worker thread. Limits: guitars
//! playing different parts from the same spot of the mix stay one track
//! guitars, no drums/vocals, no palm muting or harmonics.

pub mod arrange;
pub mod basic_pitch;
pub mod beat_net;
pub mod beats;
#[cfg(test)]
mod eval;
pub mod fingering_net;
pub mod guitar_crnn;
pub mod resample;
pub mod separation;
pub mod stereo;
pub mod techniques;
pub mod youtube;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backend::audio_file::decode_file;
use crate::shared::tab::model::Score;
use crate::shared::tab::repository::{self, METADATA_VERSION, SourceAudio, TabMetadata, TrackInfo};
use basic_pitch::{NoteEstimate, NoteParams};

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub audio_path: PathBuf,
    pub title: String,
    pub artist: String,
    /// Separate the instruments when a separator is installed.
    pub separate: bool,
    /// Which guitar CRNN to run when both are installed.
    pub guitar_model: guitar_crnn::GuitarModel,
    /// Learned fingering (when installed) or the Viterbi, for guitars.
    pub fingering: fingering_net::Fingering,
    /// Estimate each guitar track's tone and save it as a user preset
    /// tagged with the song (`crate::tone`; needs separation).
    pub tone: bool,
    pub guitar: bool,
    pub bass: bool,
    /// Tab repository folder.
    pub output_dir: PathBuf,
    pub spotify_track_id: Option<String>,
    /// Set by [`run_youtube`]: the video the audio came from.
    pub youtube_video_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Downloading,
    Decoding,
    Separating,
    Beats,
    Guitar,
    Techniques,
    Bass,
    Saving,
    Tone,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Downloading => "Baixando o áudio do YouTube",
            Stage::Decoding => "Lendo o áudio",
            Stage::Separating => "Separando instrumentos",
            Stage::Beats => "Detectando andamento e compassos",
            Stage::Guitar => "Transcrevendo a guitarra",
            Stage::Techniques => "Detectando bends, slides e vibratos",
            Stage::Bass => "Transcrevendo o baixo",
            Stage::Saving => "Gravando a tab",
            Stage::Tone => "Estimando o timbre da guitarra",
        }
    }

    /// Share of the whole job before this stage, and this stage's share.
    fn span(self, separating: bool) -> (f32, f32) {
        let sep = if separating { 0.35 } else { 0.0 };
        match self {
            Stage::Downloading | Stage::Decoding => (0.0, 0.05),
            Stage::Separating => (0.05, sep),
            Stage::Beats => (0.05 + sep, 0.05),
            Stage::Guitar => (0.10 + sep, (0.83 - sep) * 0.45),
            Stage::Techniques => (0.10 + sep + (0.83 - sep) * 0.45, (0.83 - sep) * 0.15),
            Stage::Bass => (0.10 + sep + (0.83 - sep) * 0.6, (0.83 - sep) * 0.4),
            Stage::Saving => (0.93, 0.02),
            Stage::Tone => (0.95, 0.05),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub stage: Stage,
    /// Whole job, 0..=1.
    pub overall: f32,
}

#[derive(Clone, Debug)]
pub struct Output {
    pub tab_path: PathBuf,
    pub metadata: TabMetadata,
    pub score: Score,
    /// True when the same audio had been transcribed before and was redone.
    pub replaced: bool,
    /// Notes of the first guitar track as transcribed, before quantization
    /// (audio time).
    pub guitar_notes: Vec<NoteEstimate>,
    /// Techniques of `guitar_notes`, index for index.
    pub guitar_techniques: Vec<techniques::Technique>,
    /// With [`Request::tone`]: (track name, preset file) per guitar track.
    pub tone_presets: Vec<(String, PathBuf)>,
}

/// Splits "Artist - Title" file names; otherwise the whole stem is the title.
pub fn guess_artist_title(path: &Path) -> (String, String) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = stem.replace('_', " ");
    // Drop a leading track number ("01 - ", "01. ").
    let trimmed = stem
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['.', ' ', '-']);
    let stem = if trimmed.len() + 2 < stem.len() && !trimmed.is_empty() {
        trimmed.to_string()
    } else {
        stem
    };
    match stem.split_once(" - ") {
        Some((artist, title)) => (artist.trim().to_string(), title.trim().to_string()),
        None => (String::new(), stem.trim().to_string()),
    }
}

fn check(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("cancelado".into())
    } else {
        Ok(())
    }
}

/// Decoded mono audio, resampled on demand for each model.
#[derive(Clone)]
struct Mono {
    samples: Vec<f32>,
    rate: u32,
}

impl Mono {
    fn load(path: &Path) -> Result<Self, String> {
        let audio = decode_file(path)?;
        Ok(Self {
            samples: audio.samples,
            rate: audio.sample_rate,
        })
    }

    fn duration(&self) -> f64 {
        self.samples.len() as f64 / self.rate.max(1) as f64
    }

    fn at(&self, rate: u32) -> Vec<f32> {
        resample::resample(&self.samples, self.rate, rate)
    }
}

/// Runs `job(0..count)` on up to `max_threads` worker threads (model
/// inference on independent audio windows), reporting the finished share to
/// `progress` from the calling thread. Stops at the first error or when
/// `cancel` is set.
pub(crate) fn parallel_map<T: Send>(
    count: usize,
    max_threads: usize,
    job: &(dyn Fn(usize) -> Result<T, String> + Sync),
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Vec<T>, String> {
    use std::sync::atomic::AtomicUsize;
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, max_threads.max(1))
        .min(count.max(1));
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<T, String>)>();
    let mut results: Vec<Option<T>> = (0..count).map(|_| None).collect();
    let mut error = None;
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let (next, failed, tx) = (&next, &failed, tx.clone());
            scope.spawn(move || {
                while !cancel.load(Ordering::Relaxed) && !failed.load(Ordering::Relaxed) {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= count {
                        break;
                    }
                    let result = job(i);
                    if result.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    if tx.send((i, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut done = 0;
        for (i, result) in rx {
            match result {
                Ok(value) => {
                    results[i] = Some(value);
                    done += 1;
                    progress(done as f32 / count.max(1) as f32);
                }
                Err(e) => error = Some(e),
            }
        }
    });
    if let Some(e) = error {
        return Err(e);
    }
    check(cancel)?;
    results
        .into_iter()
        .map(|r| r.ok_or_else(|| "janela sem resultado".to_string()))
        .collect()
}

/// Which optional models are installed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstalledModels {
    /// The GAPS guitar CRNN (`scripts/install_transcription_models.sh`), or
    /// the `GVA_GUITAR_MODEL` file.
    pub guitar_crnn: bool,
    /// The local electric fine-tune ([`guitar_crnn::ELECTRIC_MODEL_FILE`]).
    pub electric_crnn: bool,
    /// Engine name of the Beat This! checkpoint that will run.
    pub beat_tracker: Option<&'static str>,
    /// The local learned fingering model ([`fingering_net::MODEL_FILE`]).
    pub fingering_tagger: bool,
}

impl InstalledModels {
    /// Everything `scripts/install_transcription_models.sh` installs.
    pub fn complete(&self) -> bool {
        self.guitar_crnn && self.beat_tracker == Some(beat_net::FINAL_ENGINE_NAME)
    }
}

pub fn installed_models() -> InstalledModels {
    InstalledModels {
        guitar_crnn: guitar_crnn::find_model().is_some(),
        electric_crnn: guitar_crnn::find_electric_model().is_some(),
        beat_tracker: beat_net::find_models().map(|m| m.name),
        fingering_tagger: fingering_net::find_model().is_some(),
    }
}

/// Engine name of the guitar notes model for `choice`, as recorded in
/// the tab's metadata.
pub fn guitar_engine_name(choice: guitar_crnn::GuitarModel) -> &'static str {
    guitar_engine(choice).name()
}

/// Note transcription model for a stem.
#[derive(Clone, Debug, PartialEq)]
enum Engine {
    BasicPitch(NoteParams),
    /// Guitar CRNN at this ONNX path, with its engine name.
    GuitarCrnn(PathBuf, guitar_crnn::CrnnParams, &'static str),
}

impl Engine {
    /// `prune_weak` ratios: Basic Pitch reports a string's harmonics as
    /// notes; the guitar CRNN was trained not to, so it only loses
    /// detections much weaker than what starts with them.
    fn pruning(&self) -> (f32, f32) {
        match self {
            Engine::BasicPitch(_) => (0.25, 1.0),
            Engine::GuitarCrnn(..) => (0.1, 0.0),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Engine::BasicPitch(_) => "basic-pitch icassp_2022 (tract)",
            Engine::GuitarCrnn(_, _, name) => name,
        }
    }
}

/// The guitar CRNN `choice` when installed (GAPS when the electric one
/// isn't; unless `GVA_GUITAR_ENGINE=basic-pitch`), else Basic Pitch.
fn guitar_engine(choice: guitar_crnn::GuitarModel) -> Engine {
    let forced_basic = std::env::var("GVA_GUITAR_ENGINE").is_ok_and(|v| v == "basic-pitch");
    match guitar_crnn::find_model_for(choice) {
        Some((path, name)) if !forced_basic => {
            Engine::GuitarCrnn(path, guitar_crnn::CrnnParams::guitar(), name)
        }
        _ => Engine::BasicPitch(NoteParams::guitar()),
    }
}

/// Drops detections that are probably overtones or bleed: notes much weaker
/// than the loudest note starting with them, and notes an octave, an octave
/// and a fifth or two octaves above a louder note starting with them (the
/// model often reports a string's 2nd-4th harmonics as notes).
fn prune_weak(notes: Vec<NoteEstimate>, ratio: f32, overtone_ratio: f32) -> Vec<NoteEstimate> {
    let mut kept = Vec::with_capacity(notes.len());
    for note in &notes {
        let together = notes.iter().filter(|o| (o.start - note.start).abs() < 0.04);
        let loudest = together.clone().map(|o| o.amplitude).fold(0.0f32, f32::max);
        let overtone = together
            .filter(|o| matches!(note.pitch.checked_sub(o.pitch), Some(12 | 19 | 24)))
            .any(|o| note.amplitude < o.amplitude * overtone_ratio);
        if note.amplitude >= loudest * ratio && !overtone {
            kept.push(*note);
        }
    }
    kept
}

/// Notes of a stem, plus Basic Pitch's posteriors when that was the engine
/// (their contour serves technique detection).
fn transcribe_stem(
    audio: &Mono,
    engine: &Engine,
    stage: Stage,
    separating: bool,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<(Vec<NoteEstimate>, Option<basic_pitch::Posteriors>), String> {
    #[cfg(test)]
    let cached = eval::cached_notes(audio, engine);
    #[cfg(not(test))]
    let cached = None;
    let (notes, post) = match cached {
        Some(notes) => (notes, None),
        None => {
            let (notes, post) =
                transcribe_stem_uncached(audio, engine, stage, separating, progress, cancel)?;
            #[cfg(test)]
            eval::cache_notes(audio, engine, &notes);
            (notes, post)
        }
    };
    let (ratio, overtone_ratio) = engine.pruning();
    Ok((prune_weak(notes, ratio, overtone_ratio), post))
}

fn transcribe_stem_uncached(
    audio: &Mono,
    engine: &Engine,
    stage: Stage,
    separating: bool,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<(Vec<NoteEstimate>, Option<basic_pitch::Posteriors>), String> {
    let (base, share) = stage.span(separating);
    let mut report = |p: f32| {
        progress(Progress {
            stage,
            overall: base + share * p,
        })
    };
    match engine {
        Engine::BasicPitch(params) => {
            let post =
                basic_pitch::posteriors(&audio.at(basic_pitch::SAMPLE_RATE), &mut report, cancel)?;
            let notes = basic_pitch::extract_notes(&post, params);
            Ok((notes, Some(post)))
        }
        Engine::GuitarCrnn(path, params, _) => {
            let post = guitar_crnn::posteriors(
                path,
                &audio.at(guitar_crnn::SAMPLE_RATE),
                &mut report,
                cancel,
            )?;
            Ok((guitar_crnn::extract_notes(&post, params), None))
        }
    }
}

/// A pan part needs this share of the guitar stem's energy to be
/// transcribed on its own.
const MIN_PART_SHARE: f32 = 0.12;
/// Parts whose transcriptions agree this much (note F1) are one guitar
/// (double-tracked, or one guitar panned between two parts).
const SAME_PART_F1: f64 = 0.5;

/// Longest echo between two pan parts that still counts as one guitar (a
/// stereo or ping-pong delay puts late copies of its notes on one side).
const MAX_ECHO_SECS: f64 = 0.8;

/// Note F1 between two transcriptions (same pitch, onsets within 50 ms),
/// with `b` shifted by `lag` seconds.
fn agreement_at(a: &[NoteEstimate], b: &[NoteEstimate], lag: f64) -> f64 {
    // `b` sorted by start: binary search the 50 ms window of each note.
    let mut used = vec![false; b.len()];
    let mut hits = 0usize;
    for n in a {
        let t = n.start + lag;
        let from = b.partition_point(|m| m.start < t - 0.05);
        if let Some(j) = (from..b.len())
            .take_while(|&j| b[j].start <= t + 0.05)
            .find(|&j| !used[j] && b[j].pitch == n.pitch)
        {
            used[j] = true;
            hits += 1;
        }
    }
    2.0 * hits as f64 / (a.len() + b.len()).max(1) as f64
}

/// How much two transcriptions are the same playing: the best note F1
/// over echoes of `b` up to [`MAX_ECHO_SECS`] late.
fn agreement(a: &[NoteEstimate], b: &[NoteEstimate]) -> f64 {
    let mut b = b.to_vec();
    b.sort_by(|x, y| x.start.total_cmp(&y.start));
    (0..=(MAX_ECHO_SECS * 100.0) as usize)
        .map(|k| agreement_at(a, &b, k as f64 * 0.01))
        .fold(0.0, f64::max)
}

/// Share of a part's note onsets that are chords (2+ notes together).
fn chord_share(notes: &[NoteEstimate]) -> f64 {
    let chorded = notes
        .iter()
        .filter(|n| {
            notes
                .iter()
                .any(|o| !std::ptr::eq(*n, o) && (o.start - n.start).abs() < 0.035)
        })
        .count();
    chorded as f64 / notes.len().max(1) as f64
}

/// A transcribed guitar part.
struct GuitarPart {
    /// The part's audio (its pan region of the stem, or the whole input).
    audio: Mono,
    notes: Vec<NoteEstimate>,
    techniques: Vec<techniques::Technique>,
    side: Option<stereo::Side>,
    share: f32,
}

/// Pan split on, unless `GVA_SPLIT_GUITARS=0`.
fn split_enabled() -> bool {
    std::env::var("GVA_SPLIT_GUITARS").map_or(true, |v| v != "0")
}

/// Progress of part `k` of `count` inside `stage`'s span.
fn part_progress<'a>(
    progress: &'a mut dyn FnMut(Progress),
    separating: bool,
    k: usize,
    count: usize,
) -> impl FnMut(Progress) + 'a {
    move |p: Progress| {
        let (base, share) = p.stage.span(separating);
        let f = if share > 0.0 {
            ((p.overall - base) / share).clamp(0.0, 1.0)
        } else {
            0.0
        };
        progress(Progress {
            stage: p.stage,
            overall: base + share * (k as f32 + f) / count.max(1) as f32,
        })
    }
}

/// Notes and techniques of the guitar: one part from `audio`, or, given
/// the stereo stem, one per pan region with its own share of the stem
/// (left, center, right). Regions whose notes agree are one guitar and
/// keep only the loudest; regions with few notes are bleed. Returns the
/// parts with the most chorded first (the rhythm guitar).
fn transcribe_guitars(
    audio: &Mono,
    stereo_stem: Option<&crate::backend::audio_file::DecodedStereo>,
    engine: &Engine,
    separating: bool,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<Vec<GuitarPart>, String> {
    let candidates: Vec<(Mono, Option<stereo::Side>, f32)> = match stereo_stem {
        Some(st) if !st.mono_source && split_enabled() => {
            let parts: Vec<_> = stereo::split_by_pan(&st.left, &st.right)
                .into_iter()
                .filter(|p| p.share >= MIN_PART_SHARE)
                .collect();
            if parts.len() >= 2 {
                parts
                    .into_iter()
                    .map(|p| {
                        let mono = Mono {
                            samples: p.samples,
                            rate: st.sample_rate,
                        };
                        (mono, Some(p.side), p.share)
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };
    if candidates.is_empty() {
        let (notes, post) =
            transcribe_stem(audio, engine, Stage::Guitar, separating, progress, cancel)?;
        let (notes, techniques) =
            detect_techniques(audio, notes, post, separating, progress, cancel)?;
        return Ok(vec![GuitarPart {
            audio: Mono {
                samples: audio.samples.clone(),
                rate: audio.rate,
            },
            notes,
            techniques,
            side: None,
            share: 1.0,
        }]);
    }
    let count = candidates.len();
    let mut parts: Vec<GuitarPart> = Vec::with_capacity(count);
    for (k, (part_audio, side, share)) in candidates.into_iter().enumerate() {
        let mut sub = part_progress(progress, separating, k, count);
        let (notes, post) = transcribe_stem(
            &part_audio,
            engine,
            Stage::Guitar,
            separating,
            &mut sub,
            cancel,
        )?;
        let (notes, techniques) =
            detect_techniques(&part_audio, notes, post, separating, &mut sub, cancel)?;
        parts.push(GuitarPart {
            audio: part_audio,
            notes,
            techniques,
            side,
            share,
        });
    }
    // Loudest first; a part that plays what a louder one plays is the same
    // guitar.
    parts.sort_by(|a, b| b.share.total_cmp(&a.share));
    let most = parts.iter().map(|p| p.notes.len()).max().unwrap_or(0);
    let mut kept: Vec<GuitarPart> = Vec::new();
    for part in parts {
        let same = kept.iter().any(|k| {
            agreement(&k.notes, &part.notes) >= SAME_PART_F1
                || agreement(&part.notes, &k.notes) >= SAME_PART_F1
        });
        let bleed = part.notes.len() * 10 < most;
        if !same && !bleed {
            kept.push(part);
        }
    }
    kept.sort_by(|a, b| chord_share(&b.notes).total_cmp(&chord_share(&a.notes)));
    Ok(kept)
}

/// Technique detection on, unless `GVA_TECHNIQUES=0`.
fn techniques_enabled() -> bool {
    std::env::var("GVA_TECHNIQUES").map_or(true, |v| v != "0")
}

/// Bends, vibrato, slides and hammer-ons of the guitar notes, from Basic
/// Pitch's contour (`post`, computed here when the guitar engine wasn't
/// Basic Pitch). Bent notes the model split in two come back merged.
fn detect_techniques(
    audio: &Mono,
    notes: Vec<NoteEstimate>,
    post: Option<basic_pitch::Posteriors>,
    separating: bool,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<(Vec<NoteEstimate>, Vec<techniques::Technique>), String> {
    let (base, share) = Stage::Techniques.span(separating);
    progress(Progress {
        stage: Stage::Techniques,
        overall: base,
    });
    if notes.is_empty() || !techniques_enabled() {
        let n = notes.len();
        return Ok((notes, vec![techniques::Technique::default(); n]));
    }
    let samples = audio.at(basic_pitch::SAMPLE_RATE);
    let post = match post {
        Some(post) => post,
        None => basic_pitch::posteriors(
            &samples,
            &mut |p| {
                progress(Progress {
                    stage: Stage::Techniques,
                    overall: base + share * p,
                })
            },
            cancel,
        )?,
    };
    check(cancel)?;
    let evidence = techniques::Evidence {
        post: &post,
        audio: &samples,
        rate: basic_pitch::SAMPLE_RATE,
    };
    Ok(techniques::detect(
        notes,
        &evidence,
        &techniques::TechniqueParams::default(),
    ))
}

/// Beats, meter and tempo of the song.
#[derive(Clone, Debug, PartialEq)]
struct Rhythm {
    /// Beat times in seconds.
    beats: Vec<f64>,
    /// Indices into `beats` of the downbeats.
    downbeats: Vec<usize>,
    bpm: f64,
    engine: &'static str,
}

/// Beats from Beat This! on the mix when installed (it handles any
/// instrumentation and meter); otherwise the DSP tracker, on the drum stem
/// when there is one (kick and snare mark the beats far better than
/// strummed chords) or the mix, in 4/4.
fn track_rhythm(mix: &Mono, drums: Option<&Mono>, cancel: &AtomicBool) -> Result<Rhythm, String> {
    if let Some(models) = beat_net::find_models() {
        let audio = mix.at(beat_net::SAMPLE_RATE);
        let track = beat_net::track(&models.front, &models.net, &audio, cancel)?;
        if track.beats.len() >= 8 {
            return Ok(Rhythm {
                bpm: track.bpm().unwrap_or(120.0),
                beats: track.beats,
                downbeats: track.downbeats,
                engine: models.name,
            });
        }
    }
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
    let source = match drums {
        Some(d) if rms(&d.samples) > 0.1 * rms(&mix.samples) => d,
        _ => mix,
    };
    let env = beats::onset_envelope(&source.at(beats::SAMPLE_RATE), beats::SAMPLE_RATE);
    let bpm = beats::estimate_tempo(&env);
    let frames = beats::track_beats(&env, bpm);
    let phase = beats::downbeat_phase(&env, &frames);
    Ok(Rhythm {
        beats: beats::frame_times(&env, &frames),
        downbeats: beats::regular_downbeats(frames.len(), phase),
        bpm,
        engine: "spectral flux + DP",
    })
}

/// Share of the whole job taken by the YouTube download.
const DOWNLOAD_SHARE: f32 = 0.15;

/// Downloads the audio of YouTube video `video_id` (into the app's audio
/// cache, reused next time) and transcribes it. `request.audio_path` is
/// replaced by the downloaded file.
pub fn run_youtube(
    ytdlp: &Path,
    video_id: &str,
    mut request: Request,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<Output, String> {
    let dir = youtube::audio_cache_dir().ok_or("sem pasta de dados para guardar o áudio")?;
    progress(Progress {
        stage: Stage::Downloading,
        overall: 0.0,
    });
    request.audio_path = youtube::download_audio(
        ytdlp,
        video_id,
        &dir,
        &mut |p| {
            progress(Progress {
                stage: Stage::Downloading,
                overall: DOWNLOAD_SHARE * p,
            })
        },
        cancel,
    )?;
    request.youtube_video_id = Some(video_id.to_string());
    run(
        &request,
        &mut |p| {
            progress(Progress {
                stage: p.stage,
                overall: DOWNLOAD_SHARE + (1.0 - DOWNLOAD_SHARE) * p.overall,
            })
        },
        cancel,
    )
}

/// Runs the whole pipeline and saves the result into `request.output_dir`.
pub fn run(
    request: &Request,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<Output, String> {
    if !request.guitar && !request.bass {
        return Err("escolha pelo menos um instrumento".into());
    }
    let separator = request.separate.then(separation::find_separator).flatten();
    let separating = separator.is_some();
    let report = |progress: &mut dyn FnMut(Progress), stage: Stage, fraction: f32| {
        let (base, share) = stage.span(separating);
        progress(Progress {
            stage,
            overall: base + share * fraction,
        });
    };

    report(progress, Stage::Decoding, 0.0);
    let sha256 = repository::file_sha256(&request.audio_path)
        .map_err(|e| format!("não foi possível ler o áudio: {e}"))?;
    let replaced = repository::find_by_audio_hash(&request.output_dir, &sha256).is_some();
    let mix = Mono::load(&request.audio_path)?;
    let duration = mix.duration();
    check(cancel)?;

    // Stems (or the mix, without separation).
    let work_dir = std::env::temp_dir().join(format!(
        "gva_transcribe_{}_{}",
        std::process::id(),
        &sha256[..12]
    ));
    let (guitar_stem, guitar_stereo, bass_stem, drums_audio, separation_name) = match &separator {
        Some(separator) => {
            let (base, share) = Stage::Separating.span(true);
            let stems = separation::separate(
                separator,
                &request.audio_path,
                &work_dir,
                &mut |p| {
                    progress(Progress {
                        stage: Stage::Separating,
                        overall: base + share * p,
                    })
                },
                cancel,
            );
            let stems = match stems {
                Ok(s) => s,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&work_dir);
                    return Err(e);
                }
            };
            let guitar = if request.guitar {
                Some(Mono::load(&stems.path("guitar"))?)
            } else {
                None
            };
            let guitar_stereo = if request.guitar {
                crate::backend::audio_file::decode_file_stereo(&stems.path("guitar")).ok()
            } else {
                None
            };
            let bass = if request.bass {
                Some(Mono::load(&stems.path("bass"))?)
            } else {
                None
            };
            let drums = Mono::load(&stems.path("drums")).ok();
            let _ = std::fs::remove_dir_all(&work_dir);
            (guitar, guitar_stereo, bass, drums, separator.name())
        }
        None => (None, None, None, None, String::new()),
    };
    let guitar_audio = request.guitar.then(|| guitar_stem.as_ref().unwrap_or(&mix));
    let bass_audio = request.bass.then(|| bass_stem.as_ref().unwrap_or(&mix));
    check(cancel)?;

    report(progress, Stage::Beats, 0.0);
    let rhythm = track_rhythm(&mix, drums_audio.as_ref(), cancel)?;
    check(cancel)?;

    let guitar_engine = guitar_engine(request.guitar_model);
    let bass_engine = Engine::BasicPitch(NoteParams::bass());
    let guitar_parts = match guitar_audio {
        Some(audio) => transcribe_guitars(
            audio,
            guitar_stereo.as_ref(),
            &guitar_engine,
            separating,
            progress,
            cancel,
        )?,
        None => Vec::new(),
    };
    let guitar_notes: Vec<NoteEstimate> = guitar_parts
        .iter()
        .flat_map(|p| p.notes.iter().copied())
        .collect();
    let mut bass_notes = match bass_audio {
        Some(audio) => {
            transcribe_stem(
                audio,
                &bass_engine,
                Stage::Bass,
                separating,
                progress,
                cancel,
            )?
            .0
        }
        None => Vec::new(),
    };
    if !separating && request.guitar && request.bass {
        // From the mix, the bass part is the lowest line: drop what the
        // guitar transcription already explains an octave or more up.
        bass_notes.retain(|n| n.pitch < 48);
    }
    if guitar_notes.is_empty() && bass_notes.is_empty() {
        return Err("nenhuma nota foi detectada no áudio".into());
    }

    report(progress, Stage::Saving, 0.0);
    let all: Vec<&NoteEstimate> = guitar_notes.iter().chain(&bass_notes).collect();
    let first = all.iter().map(|n| n.start).fold(f64::MAX, f64::min);
    let last = all.iter().map(|n| n.end).fold(0.0, f64::max);
    let (times, bar_beats) =
        beats::bar_grid(&rhythm.beats, &rhythm.downbeats, first, last, rhythm.bpm);
    let grid = arrange::BeatGrid { times, bar_beats };
    let quantize_params = arrange::QuantizeParams::default();

    let mut tracks = Vec::new();
    let mut infos = Vec::new();
    let stem_name = |stem: &str| {
        if separating {
            stem.to_string()
        } else {
            "mix".to_string()
        }
    };
    let several = guitar_parts.len() > 1;
    let fingering_model = match request.fingering {
        fingering_net::Fingering::Learned => fingering_net::find_model(),
        fingering_net::Fingering::Viterbi => None,
    };
    let mut fingering_engine = fingering_net::VITERBI_ENGINE_NAME;
    // (guitar part index, track name), for the tone stage.
    let mut part_tracks: Vec<(usize, String)> = Vec::new();
    for (k, part) in guitar_parts.iter().enumerate() {
        if part.notes.is_empty() {
            continue;
        }
        let mut spec = arrange::guitar_spec(&part.notes);
        if several {
            spec.name = match k {
                0 => "Guitarra base (IA)".into(),
                1 => "Guitarra solo (IA)".into(),
                k => format!("Guitarra {} (IA)", k + 1),
            };
        }
        let quantized = arrange::quantize(
            &part.notes,
            &part.techniques,
            &grid,
            spec.max_notes,
            &quantize_params,
        );
        let (frets, engine) =
            fret_guitar(fingering_model.as_deref(), &quantized.events, &spec, &grid);
        if engine == fingering_net::ENGINE_NAME {
            fingering_engine = engine;
        }
        let notes = frets.iter().map(Vec::len).sum();
        tracks.push(arrange::build_track(&spec, &quantized, &frets, &grid));
        let stem = match part.side {
            Some(side) => format!("{} ({})", stem_name("guitar"), side.label()),
            None => stem_name("guitar"),
        };
        part_tracks.push((k, spec.name.clone()));
        infos.push(TrackInfo {
            name: spec.name,
            stem,
            tuning_name: spec.tuning_name,
            notes,
        });
    }
    if request.bass && !bass_notes.is_empty() {
        let spec = arrange::bass_spec(&bass_notes);
        let quantized =
            arrange::quantize(&bass_notes, &[], &grid, spec.max_notes, &quantize_params);
        let frets = arrange::fret_events(&quantized.events, &spec);
        let notes = frets.iter().map(Vec::len).sum();
        tracks.push(arrange::build_track(&spec, &quantized, &frets, &grid));
        infos.push(TrackInfo {
            name: spec.name,
            stem: stem_name("bass"),
            tuning_name: spec.tuning_name,
            notes,
        });
    }
    let score = arrange::build_score(&request.title, &request.artist, &grid, tracks);

    let metadata = TabMetadata {
        version: METADATA_VERSION,
        title: request.title.clone(),
        artist: request.artist.clone(),
        origin: "transcription".into(),
        created_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        source: SourceAudio {
            file_name: request
                .audio_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: std::fs::canonicalize(&request.audio_path)
                .ok()
                .or_else(|| Some(request.audio_path.clone())),
            sha256,
            duration_secs: duration,
            youtube_id: request.youtube_video_id.clone(),
        },
        separation: separation_name,
        transcription: {
            let mut engines = Vec::new();
            if request.guitar {
                engines.push(guitar_engine.name());
            }
            if request.bass && !engines.contains(&bass_engine.name()) {
                engines.push(bass_engine.name());
            }
            let techniques = if request.guitar && techniques_enabled() {
                "; techniques: basic-pitch contour rules"
            } else {
                ""
            };
            let fingering = if request.guitar {
                format!("; fingering: {fingering_engine}")
            } else {
                String::new()
            };
            format!(
                "{}; beats: {}{techniques}{fingering}",
                engines.join(" + "),
                rhythm.engine
            )
        },
        app_version: env!("CARGO_PKG_VERSION").into(),
        tracks: infos,
        tempo_bpm: rhythm.bpm,
        bar_audio_secs: grid.bar_starts(),
        spotify_track_id: request.spotify_track_id.clone(),
    };
    let tab_path = repository::save(&request.output_dir, &score, &metadata)?;
    report(progress, Stage::Saving, 1.0);
    let mut tone_presets = Vec::new();
    if request.tone && separating {
        let count = part_tracks.len();
        for (i, (k, track)) in part_tracks.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelado".into());
            }
            report(progress, Stage::Tone, i as f32 / count.max(1) as f32);
            let part = &guitar_parts[*k];
            let audio = part.audio.at(crate::backend::offline::RENDER_RATE);
            let estimate = crate::tone::estimate_from_transcription(
                &audio,
                &part.notes,
                &crate::tone::Options::default(),
            );
            match crate::tone::save_preset(&estimate, &request.title, &request.artist, track) {
                Ok(path) => tone_presets.push((track.clone(), path)),
                Err(e) => log::warn!("tone preset for {track}: {e}"),
            }
        }
        report(progress, Stage::Tone, 1.0);
    }
    Ok(Output {
        tab_path,
        metadata,
        score,
        replaced,
        guitar_notes: guitar_parts
            .first()
            .map(|p| p.notes.clone())
            .unwrap_or_default(),
        guitar_techniques: guitar_parts
            .into_iter()
            .next()
            .map(|p| p.techniques)
            .unwrap_or_default(),
        tone_presets,
    })
}

/// Strings and frets of one guitar part: the learned tagger at `model`
/// when given and the tuning has six strings, else (or when it fails) the
/// Viterbi. Returns the fingering engine that ran.
fn fret_guitar(
    model: Option<&Path>,
    events: &[arrange::GridEvent],
    spec: &arrange::InstrumentSpec,
    grid: &arrange::BeatGrid,
) -> (Vec<Vec<arrange::FrettedNote>>, &'static str) {
    if let Some(path) = model.filter(|_| fingering_net::supports(spec)) {
        let ends: Vec<f64> = events.iter().map(|e| grid.tick_secs(e.end_tick)).collect();
        match fingering_net::fret_events(path, events, &ends, spec) {
            Ok(frets) => return (frets, fingering_net::ENGINE_NAME),
            Err(e) => log::warn!("learned fingering failed, using the Viterbi: {e}"),
        }
    }
    (
        arrange::fret_events(events, spec),
        fingering_net::VITERBI_ENGINE_NAME,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_artist_and_title_from_file_names() {
        let g = |s: &str| guess_artist_title(Path::new(s));
        assert_eq!(
            g("/m/Metallica - One.mp3"),
            ("Metallica".into(), "One".into())
        );
        assert_eq!(
            g("/m/03 - Legião Urbana - Tempo Perdido.flac"),
            ("Legião Urbana".into(), "Tempo Perdido".into())
        );
        assert_eq!(g("/m/song_name.wav"), (String::new(), "song name".into()));
        assert_eq!(g("/m/1979.mp3"), (String::new(), "1979".into()));
    }

    #[test]
    fn echoes_of_one_part_agree_and_different_parts_do_not() {
        let line = |offset: f64, base: u8| -> Vec<NoteEstimate> {
            (0..40)
                .map(|i| NoteEstimate {
                    start: offset + i as f64 * 0.25,
                    end: offset + i as f64 * 0.25 + 0.2,
                    pitch: base + (i * 5 % 12) as u8,
                    amplitude: 0.8,
                })
                .collect()
        };
        let dry = line(1.0, 52);
        assert!(agreement(&dry, &line(1.0, 52)) > 0.99);
        // A 330 ms echo (a stereo delay) is the same guitar.
        assert!(agreement(&dry, &line(1.33, 52)) > 0.9);
        // Another part: a different line on other pitches.
        assert!(agreement(&dry, &line(1.1, 60)) < SAME_PART_F1);
    }

    #[test]
    fn weak_simultaneous_detections_are_pruned() {
        let n = |pitch, amplitude| NoteEstimate {
            start: 1.0,
            end: 2.0,
            pitch,
            amplitude,
        };
        let kept = prune_weak(vec![n(40, 0.8), n(47, 0.5), n(64, 0.1)], 0.25, 1.0);
        assert_eq!(
            kept.iter().map(|k| k.pitch).collect::<Vec<_>>(),
            vec![40, 47]
        );
        // A quieter octave above a note is an overtone; a louder one stays.
        let octave = prune_weak(vec![n(40, 0.8), n(52, 0.5)], 0.25, 1.0);
        assert_eq!(octave.len(), 1);
        let doubled = prune_weak(vec![n(40, 0.5), n(52, 0.8)], 0.25, 1.0);
        assert_eq!(doubled.len(), 2);
    }

    #[test]
    fn stage_spans_cover_the_whole_job() {
        for separating in [false, true] {
            let (save, save_share) = Stage::Saving.span(separating);
            let (tone, tone_share) = Stage::Tone.span(separating);
            assert!((save + save_share - tone).abs() < 1e-6);
            assert!((tone + tone_share - 1.0).abs() < 1e-6);
            let (base, share) = Stage::Bass.span(separating);
            assert!((base + share - save).abs() < 1e-6);
            let (tb, ts) = Stage::Techniques.span(separating);
            assert!((tb + ts - base).abs() < 1e-6);
            let (gb, gs) = Stage::Guitar.span(separating);
            assert!((gb + gs - tb).abs() < 1e-6);
        }
    }

    /// End-to-end: a synthetic guitar riff written to WAV comes back as a
    /// tab with the right notes, saved in a repository folder.
    #[test]
    fn transcribes_a_wav_into_the_repository() {
        let dir = std::env::temp_dir().join(format!("gva_transcribe_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("Band - Riff.wav");
        // 100 BPM, quarter notes A2 C3 D3 E3 repeated over 4 bars, with a
        // low click on each beat so the beat tracker has something to lock to.
        let sr = 22_050u32;
        let beat = 0.6;
        let pitches = [45u8, 48, 50, 52];
        let total = (beat * 17.0 * sr as f64) as usize;
        let mut samples = vec![0f32; total];
        for b in 0..16 {
            let start = ((0.5 + b as f64 * beat) * sr as f64) as usize;
            let f = 440.0 * 2f64.powf((pitches[b % 4] as f64 - 69.0) / 12.0);
            for i in 0..((beat * sr as f64) as usize).min(total - start) {
                let t = i as f64 / sr as f64;
                let mut v = 0.0;
                for h in 1..=6 {
                    v += (std::f64::consts::TAU * f * h as f64 * t).sin() / h as f64;
                }
                let click = if t < 0.02 {
                    (std::f64::consts::TAU * 2500.0 * t).sin() * 0.3
                } else {
                    0.0
                };
                samples[start + i] += (0.25 * (-t * 3.0).exp() * v + click) as f32;
            }
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sr,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        for s in &samples {
            writer
                .write_sample((s.clamp(-1.0, 1.0) * 32000.0) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();

        let (artist, title) = guess_artist_title(&wav);
        let request = Request {
            audio_path: wav.clone(),
            title,
            artist,
            separate: false,
            guitar_model: Default::default(),
            fingering: Default::default(),
            tone: false,
            guitar: true,
            bass: false,
            output_dir: dir.join("repo"),
            spotify_track_id: Some("abc".into()),
            youtube_video_id: None,
        };
        let mut last = 0.0;
        let output = run(
            &request,
            &mut |p| {
                assert!(p.overall + 1e-6 >= last, "progress went back");
                last = p.overall;
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(output.tab_path.ends_with("Band - Riff.gp5"));
        let meta = repository::load_metadata(&output.tab_path).unwrap();
        assert_eq!(meta.spotify_track_id.as_deref(), Some("abc"));
        // 100 BPM, or its double: Beat This! final0 counts this short
        // synthetic riff in eighths (eval_guitarset also accepts a tempo
        // octave). The notes below are checked either way.
        assert!(
            [100.0, 200.0]
                .iter()
                .any(|bpm| (meta.tempo_bpm - bpm).abs() < 3.0 * bpm / 100.0),
            "tempo {}",
            meta.tempo_bpm
        );

        let score = crate::shared::tab::import::load_score_file(&output.tab_path).unwrap();
        let track = &score.tracks[0];
        let played: Vec<u8> = track
            .bars
            .iter()
            .flat_map(|b| b.voices[0].iter())
            .filter(|beat| beat.notes.iter().any(|n| !n.tie))
            .flat_map(|beat| {
                beat.notes
                    .iter()
                    .map(|n| track.tuning[n.string as usize - 1] + n.fret as u8)
            })
            .collect();
        let expected: Vec<u8> = (0..16).map(|b| pitches[b % 4]).collect();
        // Longest common subsequence: tolerates an extra or missing note.
        let mut lcs = vec![vec![0usize; played.len() + 1]; expected.len() + 1];
        for i in 0..expected.len() {
            for j in 0..played.len() {
                lcs[i + 1][j + 1] = if expected[i] == played[j] {
                    lcs[i][j] + 1
                } else {
                    lcs[i][j + 1].max(lcs[i + 1][j])
                };
            }
        }
        let hits = lcs[expected.len()][played.len()];
        assert!(
            hits >= 14 && played.len() <= 18,
            "expected {expected:?}, got {played:?}"
        );
        // The riff starts on bar 1 (no empty pickup bar before it).
        assert!(track.bars[0].voices[0].iter().any(|b| !b.notes.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
