//! The percentile slider: where the current value sits in its window's
//! ranks, a track filled to a marker with quartile ticks, the marker in the
//! warning color at or beyond the decile edges. The rank number is painted
//! beside it from the host's text.

use gpui::{
    Div, Hsla, InteractiveElement as _, ParentElement, Pixels, SharedString, Styled, div, px,
    relative,
};

use super::design;

pub const QUARTILES: [f32; 3] = [0.25, 0.5, 0.75];
pub const WARN_LOW: f64 = 0.10;
pub const WARN_HIGH: f64 = 0.90;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slider {
    /// Marker position along the track in `[0, 1]`.
    pub position: f32,
    /// At or beyond a decile edge.
    pub warn: bool,
}

/// The slider for a rank in `[0, 1]` (the share of the window's daily values
/// strictly below the current). A rank outside the range is clamped for the
/// marker and still warns; a non-finite rank has no slider.
pub fn slider(rank: f64) -> Option<Slider> {
    if !rank.is_finite() {
        return None;
    }
    Some(Slider {
        position: rank.clamp(0.0, 1.0) as f32,
        warn: rank <= WARN_LOW || rank >= WARN_HIGH,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliderPaint {
    pub track: Hsla,
    pub fill: Hsla,
    pub tick: Hsla,
    pub marker: Hsla,
    pub warning: Hsla,
    pub text: Hsla,
    pub radius: Pixels,
}

/// Track geometry at the design rem (the mockup's 56 x 4 px track and
/// 2 x 10 px marker at a 13 px base).
pub(crate) const TRACK_WIDTH: f32 = super::widths::SLIDER_TRACK;
const TRACK_HEIGHT: f32 = 4.0;
const MARKER_WIDTH: f32 = 2.0;
const MARKER_HEIGHT: f32 = 10.0;
pub(crate) const GAP: f32 = super::widths::SLIDER_GAP;
/// Quartile ticks are one pixel: a hairline, not a size.
const TICK: f32 = 1.0;

/// Track with fill to the marker, quartile ticks and the marker (the
/// warning color at the edges), then the host's rank text. With no slider
/// the text alone.
pub fn paint(
    text: SharedString,
    slider: Option<Slider>,
    paint: SliderPaint,
    id: SharedString,
) -> Div {
    let text_sel = id.clone();
    // The label takes the space after the track, right-aligned, so in a
    // column of cells every track starts at the cell's leading edge
    // whatever the width of its number.
    let label = div()
        .flex_1()
        .flex()
        .justify_end()
        .text_color(paint.text)
        .debug_selector(move || format!("{text_sel}-text"))
        .child(text);
    let Some(s) = slider else {
        return div().w_full().flex().items_center().child(label);
    };
    let (track_sel, fill_sel, marker_sel) = (id.clone(), id.clone(), id);
    let marker = if s.warn { paint.warning } else { paint.marker };
    let track = div()
        .relative()
        .flex_none()
        .w(design(TRACK_WIDTH))
        .h(design(TRACK_HEIGHT))
        .rounded(paint.radius)
        .bg(paint.track)
        .debug_selector(move || format!("{track_sel}-track"))
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .h_full()
                .w(relative(s.position))
                .rounded(paint.radius)
                .bg(paint.fill)
                .debug_selector(move || format!("{fill_sel}-fill")),
        )
        .children(QUARTILES.map(|q| {
            div()
                .absolute()
                .top_0()
                .h_full()
                .w(px(TICK))
                .left(relative(q))
                .bg(paint.tick)
        }))
        .child(
            // Centered on the position: a negative half-width margin keeps
            // the marker's center, not its leading edge, at the rank.
            div()
                .absolute()
                .left(relative(s.position))
                .ml(design(-MARKER_WIDTH / 2.0))
                .top(design(-(MARKER_HEIGHT - TRACK_HEIGHT) / 2.0))
                .w(design(MARKER_WIDTH))
                .h(design(MARKER_HEIGHT))
                .bg(marker)
                .debug_selector(move || format!("{marker_sel}-marker")),
        );
    div()
        .w_full()
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
    fn position_is_the_rank_and_the_middle_does_not_warn() {
        assert_eq!(
            slider(0.5),
            Some(Slider {
                position: 0.5,
                warn: false
            })
        );
        assert_eq!(
            slider(0.23),
            Some(Slider {
                position: 0.23,
                warn: false
            })
        );
    }

    #[test]
    fn the_decile_edges_warn_inclusive() {
        assert!(slider(0.10).unwrap().warn);
        assert!(slider(0.90).unwrap().warn);
        assert!(slider(0.91).unwrap().warn);
        assert!(slider(0.0).unwrap().warn);
        assert!(!slider(0.1001).unwrap().warn);
        assert!(!slider(0.8999).unwrap().warn);
    }

    #[test]
    fn an_out_of_range_rank_clamps_and_still_warns() {
        assert_eq!(
            slider(1.4),
            Some(Slider {
                position: 1.0,
                warn: true
            })
        );
        assert_eq!(
            slider(-0.2),
            Some(Slider {
                position: 0.0,
                warn: true
            })
        );
    }

    #[test]
    fn a_non_finite_rank_has_no_slider() {
        assert_eq!(slider(f64::NAN), None);
        assert_eq!(slider(f64::INFINITY), None);
    }

    fn test_paint() -> SliderPaint {
        SliderPaint {
            track: hsla(0.0, 0.0, 0.2, 1.0),
            fill: hsla(0.6, 0.6, 0.5, 1.0),
            tick: hsla(0.0, 0.0, 0.1, 1.0),
            marker: hsla(0.0, 0.0, 0.9, 1.0),
            warning: hsla(0.1, 1.0, 0.5, 1.0),
            text: hsla(0.0, 0.0, 0.6, 1.0),
            radius: px(2.0),
        }
    }

    struct Probe(Option<Slider>);
    impl gpui::Render for Probe {
        fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
            div().child(paint("75".into(), self.0, test_paint(), "probe".into()))
        }
    }

    fn drawn(cx: &mut TestAppContext, slider: Option<Slider>) -> VisualTestContext {
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |_w, cx| {
                    cx.new(|_| Probe(slider))
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
    fn the_fill_and_marker_sit_at_the_rank(cx: &mut TestAppContext) {
        let mut vcx = drawn(cx, slider(0.75));
        let track = vcx.debug_bounds("probe-track").expect("track");
        let fill = vcx.debug_bounds("probe-fill").expect("fill");
        let marker = vcx.debug_bounds("probe-marker").expect("marker");
        assert!(
            vcx.debug_bounds("probe-text").is_some(),
            "the rank number is painted"
        );
        let w = track.size.width.as_f32();
        assert!(w > 0.0);
        assert!(
            (fill.size.width.as_f32() - 0.75 * w).abs() <= 1.0,
            "fill {fill:?} track {track:?}"
        );
        let center = marker.origin.x.as_f32() + marker.size.width.as_f32() / 2.0;
        let at = track.origin.x.as_f32() + 0.75 * w;
        assert!((center - at).abs() <= 1.0, "marker center {center} at {at}");
        let mut vcx = drawn(cx, None);
        assert!(vcx.debug_bounds("probe-track").is_none());
        assert!(
            vcx.debug_bounds("probe-text").is_some(),
            "no rank: the text alone"
        );
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;
    use gpui::{
        AppContext as _, IntoElement, TestAppContext, VisualTestContext, Window, div, hsla, px,
    };

    struct Column;
    impl gpui::Render for Column {
        fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let p = SliderPaint {
                track: hsla(0.0, 0.0, 0.2, 1.0),
                fill: hsla(0.6, 0.6, 0.5, 1.0),
                tick: hsla(0.0, 0.0, 0.1, 1.0),
                marker: hsla(0.0, 0.0, 0.9, 1.0),
                warning: hsla(0.1, 1.0, 0.5, 1.0),
                text: hsla(0.0, 0.0, 0.6, 1.0),
                radius: px(2.0),
            };
            let row = |text: &'static str, id: &'static str| {
                div().w(px(160.0)).flex().justify_end().child(paint(
                    text.into(),
                    slider(0.5),
                    p,
                    id.into(),
                ))
            };
            div()
                .flex()
                .flex_col()
                .child(row("5", "r0"))
                .child(row("100", "r1"))
        }
    }

    #[gpui::test]
    fn tracks_line_up_down_a_right_aligned_column(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| {
                cx.open_window(gpui::WindowOptions::default(), |_w, cx| cx.new(|_| Column))
            })
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|w, cx| {
            let _ = w.draw(cx);
        });
        let a = vcx.debug_bounds("r0-track").expect("row 0 track");
        let b = vcx.debug_bounds("r1-track").expect("row 1 track");
        assert!(
            (a.origin.x.as_f32() - b.origin.x.as_f32()).abs() < 0.5,
            "a one-digit and a three-digit rank share the track's x: {a:?} {b:?}"
        );
    }
}
