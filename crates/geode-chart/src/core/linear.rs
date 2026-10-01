//! A linear x axis: a value maps to a pixel by its place in the view. The
//! strike-like axes of a slice chart use it; `reversed` runs the axis right
//! to left, for call delta, so strike still increases left to right.

use super::Rect;
use super::scale::{LinearScale, fmt_tick};
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
    /// A fixed number of decimals.
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
}

/// A call delta as a trader says it: the put delta above a half, the call
/// delta below, and plain `50` at the money.
pub fn delta_label(delta: f64) -> String {
    let pct = (delta * 100.0).round();
    if pct == 50.0 {
        "50".to_string()
    } else if pct > 50.0 {
        format!("{}p", 100.0 - pct)
    } else {
        format!("{pct}c")
    }
}

/// One x value as its axis labels it. `step` is the tick step, which sets
/// the decimals of the stepped formats.
pub fn fmt_x(value: f64, step: f64, format: XFormat) -> String {
    match format {
        XFormat::Price => fmt_tick(value, step),
        XFormat::Percent => format!("{}%", fmt_tick(value * 100.0, step * 100.0)),
        XFormat::Fixed(n) => format!("{value:.*}", n as usize),
        XFormat::Delta => delta_label(value),
    }
}

/// Ticks for the view into `out` (cleared first), in ascending pixel x.
/// 1-2-5 steps, except a delta axis showing at least three rungs of
/// [`DELTA_LADDER`], which labels those.
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
    if span.is_nan() || span <= 0.0 || plot.w <= 0.0 {
        return;
    }
    let hint = ((plot.w / tick_gap_px.max(1.0)) as usize).max(2);
    let step = LinearScale::nice_step(span, hint);
    let mut values: Vec<f64> = Vec::new();
    if format == XFormat::Delta {
        values.extend(
            DELTA_LADDER
                .iter()
                .copied()
                .filter(|d| *d >= view.lo && *d <= view.hi),
        );
        if values.len() < 3 {
            values.clear();
        }
    }
    if values.is_empty() {
        LinearScale::new((view.lo, view.hi), 0.0, 1.0).ticks(hint, &mut values);
    }
    out.extend(values.into_iter().map(|v| Tick {
        x: scale.x_of(v, view, plot),
        label: fmt_x(v, step, format),
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
        let view = View::with_min_span((0.02, 0.98), 0.02);
        let mut out = Vec::new();
        x_ticks(
            LinearX { reversed: true },
            view,
            PLOT,
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
}
