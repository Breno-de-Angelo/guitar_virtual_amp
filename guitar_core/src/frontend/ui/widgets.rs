//! Custom-painted controls: knobs, bypass LEDs, footswitches and level
//! meters. Colors and sizes come from `frontend::style`.

use std::ops::RangeInclusive;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, Widget,
    pos2, vec2,
};

use crate::frontend::style::{self, Palette};

/// Knob sweep: from 7:30 to 4:30 o'clock, like a real pot.
const SWEEP_START: f32 = -225.0_f32;
const SWEEP: f32 = 270.0_f32;
/// Pixels of vertical drag for the full range (Shift = 5x finer).
const DRAG_PIXELS: f32 = 160.0;

/// A rotary control. Drag up/down to turn, Shift for fine control,
/// double-click to reset to the default, right-click to type a value.
pub struct Knob<'a> {
    value: &'a mut f32,
    range: RangeInclusive<f32>,
    label: &'a str,
    default: Option<f32>,
    logarithmic: bool,
    suffix: &'a str,
    decimals: usize,
    integer: bool,
    color: Option<Color32>,
    size: f32,
}

impl<'a> Knob<'a> {
    pub fn new(value: &'a mut f32, range: RangeInclusive<f32>, label: &'a str) -> Self {
        Self {
            value,
            range,
            label,
            default: None,
            logarithmic: false,
            suffix: "",
            decimals: 2,
            integer: false,
            color: None,
            size: style::KNOB_SIZE,
        }
    }
    pub fn default_value(mut self, default: f32) -> Self {
        self.default = Some(default);
        self
    }
    /// Even turns per octave/decade, for frequencies and times.
    pub fn logarithmic(mut self, log: bool) -> Self {
        self.logarithmic = log && *self.range.start() > 0.0;
        self
    }
    pub fn suffix(mut self, suffix: &'a str) -> Self {
        self.suffix = suffix;
        self
    }
    pub fn decimals(mut self, decimals: usize) -> Self {
        self.decimals = decimals;
        self
    }
    pub fn integer(mut self) -> Self {
        self.integer = true;
        self.decimals = 0;
        self
    }
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    fn value_to_t(&self, value: f32) -> f32 {
        let (min, max) = (*self.range.start(), *self.range.end());
        if max <= min {
            return 0.0;
        }
        let t = if self.logarithmic {
            (value.max(min) / min).ln() / (max / min).ln()
        } else {
            (value - min) / (max - min)
        };
        t.clamp(0.0, 1.0)
    }

    fn t_to_value(&self, t: f32) -> f32 {
        let (min, max) = (*self.range.start(), *self.range.end());
        let t = t.clamp(0.0, 1.0);
        let value = if self.logarithmic { min * (max / min).powf(t) } else { min + (max - min) * t };
        if self.integer { value.round() } else { value }
    }
}

/// `value` formatted with a sensible number of decimals for its size.
pub fn format_value(value: f32, decimals: usize, suffix: &str) -> String {
    if value.abs() >= 1000.0 && suffix.trim() == "Hz" {
        format!("{:.1} kHz", value / 1000.0)
    } else {
        format!("{value:.decimals$}{suffix}")
    }
}

impl Widget for Knob<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let palette = Palette::of(ui.ctx());
        let label_font = FontId::proportional(style::text::SMALL);
        let width = (self.size + 18.0).max(58.0);
        let height = self.size + 30.0;
        let (rect, mut response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
        let accent = self.color.unwrap_or(palette.accent);

        let before = *self.value;
        let mut t = self.value_to_t(*self.value);
        if response.dragged() {
            let fine = ui.input(|i| i.modifiers.shift);
            let pixels = if fine { DRAG_PIXELS * 5.0 } else { DRAG_PIXELS };
            t = (t - response.drag_delta().y / pixels + response.drag_delta().x / pixels).clamp(0.0, 1.0);
            *self.value = self.t_to_value(t);
        }
        if response.double_clicked()
            && let Some(default) = self.default
        {
            *self.value = default;
            t = self.value_to_t(default);
        }
        let mut typed = *self.value;
        let (min, max) = (*self.range.start(), *self.range.end());
        response.context_menu(|ui| {
            ui.label(self.label);
            let drag = egui::DragValue::new(&mut typed)
                .range(min..=max)
                .speed((max - min) / 200.0)
                .suffix(self.suffix)
                .max_decimals(self.decimals.max(1));
            ui.add(drag);
            if let Some(default) = self.default
                && ui.button("Reset").clicked()
            {
                typed = default;
                ui.close();
            }
        });
        if typed != *self.value {
            *self.value = if self.integer { typed.round() } else { typed };
            t = self.value_to_t(*self.value);
        }
        if *self.value != before {
            response.mark_changed();
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }

        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect.expand(2.0));
            let radius = self.size / 2.0;
            let center = pos2(rect.center().x, rect.top() + radius + 2.0);
            let active = response.hovered() || response.dragged();
            // Track and value arcs.
            let arc = |from: f32, to: f32| -> Vec<Pos2> {
                let steps = 32;
                (0..=steps)
                    .map(|i| {
                        let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
                        center + vec2(a.cos(), a.sin()) * (radius - 1.5)
                    })
                    .collect()
            };
            let start = SWEEP_START;
            painter.add(Shape::line(arc(start, start + SWEEP), Stroke::new(3.0_f32, palette.control)));
            // Bipolar ranges (e.g. -12..12 dB) fill from the center.
            let zero = if min < 0.0 && max > 0.0 { self.value_to_t(0.0) } else { 0.0 };
            let (a0, a1) = (start + SWEEP * zero.min(t), start + SWEEP * zero.max(t));
            if a1 - a0 > 0.5 {
                painter.add(Shape::line(arc(a0, a1), Stroke::new(3.0_f32, accent)));
            }
            // Body.
            let body = if palette.dark { Color32::from_rgb(28, 28, 31) } else { Color32::from_rgb(70, 70, 76) };
            painter.circle_filled(center, radius - 6.0, body);
            painter.circle_stroke(
                center,
                radius - 6.0,
                Stroke::new(1.0_f32, if active { accent } else { Color32::from_gray(90) }),
            );
            let angle = (start + SWEEP * t).to_radians();
            let dir = vec2(angle.cos(), angle.sin());
            painter.line_segment(
                [center + dir * (radius * 0.18), center + dir * (radius - 9.0)],
                Stroke::new(2.5_f32, Color32::from_gray(235)),
            );
            // Label, and the value while hovering/dragging.
            let text = if active {
                format_value(*self.value, self.decimals, self.suffix)
            } else {
                self.label.to_string()
            };
            let text_color = if active { accent } else { palette.text_dim };
            painter.text(
                pos2(rect.center().x, rect.bottom() - 2.0),
                Align2::CENTER_BOTTOM,
                text,
                label_font,
                text_color,
            );
        }
        let value_text = format_value(*self.value, self.decimals, self.suffix);
        let (value, label) = (*self.value as f64, self.label.to_string());
        response.widget_info(|| egui::WidgetInfo::slider(true, value, label.clone()));
        response.on_hover_text(format!(
            "{}: {value_text}\nDrag to turn (Shift: fine), double-click to reset, right-click to type",
            self.label
        ))
    }
}

/// Knob over a `usize` (e.g. delay taps).
pub fn knob_usize(
    ui: &mut Ui,
    value: &mut usize,
    range: RangeInclusive<usize>,
    label: &str,
    default: usize,
    color: Color32,
) -> Response {
    let mut v = *value as f32;
    let response = ui.add(
        Knob::new(&mut v, (*range.start() as f32)..=(*range.end() as f32), label)
            .integer()
            .default_value(default as f32)
            .color(color),
    );
    *value = (v.round() as usize).clamp(*range.start(), *range.end());
    response
}

/// A round status light. Clickable.
pub fn led(ui: &mut Ui, on: bool, size: f32, on_color: Color32) -> Response {
    let palette = Palette::of(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let c = rect.center();
        let r = size / 2.0 - 1.0;
        if on {
            painter.circle_filled(c, r + 2.5, on_color.gamma_multiply(0.25));
            painter.circle_filled(c, r, on_color);
            painter.circle_filled(c + vec2(-r * 0.3, -r * 0.3), r * 0.3, Color32::from_white_alpha(120));
        } else {
            painter.circle_filled(c, r, palette.led_off);
        }
        painter.circle_stroke(c, r, Stroke::new(1.0_f32, Color32::from_black_alpha(120)));
    }
    response
}

/// A stompbox footswitch: a metal button that looks pressed while held.
pub fn footswitch(ui: &mut Ui, on: bool) -> Response {
    let size = 34.0;
    let (rect, response) = ui.allocate_exact_size(vec2(size + 8.0, size + 4.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let c = rect.center();
        let r = size / 2.0;
        let pressed = response.is_pointer_button_down_on();
        painter.circle_filled(c + vec2(0.0, 2.0), r, Color32::from_black_alpha(90));
        let outer = if response.hovered() { Color32::from_gray(200) } else { Color32::from_gray(170) };
        painter.circle_filled(c, r, outer);
        painter.circle_filled(c, r - 4.0, if pressed { Color32::from_gray(120) } else { Color32::from_gray(150) });
        painter.circle_stroke(c, r - 4.0, Stroke::new(1.0_f32, Color32::from_gray(100)));
        painter.circle_filled(c + vec2(-3.0, -4.0), 3.0, Color32::from_white_alpha(if on { 140 } else { 90 }));
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Meter ballistics: instant rise, 24 dB/s fall, 1.5 s peak hold and a
/// clip light that stays on for 2 s.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterState {
    pub level: f32,
    pub hold: f32,
    hold_secs: f32,
    pub clip_secs: f32,
}

/// Meter floor, dB.
pub const METER_FLOOR_DB: f32 = -60.0;

fn to_db(level: f32) -> f32 {
    20.0 * level.max(1e-6).log10()
}

impl MeterState {
    pub fn update(&mut self, peak: f32, dt: f32) {
        let peak = peak.abs().min(1.0);
        let fallen = 10f32.powf((to_db(self.level) - 24.0 * dt) / 20.0);
        self.level = peak.max(if self.level > 1e-6 { fallen } else { 0.0 });
        self.hold_secs -= dt;
        if peak >= self.hold {
            self.hold = peak;
            self.hold_secs = 1.5;
        } else if self.hold_secs <= 0.0 {
            // Hold expired: the marker falls with the meter.
            self.hold = self.level;
        }
        self.clip_secs = if peak >= 0.999 { 2.0 } else { (self.clip_secs - dt).max(0.0) };
    }

    /// 0..1 position on the meter for a linear level.
    pub fn fraction(level: f32) -> f32 {
        ((to_db(level) - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
    }
}

/// A horizontal peak meter, `size` wide. Hover shows the level in dBFS.
pub fn level_meter(ui: &mut Ui, state: &MeterState, size: Vec2, label: &str) -> Response {
    let palette = Palette::of(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        let clip_w = 6.0;
        let bar = Rect::from_min_max(rect.min, pos2(rect.max.x - clip_w - 2.0, rect.max.y));
        // Colored segments: green to -12 dB, yellow to -3 dB, red above.
        let frac = MeterState::fraction(state.level);
        let segments = [(-12.0, palette.ok), (-3.0, palette.warn), (0.0, palette.danger)];
        let mut from = 0.0;
        for (db, color) in segments {
            let to = ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).min(frac);
            if to > from {
                let r = Rect::from_min_max(
                    pos2(bar.left() + bar.width() * from, bar.top()),
                    pos2(bar.left() + bar.width() * to, bar.bottom()),
                );
                painter.rect_filled(r, 1.0, color);
            }
            from = from.max((db - METER_FLOOR_DB) / -METER_FLOOR_DB);
        }
        let hold_x = bar.left() + bar.width() * MeterState::fraction(state.hold);
        if state.hold > 1e-4 {
            painter.line_segment([pos2(hold_x, bar.top()), pos2(hold_x, bar.bottom())], Stroke::new(1.5_f32, palette.text));
        }
        let clip = Rect::from_min_max(pos2(rect.max.x - clip_w, rect.min.y), rect.max);
        painter.rect_filled(
            clip,
            1.0,
            if state.clip_secs > 0.0 { palette.danger } else { palette.danger.gamma_multiply(0.2) },
        );
        if !label.is_empty() {
            painter.text(
                pos2(bar.left() + 3.0, bar.center().y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(9.5),
                if palette.dark { Color32::from_gray(230) } else { Color32::from_gray(30) },
            );
        }
    }
    let db = to_db(state.hold);
    let text = if state.hold < 1e-5 { "silence".to_string() } else { format!("peak {db:.1} dBFS") };
    let clip_text = if state.clip_secs > 0.0 { " · CLIPPING" } else { "" };
    response.on_hover_text(format!("{label} {text}{clip_text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knob_mapping_round_trips_linear_and_log() {
        let mut v = 0.0;
        let k = Knob::new(&mut v, -12.0..=12.0, "x");
        assert!((k.value_to_t(0.0) - 0.5).abs() < 1e-6);
        assert!((k.t_to_value(0.25) + 6.0).abs() < 1e-5);
        let mut f = 1000.0;
        let k = Knob::new(&mut f, 100.0..=10_000.0, "f").logarithmic(true);
        assert!((k.value_to_t(1000.0) - 0.5).abs() < 1e-5);
        assert!((k.t_to_value(0.5) - 1000.0).abs() < 0.1);
        let mut n = 0.0;
        let k = Knob::new(&mut n, 1.0..=8.0, "n").integer();
        assert_eq!(k.t_to_value(0.49), 4.0);
    }

    #[test]
    fn meter_falls_slowly_and_holds_peaks() {
        let mut m = MeterState::default();
        m.update(0.5, 0.016);
        assert_eq!(m.level, 0.5);
        m.update(0.0, 0.5); // 12 dB lower after half a second
        assert!((to_db(m.level) - (to_db(0.5) - 12.0)).abs() < 0.1);
        assert_eq!(m.hold, 0.5);
        m.update(1.0, 0.016);
        assert!(m.clip_secs > 0.0);
    }

    #[test]
    fn formats_kilohertz() {
        assert_eq!(format_value(2500.0, 0, " Hz"), "2.5 kHz");
        assert_eq!(format_value(250.0, 0, " Hz"), "250 Hz");
        assert_eq!(format_value(0.5, 2, ""), "0.50");
    }

    #[test]
    fn widgets_render_without_panicking() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let mut v = 1.0;
                ui.add(Knob::new(&mut v, 0.0..=5.0, "Gain").default_value(1.0));
                let mut taps = 3;
                knob_usize(ui, &mut taps, 1..=8, "Taps", 4, Color32::RED);
                led(ui, true, 14.0, Color32::RED);
                footswitch(ui, false);
                level_meter(ui, &MeterState { level: 0.5, hold: 0.7, ..Default::default() }, vec2(100.0, 10.0), "IN");
            });
        });
    }
}
