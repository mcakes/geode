//! Cell choice typeahead, the entry bar's completion list, and the pick type
//! behind the tile's action menu. Popup geometry and layering, and the menu
//! itself, are `geode_tile`'s; what these lists hold is the pricer's.

use crate::core::complete::Completion;
use crate::tile::PricerTile;
use geode_shell::choice::ChoiceList;
use geode_shell::shell::scale;
use geode_tile::menu::MenuPick;
use geode_tile::popover::{self, ROW_HEIGHT, ROW_INSET};
use gpui::prelude::*;
use gpui::{Anchor, App, Entity, MouseButton, SharedString, div};
use gpui_component::{ActiveTheme as _, h_flex};

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
    let mut list = popover::surface(cx)
        .debug_selector(|| "pricer-choice".into())
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_editor(window, cx))
        });
    if p.rows.is_empty() {
        list = list.child(popover::empty_row(theme, "no option matches"));
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
    popover::anchor_popup(list, Anchor::TopLeft)
}

/// The entry bar's list when the underlying slot has nothing to offer
/// because nothing is configured, not because nothing matches.
pub(crate) const NO_UNDERLYINGS: &str = "no underlyings configured ([pricing] underlyings)";

/// The entry bar's suggestions for the slot at the caret, hung from the
/// bar's bottom-left over the table: at most `complete::MAX_ROWS` rows,
/// the window following the lit row as Tab cycles. Labels and details
/// were prepared by `Completion::refresh`. A row press writes it, exactly
/// as Tab would, and keeps focus in the field (`stop_propagation`, and
/// `occlude` so the table under the list gets no press). `None` when the
/// slot offers nothing.
pub(crate) fn render_entry_list(
    c: &Completion,
    tile: &Entity<PricerTile>,
    cx: &App,
) -> Option<impl IntoElement + use<>> {
    if !c.no_underlyings() && c.candidate_count() == 0 {
        return None;
    }
    let theme = cx.theme();
    let mut list = popover::surface(cx)
        .debug_selector(|| "pricer-entry-list".into())
        .occlude();
    if c.no_underlyings() {
        list = list.child(
            popover::empty_row(theme, NO_UNDERLYINGS).debug_selector(|| "pricer-entry-none".into()),
        );
    }
    let lit = c.highlighted();
    for (i, s) in c.painted() {
        let (fg, muted) = if i == lit {
            (theme.accent_foreground, theme.accent_foreground)
        } else {
            (theme.popover_foreground, theme.muted_foreground)
        };
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .gap_2()
                .rounded(theme.radius)
                .items_center()
                .text_color(fg)
                .when(i == lit, |d| d.bg(theme.accent))
                .debug_selector(move || format!("pricer-entry-row-{i}"))
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.entry_pick(i, window, cx))
                    }
                })
                .child(s.label.clone())
                .when(!s.detail.is_empty(), |d| {
                    d.child(div().text_xs().text_color(muted).child(s.detail.clone()))
                }),
        );
    }
    Some(popover::anchor_popup(list, Anchor::TopLeft))
}

/// What a pricer menu row does when picked: dispatch one of the tile's
/// actions, or show the sheet through a view.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PricerPick {
    Action(&'static str),
    View(SharedString),
}

impl MenuPick for PricerPick {
    fn element_name(&self) -> SharedString {
        match self {
            PricerPick::Action(id) => SharedString::new_static(id),
            PricerPick::View(name) => format!("view:{name}").into(),
        }
    }
}
