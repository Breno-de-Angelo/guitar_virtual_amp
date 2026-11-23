//! Simplified, playback- and rendering-oriented score model.
//!
//! The `guitarpro` crate's `Song` mirrors the Guitar Pro file layout closely
//! (and differs subtly between GP3-5 and GPIF-based GP6+ files). Everything
//! downstream (tab rendering, synth event scheduling, audio sync) only needs a
//! small subset of it, so `import` flattens a `Song` into these types once at
//! load time. That keeps format quirks in one place.

/// Ticks per quarter note, matching Guitar Pro's internal resolution.
pub const TICKS_PER_QUARTER: i64 = 960;

/// One full score: header bars shared by every track, plus the tracks.
#[derive(Clone, Debug, Default)]
pub struct Score {
    pub title: String,
    pub artist: String,
    /// Tempo at the start of the song, in quarter notes per minute.
    pub initial_tempo: f64,
    pub bars: Vec<MasterBar>,
    pub tracks: Vec<ScoreTrack>,
    /// Guitar Pro 8 audio sync anchors (empty for every other format).
    pub sync_points: Vec<FileSyncPoint>,
    /// Guitar Pro 8 embedded backing track (raw, still-encoded audio bytes).
    pub backing_audio: Option<Vec<u8>>,
}

/// Per-bar data shared by every track ("measure header" in GP terms).
#[derive(Clone, Debug, Default)]
pub struct MasterBar {
    pub numerator: u32,
    pub denominator: u32,
    /// Tempo changes inside this bar: `(tick offset from bar start, bpm)`,
    /// sorted by tick. Empty means "keep whatever tempo was playing".
    pub tempo_changes: Vec<(i64, f64)>,
    pub repeat_open: bool,
    /// How many times playback jumps back to the matching repeat-open bar
    /// when it reaches the end of this bar (0 = not a repeat-close bar).
    pub repeat_jumps: u32,
    /// Alternate-ending bitmask: bit `n` set means "played on pass `n + 1`".
    /// 0 means the bar is played on every pass.
    pub alternatives: u8,
    pub marker: Option<String>,
}

impl MasterBar {
    /// Nominal length of the bar in ticks, from its time signature.
    pub fn length_ticks(&self) -> i64 {
        let den = self.denominator.max(1) as i64;
        self.numerator.max(1) as i64 * TICKS_PER_QUARTER * 4 / den
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScoreTrack {
    pub name: String,
    /// Open-string MIDI pitches, index 0 = string 1 (the highest-pitched).
    pub tuning: Vec<u8>,
    pub percussion: bool,
    /// General MIDI program (0-127).
    pub program: u8,
    /// Capo fret; added to every fret number to get the sounding pitch.
    pub capo: i32,
    /// MIDI volume 0-127 from the file's mixer settings.
    pub volume: u8,
    /// One entry per `Score::bars` element.
    pub bars: Vec<TrackBar>,
}

impl ScoreTrack {
    pub fn string_count(&self) -> usize {
        self.tuning.len().max(1)
    }
}

#[derive(Clone, Debug, Default)]
pub struct TrackBar {
    /// Voices in file order; voice 0 is the main one drawn on the tab.
    pub voices: Vec<Vec<TabBeat>>,
}

#[derive(Clone, Debug, Default)]
pub struct TabBeat {
    /// Offset from the start of the bar, in ticks.
    pub start: i64,
    /// Duration in ticks (dots and tuplets already applied).
    pub duration: i64,
    /// Notated base value: 1 = whole, 2 = half, 4 = quarter, 8 = eighth...
    pub value: u16,
    pub dotted: bool,
    /// Tuplet "enters" count (3 for a triplet), when the beat is in a tuplet.
    pub tuplet: Option<u8>,
    pub notes: Vec<TabNote>,
    /// Text shown above the beat (palm mute, let ring, beat text).
    pub palm_mute: bool,
    pub let_ring: bool,
    pub text: String,
}

impl TabBeat {
    pub fn is_rest(&self) -> bool {
        self.notes.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabNote {
    /// 1-based string number (1 = highest-pitched string).
    pub string: u8,
    /// Fret number; for percussion tracks this is the GM drum key instead.
    pub fret: i16,
    pub velocity: u8,
    /// Continues the previous note on the same string instead of re-picking.
    pub tie: bool,
    pub dead: bool,
    pub ghost: bool,
    pub hammer: bool,
    pub slide: bool,
    pub bend: bool,
    pub vibrato: bool,
    pub harmonic: bool,
    pub palm_mute: bool,
    pub let_ring: bool,
    pub staccato: bool,
}

/// A Guitar Pro 8 sync point: "occurrence `occurrence` of bar `bar` starts at
/// `audio_secs` in the embedded backing track".
#[derive(Clone, Debug, PartialEq)]
pub struct FileSyncPoint {
    pub bar: usize,
    pub occurrence: usize,
    pub audio_secs: f64,
}
