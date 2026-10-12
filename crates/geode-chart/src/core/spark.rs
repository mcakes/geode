//! Sparkline and term-glyph layout: values evenly spaced by index inside a
//! rect shrunk by an inset, y on the series' own or a shared range, clamped
//! to the rect, decimated to the width. Window-free; the `spark` element
//! paints what this returns.

use super::decimate::decimate;
use super::{Point, Rect};

/// The finite min and max across every row: a column's shared y-range, so
/// rows read against one scale. `None` when no row holds a finite value.
pub fn shared_range<'a>(rows: impl IntoIterator<Item = &'a [f64]>) -> Option<(f64, f64)> {
    rows.into_iter()
        .flat_map(|r| r.iter().copied())
        .filter(|v| v.is_finite())
        .fold(None, |acc, v| match acc {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
}

/// Buffers a layout reuses; their capacity survives across calls.
#[derive(Debug, Default)]
pub struct SparkScratch {
    /// The polyline in absolute coordinates, `Point::BREAK` between runs.
    pub points: Vec<Point>,
    /// A run of one finite value between gaps or the series' edges: no
    /// segment draws it, so the element paints it as a dot.
    pub isolated: Vec<Point>,
    xs: Vec<f32>,
    ys: Vec<f64>,
    runs: Vec<Point>,
}

/// The end of the line (the last finite value) and the marked point.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SparkMarks {
    pub end: Option<Point>,
    pub mark: Option<Point>,
}

/// Lay `values` out inside `rect` shrunk by `inset` on every side.
/// `range` is the shared y-range, else the series' own finite range; a
/// value outside it is clamped to the inset edge, and a flat range draws
/// through the middle. Non-finite values break the line. A rect no larger
/// than twice the inset, or a series with no finite value, lays out nothing.
pub fn layout(
    values: &[f64],
    range: Option<(f64, f64)>,
    mark: Option<usize>,
    rect: Rect,
    inset: f32,
    scratch: &mut SparkScratch,
) -> SparkMarks {
    scratch.points.clear();
    scratch.isolated.clear();
    let (w, h) = (rect.w - 2.0 * inset, rect.h - 2.0 * inset);
    if !(w > 0.0 && h > 0.0 && values.iter().any(|v| v.is_finite())) {
        return SparkMarks::default();
    }
    let Some((lo, hi)) = range.or_else(|| shared_range([values])) else {
        return SparkMarks::default();
    };
    let (left, top, bottom) = (rect.x + inset, rect.y + inset, rect.y + inset + h);
    let n = values.len();
    let x_of = |i: usize| -> f32 {
        if n == 1 {
            w / 2.0
        } else {
            w * i as f32 / (n - 1) as f32
        }
    };
    let y_of = |v: f64| -> f32 {
        if hi > lo {
            let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
            bottom - t * h
        } else {
            top + h / 2.0
        }
    };
    // Plot-relative xs (decimate's columns start at zero), absolute ys.
    scratch.xs.clear();
    scratch.ys.clear();
    for (i, v) in values.iter().enumerate() {
        scratch.xs.push(x_of(i));
        scratch.ys.push(if v.is_finite() {
            y_of(*v) as f64
        } else {
            f64::NAN
        });
    }
    let columns = w.ceil().max(1.0) as usize;
    decimate(&scratch.xs, &scratch.ys, columns, &mut scratch.points);
    for p in scratch.points.iter_mut().filter(|p| !p.is_break()) {
        p.x += left;
    }
    // Decimation keeps each column's extremes, so a run's first or last
    // value is dropped when it is neither: the line would end away from the
    // end dot. Each run is rebuilt to start and end on its true values, and
    // a run of one value, which no segment draws, is noted as isolated.
    let point_at = |i: usize| Point::new(left + x_of(i), y_of(values[i]));
    scratch.isolated.clear();
    scratch.runs.clear();
    let mut chunks = scratch.points.split(|p| p.is_break());
    let mut i = 0;
    while i < n {
        if !values[i].is_finite() {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && values[i].is_finite() {
            i += 1;
        }
        let chunk = chunks.next().unwrap_or(&[]);
        if !scratch.runs.is_empty() {
            scratch.runs.push(Point::BREAK);
        }
        let (first, last) = (point_at(start), point_at(i - 1));
        if start == i - 1 {
            scratch.isolated.push(first);
            scratch.runs.push(first);
            continue;
        }
        if chunk.first() != Some(&first) {
            scratch.runs.push(first);
        }
        scratch.runs.extend_from_slice(chunk);
        if scratch.runs.last() != Some(&last) {
            scratch.runs.push(last);
        }
    }
    std::mem::swap(&mut scratch.points, &mut scratch.runs);
    let end = values.iter().rposition(|v| v.is_finite()).map(point_at);
    let mark = mark
        .filter(|i| values.get(*i).is_some_and(|v| v.is_finite()))
        .map(point_at);
    SparkMarks { end, mark }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rect() -> Rect {
        Rect::new(10.0, 20.0, 72.0, 16.0)
    }

    fn finite(points: &[Point]) -> Vec<Point> {
        points.iter().copied().filter(|p| !p.is_break()).collect()
    }

    #[test]
    fn x_is_even_by_index_and_y_grows_upward() {
        let mut s = SparkScratch::default();
        let marks = layout(&[1.0, 2.0, 3.0], None, None, rect(), 2.0, &mut s);
        let p = finite(&s.points);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].x, 12.0);
        assert_eq!(p[2].x, 80.0);
        assert!((p[1].x - 46.0).abs() < 1e-4);
        // Own range 1..3: the lowest value at the inset bottom, the highest at the inset top.
        assert_eq!(p[0].y, 34.0);
        assert_eq!(p[2].y, 22.0);
        assert!((p[1].y - 28.0).abs() < 1e-4);
        assert_eq!(marks.end, Some(p[2]));
        assert_eq!(marks.mark, None);
    }

    #[test]
    fn a_shared_range_places_values_on_the_columns_scale() {
        let mut s = SparkScratch::default();
        layout(&[2.0, 2.0], Some((0.0, 4.0)), None, rect(), 2.0, &mut s);
        let p = finite(&s.points);
        assert!(
            p.iter().all(|q| (q.y - 28.0).abs() < 1e-4),
            "2 is the middle of 0..4: {p:?}"
        );
    }

    #[test]
    fn a_value_outside_the_range_is_clamped_inside_the_rect() {
        let mut s = SparkScratch::default();
        layout(&[-5.0, 9.0], Some((0.0, 4.0)), None, rect(), 2.0, &mut s);
        let p = finite(&s.points);
        assert_eq!(p[0].y, 34.0, "below the range: the inset bottom");
        assert_eq!(p[1].y, 22.0, "above the range: the inset top");
    }

    #[test]
    fn a_flat_series_runs_through_the_middle() {
        let mut s = SparkScratch::default();
        layout(&[0.21, 0.21, 0.21], None, None, rect(), 2.0, &mut s);
        assert!(finite(&s.points).iter().all(|q| (q.y - 28.0).abs() < 1e-4));
        layout(&[1.0, 3.0], Some((5.0, 5.0)), None, rect(), 2.0, &mut s);
        assert!(
            finite(&s.points).iter().all(|q| (q.y - 28.0).abs() < 1e-4),
            "a flat shared range too"
        );
    }

    #[test]
    fn a_non_finite_value_breaks_the_line_and_the_end_is_the_last_finite_value() {
        let mut s = SparkScratch::default();
        let marks = layout(
            &[1.0, 2.0, f64::NAN, 3.0, 4.0, f64::NAN],
            None,
            None,
            rect(),
            2.0,
            &mut s,
        );
        assert_eq!(
            s.points.iter().filter(|p| p.is_break()).count(),
            1,
            "one break between the runs, none trailing"
        );
        let p = finite(&s.points);
        assert_eq!(marks.end, Some(*p.last().unwrap()));
        assert!(
            (marks.end.unwrap().x - (12.0 + 68.0 * 4.0 / 5.0)).abs() < 1e-4,
            "index 4 of 0..=5"
        );
    }

    #[test]
    fn the_end_is_the_last_finite_value() {
        let mut s = SparkScratch::default();
        let marks = layout(&[1.0, 2.0, f64::NAN], None, None, rect(), 2.0, &mut s);
        assert_eq!(
            marks.end.map(|e| e.x),
            Some(46.0),
            "index 1 of 0..=2, not the NaN at index 2"
        );
    }

    #[test]
    fn a_mark_sits_on_its_value_and_a_bad_mark_has_none() {
        let mut s = SparkScratch::default();
        let marks = layout(&[1.0, 2.0, 3.0], None, Some(1), rect(), 2.0, &mut s);
        assert_eq!(marks.mark.map(|m| (m.x, m.y)), Some((46.0, 28.0)));
        assert_eq!(
            layout(&[1.0, f64::NAN, 3.0], None, Some(1), rect(), 2.0, &mut s).mark,
            None
        );
        assert_eq!(
            layout(&[1.0, 2.0], None, Some(7), rect(), 2.0, &mut s).mark,
            None
        );
    }

    #[test]
    fn one_value_is_a_dot_and_none_is_nothing() {
        let mut s = SparkScratch::default();
        let marks = layout(&[0.3], None, None, rect(), 2.0, &mut s);
        assert_eq!(
            marks.end,
            Some(Point::new(46.0, 28.0)),
            "one value sits at the center"
        );
        let marks = layout(&[], None, None, rect(), 2.0, &mut s);
        assert!(s.points.is_empty());
        assert_eq!(marks, SparkMarks::default());
        let marks = layout(&[f64::NAN, f64::NAN], None, None, rect(), 2.0, &mut s);
        assert!(s.points.is_empty());
        assert_eq!(marks, SparkMarks::default());
    }

    #[test]
    fn a_rect_smaller_than_the_inset_lays_out_nothing() {
        let mut s = SparkScratch::default();
        let marks = layout(
            &[1.0, 2.0],
            None,
            None,
            Rect::new(0.0, 0.0, 3.0, 3.0),
            2.0,
            &mut s,
        );
        assert!(s.points.is_empty());
        assert_eq!(marks, SparkMarks::default());
        let marks = layout(
            &[1.0, 2.0],
            None,
            None,
            Rect::new(0.0, 0.0, 0.0, 0.0),
            0.0,
            &mut s,
        );
        assert!(s.points.is_empty());
        assert_eq!(marks, SparkMarks::default());
    }

    #[test]
    fn a_long_series_is_decimated_to_the_width() {
        let values: Vec<f64> = (0..252).map(|i| ((i as f64) * 0.37).sin()).collect();
        let mut s = SparkScratch::default();
        layout(
            &values,
            None,
            None,
            Rect::new(0.0, 0.0, 140.0, 16.0),
            2.0,
            &mut s,
        );
        assert!(
            s.points.len() <= 2 * 137,
            "at most two extrema per column: {}",
            s.points.len()
        );
        assert!(s.points.len() >= 136);
    }

    /// The real points of each run, split at breaks.
    fn runs(points: &[Point]) -> Vec<Vec<Point>> {
        let mut out = vec![Vec::new()];
        for p in points {
            if p.is_break() {
                out.push(Vec::new());
            } else {
                out.last_mut().unwrap().push(*p);
            }
        }
        out.retain(|r| !r.is_empty());
        out
    }

    #[test]
    fn the_line_starts_and_ends_on_each_runs_true_first_and_last_values() {
        // 21 values into 4 px: about five per column, every column holding
        // 0 and 1, with the first, last and pre-gap values at 0.5 (never a
        // column's extreme), so decimation alone would end the line elsewhere.
        let mut values: Vec<f64> = (0..21)
            .map(|i| if i % 2 == 0 { 0.0 } else { 1.0 })
            .collect();
        values[0] = 0.5;
        values[20] = 0.5;
        let mut s = SparkScratch::default();
        let r = Rect::new(0.0, 0.0, 8.0, 16.0);
        let marks = layout(&values, None, None, r, 2.0, &mut s);
        let p = finite(&s.points);
        assert_eq!(
            p.first().copied(),
            Some(Point::new(2.0, 8.0)),
            "the first value"
        );
        assert_eq!(
            marks.end,
            p.last().copied(),
            "the line ends under the end dot"
        );
        assert_eq!(marks.end, Some(Point::new(6.0, 8.0)));
        // A gap: the run before it ends on its true last value.
        values[10] = 0.5;
        values[11] = f64::NAN;
        layout(&values, None, None, r, 2.0, &mut s);
        let rs = runs(&s.points);
        assert_eq!(rs.len(), 2);
        assert_eq!(
            rs[0].last().copied(),
            Some(Point::new(2.0 + 4.0 * 10.0 / 20.0, 8.0))
        );
        assert_eq!(rs[1].first().map(|q| q.x), Some(2.0 + 4.0 * 12.0 / 20.0));
    }

    #[test]
    fn isolated_values_are_dots_and_draw_no_segment() {
        let mut s = SparkScratch::default();
        layout(
            &[1.0, f64::NAN, 2.0, f64::NAN, 3.0],
            None,
            None,
            rect(),
            2.0,
            &mut s,
        );
        assert_eq!(s.isolated.len(), 3, "{:?}", s.isolated);
        assert!(
            runs(&s.points).iter().all(|r| r.len() == 1),
            "no segment joins them"
        );
        layout(&[0.3], None, None, rect(), 2.0, &mut s);
        assert_eq!(
            s.isolated,
            vec![Point::new(46.0, 28.0)],
            "one value is an isolated point"
        );
        layout(&[1.0, 2.0, f64::NAN, 3.0], None, None, rect(), 2.0, &mut s);
        assert_eq!(s.isolated.len(), 1, "only the run of one");
    }

    #[test]
    fn shared_range_is_the_finite_min_and_max_across_rows() {
        let a = [0.20, 0.22, f64::NAN];
        let b = [0.18, 0.25];
        assert_eq!(shared_range([&a[..], &b[..]]), Some((0.18, 0.25)));
        assert_eq!(shared_range([&[f64::NAN][..]]), None);
        assert_eq!(shared_range(std::iter::empty()), None);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn every_point_lies_inside_the_inset_rect(
            values in prop::collection::vec(prop_oneof![9 => -1.0e6f64..1.0e6, 1 => Just(f64::NAN)], 0..400),
            lo in -10.0f64..10.0, span in 0.0f64..10.0, w in 0.0f32..300.0, h in 0.0f32..40.0,
        ) {
            let r = Rect::new(5.0, 7.0, w, h);
            let mut s = SparkScratch::default();
            let marks = layout(&values, Some((lo, lo + span)), Some(3), r, 2.0, &mut s);
            let inside = |p: &Point| p.x >= r.x + 2.0 - 1e-3 && p.x <= r.right() - 2.0 + 1e-3
                && p.y >= r.y + 2.0 - 1e-3 && p.y <= r.bottom() - 2.0 + 1e-3;
            for p in s.points.iter().filter(|p| !p.is_break()) {
                prop_assert!(inside(p), "{p:?} outside {r:?}");
            }
            for p in [marks.end, marks.mark].into_iter().flatten() {
                prop_assert!(inside(&p));
            }
        }
    }
}
