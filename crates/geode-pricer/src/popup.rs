//! Cell choice typeahead and the tile's action menu. Prepared rows are refreshed when
//! list state changes; rendering consumes those rows.

use crate::paint::Paints;
use crate::tile::PricerTile;
use geode_shell::choice::ChoiceList;
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Div, Entity, Hsla, MouseButton, SharedString, anchored,
    deferred, div, px,
};
use gpui_component::{ActiveTheme as _, ThemeStyled as _, h_flex, v_flex};

const ROW_HEIGHT: f32 = 26.0;
const ROW_INSET: f32 = 8.0;
/// Minimum popup width, including space for action titles and trailing key hints.
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
                // Keep pointer and keyboard selection on the same typeahead highlight.
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

/// Prepared action-menu row. Action and View rows accept the highlight; separators and
/// section headings are structural only.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MenuItem {
    Action {
        id: &'static str,
        title: &'static str,
        /// Default key hint in the trailing lane, replaced by the refusal reason when
        /// the action is disabled. User rebinding does not change this hint.
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

    /// Whether keyboard stepping may select this row: enabled actions and view choices
    /// qualify.
    fn lands(&self) -> bool {
        matches!(
            self,
            MenuItem::Action {
                enabled: Ok(()),
                ..
            } | MenuItem::View { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Menu {
    pub items: Vec<MenuItem>,
    pub highlighted: usize,
}

/// Starting at `from`, move `delta` enabled rows, skipping disabled actions, separators,
/// and section headers. Clamp at either end. If the current highlight is disabled, search
/// from that position; with no enabled row in the requested direction, retain it.
pub(crate) fn step(items: &[MenuItem], from: usize, delta: isize) -> usize {
    let mut at = from;
    for _ in 0..delta.unsigned_abs() {
        let next = if delta > 0 {
            (at + 1..items.len()).find(|&i| items[i].lands())
        } else {
            (0..at.min(items.len())).rev().find(|&i| items[i].lands())
        };
        match next {
            Some(i) => at = i,
            None => break,
        }
    }
    at
}

/// Keep at or find the nearest pickable row before it, then after it, when rebuilding
/// the list. Disabled actions remain pickable here; keyboard stepping separately
/// requires enabled rows.
pub(crate) fn snap(items: &[MenuItem], at: usize) -> usize {
    let at = at.min(items.len().saturating_sub(1));
    (0..=at)
        .rev()
        .find(|&i| items.get(i).is_some_and(MenuItem::pickable))
        .or_else(|| (at..items.len()).find(|&i| items[i].pickable()))
        .unwrap_or(0)
}

/// What one menu row paints: its fill, its title's colour and its
/// trailing lane's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MenuRowPaint {
    pub fill: Option<Hsla>,
    pub text: Hsla,
    pub lane: Hsla,
}

/// Only highlighted, enabled rows receive accent fill. Disabled actions can hold the
/// logical highlight and report their reason when picked, but remain muted on the
/// popover background. Keyboard stepping skips disabled rows.
pub(crate) fn menu_row_paint(
    highlighted: bool,
    enabled: bool,
    paints: &Paints,
    accent: Hsla,
) -> MenuRowPaint {
    match (highlighted, enabled) {
        (true, true) => MenuRowPaint {
            fill: Some(accent),
            text: paints.menu_active_text,
            lane: paints.menu_active_muted,
        },
        (false, true) => MenuRowPaint {
            fill: None,
            text: paints.menu_text,
            lane: paints.menu_muted,
        },
        (_, false) => MenuRowPaint {
            fill: None,
            text: paints.menu_muted,
            lane: paints.menu_muted,
        },
    }
}

/// Render the action menu below the header's right edge. Deferred anchored painting
/// escapes table clipping and places the menu above later siblings.
///
/// Pointer movement and keyboard navigation share one highlight. Enabled rows use
/// accent fill; [`menu_row_paint`] keeps disabled rows unfilled. Prepared menu text
/// colours are adjusted against their actual backgrounds.
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
                    // Separate action groups with the standard popup divider.
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
        let paint = menu_row_paint(i == m.highlighted, enabled, paints, theme.accent);
        let row = h_flex()
            .h(scale::design(ROW_HEIGHT))
            .px(scale::design(ROW_INSET))
            .rounded(theme.radius)
            .items_center()
            .justify_between()
            .gap_4()
            .when_some(paint.fill, |d, fill| d.bg(fill))
            .text_color(paint.text)
            .debug_selector(move || format!("pricer-menu-row-{i}"))
            .on_mouse_down(MouseButton::Left, {
                let tile = tile.clone();
                move |_, window, cx| {
                    cx.stop_propagation();
                    tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                }
            })
            // Pointer movement updates the logical highlight, including disabled
            // actions; picking a disabled action reports its reason.
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
                    .text_color(paint.lane)
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

    #[gpui::test]
    fn only_a_highlighted_enabled_row_takes_the_fill(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            let p = Paints::derive(theme);
            let paint = |h, e| menu_row_paint(h, e, &p, theme.accent);
            assert_eq!(paint(true, true).fill, Some(theme.accent));
            assert_eq!(paint(true, true).text, p.menu_active_text);
            assert_eq!(paint(false, true).fill, None);
            assert_eq!(paint(true, false).fill, None);
            assert_eq!(paint(true, false).text, p.menu_muted);
            assert_eq!(paint(false, false), paint(true, false));
        });
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
