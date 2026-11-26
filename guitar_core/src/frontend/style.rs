//! App-wide look: colors, spacing and the `egui::Style` built from them.
//! See `docs/style-guide.md` for the reasoning behind these values.
//!
//! Every page imports its colors and sizes from here instead of hardcoding
//! them, so the theme can change in one place.

use eframe::egui::{self, Color32, CornerRadius, Margin, Stroke, Theme, Vec2};

use crate::shared::pedals::PedalCategory;
use crate::shared::settings::{ThemePref, UiPrefs};

/// Spacing scale, in points. Use these instead of ad-hoc numbers.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
    pub const L: f32 = 16.0;
    pub const XL: f32 = 24.0;
}

/// Text sizes, in points.
pub mod text {
    pub const SMALL: f32 = 11.0;
    pub const BODY: f32 = 13.5;
    pub const HEADING: f32 = 18.0;
    /// Big readouts: the tuner note, the BPM.
    pub const DISPLAY: f32 = 44.0;
}

/// Below this window width the navigation moves to a bottom bar and the
/// controls grow to touch size.
pub const NARROW_WIDTH: f32 = 720.0;
/// Minimum height of a tappable control on narrow (phone) layouts: Android
/// asks for 48 dp, and egui points are dp on Android.
pub const TOUCH_TARGET: f32 = 44.0;
/// Width of a pedal card on the pedalboard.
pub const PEDAL_CARD_WIDTH: f32 = 212.0;
/// Diameter of a knob.
pub const KNOB_SIZE: f32 = 44.0;
pub const CARD_RADIUS: u8 = 8;

/// Semantic colors for one theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Brand / selection color: warm amber, like an amp's pilot light.
    pub accent: Color32,
    /// Accent for text on top of `accent`.
    pub on_accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub danger: Color32,
    /// Bypass LED when the pedal is on / off.
    pub led_on: Color32,
    pub led_off: Color32,
    /// Window and page background.
    pub background: Color32,
    /// Side/top bars.
    pub panel: Color32,
    /// Pedal cards and grouped sections.
    pub card: Color32,
    pub card_stroke: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    /// Knob body and slider rail.
    pub control: Color32,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            accent: Color32::from_rgb(255, 170, 60),
            on_accent: Color32::from_rgb(30, 22, 10),
            ok: Color32::from_rgb(90, 210, 110),
            warn: Color32::from_rgb(240, 200, 70),
            danger: Color32::from_rgb(255, 95, 95),
            led_on: Color32::from_rgb(255, 70, 50),
            led_off: Color32::from_rgb(70, 40, 40),
            background: Color32::from_rgb(22, 22, 25),
            panel: Color32::from_rgb(30, 30, 34),
            card: Color32::from_rgb(40, 40, 45),
            card_stroke: Color32::from_rgb(60, 60, 68),
            text: Color32::from_rgb(225, 225, 230),
            text_dim: Color32::from_rgb(150, 150, 160),
            control: Color32::from_rgb(58, 58, 64),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            accent: Color32::from_rgb(205, 110, 0),
            on_accent: Color32::WHITE,
            ok: Color32::from_rgb(20, 140, 50),
            warn: Color32::from_rgb(170, 120, 0),
            danger: Color32::from_rgb(200, 30, 30),
            led_on: Color32::from_rgb(230, 40, 30),
            led_off: Color32::from_rgb(200, 180, 180),
            background: Color32::from_rgb(244, 243, 240),
            panel: Color32::from_rgb(234, 232, 228),
            card: Color32::from_rgb(252, 252, 250),
            card_stroke: Color32::from_rgb(205, 202, 196),
            text: Color32::from_rgb(30, 30, 34),
            text_dim: Color32::from_rgb(105, 105, 115),
            control: Color32::from_rgb(215, 212, 206),
        }
    }

    /// The palette matching the theme egui is currently drawing with.
    pub fn of(ctx: &egui::Context) -> Self {
        if ctx.style().visuals.dark_mode { Self::dark() } else { Self::light() }
    }

    /// Stripe color of a pedal card, by what kind of effect it is.
    pub fn category(&self, category: PedalCategory) -> Color32 {
        let (r, g, b) = match category {
            PedalCategory::Dynamics => (90, 170, 230),
            PedalCategory::Drive => (235, 95, 60),
            PedalCategory::Amp => (240, 160, 50),
            PedalCategory::Filter => (190, 120, 230),
            PedalCategory::Modulation => (80, 200, 170),
            PedalCategory::Pitch => (230, 110, 170),
            PedalCategory::Time => (110, 140, 240),
            PedalCategory::Utility => (150, 150, 160),
        };
        if self.dark {
            Color32::from_rgb(r, g, b)
        } else {
            Color32::from_rgb((r as f32 * 0.8) as u8, (g as f32 * 0.8) as u8, (b as f32 * 0.8) as u8)
        }
    }

    /// Green-yellow-red for a level meter at `level` (0..1 of full scale).
    pub fn meter(&self, level: f32) -> Color32 {
        if level >= 0.98 {
            self.danger
        } else if level >= 0.7 {
            self.warn
        } else {
            self.ok
        }
    }
}

fn visuals(palette: &Palette) -> egui::Visuals {
    let mut v = if palette.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = palette.background;
    v.window_fill = palette.panel;
    v.extreme_bg_color = if palette.dark { Color32::from_rgb(16, 16, 18) } else { Color32::WHITE };
    v.faint_bg_color = palette.card;
    v.selection.bg_fill = palette.accent.gamma_multiply(if palette.dark { 0.55 } else { 0.35 });
    v.selection.stroke = Stroke::new(1.0_f32, palette.accent);
    v.hyperlink_color = palette.accent;
    v.warn_fg_color = palette.warn;
    v.error_fg_color = palette.danger;
    v.slider_trailing_fill = true;
    v.window_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.menu_corner_radius = CornerRadius::same(6);
    let radius = CornerRadius::same(5);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.widgets.inactive.weak_bg_fill = palette.control;
    v.widgets.inactive.bg_fill = palette.control;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, palette.accent.gamma_multiply(0.7));
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, palette.accent);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, palette.card_stroke);
    v
}

fn spacing(style: &mut egui::Style, touch: bool) {
    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(space::S, 6.0);
    s.button_padding = Vec2::new(space::S, 4.0);
    s.window_margin = Margin::same(space::M as i8);
    s.menu_margin = Margin::same(space::S as i8);
    s.slider_width = 140.0;
    s.combo_width = 140.0;
    s.interact_size.y = if touch { TOUCH_TARGET } else { 24.0 };
    s.icon_width = if touch { 20.0 } else { 14.0 };
    s.icon_spacing = 6.0;
    s.scroll.bar_width = if touch { 10.0 } else { 8.0 };
    for (text_style, font) in style.text_styles.iter_mut() {
        font.size = match text_style {
            egui::TextStyle::Small => text::SMALL,
            egui::TextStyle::Heading => text::HEADING,
            egui::TextStyle::Monospace => 13.0,
            _ => text::BODY,
        };
    }
}

/// What [`apply`] last set, so it only touches the context when it changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppliedStyle {
    theme: ThemePref,
    zoom: f32,
    touch: bool,
}

/// Installs the theme, spacing and zoom. Cheap to call every frame: it
/// returns early when nothing changed since `last`.
pub fn apply(ctx: &egui::Context, prefs: &UiPrefs, touch: bool, last: &mut Option<AppliedStyle>) {
    let wanted = AppliedStyle { theme: prefs.theme, zoom: prefs.zoom, touch };
    if *last == Some(wanted) {
        return;
    }
    *last = Some(wanted);
    ctx.set_visuals_of(Theme::Dark, visuals(&Palette::dark()));
    ctx.set_visuals_of(Theme::Light, visuals(&Palette::light()));
    ctx.all_styles_mut(|style| spacing(style, touch));
    ctx.set_theme(match prefs.theme {
        ThemePref::Dark => egui::ThemePreference::Dark,
        ThemePref::Light => egui::ThemePreference::Light,
        ThemePref::System => egui::ThemePreference::System,
    });
    let zoom = prefs.zoom.clamp(0.5, 3.0);
    if (ctx.zoom_factor() - zoom).abs() > 1e-3 {
        ctx.set_zoom_factor(zoom);
    }
}

/// A rounded card frame, used for pedal cards and page sections.
pub fn card_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.card)
        .stroke(Stroke::new(1.0_f32, palette.card_stroke))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(Margin::same(space::M as i8))
}

/// Lays `count` cells out in rows of fixed-width columns. egui can only
/// wrap widgets whose size it knows up front, so groups and frames need
/// this instead of `horizontal_wrapped`. `cell` gets the cell index and its
/// width.
pub fn grid_rows(ui: &mut egui::Ui, count: usize, min_cell_width: f32, mut cell: impl FnMut(&mut egui::Ui, usize, f32)) {
    let gap = space::S;
    let available = ui.available_width();
    let per_row = (((available + gap) / (min_cell_width + gap)).floor() as usize).max(1);
    let width = ((available + gap) / per_row as f32 - gap).max(min_cell_width.min(available));
    for row_start in (0..count).step_by(per_row) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for i in row_start..(row_start + per_row).min(count) {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_width(width);
                        cell(ui, i, width);
                    },
                );
            }
        });
        ui.add_space(gap / 2.0);
    }
}

/// A section heading with the accent color.
pub fn section_title(ui: &mut egui::Ui, title: &str) {
    let palette = Palette::of(ui.ctx());
    ui.label(egui::RichText::new(title).size(text::HEADING).strong().color(palette.text));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_only_changes_the_context_when_prefs_change() {
        let ctx = egui::Context::default();
        let mut last = None;
        let mut prefs = UiPrefs { theme: ThemePref::Light, ..Default::default() };
        apply(&ctx, &prefs, false, &mut last);
        assert!(!ctx.style().visuals.dark_mode);
        let applied = last;
        apply(&ctx, &prefs, false, &mut last);
        assert_eq!(last, applied);
        prefs.theme = ThemePref::Dark;
        apply(&ctx, &prefs, true, &mut last);
        assert!(ctx.style().visuals.dark_mode);
        assert_eq!(ctx.style().spacing.interact_size.y, TOUCH_TARGET);
    }

    #[test]
    fn meter_colors_go_green_yellow_red() {
        let p = Palette::dark();
        assert_eq!(p.meter(0.1), p.ok);
        assert_eq!(p.meter(0.8), p.warn);
        assert_eq!(p.meter(1.0), p.danger);
    }
}
