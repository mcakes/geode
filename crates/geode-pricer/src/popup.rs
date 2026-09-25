//! The tile's popups (spec §8.4–§8.5): the choice typeahead under an
//! editing cell, and the action menu. Rows are prepared when
//! the list changes, never formatted in render.

use crate::tile::PricerTile;
use geode_shell::choice::ChoiceList;
use geode_shell::shell::listrow;
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Div, Entity, MouseButton, SharedString, anchored, deferred,
    div, px,
};
use gpui_component::{ActiveTheme as _, ThemeStyled as _, h_flex, v_flex};

const ROW_HEIGHT: f32 = 26.0;
const ROW_INSET: f32 = 8.0;
const MIN_WIDTH: f32 = 160.0;
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
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MenuItem {
    Action {
        id: &'static str,
        title: &'static str,
        enabled: Result<(), &'static str>,
    },
    /// `label` is prepared when the menu opens (`view: barrier ✓` on the
    /// current one), never formatted per frame.
    View {
        name: SharedString,
        label: SharedString,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Menu {
    pub items: Vec<MenuItem>,
    pub highlighted: usize,
}

/// The `.` action menu (planning decision 22), anchored under the
/// header's right edge by the caller. The pricer's own row door
/// (`render_choice`'s shape) rather than the market-data popup, which
/// this crate may not import (CLAUDE.md); `deferred`/`anchored` escapes
/// the table's clip and paints above it, the same as `render_choice`
/// (review finding: a bare surface here was occluded by the `DataTable`,
/// a later sibling).
pub(crate) fn render_menu(m: &Menu, tile: &Entity<PricerTile>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    // The list-row tokens through the one door (`shell::listrow`): the
    // highlighted row is the state, the hovered row is the pointer.
    let row_paint = listrow::row_paint(theme);
    let mut list = popover_surface(cx)
        .debug_selector(|| "pricer-menu".into())
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, _window, cx| tile.update(cx, |t, cx| t.close_menu(cx))
        });
    for (i, item) in m.items.iter().enumerate() {
        let (label, enabled): (SharedString, bool) = match item {
            MenuItem::Action { title, enabled, .. } => ((*title).into(), enabled.is_ok()),
            MenuItem::View { label, .. } => (label.clone(), true),
        };
        let highlighted = i == m.highlighted;
        let mut row = h_flex()
            .h(scale::design(ROW_HEIGHT))
            .px(scale::design(ROW_INSET))
            .rounded(theme.radius)
            .items_center();
        row = if highlighted {
            row.bg(row_paint.active)
        } else {
            row.hover(|s| s.bg(row_paint.hover))
        };
        // A disabled row keeps muted text even when highlighted — the
        // door's `text` is only for an enabled row (review finding: a
        // highlighted disabled row must still read as disabled).
        row = row.text_color(if !enabled {
            theme.muted_foreground
        } else if highlighted {
            row_paint.text
        } else {
            theme.popover_foreground
        });
        list = list.child(
            row.debug_selector(move || format!("pricer-menu-row-{i}"))
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                    }
                })
                .child(label),
        );
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
