//! Named instrument tunings for the tuner, and helpers to find the string
//! a played note belongs to.

use crate::frontend::lib::pitch::{freq_to_midi, pitch_class_name};

/// A tuning as MIDI notes of the open strings, lowest string first.
#[derive(Clone, Debug, PartialEq)]
pub struct Tuning {
    pub name: String,
    pub strings: Vec<u8>,
}

/// `(name, open strings low to high)`.
pub const TUNINGS: &[(&str, &[u8])] = &[
    ("Standard", &[40, 45, 50, 55, 59, 64]),
    ("Drop D", &[38, 45, 50, 55, 59, 64]),
    ("Half step down (Eb)", &[39, 44, 49, 54, 58, 63]),
    ("Whole step down (D)", &[38, 43, 48, 53, 57, 62]),
    ("Drop C#", &[37, 44, 49, 54, 58, 63]),
    ("Drop C", &[36, 43, 48, 53, 57, 62]),
    ("Drop B", &[35, 42, 47, 52, 56, 61]),
    ("Open G", &[38, 43, 50, 55, 59, 62]),
    ("Open D", &[38, 45, 50, 54, 57, 62]),
    ("Open E", &[40, 47, 52, 56, 59, 64]),
    ("DADGAD", &[38, 45, 50, 55, 57, 62]),
    ("7-string standard", &[35, 40, 45, 50, 55, 59, 64]),
    ("Bass standard", &[28, 33, 38, 43]),
    ("Bass drop D", &[26, 33, 38, 43]),
    ("5-string bass", &[23, 28, 33, 38, 43]),
];

impl Tuning {
    pub fn named(name: &str) -> Option<Tuning> {
        TUNINGS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(n, s)| Tuning { name: n.to_string(), strings: s.to_vec() })
    }

    pub fn standard() -> Tuning {
        Tuning::named("Standard").expect("in the table")
    }

    /// The name to show in the UI (see [`display_name`]).
    pub fn display_name(&self) -> &str {
        display_name(&self.name)
    }

    /// From a tab track's tuning (`ScoreTrack::tuning`, string 1 = the
    /// highest string first). Named after the matching preset if any.
    pub fn from_tab(tuning_high_first: &[u8]) -> Option<Tuning> {
        if tuning_high_first.is_empty() {
            return None;
        }
        let strings: Vec<u8> = tuning_high_first.iter().rev().copied().collect();
        let name = TUNINGS
            .iter()
            .find(|(_, s)| *s == strings.as_slice())
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| label(&strings));
        Some(Tuning { name, strings })
    }

    /// The number guitarists (and tabs) give the string at `index` in
    /// [`Tuning::strings`]: 1 is the highest string, so on a standard
    /// guitar the low E (index 0) is string 6.
    pub fn string_number(&self, index: usize) -> usize {
        self.strings.len().saturating_sub(index)
    }

    /// The string a frequency is closest to: `(string index, cents off)`.
    /// Cents are measured to that string's open pitch.
    pub fn nearest_string(&self, freq: f64, a4_hz: f64) -> Option<(usize, f64)> {
        let midi = freq_to_midi(freq, a4_hz);
        self.strings
            .iter()
            .enumerate()
            .map(|(i, &s)| (i, (midi - s as f64) * 100.0))
            .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
    }
}

/// The UI text for a tuning name. The names in [`TUNINGS`] are keys
/// (saved in `UiPrefs::tuner_tuning`, matched by [`Tuning::named`]), so
/// they stay English and are translated only when shown. So do the
/// Portuguese names `transcribe::arrange` writes into tab sidecars
/// (`TrackInfo::tuning_name`). Anything else (e.g. an "F A D G B E" label)
/// is returned as is.
pub fn display_name(name: &str) -> &str {
    use crate::shared::i18n::tr;
    match name {
        "Standard" => tr("Standard", "Padrão"),
        "Half step down (Eb)" => tr("Half step down (Eb)", "Meio tom abaixo (Eb)"),
        "Whole step down (D)" => tr("Whole step down (D)", "Um tom abaixo (D)"),
        "7-string standard" => tr("7-string standard", "7 cordas padrão"),
        "Bass standard" => tr("Bass standard", "Baixo padrão"),
        "Bass drop D" => tr("Bass drop D", "Baixo drop D"),
        "5-string bass" => tr("5-string bass", "Baixo de 5 cordas"),
        "E padrão" => tr("E standard", "E padrão"),
        "C# padrão" => tr("C# standard", "C# padrão"),
        "Eb (meio tom abaixo)" => tr("Eb (half step down)", "Eb (meio tom abaixo)"),
        "7 cordas (B)" => tr("7-string (B)", "7 cordas (B)"),
        "8 cordas (F#)" => tr("8-string (F#)", "8 cordas (F#)"),
        "5 cordas (B E A D G)" => tr("5-string (B E A D G)", "5 cordas (B E A D G)"),
        other => other,
    }
}

/// "E A D G B E" style label, lowest string first.
pub fn label(strings: &[u8]) -> String {
    strings.iter().map(|&s| pitch_class_name(s as i32)).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::lib::pitch::midi_to_freq;

    #[test]
    fn tab_tunings_are_reversed_and_named() {
        let t = Tuning::from_tab(&[64, 59, 55, 50, 45, 38]).unwrap();
        assert_eq!(t.name, "Drop D");
        assert_eq!(t.strings[0], 38);
        let odd = Tuning::from_tab(&[64, 59, 55, 50, 45, 41]).unwrap();
        assert_eq!(odd.name, "F A D G B E");
        assert!(Tuning::from_tab(&[]).is_none());
    }

    #[test]
    fn finds_the_nearest_string_and_its_offset() {
        let t = Tuning::standard();
        let (string, cents) = t.nearest_string(midi_to_freq(45.1, 440.0), 440.0).unwrap();
        assert_eq!(string, 1);
        assert!((cents - 10.0).abs() < 0.01);
        // A slightly flat low E still maps to the low E string.
        let (string, cents) = t.nearest_string(80.0, 440.0).unwrap();
        assert_eq!(string, 0);
        assert!(cents < -40.0);
    }

    #[test]
    fn every_named_tuning_resolves() {
        for (name, strings) in TUNINGS {
            let t = Tuning::named(name).unwrap();
            assert_eq!(t.strings.as_slice(), *strings);
            assert!(t.strings.windows(2).all(|w| w[0] < w[1]), "{name} goes low to high");
        }
        assert_eq!(label(&Tuning::standard().strings), "E A D G B E");
    }

    #[test]
    fn strings_are_numbered_from_the_highest() {
        let t = Tuning::standard();
        // Low E is string 6, high E string 1, like in a tab.
        assert_eq!(t.string_number(0), 6);
        assert_eq!(t.string_number(1), 5);
        assert_eq!(t.string_number(5), 1);
        let seven = Tuning::named("7-string standard").unwrap();
        assert_eq!(seven.string_number(0), 7);
        assert_eq!(seven.string_number(6), 1);
        let bass = Tuning::named("Bass standard").unwrap();
        assert_eq!((0..4).map(|i| bass.string_number(i)).collect::<Vec<_>>(), [4, 3, 2, 1]);
        let five = Tuning::named("5-string bass").unwrap();
        assert_eq!(five.string_number(0), 5);
        // A tab's string 1 (its first entry) keeps number 1 here.
        let eight = Tuning::from_tab(&[64, 59, 55, 50, 45, 40, 35, 30]).unwrap();
        assert_eq!(eight.strings[7], 64);
        assert_eq!(eight.string_number(7), 1);
        assert_eq!(eight.string_number(0), 8);
    }

    #[test]
    fn display_names_keep_the_keys_in_english() {
        assert_eq!(Tuning::standard().display_name(), "Standard");
        assert_eq!(display_name("Drop D"), "Drop D");
        assert_eq!(display_name("F A D G B E"), "F A D G B E");
    }
}
