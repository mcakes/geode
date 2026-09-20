//! The transient stack-member list (tile-stacks spec §5.2): shell-owned,
//! painted in the palette's mould under the focused tile's header, no
//! `Input`, so no focus dance. The pure state is [`StackList`]; the two
//! motion rules are [`step`] and [`jump`]; [`render`] paints from
//! prepared rows only.

use gpui::prelude::*;
use gpui::{App, MouseButton, Pixels, SharedString, Window, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use super::listrow::row_paint;
use super::scale;
use crate::fonts;
use crate::tiling::{Rect, TileId};

/// Row height on the design scale (PopupMenu's geometry).
pub const ROW_HEIGHT: f32 = 26.0;
/// Where the panel hangs below the tile's top edge: the module header
/// strips share a 22 px height, plus the ring.
pub const TOP_INSET: f32 = 24.0;
pub const MAX_WIDTH: f32 = 320.0;
pub const MIN_WIDTH: f32 = 160.0;

/// One-based row gutter digits, `1`–`9` — a stack has at most nine
/// members (`ctrl+0..9` slot numbering's own bound), so a static lookup
/// spares nine per-frame `String` allocations `(i + 1).to_string()` would
/// cost while the list is open (fix round 1, Minor 3 — PHILOSOPHY.md:
/// "per-frame heap churn is a defect").
const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackList {
    /// The tile whose stack this lists (the focused tile while open).
    pub tile: TileId,
    /// Every member in stack order.
    pub members: Vec<TileId>,
    /// The row `enter` activates; opens on the active member.
    pub highlighted: usize,
}

/// A prepared row: title from `TileContent::title`, kind dimmed.
pub struct Row {
    pub title: SharedString,
    pub kind: &'static str,
}

/// `j`/`k`/arrows: a bare ±1 wraps (the one motion rule, spec §20).
pub fn step(list: &mut StackList, delta: i64) {
    let len = list.members.len() as i64;
    if len == 0 {
        return;
    }
    list.highlighted = (list.highlighted as i64 + delta).rem_euclid(len) as usize;
}

/// A digit `1`–`9`: the member at that one-based index, if any.
pub fn jump(list: &StackList, digit: u32) -> Option<TileId> {
    list.members.get(digit.checked_sub(1)? as usize).copied()
}

pub fn render(
    list: &StackList,
    rows: &[Row],
    tile_rect: Rect,
    rem_size: Pixels,
    on_row_click: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let paint = row_paint(theme);
    let row_height = scale::design_px(ROW_HEIGHT, rem_size);
    let width = scale::design_px(MAX_WIDTH, rem_size).min(
        (tile_rect.w - scale::design_px(8.0, rem_size)).max(scale::design_px(MIN_WIDTH, rem_size)),
    );
    // The 2 px left offset stays a raw window pixel (a hairline inset off
    // the tile's own border, the same as the command-line strip's own
    // `tile.x + 1.0` — fix round 1, Minor 6).
    let left = tile_rect.x + 2.0;
    let top = tile_rect.y + scale::design_px(TOP_INSET, rem_size);

    let mut panel = v_flex()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .p_1()
        .gap_y_0p5()
        .text_sm()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .shadow(super::dialog::overlay_panel_shadow())
        .debug_selector(|| "stack-list".to_string())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());

    for (i, row) in rows.iter().enumerate() {
        let is_highlighted = i == list.highlighted;
        let on_click = on_row_click.clone();
        let mut el = h_flex()
            .id(("stack-list-row", i))
            .w_full()
            .h(px(row_height))
            .items_center()
            .gap_3()
            .px_2()
            .rounded(theme.radius)
            .debug_selector(move || format!("stack-list-row-{i}"))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                on_click(i, window, cx);
            });
        if is_highlighted {
            el = el.bg(paint.active).text_color(paint.text);
        } else {
            el = el.hover(|s| s.bg(paint.hover));
        }
        let digit = DIGITS.get(i).copied().unwrap_or("");
        panel = panel.child(
            el.child(
                div()
                    .font_family(fonts::MONO)
                    .text_color(theme.muted_foreground)
                    .w(px(row_height / 2.0))
                    .child(SharedString::new_static(digit)),
            )
            .child(div().flex_1().child(row.title.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(row.kind),
            ),
        );
    }
    panel
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(members: Vec<u64>, highlighted: usize) -> StackList {
        StackList {
            tile: TileId(members[0]),
            members: members.into_iter().map(TileId).collect(),
            highlighted,
        }
    }

    #[test]
    fn step_wraps_both_ways() {
        let mut l = list(vec![1, 2, 3], 0);
        step(&mut l, 1);
        assert_eq!(l.highlighted, 1);
        step(&mut l, 1);
        assert_eq!(l.highlighted, 2);
        step(&mut l, 1);
        assert_eq!(l.highlighted, 0, "wraps forward");
        step(&mut l, -1);
        assert_eq!(l.highlighted, 2, "wraps backward");
    }

    #[test]
    fn step_on_an_empty_list_does_nothing() {
        let mut l = StackList {
            tile: TileId(1),
            members: Vec::new(),
            highlighted: 0,
        };
        step(&mut l, 1);
        assert_eq!(l.highlighted, 0);
    }

    #[test]
    fn jump_is_one_based_and_refuses_out_of_range() {
        let l = list(vec![10, 20, 30], 0);
        assert_eq!(jump(&l, 1), Some(TileId(10)));
        assert_eq!(jump(&l, 3), Some(TileId(30)));
        assert_eq!(jump(&l, 4), None);
        assert_eq!(jump(&l, 0), None, "0 is not one-based");
    }
}
