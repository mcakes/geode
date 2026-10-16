//! The heat tint: one hue behind the number, its alpha the value's position
//! in the grid's range up to a host ceiling, so levels read light to dark
//! across the whole grid. The number is the cell's text, unchanged.

use gpui::prelude::FluentBuilder as _;
use gpui::{Div, Hsla, InteractiveElement as _, ParentElement, SharedString, Styled, div};

use super::design;

/// Alpha in `[0, ceiling]` for `value`'s position in `[lo, hi]`, clamped.
/// `None` (no tint) for a non-finite input or a range with `lo >= hi`: a
/// grid whose values are all equal has no levels to show.
pub fn heat(value: f64, lo: f64, hi: f64, ceiling: f32) -> Option<f32> {
    if !(value.is_finite() && lo.is_finite() && hi.is_finite()) || lo >= hi {
        return None;
    }
    let t = ((value - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
    Some(t * ceiling.clamp(0.0, 1.0))
}

/// The finite min and max of `values`, for the host to compute once over
/// the grid it tints.
pub fn range(values: impl IntoIterator<Item = f64>) -> Option<(f64, f64)> {
    values
        .into_iter()
        .filter(|v| v.is_finite())
        .fold(None, |acc, v| match acc {
            None => Some((v, v)),
            Some((lo, hi)) => Some((lo.min(v), hi.max(v))),
        })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeatPaint {
    pub hue: Hsla,
    pub text: Hsla,
}

/// Horizontal text inset at the design rem.
pub(crate) const INSET: f32 = super::widths::HEAT_INSET;

/// The number on its tint; with no alpha (or a zero one) the number alone.
pub fn paint(text: SharedString, alpha: Option<f32>, paint: HeatPaint, id: SharedString) -> Div {
    let text_sel = id.clone();
    div()
        .px(design(INSET))
        .when_some(alpha.filter(|a| *a > 0.0), |d, a| {
            d.bg(paint.hue.opacity(a))
                .debug_selector(move || format!("{id}-heat"))
        })
        .child(
            div()
                .text_color(paint.text)
                .debug_selector(move || format!("{text_sel}-text"))
                .child(text),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, IntoElement, TestAppContext, VisualTestContext, Window, div, hsla,
    };

    #[test]
    fn alpha_is_position_in_the_range_times_the_ceiling() {
        assert_eq!(heat(10.0, 10.0, 20.0, 0.4), Some(0.0));
        assert_eq!(heat(20.0, 10.0, 20.0, 0.4), Some(0.4));
        assert!((heat(15.0, 10.0, 20.0, 0.4).unwrap() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn a_value_outside_the_range_clamps() {
        assert_eq!(heat(25.0, 10.0, 20.0, 0.4), Some(0.4));
        assert_eq!(heat(5.0, 10.0, 20.0, 0.4), Some(0.0));
    }

    #[test]
    fn a_flat_range_tints_nothing() {
        assert_eq!(heat(15.0, 15.0, 15.0, 0.4), None);
        assert_eq!(
            heat(15.0, 20.0, 10.0, 0.4),
            None,
            "an inverted range is not a range"
        );
    }

    #[test]
    fn non_finite_inputs_tint_nothing() {
        assert_eq!(heat(f64::NAN, 10.0, 20.0, 0.4), None);
        assert_eq!(heat(15.0, f64::NAN, 20.0, 0.4), None);
    }

    #[test]
    fn range_is_the_finite_min_and_max() {
        assert_eq!(range([3.0, f64::NAN, -1.0, 2.0]), Some((-1.0, 3.0)));
        assert_eq!(range([f64::NAN]), None);
        assert_eq!(range(std::iter::empty()), None);
    }

    struct Probe(Option<f32>);
    impl gpui::Render for Probe {
        fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let paint_ = HeatPaint {
                hue: hsla(0.62, 0.9, 0.5, 1.0),
                text: hsla(0.0, 0.0, 0.9, 1.0),
            };
            div().child(paint("22.8".into(), self.0, paint_, "probe".into()))
        }
    }

    fn drawn(cx: &mut TestAppContext, alpha: Option<f32>) -> VisualTestContext {
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |_w, cx| {
                    cx.new(|_| Probe(alpha))
                })
            })
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|w, cx| {
            let _ = w.draw(cx);
        });
        vcx
    }

    #[gpui::test]
    fn heat_keeps_the_number_and_tints_only_with_an_alpha(cx: &mut TestAppContext) {
        let mut vcx = drawn(cx, Some(0.3));
        assert!(vcx.debug_bounds("probe-heat").is_some(), "tinted");
        assert!(vcx.debug_bounds("probe-text").is_some());
        let mut vcx = drawn(cx, Some(0.0));
        assert!(
            vcx.debug_bounds("probe-heat").is_none(),
            "a zero alpha adds no tint"
        );
        let mut vcx = drawn(cx, None);
        assert!(vcx.debug_bounds("probe-heat").is_none());
        assert!(
            vcx.debug_bounds("probe-text").is_some(),
            "no tint: the number is still painted"
        );
    }
}
