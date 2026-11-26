//! Tuner: a needle meter for the nearest note, string targets for the
//! chosen tuning (or the open tab's), an adjustable A4 reference and a
//! fretboard showing where the note you play is on the neck.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, Ui, pos2, vec2};

use crate::frontend::lib::pitch::{detect_pitch, freq_to_midi, midi_to_freq, note_name, pitch_class_name};
use crate::frontend::lib::tunings::{TUNINGS, Tuning, display_name, label};
use crate::frontend::style::{self, Palette};
use crate::shared::config::GLOBAL_CONFIG;
use crate::shared::i18n::{Language, language, tr};
use crate::trf;
use crate::shared::settings::UiPrefs;

/// Within this many cents a string counts as in tune.
pub const IN_TUNE_CENTS: f64 = 5.0;
/// How long a reading stays on screen after the note fades.
const HOLD: Duration = Duration::from_millis(900);
/// A string held in tune this long gets a check mark.
const TUNED_AFTER: Duration = Duration::from_millis(600);
const FRETS: usize = 15;

/// Detects the pitch of the latest guitar samples. The 48 kHz buffer is
/// halved to 24 kHz first: plenty for notes below 1 kHz, and it halves the
/// YIN cost while doubling the lowest note the window can see.
pub fn detect(buffer: &VecDeque<f64>) -> Option<f64> {
    let decimated: VecDeque<f64> = buffer
        .iter()
        .copied()
        .collect::<Vec<_>>()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[a, b]| (a + b) * 0.5)
        .collect();
    detect_pitch(&decimated, GLOBAL_CONFIG.sample_rate / 2.0)
}

pub struct TunerPanel {
    /// Smoothed fractional MIDI note of the current reading.
    midi: Option<f64>,
    last_seen: Instant,
    in_tune_since: Option<(usize, Instant)>,
    /// Strings that were held in tune, for the current tuning.
    tuned: Vec<bool>,
    tuned_for: Vec<u8>,
    /// Widths of the toolbar groups last frame, for [`keep_together`].
    a4_width: f32,
    mute_width: f32,
}

impl Default for TunerPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl TunerPanel {
    pub fn new() -> Self {
        Self {
            midi: None,
            last_seen: Instant::now(),
            in_tune_since: None,
            tuned: Vec::new(),
            tuned_for: Vec::new(),
            a4_width: 0.0,
            mute_width: 0.0,
        }
    }

    /// Feeds one detection (or `None` for silence).
    fn update(&mut self, freq: Option<f64>, a4: f64) {
        let now = Instant::now();
        match freq {
            Some(f) => {
                let midi = freq_to_midi(f, a4);
                self.midi = Some(match self.midi {
                    // Same note: smooth the needle. Different note: jump.
                    Some(prev) if (prev - midi).abs() < 0.5 && now.duration_since(self.last_seen) < HOLD => {
                        prev * 0.7 + midi * 0.3
                    }
                    _ => midi,
                });
                self.last_seen = now;
            }
            None if now.duration_since(self.last_seen) > HOLD => {
                self.midi = None;
                self.in_tune_since = None;
            }
            None => {}
        }
    }

    /// The current reading, if it's live (not just held).
    fn live(&self) -> bool {
        self.last_seen.elapsed() < Duration::from_millis(150)
    }

    /// Draws the page. `tab_tuning` is the open tab's tuning, offered as a
    /// target. Returns `true` if `prefs` changed.
    pub fn ui(&mut self, ui: &mut Ui, buffer: &VecDeque<f64>, prefs: &mut UiPrefs, tab_tuning: Option<Tuning>) -> bool {
        let palette = Palette::of(ui.ctx());
        let a4 = prefs.tuner_reference_hz.clamp(400.0, 480.0) as f64;
        self.update(detect(buffer), a4);
        let mut changed = false;

        // Target tuning.
        let tuning = if prefs.tuner_tuning == TAB_TUNING {
            tab_tuning.clone().unwrap_or_else(Tuning::standard)
        } else {
            Tuning::named(&prefs.tuner_tuning).unwrap_or_else(Tuning::standard)
        };
        if self.tuned_for != tuning.strings {
            self.tuned = vec![false; tuning.strings.len()];
            self.tuned_for = tuning.strings.clone();
        }

        ui.horizontal_wrapped(|ui| {
            // Each label stays on the same line as its control when the row wraps.
            ui.horizontal(|ui| {
                ui.label(tr("Tuning:", "Afinação:"));
                let selected = if prefs.tuner_tuning == TAB_TUNING {
                    trf!("From the tab ({})", "Da tab ({})", tuning.display_name())
                } else {
                    tuning.display_name().to_string()
                };
                egui::ComboBox::from_id_salt("tuner_tuning").selected_text(selected).width(200.0).show_ui(ui, |ui| {
                    if let Some(tab) = &tab_tuning {
                        changed |= ui
                            .selectable_value(&mut prefs.tuner_tuning, TAB_TUNING.to_string(), trf!("From the tab: {}", "Da tab: {}", tab.display_name()))
                            .changed();
                        ui.separator();
                    }
                    for (name, strings) in TUNINGS {
                        let text = format!("{}  ({})", display_name(name), label(strings));
                        changed |= ui.selectable_value(&mut prefs.tuner_tuning, name.to_string(), text).changed();
                    }
                });
            });
            keep_together(ui, &mut self.a4_width, |ui| {
                ui.label("A4 =");
                changed |= ui
                    .add(egui::DragValue::new(&mut prefs.tuner_reference_hz).range(400.0..=480.0).speed(0.1).suffix(" Hz").max_decimals(1))
                    .on_hover_text(tr(
                        "Reference pitch. 440 Hz is standard; some bands tune to 442 or 432.",
                        "Nota de referência. 440 Hz é o padrão; algumas bandas afinam em 442 ou 432.",
                    ))
                    .changed();
                if (prefs.tuner_reference_hz - 440.0).abs() > 0.01 && ui.small_button("440").clicked() {
                    prefs.tuner_reference_hz = 440.0;
                    changed = true;
                }
            });
            keep_together(ui, &mut self.mute_width, |ui| {
                changed |= ui
                    .checkbox(&mut prefs.mute_while_tuning, tr("Mute output while tuning", "Silenciar saída ao afinar"))
                    .on_hover_text(tr(
                        "Silence the speakers while this page is open",
                        "Silencia os alto-falantes enquanto esta página estiver aberta",
                    ))
                    .changed();
            });
        });
        if let Some(tab) = &tab_tuning
            && prefs.tuner_tuning != TAB_TUNING
            && tab.strings != tuning.strings
        {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(palette.warn, trf!("⚠ The open tab uses {} ({}).", "⚠ A tab aberta usa {} ({}).", tab.display_name(), label(&tab.strings)));
                if ui.button(tr("Tune to the tab", "Afinar pela tab")).clicked() {
                    prefs.tuner_tuning = TAB_TUNING.to_string();
                    changed = true;
                }
            });
        }
        ui.add_space(style::space::S);

        // Reading.
        let nearest = self.midi.map(|m| {
            let note = m.round() as i32;
            (note, (m - note as f64) * 100.0)
        });
        let string_target = self.midi.and_then(|m| tuning.nearest_string(midi_to_freq(m, a4), a4));
        // Track strings held in tune.
        if let Some((string, cents)) = string_target
            && self.live()
            && cents.abs() < IN_TUNE_CENTS
        {
            match self.in_tune_since {
                Some((s, since)) if s == string => {
                    if since.elapsed() > TUNED_AFTER
                        && let Some(t) = self.tuned.get_mut(string)
                    {
                        *t = true;
                    }
                }
                _ => self.in_tune_since = Some((string, Instant::now())),
            }
        } else {
            self.in_tune_since = None;
        }

        ui.vertical_centered(|ui| {
            let live = self.live();
            let dim = |c: Color32| if live { c } else { c.gamma_multiply(0.45) };
            match nearest {
                Some((note, cents)) => {
                    let in_tune = cents.abs() < IN_TUNE_CENTS;
                    let color = dim(if in_tune { palette.ok } else { palette.text });
                    ui.label(RichText::new(note_name(note)).size(style::text::DISPLAY).strong().color(color));
                    let freq = self.midi.map(|m| midi_to_freq(m, a4)).unwrap_or(0.0);
                    ui.label(RichText::new(format!("{freq:.1} Hz · {cents:+.0} cents")).color(dim(palette.text_dim)));
                    needle(ui, cents, live, &palette);
                }
                None => {
                    ui.label(RichText::new("—").size(style::text::DISPLAY).color(palette.text_dim));
                    ui.label(RichText::new(tr("Play a single open string", "Toque uma corda solta")).color(palette.text_dim));
                    needle(ui, 0.0, false, &palette);
                }
            }
        });
        ui.add_space(style::space::S);

        // String targets.
        ui.vertical_centered(|ui| {
            let count = tuning.strings.len();
            let gap = ui.spacing().item_spacing.x;
            let available = ui.available_width();
            let per_row = chips_per_row(count, available, gap);
            let w = ((available - gap * (per_row as f32 - 1.0)) / per_row as f32).min(CHIP_MAX_WIDTH);
            for row_start in (0..count).step_by(per_row.max(1)) {
                let row = row_start..(row_start + per_row).min(count);
                ui.horizontal(|ui| {
                    let row_width = row.len() as f32 * (w + gap) - gap;
                    ui.add_space(((available - row_width) / 2.0).max(0.0));
                    for i in row {
                        let open = tuning.strings[i];
                        let active = string_target.is_some_and(|(s, _)| s == i) && self.midi.is_some();
                        let cents = string_target.filter(|(s, _)| *s == i).map(|(_, c)| c);
                        let tuned = self.tuned.get(i).copied().unwrap_or(false);
                        string_chip(ui, open as i32, tuning.string_number(i), active, cents, tuned, a4, w, &palette);
                    }
                });
            }
            if self.tuned.iter().any(|t| *t) {
                let done = self.tuned.iter().filter(|t| **t).count();
                let text = if done == self.tuned.len() {
                    tr("✔ All strings tuned", "✔ Todas as cordas afinadas").to_string()
                } else {
                    trf!("{done}/{} strings tuned", "{done}/{} cordas afinadas", self.tuned.len())
                };
                ui.horizontal(|ui| {
                    ui.add_space(((ui.available_width() - 200.0) / 2.0).max(0.0));
                    ui.label(RichText::new(text).color(palette.ok));
                    if ui.small_button(tr("Reset", "Zerar")).clicked() {
                        self.tuned.iter_mut().for_each(|t| *t = false);
                    }
                });
            }
        });
        ui.add_space(style::space::M);

        // Fretboard.
        ui.label(RichText::new(tr("Where this note is on the neck", "Onde esta nota fica no braço")).small().color(palette.text_dim));
        fretboard(ui, &tuning.strings, nearest.map(|(n, _)| n), &palette);
        changed
    }
}

/// Adds a group of widgets (a label and its control) to a
/// `horizontal_wrapped` row, after a separator, so that the group wraps as
/// a whole: egui only wraps single widgets, and would split "A4 =" from its
/// value. `width` is the group's width last frame; on the first frame, or
/// when it grew past the edge, the pass is redrawn with the new width.
fn keep_together(ui: &mut Ui, width: &mut f32, add: impl FnOnce(&mut Ui)) {
    // What's left of the current row (`available_width` is the whole row).
    let wraps = *width > ui.available_size_before_wrap().x;
    if wraps {
        ui.end_row();
    } else {
        ui.separator();
    }
    let rect = ui.horizontal(add).response.rect;
    let measured = rect.width();
    if (measured - *width).abs() > 0.5 {
        *width = measured;
        if rect.right() > ui.max_rect().right() + 0.5 {
            ui.ctx().request_discard("tuner toolbar group moved to a new row");
        }
    }
}

/// `UiPrefs::tuner_tuning` value meaning "the open tab's tuning".
pub const TAB_TUNING: &str = "@tab";

fn needle(ui: &mut Ui, cents: f64, live: bool, palette: &Palette) {
    let width = ui.available_width().min(420.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, width * 0.42), Sense::hover());
    let painter = ui.painter_at(rect);
    let center = pos2(rect.center().x, rect.bottom() - 6.0);
    let radius = (rect.width() / 2.0 - 8.0).min(rect.height() - 12.0);
    let angle = |c: f64| (270.0 + c.clamp(-50.0, 50.0) * 1.4).to_radians() as f32;
    let at = |c: f64, r: f32| center + vec2(angle(c).cos(), angle(c).sin()) * r;
    // In-tune zone.
    let zone: Vec<_> = (-5..=5).map(|c| at(c as f64, radius)).collect();
    painter.add(egui::Shape::line(zone, Stroke::new(8.0_f32, palette.ok.gamma_multiply(0.5))));
    for c in (-50..=50).step_by(10) {
        let major = c % 50 == 0 || c == 0;
        let inner = if major { radius - 16.0 } else { radius - 9.0 };
        painter.line_segment(
            [at(c as f64, inner), at(c as f64, radius)],
            Stroke::new(if major { 2.0 } else { 1.0_f32 }, palette.text_dim),
        );
    }
    let small = FontId::proportional(style::text::SMALL);
    painter.text(at(-50.0, radius - 26.0), Align2::CENTER_CENTER, "b", FontId::proportional(18.0), palette.text_dim);
    painter.text(at(50.0, radius - 26.0), Align2::CENTER_CENTER, "#", FontId::proportional(18.0), palette.text_dim);
    painter.text(at(0.0, radius - 26.0), Align2::CENTER_CENTER, "0", small, palette.text_dim);
    let in_tune = cents.abs() < IN_TUNE_CENTS;
    let color = if !live {
        palette.text_dim.gamma_multiply(0.6)
    } else if in_tune {
        palette.ok
    } else if cents.abs() < 15.0 {
        palette.warn
    } else {
        palette.danger
    };
    painter.line_segment([center, at(cents, radius - 4.0)], Stroke::new(3.0_f32, color));
    painter.circle_filled(center, 6.0, color);
}

/// String chips are at most this wide, and get two rows (or more) before
/// they'd shrink below the minimum.
const CHIP_MAX_WIDTH: f32 = 84.0;
const CHIP_MIN_WIDTH: f32 = 36.0;

/// How many string chips go in one row of `available` points: all of them
/// if they fit at [`CHIP_MIN_WIDTH`], else rows of (nearly) equal length.
fn chips_per_row(count: usize, available: f32, gap: f32) -> usize {
    if count == 0 {
        return 1;
    }
    let fit = (((available + gap) / (CHIP_MIN_WIDTH + gap)).floor() as usize).clamp(1, count);
    let rows = count.div_ceil(fit);
    count.div_ceil(rows)
}

/// The second line of a string chip: how far off the string is while it's
/// the one being played, else its number (1 = highest string). `short` is
/// the form for narrow chips.
fn chip_status(cents: Option<f64>, string_number: usize, short: bool, language: Language) -> String {
    let pt = language == Language::Portuguese;
    match cents {
        Some(c) if c.abs() < IN_TUNE_CENTS => {
            if short {
                "✔".to_string()
            } else if pt {
                "✔ afinada".to_string()
            } else {
                "✔ in tune".to_string()
            }
        }
        Some(c) => {
            // Sharp: tune down. Flat: tune up.
            let arrow = if c > 0.0 { "⬇" } else { "⬆" };
            let amount = c.abs().round();
            if short {
                format!("{arrow}{amount:.0}¢")
            } else {
                let word = match (c > 0.0, pt) {
                    (true, false) => "sharp",
                    (true, true) => "aguda",
                    (false, false) => "flat",
                    (false, true) => "grave",
                };
                format!("{arrow} {amount:.0}¢ {word}")
            }
        }
        None if short => string_number.to_string(),
        None if pt => format!("corda {string_number}"),
        None => format!("string {string_number}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn string_chip(
    ui: &mut Ui,
    open: i32,
    string_number: usize,
    active: bool,
    cents: Option<f64>,
    tuned: bool,
    a4: f64,
    width: f32,
    palette: &Palette,
) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 52.0), Sense::hover());
    let compact = width < 56.0;
    let painter = ui.painter_at(rect);
    let fill = if active { palette.accent.gamma_multiply(0.25) } else { palette.card };
    painter.rect_filled(rect, 8.0, fill);
    let stroke = if tuned { palette.ok } else if active { palette.accent } else { palette.card_stroke };
    painter.rect_stroke(rect, 8.0, Stroke::new(if active { 2.0 } else { 1.0_f32 }, stroke), egui::StrokeKind::Inside);
    painter.text(
        rect.center() - vec2(0.0, 7.0),
        Align2::CENTER_CENTER,
        format!("{}{}", note_name(open), if tuned { " ✔" } else { "" }),
        FontId::proportional(if compact { 14.0 } else { 16.0 }),
        if tuned { palette.ok } else { palette.text },
    );
    // The long form if it fits the chip, else the short one.
    let font = FontId::proportional(style::text::SMALL);
    let status_color = match cents {
        Some(c) if c.abs() < IN_TUNE_CENTS => palette.ok,
        Some(_) => palette.text,
        None => palette.text_dim,
    };
    let language = language();
    let mut status = painter.layout_no_wrap(chip_status(cents, string_number, compact, language), font.clone(), status_color);
    if !compact && status.size().x > rect.width() - 6.0 {
        status = painter.layout_no_wrap(chip_status(cents, string_number, true, language), font, status_color);
    }
    painter.galley(rect.center() + vec2(0.0, 12.0) - status.size() / 2.0, status, status_color);
    response.on_hover_text(trf!(
        "String {}: {} = {:.2} Hz. ⬆ tune up, ⬇ tune down",
        "Corda {}: {} = {:.2} Hz. ⬆ subir, ⬇ descer",
        string_number,
        note_name(open),
        midi_to_freq(open as f64, a4)
    ));
}

fn fretboard(ui: &mut Ui, strings_low_first: &[u8], note: Option<i32>, palette: &Palette) {
    let n = strings_low_first.len().max(1);
    let width = ui.available_width().min(900.0);
    let spacing = 18.0;
    let height = spacing * (n as f32 - 1.0) + 36.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    let left = rect.left() + 34.0;
    let right = rect.right() - 8.0;
    let fret_w = (right - left) / FRETS as f32;
    let top = rect.top() + 8.0;
    let wood = if palette.dark { Color32::from_rgb(52, 40, 30) } else { Color32::from_rgb(225, 205, 175) };
    painter.rect_filled(
        egui::Rect::from_min_max(pos2(left, top - 6.0), pos2(right, top + spacing * (n as f32 - 1.0) + 6.0)),
        3.0,
        wood,
    );
    for f in 0..=FRETS {
        let x = left + f as f32 * fret_w;
        let w = if f == 0 { 4.0 } else { 1.5_f32 };
        painter.line_segment(
            [pos2(x, top - 6.0), pos2(x, top + spacing * (n as f32 - 1.0) + 6.0)],
            Stroke::new(w, Color32::from_gray(if f == 0 { 230 } else { 160 })),
        );
        if [3, 5, 7, 9, 12, 15].contains(&f) {
            painter.text(
                pos2(x - fret_w / 2.0, rect.bottom() - 4.0),
                Align2::CENTER_BOTTOM,
                f.to_string(),
                FontId::proportional(10.0),
                palette.text_dim,
            );
        }
    }
    // Highest string on top, like a tab.
    for (row, &open) in strings_low_first.iter().rev().enumerate() {
        let y = top + row as f32 * spacing;
        painter.line_segment([pos2(left, y), pos2(right, y)], Stroke::new(1.0 + (n - row) as f32 * 0.25, Color32::from_gray(200)));
        painter.text(
            pos2(rect.left() + 4.0, y),
            Align2::LEFT_CENTER,
            pitch_class_name(open as i32),
            FontId::proportional(11.0),
            palette.text_dim,
        );
        let Some(note) = note else { continue };
        for fret in 0..=FRETS {
            let here = open as i32 + fret as i32;
            if here.rem_euclid(12) != note.rem_euclid(12) {
                continue;
            }
            let x = if fret == 0 { left - 12.0 } else { left + (fret as f32 - 0.5) * fret_w };
            let exact = here == note;
            let color = if exact { palette.accent } else { palette.accent.gamma_multiply(0.4) };
            painter.circle_filled(pos2(x, y), if exact { 7.0 } else { 5.0 }, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn detects_a_guitar_string_through_the_decimated_path() {
        let buffer: VecDeque<f64> =
            (0..4096).map(|i| 0.5 * (2.0 * PI * 110.0 * i as f64 / 48_000.0).sin()).collect();
        let f = detect(&buffer).expect("pitch");
        assert!((f - 110.0).abs() < 0.5, "{f}");
    }

    #[test]
    fn chip_status_says_which_way_to_tune() {
        let en = Language::English;
        let pt = Language::Portuguese;
        assert_eq!(chip_status(Some(16.2), 5, false, en), "⬇ 16¢ sharp");
        assert_eq!(chip_status(Some(16.2), 5, false, pt), "⬇ 16¢ aguda");
        assert_eq!(chip_status(Some(-9.6), 5, false, en), "⬆ 10¢ flat");
        assert_eq!(chip_status(Some(-9.6), 5, false, pt), "⬆ 10¢ grave");
        assert_eq!(chip_status(Some(-9.6), 5, true, en), "⬆10¢");
        assert_eq!(chip_status(Some(2.0), 5, false, en), "✔ in tune");
        assert_eq!(chip_status(Some(-2.0), 5, false, pt), "✔ afinada");
        assert_eq!(chip_status(Some(2.0), 5, true, pt), "✔");
        assert_eq!(chip_status(None, 6, false, en), "string 6");
        assert_eq!(chip_status(None, 6, false, pt), "corda 6");
        assert_eq!(chip_status(None, 6, true, en), "6");
    }

    #[test]
    fn string_chips_wrap_into_even_rows() {
        // Six 64-pt chips fit a wide card; on a phone they shrink.
        assert_eq!(chips_per_row(6, 1100.0, 8.0), 6);
        assert_eq!(chips_per_row(6, 340.0, 8.0), 6);
        // Eight strings don't fit 340 pt at the minimum width: two rows of 4.
        assert_eq!(chips_per_row(8, 340.0, 8.0), 4);
        // Seven in a 200-pt column: rows of 4 and 3.
        assert_eq!(chips_per_row(7, 200.0, 8.0), 4);
        assert_eq!(chips_per_row(6, 10.0, 8.0), 1);
        assert_eq!(chips_per_row(0, 300.0, 8.0), 1);
    }

    #[test]
    fn page_renders_and_marks_strings_tuned() {
        let ctx = egui::Context::default();
        let mut panel = TunerPanel::new();
        let mut prefs = UiPrefs::default();
        let buffer: VecDeque<f64> =
            (0..4096).map(|i| 0.5 * (2.0 * PI * 110.0 * i as f64 / 48_000.0).sin()).collect();
        let tab = Tuning::from_tab(&[64, 59, 55, 50, 45, 38]);
        for _ in 0..3 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel.ui(ui, &buffer, &mut prefs, tab.clone());
                });
            });
        }
        assert_eq!(panel.midi.map(|m| m.round()), Some(45.0));
        panel.in_tune_since = Some((1, Instant::now() - TUNED_AFTER * 2));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                panel.ui(ui, &buffer, &mut prefs, None);
            });
        });
        assert!(panel.tuned[1], "A string marked tuned");
    }
}
