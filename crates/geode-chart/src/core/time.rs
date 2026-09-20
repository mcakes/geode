//! The x axis (spec §8.2): `Session` maps bucket INDEX to x so a span
//! with no bucket has no width; `Continuous` maps wall-clock micros.
//! Bucket `i` occupies `[i, i+1)` (session) or `[b_i, b_i + step)`
//! (continuous); its centre is where the point paints.

use super::Rect;
use super::view::View;

#[derive(Clone, Copy, Debug)]
pub enum TimeScale<'a> {
    Session { buckets: &'a [i64] },
    Continuous { buckets: &'a [i64], step_us: i64 },
}

/// The nearest bucket to a cursor (spec §8.1).
pub struct Crosshair;

impl<'a> TimeScale<'a> {
    pub fn buckets(&self) -> &'a [i64] {
        match self {
            TimeScale::Session { buckets } | TimeScale::Continuous { buckets, .. } => buckets,
        }
    }

    /// The loaded range in this scale's units: `(0, n)` for session,
    /// `(first, last + step)` for continuous; `(0, 0)` when empty.
    pub fn full(&self) -> (f64, f64) {
        match *self {
            TimeScale::Session { buckets } => (0.0, buckets.len() as f64),
            TimeScale::Continuous { buckets, step_us } => match (buckets.first(), buckets.last()) {
                (Some(&f), Some(&l)) => (f as f64, (l + step_us) as f64),
                _ => (0.0, 0.0),
            },
        }
    }

    /// Bucket `index`'s centre in its own units.
    fn centre(&self, index: usize) -> f64 {
        match *self {
            TimeScale::Session { .. } => index as f64 + 0.5,
            TimeScale::Continuous { buckets, step_us } => {
                buckets[index] as f64 + step_us as f64 / 2.0
            }
        }
    }

    fn plot_x(&self, u: f64, view: View, plot: Rect) -> f32 {
        let span = view.span();
        if span.is_nan() || span <= 0.0 {
            return plot.x;
        }
        plot.x + ((u - view.lo) / span) as f32 * plot.w
    }

    /// Bucket `index`'s x on the plot (its centre).
    pub fn x_of(&self, index: usize, view: View, plot: Rect) -> f32 {
        self.plot_x(self.centre(index), view, plot)
    }

    /// `[start, end)` of the buckets whose slot intersects the view.
    pub fn visible(&self, view: View) -> (usize, usize) {
        let b = self.buckets();
        if b.is_empty() {
            return (0, 0);
        }
        match *self {
            TimeScale::Session { .. } => {
                let start = view.lo.floor().max(0.0) as usize;
                let end = (view.hi.ceil().max(0.0) as usize).min(b.len());
                (start.min(end), end)
            }
            TimeScale::Continuous { buckets, step_us } => {
                // first bucket whose slot end is after view.lo
                let start = buckets.partition_point(|&t| ((t + step_us) as f64) <= view.lo);
                let end = buckets.partition_point(|&t| (t as f64) < view.hi);
                (start.min(end), end)
            }
        }
    }
}

impl Crosshair {
    /// The visible bucket whose centre is nearest `cursor_x`.
    pub fn at(cursor_x: f32, scale: &TimeScale, view: View, plot: Rect) -> Option<usize> {
        let (start, end) = scale.visible(view);
        if start >= end {
            return None;
        }
        // x is monotone in index: binary search the first centre at or
        // past the cursor, then compare with its predecessor.
        let mut lo = start;
        let mut hi = end;
        while lo < hi {
            let mid = (lo + hi) / 2;
            if scale.x_of(mid, view, plot) < cursor_x {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == start {
            return Some(start);
        }
        if lo == end {
            return Some(end - 1);
        }
        let before = (scale.x_of(lo - 1, view, plot) - cursor_x).abs();
        let after = (scale.x_of(lo, view, plot) - cursor_x).abs();
        Some(if after < before { lo } else { lo - 1 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PLOT: Rect = Rect::new(100.0, 0.0, 1000.0, 100.0);
    const DAY: i64 = 86_400_000_000;
    // Mon..Fri, then Mon (a weekend gap).
    const BUCKETS: [i64; 6] = [0, DAY, 2 * DAY, 3 * DAY, 4 * DAY, 7 * DAY];

    #[test]
    fn a_session_scale_gives_every_bucket_the_same_width_whatever_its_gap() {
        let s = TimeScale::Session { buckets: &BUCKETS };
        assert_eq!(s.full(), (0.0, 6.0));
        let v = View::full(s.full());
        let xs: Vec<f32> = (0..6).map(|i| s.x_of(i, v, PLOT)).collect();
        for w in xs.windows(2) {
            assert!((w[1] - w[0] - 1000.0 / 6.0).abs() < 1e-3, "{xs:?}");
        }
        assert!(
            (xs[0] - (100.0 + 1000.0 / 12.0)).abs() < 1e-3,
            "centred in its slot"
        );
    }

    #[test]
    fn a_continuous_scale_leaves_the_weekend_its_width() {
        let s = TimeScale::Continuous {
            buckets: &BUCKETS,
            step_us: DAY,
        };
        assert_eq!(s.full(), (0.0, 8.0 * DAY as f64));
        let v = View::full(s.full());
        let fri = s.x_of(4, v, PLOT);
        let mon = s.x_of(5, v, PLOT);
        let thu = s.x_of(3, v, PLOT);
        assert!(
            (mon - fri) > 2.5 * (fri - thu),
            "the gap is three days wide"
        );
    }

    #[test]
    fn visible_is_the_index_window_the_view_intersects() {
        let s = TimeScale::Session { buckets: &BUCKETS };
        assert_eq!(s.visible(View { lo: 0.0, hi: 6.0 }), (0, 6));
        assert_eq!(s.visible(View { lo: 1.5, hi: 3.5 }), (1, 4));
        assert_eq!(s.visible(View { lo: 2.0, hi: 3.0 }), (2, 3));
        let c = TimeScale::Continuous {
            buckets: &BUCKETS,
            step_us: DAY,
        };
        assert_eq!(
            c.visible(View {
                lo: 0.5 * DAY as f64,
                hi: 5.0 * DAY as f64
            }),
            (0, 5)
        );
        assert_eq!(
            c.visible(View {
                lo: 5.5 * DAY as f64,
                hi: 6.5 * DAY as f64
            }),
            (5, 5),
            "the gap holds no bucket"
        );
        let e = TimeScale::Session { buckets: &[] };
        assert_eq!(e.visible(View { lo: 0.0, hi: 1.0 }), (0, 0));
    }

    #[test]
    fn the_crosshair_picks_the_nearest_bucket() {
        let s = TimeScale::Session { buckets: &BUCKETS };
        let v = View::full(s.full());
        let x2 = s.x_of(2, v, PLOT);
        let x3 = s.x_of(3, v, PLOT);
        assert_eq!(Crosshair::at(x2 + 1.0, &s, v, PLOT), Some(2));
        assert_eq!(Crosshair::at(x3 - 1.0, &s, v, PLOT), Some(3));
        assert_eq!(Crosshair::at((x2 + x3) / 2.0 + 0.5, &s, v, PLOT), Some(3));
        assert_eq!(
            Crosshair::at(PLOT.x - 50.0, &s, v, PLOT),
            Some(0),
            "left of the plot snaps to the first"
        );
        assert_eq!(Crosshair::at(PLOT.right() + 50.0, &s, v, PLOT), Some(5));
        let zoomed = View { lo: 2.0, hi: 4.0 };
        assert_eq!(
            Crosshair::at(PLOT.x + 1.0, &s, zoomed, PLOT),
            Some(2),
            "only visible buckets"
        );
        assert_eq!(Crosshair::at(PLOT.right() - 1.0, &s, zoomed, PLOT), Some(3));
        assert_eq!(
            Crosshair::at(500.0, &TimeScale::Session { buckets: &[] }, v, PLOT),
            None
        );
    }
}
