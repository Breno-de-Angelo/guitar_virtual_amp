//! MIDI footswitch / controller bindings: which incoming message does what.
//! The connection itself is `backend::midi` (desktop only); this module is
//! the persisted data and the message parsing, shared by every platform.

use serde::{Deserialize, Serialize};

use crate::shared::i18n::tr;
use crate::trf;

/// The part of a MIDI message a binding listens for. Channels are 0-15.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MidiTrigger {
    ControlChange { channel: u8, controller: u8 },
    Note { channel: u8, note: u8 },
    /// Any program change on the channel; the program number is the value.
    ProgramChange { channel: u8 },
}

impl MidiTrigger {
    pub fn label(&self) -> String {
        match self {
            MidiTrigger::ControlChange { channel, controller } => trf!("CC {controller} (ch {})", "CC {controller} (canal {})", channel + 1),
            MidiTrigger::Note { channel, note } => trf!("Note {note} (ch {})", "Nota {note} (canal {})", channel + 1),
            MidiTrigger::ProgramChange { channel } => trf!("Program change (ch {})", "Program change (canal {})", channel + 1),
        }
    }
}

/// What a binding does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MidiAction {
    /// Bypass on/off for the pedal at this position in the chain.
    TogglePedal(usize),
    NextPreset,
    PreviousPreset,
    /// Program change N loads setlist entry N.
    SetlistSlot,
    ToggleMute,
    TapTempo,
    ToggleMetronome,
    /// Continuous: the CC value (0-127) sets the master volume.
    OutputVolume,
    /// Record / play / overdub cycle of the first Looper pedal.
    LooperCycle,
}

impl MidiAction {
    pub fn label(&self) -> String {
        match self {
            MidiAction::TogglePedal(i) => trf!("Toggle pedal #{}", "Ligar/desligar pedal #{}", i + 1),
            MidiAction::NextPreset => tr("Next preset", "Próximo preset").into(),
            MidiAction::PreviousPreset => tr("Previous preset", "Preset anterior").into(),
            MidiAction::SetlistSlot => tr("Load setlist slot (program number)", "Carregar item da setlist (nº do programa)").into(),
            MidiAction::ToggleMute => tr("Mute / unmute", "Silenciar / reativar").into(),
            MidiAction::TapTempo => tr("Tap tempo", "Marcar o tempo (tap)").into(),
            MidiAction::ToggleMetronome => tr("Start / stop metronome", "Iniciar / parar metrônomo").into(),
            MidiAction::OutputVolume => tr("Master volume (expression pedal)", "Volume geral (pedal de expressão)").into(),
            MidiAction::LooperCycle => tr("Looper: record / play / overdub", "Looper: gravar / tocar / sobrepor").into(),
        }
    }

    /// Continuous actions use every value; the others fire on a "press".
    pub fn is_continuous(&self) -> bool {
        matches!(self, MidiAction::OutputVolume | MidiAction::SetlistSlot)
    }

    /// The actions offered in the bindings editor (pedal toggles are added
    /// per chain position).
    pub const GLOBAL: [MidiAction; 8] = [
        MidiAction::NextPreset,
        MidiAction::PreviousPreset,
        MidiAction::SetlistSlot,
        MidiAction::ToggleMute,
        MidiAction::TapTempo,
        MidiAction::ToggleMetronome,
        MidiAction::OutputVolume,
        MidiAction::LooperCycle,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiBinding {
    pub trigger: MidiTrigger,
    pub action: MidiAction,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MidiPrefs {
    /// Name of the MIDI input port to open on launch.
    pub input_port: Option<String>,
    pub bindings: Vec<MidiBinding>,
}

impl MidiPrefs {
    /// Actions bound to `trigger`, with whether they should fire for `value`.
    /// A footswitch sends a CC of 127 when pressed and 0 when released (or
    /// 127/0 on alternate presses): only values >= 64 count as a press, so a
    /// press-and-release toggles once.
    pub fn actions_for(&self, trigger: MidiTrigger, value: u8) -> Vec<MidiAction> {
        self.bindings
            .iter()
            .filter(|b| b.trigger == trigger)
            .filter(|b| b.action.is_continuous() || matches!(trigger, MidiTrigger::ProgramChange { .. }) || value >= 64)
            .map(|b| b.action)
            .collect()
    }

    /// Adds a binding, replacing whatever `action` was bound to before.
    pub fn bind(&mut self, trigger: MidiTrigger, action: MidiAction) {
        self.bindings.retain(|b| b.action != action);
        self.bindings.push(MidiBinding { trigger, action });
    }

    pub fn trigger_for(&self, action: MidiAction) -> Option<MidiTrigger> {
        self.bindings.iter().find(|b| b.action == action).map(|b| b.trigger)
    }

    pub fn unbind(&mut self, action: MidiAction) {
        self.bindings.retain(|b| b.action != action);
    }
}

/// Parses a raw MIDI message into `(trigger, value)`. Note-offs (and
/// note-ons with velocity 0) and every other message type are ignored.
pub fn parse_message(bytes: &[u8]) -> Option<(MidiTrigger, u8)> {
    let status = *bytes.first()?;
    let channel = status & 0x0F;
    match status & 0xF0 {
        0xB0 => Some((MidiTrigger::ControlChange { channel, controller: *bytes.get(1)? & 0x7F }, *bytes.get(2)? & 0x7F)),
        0x90 => {
            let velocity = *bytes.get(2)? & 0x7F;
            (velocity > 0).then_some((MidiTrigger::Note { channel, note: *bytes.get(1)? & 0x7F }, velocity))
        }
        0xC0 => Some((MidiTrigger::ProgramChange { channel }, *bytes.get(1)? & 0x7F)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cc_note_and_program_change() {
        assert_eq!(
            parse_message(&[0xB2, 64, 127]),
            Some((MidiTrigger::ControlChange { channel: 2, controller: 64 }, 127))
        );
        assert_eq!(parse_message(&[0x90, 60, 100]), Some((MidiTrigger::Note { channel: 0, note: 60 }, 100)));
        assert_eq!(parse_message(&[0x90, 60, 0]), None, "note-on with velocity 0 is a note-off");
        assert_eq!(parse_message(&[0x80, 60, 10]), None);
        assert_eq!(parse_message(&[0xCF, 5]), Some((MidiTrigger::ProgramChange { channel: 15 }, 5)));
        assert_eq!(parse_message(&[0xF8]), None, "clock");
        assert_eq!(parse_message(&[0xB0, 1]), None, "truncated");
    }

    #[test]
    fn toggles_fire_on_press_only_and_continuous_actions_on_every_value() {
        let mut prefs = MidiPrefs::default();
        let switch = MidiTrigger::ControlChange { channel: 0, controller: 80 };
        let expression = MidiTrigger::ControlChange { channel: 0, controller: 7 };
        prefs.bind(switch, MidiAction::TogglePedal(2));
        prefs.bind(expression, MidiAction::OutputVolume);
        assert_eq!(prefs.actions_for(switch, 127), vec![MidiAction::TogglePedal(2)]);
        assert!(prefs.actions_for(switch, 0).is_empty());
        assert_eq!(prefs.actions_for(expression, 10), vec![MidiAction::OutputVolume]);
    }

    #[test]
    fn binding_an_action_again_replaces_its_trigger() {
        let mut prefs = MidiPrefs::default();
        let a = MidiTrigger::Note { channel: 0, note: 1 };
        let b = MidiTrigger::Note { channel: 0, note: 2 };
        prefs.bind(a, MidiAction::NextPreset);
        prefs.bind(b, MidiAction::NextPreset);
        assert_eq!(prefs.bindings.len(), 1);
        assert_eq!(prefs.trigger_for(MidiAction::NextPreset), Some(b));
        prefs.unbind(MidiAction::NextPreset);
        assert!(prefs.bindings.is_empty());
    }
}
