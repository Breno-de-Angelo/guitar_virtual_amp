//! Mixing-desk filters (RBJ biquads), for the guitar DI's character and the
//! final mix. The app's own tone stays in its pedals.

use crate::guitar::RATE;

pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    x: [f32; 2],
    y: [f32; 2],
}

impl Biquad {
    fn new(b: [f32; 3], a0: f32, a: [f32; 2]) -> Self {
        Self { b: b.map(|v| v / a0), a: a.map(|v| v / a0), x: [0.0; 2], y: [0.0; 2] }
    }

    /// Butterworth high-pass.
    pub fn high_pass(hz: f32) -> Self {
        let (c, alpha) = Self::w(hz, std::f32::consts::FRAC_1_SQRT_2);
        Self::new([(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0], 1.0 + alpha, [-2.0 * c, 1.0 - alpha])
    }

    /// Peaking EQ, `db` at `hz`.
    pub fn peak(hz: f32, q: f32, db: f32) -> Self {
        let (c, alpha) = Self::w(hz, q);
        let a = 10f32.powf(db / 40.0);
        Self::new([1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a], 1.0 + alpha / a, [-2.0 * c, 1.0 - alpha / a])
    }

    /// High shelf, `db` above about `hz`.
    pub fn high_shelf(hz: f32, db: f32) -> Self {
        let a = 10f32.powf(db / 40.0);
        let (c, alpha) = Self::w(hz, std::f32::consts::FRAC_1_SQRT_2);
        let s = 2.0 * a.sqrt() * alpha;
        Self::new(
            [a * ((a + 1.0) + (a - 1.0) * c + s), -2.0 * a * ((a - 1.0) + (a + 1.0) * c), a * ((a + 1.0) + (a - 1.0) * c - s)],
            (a + 1.0) - (a - 1.0) * c + s,
            [2.0 * ((a - 1.0) - (a + 1.0) * c), (a + 1.0) - (a - 1.0) * c - s],
        )
    }

    fn w(hz: f32, q: f32) -> (f32, f32) {
        let w0 = std::f32::consts::TAU * hz / RATE as f32;
        (w0.cos(), w0.sin() / (2.0 * q))
    }

    pub fn run(&mut self, x: &mut [f32]) {
        for s in x.iter_mut() {
            let y = self.b[0] * *s + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
            self.x = [*s, self.x[0]];
            self.y = [y, self.y[0]];
            *s = y;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain_at(mut f: Biquad, hz: f32) -> f32 {
        let mut x: Vec<f32> = (0..RATE).map(|i| (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin()).collect();
        f.run(&mut x);
        let tail = &x[RATE / 2..];
        20.0 * ((tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt() * 2f32.sqrt()).log10()
    }

    #[test]
    fn filters_do_what_they_say() {
        assert!(gain_at(Biquad::high_pass(100.0), 40.0) < -14.0);
        assert!(gain_at(Biquad::high_pass(100.0), 1000.0).abs() < 0.2);
        assert!((gain_at(Biquad::peak(250.0, 1.0, -3.0), 250.0) + 3.0).abs() < 0.3);
        assert!((gain_at(Biquad::high_shelf(2500.0, 6.0), 10000.0) - 6.0).abs() < 0.6);
        assert!(gain_at(Biquad::high_shelf(2500.0, 6.0), 200.0).abs() < 0.3);
    }
}
