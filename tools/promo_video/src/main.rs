//! Renders the promo video described by `storyboard.toml`.
//!
//! The video is one piece of music, scene after scene on the bar lines.
//! Every app scene is the real app (`AudioApp`) rendered offscreen with
//! egui_kittest, with recorded guitar notes going through its real audio
//! path (`OfflineOutput`): the guitar tone is the app's own pedals, and the
//! tuner, analyzer, meters and practice mode react to it. Bass and drums
//! come from a SoundFont through rustysynth, as in the tab player. See
//! README.md.
//!
//! Usage: `cargo run -- [STORYBOARD] [--only id,id] [--stills DIR] [--every SECS] [--no-video]`

mod band;
mod compose;
mod dsp;
mod guitar;
mod riff;
mod scene;
mod storyboard;
mod tone;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use eframe::egui::vec2;

use band::{BASS_CHANNEL, DRUM_CHANNEL, Msg};
use compose::{Compositor, FrameSpec};
use guitar::{Guitar, RATE};
use scene::{Ctx, SceneAudio, TAIL_SECS};
use storyboard::{Layout, Scene, Storyboard, expand, expand_home};
use tone::ToneSetup;

struct Args {
    storyboard: PathBuf,
    only: Vec<String>,
    stills: Option<PathBuf>,
    every: f32,
    video: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { storyboard: "storyboard.toml".into(), only: Vec::new(), stills: None, every: 1.0, video: true };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--only" => args.only = value()?.split(',').map(str::to_owned).collect(),
            "--stills" => args.stills = Some(value()?.into()),
            "--every" => args.every = value()?.parse().map_err(|_| "--every: seconds")?,
            "--no-video" => args.video = false,
            "-h" | "--help" => {
                println!("cargo run -- [STORYBOARD] [--only id,id] [--stills DIR] [--every SECS] [--no-video]");
                std::process::exit(0);
            }
            _ if a.starts_with('-') => return Err(format!("unknown option {a}")),
            _ => args.storyboard = a.into(),
        }
    }
    Ok(args)
}

/// Raw RGBA frames into ffmpeg, encoded as H.264.
struct Encoder {
    child: Child,
}

impl Encoder {
    fn new(path: &Path, width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let child = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{width}x{height}"), "-r", &fps.to_string(), "-i", "-"])
            .args(["-c:v", "libx264", "-preset", "medium", "-crf", "18", "-pix_fmt", "yuv420p"])
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("starting ffmpeg (is it installed?): {e}"))?;
        Ok(Self { child })
    }

    fn write(&mut self, frame: &image::RgbaImage) -> Result<(), String> {
        self.child.stdin.as_mut().unwrap().write_all(frame.as_raw()).map_err(|e| format!("ffmpeg: {e}"))
    }

    fn finish(mut self) -> Result<(), String> {
        drop(self.child.stdin.take());
        let status = self.child.wait().map_err(|e| e.to_string())?;
        if status.success() { Ok(()) } else { Err(format!("ffmpeg failed: {status}")) }
    }
}

fn rms(l: &[f32], r: &[f32]) -> f32 {
    let n = l.len().max(1) as f32;
    ((l.iter().map(|s| s * s).sum::<f32>() + r.iter().map(|s| s * s).sum::<f32>()) / (2.0 * n)).sqrt()
}

/// Soft limiter: transparent below -2 dB, then a -1 dBFS ceiling, so AAC
/// encoding doesn't clip between samples.
fn limit(s: f32) -> f32 {
    let knee = 0.8;
    let y = if s.abs() <= knee { s } else { s.signum() * (knee + (1.0 - knee) * ((s.abs() - knee) / (1.0 - knee)).tanh()) };
    y * 0.891
}

fn write_wav(path: &Path, left: &[f32], right: &[f32]) -> Result<(), String> {
    let spec = hound::WavSpec { channels: 2, sample_rate: RATE as u32, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for (l, r) in left.iter().zip(right) {
        w.write_sample((limit(*l) * 32767.0) as i16).map_err(|e| e.to_string())?;
        w.write_sample((limit(*r) * 32767.0) as i16).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

/// Evens out the level from bar to bar, like a fader ride that keeps a
/// quarter of the difference (a quiet dry bar stays a bit quieter), with
/// 30 ms ramps at the bar lines. Returns the last bar's gain.
fn ride(l: &mut [f32], r: &mut [f32], bar: usize) -> f32 {
    let whole = rms(l, r);
    if whole < 1e-5 {
        return 1.0;
    }
    let gains: Vec<f32> = (0..l.len().div_ceil(bar))
        .map(|i| {
            let (a, b) = (i * bar, ((i + 1) * bar).min(l.len()));
            let level = rms(&l[a..b], &r[a..b]);
            if level < 1e-4 { 1.0 } else { (whole / level).powf(0.75).clamp(0.5, 4.0) }
        })
        .collect();
    let ramp = (0.03 * RATE as f64) as usize;
    for (j, (sl, sr)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
        let i = j / bar;
        let into = j - i * bar;
        let g = if i > 0 && into < ramp {
            let k = into as f32 / ramp as f32;
            gains[i - 1] + (gains[i] - gains[i - 1]) * k
        } else {
            gains[i]
        };
        *sl *= g;
        *sr *= g;
    }
    *gains.last().unwrap_or(&1.0)
}

/// Bass and drum events of the scenes, laid end to end; each scene's are
/// delayed by `latency` seconds to line up with its guitar.
fn band_events(board: &Storyboard, scenes: &[&Scene], latency: &[f64]) -> Vec<(f64, Msg)> {
    let mut events = vec![(0.0, Msg::Volume { channel: BASS_CHANNEL, value: 110 }), (0.0, Msg::Volume { channel: DRUM_CHANNEL, value: 120 })];
    let mut scene_start = 0.0;
    for (s, lat) in scenes.iter().zip(latency) {
        let dur = board.duration(s) as f64;
        let start = scene_start + lat;
        if let Some(name) = &s.bass {
            let program = board.riffs[name].program.unwrap_or(33) as i32;
            events.push((start, Msg::Program { channel: BASS_CHANNEL, program }));
            for n in board.riff(name).played(dur) {
                let key = n.pitch as i32;
                let velocity = (n.velocity * 118.0) as i32;
                events.push((start + n.start, Msg::On { channel: BASS_CHANNEL, key, velocity }));
                // Released a hair early, as a player lifts the finger.
                let end = start + (n.end - 0.03).max(n.start + 0.05).min(dur);
                events.push((end, Msg::Off { channel: BASS_CHANNEL, key }));
            }
        }
        if let Some(name) = &s.drums {
            events.push((start, Msg::Program { channel: DRUM_CHANNEL, program: board.drums[name].kit as i32 }));
            for (t, key, velocity) in board.drum_pattern(name).hits(dur) {
                events.push((start + t, Msg::On { channel: DRUM_CHANNEL, key: key as i32, velocity: velocity as i32 }));
                events.push((start + t + 0.1, Msg::Off { channel: DRUM_CHANNEL, key: key as i32 }));
            }
        }
        scene_start += dur;
    }
    band::humanize(&mut events, 7);
    events
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let board = Storyboard::load(&args.storyboard)?;
    let base = args.storyboard.parent().unwrap_or(Path::new(".")).to_path_buf();
    let v = &board.video;
    let screen = vec2(v.width as f32, v.height as f32);
    let output = base.join(&v.output);
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    if let Some(dir) = &args.stills {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let scenes: Vec<&Scene> = board.scenes.iter().filter(|s| args.only.is_empty() || args.only.contains(&s.id)).collect();
    if scenes.is_empty() {
        return Err("no scene matches --only".into());
    }
    let poster = v.poster.as_deref().and_then(|p| p.split_once('@')).map(|(id, t)| (id.to_owned(), t.parse::<f32>().unwrap_or(0.0)));
    let scratch = scratch_dir();

    // Instruments.
    let guitar = match Guitar::load(&board.music.guitars) {
        Ok(g) if g.is_sampled() => g,
        Ok(_) => {
            eprintln!("warning: no guitar samples set, using the plucked-string guitar (see README.md)");
            Guitar::synthetic()
        }
        Err(e) => {
            eprintln!("warning: {e}\n  falling back to the plucked-string guitar (see README.md: guitar samples)");
            Guitar::synthetic()
        }
    };
    let soundfont_path = board.music.soundfont.as_deref().map(expand_home).filter(|p| p.exists());
    let soundfont = match &soundfont_path {
        Some(p) => Some(band::load_soundfont(p)?),
        None => {
            eprintln!("warning: no SoundFont found, the video has no bass or drums (see README.md)");
            None
        }
    };
    if let Some(p) = &soundfont_path {
        // So the tab player finds it too.
        let dir = scratch.join("data/guitar_virtual_amp/soundfonts");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let _ = std::fs::copy(p, dir.join(p.file_name().unwrap()));
    }
    let tone = ToneSetup {
        ir: if board.music.cabinet { Some(tone::write_cabinet_ir(&scratch.join("ir"))?) } else { None },
        bpm: board.music.sync_delays.then_some(board.music.bpm),
    };
    let ctx = Ctx { board: &board, guitar: &guitar, tone: &tone, scratch: &scratch, screen };

    let total_secs: f64 = scenes.iter().map(|s| board.duration(s) as f64).sum();
    let total = (total_secs * RATE as f64) as usize;
    let (mut band_l, mut band_r) = match &soundfont {
        Some(sf) => {
            let latency: Vec<f64> = scenes.iter().map(|s| scene::scene_latency(&ctx, s) as f64 / RATE as f64).collect();
            band::render(sf, band_events(&board, &scenes, &latency), total)?
        }
        None => (vec![0.0; total], vec![0.0; total]),
    };
    let band_level = rms(&band_l, &band_r);
    if band_level > 1e-5 {
        let g = 10f32.powf(board.music.band_db / 20.0) / band_level;
        band_l.iter_mut().chain(band_r.iter_mut()).for_each(|s| *s *= g);
    }

    let video_tmp = output.with_extension("video.mp4");
    let audio_tmp = output.with_extension("wav");
    let mut encoder = if args.video { Some(Encoder::new(&video_tmp, v.width, v.height, v.fps)?) } else { None };
    let mut compositor = Compositor::new(v.width, v.height);
    let (mut gtr_l, mut gtr_r) = (vec![0f32; total], vec![0f32; total]);
    let fps = v.fps as f32;
    let started = std::time::Instant::now();
    // Scenes that ended but whose pedals still ring: (audio, samples left).
    let mut tails: Vec<(SceneAudio, usize)> = Vec::new();
    let mut at = 0usize;

    for (index, s) in scenes.iter().enumerate() {
        let duration = board.duration(s);
        let (mut driver, mut audio) = if s.layout == Layout::Title {
            (None, scene::title_audio(&ctx, s)?)
        } else {
            let (d, a) = scene::app_scene(&ctx, s)?;
            (Some(d), a)
        };
        let frames = (duration * fps).round() as usize;
        let scene_start = at;
        let title = expand(&s.title);
        let subtitle = expand(&s.subtitle);
        let bullets: Vec<String> = s.bullets.iter().map(|b| expand(b)).collect();
        let mut next_still = 0.0f32;
        let (mut own_l, mut own_r) = (Vec::new(), Vec::new());
        for f in 0..frames {
            let t = f as f32 / fps;
            let n = (f + 1) * RATE / v.fps as usize - f * RATE / v.fps as usize;
            // The UI first: what it does this frame (a click, a key) is heard
            // from this frame's first sample.
            let app = match &mut driver {
                Some(d) => Some(d.frame(t, &s.actions)?),
                None => None,
            };
            let (l, r) = audio.render(&tone, n);
            own_l.extend(l);
            own_r.extend(r);
            for (tail, left) in &mut tails {
                let k = n.min(*left);
                let (l, r) = tail.render(&tone, n);
                for i in 0..k {
                    if at + i < total {
                        gtr_l[at + i] += l[i] * tail.gain;
                        gtr_r[at + i] += r[i] * tail.gain;
                    }
                }
                *left -= k;
            }
            tails.retain(|(_, left)| *left > 0);
            at += n;
            let visibility = (t / v.fade).min((duration - t) / v.fade).clamp(0.0, 1.0);
            let image = compositor.frame(FrameSpec {
                layout: s.layout,
                title: title.clone(),
                subtitle: subtitle.clone(),
                bullets: bullets.clone(),
                footnote: expand(&s.footnote),
                t,
                visibility,
                app,
                watermark: v.watermark.clone(),
            });
            if let Some(enc) = &mut encoder {
                enc.write(&image)?;
            }
            if let Some(dir) = &args.stills
                && t >= next_still
            {
                next_still += args.every;
                let path = dir.join(format!("{index:02}_{}_{t:05.2}.png", s.id));
                image.save(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
            if let Some((id, sec)) = &poster
                && *id == s.id
                && f == (sec * fps).round() as usize
            {
                let path = output.with_extension("png");
                image.save(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                println!("poster: {}", path.display());
            }
        }
        // Level the scene's guitars like a fader ride, then keep its pedals
        // ringing under the next scene.
        let bar = (board.bar_secs() * RATE as f64) as usize;
        let last_bar_gain = ride(&mut own_l, &mut own_r, bar);
        let level = rms(&own_l, &own_r);
        let gain = if level > 1e-5 { (10f32.powf(board.music.guitar_db / 20.0) / level).clamp(0.05, 12.0) } else { 1.0 };
        audio.gain = gain * last_bar_gain;
        for (i, (l, r)) in own_l.iter().zip(&own_r).enumerate() {
            if scene_start + i < total {
                gtr_l[scene_start + i] += l * gain;
                gtr_r[scene_start + i] += r * gain;
            }
        }
        drop(driver);
        tails.push((audio, (TAIL_SECS * RATE as f64) as usize));
        println!("{:>5.1}s  {} ({} frames)", started.elapsed().as_secs_f32(), s.id, frames);
    }
    let _ = std::fs::remove_dir_all(&scratch);

    // Mix, with a fade at the very end.
    let fade = (1.5 * RATE as f64) as usize;
    let mix = |g: &[f32], b: &[f32]| -> Vec<f32> {
        g.iter()
            .zip(b)
            .enumerate()
            .map(|(i, (g, b))| (g + b) * ((total - i) as f32 / fade as f32).min(1.0))
            .collect()
    };
    // The engineer's guitar bus: low cut under the bass, less boxiness,
    // a little presence (the recorded guitars are dark).
    for ch in [&mut gtr_l, &mut gtr_r] {
        dsp::Biquad::high_pass(100.0).run(ch);
        dsp::Biquad::peak(260.0, 1.0, -2.5).run(ch);
        dsp::Biquad::peak(3000.0, 0.8, 2.5).run(ch);
    }
    if let Ok(dir) = std::env::var("PROMO_STEMS") {
        // The guitars and the band apart, for checking the mix.
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        write_wav(&dir.join("guitars.wav"), &gtr_l, &gtr_r)?;
        write_wav(&dir.join("band.wav"), &band_l, &band_r)?;
    }
    let (left, right) = (mix(&gtr_l, &band_l), mix(&gtr_r, &band_r));
    write_wav(&audio_tmp, &left, &right)?;
    if let Some(enc) = encoder {
        enc.finish()?;
        let status = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(&video_tmp)
            .arg("-i")
            .arg(&audio_tmp)
            .args(["-c:v", "copy", "-c:a", "aac", "-b:a", "256k", "-shortest", "-movflags", "+faststart"])
            .arg(&output)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("ffmpeg mux failed: {status}"));
        }
        let _ = std::fs::remove_file(&video_tmp);
        let _ = std::fs::remove_file(&audio_tmp);
        println!("{} ({total_secs:.1} s)", output.display());
    } else {
        println!("{}", audio_tmp.display());
    }
    Ok(())
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("gva_promo_video_{}", std::process::id()))
}

fn main() {
    // The app reads and writes settings, presets and sessions under these:
    // point them at a scratch folder so your own are never shown or touched.
    // SAFETY: single-threaded here, before the app (and any thread) starts.
    let scratch = scratch_dir();
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", scratch.join("config"));
        std::env::set_var("XDG_DATA_HOME", scratch.join("data"));
    }
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
