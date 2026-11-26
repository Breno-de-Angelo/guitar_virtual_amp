//! The "Pedalboard" page: the chain as a row of stompbox cards (wrapping on
//! narrow screens), an "add pedal" menu grouped by effect family, drag and
//! drop reordering, and undo/redo of every edit.

use eframe::egui::{self, RichText, Sense, Ui};
use strum::IntoEnumIterator;

use crate::frontend::lib::history::{ChainHistory, classify_edit};
use crate::frontend::style::{self, Palette};
use crate::frontend::ui::pedals::{PedalAction, pedal_card};
use crate::frontend::ui::tone3000::Tone3000Request;
use crate::shared::i18n::tr;
use crate::shared::pedals::{PedalCategory, PedalDescription, PedalInstance};

#[derive(Default)]
pub struct PedalboardPage {
    pub history: ChainHistory,
    /// Undo/redo changed the chain this frame: don't record that as an edit.
    navigated: bool,
    confirm_clear: bool,
    /// The other chain of an A/B comparison, and whether the live chain is B.
    compare: Option<(Vec<PedalInstance>, bool)>,
    row_heights: RowHeights,
}

/// The tallest card body of each card row, measured last frame, so every
/// card in a row is drawn that tall with its footswitch at the bottom.
/// egui only learns a card's height while drawing it, hence last frame's.
/// Cards report their unpadded height, so the rows shrink again when a tall
/// pedal goes away.
#[derive(Default, Debug, PartialEq)]
struct RowHeights {
    /// Cards per row when the heights were measured.
    per_row: usize,
    rows: Vec<f32>,
}

impl RowHeights {
    /// The body height to draw the cards of `row` with.
    fn target(&self, per_row: usize, row: usize) -> f32 {
        if per_row == self.per_row { self.rows.get(row).copied().unwrap_or(0.0) } else { 0.0 }
    }

    /// Stores this frame's tallest body per row. Returns whether the cards
    /// were drawn with different heights (then the frame should be redrawn).
    fn update(&mut self, per_row: usize, rows: Vec<f32>) -> bool {
        let changed = per_row != self.per_row
            || rows.len() != self.rows.len()
            || rows.iter().zip(&self.rows).any(|(a, b)| (a - b).abs() > 0.5);
        if changed {
            *self = Self { per_row, rows };
        }
        changed
    }
}

#[derive(Default)]
pub struct BoardOutput {
    /// The chain changed and must be sent to the audio thread.
    pub chain_changed: bool,
    pub tone3000: Option<(usize, Tone3000Request)>,
}

/// Appends `pedal` where it usually sits in a signal chain: after the last
/// pedal of the same or an earlier family (so a reverb lands after the
/// drive, a compressor before it).
pub fn insert_in_signal_order(chain: &mut Vec<PedalInstance>, pedal: PedalDescription) -> usize {
    let order = |c: PedalCategory| PedalCategory::iter().position(|x| x == c).unwrap_or(0);
    let rank = order(pedal.category());
    let at = chain
        .iter()
        .rposition(|p| order(p.description.category()) <= rank)
        .map(|i| i + 1)
        .unwrap_or(0);
    chain.insert(at, PedalInstance::new(pedal));
    at
}

/// The "Add pedal" menu contents, in two columns of effect families.
/// Returns the pedal picked.
pub fn add_pedal_menu(ui: &mut Ui) -> Option<PedalDescription> {
    let palette = Palette::of(ui.ctx());
    let mut picked = None;
    let categories: Vec<PedalCategory> = PedalCategory::iter().collect();
    let (left, right) = categories.split_at(categories.len() / 2);
    ui.horizontal_top(|ui| {
        for column in [left, right] {
            ui.vertical(|ui| {
                ui.set_min_width(170.0);
                for &category in column {
                    ui.label(RichText::new(category.label()).small().strong().color(palette.category(category)));
                    for pedal in PedalDescription::iter().filter(|p| p.category() == category) {
                        if ui.button(pedal.name()).on_hover_text(pedal.blurb()).clicked() {
                            picked = Some(pedal);
                            ui.close();
                        }
                    }
                    ui.add_space(4.0);
                }
            });
        }
    });
    picked
}

impl PedalboardPage {
    /// Call once per frame with the chain as it was when the frame started:
    /// records whatever changed it (this page, presets, MIDI, shortcuts,
    /// tone suggestions) as one undoable edit.
    pub fn end_frame(&mut self, before: &[PedalInstance], after: &[PedalInstance]) {
        if std::mem::take(&mut self.navigated) {
            return;
        }
        if let Some(kind) = classify_edit(before, after) {
            self.history.record(before, kind);
        }
    }

    pub fn undo(&mut self, chain: &mut Vec<PedalInstance>) -> bool {
        match self.history.undo(chain) {
            Some(previous) => {
                *chain = previous;
                self.navigated = true;
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self, chain: &mut Vec<PedalInstance>) -> bool {
        match self.history.redo(chain) {
            Some(next) => {
                *chain = next;
                self.navigated = true;
                true
            }
            None => false,
        }
    }

    /// Swaps the live chain with the A/B snapshot. Returns whether it did.
    pub fn swap_compare(&mut self, chain: &mut Vec<PedalInstance>) -> bool {
        match &mut self.compare {
            Some((other, on_b)) => {
                std::mem::swap(chain, other);
                *on_b = !*on_b;
                true
            }
            None => false,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, chain: &mut Vec<PedalInstance>) -> BoardOutput {
        let before = chain.clone();
        let mut out = BoardOutput::default();

        // Toolbar. Phones get the less used actions in a "…" menu so it
        // fits on one row.
        let narrow = ui.ctx().screen_rect().width() < style::NARROW_WIDTH;
        ui.horizontal_wrapped(|ui| {
            let add = ui.menu_button(RichText::new(tr("➕ Add pedal", "➕ Adicionar pedal")).strong(), add_pedal_menu);
            if let Some(Some(pedal)) = add.inner {
                insert_in_signal_order(chain, pedal);
            }
            ui.separator();
            if ui.add_enabled(self.history.can_undo(), egui::Button::new(tr("⟲ Undo", "⟲ Desfazer"))).on_hover_text("Ctrl+Z").clicked() {
                self.undo(chain);
            }
            if ui.add_enabled(self.history.can_redo(), egui::Button::new(tr("⟳ Redo", "⟳ Refazer"))).on_hover_text("Ctrl+Shift+Z / Ctrl+Y").clicked() {
                self.redo(chain);
            }
            if narrow {
                ui.menu_button("…", |ui| self.more_menu(ui, chain))
                    .response
                    .on_hover_text(tr("More actions", "Mais ações"));
                return;
            }
            ui.separator();
            bypass_all_button(ui, chain);
            if !chain.is_empty() {
                if self.confirm_clear {
                    self.confirm_clear_ui(ui, chain);
                } else if ui.button(tr("Clear", "Limpar"))
                    .on_hover_text(tr("Remove every pedal (undoable)", "Remover todos os pedais (dá para desfazer)")).clicked() {
                    self.confirm_clear = true;
                }
            }
            ui.separator();
            if self.compare.is_some() {
                self.compare_ui(ui, chain, false);
            } else {
                self.start_compare_button(ui, chain);
            }
        });
        if narrow {
            // States the "…" menu leads to stay on screen below the toolbar.
            if self.confirm_clear && !chain.is_empty() {
                ui.horizontal_wrapped(|ui| self.confirm_clear_ui(ui, chain));
            }
            if self.compare.is_some() {
                ui.horizontal_wrapped(|ui| self.compare_ui(ui, chain, true));
            }
        }
        signal_flow(ui, chain);
        ui.add_space(style::space::S);

        if chain.is_empty() {
            empty_state(ui, chain);
        } else {
            self.cards(ui, chain, &mut out);
        }

        out.chain_changed |= before != *chain;
        out
    }

    /// The phone toolbar's "…" menu: bypass all, clear (confirmed below
    /// the toolbar) and A/B.
    fn more_menu(&mut self, ui: &mut Ui, chain: &mut [PedalInstance]) {
        ui.set_min_width(190.0);
        bypass_all_button(ui, chain);
        if !chain.is_empty()
            && ui.button(tr("Clear", "Limpar"))
                .on_hover_text(tr("Remove every pedal (undoable)", "Remover todos os pedais (dá para desfazer)"))
                .clicked()
        {
            self.confirm_clear = true;
        }
        if self.compare.is_some() {
            if ui.button(tr("✖ Stop comparing", "✖ Parar de comparar")).clicked() {
                self.compare = None;
            }
        } else {
            self.start_compare_button(ui, chain);
        }
    }

    /// "Remove every pedal? Remove / Cancel".
    fn confirm_clear_ui(&mut self, ui: &mut Ui, chain: &mut Vec<PedalInstance>) {
        let palette = Palette::of(ui.ctx());
        ui.label(tr("Remove every pedal?", "Remover todos os pedais?"));
        if ui.button(RichText::new(tr("Remove", "Remover")).color(palette.danger)).clicked() {
            chain.clear();
            self.confirm_clear = false;
        }
        if ui.button(tr("Cancel", "Cancelar")).clicked() {
            self.confirm_clear = false;
        }
    }

    fn start_compare_button(&mut self, ui: &mut Ui, chain: &[PedalInstance]) {
        if ui
            .button("📌 A/B")
            .on_hover_text(tr(
                "Keep this sound as A, then edit freely and flip between A and B to compare",
                "Guardar este som como A, depois editar à vontade e alternar entre A e B para comparar",
            ))
            .clicked()
        {
            self.compare = Some((chain.to_vec(), true));
        }
    }

    /// While comparing: flip between A and B, or stop. `narrow` makes the
    /// stop button touch-sized.
    fn compare_ui(&mut self, ui: &mut Ui, chain: &mut Vec<PedalInstance>, narrow: bool) {
        let palette = Palette::of(ui.ctx());
        let Some((_, on_b)) = &self.compare else { return };
        let label = if *on_b { tr("Hearing B 🔁 A", "Ouvindo B 🔁 A") } else { tr("Hearing A 🔁 B", "Ouvindo A 🔁 B") };
        if ui
            .add(egui::Button::new(RichText::new(label).color(palette.accent)))
            .on_hover_text(tr("Flip between the two sounds", "Alternar entre os dois sons"))
            .clicked()
        {
            self.swap_compare(chain);
        }
        let stop = if narrow { ui.button("✖") } else { ui.small_button("✖") };
        if stop.on_hover_text(tr("Stop comparing (keep what you hear now)", "Parar de comparar (manter o som atual)")).clicked() {
            self.compare = None;
        }
    }

    fn cards(&mut self, ui: &mut Ui, chain: &mut Vec<PedalInstance>, out: &mut BoardOutput) {
        let palette = Palette::of(ui.ctx());
        let count = chain.len();
        let mut actions = Vec::new();
        let mut moved: Option<(usize, usize)> = None;
        egui::ScrollArea::vertical().id_salt("pedalboard_scroll").auto_shrink([false, false]).show(ui, |ui| {
            // Rows of cards. egui can't wrap widgets whose size it only
            // learns while drawing them, so the rows are laid out here.
            let gap = style::space::M;
            let available = ui.available_width();
            let per_row = (((available + gap) / (style::PEDAL_CARD_WIDTH + gap)).floor() as usize).max(1);
            let width = if per_row == 1 { available.min(520.0) } else { style::PEDAL_CARD_WIDTH };
            // Phones show one card per row: nothing to line up.
            let align = per_row > 1;
            let mut tallest = Vec::new();
            for (row, row_start) in (0..count).step_by(per_row).enumerate() {
                let body_height = if align { self.row_heights.target(per_row, row) } else { 0.0 };
                let mut row_tallest = 0.0_f32;
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    let row_end = (row_start + per_row).min(count);
                    for (i, pedal) in chain.iter_mut().enumerate().take(row_end).skip(row_start) {
                        let card =
                            ui.push_id(("pedal_card", i), |ui| pedal_card(ui, i, count, pedal, width, body_height)).inner;
                        row_tallest = row_tallest.max(card.body_height);
                        card.handle.dnd_set_drag_payload(i);
                        let zone = ui.interact(card.rect, ui.id().with(("pedal_drop", i)), Sense::hover());
                        if let Some(from) = zone.dnd_hover_payload::<usize>()
                            && *from != i
                        {
                            let x = if *from < i { card.rect.right() + 5.0 } else { card.rect.left() - 5.0 };
                            ui.painter().vline(x, card.rect.y_range(), egui::Stroke::new(3.0_f32, palette.accent));
                        }
                        if let Some(from) = zone.dnd_release_payload::<usize>() {
                            moved = Some((*from, i));
                        }
                        if card.action != PedalAction::None {
                            actions.push((i, card.action));
                        }
                    }
                });
                tallest.push(row_tallest);
                ui.add_space(gap);
            }
            if !align {
                tallest.clear();
            }
            if self.row_heights.update(per_row, tallest) {
                // Redraw now with the new heights instead of showing a
                // frame of misaligned cards.
                ui.ctx().request_discard("pedal card row heights changed");
                ui.ctx().request_repaint();
            }
            ui.add_space(style::space::XL);
        });

        if let Some((from, to)) = moved
            && from != to
            && from < chain.len()
        {
            let pedal = chain.remove(from);
            chain.insert(to.min(chain.len()), pedal);
            return;
        }
        // One structural action per frame is all a user can do; apply the
        // first so indices stay valid.
        for (i, action) in actions {
            match action {
                PedalAction::None | PedalAction::Updated => {}
                PedalAction::Deleted => {
                    chain.remove(i);
                    return;
                }
                PedalAction::Duplicate => {
                    let copy = chain[i].clone();
                    chain.insert(i + 1, copy);
                    return;
                }
                PedalAction::Move(by) => {
                    let to = (i as isize + by).clamp(0, chain.len() as isize - 1) as usize;
                    let pedal = chain.remove(i);
                    chain.insert(to, pedal);
                    return;
                }
                PedalAction::Tone3000(request) => out.tone3000 = Some((i, request)),
            }
        }
    }
}

/// "Bypass all" while any pedal is on, else "Enable all". Hidden for an
/// empty chain.
fn bypass_all_button(ui: &mut Ui, chain: &mut [PedalInstance]) {
    if chain.is_empty() {
        return;
    }
    let any_on = chain.iter().any(|p| p.enabled);
    if ui
        .button(if any_on { tr("Bypass all", "Desligar todos") } else { tr("Enable all", "Ligar todos") })
        .on_hover_text(tr(
            "Hear the dry guitar / bring every pedal back",
            "Ouvir a guitarra sem efeitos / religar todos os pedais",
        ))
        .clicked()
    {
        for pedal in chain.iter_mut() {
            pedal.enabled = !any_on;
        }
    }
}

/// "🎸 → Comp → Drive → Amp → 🔊": the chain at a glance. Clicking a chip
/// toggles that pedal's bypass.
fn signal_flow(ui: &mut Ui, chain: &mut [PedalInstance]) {
    let palette = Palette::of(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new("🎸 IN").color(palette.text_dim));
        for pedal in chain.iter_mut() {
            ui.label(RichText::new("⏵").color(palette.text_dim));
            let color = palette.category(pedal.description.category());
            let text = RichText::new(pedal.description.name()).small();
            let text = if pedal.enabled { text.color(color).strong() } else { text.color(palette.text_dim).strikethrough() };
            if ui
                .add(egui::Button::new(text).small().frame(false))
                .on_hover_text(if pedal.enabled {
                    tr("Click to bypass", "Clique para desligar")
                } else {
                    tr("Bypassed: click to turn on", "Em bypass: clique para ligar")
                })
                .clicked()
            {
                pedal.enabled = !pedal.enabled;
            }
        }
        ui.label(RichText::new("⏵ 🔊 OUT").color(palette.text_dim));
    });
}

fn empty_state(ui: &mut Ui, chain: &mut Vec<PedalInstance>) {
    let palette = Palette::of(ui.ctx());
    ui.add_space(style::space::XL);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(tr("Your pedalboard is empty", "Sua pedaleira está vazia")).size(style::text::HEADING).color(palette.text));
        ui.label(RichText::new(tr(
            "Add pedals one by one, or load a preset from the Presets page.",
            "Adicione pedais um a um ou carregue um preset na página Presets.",
        )).color(palette.text_dim));
        ui.add_space(style::space::M);
        ui.horizontal(|ui| {
            // Center the quick-start buttons.
            let width = 330.0;
            ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
            if ui.button(tr("Clean amp", "Amp limpo")).clicked() {
                chain.push(PedalInstance::new(PedalDescription::Amp(Default::default())));
                chain.push(PedalInstance::new(PedalDescription::Reverb(Default::default())));
            }
            if ui.button("Crunch").clicked() {
                chain.push(PedalInstance::new(PedalDescription::NoiseGate(Default::default())));
                chain.push(PedalInstance::new(PedalDescription::Distortion(Default::default())));
                chain.push(PedalInstance::new(PedalDescription::Amp(crate::shared::pedals::AmpParams {
                    voicing: crate::shared::pedals::AmpVoicing::Crunch,
                    gain: 2.0,
                    ..Default::default()
                })));
            }
            if ui.button(tr("Ambient", "Ambiente")).clicked() {
                chain.push(PedalInstance::new(PedalDescription::Chorus(Default::default())));
                chain.push(PedalInstance::new(PedalDescription::Delay(Default::default())));
                chain.push(PedalInstance::new(PedalDescription::Reverb(crate::shared::pedals::ReverbParams {
                    room_size: 0.85,
                    mix: 0.45,
                    damping: 0.4,
                })));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::pedals::{AmpParams, CompressorParams, ReverbParams};

    #[test]
    fn new_pedals_land_in_signal_order() {
        let mut chain = vec![
            PedalInstance::new(PedalDescription::Amp(AmpParams::default())),
            PedalInstance::new(PedalDescription::Reverb(ReverbParams::default())),
        ];
        assert_eq!(insert_in_signal_order(&mut chain, PedalDescription::Compressor(CompressorParams::default())), 0);
        assert_eq!(insert_in_signal_order(&mut chain, PedalDescription::Chorus(Default::default())), 2);
        let names: Vec<&str> = chain.iter().map(|p| p.description.name()).collect();
        assert_eq!(names, ["Compressor", "Amp", "Chorus", "Reverb"]);
    }

    #[test]
    fn ab_compare_swaps_chains_back_and_forth() {
        let mut page = PedalboardPage::default();
        let a = vec![PedalInstance::new(PedalDescription::Amp(AmpParams::default()))];
        let mut chain = a.clone();
        assert!(!page.swap_compare(&mut chain));
        page.compare = Some((chain.clone(), true));
        chain.push(PedalInstance::new(PedalDescription::Reverb(ReverbParams::default())));
        let b = chain.clone();
        assert!(page.swap_compare(&mut chain));
        assert_eq!(chain, a);
        assert!(page.swap_compare(&mut chain));
        assert_eq!(chain, b);
    }

    #[test]
    fn row_heights_follow_the_measured_cards() {
        let mut rows = RowHeights::default();
        assert_eq!(rows.target(4, 0), 0.0);
        assert!(rows.update(4, vec![300.0, 200.0]));
        assert_eq!(rows.target(4, 0), 300.0);
        assert_eq!(rows.target(4, 1), 200.0);
        assert_eq!(rows.target(4, 2), 0.0);
        // Same measurements (or sub-pixel noise): nothing to redraw.
        assert!(!rows.update(4, vec![300.2, 200.0]));
        assert_eq!(rows.target(4, 0), 300.0);
        // The tall pedal went away: the row shrinks.
        assert!(rows.update(4, vec![250.0, 200.0]));
        assert_eq!(rows.target(4, 0), 250.0);
        // A different column count invalidates every row.
        assert_eq!(rows.target(3, 0), 0.0);
        assert!(rows.update(3, vec![250.0]));
    }

    /// Runs the page for a few frames in a window `width` wide.
    fn run_page(ctx: &egui::Context, page: &mut PedalboardPage, chain: &mut Vec<PedalInstance>, width: f32) {
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 800.0))),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    page.ui(ui, chain);
                });
            });
        }
    }

    #[test]
    fn card_rows_take_the_tallest_card_and_shrink_back() {
        use crate::shared::pedals::{NoiseGateParams, TremoloParams};
        let ctx = egui::Context::default();
        let mut page = PedalboardPage::default();
        let mut chain = vec![
            PedalInstance::new(PedalDescription::Tremolo(TremoloParams::default())),
            PedalInstance::new(PedalDescription::NoiseGate(NoiseGateParams::default())),
        ];
        run_page(&ctx, &mut page, &mut chain, 1200.0);
        let with_gate = page.row_heights.target(page.row_heights.per_row, 0);
        assert!(with_gate > 0.0);
        // Two rows of knobs (gate) are taller than one (tremolo); removing
        // the gate brings the row back to the tremolo's height.
        chain.remove(1);
        run_page(&ctx, &mut page, &mut chain, 1200.0);
        let tremolo_only = page.row_heights.target(page.row_heights.per_row, 0);
        assert!(tremolo_only > 0.0 && tremolo_only < with_gate, "{tremolo_only} vs {with_gate}");
        // Phones: one card per row, nothing stored.
        run_page(&ctx, &mut page, &mut chain, 390.0);
        assert_eq!(page.row_heights.per_row, 1);
        assert!(page.row_heights.rows.is_empty());
    }

    #[test]
    fn page_records_undo_steps_for_its_own_edits() {
        let ctx = egui::Context::default();
        let mut page = PedalboardPage::default();
        let mut chain = vec![PedalInstance::new(PedalDescription::Amp(AmpParams::default()))];
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let out = page.ui(ui, &mut chain);
                assert!(!out.chain_changed);
            });
        });
        let before = chain.clone();
        chain[0].enabled = false;
        page.end_frame(&before, &chain);
        assert!(page.undo(&mut chain));
        assert!(chain[0].enabled);
        // The undo itself isn't recorded as a new edit.
        page.end_frame(&[], &chain);
        assert!(!page.history.can_undo());
        assert!(page.history.can_redo());
    }
}
