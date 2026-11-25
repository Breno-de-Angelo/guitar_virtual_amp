//! Source separation with Demucs (`htdemucs_6s`: drums, bass, guitar,
//! piano, vocals, other), so each instrument is transcribed from its own
//! stem instead of the full mix. Demucs (MIT) runs as an external program
//! from its own Python environment; `scripts/install_demucs.sh` sets one up
//! in the app data dir. Uses the GPU when PyTorch sees one.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::shared::paths::app_data_dir;

pub const MODEL: &str = "htdemucs_6s";

/// The `demucs` executable: `GVA_DEMUCS` if set, else the app's own
/// environment (`<data dir>/guitar_virtual_amp/tools/demucs-venv`), else
/// `demucs` on `PATH`.
pub fn find_demucs() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("GVA_DEMUCS") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let bundled = app_data_dir().map(|d| {
        let venv = d.join("tools").join("demucs-venv");
        if cfg!(windows) {
            venv.join("Scripts").join("demucs.exe")
        } else {
            venv.join("bin").join("demucs")
        }
    });
    if let Some(path) = bundled.filter(|p| p.is_file()) {
        return Some(path);
    }
    let name = if cfg!(windows) {
        "demucs.exe"
    } else {
        "demucs"
    };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Stem files written by Demucs.
#[derive(Clone, Debug)]
pub struct Stems {
    pub dir: PathBuf,
}

impl Stems {
    pub fn path(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.wav"))
    }
}

/// Last percentage in Demucs' tqdm output (`" 45%|####  | ..."`).
pub(crate) fn parse_progress(text: &str) -> Option<f32> {
    text.rsplit(['\r', '\n'])
        .filter_map(|chunk| {
            let percent = chunk.find("%|")?;
            let digits: String = chunk[..percent]
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            let value: f32 = digits.chars().rev().collect::<String>().parse().ok()?;
            Some(value / 100.0)
        })
        .next()
}

/// Separates `input` into stems under `work_dir`. Blocks until Demucs
/// exits; `cancel` kills it. When the GPU runs out of memory (another
/// program holds it), runs again on the CPU: several times slower, but it
/// finishes.
pub fn separate(
    demucs: &Path,
    input: &Path,
    work_dir: &Path,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Stems, String> {
    std::fs::create_dir_all(work_dir)
        .map_err(|e| format!("não foi possível criar {}: {e}", work_dir.display()))?;
    // Progress never goes back, even when the CPU run starts over.
    let mut shown = 0.0f32;
    let mut report = |p: f32| {
        if p > shown {
            shown = p;
            progress(p);
        }
    };
    match run_demucs(demucs, input, work_dir, None, &mut report, cancel) {
        Err(log) if is_gpu_out_of_memory(&log) => run_demucs(
            demucs,
            input,
            work_dir,
            Some("cpu"),
            &mut |p| report(p),
            cancel,
        )
        .map_err(|log| failure(&log))?,
        Err(log) if log == CANCELLED => return Err(CANCELLED.into()),
        Err(log) => return Err(failure(&log)),
        Ok(()) => {}
    }
    let stems = Stems {
        dir: work_dir.join(MODEL),
    };
    for stem in ["guitar", "bass", "drums"] {
        if !stems.path(stem).is_file() {
            return Err(format!("o Demucs não gerou {}", stems.path(stem).display()));
        }
    }
    progress(1.0);
    Ok(stems)
}

const CANCELLED: &str = "cancelado";

fn is_gpu_out_of_memory(log: &str) -> bool {
    log.contains("CUDA out of memory") || log.contains("OutOfMemoryError")
}

/// The last lines of Demucs' output, as an error.
fn failure(log: &str) -> String {
    let tail: Vec<&str> = log.lines().rev().take(6).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    format!("o Demucs falhou:\n{}", tail.join("\n"))
}

/// One Demucs run (`device` overrides its choice of GPU or CPU). On
/// failure returns Demucs' output, or [`CANCELLED`].
fn run_demucs(
    demucs: &Path,
    input: &Path,
    work_dir: &Path,
    device: Option<&str>,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut command = Command::new(demucs);
    command
        .arg("-n")
        .arg(MODEL)
        .arg("-o")
        .arg(work_dir)
        .arg("--filename")
        .arg("{stem}.{ext}");
    if let Some(device) = device {
        command.arg("-d").arg(device);
    }
    let mut child = command
        .arg(input)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "não foi possível iniciar o Demucs ({}): {e}",
                demucs.display()
            )
        })?;
    // Read stderr on a thread (progress + error text) so the pipe never fills.
    let mut stderr = child.stderr.take().ok_or("sem stderr do Demucs")?;
    let (tx, rx) = crossbeam::channel::unbounded::<String>();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
        }
    });
    let mut log = String::new();
    let status = loop {
        for chunk in rx.try_iter() {
            log.push_str(&chunk);
            if let Some(p) = parse_progress(&chunk) {
                progress(p);
            }
        }
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CANCELLED.into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(format!("erro esperando o Demucs: {e}")),
        }
    };
    let _ = reader.join();
    log.extend(rx.try_iter());
    if status.success() {
        Ok(())
    } else {
        Err(format!("{log}\n({status})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tqdm_progress() {
        let out =
            " 17%|#####     | 5.85/35.1 [00:01<00:06]\r 33%|##########| 11.7/35.1 [00:01<00:02]";
        assert_eq!(parse_progress(out), Some(0.33));
        assert_eq!(parse_progress("Separating track x.wav\n"), None);
        assert_eq!(parse_progress("100%|##########| 35.1/35.1"), Some(1.0));
    }

    #[test]
    fn recognizes_gpu_out_of_memory() {
        let log = "torch.OutOfMemoryError: CUDA out of memory. Tried to allocate 20.00 MiB.";
        assert!(is_gpu_out_of_memory(log));
        assert!(!is_gpu_out_of_memory("RuntimeError: file not found"));
        assert!(failure("a\nb\nc").ends_with("a\nb\nc"));
    }

    #[test]
    fn missing_executable_is_an_error() {
        let result = separate(
            Path::new("/nonexistent/demucs"),
            Path::new("/nonexistent/song.mp3"),
            &std::env::temp_dir().join("gva_sep_test"),
            &mut |_| {},
            &AtomicBool::new(false),
        );
        assert!(result.unwrap_err().contains("Demucs"));
    }
}
