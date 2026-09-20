//! The date-time field's painter (spec §4.3): the segments in the data
//! face with the three states — rest, active, typing — separators
//! between, an optional suffix after. Every colour is the host's, handed
//! in as a [`SegmentPaint`]: the painter never reads `cx.theme()`, so it
//! can be called from a closure that cannot borrow it (the `key_chip`
//! precedent), and a host derives its colours once (the panel's
//! `FlooredTones`, the shell's chip/control doors) rather than per paint.

use gpui::prelude::*;
use gpui::{App, Div, Hsla, MouseButton, Pixels, SharedString, Window, div};
use gpui_component::h_flex;

use super::{Segment, SegmentText};

/// Every colour the painter uses. `Copy`, derived once by the host and
/// handed in per paint.
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
}

/// The separator painted BEFORE segment `i`: none before the year, `-`
/// inside the date, a space between date and time, `:` inside the time.
/// `i` is the PAINTED index, and it equals `Segment::index()` only
/// because every `Precision` shows a PREFIX of the six segments (year
/// through some cutoff, never a gap) — the same assumption `paint`'s
/// click callback makes calling `Segment::at(i)`. A non-prefix precision
/// (a segment shown alone, say, or two shown with a gap between) would
/// need `SegmentText` to carry its own `Segment` rather than relying on
/// its position in the slice.
fn separator_before(i: usize) -> Option<&'static str> {
    match i {
        0 => None,
        1 | 2 => Some("-"),
        3 => Some(" "),
        _ => Some(":"),
    }
}

/// Paint `segments` (already in painted order, as
/// [`super::DateTimeField::segments`] answers them) with `paint`'s
/// colours, `suffix` after a gap when given, and a left mouse-down on
/// segment `i` calling `on_segment(Segment::at(i))` and stopping
/// propagation — the host's own container mouse-down (a click "elsewhere"
/// that cancels an editor, say) must not also fire for a click aimed
/// into a segment. Each segment carries the selector `"{selector}-{i}"`
/// and the suffix `"{selector}-suffix"`, so a window test can find them.
/// The font face is the caller's: set `font_family` on the container.
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
                .px_0p5()
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
            // `debug_bounds` takes `&'static str`, not `&str` — a dynamic
            // selector has to leak to satisfy that, and the leak is
            // bounded to these six loop iterations, never per-run or
            // per-window.
            let b = vcx.debug_bounds(Box::leak(format!("probe-seg-{i}").into_boxed_str()));
            assert!(b.is_some(), "segment {i}");
            bounds.push(b.unwrap());
        }
        assert!(vcx.debug_bounds("probe-seg-suffix").is_some());
        // The painted order holds all the way across — a scrambled
        // separator between two segments (the space before the hour, the
        // colons inside the time) would still leave segment 0 left of
        // segment 5, so the sweep checks every consecutive pair, not just
        // the two ends.
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
