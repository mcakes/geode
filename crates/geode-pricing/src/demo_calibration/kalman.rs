//! The linear algebra behind the demo `kalman_fit`: a knot basis whose
//! observation row interpolates linearly between the two knots around a
//! tenor and holds flat outside, a regularised least-squares start, and a
//! linear Kalman update applied one measurement at a time. Measurement
//! noise is diagonal, so the sequential scalar form is exactly the batch
//! update without inverting anything larger than a number.

/// The observation row for `t`: the weights of each knot, summing to one.
/// Between two knots they split linearly; at or beyond either end knot the
/// end knot carries the whole weight.
pub(crate) fn weights(knots: &[f64], t: f64) -> Vec<f64> {
    let mut row = vec![0.0; knots.len()];
    let Some(last) = knots.len().checked_sub(1) else {
        return row;
    };
    if t <= knots[0] {
        row[0] = 1.0;
        return row;
    }
    if t >= knots[last] {
        row[last] = 1.0;
        return row;
    }
    let i = knots
        .iter()
        .rposition(|k| *k <= t)
        .unwrap_or(0)
        .min(last - 1);
    let (a, b) = (knots[i], knots[i + 1]);
    let w = if b > a { (t - a) / (b - a) } else { 0.0 };
    row[i] = 1.0 - w;
    row[i + 1] = w;
    row
}

/// `values` on `knots` read at `t`, the same piecewise-linear rule as
/// `weights`. `knots` ascend; `values` is parallel to it.
pub(crate) fn interpolate(knots: &[f64], values: &[f64], t: f64) -> f64 {
    weights(knots, t)
        .iter()
        .zip(values)
        .map(|(w, v)| w * v)
        .sum()
}

/// How strongly the least-squares start ties an unobserved knot to its
/// neighbours: small enough never to bend an observed curve visibly.
const SMOOTHNESS: f64 = 1.0e-6;

/// The knot values minimising `|Hx − y|² + λ|Dx|²`, `D` the first
/// difference between neighbouring knots. The penalty makes the system
/// solvable with fewer points than knots and gives a knot no point reaches
/// the value of its nearest observed neighbour instead of zero.
pub(crate) fn least_squares(knots: &[f64], tenors: &[f64], ys: &[f64]) -> Vec<f64> {
    let n = knots.len();
    let mut a = vec![vec![0.0; n]; n];
    let mut b = vec![0.0; n];
    for (t, y) in tenors.iter().zip(ys) {
        let h = weights(knots, *t);
        for i in 0..n {
            b[i] += h[i] * y;
            for j in 0..n {
                a[i][j] += h[i] * h[j];
            }
        }
    }
    for i in 1..n {
        a[i - 1][i - 1] += SMOOTHNESS;
        a[i][i] += SMOOTHNESS;
        a[i - 1][i] -= SMOOTHNESS;
        a[i][i - 1] -= SMOOTHNESS;
    }
    solve(a, b).unwrap_or_else(|| vec![0.0; n])
}

/// The state of one fit between runs: the knot values and their
/// covariance.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Filter {
    pub mean: Vec<f64>,
    pub cov: Vec<Vec<f64>>,
}

impl Filter {
    /// A filter at `mean` with `variance` on every knot and no
    /// correlation between knots.
    pub fn new(mean: Vec<f64>, variance: f64) -> Filter {
        let n = mean.len();
        let mut cov = vec![vec![0.0; n]; n];
        for (i, row) in cov.iter_mut().enumerate() {
            row[i] = variance;
        }
        Filter { mean, cov }
    }

    /// The time step: the state keeps its mean and gains `process_noise`
    /// of variance on every knot.
    pub fn predict(&mut self, process_noise: f64) {
        for (i, row) in self.cov.iter_mut().enumerate() {
            row[i] += process_noise;
        }
    }

    /// Absorbs the measurements `(tenor, y, variance)` one at a time and
    /// answers the innovation norm: the length of `y − Hx` against the
    /// mean before any of them was applied.
    pub fn update(&mut self, knots: &[f64], tenors: &[f64], ys: &[f64], variances: &[f64]) -> f64 {
        let n = self.mean.len();
        let innovation = tenors
            .iter()
            .zip(ys)
            .map(|(t, y)| {
                let r = y - interpolate(knots, &self.mean, *t);
                r * r
            })
            .sum::<f64>()
            .sqrt();
        for ((t, y), r) in tenors.iter().zip(ys).zip(variances) {
            let h = weights(knots, *t);
            // ph = P hᵀ; s = h P hᵀ + r; k = ph / s.
            let ph: Vec<f64> = (0..n)
                .map(|i| (0..n).map(|j| self.cov[i][j] * h[j]).sum())
                .collect();
            let s: f64 = h.iter().zip(&ph).map(|(a, b)| a * b).sum::<f64>() + r;
            if s <= 0.0 || !s.is_finite() {
                continue;
            }
            let k: Vec<f64> = ph.iter().map(|v| v / s).collect();
            let residual = y - h.iter().zip(&self.mean).map(|(a, b)| a * b).sum::<f64>();
            for (m, g) in self.mean.iter_mut().zip(&k) {
                *m += g * residual;
            }
            // P ← (I − k h) P; with P symmetric, (h P)ⱼ = (P hᵀ)ⱼ = phⱼ.
            for (row, g) in self.cov.iter_mut().zip(&k) {
                for (p, q) in row.iter_mut().zip(&ph) {
                    *p -= g * q;
                }
            }
        }
        innovation
    }
}

/// Gaussian elimination with partial pivoting on a small dense system;
/// `None` when a pivot vanishes.
fn solve(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let pivot = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-300 {
            return None;
        }
        a.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..n {
            let f = a[row][col] / a[col][col];
            if f == 0.0 {
                continue;
            }
            let pivot_row = a[col].clone();
            for (x, p) in a[row][col..].iter_mut().zip(&pivot_row[col..]) {
                *x -= f * p;
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOTS: [f64; 4] = [0.5, 1.0, 2.0, 4.0];

    #[test]
    fn weights_split_linearly_between_knots_and_hold_flat_outside() {
        assert_eq!(weights(&KNOTS, 0.1), vec![1.0, 0.0, 0.0, 0.0]);
        assert_eq!(weights(&KNOTS, 9.0), vec![0.0, 0.0, 0.0, 1.0]);
        assert_eq!(weights(&KNOTS, 1.0), vec![0.0, 1.0, 0.0, 0.0]);
        let w = weights(&KNOTS, 1.5);
        assert!((w[1] - 0.5).abs() < 1e-12 && (w[2] - 0.5).abs() < 1e-12);
        let w = weights(&KNOTS, 3.0);
        assert!((w[2] - 0.5).abs() < 1e-12 && (w[3] - 0.5).abs() < 1e-12);
        assert!(weights(&[], 1.0).is_empty());
    }

    #[test]
    fn least_squares_recovers_a_line_through_the_knots() {
        // Points inside the knot range: outside it the basis is flat and
        // a line is not representable.
        let tenors = [0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];
        let ys: Vec<f64> = tenors.iter().map(|t| 0.01 + 0.002 * t).collect();
        let x = least_squares(&KNOTS, &tenors, &ys);
        for (k, v) in KNOTS.iter().zip(&x) {
            assert!((v - (0.01 + 0.002 * k)).abs() < 1e-6, "{k}: {v}");
        }
    }

    #[test]
    fn an_unobserved_knot_takes_its_neighbours_value() {
        // Only the first two knots see a point; the last two follow them
        // instead of dropping to zero.
        let x = least_squares(&KNOTS, &[0.5, 1.0], &[0.02, 0.02]);
        for v in &x {
            assert!((v - 0.02).abs() < 1e-6, "{x:?}");
        }
    }

    #[test]
    fn an_update_moves_the_mean_toward_the_measurement_by_the_gain() {
        let mut f = Filter::new(vec![0.0; 4], 1.0);
        let innovation = f.update(&KNOTS, &[1.0], &[1.0], &[1.0]);
        assert!((innovation - 1.0).abs() < 1e-12);
        // Equal prior and measurement variance: the mean moves half way
        // and the variance halves, on the observed knot alone.
        assert!((f.mean[1] - 0.5).abs() < 1e-12, "{:?}", f.mean);
        assert_eq!(f.mean[0], 0.0);
        assert!((f.cov[1][1] - 0.5).abs() < 1e-12);
        assert_eq!(f.cov[0][0], 1.0);
        f.predict(0.25);
        assert!((f.cov[1][1] - 0.75).abs() < 1e-12);
    }

    #[test]
    fn a_very_noisy_measurement_leaves_the_mean_where_it_was() {
        let mut f = Filter::new(vec![0.01; 4], 1e-4);
        f.update(&KNOTS, &[2.0], &[5.0], &[1e12]);
        assert!((f.mean[2] - 0.01).abs() < 1e-9);
    }
}
