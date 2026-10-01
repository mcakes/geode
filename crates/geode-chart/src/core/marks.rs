//! Stroke geometry for marks that are not a plain polyline: the dashes of a
//! dashed line, and the bar and diamond of a point with a range. Pure:
//! pixel points in, stroke segments out.

use super::Point;

/// One stroke segment, in layout pixels.
pub type Segment = (Point, Point);

/// Half a marker diamond's diagonal, in design pixels.
pub const MARKER_R: f32 = 3.0;

/// A pure clip rectangle in layout pixels (x0 <= x1, y0 <= y1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// The point `t` of the way from `a` to `b`, worked in f64.
fn along(a: Point, b: Point, t: f64) -> Point {
    let (ax, ay) = (a.x as f64, a.y as f64);
    Point::new(
        (ax + (b.x as f64 - ax) * t) as f32,
        (ay + (b.y as f64 - ay) * t) as f32,
    )
}

/// The part of the span `a` to `b` inside `clip`, edges included, as a
/// parameter range `(t0, t1)` with `0 <= t0 <= t1 <= 1` (Liang–Barsky).
/// `None` when the span misses the rectangle, or the rectangle is inverted
/// or not finite.
fn clip_span(a: Point, b: Point, clip: Clip) -> Option<(f64, f64)> {
    let finite =
        clip.x0.is_finite() && clip.y0.is_finite() && clip.x1.is_finite() && clip.y1.is_finite();
    if !finite {
        return None;
    }
    let (ax, ay) = (a.x as f64, a.y as f64);
    let (dx, dy) = (b.x as f64 - ax, b.y as f64 - ay);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    // Each edge as `p * t <= q`: the span is inside it while that holds.
    for (p, q) in [
        (-dx, ax - clip.x0 as f64),
        (dx, clip.x1 as f64 - ax),
        (-dy, ay - clip.y0 as f64),
        (dy, clip.y1 as f64 - ay),
    ] {
        if p == 0.0 {
            // Parallel to this edge: wholly inside it or wholly outside.
            if q < 0.0 {
                return None;
            }
        } else if p < 0.0 {
            t0 = t0.max(q / p);
        } else {
            t1 = t1.min(q / p);
        }
    }
    (t0 <= t1).then_some((t0, t1))
}

/// How many times coarser than the float grid at the span's far end the
/// shorter of dash and gap must be for a dash to be placed along it.
const DASH_RESOLUTION: f64 = 64.0;

/// Cut a polyline into the "on" segments of a `dash`-on, `gap`-off pattern,
/// into `out` (cleared first). The pattern's phase runs on along the line,
/// corners included, and restarts after a break.
///
/// Dashes are emitted only inside `clip`, so one span yields at most
/// `ceil(clip diagonal / period) + 1` segments however long it is. A span's
/// off-clip part still advances the phase by its whole length: the dashes
/// inside the clip fall exactly where the unclipped line would put them.
/// The arithmetic is f64, which places dashes correctly along spans far
/// past what f32 can hold (1e14 px). Where even f64 cannot resolve the
/// pattern at the span's length, the clipped part of that span is drawn as
/// one solid segment.
///
/// The line is solid, one segment per span, when there is no pattern to
/// draw: `dash` or `gap` is zero, negative or NaN, the period is not
/// finite, or the period is under one pixel (it would read solid anyway).
/// Solid segments are not clipped and `clip` is ignored: one per span is
/// already bounded.
///
/// In either mode a span with a non-finite end (a finite x with a NaN or
/// infinite y, say) is never emitted and restarts the phase as a break
/// does, and a span of no length is skipped, leaving the phase as it was.
/// A clip that is inverted or not finite has no inside: nothing is dashed.
pub fn dash_polyline(points: &[Point], dash: f32, gap: f32, clip: Clip, out: &mut Vec<Segment>) {
    out.clear();
    let (dash, gap) = (dash as f64, gap as f64);
    let period = dash + gap;
    let dashed = dash > 0.0 && gap > 0.0 && period.is_finite() && period >= 1.0;
    let mut phase = 0.0f64;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a.is_break() || b.is_break() {
            phase = 0.0;
            continue;
        }
        let len = (b.x as f64 - a.x as f64).hypot(b.y as f64 - a.y as f64);
        if !len.is_finite() {
            phase = 0.0;
            continue;
        }
        if len <= 0.0 {
            continue;
        }
        if !dashed {
            out.push((a, b));
            continue;
        }
        if let Some((t0, t1)) = clip_span(a, b, clip) {
            // The clipped part, as distances along the span.
            let (d0, d1) = (t0 * len, t1 * len);
            if (len + phase) * f64::EPSILON * DASH_RESOLUTION > dash.min(gap) {
                if t1 > t0 {
                    out.push((along(a, b, t0), along(a, b, t1)));
                }
            } else {
                // Dash `k` is on over `[start, start + dash)` with
                // `start = k * period - phase`. Only the dashes that reach
                // `[d0, d1]` are visited, each placed from its own index:
                // walking a running distance forward by float additions
                // strands the rest of the span once a step rounds to no
                // movement.
                let first = ((d0 + phase - dash) / period).ceil().max(0.0);
                let last = ((d1 + phase) / period).floor();
                let count = (last - first + 1.0).max(0.0) as usize;
                for i in 0..count {
                    let start = (first + i as f64) * period - phase;
                    let (from, to) = (start.max(d0), (start + dash).min(d1));
                    if to > from {
                        out.push((along(a, b, from / len), along(a, b, to / len)));
                    }
                }
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

    /// A clip that holds every fixture it is used with.
    const ALL: Clip = Clip {
        x0: -10_000.0,
        y0: -10_000.0,
        x1: 10_000.0,
        y1: 10_000.0,
    };
    /// A 400 by 200 plot at the origin.
    const PLOT: Clip = Clip {
        x0: 0.0,
        y0: 0.0,
        x1: 400.0,
        y1: 200.0,
    };

    fn near(p: Point, x: f32, y: f32) -> bool {
        (p.x - x).abs() < 1e-3 && (p.y - y).abs() < 1e-3
    }

    fn length(s: &[Segment]) -> f32 {
        s.iter()
            .map(|(a, b)| ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt())
            .sum()
    }

    fn finite(s: &[Segment]) -> bool {
        s.iter()
            .all(|(a, b)| a.x.is_finite() && a.y.is_finite() && b.x.is_finite() && b.y.is_finite())
    }

    fn assert_same(got: &[Segment], want: &[Segment]) {
        assert_eq!(got.len(), want.len(), "{got:?}\n{want:?}");
        for (g, w) in got.iter().zip(want) {
            let close = |p: Point, q: Point| (p.x - q.x).abs() < 0.01 && (p.y - q.y).abs() < 0.01;
            assert!(close(g.0, w.0) && close(g.1, w.1), "{g:?} for {w:?}");
        }
    }

    #[test]
    fn a_straight_run_is_cut_into_dashes() {
        let pts = [Point::new(0.0, 5.0), Point::new(100.0, 5.0)];
        let mut out = Vec::new();
        dash_polyline(&pts, 4.0, 3.0, ALL, &mut out);
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
        dash_polyline(&corner, 4.0, 3.0, ALL, &mut out);
        assert_eq!(out.len(), 2);
        assert!(
            near(out[1].0, 5.0, 2.0) && near(out[1].1, 5.0, 5.0),
            "{:?}",
            out[1]
        );
        // A break restarts the pattern. The first run stops 5 px in, a pixel
        // into the gap; the second still opens on a whole dash at its first
        // point (a carried phase would open it at 12).
        let broken = [
            Point::new(0.0, 0.0),
            Point::new(5.0, 0.0),
            Point::BREAK,
            Point::new(10.0, 0.0),
            Point::new(20.0, 0.0),
        ];
        dash_polyline(&broken, 4.0, 3.0, ALL, &mut out);
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(
            near(out[1].0, 10.0, 0.0) && near(out[1].1, 14.0, 0.0),
            "{:?}",
            out[1]
        );
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
        dash_polyline(&pts, dash, gap, ALL, &mut out);
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
        dash_polyline(&pts, 0.0, 3.0, ALL, &mut out);
        assert_eq!(out.len(), 2, "cleared first, then one segment per span");
        dash_polyline(&[], 4.0, 3.0, ALL, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn a_pattern_too_fine_to_see_or_with_no_gap_is_solid() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 50.0),
        ];
        let solid = [(pts[0], pts[1]), (pts[1], pts[2])];
        let mut out = Vec::new();
        for (dash, gap) in [
            (0.001, 0.001),
            (0.4, 0.5),
            (4.0, 0.0),
            (4.0, -1.0),
            (4.0, f32::NAN),
            (f32::NAN, 3.0),
            (f32::INFINITY, 3.0),
            (4.0, f32::INFINITY),
        ] {
            dash_polyline(&pts, dash, gap, ALL, &mut out);
            assert_eq!(out, solid, "dash {dash}, gap {gap}");
        }
        // Solid is one segment per span whatever the clip.
        let elsewhere = Clip {
            x0: 500.0,
            y0: 500.0,
            x1: 600.0,
            y1: 600.0,
        };
        dash_polyline(&pts, 0.0, 3.0, elsewhere, &mut out);
        assert_eq!(out, solid);
    }

    #[test]
    fn a_non_finite_or_empty_span_is_never_emitted() {
        let nan = f32::NAN;
        let mut out = Vec::new();
        for bad in [nan, f32::INFINITY, f32::NEG_INFINITY] {
            // A finite x with no y is not a break, but no line reaches it.
            let pts = [
                Point::new(0.0, 0.0),
                Point::new(5.0, 0.0),
                Point::new(7.0, bad),
                Point::new(10.0, 0.0),
                Point::new(20.0, 0.0),
            ];
            dash_polyline(&pts, 0.0, 0.0, ALL, &mut out);
            assert_eq!(out, [(pts[0], pts[1]), (pts[3], pts[4])], "solid, {bad}");
            // Dashed, it restarts the pattern as a break does: the run
            // after it opens on a whole dash.
            dash_polyline(&pts, 4.0, 3.0, ALL, &mut out);
            assert!(finite(&out), "{bad}: {out:?}");
            assert_eq!(out.len(), 3, "{bad}: {out:?}");
            assert!(
                near(out[1].0, 10.0, 0.0) && near(out[1].1, 14.0, 0.0),
                "{bad}: {:?}",
                out[1]
            );
        }
        // A repeated point is no span: nothing solid, and the dash phase
        // runs on across it.
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(5.0, 0.0),
            Point::new(5.0, 0.0),
            Point::new(10.0, 0.0),
        ];
        dash_polyline(&pts, 0.0, 0.0, ALL, &mut out);
        assert_eq!(out, [(pts[0], pts[1]), (pts[2], pts[3])]);
        dash_polyline(&pts, 4.0, 3.0, ALL, &mut out);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(
            near(out[1].0, 7.0, 0.0) && near(out[1].1, 10.0, 0.0),
            "{:?}",
            out[1]
        );
    }

    #[test]
    fn a_span_far_longer_than_the_plot_is_dashed_only_inside_the_clip() {
        // 2e7 px of line through a 400 px plot at a 10 px period: the 40
        // dashes on the plot, not two million.
        let pts = [Point::new(-1e7, 100.0), Point::new(1e7, 100.0)];
        let mut out = Vec::new();
        dash_polyline(&pts, 6.0, 4.0, PLOT, &mut out);
        assert!(out.len() <= 60, "{}", out.len());
        assert_eq!(out.len(), 40);
        assert!(finite(&out));
        assert!(
            out.iter()
                .all(|(a, b)| a.x >= 0.0 && b.x <= 400.0 && b.x > a.x),
            "{out:?}"
        );
        assert!(
            near(out[0].0, 0.0, 100.0) && near(out[0].1, 6.0, 100.0),
            "{:?}",
            out[0]
        );
        // A span that misses the plot has no dashes at all.
        let pts = [Point::new(-1e7, 300.0), Point::new(1e7, 300.0)];
        dash_polyline(&pts, 6.0, 4.0, PLOT, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_span_past_the_reach_of_f32_stays_bounded_and_finite() {
        let mut out = Vec::new();
        // One end at 1e14: the dash arithmetic is f64, which still places
        // every dash on the plot.
        let pts = [Point::new(0.0, 100.0), Point::new(1e14, 100.0)];
        dash_polyline(&pts, 6.0, 4.0, PLOT, &mut out);
        assert_eq!(out.len(), 40);
        assert!(finite(&out));
        assert!(
            near(out[0].0, 0.0, 100.0) && near(out[0].1, 6.0, 100.0),
            "{:?}",
            out[0]
        );
        assert!(
            near(out[39].0, 390.0, 100.0) && near(out[39].1, 396.0, 100.0),
            "{:?}",
            out[39]
        );
        // One end at 1e30: f64 cannot place a 10 px pattern along that, so
        // the part on the plot is one solid segment.
        let pts = [Point::new(0.0, 100.0), Point::new(1e30, 100.0)];
        dash_polyline(&pts, 6.0, 4.0, PLOT, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            near(out[0].0, 0.0, 100.0) && near(out[0].1, 400.0, 100.0),
            "{:?}",
            out[0]
        );
        // Both ends out there: bounded and finite, whatever is drawn.
        let pts = [Point::new(-1e30, 100.0), Point::new(1e30, 100.0)];
        dash_polyline(&pts, 6.0, 4.0, PLOT, &mut out);
        assert!(out.len() <= 1 && finite(&out), "{out:?}");
    }

    #[test]
    fn clipping_does_not_move_the_pattern() {
        let (dash, gap) = (6.0, 4.0);
        let on_plot = [Point::new(0.0, 50.0), Point::new(400.0, 50.0)];
        let mut bare = Vec::new();
        dash_polyline(&on_plot, dash, gap, PLOT, &mut bare);
        assert_eq!(bare.len(), 40);
        // A lead-in of exactly 100,000 periods, all of it off the plot: the
        // dashes on the plot fall where they fall without it.
        let far = [Point::new(-1e6, 50.0), on_plot[0], on_plot[1]];
        let mut led = Vec::new();
        dash_polyline(&far, dash, gap, PLOT, &mut led);
        assert_same(&led, &bare);
        // A lead-in that stops 3 px into a dash. The clipped span still
        // advances the phase by its whole length, so the plot opens on the
        // 3 px left of that dash, exactly as the unclipped line does.
        let mid = [Point::new(-1003.0, 50.0), on_plot[0], on_plot[1]];
        let mut tight = Vec::new();
        dash_polyline(&mid, dash, gap, PLOT, &mut tight);
        let mut wide = Vec::new();
        dash_polyline(&mid, dash, gap, ALL, &mut wide);
        let wide_on_plot: Vec<Segment> = wide
            .iter()
            .filter(|(_, b)| b.x > 0.0)
            .map(|(a, b)| (Point::new(a.x.max(0.0), a.y), *b))
            .collect();
        assert_same(&tight, &wide_on_plot);
        assert!(
            near(tight[0].0, 0.0, 50.0) && near(tight[0].1, 3.0, 50.0),
            "{:?}",
            tight[0]
        );
    }

    #[test]
    fn a_diagonal_span_is_clipped_on_both_axes() {
        // y = x from -100 to 500 crosses the plot from (0, 0) to (200, 200):
        // distances 100√2 to 300√2 along the span.
        let pts = [Point::new(-100.0, -100.0), Point::new(500.0, 500.0)];
        let mut out = Vec::new();
        dash_polyline(&pts, 6.0, 4.0, PLOT, &mut out);
        let (d0, d1) = (100.0 * 2f32.sqrt(), 300.0 * 2f32.sqrt());
        let want: f32 = (0..100)
            .map(|k| {
                let start = k as f32 * 10.0;
                ((start + 6.0).min(d1) - start.max(d0)).max(0.0)
            })
            .sum();
        assert!(
            (length(&out) - want).abs() < 0.01,
            "{} of {want}",
            length(&out)
        );
        let inside = |p: Point| (-1e-3..=200.001).contains(&p.x) && (p.x - p.y).abs() < 1e-3;
        assert!(out.iter().all(|(a, b)| inside(*a) && inside(*b)), "{out:?}");
        // A span lying along the clip's edge is inside it.
        let edge = [Point::new(0.0, 200.0), Point::new(400.0, 200.0)];
        dash_polyline(&edge, 6.0, 4.0, PLOT, &mut out);
        assert_eq!(out.len(), 40);
        // A clip with no area to speak of, or none that is finite, has no
        // dashes.
        for clip in [
            Clip {
                x0: 400.0,
                x1: 0.0,
                ..PLOT
            },
            Clip {
                y1: f32::NAN,
                ..PLOT
            },
            Clip {
                x1: f32::INFINITY,
                ..PLOT
            },
        ] {
            dash_polyline(&edge, 6.0, 4.0, clip, &mut out);
            assert!(out.is_empty(), "{clip:?}: {out:?}");
        }
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

    #[test]
    fn a_nan_hi_skips_the_bar_and_keeps_the_diamond() {
        let mut out = Vec::new();
        let (bars, markers) = point_marks(&[10.0], &[50.0], &[55.0], &[f32::NAN], 3.0, &mut out);
        assert_eq!((bars, markers), (0, 1));
        assert_eq!(out.len(), 4);
        assert!(finite(&out), "{out:?}");
    }
}
