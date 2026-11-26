//! Separable CMA-ES (Ros and Hansen, 2008): CMA-ES with a diagonal
//! covariance, enough for the ~10 parameters of a pedal chain and cheap to
//! update. The search space is the unit cube; candidates outside it are
//! evaluated at the nearest point inside, plus a penalty on the distance.
//! Each generation's candidates are evaluated in parallel.

/// Small deterministic generator (SplitMix64), so searches are reproducible.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let u = self.uniform().max(1e-12);
        let v = self.uniform();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// Search settings.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// Candidates per generation.
    pub population: usize,
    pub generations: usize,
    /// Initial step size, in units of the unit cube.
    pub sigma: f64,
    pub seed: u64,
    pub threads: usize,
}

/// Result of [`minimize`].
#[derive(Clone, Debug)]
pub struct Found {
    pub x: Vec<f64>,
    pub value: f64,
    pub evaluations: usize,
}

const BOUND_PENALTY: f64 = 10.0;

fn clamp_unit(x: &[f64]) -> (Vec<f64>, f64) {
    let mut outside = 0.0;
    let inside = x
        .iter()
        .map(|&v| {
            let c = v.clamp(0.0, 1.0);
            outside += (v - c) * (v - c);
            c
        })
        .collect();
    (inside, outside)
}

/// Evaluates `f` on every candidate, on up to `threads` threads.
fn evaluate(f: &(dyn Fn(&[f64]) -> f64 + Sync), xs: &[Vec<f64>], threads: usize) -> Vec<f64> {
    let threads = threads.clamp(1, xs.len().max(1));
    let mut out = vec![0.0; xs.len()];
    let chunk = xs.len().div_ceil(threads);
    std::thread::scope(|s| {
        for (xs, out) in xs.chunks(chunk).zip(out.chunks_mut(chunk)) {
            s.spawn(move || {
                for (x, o) in xs.iter().zip(out.iter_mut()) {
                    let (inside, outside) = clamp_unit(x);
                    *o = f(&inside) + BOUND_PENALTY * outside;
                }
            });
        }
    });
    out
}

/// Minimizes `f` over the unit cube from `x0`. Returns the best point seen
/// (inside the cube).
pub fn minimize(f: &(dyn Fn(&[f64]) -> f64 + Sync), x0: &[f64], s: Settings) -> Found {
    let n = x0.len();
    let nf = n as f64;
    let lambda = s.population.max(4);
    let mu = lambda / 2;
    let raw: Vec<f64> = (0..mu)
        .map(|i| ((lambda as f64 + 1.0) / 2.0).ln() - ((i + 1) as f64).ln())
        .collect();
    let sum: f64 = raw.iter().sum();
    let w: Vec<f64> = raw.iter().map(|v| v / sum).collect();
    let mueff = 1.0 / w.iter().map(|v| v * v).sum::<f64>();

    let cs = (mueff + 2.0) / (nf + mueff + 5.0);
    let ds = 1.0 + 2.0 * (((mueff - 1.0) / (nf + 1.0)).sqrt() - 1.0).max(0.0) + cs;
    let cc = (4.0 + mueff / nf) / (nf + 4.0 + 2.0 * mueff / nf);
    // Separable variant: learning rates scaled up by (n + 2) / 3.
    let sep = (nf + 2.0) / 3.0;
    let c1 = (2.0 / ((nf + 1.3).powi(2) + mueff) * sep).min(1.0);
    let cmu = ((2.0 * (mueff - 2.0 + 1.0 / mueff) / ((nf + 2.0).powi(2) + mueff)) * sep)
        .min(1.0 - c1)
        .max(0.0);
    let chi_n = nf.sqrt() * (1.0 - 1.0 / (4.0 * nf) + 1.0 / (21.0 * nf * nf));

    let mut rng = Rng::new(s.seed);
    let mut mean = x0.to_vec();
    let mut sigma = s.sigma;
    let mut c = vec![1.0f64; n];
    let mut ps = vec![0.0f64; n];
    let mut pc = vec![0.0f64; n];
    let (start, _) = clamp_unit(x0);
    let mut best = Found {
        value: evaluate(f, std::slice::from_ref(&start), 1)[0],
        x: start,
        evaluations: 1,
    };

    for generation in 0..s.generations {
        let z: Vec<Vec<f64>> = (0..lambda)
            .map(|_| (0..n).map(|_| rng.normal()).collect())
            .collect();
        let xs: Vec<Vec<f64>> = z
            .iter()
            .map(|z| {
                (0..n)
                    .map(|i| mean[i] + sigma * c[i].sqrt() * z[i])
                    .collect()
            })
            .collect();
        let values = evaluate(f, &xs, s.threads);
        best.evaluations += lambda;
        let mut order: Vec<usize> = (0..lambda).collect();
        order.sort_by(|&a, &b| values[a].total_cmp(&values[b]));
        if values[order[0]] < best.value {
            best.value = values[order[0]];
            best.x = clamp_unit(&xs[order[0]]).0;
        }

        let old = mean.clone();
        for (i, m) in mean.iter_mut().enumerate() {
            *m = (0..mu).map(|k| w[k] * xs[order[k]][i]).sum();
        }
        // Step in the space of z (C is diagonal, so C^-1/2 is elementwise).
        let zmean: Vec<f64> = (0..n)
            .map(|i| (mean[i] - old[i]) / sigma / c[i].sqrt())
            .collect();
        let k_s = (cs * (2.0 - cs) * mueff).sqrt();
        for i in 0..n {
            ps[i] = (1.0 - cs) * ps[i] + k_s * zmean[i];
        }
        let ps_norm = ps.iter().map(|v| v * v).sum::<f64>().sqrt();
        let hsig = ps_norm / (1.0 - (1.0 - cs).powi(2 * (generation as i32 + 1))).sqrt()
            < (1.4 + 2.0 / (nf + 1.0)) * chi_n;
        let k_c = (cc * (2.0 - cc) * mueff).sqrt();
        for i in 0..n {
            let step = (mean[i] - old[i]) / sigma;
            pc[i] = (1.0 - cc) * pc[i] + if hsig { k_c * step } else { 0.0 };
            let rank_mu: f64 = (0..mu)
                .map(|k| {
                    let y = (xs[order[k]][i] - old[i]) / sigma;
                    w[k] * y * y
                })
                .sum();
            let correction = if hsig { 0.0 } else { cc * (2.0 - cc) * c[i] };
            c[i] = (1.0 - c1 - cmu) * c[i] + c1 * (pc[i] * pc[i] + correction) + cmu * rank_mu;
            c[i] = c[i].clamp(1e-8, 1e4);
        }
        sigma *= ((cs / ds) * (ps_norm / chi_n - 1.0)).exp();
        sigma = sigma.clamp(1e-6, 1.0);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_minimum_of_a_shifted_ellipsoid() {
        let target = [0.2, 0.7, 0.5, 0.9, 0.1];
        let f = |x: &[f64]| -> f64 {
            x.iter()
                .zip(target)
                .enumerate()
                .map(|(i, (v, t))| (i + 1) as f64 * (v - t).powi(2))
                .sum()
        };
        let found = minimize(
            &f,
            &[0.5; 5],
            Settings {
                population: 12,
                generations: 80,
                sigma: 0.3,
                seed: 1,
                threads: 2,
            },
        );
        for (v, t) in found.x.iter().zip(target) {
            assert!((v - t).abs() < 0.01, "{:?}", found.x);
        }
    }

    #[test]
    fn stays_inside_the_unit_cube() {
        // Minimum outside the cube: the answer is the nearest corner.
        let f = |x: &[f64]| -> f64 { x.iter().map(|v| (v - 2.0).powi(2)).sum() };
        let found = minimize(
            &f,
            &[0.5; 3],
            Settings {
                population: 8,
                generations: 40,
                sigma: 0.3,
                seed: 2,
                threads: 1,
            },
        );
        assert!(found.x.iter().all(|&v| (0.0..=1.0).contains(&v)));
        assert!(found.x.iter().all(|&v| v > 0.95), "{:?}", found.x);
    }
}
