//! Prepared xy data and presentation shared with the painting element.
//!
//! Each [`XySlot`] carries its own x values, ascending, because curves and
//! quoted points sit on different grids. The model derives its x range; the
//! element derives pixel coordinates and y scales over what the view shows.
//! Callers must change `version` whenever model contents change, so cached
//! scales, labels and paths cannot outlive their inputs.

use std::sync::Arc;

use gpui::{Hsla, SharedString};

use crate::core::axis::{Axis, Pane, Side};
use crate::core::layout::LayoutOptions;
use crate::core::linear::XFormat;
use crate::core::view::View;

/// How a y axis labels its values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum YFormat {
    #[default]
    Plain,
    /// A ratio shown as a percent: `0.2` reads `20%`.
    Percent,
}

/// A line's stroke. Points ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Style {
    #[default]
    Solid,
    Dashed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct XAxis {
    pub format: XFormat,
    /// Runs the axis right to left.
    pub reversed: bool,
}

/// What a slot paints. Every array of a slot is one column of the same
/// points, and `xs` is finite and ascending: the window, the nearest point
/// and a line's value are all binary searches over it. [`XyModel::new`]
/// makes that so; a slot edited afterwards must keep it so.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotKind {
    /// A polyline through `(xs[i], ys[i])`. A `NaN` in `ys` is a gap.
    Line { xs: Vec<f64>, ys: Vec<f64> },
    /// A marker at `mid` and a vertical bar from `lo` to `hi` per point.
    Points {
        xs: Vec<f64>,
        mid: Vec<f64>,
        lo: Vec<f64>,
        hi: Vec<f64>,
    },
}

impl SlotKind {
    /// Cut the arrays to their shared length, drop every point whose x is
    /// not finite, and put the rest in ascending x, equal xs in the order
    /// they came. A slot that is already so is checked in one pass and
    /// left untouched.
    fn normalise(&mut self) {
        let mut columns = match self {
            SlotKind::Line { xs, ys } => [Some(xs), Some(ys), None, None],
            SlotKind::Points { xs, mid, lo, hi } => [Some(xs), Some(mid), Some(lo), Some(hi)],
        };
        let n = columns.iter().flatten().map(|c| c.len()).min().unwrap_or(0);
        for column in columns.iter_mut().flatten() {
            column.truncate(n);
        }
        let Some(xs) = columns[0].as_deref() else {
            return;
        };
        let clean = xs.first().is_none_or(|x| x.is_finite())
            && xs.windows(2).all(|w| w[1].is_finite() && w[0] <= w[1]);
        if clean {
            return;
        }
        let mut order: Vec<usize> = (0..n).filter(|i| xs[*i].is_finite()).collect();
        // Stable, so points sharing an x keep their order.
        order.sort_by(|a, b| xs[*a].total_cmp(&xs[*b]));
        for column in columns.iter_mut().flatten() {
            let kept: Vec<f64> = order.iter().map(|i| column[*i]).collect();
            **column = kept;
        }
    }
}

/// One series slot as the chart paints it.
#[derive(Debug, Clone, PartialEq)]
pub struct XySlot {
    pub number: u16,
    pub label: SharedString,
    pub color: Hsla,
    pub axis: Axis,
    pub visible: bool,
    pub style: Style,
    pub kind: SlotKind,
}

impl XySlot {
    pub fn xs(&self) -> &[f64] {
        match &self.kind {
            SlotKind::Line { xs, .. } | SlotKind::Points { xs, .. } => xs,
        }
    }

    /// Points the slot can paint: the shared length of its arrays.
    pub fn len(&self) -> usize {
        match &self.kind {
            SlotKind::Line { xs, ys } => xs.len().min(ys.len()),
            SlotKind::Points { xs, mid, lo, hi } => {
                xs.len().min(mid.len()).min(lo.len()).min(hi.len())
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `[start, end)` of the points the view shows. A line takes one knot
    /// more on each side, so it runs to the plot's edges and beyond rather
    /// than stopping at its last visible knot. A line window of one knot is
    /// empty instead: one knot is no span, so a line wholly to one side of
    /// the view offers nothing to paint and nothing to scale over.
    pub fn window(&self, view: View) -> (usize, usize) {
        let n = self.len();
        let xs = &self.xs()[..n];
        let start = xs.partition_point(|x| *x < view.lo);
        let end = xs.partition_point(|x| *x <= view.hi).max(start);
        match self.kind {
            SlotKind::Line { .. } => {
                let (first, last) = (start.saturating_sub(1), (end + 1).min(n));
                if last - first < 2 {
                    (start, start)
                } else {
                    (first, last)
                }
            }
            SlotKind::Points { .. } => (start, end),
        }
    }

    /// Every y the slot paints within `window`: a line's values, or a
    /// point's mid, low and high. What the slot's axis scales over.
    pub fn values_in(&self, window: (usize, usize)) -> impl Iterator<Item = f64> + '_ {
        let end = window.1.min(self.len());
        let start = window.0.min(end);
        let (a, b, c): (&[f64], &[f64], &[f64]) = match &self.kind {
            SlotKind::Line { ys, .. } => (&ys[start..end], &[], &[]),
            SlotKind::Points { mid, lo, hi, .. } => {
                (&mid[start..end], &lo[start..end], &hi[start..end])
            }
        };
        a.iter().chain(b).chain(c).copied()
    }

    /// The index of the point whose x is nearest `u`; `None` for a slot
    /// with no points or a `u` that is not a number.
    pub fn nearest(&self, u: f64) -> Option<usize> {
        let n = self.len();
        if n == 0 || u.is_nan() {
            return None;
        }
        let xs = &self.xs()[..n];
        let i = xs.partition_point(|x| *x < u);
        if i == 0 {
            return Some(0);
        }
        if i == n {
            return Some(n - 1);
        }
        Some(if xs[i] - u < u - xs[i - 1] { i } else { i - 1 })
    }

    /// A line's value at `u`, read between its two knots. `None` for a
    /// points slot, outside the line's own range, or across a gap.
    pub fn line_value_at(&self, u: f64) -> Option<f64> {
        let SlotKind::Line { xs, ys } = &self.kind else {
            return None;
        };
        let n = self.len();
        // Written as the range it accepts, so a NaN `u` is outside it
        // rather than past both comparisons and into the knots below.
        if n == 0 || !(xs[0]..=xs[n - 1]).contains(&u) {
            return None;
        }
        let i = xs[..n].partition_point(|x| *x < u);
        let v = if xs.get(i) == Some(&u) {
            ys[i]
        } else if i == 0 || i >= n {
            // Unreachable while `xs` is ascending; a NaN knot breaks that.
            return None;
        } else {
            let (x0, x1) = (xs[i - 1], xs[i]);
            ys[i - 1] + (ys[i] - ys[i - 1]) * (u - x0) / (x1 - x0)
        };
        v.is_finite().then_some(v)
    }
}

/// Everything one frame of the xy chart paints from.
#[derive(Debug, Clone, PartialEq)]
pub struct XyModel {
    /// The cache key: 1 or more for a built model, and changed by the
    /// builder on every change.
    pub version: u64,
    pub x: XAxis,
    /// Indexed in `Axis::ALL` order.
    pub y_format: [YFormat; 4],
    pub split: f32,
    pub slots: Vec<XySlot>,
    full: (f64, f64),
}

impl XyModel {
    /// Build a model, normalising every slot on the way in: its arrays are
    /// cut to their shared length, a point whose x is not finite is
    /// dropped, and points out of x order are sorted into it (a line then
    /// runs through its points in x order). A slot already so is not
    /// copied. The slots are public; one edited after construction must
    /// keep its xs finite and ascending.
    ///
    /// A caller's versions start at 1 and change on every change to the
    /// model, `split` included: [`XyModel::empty`] is version 0, and the
    /// element's caches tell one model from the next by the version alone.
    /// A version reused for other contents paints the earlier contents'
    /// paths and axes.
    pub fn new(
        version: u64,
        x: XAxis,
        y_format: [YFormat; 4],
        split: f32,
        mut slots: Vec<XySlot>,
    ) -> Arc<Self> {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for slot in slots.iter_mut() {
            slot.kind.normalise();
            if !slot.visible {
                continue;
            }
            // Finite and ascending now: the ends are the extremes.
            if let (Some(first), Some(last)) = (slot.xs().first(), slot.xs().last()) {
                lo = lo.min(*first);
                hi = hi.max(*last);
            }
        }
        let full = if lo <= hi { (lo, hi) } else { (0.0, 0.0) };
        Arc::new(Self {
            version,
            x,
            y_format,
            split,
            slots,
            full,
        })
    }

    /// A model with no slots, at version 0, which no built model takes.
    pub fn empty() -> Arc<Self> {
        Self::new(0, XAxis::default(), [YFormat::Plain; 4], 0.7, Vec::new())
    }

    /// The x range of the visible slots' finite values, as of construction;
    /// `(0, 0)` when there are none.
    pub fn full(&self) -> (f64, f64) {
        self.full
    }

    pub fn y_format_of(&self, axis: Axis) -> YFormat {
        let i = Axis::ALL.iter().position(|a| *a == axis).unwrap_or(0);
        self.y_format[i]
    }

    fn uses(&self, pane: Pane, side: Side) -> bool {
        self.slots
            .iter()
            .any(|s| s.visible && s.axis.pane() == pane && s.axis.side() == side)
    }

    /// What the layout reserves: an axis column per side a visible slot
    /// uses, and no density strip.
    pub fn layout_options(&self, rem_px: f32) -> LayoutOptions {
        LayoutOptions {
            upper_left: self.uses(Pane::Upper, Side::Left),
            upper_right: self.uses(Pane::Upper, Side::Right),
            lower_left: self.uses(Pane::Lower, Side::Left),
            lower_right: self.uses(Pane::Lower, Side::Right),
            density: false,
            split: self.split,
            rem_px,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(axis: Axis, xs: &[f64], ys: &[f64]) -> XySlot {
        XySlot {
            number: 1,
            label: "curve".into(),
            color: gpui::red(),
            axis,
            visible: true,
            style: Style::Solid,
            kind: SlotKind::Line {
                xs: xs.to_vec(),
                ys: ys.to_vec(),
            },
        }
    }

    fn points(axis: Axis, xs: &[f64], mid: &[f64], lo: &[f64], hi: &[f64]) -> XySlot {
        XySlot {
            number: 2,
            label: "chain".into(),
            color: gpui::blue(),
            axis,
            visible: true,
            style: Style::Solid,
            kind: SlotKind::Points {
                xs: xs.to_vec(),
                mid: mid.to_vec(),
                lo: lo.to_vec(),
                hi: hi.to_vec(),
            },
        }
    }

    fn model(slots: Vec<XySlot>) -> Arc<XyModel> {
        XyModel::new(1, XAxis::default(), [YFormat::Plain; 4], 0.7, slots)
    }

    #[test]
    fn the_full_range_is_the_visible_slots_finite_xs() {
        let mut hidden = line(Axis::Left, &[0.1, 5.0], &[1.0, 1.0]);
        hidden.visible = false;
        let m = model(vec![
            line(Axis::Left, &[0.8, 1.0, 1.2], &[0.3, 0.2, 0.25]),
            points(
                Axis::Left,
                &[0.7, f64::NAN, 1.1],
                &[0.3; 3],
                &[0.3; 3],
                &[0.3; 3],
            ),
            hidden,
        ]);
        assert_eq!(m.full(), (0.7, 1.2));
        assert_eq!(XyModel::empty().full(), (0.0, 0.0));
    }

    #[test]
    fn a_line_window_reaches_one_knot_past_each_edge_and_a_points_window_does_not() {
        let xs = [0.8, 0.9, 1.0, 1.1, 1.2];
        let view = View::with_min_span((0.95, 1.05), 0.01);
        assert_eq!(line(Axis::Left, &xs, &[0.0; 5]).window(view), (1, 4));
        assert_eq!(
            points(Axis::Left, &xs, &[0.0; 5], &[0.0; 5], &[0.0; 5]).window(view),
            (2, 3)
        );
    }

    #[test]
    fn a_slot_outside_the_view_has_an_empty_window() {
        let view = View::with_min_span((2.0, 3.0), 0.01);
        let l = line(Axis::Left, &[0.8, 1.2], &[0.3, 0.2]);
        // One knot past the view's edge is all a line off to one side has
        // to offer, and one knot is no span: nothing of it can paint, so
        // nothing of it reaches the y domain either.
        for view in [view, View::with_min_span((0.1, 0.5), 0.01)] {
            let (start, end) = l.window(view);
            assert_eq!(start, end, "{view:?}");
            assert_eq!(l.values_in((start, end)).count(), 0, "{view:?}");
        }
        let p = points(Axis::Left, &[0.8, 1.2], &[0.3; 2], &[0.3; 2], &[0.3; 2]);
        assert_eq!(p.window(view), (2, 2));
        assert_eq!(p.values_in(p.window(view)).count(), 0);
    }

    #[test]
    fn a_line_window_holds_two_knots_or_none() {
        // A view between two knots, with no knot inside it: the span from
        // one to the other crosses the plot, so both are the window.
        let l = line(Axis::Left, &[0.8, 1.0, 1.2], &[0.3, 0.2, 0.25]);
        let between = View::with_min_span((1.05, 1.1), 0.01);
        assert_eq!(l.window(between), (1, 3));
        assert_eq!(
            l.values_in(l.window(between)).collect::<Vec<_>>(),
            [0.2, 0.25]
        );
        // A view that just reaches the last knot still has the span to it.
        assert_eq!(l.window(View::with_min_span((1.2, 3.0), 0.01)), (1, 3));
        // A line of one knot has no span, wherever the view is.
        let one = line(Axis::Left, &[1.0], &[0.3]);
        let around = View::with_min_span((0.5, 1.5), 0.01);
        let (start, end) = one.window(around);
        assert_eq!(start, end);
        assert_eq!(one.values_in((start, end)).count(), 0);
        // A point alone in view is still a point.
        let p = points(Axis::Left, &[1.0], &[0.3], &[0.3], &[0.3]);
        assert_eq!(p.window(around), (0, 1));
    }

    /// The one slot of a model built from `slot`, as construction left it.
    fn built(slot: XySlot) -> XySlot {
        model(vec![slot]).slots[0].clone()
    }

    #[test]
    fn a_point_with_no_x_is_dropped_and_the_rest_still_paint() {
        let m = model(vec![points(
            Axis::Left,
            &[0.7, f64::NAN, 1.1],
            &[0.3, 0.4, 0.5],
            &[0.29, 0.39, 0.49],
            &[0.31, 0.41, 0.51],
        )]);
        let p = &m.slots[0];
        assert_eq!(p.len(), 2);
        assert_eq!(p.xs(), [0.7, 1.1]);
        assert_eq!(m.full(), (0.7, 1.1));
        let window = p.window(View::with_min_span(m.full(), 0.01));
        assert_eq!(window, (0, 2), "the point past the NaN is in view");
        let mut v: Vec<f64> = p.values_in(window).collect();
        v.sort_by(f64::total_cmp);
        assert_eq!(v, [0.29, 0.3, 0.31, 0.49, 0.5, 0.51], "each array in step");
        assert_eq!(p.nearest(1.0), Some(1));
    }

    #[test]
    fn a_nan_x_is_dropped_wherever_it_sits() {
        let nan = f64::NAN;
        for (xs, ys, want_xs, want_ys) in [
            (
                [nan, 1.0, 2.0],
                [10.0, 20.0, 30.0],
                [1.0, 2.0],
                [20.0, 30.0],
            ),
            (
                [1.0, nan, 2.0],
                [10.0, 20.0, 30.0],
                [1.0, 2.0],
                [10.0, 30.0],
            ),
            (
                [1.0, 2.0, nan],
                [10.0, 20.0, 30.0],
                [1.0, 2.0],
                [10.0, 20.0],
            ),
            (
                [1.0, f64::INFINITY, 2.0],
                [10.0, 20.0, 30.0],
                [1.0, 2.0],
                [10.0, 30.0],
            ),
        ] {
            let l = built(line(Axis::Left, &xs, &ys));
            assert_eq!(l, line(Axis::Left, &want_xs, &want_ys), "{xs:?}");
            let full = View::with_min_span((0.0, 3.0), 0.01);
            assert_eq!(l.window(full), (0, 2), "{xs:?}");
            assert_eq!(l.values_in((0, 2)).collect::<Vec<_>>(), want_ys, "{xs:?}");
            assert_eq!(l.nearest(1.9), Some(1), "{xs:?}");
            assert_eq!(l.line_value_at(1.5), Some((want_ys[0] + want_ys[1]) / 2.0));
        }
    }

    #[test]
    fn unsorted_points_are_put_in_x_order() {
        let l = built(line(Axis::Left, &[1.2, 0.8, 1.0], &[3.0, 1.0, 2.0]));
        assert_eq!(l, line(Axis::Left, &[0.8, 1.0, 1.2], &[1.0, 2.0, 3.0]));
        assert_eq!(l.nearest(0.95), Some(1));
        assert_eq!(l.window(View::with_min_span((0.9, 1.1), 0.01)), (0, 3));
        // Every array moves with its x, a NaN x goes on the way, and equal
        // xs keep the order they came in.
        let p = built(points(
            Axis::Left,
            &[2.0, 1.0, f64::NAN, 1.0],
            &[20.0, 10.0, 0.0, 11.0],
            &[19.0, 9.0, 0.0, 10.5],
            &[21.0, 12.0, 0.0, 11.5],
        ));
        let want = points(
            Axis::Left,
            &[1.0, 1.0, 2.0],
            &[10.0, 11.0, 20.0],
            &[9.0, 10.5, 19.0],
            &[12.0, 11.5, 21.0],
        );
        assert_eq!(p, want);
    }

    #[test]
    fn a_clean_slot_is_left_as_it_came() {
        let l = line(
            Axis::Left,
            &[0.8, 1.0, 1.0, 1.2],
            &[1.0, f64::MAX, 2.0, 3.0],
        );
        let p = points(Axis::Right, &[0.8, 1.2], &[0.3; 2], &[0.2; 2], &[0.4; 2]);
        let buffer = l.xs().as_ptr();
        let m = model(vec![l.clone(), p.clone()]);
        assert_eq!(m.slots, [l.clone(), p]);
        // Moved in, not rebuilt: the same allocation.
        let m = model(vec![l]);
        assert_eq!(m.slots[0].xs().as_ptr(), buffer);
    }

    #[test]
    fn mismatched_arrays_are_cut_to_the_shortest() {
        let l = built(line(Axis::Left, &[1.0, 2.0, 3.0], &[10.0, 20.0]));
        assert_eq!(l, line(Axis::Left, &[1.0, 2.0], &[10.0, 20.0]));
        let p = built(points(
            Axis::Left,
            &[1.0, 2.0, 3.0],
            &[0.3; 3],
            &[0.2; 2],
            &[0.4; 4],
        ));
        assert_eq!(
            p,
            points(Axis::Left, &[1.0, 2.0], &[0.3; 2], &[0.2; 2], &[0.4; 2])
        );
        // The x beyond the shared length is not part of the range.
        let m = model(vec![line(Axis::Left, &[1.0, 2.0, 9.0], &[10.0, 20.0])]);
        assert_eq!(m.full(), (1.0, 2.0));
    }

    #[test]
    fn nearest_has_no_answer_for_an_x_that_is_not_a_number() {
        let l = line(Axis::Left, &[1.0, 2.0, 4.0], &[10.0, 20.0, 40.0]);
        assert_eq!(l.nearest(f64::NAN), None);
        assert_eq!(l.nearest(f64::INFINITY), Some(2));
        assert_eq!(l.nearest(f64::NEG_INFINITY), Some(0));
    }

    #[test]
    fn each_axis_reads_its_own_y_format() {
        for percent in 0..4 {
            let mut y_format = [YFormat::Plain; 4];
            y_format[percent] = YFormat::Percent;
            let m = XyModel::new(1, XAxis::default(), y_format, 0.7, Vec::new());
            for (i, axis) in Axis::ALL.into_iter().enumerate() {
                let want = if i == percent {
                    YFormat::Percent
                } else {
                    YFormat::Plain
                };
                assert_eq!(m.y_format_of(axis), want, "percent at {percent}, {axis:?}");
            }
        }
    }

    #[test]
    fn the_values_a_slot_offers_its_axis_include_a_points_range() {
        let p = points(
            Axis::Left,
            &[1.0, 2.0],
            &[0.20, 0.30],
            &[0.19, 0.29],
            &[0.21, 0.31],
        );
        let mut v: Vec<f64> = p.values_in((0, 2)).collect();
        v.sort_by(f64::total_cmp);
        assert_eq!(v, [0.19, 0.20, 0.21, 0.29, 0.30, 0.31]);
        let l = line(Axis::Left, &[1.0, 2.0, 3.0], &[5.0, 6.0, 7.0]);
        assert_eq!(l.values_in((1, 3)).collect::<Vec<_>>(), [6.0, 7.0]);
    }

    #[test]
    fn nearest_finds_the_closest_x_and_a_line_reads_between_its_knots() {
        let l = line(Axis::Left, &[1.0, 2.0, 4.0], &[10.0, 20.0, 40.0]);
        assert_eq!(l.nearest(0.0), Some(0));
        assert_eq!(l.nearest(2.9), Some(1));
        assert_eq!(l.nearest(3.1), Some(2));
        assert_eq!(l.nearest(99.0), Some(2));
        assert_eq!(l.line_value_at(3.0), Some(30.0));
        assert_eq!(l.line_value_at(1.0), Some(10.0));
        assert_eq!(l.line_value_at(0.5), None, "outside the line's own range");
        assert_eq!(line(Axis::Left, &[], &[]).nearest(1.0), None);
        let gap = line(Axis::Left, &[1.0, 2.0], &[10.0, f64::NAN]);
        assert_eq!(gap.line_value_at(1.5), None, "a gap is not bridged");
        let p = points(Axis::Left, &[1.0], &[0.2], &[0.2], &[0.2]);
        assert_eq!(p.line_value_at(1.0), None, "points are not interpolated");
    }

    #[test]
    fn a_line_has_no_value_at_an_x_that_is_not_a_number() {
        let l = line(Axis::Left, &[1.0, 2.0, 4.0], &[10.0, 20.0, 40.0]);
        assert_eq!(l.line_value_at(f64::NAN), None);
        assert_eq!(l.line_value_at(f64::INFINITY), None);
        assert_eq!(l.line_value_at(4.0), Some(40.0), "the last knot itself");
    }

    #[test]
    fn layout_options_follow_visible_slots_and_reserve_no_density_strip() {
        let mut m = (*model(vec![
            line(Axis::Left, &[1.0], &[1.0]),
            line(Axis::BottomLeft, &[1.0], &[1.0]),
        ]))
        .clone();
        let o = m.layout_options(12.0);
        assert!(o.upper_left && o.lower_left && !o.upper_right && !o.lower_right);
        assert!(!o.density);
        m.slots[1].visible = false;
        assert!(
            !m.layout_options(12.0).lower_left,
            "a hidden slot reserves nothing"
        );
        assert_eq!(m.y_format_of(Axis::BottomRight), YFormat::Plain);
    }
}
