//! Renders the app's pages offscreen (egui_kittest + wgpu) to PNG files.
//!
//! Usage: `cargo run -- [OUT_DIR] [FILTER]` from this folder. OUT_DIR
//! defaults to `snapshots/`; FILTER renders only shots whose name contains
//! it. `GVA_SNAPSHOT_TAB=<file.gp5>` adds a Tabs page shot with that tab
//! open and the Loop and Practice toggles clicked.
//!
//! The app runs without audio devices (`AudioSetup::without_audio`), and
//! `XDG_CONFIG_HOME`/`XDG_DATA_HOME` point at a scratch folder so it never
//! reads or writes your real settings and presets.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_kittest::kittest::Queryable;
use guitar_core::backend::capture::AudioSetup;
use guitar_core::frontend::gui::{AudioApp, Page, VISUALIZER_CHANNEL_SAMPLES};
use guitar_core::shared::settings::{AppSettings, PresetRef, ThemePref};

const WIDE: egui::Vec2 = egui::vec2(1280.0, 820.0);
/// A typical phone, in dp.
const PHONE: egui::Vec2 = egui::vec2(390.0, 844.0);

struct Shot {
    name: &'static str,
    size: egui::Vec2,
    page: Page,
    theme: ThemePref,
    /// Factory preset to load, or "".
    preset: &'static str,
    /// A test tone fed to the tuner/analyzer, Hz.
    tone_hz: Option<f64>,
}

fn harness(settings: AppSettings, size: egui::Vec2, page: Option<Page>) -> (egui_kittest::Harness<'static, AudioApp>, crossbeam::channel::Sender<i16>) {
    let (audio_tx, audio_rx) = crossbeam::channel::bounded(VISUALIZER_CHANNEL_SAMPLES);
    let tx = audio_tx.clone();
    let harness = egui_kittest::Harness::builder().with_size(size).build_eframe(move |_cc| {
        let mut app = AudioApp::new(audio_rx, AudioSetup::without_audio(audio_tx), settings);
        if let Some(page) = page {
            app.set_page(page);
        }
        app
    });
    (harness, tx)
}

fn save(harness: &mut egui_kittest::Harness<'_, AudioApp>, dir: &Path, name: &str) {
    let path = dir.join(format!("{name}.png"));
    harness.render().expect("render").save(&path).expect("write png");
    println!("{}", path.display());
}

fn page_shot(shot: &Shot, dir: &Path) {
    let mut settings = AppSettings::default();
    settings.ui.theme = shot.theme;
    if !shot.preset.is_empty() {
        settings.last_preset = Some(PresetRef::Factory(shot.preset.into()));
    }
    let (mut harness, tx) = harness(settings, shot.size, Some(shot.page));
    for _ in 0..6 {
        if let Some(f) = shot.tone_hz {
            for i in 0..4096 {
                let t = i as f64 / 48_000.0;
                let v = 0.4 * (std::f64::consts::TAU * f * t).sin() + 0.15 * (std::f64::consts::TAU * 2.0 * f * t).sin();
                let _ = tx.try_send((v * 32767.0) as i16);
            }
        }
        harness.step();
    }
    save(&mut harness, dir, shot.name);
}

/// Opens the add-pedal menu, adds a Phaser, drags its Rate knob and opens
/// the shortcuts window.
fn interaction_shots(dir: &Path) {
    let mut settings = AppSettings::default();
    settings.last_preset = Some(PresetRef::Factory("Clean Jazz".into()));
    let (mut harness, _tx) = harness(settings, WIDE, Some(Page::Pedalboard));
    harness.run_steps(3);
    harness.get_by_label("➕ Add pedal").click();
    harness.run_steps(3);
    save(&mut harness, dir, "add_menu");
    harness.get_by_label("Phaser").click();
    harness.run_steps(3);
    let center = harness.get_all_by_label("Rate").next().expect("a Rate knob").rect().center() - egui::vec2(0.0, 10.0);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    harness.input_mut().events.push(egui::Event::PointerMoved(center));
    harness.run_steps(1);
    harness.input_mut().events.push(button(center, true));
    harness.run_steps(1);
    for i in 1..=10 {
        harness.input_mut().events.push(egui::Event::PointerMoved(center - egui::vec2(0.0, 6.0 * i as f32)));
        harness.run_steps(1);
    }
    save(&mut harness, dir, "knob_drag");
    harness.input_mut().events.push(button(center - egui::vec2(0.0, 60.0), false));
    harness.key_press(egui::Key::F1);
    harness.run_steps(3);
    save(&mut harness, dir, "shortcuts");
}

fn tab_shot(dir: &Path, tab: PathBuf) {
    let mut settings = AppSettings::default();
    settings.last_tab = Some(tab);
    let (mut harness, _tx) = harness(settings, WIDE, Some(Page::Tabs));
    // The tab loads on a worker thread.
    for _ in 0..30 {
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    harness.get_by_label("🔁 Loop").click();
    harness.step();
    harness.get_by_label("🎯 Prática").click();
    harness.run_steps(4);
    save(&mut harness, dir, "tabs_loaded");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().unwrap_or_else(|| "snapshots".into()));
    let filter = args.next().unwrap_or_default();
    std::fs::create_dir_all(&dir).expect("create output dir");
    let scratch = std::env::temp_dir().join(format!("gva_ui_snapshots_{}", std::process::id()));
    // SAFETY: single-threaded here, before the app (and any thread) starts.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", scratch.join("config"));
        std::env::set_var("XDG_DATA_HOME", scratch.join("data"));
    }

    let shots = [
        Shot { name: "pedals_dark", size: WIDE, page: Page::Pedalboard, theme: ThemePref::Dark, preset: "Metal High Gain", tone_hz: None },
        Shot { name: "pedals_light", size: WIDE, page: Page::Pedalboard, theme: ThemePref::Light, preset: "Ambient Ping-Pong", tone_hz: None },
        Shot { name: "pedals_empty", size: WIDE, page: Page::Pedalboard, theme: ThemePref::Dark, preset: "", tone_hz: None },
        Shot { name: "pedals_phone", size: PHONE, page: Page::Pedalboard, theme: ThemePref::Dark, preset: "Blues Crunch", tone_hz: None },
        Shot { name: "presets", size: WIDE, page: Page::Presets, theme: ThemePref::Dark, preset: "Rock Lead", tone_hz: None },
        Shot { name: "tuner", size: WIDE, page: Page::Tuner, theme: ThemePref::Dark, preset: "", tone_hz: Some(111.0) },
        Shot { name: "tuner_phone", size: PHONE, page: Page::Tuner, theme: ThemePref::Dark, preset: "", tone_hz: Some(146.0) },
        Shot { name: "tabs", size: WIDE, page: Page::Tabs, theme: ThemePref::Dark, preset: "", tone_hz: None },
        Shot { name: "analyzer", size: WIDE, page: Page::Analyzer, theme: ThemePref::Dark, preset: "", tone_hz: Some(220.0) },
        Shot { name: "settings", size: WIDE, page: Page::Settings, theme: ThemePref::Dark, preset: "", tone_hz: None },
        Shot { name: "spotify", size: WIDE, page: Page::Spotify, theme: ThemePref::Dark, preset: "", tone_hz: None },
    ];
    for shot in shots.iter().filter(|s| s.name.contains(&filter)) {
        page_shot(shot, &dir);
    }
    if "add_menu knob_drag shortcuts".contains(&filter) || filter.is_empty() {
        interaction_shots(&dir);
    }
    if let Ok(tab) = std::env::var("GVA_SNAPSHOT_TAB")
        && "tabs_loaded".contains(&filter)
    {
        tab_shot(&dir, PathBuf::from(tab));
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
