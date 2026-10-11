//! The bar from zero: a signed spread drawn as a bar from a center axis,
//! its length the value's share of the column's largest magnitude, on its
//! sign's side; the signed number painted beside it from the host's text.

use geode_core::format::Sign;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Div, Hsla, InteractiveElement as _, ParentElement, Pixels, SharedString, Styled, div, px,
    relative,
};

use super::design;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub sign: Sign,
    /// Share of one half-width in `[0, 1]`.
    pub extent: f32,
}

/// The bar for `value` against the column `scale`. `None` for a non-finite
/// value; zero, or a zero or non-finite scale, draws no bar.
pub fn bar(value: f64, scale: f64) -> Option<Bar> {
    if !value.is_finite() {
        return None;
    }
    let sign = if value > 0.0 {
        Sign::Positive
    } else if value < 0.0 {
        Sign::Negative
    } else {
        Sign::Zero
    };
    let extent = if scale.is_finite() && scale > 0.0 {
        (value.abs() / scale).min(1.0) as f32
    } else {
        0.0
    };
    Some(Bar { sign, extent })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarPaint {
    pub positive: Hsla,
    pub negative: Hsla,
    pub zero: Hsla,
    pub axis: Hsla,
    /// Plain text color for a value with no bar.
    pub text: Hsla,
    /// Bar fill alpha over its sign color (the mockup's 0.75).
    pub fill_alpha: f32,
    pub radius: Pixels,
}

/// Bar track geometry at the design rem (the mockup's 64 x 10 px at 13 px).
const TRACK_WIDTH: f32 = 60.0;
const TRACK_HEIGHT: f32 = 10.0;
/// The bar's vertical inset inside the track.
const BAR_INSET: f32 = 2.0;
const GAP: f32 = 6.0;
/// The zero axis is a hairline.
const AXIS: f32 = 1.0;

/// The track with its zero axis and the bar on the sign's side, then the
/// host's signed text in the sign's color. With no bar the text alone, in
/// the plain text color.
pub fn paint(text: SharedString, bar: Option<Bar>, paint: BarPaint, id: SharedString) -> Div {
    let text_sel = id.clone();
    let color = bar.map_or(paint.text, |b| match b.sign {
        Sign::Positive => paint.positive,
        Sign::Negative => paint.negative,
        Sign::Zero => paint.zero,
    });
    let label = div()
        .text_color(color)
        .debug_selector(move || format!("{text_sel}-text"))
        .child(text);
    let Some(b) = bar else {
        return div().flex().items_center().child(label);
    };
    let (track_sel, axis_sel, bar_sel) = (id.clone(), id.clone(), id);
    // Half the track is one unit of extent; the bar starts at the axis.
    let half = b.extent * 0.5;
    let start = match b.sign {
        Sign::Negative => 0.5 - half,
        _ => 0.5,
    };
    let track = div()
        .relative()
        .flex_none()
        .w(design(TRACK_WIDTH))
        .h(design(TRACK_HEIGHT))
        .debug_selector(move || format!("{track_sel}-track"))
        .child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .left(relative(0.5))
                .w(px(AXIS))
                .bg(paint.axis)
                .debug_selector(move || format!("{axis_sel}-axis")),
        )
        .when(half > 0.0, |d| {
            d.child(
                div()
                    .absolute()
                    .top(design(BAR_INSET))
                    .bottom(design(BAR_INSET))
                    .left(relative(start))
                    .w(relative(half))
                    .rounded(paint.radius)
                    .bg(color.opacity(paint.fill_alpha))
                    .debug_selector(move || format!("{bar_sel}-bar")),
            )
        });
    div()
        .flex()
        .items_center()
        .gap(design(GAP))
        .child(track)
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, IntoElement, TestAppContext, VisualTestContext, Window, div, hsla, px,
    };

    #[test]
    fn extent_is_magnitude_over_the_scale_on_the_signs_side() {
        let b = bar(3.4, 10.0).unwrap();
        assert_eq!(b.sign, Sign::Positive);
        assert!((b.extent - 0.34).abs() < 1e-6);
        assert_eq!(bar(-1.2, 10.0).map(|b| b.sign), Some(Sign::Negative));
        assert!((bar(-1.2, 10.0).unwrap().extent - 0.12).abs() < 1e-6);
    }

    #[test]
    fn a_value_beyond_the_scale_fills_the_half_and_no_more() {
        assert_eq!(bar(25.0, 10.0).unwrap().extent, 1.0);
        assert_eq!(bar(-25.0, 10.0).unwrap().extent, 1.0);
    }

    #[test]
    fn zero_and_a_zero_scale_draw_no_bar() {
        assert_eq!(
            bar(0.0, 10.0),
            Some(Bar {
                sign: Sign::Zero,
                extent: 0.0
            })
        );
        assert_eq!(bar(2.0, 0.0).unwrap().extent, 0.0);
        assert_eq!(bar(2.0, f64::NAN).unwrap().extent, 0.0);
    }

    #[test]
    fn a_non_finite_value_has_no_bar() {
        assert_eq!(bar(f64::NAN, 10.0), None);
    }

    fn test_paint() -> BarPaint {
        BarPaint {
            positive: hsla(0.47, 0.6, 0.4, 1.0),
            negative: hsla(0.0, 0.8, 0.6, 1.0),
            zero: hsla(0.0, 0.0, 0.6, 1.0),
            axis: hsla(0.0, 0.0, 0.3, 1.0),
            text: hsla(0.0, 0.0, 0.9, 1.0),
            fill_alpha: 0.75,
            radius: px(2.0),
        }
    }

    struct Probe(Option<Bar>);
    impl gpui::Render for Probe {
        fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
            div().child(paint("-5.0".into(), self.0, test_paint(), "probe".into()))
        }
    }

    fn drawn(cx: &mut TestAppContext, bar: Option<Bar>) -> VisualTestContext {
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |_w, cx| {
                    cx.new(|_| Probe(bar))
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
    fn a_negative_bar_grows_left_of_the_axis(cx: &mut TestAppContext) {
        let mut vcx = drawn(cx, bar(-5.0, 10.0));
        let track = vcx.debug_bounds("probe-track").expect("track");
        let axis = vcx.debug_bounds("probe-axis").expect("axis");
        let b = vcx.debug_bounds("probe-bar").expect("bar");
        let w = track.size.width.as_f32();
        let right = b.origin.x.as_f32() + b.size.width.as_f32();
        assert!(
            (right - axis.origin.x.as_f32()).abs() <= 1.0,
            "bar {b:?} axis {axis:?}"
        );
        assert!((b.size.width.as_f32() - 0.25 * w).abs() <= 1.0);
        let mut vcx = drawn(cx, bar(5.0, 10.0));
        let axis = vcx.debug_bounds("probe-axis").expect("axis");
        let b = vcx.debug_bounds("probe-bar").expect("bar");
        assert!(
            (b.origin.x.as_f32() - axis.origin.x.as_f32()).abs() <= 1.0,
            "a positive bar starts at the axis"
        );
        let mut vcx = drawn(cx, None);
        assert!(vcx.debug_bounds("probe-track").is_none());
        assert!(vcx.debug_bounds("probe-text").is_some());
    }
}
