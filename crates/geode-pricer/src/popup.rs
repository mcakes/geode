//! The tile's popups (spec §8.4–§8.5): the choice typeahead under an
//! editing cell, and the action menu. Rows are prepared when
//! the list changes, never formatted in render.

use crate::paint::Paints;
use crate::tile::PricerTile;
use geode_shell::choice::ChoiceList;
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Div, Entity, MouseButton, SharedString, anchored, deferred,
    div, px,
};
use gpui_component::{ActiveTheme as _, ThemeStyled as _, h_flex, v_flex};

const ROW_HEIGHT: f32 = 26.0;
const ROW_INSET: f32 = 8.0;
/// The market-data action list's width, so the two menus a trader moves
/// between share one geometry (and the trailing key lane has room).
const MIN_WIDTH: f32 = 240.0;
/// The leading tick slot on a `View` row: the same width ticked or not,
/// so the view names share one leading edge.
const TICK_SLOT: f32 = 14.0;
/// How far a popup keeps from the window's edge when it is snapped back
/// on screen (the market-data popups' margin).
const SNAP_MARGIN: f32 = 8.0;

pub(crate) fn popover_surface(cx: &App) -> Div {
    v_flex()
        .min_w(scale::design(MIN_WIDTH))
        .p_1()
        .gap_y_0p5()
        .text_sm()
        .popover_style(cx)
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChoicePaint {
    pub rows: Vec<SharedString>,
    /// Window-relative, as `ChoiceList::highlighted` answers it.
    pub highlighted: usize,
}

pub(crate) fn choice_paint(list: &ChoiceList) -> ChoicePaint {
    ChoicePaint {
        rows: list
            .painted()
            .iter()
            .map(|r| list.options()[r.row].clone().into())
            .collect(),
        highlighted: list.highlighted(),
    }
}

/// The ranked options under the editing cell, anchored by their top-left
/// corner at the point the delegate paints them from (the cell's
/// bottom-left). `deferred` escapes the table's clip; the snap keeps a
/// bottom-row list on screen. A row click picks it (`stop_propagation`: a
/// click that means "pick" must not also land on the grid, which would
/// cancel the editor); a press anywhere else closes the editor.
pub(crate) fn render_choice(
    p: &ChoicePaint,
    tile: &Entity<PricerTile>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(|| "pricer-choice".into())
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_editor(window, cx))
        });
    if p.rows.is_empty() {
        list = list.child(
            div()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .flex()
                .items_center()
                .text_color(theme.muted_foreground)
                .child("no option matches"),
        );
    }
    for (i, text) in p.rows.iter().enumerate() {
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .rounded(theme.radius)
                .items_center()
                .when(i == p.highlighted, |d| {
                    d.bg(theme.accent).text_color(theme.accent_foreground)
                })
                .when(i != p.highlighted, |d| {
                    d.text_color(theme.popover_foreground)
                })
                .debug_selector(move || format!("pricer-choice-row-{i}"))
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.choice_pick(i, window, cx))
                    }
                })
                // The highlight follows the pointer (market-data's
                // `choice_hover`), so a hovered row and the highlighted
                // one are never two different rows.
                .on_mouse_move({
                    let tile = tile.clone();
                    move |_, _, cx| tile.update(cx, |t, cx| t.choice_hover(i, cx))
                })
                .child(text.clone()),
        );
    }
    deferred(
        anchored()
            .anchor(Anchor::TopLeft)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(SNAP_MARGIN))
            .child(list),
    )
    .with_priority(1)
}

/// One row of the action menu (spec §8.5's `.`; planning decision 22).
/// Only `Action` and `View` rows take the highlight; a `Separator` or
/// `Section` is structure the highlight steps over.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MenuItem {
    Action {
        id: &'static str,
        title: &'static str,
        /// The default key, painted muted in the trailing lane (the
        /// market-data list's convention); a disabled row paints its
        /// reason there instead.
        hint: &'static str,
        enabled: Result<(), &'static str>,
    },
    /// A choice row under the `View` section: `current` puts the tick in
    /// its leading slot.
    View {
        name: SharedString,
        current: bool,
    },
    Separator,
    Section(&'static str),
}

impl MenuItem {
    pub(crate) fn pickable(&self) -> bool {
        matches!(self, MenuItem::Action { .. } | MenuItem::View { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Menu {
    pub items: Vec<MenuItem>,
    pub highlighted: usize,
}

/// Move `delta` pickable rows from `from`, clamped at either end (never
/// landing on a separator or section header).
pub(crate) fn step(items: &[MenuItem], from: usize, delta: isize) -> usize {
    let pickable: Vec<usize> = items
        .iter()
        .enumerate()
        .filter_map(|(i, r)| r.pickable().then_some(i))
        .collect();
    let Some(last) = pickable.len().checked_sub(1) else {
        return from;
    };
    let pos = pickable.iter().position(|&i| i >= from).unwrap_or(last);
    pickable[(pos as isize + delta).clamp(0, last as isize) as usize]
}

/// `at`, or the nearest pickable row before it (after it, if none
/// precedes it) — where a highlight lands when the list changes under it.
pub(crate) fn snap(items: &[MenuItem], at: usize) -> usize {
    let at = at.min(items.len().saturating_sub(1));
    (0..=at)
        .rev()
        .find(|&i| items.get(i).is_some_and(MenuItem::pickable))
        .or_else(|| (at..items.len()).find(|&i| items[i].pickable()))
        .unwrap_or(0)
}

/// The `.` action menu (planning decision 22), anchored under the
/// header's right edge by the caller. The pricer's own row door
/// (`render_choice`'s shape) rather than the market-data popup, which
/// this crate may not import (CLAUDE.md); `deferred`/`anchored` escapes
/// the table's clip and paints above it, the same as `render_choice`
/// (review finding: a bare surface here was occluded by the `DataTable`,
/// a later sibling).
///
/// The menu family's highlight (`shell::listrow`'s doc: menus wear
/// `accent`, lists the list pair), moved by the pointer as by `j`/`k`,
/// so there is no separate hover fill to disagree with it. A disabled
/// row keeps the highlight fill when the keyboard lands on it (the trader
/// must see where `enter` would answer) but paints muted text, and its
/// trailing lane names why it is disabled. Every text colour here is
/// floored against the ground it paints on (`Paints`'s menu colours).
pub(crate) fn render_menu(
    m: &Menu,
    paints: &Paints,
    tile: &Entity<PricerTile>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(|| "pricer-menu".into())
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, _window, cx| tile.update(cx, |t, cx| t.close_menu(cx))
        });
    for (i, item) in m.items.iter().enumerate() {
        let (title, lane, enabled, tick): (SharedString, &'static str, bool, Option<bool>) =
            match item {
                MenuItem::Separator => {
                    // `PopupMenu`'s own separator (the market-data list's).
                    list = list.child(
                        div()
                            .my_0p5()
                            .mx_neg_1()
                            .border_b(px(2.))
                            .border_color(theme.border),
                    );
                    continue;
                }
                MenuItem::Section(s) => {
                    list = list.child(
                        div()
                            .px(scale::design(ROW_INSET))
                            .pt_1()
                            .text_xs()
                            .text_color(paints.menu_muted)
                            .debug_selector(move || format!("pricer-menu-section-{i}"))
                            .child(*s),
                    );
                    continue;
                }
                MenuItem::Action {
                    title,
                    hint,
                    enabled,
                    ..
                } => match enabled {
                    Ok(()) => ((*title).into(), *hint, true, None),
                    Err(why) => ((*title).into(), *why, false, None),
                },
                MenuItem::View { name, current } => (name.clone(), "", true, Some(*current)),
            };
        let highlighted = i == m.highlighted;
        let (text, lane_text) = match (highlighted, enabled) {
            (true, true) => (paints.menu_active_text, paints.menu_active_muted),
            (true, false) => (paints.menu_active_muted, paints.menu_active_muted),
            (false, true) => (paints.menu_text, paints.menu_muted),
            (false, false) => (paints.menu_muted, paints.menu_muted),
        };
        let row = h_flex()
            .h(scale::design(ROW_HEIGHT))
            .px(scale::design(ROW_INSET))
            .rounded(theme.radius)
            .items_center()
            .justify_between()
            .gap_4()
            .when(highlighted, |d| d.bg(theme.accent))
            .text_color(text)
            .debug_selector(move || format!("pricer-menu-row-{i}"))
            .on_mouse_down(MouseButton::Left, {
                let tile = tile.clone();
                move |_, window, cx| {
                    cx.stop_propagation();
                    tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                }
            })
            // The mouse form of `j`/`k` (market-data's `menu_hover`): the
            // highlight follows the pointer, disabled rows included — a
            // hover is a hover, and `enter` there answers with the reason.
            .on_mouse_move({
                let tile = tile.clone();
                move |_, _, cx| tile.update(cx, |t, cx| t.menu_hover(i, cx))
            })
            .child(
                h_flex()
                    .gap_1()
                    .when_some(tick, |d, on| {
                        d.child(
                            div()
                                .w(scale::design(TICK_SLOT))
                                .flex_shrink_0()
                                .child(if on { "\u{2713}" } else { "" }),
                        )
                    })
                    .child(title),
            )
            .child(
                div()
                    .text_color(lane_text)
                    .debug_selector(move || format!("pricer-menu-lane-{i}"))
                    .child(lane),
            );
        list = list.child(row);
    }
    deferred(
        anchored()
            .anchor(Anchor::TopRight)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(SNAP_MARGIN))
            .child(list),
    )
    .with_priority(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<MenuItem> {
        let action = |id| MenuItem::Action {
            id,
            title: "t",
            hint: "",
            enabled: Ok(()),
        };
        vec![
            action("a"),
            MenuItem::Separator,
            action("b"),
            MenuItem::Separator,
            MenuItem::Section("View"),
            MenuItem::View {
                name: "v".into(),
                current: true,
            },
        ]
    }

    #[test]
    fn the_highlight_steps_over_separators_and_sections_and_clamps() {
        let m = items();
        assert_eq!(step(&m, 0, 1), 2);
        assert_eq!(step(&m, 2, 1), 5, "over a separator and the section");
        assert_eq!(step(&m, 5, 1), 5, "clamped at the end");
        assert_eq!(step(&m, 5, -2), 0);
        assert_eq!(step(&m, 0, -1), 0, "clamped at the start");
        assert_eq!(step(&m, 0, 7), 5);
    }

    #[test]
    fn a_highlight_snaps_back_onto_a_pickable_row() {
        let m = items();
        assert_eq!(snap(&m, 4), 2, "the section header gives way upward");
        assert_eq!(snap(&m, 99), 5, "past the end clamps to the last row");
        assert_eq!(snap(&m, 2), 2);
        let lead = vec![MenuItem::Section("x"), m[0].clone()];
        assert_eq!(snap(&lead, 0), 1, "nothing before it: the next one");
    }
}
