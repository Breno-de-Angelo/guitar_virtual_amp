//! Automatic tablature from an audio file, fully offline with open-source
//! models:
//!
//! 1. decode the file (Symphonia) and hash it;
//! 2. separate guitar and bass with Demucs `htdemucs_6s`, when installed
//!    ([`separation`]); otherwise transcribe from the mix;
//! 3. track beats, downbeats and the meter (3/4 or 4/4) with Beat This!
//!    ([`beat_net`]), or with the spectral-flux tracker in 4/4 ([`beats`])
//!    when that model isn't installed;
//! 4. transcribe the guitar with the guitar-trained CRNN ([`guitar_crnn`],
//!    optional) or Basic Pitch, and the bass with Basic Pitch
//!    ([`basic_pitch`]);
//! 5. group chords, quantize each beat to 16ths or 8th triplets, pick
//!    strings/frets (Viterbi over hand positions with a prior learned from
//!    guitarists) and lay out bars ([`arrange`]);
//! 6. save a Guitar Pro 5 file plus sidecar into the tab repository
//!    (`shared::tab::repository`).
//!
//! The optional models come from `scripts/install_transcription_models.sh`.
//! Everything is blocking; run it on a worker thread. Limits: no techniques
//! (bends, slides...), one guitar track even when the song has several
//! guitars, no drums/vocals, one meter per song.

pub mod arrange;
pub mod basic_pitch;
pub mod beat_net;
pub mod beats;
#[cfg(test)]
mod eval;
pub mod guitar_crnn;
pub mod resample;
pub mod separation;
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
    /// Use Demucs when it's installed.
    pub separate: bool,
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
    Bass,
    Saving,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Downloading => "Baixando o áudio do YouTube",
            Stage::Decoding => "Lendo o áudio",
            Stage::Separating => "Separando instrumentos (Demucs)",
            Stage::Beats => "Detectando andamento e compassos",
            Stage::Guitar => "Transcrevendo a guitarra",
            Stage::Bass => "Transcrevendo o baixo",
            Stage::Saving => "Gravando a tab",
        }
    }

    /// Share of the whole job before this stage, and this stage's share.
    fn span(self, separating: bool) -> (f32, f32) {
        let sep = if separating { 0.35 } else { 0.0 };
        match self {
            Stage::Downloading | Stage::Decoding => (0.0, 0.05),
            Stage::Separating => (0.05, sep),
            Stage::Beats => (0.05 + sep, 0.05),
            Stage::Guitar => (0.10 + sep, (0.85 - sep) * 0.6),
            Stage::Bass => (0.10 + sep + (0.85 - sep) * 0.6, (0.85 - sep) * 0.4),
            Stage::Saving => (0.95, 0.05),
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
    /// Guitar notes as transcribed, before quantization (audio time).
    pub guitar_notes: Vec<NoteEstimate>,
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

/// Which optional models (`scripts/install_transcription_models.sh`) are
/// installed: `(guitar CRNN, Beat This!)`.
pub fn installed_models() -> (bool, bool) {
    (
        guitar_crnn::find_model().is_some(),
        beat_net::find_models().is_some(),
    )
}

/// Note transcription model for a stem.
#[derive(Clone, Debug, PartialEq)]
enum Engine {
    BasicPitch(NoteParams),
    /// Guitar CRNN at this ONNX path.
    GuitarCrnn(PathBuf, guitar_crnn::CrnnParams),
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
            Engine::GuitarCrnn(..) => guitar_crnn::ENGINE_NAME,
        }
    }
}

/// The guitar CRNN when installed (unless `GVA_GUITAR_ENGINE=basic-pitch`),
/// else Basic Pitch.
fn guitar_engine() -> Engine {
    let forced_basic = std::env::var("GVA_GUITAR_ENGINE").is_ok_and(|v| v == "basic-pitch");
    match guitar_crnn::find_model() {
        Some(path) if !forced_basic => Engine::GuitarCrnn(path, guitar_crnn::CrnnParams::guitar()),
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

fn transcribe_stem(
    audio: &Mono,
    engine: &Engine,
    stage: Stage,
    separating: bool,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<Vec<NoteEstimate>, String> {
    #[cfg(test)]
    let cached = eval::cached_notes(audio, engine);
    #[cfg(not(test))]
    let cached = None;
    let notes = match cached {
        Some(notes) => notes,
        None => {
            let notes =
                transcribe_stem_uncached(audio, engine, stage, separating, progress, cancel)?;
            #[cfg(test)]
            eval::cache_notes(audio, engine, &notes);
            notes
        }
    };
    let (ratio, overtone_ratio) = engine.pruning();
    Ok(prune_weak(notes, ratio, overtone_ratio))
}

fn transcribe_stem_uncached(
    audio: &Mono,
    engine: &Engine,
    stage: Stage,
    separating: bool,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<Vec<NoteEstimate>, String> {
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
            Ok(basic_pitch::extract_notes(&post, params))
        }
        Engine::GuitarCrnn(path, params) => {
            let post = guitar_crnn::posteriors(
                path,
                &audio.at(guitar_crnn::SAMPLE_RATE),
                &mut report,
                cancel,
            )?;
            Ok(guitar_crnn::extract_notes(&post, params))
        }
    }
}

/// Beats, meter and tempo of the song.
#[derive(Clone, Debug, PartialEq)]
struct Rhythm {
    /// Beat times in seconds.
    beats: Vec<f64>,
    beats_per_bar: usize,
    /// Index into `beats` of a downbeat.
    phase: usize,
    bpm: f64,
    engine: &'static str,
}

/// Beats from Beat This! on the mix when installed (it handles any
/// instrumentation and meter); otherwise the DSP tracker, on the drum stem
/// when there is one (kick and snare mark the beats far better than
/// strummed chords) or the mix, in 4/4.
fn track_rhythm(mix: &Mono, drums: Option<&Mono>, cancel: &AtomicBool) -> Result<Rhythm, String> {
    if let Some((front, net)) = beat_net::find_models() {
        let track = beat_net::track(&front, &net, &mix.at(beat_net::SAMPLE_RATE), cancel)?;
        if track.beats.len() >= 8 {
            return Ok(Rhythm {
                bpm: track.bpm().unwrap_or(120.0),
                beats: track.beats,
                beats_per_bar: track.beats_per_bar,
                phase: track.phase,
                engine: beat_net::ENGINE_NAME,
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
        beats_per_bar: beats::BEATS_PER_BAR,
        phase,
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
    let demucs = request.separate.then(separation::find_demucs).flatten();
    let separating = demucs.is_some();
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

    // Stems (or the mix, without Demucs).
    let work_dir = std::env::temp_dir().join(format!(
        "gva_transcribe_{}_{}",
        std::process::id(),
        &sha256[..12]
    ));
    let (guitar_stem, bass_stem, drums_audio, separation_name) = match &demucs {
        Some(demucs) => {
            let (base, share) = Stage::Separating.span(true);
            let stems = separation::separate(
                demucs,
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
            let bass = if request.bass {
                Some(Mono::load(&stems.path("bass"))?)
            } else {
                None
            };
            let drums = Mono::load(&stems.path("drums")).ok();
            let _ = std::fs::remove_dir_all(&work_dir);
            (guitar, bass, drums, format!("demucs {}", separation::MODEL))
        }
        None => (None, None, None, String::new()),
    };
    let guitar_audio = request.guitar.then(|| guitar_stem.as_ref().unwrap_or(&mix));
    let bass_audio = request.bass.then(|| bass_stem.as_ref().unwrap_or(&mix));
    check(cancel)?;

    report(progress, Stage::Beats, 0.0);
    let rhythm = track_rhythm(&mix, drums_audio.as_ref(), cancel)?;
    check(cancel)?;

    let guitar_engine = guitar_engine();
    let bass_engine = Engine::BasicPitch(NoteParams::bass());
    let guitar_notes = match guitar_audio {
        Some(audio) => transcribe_stem(
            audio,
            &guitar_engine,
            Stage::Guitar,
            separating,
            progress,
            cancel,
        )?,
        None => Vec::new(),
    };
    let mut bass_notes = match bass_audio {
        Some(audio) => transcribe_stem(
            audio,
            &bass_engine,
            Stage::Bass,
            separating,
            progress,
            cancel,
        )?,
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
    let grid = arrange::BeatGrid {
        times: beats::bar_grid(
            &rhythm.beats,
            rhythm.beats_per_bar,
            rhythm.phase,
            first,
            last,
            rhythm.bpm,
        ),
        beats_per_bar: rhythm.beats_per_bar,
    };
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
    if request.guitar && !guitar_notes.is_empty() {
        let spec = arrange::guitar_spec(&guitar_notes);
        let quantized = arrange::quantize(&guitar_notes, &grid, spec.max_notes, &quantize_params);
        let frets = arrange::fret_events(&quantized.events, &spec);
        let notes = frets.iter().map(Vec::len).sum();
        tracks.push(arrange::build_track(&spec, &quantized, &frets, &grid));
        infos.push(TrackInfo {
            name: spec.name,
            stem: stem_name("guitar"),
            tuning_name: spec.tuning_name,
            notes,
        });
    }
    if request.bass && !bass_notes.is_empty() {
        let spec = arrange::bass_spec(&bass_notes);
        let quantized = arrange::quantize(&bass_notes, &grid, spec.max_notes, &quantize_params);
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
            format!("{}; beats: {}", engines.join(" + "), rhythm.engine)
        },
        app_version: env!("CARGO_PKG_VERSION").into(),
        tracks: infos,
        tempo_bpm: rhythm.bpm,
        bar_audio_secs: grid.bar_starts(),
        spotify_track_id: request.spotify_track_id.clone(),
    };
    let tab_path = repository::save(&request.output_dir, &score, &metadata)?;
    report(progress, Stage::Saving, 1.0);
    Ok(Output {
        tab_path,
        metadata,
        score,
        replaced,
        guitar_notes,
    })
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
            let (base, share) = Stage::Bass.span(separating);
            assert!((base + share - 0.95).abs() < 1e-6);
            let (gb, gs) = Stage::Guitar.span(separating);
            assert!((gb + gs - base).abs() < 1e-6);
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
        assert!(
            (meta.tempo_bpm - 100.0).abs() < 3.0,
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
