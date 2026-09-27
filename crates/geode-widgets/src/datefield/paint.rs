//! Date-time segments with rest, active, and typing states, separators, and an optional
//! suffix. The host supplies prepared SegmentPaint colours and padding policy; the
//! painter does not read the theme. The caller also supplies the font family.

use gpui::prelude::*;
use gpui::{App, Div, Hsla, MouseButton, Pixels, SharedString, Window, div};
use gpui_component::h_flex;

use super::{Segment, SegmentText};

/// Every colour the painter uses, and whether segments are padded.
/// `Copy`, derived once by the host and handed in per paint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentPaint {
    pub rest_text: Hsla,
    pub rest_fill: Option<Hsla>,
    pub active_text: Hsla,
    pub active_fill: Hsla,
    pub typing_text: Hsla,
    pub typing_fill: Hsla,
    pub separator: Hsla,
    pub suffix: Hsla,
    pub radius: Pixels,
    /// Remove horizontal segment padding for a field replacing plain date text in a
    /// grid cell. Separators, font metrics, and any suffix still determine the rendered
    /// width.
    pub flush: bool,
}

/// Separator before a segment's painted index: dashes within the date,
/// a space before the hour, and colons within the time. The slice must be a
/// prefix of year/month/day/hour/minute/second; separators and click targets
/// both derive segment identity from that position.
fn separator_before(i: usize) -> Option<&'static str> {
    match i {
        0 => None,
        1 | 2 => Some("-"),
        3 => Some(" "),
        _ => Some(":"),
    }
}

/// Paint prepared segments in the prefix order returned by
/// [`super::DateTimeField::segments`], followed by an optional suffix. Typing
/// colors take precedence over active colors, then rest colors apply.
///
/// Left mouse-down invokes `on_segment` synchronously, then stops propagation
/// so an enclosing editor does not also treat it as an outside click. The
/// callback owns state changes, focus, and repaint notification; this painter
/// installs no keyboard handler. The host sets the container's font family.
///
/// Debug selectors are `"{selector}-{i}"` for segments and
/// `"{selector}-suffix"` for the suffix.
pub fn paint(
    segments: &[SegmentText],
    suffix: Option<SharedString>,
    paint: SegmentPaint,
    selector: SharedString,
    on_segment: impl Fn(Segment, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let mut row = h_flex().items_center();
    for (i, seg) in segments.iter().enumerate() {
        if let Some(sep) = separator_before(i) {
            row = row.child(div().text_color(paint.separator).child(sep));
        }
        let (text, fill) = if seg.typing {
            (paint.typing_text, Some(paint.typing_fill))
        } else if seg.active {
            (paint.active_text, Some(paint.active_fill))
        } else {
            (paint.rest_text, paint.rest_fill)
        };
        let on_segment = on_segment.clone();
        let sel = selector.clone();
        row = row.child(
            div()
                .when(!paint.flush, |d| d.px_0p5())
                .rounded(paint.radius)
                .text_color(text)
                .when_some(fill, |d, f| d.bg(f))
                .debug_selector(move || format!("{sel}-{i}"))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    if let Some(segment) = Segment::at(i) {
                        on_segment(segment, window, cx);
                    }
                    cx.stop_propagation();
                })
                .child(seg.text.clone()),
        );
    }
    if let Some(suffix) = suffix {
        let sel = selector.clone();
        row = row.child(
            div()
                .ml_2()
                .text_color(paint.suffix)
                .debug_selector(move || format!("{sel}-suffix"))
                .child(suffix),
        );
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext, div, hsla, px};

    struct Probe;
    impl gpui::Render for Probe {
        fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let segments = vec![
                SegmentText {
                    text: "2026".into(),
                    active: false,
                    typing: false,
                },
                SegmentText {
                    text: "09".into(),
                    active: false,
                    typing: false,
                },
                SegmentText {
                    text: "18".into(),
                    active: true,
                    typing: false,
                },
                SegmentText {
                    text: "18".into(),
                    active: false,
                    typing: false,
                },
                SegmentText {
                    text: "00".into(),
                    active: false,
                    typing: false,
                },
                SegmentText {
                    text: "00".into(),
                    active: false,
                    typing: false,
                },
            ];
            let paint = SegmentPaint {
                rest_text: hsla(0., 0., 0.9, 1.),
                rest_fill: None,
                active_text: hsla(0., 0., 0.1, 1.),
                active_fill: hsla(0.1, 1., 0.5, 1.),
                typing_text: hsla(0., 0., 0.9, 1.),
                typing_fill: hsla(0.6, 0.5, 0.3, 1.),
                separator: hsla(0., 0., 0.5, 1.),
                suffix: hsla(0., 0., 0.5, 1.),
                radius: px(3.),
                flush: false,
            };
            div().child(super::paint(
                &segments,
                Some("EDT".into()),
                paint,
                "probe-seg".into(),
                |_segment, _w, _cx| {},
            ))
        }
    }

    #[gpui::test]
    fn every_segment_and_the_suffix_are_painted_with_their_selectors(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| cx.open_window(gpui::WindowOptions::default(), |_w, cx| cx.new(|_| Probe)))
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|w, cx| {
            let _ = w.draw(cx);
        });
        let mut bounds = Vec::new();
        for i in 0..6 {
            // `debug_bounds` requires static selectors. This test leaks six short
            // strings per invocation to provide them.
            let b = vcx.debug_bounds(Box::leak(format!("probe-seg-{i}").into_boxed_str()));
            assert!(b.is_some(), "segment {i}");
            bounds.push(b.unwrap());
        }
        assert!(vcx.debug_bounds("probe-seg-suffix").is_some());
        // Check every adjacent pair: checking only the endpoints would miss
        // an internal segment appearing out of order.
        for pair in bounds.windows(2) {
            assert!(
                pair[1].origin.x > pair[0].origin.x,
                "painted left to right: {pair:?}"
            );
        }
    }

    #[test]
    fn separators_are_dashes_inside_the_date_a_space_then_colons() {
        assert_eq!(
            (0..6).map(separator_before).collect::<Vec<_>>(),
            [None, Some("-"), Some("-"), Some(" "), Some(":"), Some(":")]
        );
    }
}
