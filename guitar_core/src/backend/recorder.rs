//! Records what the output callback produces into WAV files.
//!
//! The output callback `try_send`s one `f32` per frame into a bounded channel
//! while `AudioControls::record_source` is set (it never blocks: when the
//! channel is full the sample is dropped). A worker thread started by
//! [`Recorder::start`] drains the channel into a 16-bit mono WAV file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam::channel::{bounded, Receiver, RecvTimeoutError, Sender};

use crate::backend::controls::{AudioControls, RecordSource};
use crate::shared::paths;

/// Two seconds at 48 kHz: the worker only has to wake up a few times per
/// second to keep up.
pub const RECORD_CHANNEL_SAMPLES: usize = 96_000;

/// Creates the channel the output callback writes recorded samples into.
pub fn record_channel() -> (Sender<f32>, Receiver<f32>) {
    bounded(RECORD_CHANNEL_SAMPLES)
}

/// `<data dir>/guitar_virtual_amp/recordings`.
pub fn recordings_dir() -> Option<PathBuf> {
    paths::app_data_dir().map(|d| d.join("recordings"))
}

/// A finished recording.
#[derive(Clone, Debug, PartialEq)]
pub struct Recording {
    pub path: PathBuf,
    pub duration_secs: f64,
}

/// WAV files in `dir`, newest first.
pub fn list_recordings(dir: &Path) -> Vec<Recording> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<(std::time::SystemTime, Recording)> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")))
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            let reader = hound::WavReader::open(&path).ok()?;
            let spec = reader.spec();
            let frames = reader.duration() as f64;
            let duration_secs = frames / spec.sample_rate.max(1) as f64;
            Some((modified, Recording { path, duration_secs }))
        })
        .collect();
    found.sort_by_key(|f| std::cmp::Reverse(f.0));
    found.into_iter().map(|(_, r)| r).collect()
}

/// File name for a recording started now, e.g. `take-20261125-231502.wav`.
fn file_name_for_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("take-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}.wav")
}

/// UTC calendar date and time for a Unix timestamp (Howard Hinnant's
/// days-from-civil algorithm, inverted), to avoid a date-time dependency.
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32, (rem % 60) as u32)
}

struct ActiveRecording {
    path: PathBuf,
    started: Instant,
    source: RecordSource,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Result<u64, String>>,
}

/// UI-side handle: starts and stops recordings.
pub struct Recorder {
    rx: Receiver<f32>,
    controls: Arc<AudioControls>,
    active: Option<ActiveRecording>,
}

impl Recorder {
    pub fn new(rx: Receiver<f32>, controls: Arc<AudioControls>) -> Self {
        Self { rx, controls, active: None }
    }

    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    /// Seconds since the current recording started.
    pub fn elapsed_secs(&self) -> Option<f64> {
        self.active.as_ref().map(|a| a.started.elapsed().as_secs_f64())
    }

    pub fn source(&self) -> Option<RecordSource> {
        self.active.as_ref().map(|a| a.source)
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.active.as_ref().map(|a| a.path.as_path())
    }

    /// Starts recording `source` into a new file in `dir`.
    pub fn start(&mut self, dir: &Path, source: RecordSource, sample_rate: u32) -> Result<PathBuf, String> {
        if self.active.is_some() {
            return Err("Already recording".into());
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
        let path = dir.join(file_name_for_now());
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sample_rate.max(1),
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer =
            hound::WavWriter::create(&path, spec).map_err(|e| format!("Can't create {}: {e}", path.display()))?;
        // Samples left over from an earlier recording.
        while self.rx.try_recv().is_ok() {}
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let rx = self.rx.clone();
        let worker = std::thread::spawn(move || -> Result<u64, String> {
            let mut frames = 0u64;
            let mut write = |sample: f32, frames: &mut u64| -> Result<(), String> {
                let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                *frames += 1;
                writer.write_sample(value).map_err(|e| e.to_string())
            };
            loop {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(sample) => write(sample, &mut frames)?,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                if worker_stop.load(Ordering::Relaxed) {
                    break;
                }
            }
            for sample in rx.try_iter() {
                write(sample, &mut frames)?;
            }
            writer.finalize().map_err(|e| e.to_string())?;
            Ok(frames)
        });
        self.controls.set_record_source(Some(source));
        self.active = Some(ActiveRecording { path: path.clone(), started: Instant::now(), source, stop, worker });
        Ok(path)
    }

    /// Stops the current recording and returns the finished file.
    pub fn stop(&mut self) -> Option<Result<PathBuf, String>> {
        let active = self.active.take()?;
        self.controls.set_record_source(None);
        active.stop.store(true, Ordering::Relaxed);
        Some(match active.worker.join() {
            Ok(Ok(_frames)) => Ok(active.path),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("The recording thread crashed".into()),
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_unix_time_to_a_calendar_date() {
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
        // 2024-02-29 12:34:56 UTC.
        assert_eq!(civil_from_unix(1_709_210_096), (2024, 2, 29, 12, 34, 56));
    }

    #[test]
    fn records_samples_sent_while_active_into_a_wav() {
        let dir = std::env::temp_dir().join(format!("gva_recorder_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let controls = Arc::new(AudioControls::default());
        let (tx, rx) = record_channel();
        let _ = tx.send(0.9); // stale, from before the recording
        let mut recorder = Recorder::new(rx, controls.clone());
        let path = recorder.start(&dir, RecordSource::Guitar, 8000).unwrap();
        assert_eq!(controls.record_source(), Some(RecordSource::Guitar));
        for i in 0..800 {
            tx.send((i as f32 / 800.0) - 0.5).unwrap();
        }
        assert_eq!(recorder.stop().unwrap().unwrap(), path);
        assert_eq!(controls.record_source(), None);
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().sample_rate, 8000);
        assert_eq!(reader.duration(), 800);
        let listed = list_recordings(&dir);
        assert_eq!(listed.len(), 1);
        assert!((listed[0].duration_secs - 0.1).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
