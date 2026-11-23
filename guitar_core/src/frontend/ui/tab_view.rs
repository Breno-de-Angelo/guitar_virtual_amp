//! Tablature renderer: lays bars out in wrapped rows and paints them with
//! `egui::Painter`, with a playback cursor and click-to-seek.

use eframe::egui::{
    self, Align, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2,
};

use std::collections::HashMap;

use crate::frontend::lib::practice::NoteKey;
use crate::shared::tab::model::{Score, ScoreTrack, TabBeat, TabNote, TICKS_PER_QUARTER};

const LINE_SPACING: f32 = 13.0;
/// Space above the top string for bar numbers, markers and P.M. labels.
const ROW_TOP: f32 = 38.0;
/// Space below the bottom string for rhythm stems.
const ROW_BOTTOM: f32 = 30.0;
const ROW_GAP: f32 = 10.0;
const BAR_PAD_LEFT: f32 = 12.0;
const BAR_PAD_RIGHT: f32 = 8.0;
const EMPTY_BAR_WIDTH: f32 = 70.0;
const MARGIN_X: f32 = 8.0;

/// Width given to a beat, growing sub-linearly with its duration so short
/// notes stay readable without whole notes taking half a row.
fn beat_width(duration_ticks: i64) -> f32 {
    let quarters = duration_ticks as f32 / TICKS_PER_QUARTER as f32;
    22.0 + 22.0 * quarters.sqrt().min(2.0)
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarLayout {
    pub bar: usize,
    pub row: usize,
    /// Left edge, relative to the row's left edge.
    pub x: f32,
    pub width: f32,
    /// `(tick, x relative to bar start)` for each main-voice beat.
    pub beats: Vec<(i64, f32)>,
    pub length_ticks: i64,
}

impl BarLayout {
    /// Horizontal position of `tick` inside the bar, interpolating between
    /// main-voice beats (relative to the bar start).
    pub fn tick_x(&self, tick: f64) -> f32 {
        let end_x = self.width - BAR_PAD_RIGHT;
        if self.beats.is_empty() {
            let t = (tick / self.length_ticks.max(1) as f64).clamp(0.0, 1.0) as f32;
            return BAR_PAD_LEFT + (end_x - BAR_PAD_LEFT) * t;
        }
        let idx = self.beats.partition_point(|(t, _)| (*t as f64) <= tick);
        if idx == 0 {
            return self.beats[0].1;
        }
        let (t0, x0) = self.beats[idx - 1];
        let (t1, x1) = self.beats.get(idx).copied().unwrap_or((self.length_ticks, end_x));
        let span = (t1 - t0).max(1) as f64;
        let frac = ((tick - t0 as f64) / span).clamp(0.0, 1.0) as f32;
        x0 + (x1 - x0) * frac
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabLayout {
    pub bars: Vec<BarLayout>,
    /// Index range into `bars` for each row.
    pub rows: Vec<std::ops::Range<usize>>,
    pub row_height: f32,
    pub width: f32,
}

impl TabLayout {
    pub fn build(score: &Score, track: &ScoreTrack, width: f32) -> Self {
        let strings = track.string_count();
        let row_height = ROW_TOP + (strings.saturating_sub(1)) as f32 * LINE_SPACING + ROW_BOTTOM;
        let usable = (width - 2.0 * MARGIN_X).max(200.0);

        // Natural (unstretched) layout of each bar.
        let mut bars: Vec<BarLayout> = score
            .bars
            .iter()
            .enumerate()
            .map(|(i, master)| {
                let main_voice = track.bars.get(i).and_then(|b| b.voices.first());
                let mut x = BAR_PAD_LEFT;
                let mut beats = Vec::new();
                for beat in main_voice.into_iter().flatten() {
                    beats.push((beat.start, x + 6.0));
                    x += beat_width(beat.duration);
                }
                let width = if beats.is_empty() { EMPTY_BAR_WIDTH } else { x + BAR_PAD_RIGHT };
                BarLayout {
                    bar: i,
                    row: 0,
                    x: 0.0,
                    width: width.min(usable),
                    beats,
                    length_ticks: master.length_ticks(),
                }
            })
            .collect();

        // Greedy line breaking, then stretch every full row to the width.
        let mut rows = Vec::new();
        let mut start = 0;
        let mut row_width = 0.0;
        for (i, bar) in bars.iter().enumerate() {
            if i > start && row_width + bar.width > usable {
                rows.push(start..i);
                start = i;
                row_width = 0.0;
            }
            row_width += bar.width;
        }
        if start < bars.len() {
            rows.push(start..bars.len());
        }
        let row_count = rows.len();
        for (row_index, range) in rows.iter().enumerate() {
            let natural: f32 = bars[range.clone()].iter().map(|b| b.width).sum();
            let is_last = row_index + 1 == row_count;
            let stretch = if is_last && natural < usable * 0.75 { 1.0 } else { usable / natural };
            let mut x = 0.0;
            for bar in &mut bars[range.clone()] {
                bar.row = row_index;
                bar.x = x;
                bar.width *= stretch;
                for beat in &mut bar.beats {
                    beat.1 *= stretch;
                }
                x += bar.width;
            }
        }
        TabLayout { bars, rows, row_height, width }
    }

    fn row_top(&self, row: usize) -> f32 {
        row as f32 * (self.row_height + ROW_GAP)
    }

    pub fn total_height(&self) -> f32 {
        self.row_top(self.rows.len()) + ROW_GAP
    }
}

/// Where the playback cursor is: a bar and a (fractional) tick inside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorPos {
    pub bar: usize,
    pub tick: f64,
}

pub struct TabViewResponse {
    /// Bar the user clicked, if any.
    pub clicked_bar: Option<usize>,
}

struct Palette {
    line: Color32,
    text: Color32,
    dim: Color32,
    accent: Color32,
    cursor: Color32,
    bar_highlight: Color32,
    note_bg: Color32,
    hit: Color32,
    miss: Color32,
}

impl Palette {
    fn from_ui(ui: &Ui) -> Self {
        let v = ui.visuals();
        let dark = v.dark_mode;
        Palette {
            line: if dark { Color32::from_gray(110) } else { Color32::from_gray(150) },
            text: v.strong_text_color(),
            dim: v.weak_text_color(),
            accent: if dark { Color32::from_rgb(255, 170, 60) } else { Color32::from_rgb(200, 100, 0) },
            cursor: Color32::from_rgb(230, 60, 60),
            bar_highlight: if dark {
                Color32::from_rgba_unmultiplied(255, 200, 80, 22)
            } else {
                Color32::from_rgba_unmultiplied(255, 170, 0, 40)
            },
            note_bg: v.extreme_bg_color,
            hit: if dark { Color32::from_rgb(90, 210, 110) } else { Color32::from_rgb(20, 140, 50) },
            miss: if dark { Color32::from_rgb(255, 95, 95) } else { Color32::from_rgb(200, 30, 30) },
        }
    }
}

fn note_label(note: &TabNote, percussion: bool) -> String {
    if note.dead {
        return "x".to_string();
    }
    let fret = note.fret.to_string();
    if percussion {
        fret
    } else if note.harmonic {
        format!("<{fret}>")
    } else if note.ghost || note.tie {
        format!("({fret})")
    } else {
        fret
    }
}

fn note_suffix(note: &TabNote) -> &'static str {
    if note.bend {
        "b"
    } else if note.slide {
        "/"
    } else if note.hammer {
        "h"
    } else if note.vibrato {
        "~"
    } else {
        ""
    }
}

/// Practice-mode results: `true` = played correctly. Notes without an entry
/// haven't been judged and keep their normal color.
pub type NoteMarks = HashMap<NoteKey, bool>;

/// Draws the whole tab for `track` inside a vertical scroll area.
/// `follow_row` scrolls the row containing the cursor into view.
pub fn tab_view(
    ui: &mut Ui,
    score: &Score,
    track: &ScoreTrack,
    layout: &TabLayout,
    cursor: Option<CursorPos>,
    follow_row: bool,
    marks: Option<&NoteMarks>,
) -> TabViewResponse {
    let palette = Palette::from_ui(ui);
    let mut clicked_bar = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("tab_scroll")
        .show_viewport(ui, |ui, viewport| {
            let (rect, response) = ui.allocate_exact_size(
                vec2(layout.width.max(ui.available_width()), layout.total_height()),
                Sense::click(),
            );
            let origin = rect.min + vec2(MARGIN_X, ROW_GAP);
            let painter = ui.painter_at(rect);
            let strings = track.string_count();
            let staff_height = (strings.saturating_sub(1)) as f32 * LINE_SPACING;

            let cursor_bar = cursor.and_then(|c| layout.bars.get(c.bar).map(|b| (c, b)));
            if follow_row && let Some((_, bar)) = cursor_bar {
                let top = origin.y + layout.row_top(bar.row);
                let row_rect = Rect::from_min_size(
                    pos2(rect.min.x, top),
                    vec2(rect.width(), layout.row_height),
                );
                ui.scroll_to_rect(row_rect, Some(Align::Center));
            }

            for (row_index, range) in layout.rows.iter().enumerate() {
                let row_top = layout.row_top(row_index);
                // `viewport` is in content coordinates (relative to `rect`).
                let visible_top = viewport.min.y - layout.row_height;
                if row_top + ROW_GAP < visible_top || row_top > viewport.max.y {
                    continue;
                }
                let staff_top = origin.y + row_top + ROW_TOP;
                let row_bars = &layout.bars[range.clone()];
                let x_start = origin.x + row_bars.first().map(|b| b.x).unwrap_or(0.0);
                let x_end = origin.x + row_bars.last().map(|b| b.x + b.width).unwrap_or(0.0);

                for bar_layout in row_bars {
                    let bar_x = origin.x + bar_layout.x;
                    if let Some((_, cb)) = cursor_bar
                        && cb.bar == bar_layout.bar
                    {
                        painter.rect_filled(
                            Rect::from_min_max(
                                pos2(bar_x, staff_top - 8.0),
                                pos2(bar_x + bar_layout.width, staff_top + staff_height + 8.0),
                            ),
                            2.0,
                            palette.bar_highlight,
                        );
                    }
                }

                for s in 0..strings {
                    let y = staff_top + s as f32 * LINE_SPACING;
                    painter.line_segment([pos2(x_start, y), pos2(x_end, y)], Stroke::new(1.0_f32, palette.line));
                }

                for bar_layout in row_bars {
                    paint_bar(
                        &painter,
                        &palette,
                        score,
                        track,
                        bar_layout,
                        pos2(origin.x + bar_layout.x, staff_top),
                        staff_height,
                        marks,
                    );
                }
                // Closing line of the row.
                painter.line_segment(
                    [pos2(x_end, staff_top), pos2(x_end, staff_top + staff_height)],
                    Stroke::new(1.0_f32, palette.line),
                );
            }

            if let Some((c, bar)) = cursor_bar {
                let x = origin.x + bar.x + bar.tick_x(c.tick);
                let top = origin.y + layout.row_top(bar.row) + ROW_TOP - 10.0;
                painter.line_segment(
                    [pos2(x, top), pos2(x, top + staff_height + 20.0)],
                    Stroke::new(2.0_f32, palette.cursor),
                );
            }

            if response.clicked()
                && let Some(pos) = response.interact_pointer_pos()
            {
                clicked_bar = hit_test(layout, pos - origin.to_vec2());
            }
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        });
    TabViewResponse { clicked_bar }
}

fn hit_test(layout: &TabLayout, local: Pos2) -> Option<usize> {
    let row_stride = layout.row_height + ROW_GAP;
    if local.y < 0.0 {
        return None;
    }
    let row = (local.y / row_stride) as usize;
    let range = layout.rows.get(row)?;
    layout.bars[range.clone()]
        .iter()
        .find(|b| local.x >= b.x && local.x < b.x + b.width)
        .map(|b| b.bar)
}

#[allow(clippy::too_many_arguments)]
fn paint_bar(
    painter: &egui::Painter,
    palette: &Palette,
    score: &Score,
    track: &ScoreTrack,
    layout: &BarLayout,
    staff_origin: Pos2,
    staff_height: f32,
    marks: Option<&NoteMarks>,
) {
    let master = &score.bars[layout.bar];
    let x = staff_origin.x;
    let top = staff_origin.y;
    let bottom = top + staff_height;
    let small = FontId::proportional(10.0);
    let fret_font = FontId::monospace(11.0);
    let line = Stroke::new(1.0_f32, palette.line);

    // Bar line + number.
    painter.line_segment([pos2(x, top), pos2(x, bottom)], line);
    painter.text(pos2(x + 2.0, top - 14.0), Align2::LEFT_BOTTOM, (layout.bar + 1).to_string(), small.clone(), palette.dim);

    let mut header_x = x + 18.0;
    if let Some(marker) = &master.marker {
        let r = painter.text(pos2(header_x, top - 14.0), Align2::LEFT_BOTTOM, marker, FontId::proportional(12.0), palette.accent);
        header_x = r.right() + 8.0;
    }
    if master.alternatives != 0 {
        let endings: Vec<String> = (0..8)
            .filter(|i| master.alternatives & (1 << i) != 0)
            .map(|i| format!("{}.", i + 1))
            .collect();
        painter.text(pos2(header_x, top - 14.0), Align2::LEFT_BOTTOM, endings.join(" "), small.clone(), palette.text);
        painter.line_segment([pos2(x + 2.0, top - 26.0), pos2(x + layout.width - 4.0, top - 26.0)], line);
    }
    if master.repeat_open {
        painter.line_segment([pos2(x + 3.0, top), pos2(x + 3.0, bottom)], Stroke::new(3.0_f32, palette.text));
        let mid = (top + bottom) / 2.0;
        painter.circle_filled(pos2(x + 8.0, mid - 6.0), 1.8, palette.text);
        painter.circle_filled(pos2(x + 8.0, mid + 6.0), 1.8, palette.text);
    }
    if master.repeat_jumps > 0 {
        let rx = x + layout.width - 3.0;
        painter.line_segment([pos2(rx, top), pos2(rx, bottom)], Stroke::new(3.0_f32, palette.text));
        let mid = (top + bottom) / 2.0;
        painter.circle_filled(pos2(rx - 5.0, mid - 6.0), 1.8, palette.text);
        painter.circle_filled(pos2(rx - 5.0, mid + 6.0), 1.8, palette.text);
        painter.text(pos2(rx, top - 14.0), Align2::RIGHT_BOTTOM, format!("x{}", master.repeat_jumps + 1), small.clone(), palette.text);
    }

    let Some(track_bar) = track.bars.get(layout.bar) else { return };
    for (voice_index, voice) in track_bar.voices.iter().enumerate() {
        let voice_color = if voice_index == 0 { palette.text } else { palette.dim };
        for (beat_index, beat) in voice.iter().enumerate() {
            let bx = x + layout.tick_x(beat.start as f64);
            for (note_index, note) in beat.notes.iter().enumerate() {
                let key = NoteKey {
                    bar: layout.bar,
                    voice: voice_index as u8,
                    beat: beat_index as u16,
                    note: note_index as u8,
                };
                let mark = marks.and_then(|m| m.get(&key)).copied();
                let color = match mark {
                    Some(true) => palette.hit,
                    Some(false) => palette.miss,
                    None => voice_color,
                };
                let string = (note.string as usize).clamp(1, track.string_count());
                let y = top + (string - 1) as f32 * LINE_SPACING;
                let label = note_label(note, track.percussion);
                let galley = painter.layout_no_wrap(label, fret_font.clone(), color);
                let rect = Rect::from_center_size(pos2(bx, y), galley.size() + vec2(2.0, -2.0));
                painter.rect_filled(rect, 1.0, palette.note_bg);
                if mark.is_some() {
                    painter.rect_stroke(rect.expand(1.0), 2.0, Stroke::new(1.0_f32, color), egui::StrokeKind::Outside);
                }
                painter.galley(rect.min + vec2(1.0, -1.0), galley, color);
                let suffix = note_suffix(note);
                if !suffix.is_empty() {
                    painter.text(pos2(rect.right() + 1.0, y), Align2::LEFT_CENTER, suffix, small.clone(), palette.dim);
                }
            }
            if voice_index == 0 {
                paint_beat_annotations(painter, palette, beat, bx, top, bottom);
            }
        }
    }
}

fn paint_beat_annotations(
    painter: &egui::Painter,
    palette: &Palette,
    beat: &TabBeat,
    x: f32,
    top: f32,
    bottom: f32,
) {
    let small = FontId::proportional(9.0);
    let mut label_y = top - 3.0;
    if beat.palm_mute {
        painter.text(pos2(x, label_y), Align2::CENTER_BOTTOM, "P.M.", small.clone(), palette.dim);
        label_y -= 10.0;
    }
    if beat.let_ring {
        painter.text(pos2(x, label_y), Align2::CENTER_BOTTOM, "L.R.", small.clone(), palette.dim);
    }
    if !beat.text.is_empty() {
        painter.text(pos2(x, top - 26.0), Align2::CENTER_BOTTOM, &beat.text, small.clone(), palette.accent);
    }

    // Rhythm notation below the staff: stems, flags, dots, tuplets.
    let stem_top = bottom + 7.0;
    let stem_bottom = bottom + 20.0;
    let stroke = Stroke::new(1.0_f32, palette.dim);
    if beat.is_rest() {
        let w = if beat.value <= 2 { 6.0 } else { 3.0 };
        painter.rect_filled(Rect::from_center_size(pos2(x, stem_top + 5.0), vec2(w, 3.0)), 0.0, palette.dim);
    } else if beat.value >= 2 {
        painter.line_segment([pos2(x, stem_top), pos2(x, stem_bottom)], stroke);
        let flags = match beat.value {
            8 => 1,
            16 => 2,
            32 => 3,
            v if v >= 64 => 4,
            _ => 0,
        };
        for f in 0..flags {
            let y = stem_bottom - f as f32 * 3.0;
            painter.line_segment([pos2(x, y), pos2(x + 6.0, y - 2.0)], stroke);
        }
    } else {
        // Whole note: no stem, a small hollow mark.
        painter.circle_stroke(pos2(x, stem_top + 5.0), 2.5, stroke);
    }
    if beat.dotted {
        painter.circle_filled(pos2(x + 4.0, stem_bottom - 1.0), 1.2, palette.dim);
    }
    if let Some(n) = beat.tuplet {
        painter.text(pos2(x, stem_bottom + 1.0), Align2::CENTER_TOP, n.to_string(), small, palette.dim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::tab::model::{MasterBar, TrackBar};

    fn score_with(bar_beats: Vec<Vec<TabBeat>>) -> (Score, ScoreTrack) {
        let bars = bar_beats
            .iter()
            .map(|_| MasterBar { numerator: 4, denominator: 4, ..Default::default() })
            .collect();
        let track = ScoreTrack {
            tuning: vec![64, 59, 55, 50, 45, 40],
            bars: bar_beats.into_iter().map(|b| TrackBar { voices: vec![b] }).collect(),
            ..Default::default()
        };
        (Score { bars, tracks: vec![track.clone()], ..Default::default() }, track)
    }

    fn quarters() -> Vec<TabBeat> {
        (0..4)
            .map(|i| TabBeat { start: i * 960, duration: 960, value: 4, ..Default::default() })
            .collect()
    }

    #[test]
    fn bars_wrap_into_rows_that_fill_the_width() {
        let (score, track) = score_with((0..20).map(|_| quarters()).collect());
        let layout = TabLayout::build(&score, &track, 800.0);
        assert!(layout.rows.len() > 1);
        let usable = 800.0 - 2.0 * MARGIN_X;
        for range in &layout.rows[..layout.rows.len() - 1] {
            let w: f32 = layout.bars[range.clone()].iter().map(|b| b.width).sum();
            assert!((w - usable).abs() < 0.5, "row width {w}");
        }
        // Rows are contiguous and cover every bar exactly once.
        let covered: usize = layout.rows.iter().map(|r| r.len()).sum();
        assert_eq!(covered, 20);
    }

    #[test]
    fn tick_x_is_monotonic_inside_a_bar() {
        let (score, track) = score_with(vec![quarters()]);
        let layout = TabLayout::build(&score, &track, 1000.0);
        let bar = &layout.bars[0];
        let mut last = f32::MIN;
        for tick in (0..3840).step_by(120) {
            let x = bar.tick_x(tick as f64);
            assert!(x >= last);
            last = x;
        }
        assert!(last < bar.width);
    }

    #[test]
    fn hit_test_finds_clicked_bar() {
        let (score, track) = score_with((0..20).map(|_| quarters()).collect());
        let layout = TabLayout::build(&score, &track, 800.0);
        let second_row = &layout.bars[layout.rows[1].start];
        let y = layout.row_top(1) + 40.0;
        let hit = hit_test(&layout, pos2(second_row.x + 5.0, y));
        assert_eq!(hit, Some(second_row.bar));
    }

    #[test]
    fn labels_reflect_note_kind() {
        let n = TabNote { fret: 7, ..Default::default() };
        assert_eq!(note_label(&n, false), "7");
        assert_eq!(note_label(&TabNote { dead: true, ..n.clone() }, false), "x");
        assert_eq!(note_label(&TabNote { tie: true, ..n.clone() }, false), "(7)");
        assert_eq!(note_label(&TabNote { harmonic: true, ..n.clone() }, false), "<7>");
    }
}
