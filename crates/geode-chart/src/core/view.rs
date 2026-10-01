//! The visible window along x, in the caller's units: an index window
//! under `Session` (bucket `i` occupies `[i, i+1)`), a micros window under
//! `Continuous`, a strike or moneyness window on a linear axis.
//! Unit-agnostic: every method takes the loaded `full` range in the same
//! units and clamps to it, and the view carries the narrowest span a zoom
//! may reach in those units.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub lo: f64,
    pub hi: f64,
    /// Narrowest window a zoom can reach, in the view's own units.
    pub min_span: f64,
}

/// The default narrowest window: two buckets, or two micros.
pub const MIN_SPAN: f64 = 2.0;

impl View {
    pub fn full(full: (f64, f64)) -> Self {
        Self::with_min_span(full, MIN_SPAN)
    }

    /// The whole range with a caller-chosen narrowest window, for an axis
    /// whose units are not buckets. A non-finite or negative minimum means
    /// none.
    pub fn with_min_span(full: (f64, f64), min_span: f64) -> Self {
        Self {
            lo: full.0,
            hi: full.1,
            min_span: if min_span.is_finite() && min_span > 0.0 {
                min_span
            } else {
                0.0
            },
        }
    }
    pub fn span(&self) -> f64 {
        self.hi - self.lo
    }
    pub fn reset(&mut self, full: (f64, f64)) {
        *self = Self::with_min_span(full, self.min_span);
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
        let width = (self.span() / factor).max(self.min_span);
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
    /// `min_span` unless `full` itself is.
    fn clamp(&mut self, full: (f64, f64)) {
        let full_w = (full.1 - full.0).max(0.0);
        let mut w = self.span().max(self.min_span.min(full_w)).min(full_w);
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
        let mut v = View {
            lo: 40.0,
            hi: 60.0,
            min_span: MIN_SPAN,
        };
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
        let mut v = View {
            lo: 0.0,
            hi: 100.0,
            min_span: MIN_SPAN,
        };
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
        let mut v = View {
            lo: 40.0,
            hi: 60.0,
            min_span: MIN_SPAN,
        };
        v.jump_end(FULL);
        assert_eq!((v.lo, v.hi), (80.0, 100.0));
        v.jump_start(FULL);
        assert_eq!((v.lo, v.hi), (0.0, 20.0));
    }

    #[test]
    fn the_key_changes_with_the_window() {
        let a = View {
            lo: 0.0,
            hi: 1.0,
            min_span: MIN_SPAN,
        }
        .key();
        let b = View {
            lo: 0.0,
            hi: 2.0,
            min_span: MIN_SPAN,
        }
        .key();
        assert_ne!(a, b);
        assert_eq!(
            a,
            View {
                lo: 0.0,
                hi: 1.0,
                min_span: MIN_SPAN,
            }
            .key()
        );
    }

    #[test]
    fn a_view_narrows_to_its_own_min_span() {
        let full = (0.8, 1.2);
        let mut v = View::with_min_span(full, 0.01);
        for _ in 0..40 {
            v.zoom(2.0, 0.5, full);
        }
        assert!((v.span() - 0.01).abs() < 1e-12, "{v:?}");
        // The default is unchanged: two units.
        assert_eq!(View::full((0.0, 100.0)).min_span, MIN_SPAN);
    }

    #[test]
    fn reset_keeps_the_views_own_min_span() {
        let full = (0.8, 1.2);
        let mut v = View::with_min_span(full, 0.01);
        v.zoom(4.0, 0.5, full);
        v.reset(full);
        assert_eq!((v.lo, v.hi), full);
        assert_eq!(v.min_span, 0.01);
        for _ in 0..40 {
            v.zoom(2.0, 0.5, full);
        }
        assert!((v.span() - 0.01).abs() < 1e-12, "{v:?}");
    }

    #[test]
    fn a_nonsense_min_span_means_no_floor() {
        // "None" is no floor at all, not a view that never zooms: the window
        // narrows well past what the default two-unit floor would allow.
        for nonsense in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            let mut v = View::with_min_span(FULL, nonsense);
            v.zoom(1e3, 0.5, FULL);
            assert!(v.lo.is_finite() && v.hi.is_finite(), "{nonsense}: {v:?}");
            assert!((v.span() - 0.1).abs() < 1e-9, "{nonsense}: {v:?}");
            assert!(v.span() < MIN_SPAN);
            assert_eq!(v.min_span, 0.0, "{nonsense}");
        }
    }
}
