//! Geometry and layering for a tile's anchored popups: the popover surface,
//! the deferred anchor that lifts a popup above the tile's clip and keeps
//! it on screen, and the common row frame. What a popup lists is the
//! module's content; this door owns where and how it floats.
//!
//! The shell owns the popover (the menu is built on it); geode-tile
//! re-exports it as `geode_tile::popover`.

use crate::shell::{kbd, scale};
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Deferred, Div, ElementId, Hsla, MouseButton, Stateful,
    Window, anchored, deferred, div, px,
};
use gpui_component::{Theme, ThemeStyled as _, h_flex, v_flex};

/// Popup-row height in design pixels, scaled with the shell's rem size.
pub const ROW_HEIGHT: f32 = 26.0;
/// Horizontal row inset in design pixels.
pub const ROW_INSET: f32 = 8.0;
/// The popup's minimum width at the design rem: room for a title and a
/// trailing key lane.
pub const MIN_WIDTH: f32 = 240.0;
/// Least distance, in window pixels, a snapped popup keeps from the window
/// edge. Not rem-scaled: it is clearance from the window frame, not content.
pub const SNAP_MARGIN: f32 = 8.0;

/// The popover treatment every tile popup shares: scaled minimum width,
/// inset and row spacing.
pub fn surface(cx: &App) -> Div {
    v_flex()
        .min_w(scale::design(MIN_WIDTH))
        .p_1()
        .gap_y_0p5()
        .text_sm()
        .popover_style(cx)
}

/// Hang `content` by its own `corner` from the zero-size point the caller
/// paints this at. `deferred` paints it above later siblings and outside the
/// tile's and table's clips (priority 1, over the tile's own deferred
/// elements); the snap keeps a popup opened near an edge on screen. A popup
/// painted without the snap runs off the window when opened from a bottom
/// row or the right edge.
pub fn anchor_popup(content: impl IntoElement, corner: Anchor) -> Deferred {
    deferred(
        anchored()
            .anchor(corner)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(SNAP_MARGIN))
            .child(content),
    )
    .with_priority(1)
}

/// Shared list-row geometry, selection colors, and left-press handling.
/// Unselected rows show the supplied hover fill without moving a cursor.
/// The press is consumed before the callback runs, so the surface beneath
/// cannot also act on it.
///
/// The caller supplies a per-popup row id and derives `hover` once per
/// popup paint, avoiding a contrast calculation per row.
pub fn row_shell(
    theme: &Theme,
    hover: Hsla,
    id: ElementId,
    highlighted: bool,
    selector: impl FnOnce() -> String,
    on_down: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h(scale::design(ROW_HEIGHT))
        .px(scale::design(ROW_INSET))
        .gap_2()
        .rounded(theme.radius)
        .items_center()
        .when(highlighted, |d| {
            d.bg(theme.accent).text_color(theme.accent_foreground)
        })
        .when(!highlighted, |d| {
            d.text_color(theme.popover_foreground)
                .hover(move |s| s.bg(hover))
        })
        .debug_selector(selector)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            on_down(window, cx);
        })
}

/// The "nothing here" row: muted, the same height and inset as a row, with
/// backtick-quoted runs painted as keys.
pub fn empty_row(theme: &Theme, text: &'static str) -> Div {
    div()
        .h(scale::design(ROW_HEIGHT))
        .px(scale::design(ROW_INSET))
        .flex()
        .items_center()
        .text_color(theme.muted_foreground)
        .child(kbd::marked(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Pixels, Point, Render, Size, TestAppContext, point};

    /// A popup hung from a point the test chooses from the viewport size.
    struct Probe {
        corner: Anchor,
        at: fn(Size<Pixels>) -> Point<Pixels>,
    }

    impl Render for Probe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let at = (self.at)(window.viewport_size());
            div().size_full().child(
                div().absolute().left(at.x).top(at.y).child(anchor_popup(
                    surface(cx)
                        .debug_selector(|| "probe-popup".into())
                        .child("row"),
                    self.corner,
                )),
            )
        }
    }

    fn bounds_of(
        corner: Anchor,
        at: fn(Size<Pixels>) -> Point<Pixels>,
        cx: &mut TestAppContext,
    ) -> (gpui::Bounds<Pixels>, Size<Pixels>) {
        cx.update(gpui_component::init);
        let (_view, vcx) = cx.add_window_view(|_, _| Probe { corner, at });
        vcx.run_until_parked();
        let size = vcx.update(|window, _| window.viewport_size());
        let bounds = vcx.debug_bounds("probe-popup").expect("the popup paints");
        (bounds, size)
    }

    #[gpui::test]
    fn a_popup_hangs_from_its_anchor_corner(cx: &mut TestAppContext) {
        let (b, _) = bounds_of(Anchor::TopRight, |_| point(px(400.), px(40.)), cx);
        assert!(
            (b.top_right().x - px(400.)).abs() <= px(0.5),
            "the popup's right edge is on the anchor: {b:?}"
        );
        assert!((b.top_right().y - px(40.)).abs() <= px(0.5), "{b:?}");
    }

    #[gpui::test]
    fn a_popup_near_the_edge_snaps_inside_the_margin(cx: &mut TestAppContext) {
        let (b, size) = bounds_of(
            Anchor::TopLeft,
            |size| point(size.width - px(20.), px(40.)),
            cx,
        );
        let limit = size.width - px(SNAP_MARGIN);
        assert!(
            (b.top_right().x - limit).abs() <= px(0.5),
            "snapped to the margin, not the window edge: {b:?} in {size:?}"
        );
    }
}
