//! Natural cubic spline through sorted knots, for the demo vol model's
//! smile in moneyness. Second derivatives are zero at both ends;
//! evaluation beyond the end knots continues the end slope, so a chain
//! strike outside the node ladder still gets a finite vol.

pub struct Spline {
    xs: Vec<f64>,
    ys: Vec<f64>,
    /// Second derivative at each knot; zero at both ends (natural).
    m: Vec<f64>,
}

impl Spline {
    /// `None` when fewer than two knots, mismatched lengths, or `xs` not
    /// strictly increasing.
    pub fn natural(xs: &[f64], ys: &[f64]) -> Option<Spline> {
        let n = xs.len();
        let ascending = |w: &[f64]| w[1].partial_cmp(&w[0]) == Some(std::cmp::Ordering::Greater);
        if n < 2 || ys.len() != n || !xs.windows(2).all(ascending) {
            return None;
        }
        let mut m = vec![0.0; n];
        if n > 2 {
            // Thomas algorithm on the (n-2)×(n-2) tridiagonal system.
            let h: Vec<f64> = xs.windows(2).map(|w| w[1] - w[0]).collect();
            let mut b: Vec<f64> = (1..n - 1).map(|i| 2.0 * (h[i - 1] + h[i])).collect();
            let mut d: Vec<f64> = (1..n - 1)
                .map(|i| 6.0 * ((ys[i + 1] - ys[i]) / h[i] - (ys[i] - ys[i - 1]) / h[i - 1]))
                .collect();
            for i in 1..n - 2 {
                let w = h[i] / b[i - 1];
                b[i] -= w * h[i];
                d[i] -= w * d[i - 1];
            }
            let last = n - 3;
            m[n - 2] = d[last] / b[last];
            for i in (0..last).rev() {
                m[i + 1] = (d[i] - h[i + 1] * m[i + 2]) / b[i];
            }
        }
        Some(Spline {
            xs: xs.to_vec(),
            ys: ys.to_vec(),
            m,
        })
    }

    pub fn eval(&self, x: f64) -> f64 {
        let n = self.xs.len();
        if x <= self.xs[0] {
            return self.ys[0] + self.slope_at_start() * (x - self.xs[0]);
        }
        if x >= self.xs[n - 1] {
            return self.ys[n - 1] + self.slope_at_end() * (x - self.xs[n - 1]);
        }
        let i = self.xs.partition_point(|k| *k <= x) - 1;
        let h = self.xs[i + 1] - self.xs[i];
        let a = (self.xs[i + 1] - x) / h;
        let b = (x - self.xs[i]) / h;
        a * self.ys[i]
            + b * self.ys[i + 1]
            + ((a * a * a - a) * self.m[i] + (b * b * b - b) * self.m[i + 1]) * h * h / 6.0
    }

    fn slope_at_start(&self) -> f64 {
        let h = self.xs[1] - self.xs[0];
        (self.ys[1] - self.ys[0]) / h - h * (2.0 * self.m[0] + self.m[1]) / 6.0
    }

    fn slope_at_end(&self) -> f64 {
        let n = self.xs.len();
        let h = self.xs[n - 1] - self.xs[n - 2];
        (self.ys[n - 1] - self.ys[n - 2]) / h + h * (self.m[n - 2] + 2.0 * self.m[n - 1]) / 6.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spline_passes_through_its_knots() {
        let xs = [-0.2, -0.1, 0.0, 0.05, 0.1];
        let ys = [0.35, 0.27, 0.2, 0.19, 0.21];
        let s = Spline::natural(&xs, &ys).unwrap();
        for (x, y) in xs.iter().zip(ys) {
            assert!((s.eval(*x) - y).abs() < 1e-12, "{x}");
        }
    }

    #[test]
    fn linear_data_is_reproduced_between_and_beyond_the_knots() {
        let xs = [0.0, 1.0, 2.0, 3.0];
        let ys = [1.0, 3.0, 5.0, 7.0];
        let s = Spline::natural(&xs, &ys).unwrap();
        for x in [0.5, 1.5, 2.25, -1.0, 4.0] {
            assert!((s.eval(x) - (1.0 + 2.0 * x)).abs() < 1e-9, "{x}");
        }
    }

    #[test]
    fn two_knots_make_a_line_and_bad_input_makes_nothing() {
        let s = Spline::natural(&[0.0, 2.0], &[0.0, 4.0]).unwrap();
        assert!((s.eval(1.0) - 2.0).abs() < 1e-12);
        assert!(Spline::natural(&[0.0], &[1.0]).is_none());
        assert!(Spline::natural(&[0.0, 1.0], &[1.0]).is_none());
        assert!(Spline::natural(&[0.0, 1.0, 1.0], &[1.0, 2.0, 3.0]).is_none());
        assert!(Spline::natural(&[1.0, 0.0], &[1.0, 2.0]).is_none());
    }

    #[test]
    fn a_natural_spline_is_smooth_at_an_interior_knot() {
        // The first derivative from the left equals the one from the right.
        let xs = [0.0, 1.0, 2.0, 3.0];
        let ys = [0.0, 1.0, 0.0, 1.0];
        let s = Spline::natural(&xs, &ys).unwrap();
        let h = 1e-6;
        let left = (s.eval(1.0) - s.eval(1.0 - h)) / h;
        let right = (s.eval(1.0 + h) - s.eval(1.0)) / h;
        assert!((left - right).abs() < 1e-4, "{left} {right}");
    }

    #[test]
    fn a_natural_spline_is_smooth_at_every_interior_knot_on_uneven_spacing() {
        // Uneven spacing with a curved ladder: every interior second
        // derivative is nonzero, so a wrong forward elimination (or an
        // h index swap) breaks C1 continuity at some knot past the first.
        let xs = [-0.2, -0.1, 0.0, 0.05, 0.1];
        let ys = [0.35, 0.27, 0.2, 0.19, 0.21];
        let s = Spline::natural(&xs, &ys).unwrap();
        let h = 1e-6;
        for x in &xs[1..xs.len() - 1] {
            let left = (s.eval(*x) - s.eval(x - h)) / h;
            let right = (s.eval(x + h) - s.eval(*x)) / h;
            assert!((left - right).abs() < 1e-4, "{x}: {left} {right}");
        }
    }
}
