//! A linear x axis: a value maps to a pixel by its place in the view. The
//! strike-like axes of a slice chart use it; `reversed` runs the axis right
//! to left, for call delta, so strike still increases left to right.

use super::Rect;
use super::scale::{LinearScale, fmt_percent, fmt_tick};
use super::time::Tick;
use super::view::View;

/// How an x value is labelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum XFormat {
    /// A price or strike, with the decimals its tick step needs.
    #[default]
    Price,
    /// A ratio shown as a percent: `0.95` reads `95%`.
    Percent,
    /// At least this many decimals, more when the tick step needs them.
    Fixed(u8),
    /// A call delta read the trader's way: `0.90` is a `10p`, `0.25` a `25c`.
    Delta,
}

/// The delta ticks a trader reads, as call deltas.
pub const DELTA_LADDER: [f64; 7] = [0.05, 0.10, 0.25, 0.50, 0.75, 0.90, 0.95];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct LinearX {
    pub reversed: bool,
}

impl LinearX {
    /// `u`'s x on the plot. A view with no span maps everything to the
    /// plot's left edge rather than to NaN.
    pub fn x_of(&self, u: f64, view: View, plot: Rect) -> f32 {
        let span = view.span();
        if span.is_nan() || span <= 0.0 {
            return plot.x;
        }
        let t = ((u - view.lo) / span) as f32;
        let t = if self.reversed { 1.0 - t } else { t };
        plot.x + t * plot.w
    }

    /// The value under pixel `x`: `x_of`'s inverse.
    pub fn value_at(&self, x: f32, view: View, plot: Rect) -> f64 {
        if plot.w <= 0.0 {
            return view.lo;
        }
        let t = ((x - plot.x) / plot.w) as f64;
        let t = if self.reversed { 1.0 - t } else { t };
        view.lo + t * view.span()
    }

    /// The `about` [`View::zoom`] takes for a cursor `fraction` of the way
    /// across the plot from its left edge (the fraction a plot hit
    /// carries). A view zooms in value space, where `about` runs from
    /// `view.lo`; on a reversed axis `view.lo` is the plot's right edge.
    /// Handed the pixel fraction as it is, a reversed axis zooms about the
    /// mirror point and the value under the cursor slides away.
    pub fn about(self, fraction: f64) -> f64 {
        if self.reversed {
            1.0 - fraction
        } else {
            fraction
        }
    }

    /// The sign a pan takes so that it runs the way it does on screen:
    /// `view.pan(f * pan_sign(), full)` moves the window `f` of its width
    /// to the right on screen, and `view.pan(-f * pan_sign(), full)` is a
    /// drag to the right by `f` of the plot, the picture following the
    /// pointer. A view pans in value space, and on a reversed axis higher
    /// values lie to the left: without the sign a drag there moves the
    /// picture against the pointer.
    pub fn pan_sign(self) -> f64 {
        if self.reversed { -1.0 } else { 1.0 }
    }
}

/// The decimals a label needs to tell ticks `step` apart, six at most. The
/// nudge keeps a step stored a hair under a power of ten (`0.01 * 100`) from
/// taking a decimal it does not need. None for a step that is not a
/// positive number.
pub(crate) fn step_decimals(step: f64) -> usize {
    if !step.is_finite() || step <= 0.0 {
        return 0;
    }
    (-(step * (1.0 + 1e-9)).log10()).ceil().clamp(0.0, 6.0) as usize
}

/// A call delta as a trader says it: the put delta above a half, the call
/// delta below, and plain `50` at the money. Whole percents.
pub fn delta_label(delta: f64) -> String {
    delta_label_with(delta, 0)
}

/// [`delta_label`] with `decimals` places of a percent, trailing zeros kept
/// so one axis reads at one precision: `0.505` at one decimal is `49.5p`.
/// The bare number marks the money only when the delta rounds to exactly 50
/// at that precision (`50.0` at one decimal). Six decimals at most.
pub fn delta_label_with(delta: f64, decimals: usize) -> String {
    let decimals = decimals.min(6);
    let unit = 10f64.powi(decimals as i32);
    let pct = (delta * 100.0 * unit).round() / unit;
    if pct == 50.0 {
        format!("{pct:.decimals$}")
    } else if pct > 50.0 {
        format!("{:.decimals$}p", 100.0 - pct)
    } else {
        format!("{pct:.decimals$}c")
    }
}

/// One x value as its axis labels it. `step` is the tick step, which sets
/// the decimals: a price or percent takes what the step needs, a fixed
/// format its own count or what the step needs if that is more (the step
/// asks for six at most; a larger count of its own is honoured), a delta
/// none, one or two places of a percent.
pub fn fmt_x(value: f64, step: f64, format: XFormat) -> String {
    match format {
        XFormat::Price => fmt_tick(value, step),
        XFormat::Percent => fmt_percent(value, step),
        XFormat::Fixed(n) => {
            let decimals = (n as usize).max(step_decimals(step));
            format!("{value:.decimals$}")
        }
        XFormat::Delta => delta_label_with(value, step_decimals(step * 100.0).min(2)),
    }
}

/// [`DELTA_LADDER`] in the order its rungs claim room on a crowded axis: the
/// money, then the 25s, the 10s and the 5s.
const LADDER_PRIORITY: [f64; 7] = [0.50, 0.25, 0.75, 0.10, 0.90, 0.05, 0.95];

/// The ladder rungs a delta axis labels, ascending, into `out`. A rung in
/// view is kept, in priority order, when it sits at least `tick_gap_px`
/// from every rung already kept. Fewer than three survivors leave `out`
/// empty: two labels are not an axis.
fn ladder_rungs(scale: LinearX, view: View, plot: Rect, tick_gap_px: f32, out: &mut Vec<f64>) {
    let mut kept = [(0.0f32, 0.0f64); 7];
    let mut n = 0;
    for rung in LADDER_PRIORITY {
        if rung < view.lo || rung > view.hi {
            continue;
        }
        let x = scale.x_of(rung, view, plot);
        if kept[..n].iter().all(|(k, _)| (x - k).abs() >= tick_gap_px) {
            kept[n] = (x, rung);
            n += 1;
        }
    }
    if n < 3 {
        return;
    }
    out.extend(kept[..n].iter().map(|(_, rung)| *rung));
    out.sort_by(f64::total_cmp);
}

/// The fewest labels an axis reads as a scale: one label says where a value
/// is and not how far apart two are.
const MIN_X_TICKS: usize = 2;

/// How many steps down the 1-2-5 ladder [`x_ticks`] goes to reach
/// [`MIN_X_TICKS`]. The step asked for is under a view and a quarter wide
/// and two steps down divide it by four at least, so two reach it; the rest
/// is room for a tick that rounding puts just outside an edge.
const FINER_STEPS: usize = 4;

/// The largest tick index (a tick's value over the step) ticks are counted
/// at: `f64` holds every integer below it exactly.
const MAX_TICK_INDEX: f64 = 9.0e15;

/// The 1-2-5 step next below `step`, itself a 1-2-5 step: 5 gives 2, 2
/// gives 1, 1 gives 0.5. Not a number for a step that is not a positive
/// number.
fn finer_step(step: f64) -> f64 {
    // The nudge keeps a step stored a hair under a power of ten in its own
    // decade.
    let magnitude = 10f64.powf((step.log10() + 1e-9).floor());
    let residual = step / magnitude;
    if residual > 3.5 {
        2.0 * magnitude
    } else if residual > 1.5 {
        magnitude
    } else {
        0.5 * magnitude
    }
}

/// Whether ticks `step` apart can be counted across `view`: the step is a
/// positive number, and the view's values are not so far from zero for
/// their span that one tick's index is the next one's too. There the tick
/// walk cannot advance.
fn countable(view: View, step: f64) -> bool {
    step > 0.0 && step.is_finite() && view.lo.abs().max(view.hi.abs()) / step < MAX_TICK_INDEX
}

/// Ticks for the view into `out` (cleared first), in ascending pixel x; none
/// for a view with no span or a plot with no finite width. 1-2-5 steps,
/// labelled with the decimals the step needs. A delta axis instead labels
/// the rungs of [`DELTA_LADDER`] that fit at `tick_gap_px`, as whole
/// percents, when at least three do.
///
/// A narrow plot asks for a step as wide as its view, and such a step can
/// have one multiple in the view or none. The step then goes down the 1-2-5
/// ladder, [`FINER_STEPS`] at most, until two ticks fall in the view; the
/// labels take the decimals of the step used. A view too narrow for its
/// values to count ticks in keeps the ticks it has.
pub fn x_ticks(
    scale: LinearX,
    view: View,
    plot: Rect,
    tick_gap_px: f32,
    format: XFormat,
    out: &mut Vec<Tick>,
) {
    out.clear();
    let span = view.span();
    if span.is_nan() || span <= 0.0 || !plot.w.is_finite() || plot.w <= 0.0 {
        return;
    }
    let hint = ((plot.w / tick_gap_px.max(1.0)) as usize).max(2);
    let mut step = LinearScale::nice_step(span, hint);
    let mut values: Vec<f64> = Vec::new();
    if format == XFormat::Delta {
        ladder_rungs(scale, view, plot, tick_gap_px, &mut values);
    }
    let ladder = !values.is_empty();
    if !ladder {
        let axis = LinearScale::new((view.lo, view.hi), 0.0, 1.0);
        axis.ticks(hint, &mut values);
        for _ in 0..FINER_STEPS {
            if values.len() >= MIN_X_TICKS {
                break;
            }
            let finer = finer_step(step);
            if !countable(view, finer) {
                break;
            }
            step = finer;
            axis.ticks_at(step, &mut values);
        }
    }
    out.extend(values.into_iter().map(|v| Tick {
        x: scale.x_of(v, view, plot),
        label: if ladder {
            delta_label(v)
        } else {
            fmt_x(v, step, format)
        },
    }));
    if scale.reversed {
        out.reverse();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLOT: Rect = Rect::new(100.0, 0.0, 400.0, 200.0);

    fn labels(t: &[Tick]) -> Vec<&str> {
        t.iter().map(|t| t.label.as_str()).collect()
    }

    #[test]
    fn a_value_maps_across_the_plot_and_back() {
        let view = View::with_min_span((0.8, 1.2), 0.01);
        let s = LinearX { reversed: false };
        assert_eq!(s.x_of(0.8, view, PLOT), 100.0);
        assert_eq!(s.x_of(1.2, view, PLOT), 500.0);
        assert!((s.x_of(1.0, view, PLOT) - 300.0).abs() < 1e-3);
        assert!((s.value_at(300.0, view, PLOT) - 1.0).abs() < 1e-9);
        let r = LinearX { reversed: true };
        assert_eq!(
            r.x_of(0.8, view, PLOT),
            500.0,
            "the low value sits on the right"
        );
        assert_eq!(r.x_of(1.2, view, PLOT), 100.0);
        assert!((r.value_at(r.x_of(0.93, view, PLOT), view, PLOT) - 0.93).abs() < 1e-6);
    }

    #[test]
    fn a_zoom_about_the_cursor_keeps_the_value_under_it_on_both_orientations() {
        let full = (0.5, 1.5);
        for reversed in [false, true] {
            let s = LinearX { reversed };
            for fraction in [0.0f32, 0.3, 0.85] {
                let mut view = View::with_min_span((0.8, 1.2), 0.01);
                let x = PLOT.x + PLOT.w * fraction;
                let under = s.value_at(x, view, PLOT);
                view.zoom(2.0, s.about(fraction as f64), full);
                assert!((view.span() - 0.2).abs() < 1e-12, "it zoomed: {view:?}");
                let after = s.value_at(x, view, PLOT);
                assert!(
                    (after - under).abs() < 1e-6,
                    "reversed={reversed} at {fraction}: {under} became {after}"
                );
            }
        }
    }

    #[test]
    fn a_drag_right_moves_the_picture_right_on_both_orientations() {
        let full = (0.5, 1.5);
        for reversed in [false, true] {
            let s = LinearX { reversed };
            let mut view = View::with_min_span((0.8, 1.2), 0.01);
            // The value under a pixel, then a drag of 40 px to the right:
            // the same value sits 40 px further right.
            let x = PLOT.x + 150.0;
            let grabbed = s.value_at(x, view, PLOT);
            let dragged = 40.0 / PLOT.w as f64;
            view.pan(-dragged * s.pan_sign(), full);
            let moved = s.x_of(grabbed, view, PLOT);
            assert!(
                (moved - (x + 40.0)).abs() < 1e-3,
                "reversed={reversed}: {x} became {moved}"
            );
            // A pan right by a tenth moves the window right on screen: the
            // value at the plot's right edge comes a tenth of the way in.
            let mut view = View::with_min_span((0.8, 1.2), 0.01);
            let edge = s.value_at(PLOT.right(), view, PLOT);
            view.pan(0.1 * s.pan_sign(), full);
            let came_in = s.x_of(edge, view, PLOT);
            assert!(
                (came_in - (PLOT.right() - 0.1 * PLOT.w)).abs() < 1e-3,
                "reversed={reversed}: {came_in}"
            );
        }
    }

    #[test]
    fn a_zero_span_view_maps_to_the_plot_edge_and_has_no_ticks() {
        let view = View::with_min_span((1.0, 1.0), 0.0);
        for reversed in [false, true] {
            let s = LinearX { reversed };
            assert_eq!(s.x_of(1.0, view, PLOT), PLOT.x);
            assert!(s.value_at(250.0, view, PLOT).is_finite());
            let mut out = vec![Tick {
                x: 0.0,
                label: "stale".into(),
            }];
            x_ticks(s, view, PLOT, 64.0, XFormat::Price, &mut out);
            assert!(out.is_empty(), "cleared, and nothing to label");
        }
    }

    #[test]
    fn moneyness_ticks_are_1_2_5_and_read_as_percent() {
        // Edges sit off the tick values on purpose: `1.2 / 0.1` is not
        // exactly 12 in floating point, and a tick on the edge would come
        // and go with the rounding.
        let view = View::with_min_span((0.79, 1.21), 0.01);
        let mut out = Vec::new();
        x_ticks(
            LinearX::default(),
            view,
            PLOT,
            64.0,
            XFormat::Percent,
            &mut out,
        );
        assert_eq!(labels(&out), ["80%", "90%", "100%", "110%", "120%"]);
        assert!(out.windows(2).all(|w| w[1].x > w[0].x));
        assert!(out[0].x > PLOT.x && out[4].x < PLOT.right());
    }

    #[test]
    fn strike_and_log_moneyness_ticks_use_their_own_formats() {
        let mut out = Vec::new();
        let strikes = View::with_min_span((7000.0, 8000.0), 50.0);
        x_ticks(
            LinearX::default(),
            strikes,
            PLOT,
            64.0,
            XFormat::Price,
            &mut out,
        );
        assert_eq!(
            labels(&out),
            ["7000", "7200", "7400", "7600", "7800", "8000"]
        );
        let logm = View::with_min_span((-0.2, 0.1), 0.01);
        x_ticks(
            LinearX::default(),
            logm,
            PLOT,
            100.0,
            XFormat::Fixed(2),
            &mut out,
        );
        assert_eq!(labels(&out), ["-0.20", "-0.10", "0.00", "0.10"]);
    }

    #[test]
    fn the_delta_axis_reads_reversed_with_trader_labels() {
        // 1000 px wide, so the 5s have their 40 px beside the 10s.
        let view = View::with_min_span((0.02, 0.98), 0.02);
        let mut out = Vec::new();
        x_ticks(
            LinearX { reversed: true },
            view,
            Rect::new(100.0, 0.0, 1000.0, 200.0),
            40.0,
            XFormat::Delta,
            &mut out,
        );
        assert_eq!(labels(&out), ["5p", "10p", "25p", "50", "25c", "10c", "5c"]);
        assert!(
            out.windows(2).all(|w| w[1].x > w[0].x),
            "ticks come out in ascending pixel x"
        );
    }

    #[test]
    fn a_zoomed_delta_view_falls_back_to_nice_ticks() {
        // Fewer than three ladder rungs in view: 1-2-5 ticks, here every
        // 0.02, still labelled as deltas and still in ascending pixel x.
        let view = View::with_min_span((0.395, 0.465), 0.02);
        let mut out = Vec::new();
        x_ticks(
            LinearX { reversed: true },
            view,
            PLOT,
            64.0,
            XFormat::Delta,
            &mut out,
        );
        assert_eq!(labels(&out), ["46c", "44c", "42c", "40c"]);
        assert!(out.windows(2).all(|w| w[1].x > w[0].x));
    }

    #[test]
    fn delta_labels_name_the_put_or_call_delta() {
        assert_eq!(delta_label(0.90), "10p");
        assert_eq!(delta_label(0.75), "25p");
        assert_eq!(delta_label(0.50), "50");
        assert_eq!(delta_label(0.25), "25c");
        assert_eq!(delta_label(0.051), "5c");
    }

    fn plot(w: f32) -> Rect {
        Rect::new(0.0, 0.0, w, 200.0)
    }

    /// Delta tick labels in pixel order, at a 40 px tick gap on a plot `w`
    /// wide whose x origin is zero.
    fn delta_labels(reversed: bool, view: (f64, f64), w: f32) -> Vec<String> {
        let mut out = Vec::new();
        x_ticks(
            LinearX { reversed },
            View::with_min_span(view, 0.0),
            plot(w),
            40.0,
            XFormat::Delta,
            &mut out,
        );
        assert!(
            out.windows(2).all(|t| t[1].x > t[0].x),
            "ascending pixel x: {out:?}"
        );
        out.into_iter().map(|t| t.label).collect()
    }

    #[test]
    fn a_tick_at_zero_has_no_minus_sign() {
        let mut out = Vec::new();
        let view = View::with_min_span((-0.05, 0.3), 0.0);
        x_ticks(
            LinearX::default(),
            view,
            PLOT,
            100.0,
            XFormat::Fixed(2),
            &mut out,
        );
        assert_eq!(out[0].label, "0.00");
        let view = View::with_min_span((-0.5, 3.0), 0.0);
        x_ticks(
            LinearX::default(),
            view,
            PLOT,
            100.0,
            XFormat::Price,
            &mut out,
        );
        assert_eq!(out[0].label, "0");
    }

    #[test]
    fn delta_rungs_closer_than_the_tick_gap_give_way_by_priority() {
        // 400 px: 5 and 95 sit 20 px from 10 and 90, which outrank them.
        assert_eq!(
            delta_labels(true, (0.0, 1.0), 400.0),
            ["10p", "25p", "50", "25c", "10c"]
        );
        assert_eq!(
            delta_labels(false, (0.0, 1.0), 400.0),
            ["10c", "25c", "50", "25p", "10p"]
        );
        // 1000 px: 5 and 10 sit 50 px apart, so every rung has room.
        assert_eq!(
            delta_labels(true, (0.0, 1.0), 1000.0),
            ["5p", "10p", "25p", "50", "25c", "10c", "5c"]
        );
        // 160 px: 25, 50 and 75 are exactly one gap apart and stay; the
        // 10s are 24 px from the 25s.
        assert_eq!(delta_labels(true, (0.0, 1.0), 160.0), ["25p", "50", "25c"]);
    }

    #[test]
    fn a_delta_axis_with_room_for_one_rung_falls_back_to_nice_ticks() {
        // 70 px: 25 and 75 are 17.5 px from 50, so 50 alone survives the
        // thinning. One rung is no axis: the ticks are 1-2-5 instead, and
        // they reach the ends of the view, where no rung sits.
        let labels = delta_labels(true, (0.0, 1.0), 70.0);
        assert!(labels.len() >= 2, "{labels:?}");
        assert_eq!(labels.first().map(String::as_str), Some("0p"));
        assert_eq!(labels.last().map(String::as_str), Some("0c"));
    }

    #[test]
    fn two_rungs_in_view_are_nice_ticks_and_three_are_the_ladder() {
        // 10 and 25 alone: every 0.05 instead.
        assert_eq!(
            delta_labels(false, (0.07, 0.31), 400.0),
            ["10c", "15c", "20c", "25c", "30c"]
        );
        // 10, 25 and 50, far apart on 400 px: the rungs themselves.
        assert_eq!(
            delta_labels(false, (0.07, 0.52), 400.0),
            ["10c", "25c", "50"]
        );
    }

    #[test]
    fn ladder_ticks_label_whole_whatever_the_step() {
        // 2000 px at a 40 px gap over 0.22 of delta: the 1-2-5 step would be
        // 0.005, whose labels carry a decimal. The rungs are whole percents
        // by definition and read so.
        assert_eq!(
            delta_labels(false, (0.04, 0.26), 2000.0),
            ["5c", "10c", "25c"]
        );
    }

    #[test]
    fn delta_labels_take_the_decimals_the_step_needs() {
        assert_eq!(delta_label_with(0.505, 1), "49.5p");
        assert_eq!(delta_label_with(0.495, 1), "49.5c");
        assert_eq!(delta_label_with(0.405, 1), "40.5c");
        assert_eq!(delta_label_with(0.41, 1), "41.0c", "trailing zeros stay");
        assert_eq!(delta_label_with(0.50, 1), "50.0");
        assert_eq!(delta_label_with(0.4995, 2), "49.95c");
        assert_eq!(delta_label_with(0.50, 0), "50");
        assert_eq!(delta_label_with(0.90, 0), "10p");
        // `fmt_x` reads the decimals off the step, in percent units.
        assert_eq!(
            fmt_x(0.41, 0.01, XFormat::Delta),
            "41c",
            "a step of exactly 0.01 reads whole"
        );
        assert_eq!(
            fmt_x(0.41, 0.01 * (1.0 - 1e-12), XFormat::Delta),
            "41c",
            "nor does a step stored a hair under it"
        );
        assert_eq!(fmt_x(0.405, 0.005, XFormat::Delta), "40.5c");
        assert_eq!(fmt_x(0.4995, 0.0005, XFormat::Delta), "49.95c");
        assert_eq!(
            fmt_x(0.4995, 0.00005, XFormat::Delta),
            "49.95c",
            "two decimals at most"
        );
    }

    #[test]
    fn a_narrow_delta_view_repeats_no_label() {
        // A step of 0.002: whole percents would read 40c three times over.
        let labels = delta_labels(true, (0.40, 0.42), 400.0);
        assert!(labels.len() >= 5, "{labels:?}");
        assert!(labels.windows(2).all(|w| w[0] != w[1]), "{labels:?}");
        assert!(labels.iter().any(|l| l == "41.0c"), "{labels:?}");
        assert!(
            labels.iter().all(|l| l.contains('.')),
            "one axis, one precision: {labels:?}"
        );
    }

    #[test]
    fn fixed_labels_widen_to_the_decimals_the_step_needs() {
        let mut out = Vec::new();
        let view = View::with_min_span((-0.015, 0.015), 0.0);
        // 400 px at a 50 px gap: a step of 0.005, which two decimals
        // cannot tell apart.
        x_ticks(
            LinearX::default(),
            view,
            PLOT,
            50.0,
            XFormat::Fixed(2),
            &mut out,
        );
        let l = labels(&out);
        assert!(l.windows(2).all(|w| w[0] != w[1]), "{l:?}");
        assert!(l.contains(&"0.000") && l.contains(&"0.005"), "{l:?}");
        let zero_with_a_sign = |s: &&str| s.starts_with('-') && s.parse::<f64>() == Ok(0.0);
        assert!(!l.iter().any(zero_with_a_sign), "{l:?}");
        // The asked-for decimals are the floor, six the ceiling.
        assert_eq!(fmt_x(1.5, 0.5, XFormat::Fixed(2)), "1.50");
        assert_eq!(fmt_x(0.123456789, 1e-9, XFormat::Fixed(2)), "0.123457");
        // Only the step's share is capped: more decimals asked for are given.
        assert_eq!(fmt_x(0.123456789, 0.5, XFormat::Fixed(8)), "0.12345679");
    }

    #[test]
    fn no_format_repeats_a_label_on_adjacent_ticks() {
        let cases: [(XFormat, &[(f64, f64)]); 4] = [
            (
                XFormat::Price,
                &[
                    (7000.0, 8000.0),
                    (99.93, 100.07),
                    (0.0012, 0.0019),
                    (-3.0, 250_000.0),
                ],
            ),
            (
                XFormat::Percent,
                &[(0.79, 1.21), (0.991, 1.012), (0.0, 3.0)],
            ),
            (
                XFormat::Fixed(2),
                &[
                    (-0.2, 0.1),
                    (-0.015, 0.015),
                    (0.40013, 0.40031),
                    (-40.0, 900.0),
                ],
            ),
            (
                XFormat::Delta,
                &[(0.02, 0.98), (0.40, 0.42), (0.4991, 0.5013), (0.07, 0.31)],
            ),
        ];
        let mut out = Vec::new();
        for (format, views) in cases {
            for &view in views {
                for reversed in [false, true] {
                    for gap in [40.0, 64.0, 100.0] {
                        x_ticks(
                            LinearX { reversed },
                            View::with_min_span(view, 0.0),
                            PLOT,
                            gap,
                            format,
                            &mut out,
                        );
                        let l = labels(&out);
                        let case = format!("{format:?} {view:?} reversed {reversed} gap {gap}");
                        assert!(l.len() >= 2, "{case}: {l:?}");
                        assert!(l.windows(2).all(|w| w[0] != w[1]), "{case}: {l:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_view_no_tick_of_the_first_step_falls_in_still_has_two_labels() {
        // 180 px at a 64 px gap asks for two ticks over 0.45 of delta: a
        // step of 0.5, and no multiple of 0.5 lies in the view. Only two
        // ladder rungs fit, so the ladder does not label it either.
        let mut out = Vec::new();
        x_ticks(
            LinearX { reversed: true },
            View::with_min_span((0.54, 0.99), 0.0),
            plot(180.0),
            64.0,
            XFormat::Delta,
            &mut out,
        );
        assert_eq!(labels(&out), ["20p", "40p"], "a step of 0.2");
        assert!(out[0].x < out[1].x);
    }

    #[test]
    fn a_finer_step_labels_with_its_own_decimals() {
        // 180 px over 0.016: a step of 0.01 has the one tick 0.02 in view.
        // The step below it, 0.005, has three, and two decimals cannot tell
        // them apart.
        let mut out = Vec::new();
        x_ticks(
            LinearX::default(),
            View::with_min_span((0.012, 0.028), 0.0),
            plot(180.0),
            64.0,
            XFormat::Fixed(2),
            &mut out,
        );
        assert_eq!(labels(&out), ["0.015", "0.020", "0.025"]);
    }

    #[test]
    fn a_narrow_plot_keeps_at_least_two_labels() {
        let cases: [(XFormat, &[(f64, f64)]); 4] = [
            // Strikes.
            (
                XFormat::Price,
                &[(7030.0, 7790.0), (5100.0, 9400.0), (96.0, 143.0)],
            ),
            // Moneyness.
            (
                XFormat::Percent,
                &[(0.83, 1.17), (0.54, 0.99), (1.02, 1.46)],
            ),
            // Log-moneyness.
            (
                XFormat::Fixed(2),
                &[(-0.23, 0.14), (0.012, 0.028), (-0.46, -0.03)],
            ),
            (
                XFormat::Delta,
                &[(0.54, 0.99), (0.31, 0.69), (0.02, 0.46), (0.52, 0.74)],
            ),
        ];
        let mut out = Vec::new();
        for (format, views) in cases {
            for &view in views {
                for reversed in [false, true] {
                    for w in [150.0, 180.0, 220.0] {
                        let plot = Rect::new(44.0, 0.0, w, 200.0);
                        x_ticks(
                            LinearX { reversed },
                            View::with_min_span(view, 0.0),
                            plot,
                            64.0,
                            format,
                            &mut out,
                        );
                        let case = format!("{format:?} {view:?} reversed {reversed} on {w} px");
                        let l = labels(&out);
                        assert!(l.len() >= 2, "{case}: {l:?}");
                        assert!(out.windows(2).all(|t| t[1].x > t[0].x), "{case}: {out:?}");
                        assert!(l.windows(2).all(|t| t[0] != t[1]), "{case}: {l:?}");
                        assert!(
                            out.iter().all(|t| t.x >= plot.x && t.x <= plot.right()),
                            "{case}: {out:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_view_too_narrow_to_count_ticks_in_returns_what_it_has() {
        // Three float steps wide at 2^50: the values cannot tell ticks of
        // a finer step apart, so none is tried, and the call returns.
        let lo = 2f64.powi(50) + 0.625;
        let view = View::with_min_span((lo, lo + 0.375), 0.0);
        assert!(!countable(view, 0.05));
        assert!(countable(view, 0.2));
        assert!(!countable(view, 0.0) && !countable(view, f64::NAN));
        assert!(!countable(view, f64::INFINITY));
        let mut out = Vec::new();
        for format in [XFormat::Price, XFormat::Fixed(2)] {
            x_ticks(
                LinearX::default(),
                view,
                plot(180.0),
                64.0,
                format,
                &mut out,
            );
            assert!(out.len() <= 4, "{out:?}");
        }
    }

    #[test]
    fn the_step_below_a_1_2_5_step_is_the_next_on_the_ladder() {
        for (step, finer) in [
            (10.0, 5.0),
            (5.0, 2.0),
            (2.0, 1.0),
            (1.0, 0.5),
            (0.5, 0.2),
            (0.2, 0.1),
            (0.1, 0.05),
            (2000.0, 1000.0),
            (1e-7, 5e-8),
        ] {
            let got = finer_step(step);
            assert!(
                (got - finer).abs() <= finer * 1e-9,
                "{step}: {got} for {finer}"
            );
        }
    }

    #[test]
    fn a_plot_with_no_width_has_no_ticks() {
        let view = View::with_min_span((0.02, 0.98), 0.01);
        for w in [0.0, -5.0, f32::NAN, f32::INFINITY] {
            for format in [XFormat::Price, XFormat::Delta] {
                let mut out = vec![Tick {
                    x: 0.0,
                    label: "stale".into(),
                }];
                x_ticks(LinearX::default(), view, plot(w), 40.0, format, &mut out);
                assert!(out.is_empty(), "width {w}, {format:?}: {out:?}");
            }
        }
    }
}
