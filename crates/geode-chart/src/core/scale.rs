//! Value-to-pixel mapping on a y axis, with 1-2-5 nice ticks.

/// `lo` maps to `bottom`, `hi` to `top` (pixel y grows downward).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearScale {
    pub lo: f64,
    pub hi: f64,
    pub top: f32,
    pub bottom: f32,
}

/// Padding either side of a data domain, as a fraction of its span.
pub const DOMAIN_PAD: f64 = 0.05;

impl LinearScale {
    /// A non-finite or empty domain is made a unit span around its
    /// finite end (or zero) so a flat series still paints mid-pane.
    pub fn new(domain: (f64, f64), top: f32, bottom: f32) -> Self {
        let (mut lo, mut hi) = domain;
        if !lo.is_finite() {
            lo = if hi.is_finite() { hi } else { 0.0 };
        }
        if !hi.is_finite() {
            hi = lo;
        }
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        if hi - lo <= 0.0 {
            lo -= 1.0;
            hi += 1.0;
        }
        Self {
            lo,
            hi,
            top,
            bottom,
        }
    }

    pub fn y(&self, value: f64) -> f32 {
        let t = ((value - self.lo) / (self.hi - self.lo)) as f32;
        self.bottom - t * (self.bottom - self.top)
    }

    pub fn value(&self, y: f32) -> f64 {
        let t = ((self.bottom - y) / (self.bottom - self.top)) as f64;
        self.lo + t * (self.hi - self.lo)
    }

    /// The 1-2-5 step that gives about `count` ticks over `span`.
    pub fn nice_step(span: f64, count: usize) -> f64 {
        let raw = span / count.max(1) as f64;
        let magnitude = 10f64.powf(raw.log10().floor());
        let residual = raw / magnitude;
        let factor = if residual <= 1.0 {
            1.0
        } else if residual <= 2.0 {
            2.0
        } else if residual <= 5.0 {
            5.0
        } else {
            10.0
        };
        factor * magnitude
    }

    /// Nice ticks inside `[lo, hi]`, about `count_hint` of them, into
    /// `out` (cleared first). A zero hint yields none.
    pub fn ticks(&self, count_hint: usize, out: &mut Vec<f64>) {
        out.clear();
        if count_hint == 0 {
            return;
        }
        let step = Self::nice_step(self.hi - self.lo, count_hint);
        if step <= 0.0 || !step.is_finite() {
            return;
        }
        let first = (self.lo / step).ceil();
        let last = (self.hi / step).floor();
        let mut k = first;
        while k <= last {
            // Round to the step's own decimals so 0.1 * 3 reads 0.3. Adding
            // zero turns the `-0.0` of a `lo` just under zero into `0.0`, so
            // the tick never reads `-0`.
            let v = (k * step * 1e9).round() / 1e9 + 0.0;
            out.push(v);
            k += 1.0;
        }
    }

    pub fn step_for(&self, count_hint: usize) -> f64 {
        Self::nice_step(self.hi - self.lo, count_hint.max(1))
    }
}

/// A tick label with the decimals its step needs and no more.
pub fn fmt_tick(value: f64, step: f64) -> String {
    let decimals = if step >= 1.0 || step <= 0.0 || !step.is_finite() {
        0
    } else {
        (-step.log10().floor()) as usize
    };
    format!("{value:.decimals$}")
}

/// A ratio's tick label as a percent, with the decimals its step needs in
/// percent: `0.205` at a step of `0.005` reads `20.5%`.
pub fn fmt_percent(value: f64, step: f64) -> String {
    format!("{}%", fmt_tick(value * 100.0, step * 100.0))
}

/// A readout value: 2 decimals from 100 up, 4 from 1 up, 6 below.
pub fn fmt_value(value: f64) -> String {
    if value.is_nan() {
        return "—".to_string();
    }
    let a = value.abs();
    if a >= 100.0 {
        format!("{value:.2}")
    } else if a >= 1.0 {
        format!("{value:.4}")
    } else {
        format!("{value:.6}")
    }
}

/// The finite min/max of `values`, padded by [`DOMAIN_PAD`] of the span
/// (a unit either side when flat); `None` when nothing is finite.
pub fn axis_domain(values: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for v in values {
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if lo > hi {
        return None;
    }
    let pad = if hi > lo { (hi - lo) * DOMAIN_PAD } else { 1.0 };
    Some((lo - pad, hi + pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_domain_onto_the_range_bottom_up() {
        let s = LinearScale::new((0.0, 100.0), 10.0, 110.0);
        assert_eq!(s.y(0.0), 110.0);
        assert_eq!(s.y(100.0), 10.0);
        assert_eq!(s.y(50.0), 60.0);
        assert!((s.value(60.0) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn a_degenerate_domain_is_padded_so_a_flat_series_still_paints() {
        let s = LinearScale::new((5.0, 5.0), 0.0, 100.0);
        assert!(s.lo < 5.0 && s.hi > 5.0);
        assert_eq!(s.y(5.0), 50.0);
        let s = LinearScale::new((f64::NAN, 1.0), 0.0, 100.0);
        assert!(s.lo.is_finite() && s.hi.is_finite() && s.lo < s.hi);
    }

    #[test]
    fn ticks_step_by_one_two_five_and_stay_inside_the_domain() {
        let s = LinearScale::new((0.0, 100.0), 0.0, 200.0);
        let mut out = Vec::new();
        s.ticks(5, &mut out);
        assert_eq!(out, vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
        let s = LinearScale::new((0.13, 0.87), 0.0, 200.0);
        s.ticks(4, &mut out);
        assert_eq!(out, vec![0.2, 0.4, 0.6, 0.8]);
        for w in out.windows(2) {
            assert!(w[1] > w[0]);
        }
        assert_eq!(LinearScale::nice_step(100.0, 5), 20.0);
        assert_eq!(LinearScale::nice_step(100.0, 3), 50.0);
        assert_eq!(LinearScale::nice_step(0.74, 4), 0.2);
        assert_eq!(LinearScale::nice_step(7.0, 7), 1.0);
    }

    #[test]
    fn ticks_with_a_zero_hint_are_empty_not_a_division_by_zero() {
        let s = LinearScale::new((0.0, 1.0), 0.0, 10.0);
        let mut out = vec![1.0];
        s.ticks(0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_tick_at_zero_is_positive_zero() {
        // `lo` within one step below zero: `ceil(-0.5)` is `-0.0`, and a
        // negative zero would format as `-0`.
        let s = LinearScale::new((-0.05, 0.3), 0.0, 200.0);
        assert_eq!(s.step_for(4), 0.1);
        let mut out = Vec::new();
        s.ticks(4, &mut out);
        assert_eq!(out[0], 0.0);
        assert!(out[0].is_sign_positive(), "{:?}", out[0]);
        assert_eq!(fmt_tick(out[0], 0.1), "0.0");
    }

    #[test]
    fn tick_labels_carry_the_steps_decimals() {
        assert_eq!(fmt_tick(20.0, 20.0), "20");
        assert_eq!(fmt_tick(0.6, 0.2), "0.6");
        assert_eq!(fmt_tick(1250.0, 250.0), "1250");
        assert_eq!(fmt_tick(0.05, 0.05), "0.05");
        assert_eq!(fmt_tick(-0.5, 0.5), "-0.5");
    }

    #[test]
    fn a_percent_label_carries_the_decimals_of_its_step_in_percent() {
        assert_eq!(fmt_percent(0.2, 0.05), "20%");
        assert_eq!(fmt_percent(0.205, 0.005), "20.5%");
        assert_eq!(fmt_percent(1.1, 0.1), "110%");
        assert_eq!(fmt_percent(0.07, 0.01), "7%");
        assert_eq!(fmt_percent(0.0, 0.05), "0%");
        assert_eq!(fmt_percent(-0.05, 0.05), "-5%");
    }

    #[test]
    fn readout_values_take_more_decimals_as_they_shrink() {
        assert_eq!(fmt_value(f64::NAN), "—");
        assert_eq!(fmt_value(4512.3456), "4512.35");
        assert_eq!(fmt_value(12.3456789), "12.3457");
        assert_eq!(fmt_value(0.123456789), "0.123457");
        assert_eq!(fmt_value(-0.5), "-0.500000");
    }

    #[test]
    fn axis_domain_pads_the_finite_extremes_and_ignores_nan() {
        let d = axis_domain([f64::NAN, 10.0, 30.0, f64::NAN].iter().copied()).unwrap();
        assert!((d.0 - 9.0).abs() < 1e-9, "{d:?}");
        assert!((d.1 - 31.0).abs() < 1e-9, "{d:?}");
        assert_eq!(axis_domain([f64::NAN].iter().copied()), None);
        assert_eq!(axis_domain(std::iter::empty()), None);
        let flat = axis_domain([7.0, 7.0].iter().copied()).unwrap();
        assert!(flat.0 < 7.0 && flat.1 > 7.0);
    }
}
