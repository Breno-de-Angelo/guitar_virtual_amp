//! Draws a video frame: background, caption, the app render in a window or
//! phone, the mouse cursor, fades. Uses egui itself (offscreen, through
//! egui_kittest), so the type and colors match the app's style.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, Vec2, epaint, pos2, vec2};
use egui_kittest::Harness;
use guitar_core::frontend::style::Palette;

use crate::storyboard::Layout;

/// The app part of a frame.
pub struct AppView {
    /// The whole app, rendered so that `view` maps 1:1 onto screen pixels.
    pub image: egui::ColorImage,
    /// App size, points.
    pub app_size: Vec2,
    /// Part of the app shown (zoom), points.
    pub view: Rect,
    /// Mouse pointer, app points.
    pub cursor: Option<Pos2>,
    pub pressed: bool,
    /// Last click: where (app points) and how long ago (seconds).
    pub ripple: Option<(Pos2, f32)>,
}

pub struct FrameSpec {
    pub layout: Layout,
    pub title: String,
    pub subtitle: String,
    pub bullets: Vec<String>,
    pub footnote: String,
    /// Scene time, seconds.
    pub t: f32,
    /// 0 = background only, 1 = fully visible.
    pub visibility: f32,
    pub app: Option<AppView>,
    pub watermark: String,
}

/// Where the app goes on screen, in pixels.
pub fn app_rect(layout: Layout, screen: Vec2, app_size: Vec2) -> Rect {
    let region = match layout {
        Layout::Phone => Rect::from_min_max(pos2(screen.x * 0.52, 74.0), pos2(screen.x * 0.92, screen.y - 74.0)),
        _ => Rect::from_min_max(pos2(120.0, 196.0), pos2(screen.x - 120.0, screen.y - 52.0)),
    };
    let scale = (region.width() / app_size.x).min(region.height() / app_size.y);
    Rect::from_center_size(region.center(), app_size * scale)
}

struct State {
    spec: Option<FrameSpec>,
    texture: Option<egui::TextureHandle>,
}

pub struct Compositor {
    harness: Harness<'static, State>,
}

impl Compositor {
    pub fn new(width: u32, height: u32) -> Self {
        let harness = Harness::builder()
            .with_size(vec2(width as f32, height as f32))
            .with_pixels_per_point(1.0)
            .wgpu()
            .build_state(draw, State { spec: None, texture: None });
        Self { harness }
    }

    pub fn frame(&mut self, spec: FrameSpec) -> image::RgbaImage {
        self.harness.state_mut().spec = Some(spec);
        self.harness.step();
        self.harness.render().expect("render frame")
    }
}

fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// 0 -> 1 over half a second, starting `delay` seconds into the scene.
fn appear(t: f32, delay: f32) -> f32 {
    ease((t - delay) / 0.5)
}

fn text(p: &egui::Painter, pos: Pos2, anchor: Align2, s: &str, size: f32, color: Color32, wrap: f32) -> Rect {
    let galley = p.layout(s.to_owned(), FontId::proportional(size), color, wrap);
    let rect = anchor.anchor_size(pos, galley.size());
    p.galley(rect.min, galley, color);
    rect
}

fn draw(ctx: &egui::Context, state: &mut State) {
    let Some(mut spec) = state.spec.take() else { return };
    let palette = Palette::dark();
    let p = ctx.layer_painter(egui::LayerId::background());
    let screen = ctx.screen_rect();
    background(&p, screen, &palette);

    let texture = spec.app.as_mut().map(|app| {
        let image = std::mem::replace(&mut app.image, egui::ColorImage::filled([1, 1], Color32::BLACK));
        match &mut state.texture {
            Some(t) => t.set(image, egui::TextureOptions::LINEAR),
            None => state.texture = Some(ctx.load_texture("app", image, egui::TextureOptions::LINEAR)),
        }
        state.texture.as_ref().unwrap().id()
    });

    match (spec.layout, &spec.app, texture) {
        (Layout::Desktop, Some(app), Some(tex)) => desktop(&p, &spec, app, tex, &palette),
        (Layout::Phone, Some(app), Some(tex)) => phone(&p, &spec, app, tex, &palette),
        _ => title_card(&p, &spec, &palette),
    }

    if spec.visibility < 1.0 {
        p.rect_filled(screen, 0.0, palette.background.gamma_multiply(1.0 - spec.visibility));
    }
}

fn background(p: &egui::Painter, screen: Rect, palette: &Palette) {
    let mut mesh = epaint::Mesh::default();
    let top = palette.panel;
    let bottom = Color32::from_rgb(12, 12, 14);
    mesh.colored_vertex(screen.left_top(), top);
    mesh.colored_vertex(screen.right_top(), top);
    mesh.colored_vertex(screen.right_bottom(), bottom);
    mesh.colored_vertex(screen.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    // A faint amber glow, like a pilot light, behind the top center.
    let center = pos2(screen.center().x, screen.height() * 0.3);
    let glow = palette.accent.gamma_multiply(0.07);
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(center, glow);
    let n = 64;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        mesh.colored_vertex(center + vec2(a.cos() * screen.width() * 0.5, a.sin() * screen.height() * 0.55), Color32::TRANSPARENT);
    }
    for i in 0..n {
        mesh.add_triangle(base, base + 1 + i, base + 2 + i);
    }
    p.add(mesh);
}

/// The app's knob, as a logo: 270 degree sweep, amber value arc.
fn knob(p: &egui::Painter, center: Pos2, r: f32, value: f32, palette: &Palette) {
    let angle = |v: f32| (135.0 + 270.0 * v).to_radians();
    let arc = |from: f32, to: f32, radius: f32| -> Vec<Pos2> {
        let n = 72;
        (0..=n)
            .map(|i| {
                let a = angle(from + (to - from) * i as f32 / n as f32);
                center + vec2(a.cos(), a.sin()) * radius
            })
            .collect()
    };
    p.add(epaint::Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(140) }.as_shape(
        Rect::from_center_size(center, Vec2::splat(r * 2.0)),
        CornerRadius::same(r.min(255.0) as u8),
    ));
    p.add(egui::Shape::line(arc(0.0, 1.0, r * 1.22), Stroke::new(r * 0.12, palette.control)));
    p.add(egui::Shape::line(arc(0.0, value, r * 1.22), Stroke::new(r * 0.12, palette.accent)));
    p.circle(center, r, palette.card, Stroke::new(2.0_f32, palette.card_stroke));
    let a = angle(value);
    let dir = vec2(a.cos(), a.sin());
    p.line_segment([center + dir * r * 0.25, center + dir * r * 0.8], Stroke::new(r * 0.1, palette.text));
}

fn title_card(p: &egui::Painter, spec: &FrameSpec, palette: &Palette) {
    let screen = p.clip_rect();
    let cx = screen.center().x;
    let t = spec.t;
    let turn = ease(t / 2.2);
    knob(p, pos2(cx, screen.height() * 0.3), 62.0, 0.12 + 0.68 * turn, palette);
    let a = appear(t, 0.15);
    let y = screen.height() * 0.45 + (1.0 - a) * 16.0;
    let title = text(p, pos2(cx, y), Align2::CENTER_TOP, &spec.title, 92.0, palette.text.gamma_multiply(a), f32::INFINITY);
    let a2 = appear(t, 0.45);
    let mut y = title.bottom() + 18.0;
    if !spec.subtitle.is_empty() {
        y = text(p, pos2(cx, y), Align2::CENTER_TOP, &spec.subtitle, 36.0, palette.text_dim.gamma_multiply(a2), 1500.0).bottom();
    }
    y += 40.0;
    for (i, b) in spec.bullets.iter().enumerate() {
        let a = appear(t, 0.8 + 0.18 * i as f32);
        // Links stand out in the accent color.
        let color = if b.contains("://") || b.contains(".com") { palette.accent } else { palette.text };
        y = text(p, pos2(cx, y), Align2::CENTER_TOP, b, 30.0, color.gamma_multiply(a), 1500.0).bottom() + 14.0;
    }
    if !spec.footnote.is_empty() {
        let a = appear(t, 1.2);
        text(p, pos2(cx, screen.height() - 48.0), Align2::CENTER_BOTTOM, &spec.footnote, 19.0, palette.text_dim.gamma_multiply(a), 1700.0);
    }
}

fn caption(p: &egui::Painter, pos: Pos2, spec: &FrameSpec, palette: &Palette, title_size: f32, wrap: f32) -> f32 {
    let a = appear(spec.t, 0.1);
    let dy = (1.0 - a) * 12.0;
    let title = text(p, pos + vec2(0.0, dy), Align2::LEFT_TOP, &spec.title, title_size, palette.text.gamma_multiply(a), wrap);
    p.rect_filled(
        Rect::from_min_size(pos2(pos.x - 24.0, title.top() + 6.0), vec2(6.0, title.height() - 12.0)),
        3.0,
        palette.accent.gamma_multiply(a),
    );
    let a2 = appear(spec.t, 0.35);
    let mut y = title.bottom() + 8.0;
    if !spec.subtitle.is_empty() {
        let sub = text(p, pos2(pos.x, y + (1.0 - a2) * 10.0), Align2::LEFT_TOP, &spec.subtitle, title_size * 0.54, palette.text_dim.gamma_multiply(a2), wrap);
        y = sub.bottom();
    }
    y
}

/// App points -> screen pixels.
fn to_screen(app: &AppView, display: Rect, point: Pos2) -> Pos2 {
    display.min + (point - app.view.min) * (display.width() / app.view.width())
}

fn app_image(p: &egui::Painter, app: &AppView, tex: egui::TextureId, display: Rect, radius: u8) {
    let uv = Rect::from_min_max(
        (app.view.min.to_vec2() / app.app_size).to_pos2(),
        (app.view.max.to_vec2() / app.app_size).to_pos2(),
    );
    p.add(epaint::RectShape::filled(display, radius, Color32::WHITE).with_texture(tex, uv));
}

fn pointer(p: &egui::Painter, app: &AppView, display: Rect, palette: &Palette) {
    let p = p.with_clip_rect(display);
    if let Some((at, age)) = app.ripple
        && age < 0.5
    {
        let k = age / 0.5;
        p.circle_stroke(to_screen(app, display, at), 6.0 + 26.0 * k, Stroke::new(3.0_f32, palette.accent.gamma_multiply(1.0 - k)));
    }
    let Some(cursor) = app.cursor else { return };
    let tip = to_screen(app, display, cursor);
    let s = if app.pressed { 1.35 } else { 1.5 };
    let pt = |x: f32, y: f32| tip + vec2(x, y) * s;
    let head = vec![pt(0.0, 0.0), pt(0.0, 18.0), pt(4.5, 14.0), pt(13.0, 13.0)];
    let tail = vec![pt(4.5, 14.0), pt(8.0, 21.0), pt(10.8, 19.8), pt(7.3, 13.0)];
    let outline = vec![pt(0.0, 0.0), pt(0.0, 18.0), pt(4.5, 14.0), pt(8.0, 21.0), pt(10.8, 19.8), pt(7.3, 13.0), pt(13.0, 13.0)];
    for part in [&head, &tail] {
        let shadow = part.iter().map(|q| *q + vec2(1.5, 2.5)).collect();
        p.add(egui::Shape::convex_polygon(shadow, Color32::from_black_alpha(70), Stroke::NONE));
    }
    p.add(egui::Shape::convex_polygon(head, Color32::WHITE, Stroke::NONE));
    p.add(egui::Shape::convex_polygon(tail, Color32::WHITE, Stroke::NONE));
    p.add(egui::Shape::closed_line(outline, Stroke::new(1.4_f32, Color32::BLACK)));
}

fn desktop(p: &egui::Painter, spec: &FrameSpec, app: &AppView, tex: egui::TextureId, palette: &Palette) {
    let screen = p.clip_rect();
    let display = app_rect(Layout::Desktop, screen.size(), app.app_size);
    caption(p, pos2(display.left(), 46.0), spec, palette, 50.0, display.width());
    p.add(epaint::Shadow { offset: [0, 18], blur: 56, spread: 0, color: Color32::from_black_alpha(160) }.as_shape(display, 10));
    app_image(p, app, tex, display, 10);
    p.rect_stroke(display, 10.0, Stroke::new(1.0_f32, palette.card_stroke), egui::StrokeKind::Outside);
    pointer(p, app, display, palette);
    if !spec.watermark.is_empty() {
        text(p, pos2(display.right(), 70.0), Align2::RIGHT_TOP, &spec.watermark, 20.0, palette.text_dim.gamma_multiply(0.8), f32::INFINITY);
    }
}

fn phone(p: &egui::Painter, spec: &FrameSpec, app: &AppView, tex: egui::TextureId, palette: &Palette) {
    let screen = p.clip_rect();
    let display = app_rect(Layout::Phone, screen.size(), app.app_size);
    let bezel = display.expand(16.0);
    p.add(epaint::Shadow { offset: [0, 18], blur: 56, spread: 0, color: Color32::from_black_alpha(170) }.as_shape(bezel, 50));
    p.rect(bezel, 50.0, Color32::from_rgb(10, 10, 12), Stroke::new(2.0_f32, Color32::from_rgb(62, 62, 70)), egui::StrokeKind::Inside);
    app_image(p, app, tex, display, 34);
    pointer(p, app, display, palette);
    let left = 170.0;
    let wrap = bezel.left() - left - 110.0;
    let mut y = caption(p, pos2(left, screen.height() * 0.33), spec, palette, 62.0, wrap) + 34.0;
    for (i, b) in spec.bullets.iter().enumerate() {
        let a = appear(spec.t, 0.7 + 0.18 * i as f32);
        p.circle_filled(pos2(left + 7.0, y + 17.0), 5.0, palette.accent.gamma_multiply(a));
        y = text(p, pos2(left + 28.0, y), Align2::LEFT_TOP, b, 29.0, palette.text.gamma_multiply(a), wrap - 28.0).bottom() + 14.0;
    }
}
