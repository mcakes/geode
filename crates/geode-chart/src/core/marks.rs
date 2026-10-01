//! Stroke geometry for marks that are not a plain polyline: the dashes of a
//! dashed line, and the bar and diamond of a point with a range. Pure:
//! pixel points in, stroke segments out.

use super::Point;

/// One stroke segment, in layout pixels.
pub type Segment = (Point, Point);

/// Half a marker diamond's diagonal, in design pixels.
pub const MARKER_R: f32 = 3.0;

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Cut a polyline into the "on" segments of a `dash`-on, `gap`-off pattern,
/// into `out` (cleared first). The pattern's phase runs on along the line,
/// corners included, and restarts after a break. A `dash` of zero or less
/// means solid: one segment per span. So does a pattern with no finite
/// period.
pub fn dash_polyline(points: &[Point], dash: f32, gap: f32, out: &mut Vec<Segment>) {
    out.clear();
    let period = dash + gap.max(0.0);
    let solid = !period.is_finite() || dash <= 0.0;
    let mut phase = 0.0f32;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a.is_break() || b.is_break() {
            phase = 0.0;
            continue;
        }
        if solid {
            out.push((a, b));
            continue;
        }
        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        if !len.is_finite() || len <= 0.0 {
            continue;
        }
        // Along this span dash `k` is on over `[start, start + dash)` with
        // `start = k * period - phase`. Each dash is placed from its own
        // index: walking a running distance forward by float additions
        // strands the rest of the span once a step rounds to no movement.
        let dashes = ((len + phase) / period).ceil() as usize;
        for k in 0..dashes {
            let start = k as f32 * period - phase;
            let (from, to) = (start.max(0.0), (start + dash).min(len));
            if to > from {
                out.push((lerp(a, b, from / len), lerp(a, b, to / len)));
            }
        }
        phase = (phase + len) % period;
    }
}

/// The stroke segments of a points slot, into `out` (cleared first): per
/// point, a vertical bar from `lo` to `hi` when the range has height, and a
/// diamond of half-diagonal `r` at `mid`. All inputs are pixels; a
/// non-finite x skips the point, a non-finite `mid` its diamond, a
/// non-finite `lo` or `hi` its bar. Only the shared length of the four
/// slices is read. Returns `(bars, markers)`.
pub fn point_marks(
    xs: &[f32],
    mid: &[f32],
    lo: &[f32],
    hi: &[f32],
    r: f32,
    out: &mut Vec<Segment>,
) -> (usize, usize) {
    out.clear();
    let n = xs.len().min(mid.len()).min(lo.len()).min(hi.len());
    let (mut bars, mut markers) = (0, 0);
    for i in 0..n {
        let x = xs[i];
        if !x.is_finite() {
            continue;
        }
        if lo[i].is_finite() && hi[i].is_finite() && lo[i] != hi[i] {
            out.push((Point::new(x, lo[i]), Point::new(x, hi[i])));
            bars += 1;
        }
        let m = mid[i];
        if m.is_finite() {
            let tips = [
                Point::new(x - r, m),
                Point::new(x, m - r),
                Point::new(x + r, m),
                Point::new(x, m + r),
            ];
            for k in 0..4 {
                out.push((tips[k], tips[(k + 1) % 4]));
            }
            markers += 1;
        }
    }
    (bars, markers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(p: Point, x: f32, y: f32) -> bool {
        (p.x - x).abs() < 1e-3 && (p.y - y).abs() < 1e-3
    }

    fn length(s: &[Segment]) -> f32 {
        s.iter()
            .map(|(a, b)| ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt())
            .sum()
    }

    #[test]
    fn a_straight_run_is_cut_into_dashes() {
        let pts = [Point::new(0.0, 5.0), Point::new(100.0, 5.0)];
        let mut out = Vec::new();
        dash_polyline(&pts, 4.0, 3.0, &mut out);
        assert_eq!(out.len(), 15, "ceil(100 / 7) dashes");
        assert!(
            near(out[0].0, 0.0, 5.0) && near(out[0].1, 4.0, 5.0),
            "{:?}",
            out[0]
        );
        assert!(near(out[1].0, 7.0, 5.0), "{:?}", out[1]);
        // 14 full dashes and 2 px of the fifteenth.
        assert!((length(&out) - 58.0).abs() < 1e-3, "{}", length(&out));
    }

    #[test]
    fn the_dash_phase_carries_round_a_corner_and_restarts_after_a_break() {
        // 5 px right, then 5 px down: a 4-on 3-off pattern is on for
        // 0..4 and 7..10, so the second dash lies wholly in the second leg.
        let corner = [
            Point::new(0.0, 0.0),
            Point::new(5.0, 0.0),
            Point::new(5.0, 5.0),
        ];
        let mut out = Vec::new();
        dash_polyline(&corner, 4.0, 3.0, &mut out);
        assert_eq!(out.len(), 2);
        assert!(
            near(out[1].0, 5.0, 2.0) && near(out[1].1, 5.0, 5.0),
            "{:?}",
            out[1]
        );
        // A break restarts the pattern: each 2 px run begins on a dash.
        let broken = [
            Point::new(0.0, 0.0),
            Point::new(2.0, 0.0),
            Point::BREAK,
            Point::new(10.0, 0.0),
            Point::new(12.0, 0.0),
        ];
        dash_polyline(&broken, 4.0, 3.0, &mut out);
        assert_eq!(out.len(), 2);
        assert!(near(out[1].0, 10.0, 0.0), "{:?}", out[1]);
    }

    #[test]
    fn a_long_span_off_the_pixel_grid_keeps_every_dash() {
        // A rem-scaled pattern over fractional pixels, the way a chart
        // paints it: a short lead span leaves a phase behind, and the long
        // span after it must still be dashed to its far end.
        let (dash, gap) = (4.0 * 14.0 / 12.0, 3.0 * 14.0 / 12.0);
        let pts = [
            Point::new(115.41, 7.0),
            Point::new(130.39, 7.0),
            Point::new(311.83, 7.0),
        ];
        let mut out = Vec::new();
        dash_polyline(&pts, dash, gap, &mut out);
        // 196.42 px is 24 whole periods and 0.42 px of a twenty-fifth dash.
        let total = 311.83 - 115.41;
        let period = dash + gap;
        let whole = (total / period).floor();
        assert_eq!(whole, 24.0);
        let want = whole * dash + (total - whole * period).min(dash);
        assert!(
            (length(&out) - want).abs() < 0.01,
            "{} of {want}",
            length(&out)
        );
        let last = out.last().expect("dashes");
        assert!(near(last.1, 311.83, 7.0), "{last:?}");
    }

    #[test]
    fn no_dash_length_means_solid_segments() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(3.0, 0.0),
            Point::new(3.0, 3.0),
        ];
        let mut out = vec![(Point::new(9.0, 9.0), Point::new(9.0, 9.0))];
        dash_polyline(&pts, 0.0, 3.0, &mut out);
        assert_eq!(out.len(), 2, "cleared first, then one segment per span");
        dash_polyline(&[], 4.0, 3.0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn each_point_gets_a_bar_and_a_diamond() {
        let xs = [10.0, 20.0, 30.0];
        let mid = [50.0, 52.0, 54.0];
        let lo = [55.0, 57.0, 59.0];
        let hi = [45.0, 47.0, 49.0];
        let mut out = Vec::new();
        let (bars, markers) = point_marks(&xs, &mid, &lo, &hi, 3.0, &mut out);
        assert_eq!((bars, markers), (3, 3));
        assert_eq!(out.len(), 3 + 3 * 4, "one bar and four diamond edges each");
        assert_eq!(
            out[0],
            (Point::new(10.0, 55.0), Point::new(10.0, 45.0)),
            "the bar"
        );
        assert_eq!(
            out[1].0,
            Point::new(7.0, 50.0),
            "the diamond starts at its left tip"
        );
    }

    #[test]
    fn a_point_with_no_range_is_a_diamond_alone() {
        let mut out = Vec::new();
        let (bars, markers) = point_marks(&[10.0], &[50.0], &[50.0], &[50.0], 3.0, &mut out);
        assert_eq!((bars, markers), (0, 1));
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn a_nan_or_short_array_skips_the_mark_not_the_slot() {
        let nan = f32::NAN;
        let mut out = Vec::new();
        // Point 0: no mid, so a bar alone. Point 1: no x, so nothing.
        // Point 2: no lo, so a diamond alone. Point 3 has no hi entry at all.
        let (bars, markers) = point_marks(
            &[10.0, nan, 30.0, 40.0],
            &[nan, 50.0, 50.0, 50.0],
            &[55.0, 55.0, nan, 55.0],
            &[45.0, 45.0, 45.0],
            3.0,
            &mut out,
        );
        assert_eq!((bars, markers), (1, 1));
        assert_eq!(out.len(), 1 + 4);
        assert!(
            out.iter()
                .all(|(a, b)| a.x.is_finite() && a.y.is_finite() && b.y.is_finite())
        );
    }
}
