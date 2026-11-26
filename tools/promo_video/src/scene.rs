//! Runs one scene: the real app (offscreen) fed with the scene's guitar
//! through `OfflineOutput`, the scripted mouse, keys and zoom, and the
//! scene's other guitars (double take, backing parts) through their chains.

use std::path::{Path, PathBuf};

use crossbeam::channel::{Receiver, Sender, bounded};
use eframe::egui::{self, Pos2, Rect, Vec2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use guitar_core::backend::capture::{AudioSetup, OfflineOutput};
use guitar_core::backend::offline::factory_preset;
use guitar_core::backend::pedals::pedal::PedalChain;
use guitar_core::frontend::gui::{AudioApp, Page, VISUALIZER_CHANNEL_SAMPLES};
use guitar_core::shared::pedals::PedalInstance;
use guitar_core::shared::preset::Preset;
use guitar_core::shared::settings::{AppSettings, PresetRef, ThemePref};

use crate::compose::{AppView, app_rect};
use crate::guitar::{Guitar, RATE};
use crate::riff;
use crate::storyboard::{Action, Layout, Scene, Storyboard, Theme};
use crate::tone::ToneSetup;

/// The cursor takes this long to reach a widget before clicking it.
const GLIDE_SECS: f32 = 0.7;
/// Clicks act on release: the button goes down this long before `at`.
const PRESS_SECS: f32 = 0.08;
/// A scene's pedals keep ringing (delay, reverb) this long into the next.
pub const TAIL_SECS: f64 = 3.0;
/// Notes still sounding at the cut fade out over this long.
const CUT_FADE_SECS: f64 = 0.05;
const PHONE: Vec2 = vec2(390.0, 844.0);
const DESKTOP: Vec2 = vec2(1280.0, 760.0);

pub struct Ctx<'a> {
    pub board: &'a Storyboard,
    pub guitar: &'a Guitar,
    pub tone: &'a ToneSetup,
    pub scratch: &'a Path,
    pub screen: Vec2,
}

/// Constant-power pan, -1 (left) .. 1 (right): (left gain, right gain).
fn pan_gains(pan: f32) -> (f32, f32) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (a.cos(), a.sin())
}

/// A take of `riff` for the scene (plus its tail), as interface samples.
fn di(ctx: &Ctx, scene: &Scene, riff_name: &str, take: usize, seed: u64) -> Vec<i16> {
    let dur = ctx.board.duration(scene) as f64;
    let len = ((dur + TAIL_SECS) * RATE as f64) as usize;
    let r = ctx.board.riff(riff_name);
    let notes = r.played(dur);
    to_interface(ctx.guitar.render(take, &notes, 0.0, len, seed), dur)
}

/// f32 DI -> i16, with the notes still ringing at the cut faded out.
fn to_interface(mut wave: Vec<f32>, dur: f64) -> Vec<i16> {
    let cut = (dur * RATE as f64) as usize;
    let fade = (CUT_FADE_SECS * RATE as f64) as usize;
    for (i, s) in wave.iter_mut().enumerate().skip(cut) {
        *s *= 1.0 - ((i - cut) as f32 / fade as f32).min(1.0);
    }
    wave.iter().map(|s| (s * 32767.0).clamp(-32767.0, 32767.0) as i16).collect()
}

fn main_di(ctx: &Ctx, scene: &Scene) -> Result<Vec<i16>, String> {
    let dur = ctx.board.duration(scene) as f64;
    let len = ((dur + TAIL_SECS) * RATE as f64) as usize;
    let Some(g) = &scene.guitar else { return Ok(vec![0; len]) };
    let seed = hash(&scene.id);
    if let Some(name) = &g.riff {
        let wave = di(ctx, scene, name, g.take, seed);
        if let Ok(dir) = std::env::var("PROMO_STEMS") {
            // The dry guitar going into the app, for checking notes and timing.
            let spec = hound::WavSpec { channels: 1, sample_rate: RATE as u32, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(mut w) = hound::WavWriter::create(Path::new(&dir).join(format!("di_{}.wav", scene.id)), spec) {
                wave.iter().for_each(|s| drop(w.write_sample(*s)));
                let _ = w.finalize();
            }
        }
        return Ok(wave);
    }
    if let Some(pos) = &g.tune {
        let hit = riff::parse_position(pos, 6)?;
        let bar = ctx.board.bar_secs();
        let wave = ctx.guitar.tuning(g.take, hit.string, hit.fret, (g.cents[0], g.cents[1]), bar * 0.8, bar, len);
        return Ok(to_interface(wave, dur));
    }
    Ok(vec![0; len])
}

fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn process(chain: &mut PedalChain, input: &[i16]) -> Vec<f32> {
    let mut block = input.to_vec();
    chain.process_block(&mut block);
    block.iter().map(|s| *s as f32 / 32768.0).collect()
}

/// Every guitar of a scene, rendered frame by frame into a stereo bus.
pub struct SceneAudio {
    output: OfflineOutput,
    input_tx: Sender<i16>,
    /// Keeps the analysis channel open when no app reads it.
    _analysis: Option<Receiver<i16>>,
    /// Chains the app sends, relayed to `output` (and the double take)
    /// after `ToneSetup::prepare`.
    relay: Option<(Receiver<Vec<PedalInstance>>, Sender<Vec<PedalInstance>>)>,
    main: Vec<i16>,
    main_pan: f32,
    double: Option<(PedalChain, Vec<i16>, f32)>,
    backing: Vec<(PedalChain, Vec<i16>, f32, f32)>,
    pos: usize,
    /// Leveling gain, set when the scene ends (applies to its tail).
    pub gain: f32,
}

impl SceneAudio {
    /// The next `frames` samples, (left, right).
    pub fn render(&mut self, tone: &ToneSetup, frames: usize) -> (Vec<f32>, Vec<f32>) {
        if let Some((from_app, to_output)) = &self.relay {
            for chain in from_app.try_iter() {
                let chain = tone.prepare(chain);
                if let Some((mirror, _, _)) = &mut self.double {
                    mirror.apply_description(chain.clone());
                }
                let _ = to_output.try_send(chain);
            }
        }
        let range = self.pos..self.pos + frames;
        let slice = |v: &[i16]| -> Vec<i16> { range.clone().map(|i| v.get(i).copied().unwrap_or(0)).collect() };
        for s in slice(&self.main) {
            let _ = self.input_tx.try_send(s);
        }
        let main = self.output.render(frames).to_vec();
        let (gl, gr) = pan_gains(self.main_pan);
        let mut left: Vec<f32> = main.iter().map(|s| s * gl).collect();
        let mut right: Vec<f32> = main.iter().map(|s| s * gr).collect();
        let mut add = |wave: &[f32], pan: f32, gain: f32| {
            let (l, r) = pan_gains(pan);
            for (i, s) in wave.iter().enumerate() {
                left[i] += s * l * gain;
                right[i] += s * r * gain;
            }
        };
        if let Some((chain, input, pan)) = &mut self.double {
            let wave = process(chain, &slice(input));
            add(&wave, *pan, 1.0);
        }
        for (chain, input, pan, gain) in &mut self.backing {
            let wave = process(chain, &slice(input));
            add(&wave, *pan, *gain);
        }
        self.pos += frames;
        (left, right)
    }
}

/// Samples a chain delays the guitar: the Cabinet convolves in blocks of
/// 1024 (see `backend::pedals::cabinet`).
fn chain_latency(pedals: &[PedalInstance]) -> usize {
    let cabs = pedals
        .iter()
        .filter(|p| matches!(&p.description, guitar_core::shared::pedals::PedalDescription::Cabinet(c) if c.ir_path.is_some()))
        .count();
    cabs * 1024
}

/// How late the scene's guitar comes out of its pedals, in samples: the
/// band is delayed as much, as an engineer lines up the tracks.
pub fn scene_latency(ctx: &Ctx, scene: &Scene) -> usize {
    scene.preset.as_deref().map_or(0, |p| chain_latency(&preset_pedals(ctx, p)))
}

fn preset_pedals(ctx: &Ctx, name: &str) -> Vec<PedalInstance> {
    ctx.tone.prepare(factory_preset(name).map(|p| p.pedals).unwrap_or_default())
}

/// The scene's guitars besides the main one.
fn extra_parts(ctx: &Ctx, scene: &Scene, pedals: &[PedalInstance]) -> (Option<(PedalChain, Vec<i16>, f32)>, Vec<(PedalChain, Vec<i16>, f32, f32)>) {
    let seed = hash(&scene.id);
    let double = match (&scene.guitar, scene.double) {
        (Some(g), true) => g.riff.as_ref().map(|r| (PedalChain::from_description(pedals.to_vec()), di(ctx, scene, r, g.take + 1, seed ^ 0x55), 0.75)),
        _ => None,
    };
    let mut backing = Vec::new();
    for (k, b) in scene.backing.iter().enumerate() {
        let chain = preset_pedals(ctx, &b.preset);
        let gain = 10f32.powf(b.gain_db / 20.0);
        let s = seed ^ (k as u64 + 1) * 0x9E37;
        if b.double {
            backing.push((PedalChain::from_description(chain.clone()), di(ctx, scene, &b.riff, 0, s), -0.85, gain));
            backing.push((PedalChain::from_description(chain), di(ctx, scene, &b.riff, 1, s ^ 0x77), 0.85, gain));
        } else {
            backing.push((PedalChain::from_description(chain), di(ctx, scene, &b.riff, 0, s), 0.0, gain));
        }
    }
    (double, backing)
}

/// A title scene's audio: the guitars through the scene's preset.
pub fn title_audio(ctx: &Ctx, scene: &Scene) -> Result<SceneAudio, String> {
    let (audio_tx, audio_rx) = bounded(VISUALIZER_CHANNEL_SAMPLES);
    let setup = AudioSetup::without_audio(audio_tx);
    let pedals = scene.preset.as_deref().map(|p| preset_pedals(ctx, p)).unwrap_or_default();
    let (double, backing) = extra_parts(ctx, scene, &pedals);
    Ok(SceneAudio {
        output: OfflineOutput::new(&setup, pedals),
        input_tx: setup.input_tx.clone(),
        _analysis: Some(audio_rx),
        relay: None,
        main: main_di(ctx, scene)?,
        main_pan: if double.is_some() { -0.75 } else { 0.0 },
        double,
        backing,
        pos: 0,
        gain: 1.0,
    })
}

/// Drives the app through a scene's actions, frame by frame.
pub struct AppDriver {
    harness: Harness<'static, AudioApp>,
    size: Vec2,
    display: Rect,
    ppp: f32,
    cursor: Option<Pos2>,
    /// Cursor animation: from, to, start and end time.
    glide: Option<(Pos2, Pos2, f32, f32)>,
    drag: Option<(Pos2, Pos2, f32, f32)>,
    pressed: bool,
    release_at: Option<f32>,
    ripple: Option<(Pos2, f32)>,
    view: Rect,
    zoom: Option<(Rect, Rect, f32, f32)>,
    started: Vec<bool>,
    fired: Vec<bool>,
    scene_id: String,
    /// A user preset written for this scene, deleted when it ends.
    temp_preset: Option<PathBuf>,
}

impl Drop for AppDriver {
    fn drop(&mut self) {
        if let Some(p) = &self.temp_preset {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// The scene's parts as a Guitar Pro file: guitar, backing guitars, bass, drums.
fn write_tab(ctx: &Ctx, scene: &Scene) -> Result<PathBuf, String> {
    let board = ctx.board;
    let bars = scene.bars.ceil() as usize;
    let mut tracks = Vec::new();
    if let Some(name) = scene.guitar.as_ref().and_then(|g| g.riff.as_ref()) {
        let program = board.riffs[name].program.unwrap_or(29);
        tracks.push(board.riff(name).track("Lead guitar", program, bars));
    }
    for b in &scene.backing {
        let program = board.riffs[&b.riff].program.unwrap_or(30);
        tracks.push(board.riff(&b.riff).track("Rhythm guitar", program, bars));
    }
    if let Some(name) = &scene.bass {
        let program = board.riffs[name].program.unwrap_or(33);
        tracks.push(board.riff(name).track("Bass", program, bars));
    }
    if let Some(name) = &scene.drums {
        tracks.push(board.drum_pattern(name).track(bars));
    }
    let score = riff::score("Promo Jam", "Guitar Virtual Amp", board.music.bpm, bars, tracks);
    let bytes = guitar_core::shared::tab::export::score_to_gp5(&score)?;
    let dir = ctx.scratch.join("tabs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("Guitar Virtual Amp - Promo Jam ({}).gp5", scene.id));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn app_scene(ctx: &Ctx, scene: &Scene) -> Result<(AppDriver, SceneAudio), String> {
    let board = ctx.board;
    let mut settings = AppSettings::default();
    settings.ui.theme = match scene.theme {
        Theme::Dark => ThemePref::Dark,
        Theme::Light => ThemePref::Light,
    };
    settings.ui.metronome_bpm = board.music.bpm as f32;
    // A count-in, not a click track over the guitar.
    settings.ui.metronome_volume = 0.35;
    let mut pedals = scene.preset.as_deref().map(|p| preset_pedals(ctx, p)).unwrap_or_default();
    if scene.bypass_all {
        pedals.iter_mut().for_each(|p| p.enabled = false);
    }
    let mut temp_preset = None;
    if let Some(name) = &scene.preset {
        if scene.page.as_deref() == Some("Presets") && !scene.bypass_all {
            // The factory entry, so the Presets page shows it selected; the
            // relay adds the IR to what it sends to the audio.
            settings.last_preset = Some(PresetRef::Factory(name.clone()));
        } else {
            // A user preset with the IR loaded, so the Cabinet card shows it.
            let dir = guitar_core::shared::preset::user_preset_dir().ok_or("no preset dir")?;
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let path = dir.join(format!("{}.json", guitar_core::shared::preset::sanitize_file_stem(name)));
            let preset = Preset { name: name.clone(), author: None, pedals: pedals.clone(), tags: Vec::new(), song: None, artist: None };
            guitar_core::shared::preset::save_to_file(&preset, &path).map_err(|e| format!("{e:?}"))?;
            settings.last_preset = Some(PresetRef::User(path.clone()));
            temp_preset = Some(path);
        }
    }
    if scene.tab {
        settings.last_tab = Some(write_tab(ctx, scene)?);
    }
    let page = match &scene.page {
        Some(p) => Page::from_label(p),
        None if scene.tab => Some(Page::Tabs),
        None => Some(Page::Pedalboard),
    };

    let (audio_tx, audio_rx) = bounded(VISUALIZER_CHANNEL_SAMPLES);
    let mut setup = AudioSetup::without_audio(audio_tx);
    // The output reads chains from the relay instead of the app directly.
    let (relay_tx, relay_rx) = bounded::<Vec<PedalInstance>>(8);
    let from_app = std::mem::replace(&mut setup.pedal_rx, relay_rx);
    let output = OfflineOutput::new(&setup, Vec::new());
    setup.pedal_rx = from_app.clone();
    let song_player = setup.song_player.clone();
    let (double, backing) = extra_parts(ctx, scene, &pedals);
    let audio = SceneAudio {
        output,
        input_tx: setup.input_tx.clone(),
        _analysis: None,
        relay: Some((from_app, relay_tx)),
        main: main_di(ctx, scene)?,
        main_pan: if double.is_some() { -0.75 } else { 0.0 },
        double,
        backing,
        pos: 0,
        gain: 1.0,
    };

    let size = match scene.layout {
        Layout::Phone => PHONE,
        _ => scene.window.map_or(DESKTOP, |[w, h]| vec2(w, h)),
    };
    let display = app_rect(scene.layout, ctx.screen, size);
    let ppp = display.width() / size.x;
    let fps = board.video.fps as f32;
    let mut harness = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(ppp)
        .with_step_dt(1.0 / fps)
        .wgpu()
        .build_eframe(move |_cc| {
            let mut app = AudioApp::new(audio_rx, setup, settings);
            app.clear_device_error();
            if let Some(page) = page {
                app.set_page(page);
            }
            app
        });
    if scene.tab {
        // The tab loads on a worker thread.
        for _ in 0..200 {
            harness.step();
            if harness.query_by_label_contains("Loop").is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    harness.run_steps(4);
    let n = scene.actions.len();
    let mut driver = AppDriver {
        harness,
        size,
        display,
        ppp,
        cursor: None,
        glide: None,
        drag: None,
        pressed: false,
        release_at: None,
        ripple: None,
        view: Rect::from_min_size(Pos2::ZERO, size),
        zoom: None,
        started: vec![false; n],
        fired: vec![false; n],
        scene_id: scene.id.clone(),
        temp_preset,
    };
    for a in &scene.setup {
        driver.instant(a)?;
    }
    if scene.tab {
        // The band is mixed with the rest of the video's (same synth and
        // SoundFont, in stereo); the tab player still plays and scores.
        song_player.set_gains(0.0, 0.0);
    }
    Ok((driver, audio))
}

fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn lerp_pos(a: Pos2, b: Pos2, k: f32) -> Pos2 {
    a + (b - a) * k
}

fn lerp_rect(a: Rect, b: Rect, k: f32) -> Rect {
    Rect::from_min_max(lerp_pos(a.min, b.min, k), lerp_pos(a.max, b.max, k))
}

impl AppDriver {
    fn find(&self, label: &str, a: &Action) -> Result<Pos2, String> {
        let rects: Vec<Rect> = if a.contains {
            self.harness.query_all_by_label_contains(label).map(|n| n.rect()).collect()
        } else {
            self.harness.query_all_by_label(label).map(|n| n.rect()).collect()
        };
        // AccessKit bounds are in pixels (egui scales its root node).
        rects.get(a.nth).map(|r| (r.center().to_vec2() / self.ppp).to_pos2()).ok_or_else(|| {
            format!(
                "scene '{}', action at {:.2}s: {} widget labelled '{label}' (found {}). \
                 Labels are exact, icons included; set contains = true to match part of one.",
                self.scene_id,
                a.at,
                if rects.is_empty() { "no".to_string() } else { format!("no #{}", a.nth) },
                rects.len()
            )
        })
    }

    /// A setup action: done at once, before the first frame.
    fn instant(&mut self, a: &Action) -> Result<(), String> {
        if let Some(label) = &a.click {
            let pos = self.find(label, a)?;
            self.push(egui::Event::PointerMoved(pos));
            self.harness.step();
            self.button(pos, true);
            self.harness.step();
            self.button(pos, false);
            self.harness.step();
            self.push(egui::Event::PointerGone);
        } else if let Some(key) = &a.key {
            self.key(key);
        } else if let Some(page) = &a.page {
            self.harness.state_mut().set_page(Page::from_label(page).expect("checked when loading"));
        } else {
            return Err(format!("scene '{}': setup actions can click, key or page", self.scene_id));
        }
        self.harness.run_steps(3);
        Ok(())
    }

    /// The view `rect` (app points) widened to the display's aspect ratio
    /// and kept inside the app.
    fn fit_view(&self, [x, y, rw, rh]: [f32; 4]) -> Rect {
        let aspect = self.size.x / self.size.y;
        let (mut w, mut h) = (rw.max(10.0), rh.max(10.0));
        if w / h > aspect {
            h = w / aspect;
        } else {
            w = h * aspect;
        }
        let (w, h) = (w.min(self.size.x), h.min(self.size.y));
        // Grown around the center of the requested rect.
        let min_x = (x + rw * 0.5 - w * 0.5).clamp(0.0, self.size.x - w);
        let min_y = (y + rh * 0.5 - h * 0.5).clamp(0.0, self.size.y - h);
        Rect::from_min_size(pos2(min_x, min_y), vec2(w, h))
    }

    fn push(&mut self, event: egui::Event) {
        self.harness.input_mut().events.push(event);
    }

    fn key(&mut self, name: &str) {
        let key = egui::Key::from_name(name).expect("checked when loading");
        for pressed in [true, false] {
            self.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
        }
    }

    fn button(&mut self, pos: Pos2, pressed: bool) {
        self.pressed = pressed;
        self.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }

    /// Advances the app to scene time `t` and renders it.
    pub fn frame(&mut self, t: f32, actions: &[Action]) -> Result<AppView, String> {
        for (i, a) in actions.iter().enumerate() {
            let pointer = a.click.as_ref().or(a.drag.as_ref()).or(a.hover.as_ref());
            // A click acts on release, which lands at `at`.
            let down_at = if a.click.is_some() { (a.at - PRESS_SECS).max(0.0) } else { a.at };
            if let Some(label) = pointer
                && !self.started[i]
                && t >= down_at - GLIDE_SECS
            {
                self.started[i] = true;
                let target = self.find(label, a)?;
                // The cursor first comes in from the right edge.
                let from = self.cursor.unwrap_or(pos2(self.size.x + 30.0, self.size.y * 0.75));
                self.glide = Some((from, target, t, down_at.max(t + 0.1)));
            }
            if self.fired[i] || t < down_at {
                continue;
            }
            self.fired[i] = true;
            let at = self.glide.map(|g| g.1).or(self.cursor).unwrap_or(self.size.to_pos2() * 0.5);
            if a.click.is_some() {
                self.glide = None;
                self.cursor = Some(at);
                self.push(egui::Event::PointerMoved(at));
                self.button(at, true);
                self.release_at = Some(a.at.max(t + 0.01));
                self.ripple = Some((at, t));
            } else if a.drag.is_some() {
                self.glide = None;
                self.cursor = Some(at);
                self.push(egui::Event::PointerMoved(at));
                self.button(at, true);
                self.drag = Some((at, at + vec2(a.dx, a.dy), t, t + a.secs.max(0.05)));
            } else if let Some(key) = &a.key {
                self.key(key);
            } else if let Some(page) = &a.page {
                let page = Page::from_label(page).expect("checked when loading");
                self.harness.state_mut().set_page(page);
            } else if let Some(amount) = a.scroll {
                let pos = self.cursor.unwrap_or(self.size.to_pos2() * 0.5);
                self.push(egui::Event::PointerMoved(pos));
                self.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(0.0, -amount),
                    modifiers: egui::Modifiers::NONE,
                });
            } else if let Some(rect) = a.zoom {
                self.zoom = Some((self.view, self.fit_view(rect), t, t + a.secs.max(0.05)));
            } else if a.unzoom {
                self.zoom = Some((self.view, Rect::from_min_size(Pos2::ZERO, self.size), t, t + a.secs.max(0.05)));
            }
        }

        if let Some((from, to, t0, t1)) = self.glide {
            let k = ease((t - t0) / (t1 - t0));
            self.cursor = Some(lerp_pos(from, to, k));
            if k >= 1.0 {
                self.glide = None;
            }
        }
        if let Some((from, to, t0, t1)) = self.drag {
            let k = ease((t - t0) / (t1 - t0));
            let pos = lerp_pos(from, to, k);
            self.cursor = Some(pos);
            if k >= 1.0 {
                self.push(egui::Event::PointerMoved(pos));
                self.button(pos, false);
                self.drag = None;
            }
        }
        if let Some(at) = self.release_at
            && t + 1e-4 >= at
        {
            let pos = self.cursor.unwrap_or_default();
            self.button(pos, false);
            self.release_at = None;
        }
        if let Some(pos) = self.cursor {
            self.push(egui::Event::PointerMoved(pos));
        }
        if let Some((from, to, t0, t1)) = self.zoom {
            let k = ease((t - t0) / (t1 - t0));
            self.view = lerp_rect(from, to, k);
            if k >= 1.0 {
                self.zoom = None;
            }
        }

        // Render so the shown part maps 1:1 onto video pixels: zooms stay sharp.
        let ppp = (self.display.width() / self.view.width()).clamp(0.5, 4.0);
        if (ppp - self.ppp).abs() > 1e-4 {
            // The native scale, not `set_pixels_per_point`: that one zooms
            // (the layout would reflow into fewer points).
            if let Some(v) = self.harness.input_mut().viewports.get_mut(&egui::ViewportId::ROOT) {
                v.native_pixels_per_point = Some(ppp);
            }
            self.ppp = ppp;
        }
        self.harness.step();
        let image = self.harness.render().map_err(|e| format!("render: {e}"))?;
        let image = egui::ColorImage::from_rgba_unmultiplied([image.width() as usize, image.height() as usize], image.as_raw());
        Ok(AppView {
            image,
            app_size: self.size,
            view: self.view,
            cursor: self.cursor.filter(|c| c.x <= self.size.x + 40.0),
            pressed: self.pressed,
            ripple: self.ripple.map(|(p, at)| (p, t - at)),
        })
    }
}
