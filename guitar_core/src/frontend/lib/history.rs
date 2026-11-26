//! Undo/redo for pedal-chain edits.
//!
//! The pedalboard records the chain as it was before each edit. Turning a
//! knob changes the chain on every frame of the drag, so consecutive tweaks
//! of the same pedal within [`COALESCE`] become one undo step.

use std::time::{Duration, Instant};

use crate::shared::pedals::PedalInstance;

/// Tweaks of the same pedal closer together than this are one undo step.
pub const COALESCE: Duration = Duration::from_millis(1000);
const LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// A parameter of the pedal at this index changed.
    Tweak(usize),
    /// Pedals added, removed, moved, or the whole chain replaced.
    Structure,
}

#[derive(Default)]
pub struct ChainHistory {
    undo: Vec<Vec<PedalInstance>>,
    redo: Vec<Vec<PedalInstance>>,
    last: Option<(EditKind, Instant)>,
}

impl ChainHistory {
    /// Call after an edit, with the chain as it was before it.
    pub fn record(&mut self, before: &[PedalInstance], kind: EditKind) {
        self.record_at(before, kind, Instant::now());
    }

    fn record_at(&mut self, before: &[PedalInstance], kind: EditKind, now: Instant) {
        let coalesce = matches!(kind, EditKind::Tweak(_))
            && self.last.is_some_and(|(k, t)| k == kind && now.duration_since(t) < COALESCE)
            && !self.undo.is_empty();
        self.last = Some((kind, now));
        self.redo.clear();
        if coalesce {
            return;
        }
        self.undo.push(before.to_vec());
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
    }

    /// Returns the chain to go back to, given the current one.
    pub fn undo(&mut self, current: &[PedalInstance]) -> Option<Vec<PedalInstance>> {
        let previous = self.undo.pop()?;
        self.redo.push(current.to_vec());
        self.last = None;
        Some(previous)
    }

    pub fn redo(&mut self, current: &[PedalInstance]) -> Option<Vec<PedalInstance>> {
        let next = self.redo.pop()?;
        self.undo.push(current.to_vec());
        self.last = None;
        Some(next)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// What changed between two chains: `None` if nothing did.
pub fn classify_edit(before: &[PedalInstance], after: &[PedalInstance]) -> Option<EditKind> {
    if before == after {
        return None;
    }
    if before.len() != after.len() {
        return Some(EditKind::Structure);
    }
    let differing: Vec<usize> = (0..before.len()).filter(|&i| before[i] != after[i]).collect();
    let same_kinds = before
        .iter()
        .zip(after)
        .all(|(a, b)| std::mem::discriminant(&a.description) == std::mem::discriminant(&b.description));
    match differing.as_slice() {
        [i] if same_kinds => Some(EditKind::Tweak(*i)),
        _ => Some(EditKind::Structure),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::pedals::{DelayParams, PedalDescription, ReverbParams};

    fn delay(ms: f32) -> PedalInstance {
        PedalInstance::new(PedalDescription::Delay(DelayParams { delay_ms: ms, ..Default::default() }))
    }

    #[test]
    fn knob_drags_coalesce_into_one_step() {
        let mut h = ChainHistory::default();
        let t0 = Instant::now();
        h.record_at(&[delay(100.0)], EditKind::Tweak(0), t0);
        h.record_at(&[delay(110.0)], EditKind::Tweak(0), t0 + Duration::from_millis(16));
        h.record_at(&[delay(120.0)], EditKind::Tweak(0), t0 + Duration::from_millis(32));
        let current = [delay(130.0)];
        assert_eq!(h.undo(&current), Some(vec![delay(100.0)]));
        assert!(!h.can_undo());
        assert_eq!(h.redo(&[delay(100.0)]), Some(vec![delay(130.0)]));
    }

    #[test]
    fn separate_edits_are_separate_steps_and_new_edits_clear_redo() {
        let mut h = ChainHistory::default();
        let t0 = Instant::now();
        h.record_at(&[], EditKind::Structure, t0);
        h.record_at(&[delay(1.0)], EditKind::Tweak(0), t0);
        h.record_at(&[delay(2.0)], EditKind::Tweak(0), t0 + COALESCE * 2);
        assert_eq!(h.undo(&[delay(3.0)]), Some(vec![delay(2.0)]));
        assert!(h.can_redo());
        h.record_at(&[delay(2.0)], EditKind::Structure, t0 + COALESCE * 3);
        assert!(!h.can_redo());
    }

    #[test]
    fn classifies_edits() {
        let reverb = PedalInstance::new(PedalDescription::Reverb(ReverbParams::default()));
        assert_eq!(classify_edit(&[delay(1.0)], &[delay(1.0)]), None);
        assert_eq!(
            classify_edit(&[delay(1.0), reverb.clone()], &[delay(2.0), reverb.clone()]),
            Some(EditKind::Tweak(0))
        );
        assert_eq!(classify_edit(&[delay(1.0)], &[]), Some(EditKind::Structure));
        assert_eq!(
            classify_edit(&[delay(1.0), reverb.clone()], &[reverb, delay(1.0)]),
            Some(EditKind::Structure)
        );
    }
}
