//! A stand-in for the missing dry signal: the transcribed notes played by a
//! plucked-string model (extended Karplus-Strong), at the times the notes
//! were played. It has the target's notes and timing but not its guitar, so
//! the tone search compares it "up to an EQ" (see [`super::features`]).

/// One note to play.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Note {
    pub start: f64,
    pub end: f64,
    pub pitch: u8,
    /// 0..1.
    pub velocity: f32,
}

/// The string model's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    /// Where along the string it is plucked (fraction of the length): sets
    /// the comb notches of the excitation, as a pick near the bridge does.
    pub pluck_position: f32,
    /// After the note ends, the string is damped to -60 dB in this time.
    pub release_secs: f32,
    /// Ring time to -60 dB of a held low E; higher notes ring shorter.
    pub low_e_t60: f32,
    /// Excitation low-pass coefficient at zero velocity; full velocity adds
    /// `velocity_brightness`.
    pub brightness: f32,
    pub velocity_brightness: f32,
    /// Filter the result with [`DI_EQ`], the measured difference between
    /// real DI recordings and this model.
    pub di_eq: bool,
}

/// Long-term spectrum of real DI minus this model's, dB per band centre
/// (Hz), measured by `tone::eval::fit_pluck` on 14 Guitar-TECHS DI
/// recordings (P1/P2 single notes, chords, scales) with the default model.
pub const DI_EQ: &[(f32, f32)] = &[
    (69.0, 16.0),
    (79.0, 13.7),
    (90.0, 8.9),
    (103.0, 7.3),
    (119.0, 6.0),
    (136.0, 5.9),
    (156.0, 6.5),
    (178.0, 6.6),
    (204.0, 5.5),
    (234.0, 4.6),
    (269.0, 3.8),
    (308.0, 2.2),
    (353.0, 1.4),
    (404.0, 1.8),
    (463.0, 0.5),
    (531.0, -0.1),
    (608.0, -4.8),
    (697.0, -6.6),
    (799.0, -5.2),
    (915.0, -3.8),
    (1049.0, -7.5),
    (1202.0, -9.6),
    (1377.0, -9.0),
    (1578.0, -7.5),
    (1809.0, -6.2),
    (2073.0, -5.1),
    (2375.0, -4.0),
    (2722.0, -3.6),
    (3119.0, -3.7),
    (3575.0, -4.3),
    (4097.0, -4.0),
    (4695.0, -3.0),
    (5380.0, -3.1),
    (6165.0, -2.3),
    (7065.0, -1.4),
    (8096.0, -0.8),
    (9278.0, -0.3),
    (10632.0, 0.1),
    (12184.0, 0.5),
    (13962.0, 2.2),
];

impl Default for Model {
    fn default() -> Self {
        Self {
            // Fitted by `tone::eval::fit_pluck` (EQ-forgiven distance to the
            // real DI 13.05 -> 9.90 dB on the fitting set).
            pluck_position: 0.48,
            release_secs: 0.45,
            low_e_t60: 16.0,
            brightness: 0.008,
            velocity_brightness: 0.0,
            di_eq: true,
        }
    }
}

fn hz(pitch: u8) -> f32 {
    440.0 * 2f32.powf((pitch as f32 - 69.0) / 12.0)
}

/// Feedback gain giving a decay to -60 dB in `t60` seconds at `f0`.
fn loop_gain(f0: f32, t60: f32) -> f32 {
    10f32.powf(-3.0 / (f0 * t60.max(1e-3)))
}

fn pluck(m: &Model, note: &Note, rate: u32, seed: &mut u32, out: &mut [f32]) {
    let rate_f = rate as f32;
    let f0 = hz(note.pitch);
    // The two-point loss filter adds half a sample of delay.
    let period = rate_f / f0 - 0.5;
    let len = period.floor() as usize;
    if len < 2 {
        return;
    }
    let frac = period - len as f32;
    let start = (note.start * rate as f64) as usize;
    let held = ((note.end - note.start).max(0.03) * rate as f64) as usize;
    let total = held + (m.release_secs * 3.0 * rate_f) as usize;
    if start >= out.len() {
        return;
    }
    // Excitation: white noise, low-passed more for soft notes, with the
    // comb of the pluck position.
    let brightness =
        (m.brightness + m.velocity_brightness * note.velocity.clamp(0.0, 1.0)).clamp(0.01, 1.0);
    // One slot more than the integer delay, for the fractional part.
    let size = len + 1;
    let mut line = vec![0f32; size];
    let mut lp = 0.0;
    for v in line.iter_mut() {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
        lp += brightness * (noise - lp);
        *v = lp;
    }
    let notch = ((m.pluck_position * len as f32) as usize).max(1);
    let excitation: Vec<f32> = (0..line.len())
        .map(|i| line[i] - if i >= notch { line[i - notch] } else { 0.0 })
        .collect();
    line.copy_from_slice(&excitation);
    let peak = line.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    let amp = note.velocity.clamp(0.05, 1.0).powf(1.5) / peak;
    line.iter_mut().for_each(|v| *v *= amp);

    // Higher notes decay faster, roughly as on a real string.
    let t60 = (m.low_e_t60 * (hz(40) / f0).powf(0.7)).clamp(0.4, m.low_e_t60);
    let ring = loop_gain(f0, t60);
    let damped = loop_gain(f0, m.release_secs);
    let mut pos = 0usize;
    let mut prev = 0.0;
    for i in 0..total {
        let j = start + i;
        if j >= out.len() {
            break;
        }
        // Slot `pos` is `size` samples old, the next one `len`: interpolate
        // to a delay of `len + frac`.
        let y = frac * line[pos] + (1.0 - frac) * line[(pos + 1) % size];
        out[j] += y;
        let g = if i < held { ring } else { damped };
        // Two-point average = the string's high-frequency loss.
        let next = g * 0.5 * (y + prev);
        prev = y;
        line[pos] = next;
        pos = (pos + 1) % size;
    }
}

/// Plays `notes` into a buffer of `len` samples at `rate`.
pub fn render(notes: &[Note], len: usize, rate: u32) -> Vec<f32> {
    render_with(&Model::default(), notes, len, rate)
}

/// [`render`] with other model settings.
pub fn render_with(m: &Model, notes: &[Note], len: usize, rate: u32) -> Vec<f32> {
    let mut out = vec![0f32; len];
    let mut seed = 0x1234_5678u32;
    for note in notes {
        pluck(m, note, rate, &mut seed, &mut out);
    }
    // DC blocker: the loop and the excitation leave a little offset.
    let (mut x1, mut y1) = (0.0, 0.0);
    for v in out.iter_mut() {
        let y = *v - x1 + 0.995 * y1;
        x1 = *v;
        y1 = y;
        *v = y;
    }
    if m.di_eq && !DI_EQ.is_empty() {
        let (hz, db): (Vec<f32>, Vec<f32>) = DI_EQ.iter().copied().unzip();
        let ir = super::matching::ir_from_eq(&db, &hz, rate);
        out = super::matching::convolve(&out, &ir);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_sounds_while_held_and_stops_after() {
        let rate = 48_000;
        let note = Note {
            start: 0.1,
            end: 0.6,
            pitch: 52,
            velocity: 0.8,
        };
        let out = render(&[note], rate as usize, rate);
        let rms = |a: f64, b: f64| {
            let s = &out[(a * rate as f64) as usize..(b * rate as f64) as usize];
            (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt()
        };
        assert!(rms(0.0, 0.09) < 1e-4, "silent before the note");
        assert!(rms(0.15, 0.55) > 0.01, "sounding while held");
        assert!(
            rms(0.85, 1.0) < rms(0.15, 0.55) * 0.01,
            "damped after the end"
        );
    }

    #[test]
    fn plays_at_the_pitch_of_the_note() {
        let rate = 48_000;
        let note = Note {
            start: 0.0,
            end: 1.0,
            pitch: 57, // A3, 220 Hz
            velocity: 0.8,
        };
        let out = render(&[note], rate as usize, rate);
        // Autocorrelation peak near the period.
        let seg = &out[2_000..20_000];
        let best = (150..300)
            .max_by(|&a, &b| {
                let c = |l: usize| seg.iter().zip(&seg[l..]).map(|(x, y)| x * y).sum::<f32>();
                c(a).total_cmp(&c(b))
            })
            .unwrap();
        let period = rate as f32 / 220.0;
        assert!((best as f32 - period).abs() < 2.0, "{best} vs {period}");
    }
}
