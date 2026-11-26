//! Source separation, so each instrument is transcribed from its own stem
//! instead of the full mix. Two external separators, each in its own Python
//! environment in the app data dir, both using the GPU when PyTorch sees
//! one:
//!
//! - BS-RoFormer SW (6 stems: bass, drums, other, vocals, guitar, piano)
//!   through ZFTurbo's Music-Source-Separation-Training code, run by
//!   `scripts/separate_msst.py`; `scripts/install_bs_roformer.sh` sets it
//!   up. Its guitar stem transcribes better (note F50 0.81 against 0.74 for
//!   htdemucs_6s on band mixes), so it's preferred when installed. The
//!   checkpoint has no stated license: local use only, never bundled.
//! - Demucs `htdemucs_6s` (MIT; drums, bass, guitar, piano, vocals, other),
//!   from `scripts/install_demucs.sh`.
//!
//! `GVA_SEPARATOR=demucs|bs-roformer` forces one of them (no separation
//! when that one isn't installed).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::shared::paths::app_data_dir;
use crate::trf;

pub const MODEL: &str = "htdemucs_6s";

/// The app's tools folder (`<data dir>/guitar_virtual_amp/tools`).
fn tools_dir() -> Option<PathBuf> {
    app_data_dir().map(|d| d.join("tools"))
}

/// The `demucs` executable: `GVA_DEMUCS` if set, else the app's own
/// environment (`<data dir>/guitar_virtual_amp/tools/demucs-venv`), else
/// `demucs` on `PATH`.
pub fn find_demucs() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("GVA_DEMUCS") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let bundled = tools_dir().map(|d| {
        let venv = d.join("demucs-venv");
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

/// Sample rate BS-RoFormer SW was trained at; its input is resampled to it.
pub const ROFORMER_RATE: u32 = 44_100;
const ROFORMER_CHECKPOINT: &str = "BS-Rofo-SW-Fixed.ckpt";
const ROFORMER_CONFIG: &str = "BS-Rofo-SW-Fixed.yaml";

/// The files of the BS-RoFormer install (`scripts/install_bs_roformer.sh`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roformer {
    pub python: PathBuf,
    /// `scripts/separate_msst.py`, copied into the environment.
    pub runner: PathBuf,
    /// The Music-Source-Separation-Training source folder.
    pub msst: PathBuf,
    pub config: PathBuf,
    pub checkpoint: PathBuf,
}

impl Roformer {
    /// The install under `tools` (`msst-venv/` and
    /// `models/bs_roformer_sw/`), when every file is there.
    pub fn in_tools(tools: &Path) -> Option<Self> {
        let venv = tools.join("msst-venv");
        let models = tools.join("models").join("bs_roformer_sw");
        let python = if cfg!(windows) {
            venv.join("Scripts").join("python.exe")
        } else {
            venv.join("bin").join("python")
        };
        let found = Self {
            python,
            runner: venv.join("separate_msst.py"),
            msst: venv.join("src").join("msst"),
            config: models.join(ROFORMER_CONFIG),
            checkpoint: models.join(ROFORMER_CHECKPOINT),
        };
        let complete = found.python.is_file()
            && found.runner.is_file()
            && found.msst.join("utils").join("model_utils.py").is_file()
            && found.config.is_file()
            && found.checkpoint.is_file();
        complete.then_some(found)
    }
}

/// BS-RoFormer SW, when installed in the app's tools folder.
pub fn find_bs_roformer() -> Option<Roformer> {
    Roformer::in_tools(&tools_dir()?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Separator {
    /// The `demucs` executable.
    Demucs(PathBuf),
    BsRoformer(Roformer),
}

impl Separator {
    /// Recorded in the tab's metadata.
    pub fn name(&self) -> String {
        match self {
            Separator::Demucs(_) => format!("demucs {MODEL}"),
            Separator::BsRoformer(_) => "bs-roformer sw (msst)".into(),
        }
    }

    /// Short name for the UI and messages.
    pub fn label(&self) -> &'static str {
        match self {
            Separator::Demucs(_) => "Demucs",
            Separator::BsRoformer(_) => "BS-RoFormer",
        }
    }

    /// Folder under the work dir that gets the stems.
    fn stem_dir(&self) -> &'static str {
        match self {
            Separator::Demucs(_) => MODEL,
            Separator::BsRoformer(_) => "bs_roformer_sw",
        }
    }
}

/// `GVA_SEPARATOR` value forcing a separator.
const FORCE_DEMUCS: &str = "demucs";
const FORCE_ROFORMER: &str = "bs-roformer";

/// BS-RoFormer when installed, else Demucs; `forced` (the
/// `GVA_SEPARATOR` value) picks one, and then only that one. Other values
/// are ignored.
fn choose(
    forced: Option<&str>,
    roformer: Option<Roformer>,
    demucs: Option<PathBuf>,
) -> Option<Separator> {
    let roformer = roformer.map(Separator::BsRoformer);
    let demucs = demucs.map(Separator::Demucs);
    match forced.map(str::trim) {
        Some(FORCE_DEMUCS) => demucs,
        Some(FORCE_ROFORMER) => roformer,
        _ => roformer.or(demucs),
    }
}

/// The separator to use (see [`choose`]).
pub fn find_separator() -> Option<Separator> {
    let forced = std::env::var("GVA_SEPARATOR").ok();
    choose(forced.as_deref(), find_bs_roformer(), find_demucs())
}

/// Stem files written by a separator.
#[derive(Clone, Debug)]
pub struct Stems {
    pub dir: PathBuf,
}

impl Stems {
    pub fn path(&self, stem: &str) -> PathBuf {
        self.dir.join(format!("{stem}.wav"))
    }
}

/// Last percentage in a tqdm progress output (`" 45%|####  | ..."`).
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

/// Separates `input` into stems under `work_dir`. Blocks until the
/// separator exits; `cancel` kills it. When the GPU runs out of memory
/// (another program holds it), runs again on the CPU: several times
/// slower, but it finishes.
pub fn separate(
    separator: &Separator,
    input: &Path,
    work_dir: &Path,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Stems, String> {
    let label = separator.label();
    std::fs::create_dir_all(work_dir)
        .map_err(|e| trf!("couldn't create {}: {e}", "não foi possível criar {}: {e}", work_dir.display()))?;
    let stems = Stems {
        dir: work_dir.join(separator.stem_dir()),
    };
    // BS-RoFormer reads a WAV at its rate: decode (any format the app
    // reads) and resample here.
    let input = match separator {
        Separator::Demucs(_) => input.to_path_buf(),
        Separator::BsRoformer(_) => {
            let wav = work_dir.join("input_44k.wav");
            write_stereo_wav(input, &wav, ROFORMER_RATE)?;
            wav
        }
    };
    check_cancel(cancel)?;
    // Progress never goes back, even when the CPU run starts over.
    let mut shown = 0.0f32;
    let mut report = |p: f32| {
        if p > shown {
            shown = p;
            progress(p);
        }
    };
    let command = |device| separator_command(separator, &input, work_dir, &stems.dir, device);
    match run_tool(command(None), label, &mut report, cancel) {
        Err(log) if is_gpu_out_of_memory(&log) => {
            log::warn!("{label}: GPU out of memory, separating on the CPU");
            run_tool(command(Some("cpu")), label, &mut |p| report(p), cancel)
                .map_err(|log| failure(label, &log))?
        }
        Err(log) if log == CANCELLED => return Err(CANCELLED.into()),
        Err(log) => return Err(failure(label, &log)),
        Ok(()) => {}
    }
    for stem in ["guitar", "bass", "drums"] {
        if !stems.path(stem).is_file() {
            return Err(trf!(
                "{label} didn't produce {}",
                "o {label} não gerou {}",
                stems.path(stem).display()
            ));
        }
    }
    progress(1.0);
    Ok(stems)
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}

/// The command line of one separator run (`device` overrides its choice of
/// GPU or CPU).
fn separator_command(
    separator: &Separator,
    input: &Path,
    work_dir: &Path,
    stem_dir: &Path,
    device: Option<&str>,
) -> Command {
    match separator {
        Separator::Demucs(demucs) => {
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
            command.arg(input);
            command
        }
        Separator::BsRoformer(r) => {
            let mut command = Command::new(&r.python);
            command
                .arg(&r.runner)
                .arg("--msst")
                .arg(&r.msst)
                .arg("--config")
                .arg(&r.config)
                .arg("--checkpoint")
                .arg(&r.checkpoint);
            if let Some(device) = device {
                command.arg("--device").arg(device);
            }
            command.arg(input).arg(stem_dir);
            command
        }
    }
}

/// Decodes `input` and writes it as a stereo 32-bit float WAV at `rate`.
fn write_stereo_wav(input: &Path, output: &Path, rate: u32) -> Result<(), String> {
    let audio = crate::backend::audio_file::decode_file_stereo(input)?;
    let left = super::resample::resample(&audio.left, audio.sample_rate, rate);
    let right = super::resample::resample(&audio.right, audio.sample_rate, rate);
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let error = |e: hound::Error| trf!("couldn't write {}: {e}", "não foi possível gravar {}: {e}", output.display());
    let mut writer = hound::WavWriter::create(output, spec).map_err(error)?;
    for (l, r) in left.iter().zip(&right) {
        writer.write_sample(*l).map_err(error)?;
        writer.write_sample(*r).map_err(error)?;
    }
    writer.finalize().map_err(error)
}

const CANCELLED: &str = "cancelado";

fn is_gpu_out_of_memory(log: &str) -> bool {
    log.contains("CUDA out of memory") || log.contains("OutOfMemoryError")
}

/// The last lines of the separator's output, as an error.
fn failure(label: &str, log: &str) -> String {
    let tail: Vec<&str> = log.lines().rev().take(6).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    trf!("{label} failed:\n{}", "o {label} falhou:\n{}", tail.join("\n"))
}

/// Runs a separator to the end, reading tqdm progress from its stderr. On
/// failure returns its output, or [`CANCELLED`].
fn run_tool(
    mut command: Command,
    label: &str,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            trf!(
                "couldn't start {label} ({}): {e}",
                "não foi possível iniciar o {label} ({}): {e}",
                command.get_program().to_string_lossy()
            )
        })?;
    // Read stderr on a thread (progress + error text) so the pipe never fills.
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| trf!("no stderr from {label}", "sem stderr do {label}"))?;
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
            Err(e) => return Err(trf!("error waiting for {label}: {e}", "erro esperando o {label}: {e}")),
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
        // MSST's demix bar has a description before the percentage.
        let msst = "\rProcessing audio chunks:  45%|####5     | 4.6M/10.2M [00:20<00:24]";
        assert_eq!(parse_progress(msst), Some(0.45));
    }

    #[test]
    fn recognizes_gpu_out_of_memory() {
        let log = "torch.OutOfMemoryError: CUDA out of memory. Tried to allocate 20.00 MiB.";
        assert!(is_gpu_out_of_memory(log));
        assert!(!is_gpu_out_of_memory("RuntimeError: file not found"));
        assert!(failure("Demucs", "a\nb\nc").ends_with("a\nb\nc"));
    }

    #[test]
    fn missing_executable_is_an_error() {
        let result = separate(
            &Separator::Demucs(PathBuf::from("/nonexistent/demucs")),
            Path::new("/nonexistent/song.mp3"),
            &std::env::temp_dir().join("gva_sep_test"),
            &mut |_| {},
            &AtomicBool::new(false),
        );
        assert!(result.unwrap_err().contains("Demucs"));
    }

    fn roformer_at(tools: &Path) -> Roformer {
        Roformer {
            python: tools.join("msst-venv/bin/python"),
            runner: tools.join("msst-venv/separate_msst.py"),
            msst: tools.join("msst-venv/src/msst"),
            config: tools.join("models/bs_roformer_sw").join(ROFORMER_CONFIG),
            checkpoint: tools
                .join("models/bs_roformer_sw")
                .join(ROFORMER_CHECKPOINT),
        }
    }

    #[cfg(unix)]
    #[test]
    fn finds_bs_roformer_only_when_every_file_is_installed() {
        let tools = std::env::temp_dir().join(format!("gva_msst_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tools);
        let expected = roformer_at(&tools);
        let files = [
            expected.python.clone(),
            expected.runner.clone(),
            expected.msst.join("utils/model_utils.py"),
            expected.config.clone(),
            expected.checkpoint.clone(),
        ];
        for (k, file) in files.iter().enumerate() {
            assert_eq!(Roformer::in_tools(&tools), None, "found with {k} files");
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"").unwrap();
        }
        assert_eq!(Roformer::in_tools(&tools), Some(expected));
        let _ = std::fs::remove_dir_all(&tools);
    }

    #[test]
    fn prefers_bs_roformer_unless_forced() {
        let roformer = roformer_at(Path::new("/t"));
        let demucs = PathBuf::from("/t/demucs");
        let both = |forced| choose(forced, Some(roformer.clone()), Some(demucs.clone()));
        assert_eq!(both(None), Some(Separator::BsRoformer(roformer.clone())));
        assert_eq!(
            both(Some("demucs")),
            Some(Separator::Demucs(demucs.clone()))
        );
        assert_eq!(
            both(Some("bs-roformer")),
            Some(Separator::BsRoformer(roformer.clone()))
        );
        // Unknown values are ignored.
        assert_eq!(
            both(Some("x")),
            Some(Separator::BsRoformer(roformer.clone()))
        );
        // Without BS-RoFormer, Demucs; a forced separator that isn't
        // installed means no separation.
        assert_eq!(
            choose(None, None, Some(demucs.clone())),
            Some(Separator::Demucs(demucs.clone()))
        );
        assert_eq!(choose(Some("bs-roformer"), None, Some(demucs)), None);
        assert_eq!(choose(Some("demucs"), Some(roformer), None), None);
        assert_eq!(choose(None, None, None), None);
    }

    #[test]
    fn separators_have_distinct_names_and_stem_folders() {
        let d = Separator::Demucs(PathBuf::from("/t/demucs"));
        let r = Separator::BsRoformer(roformer_at(Path::new("/t")));
        assert_eq!(d.name(), "demucs htdemucs_6s");
        assert_eq!(r.name(), "bs-roformer sw (msst)");
        assert_ne!(d.stem_dir(), r.stem_dir());
        assert_eq!((d.label(), r.label()), ("Demucs", "BS-RoFormer"));
    }

    #[test]
    fn roformer_command_passes_the_install_paths() {
        let r = roformer_at(Path::new("/t"));
        let command = separator_command(
            &Separator::BsRoformer(r.clone()),
            Path::new("/w/in.wav"),
            Path::new("/w"),
            Path::new("/w/bs_roformer_sw"),
            Some("cpu"),
        );
        assert_eq!(command.get_program(), r.python.as_os_str());
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], r.runner.to_string_lossy());
        assert!(args.windows(2).any(|w| w == ["--device", "cpu"]));
        assert_eq!(&args[args.len() - 2..], ["/w/in.wav", "/w/bs_roformer_sw"]);
    }
}
