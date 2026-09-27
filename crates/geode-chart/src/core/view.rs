//! The visible window along x: an index window under
//! `Session` (bucket `i` occupies `[i, i+1)`), a micros window under
//! `Continuous`. Unit-agnostic: every method takes the loaded `full`
//! range in the same units and clamps to it.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub lo: f64,
    pub hi: f64,
}

/// Narrowest window a zoom can reach (units): two buckets, or two micros.
pub const MIN_SPAN: f64 = 2.0;

impl View {
    pub fn full(full: (f64, f64)) -> Self {
        Self {
            lo: full.0,
            hi: full.1,
        }
    }
    pub fn span(&self) -> f64 {
        self.hi - self.lo
    }
    pub fn reset(&mut self, full: (f64, f64)) {
        *self = Self::full(full);
    }
    /// Shift by `fraction` of the current width (negative = left).
    pub fn pan(&mut self, fraction: f64, full: (f64, f64)) {
        let d = self.span() * fraction;
        self.lo += d;
        self.hi += d;
        self.clamp(full);
    }
    /// Narrow by `factor` (> 1 zooms in) keeping the point at `about`
    /// (0 = left edge, 1 = right edge) where it is.
    pub fn zoom(&mut self, factor: f64, about: f64, full: (f64, f64)) {
        if factor.is_nan() || factor <= 0.0 || !factor.is_finite() {
            return;
        }
        let about = about.clamp(0.0, 1.0);
        let pivot = self.lo + self.span() * about;
        let width = (self.span() / factor).max(MIN_SPAN);
        self.lo = pivot - width * about;
        self.hi = self.lo + width;
        self.clamp(full);
    }
    pub fn jump_start(&mut self, full: (f64, f64)) {
        let w = self.span();
        self.lo = full.0;
        self.hi = full.0 + w;
        self.clamp(full);
    }
    pub fn jump_end(&mut self, full: (f64, f64)) {
        let w = self.span();
        self.hi = full.1;
        self.lo = full.1 - w;
        self.clamp(full);
    }
    /// Never wider than `full`, never outside it, never narrower than
    /// `MIN_SPAN` unless `full` itself is.
    fn clamp(&mut self, full: (f64, f64)) {
        let full_w = (full.1 - full.0).max(0.0);
        let mut w = self.span().max(MIN_SPAN.min(full_w)).min(full_w);
        if !w.is_finite() {
            w = full_w;
        }
        if self.lo < full.0 {
            self.lo = full.0;
        }
        if self.lo + w > full.1 {
            self.lo = full.1 - w;
        }
        self.hi = self.lo + w;
    }
    /// A hashable identity for the path cache key.
    pub fn key(&self) -> (u64, u64) {
        (self.lo.to_bits(), self.hi.to_bits())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FULL: (f64, f64) = (0.0, 100.0);

    #[test]
    fn full_covers_the_range_and_reset_returns_to_it() {
        let mut v = View::full(FULL);
        assert_eq!((v.lo, v.hi), FULL);
        v.zoom(2.0, 0.5, FULL);
        assert_eq!((v.lo, v.hi), (25.0, 75.0));
        v.reset(FULL);
        assert_eq!((v.lo, v.hi), FULL);
    }

    #[test]
    fn a_pan_past_the_end_clamps() {
        let mut v = View { lo: 40.0, hi: 60.0 };
        v.pan(0.5, FULL);
        assert_eq!((v.lo, v.hi), (50.0, 70.0));
        v.pan(5.0, FULL);
        assert_eq!(
            (v.lo, v.hi),
            (80.0, 100.0),
            "keeps its width, stops at the end"
        );
        v.pan(-9.0, FULL);
        assert_eq!((v.lo, v.hi), (0.0, 20.0));
    }

    #[test]
    fn a_zoom_about_a_point_keeps_that_point_still_and_never_narrows_below_min_span() {
        let mut v = View { lo: 0.0, hi: 100.0 };
        v.zoom(2.0, 0.25, FULL);
        assert_eq!((v.lo, v.hi), (12.5, 62.5));
        for _ in 0..40 {
            v.zoom(2.0, 0.5, FULL);
        }
        assert!((v.hi - v.lo - MIN_SPAN).abs() < 1e-9, "{v:?}");
        v.zoom(0.001, 0.5, FULL);
        assert_eq!((v.lo, v.hi), FULL, "a zoom out never exceeds the range");
    }

    #[test]
    fn a_range_narrower_than_min_span_is_shown_whole() {
        let full = (0.0, 1.0);
        let mut v = View::full(full);
        v.zoom(4.0, 0.5, full);
        assert_eq!((v.lo, v.hi), full);
    }

    #[test]
    fn jumps_keep_the_width() {
        let mut v = View { lo: 40.0, hi: 60.0 };
        v.jump_end(FULL);
        assert_eq!((v.lo, v.hi), (80.0, 100.0));
        v.jump_start(FULL);
        assert_eq!((v.lo, v.hi), (0.0, 20.0));
    }

    #[test]
    fn the_key_changes_with_the_window() {
        let a = View { lo: 0.0, hi: 1.0 }.key();
        let b = View { lo: 0.0, hi: 2.0 }.key();
        assert_ne!(a, b);
        assert_eq!(a, View { lo: 0.0, hi: 1.0 }.key());
    }
}
