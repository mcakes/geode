//! Cell choice typeahead, the entry bar's completion list, the sheet
//! picker, and the pick type behind the tile's action menu. Popup geometry and layering, and the menu
//! itself, are `geode_tile`'s; what these lists hold is the pricer's.

use crate::core::complete::Completion;
use crate::tile::PricerTile;
use geode_shell::choice::ChoiceList;
use geode_shell::shell::scale;
use geode_tile::menu::{MenuPaint, MenuPick};
use geode_tile::popover::{self, ROW_HEIGHT, ROW_INSET};
use gpui::prelude::*;
use gpui::{Anchor, App, ElementId, Entity, MouseButton, SharedString, div};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, h_flex};
use std::collections::BTreeSet;

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

/// The sheet picker's key context: `lib::init` reclaims `tab`/`shift-tab`
/// in it from gpui-component's focus cycling, so `tab` completes.
pub const SHEET_PICKER_CONTEXT: &str = "PricerSheetPicker";

/// What a sheet picker's pick does: open the sheet (`:e`'s route) or arm
/// its removal (`:rm`'s route).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerPurpose {
    Open,
    Remove,
}

impl PickerPurpose {
    /// The filter field's placeholder: what a pick will do.
    pub(crate) fn placeholder(self) -> &'static str {
        match self {
            PickerPurpose::Open => "open sheet",
            PickerPurpose::Remove => "remove sheet",
        }
    }
}

/// One sheet picker row: a name and its marks, prepared when the picker
/// opens. The marks describe; they refuse nothing. A pick goes through
/// the command's own route, which decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SheetRow {
    pub name: SharedString,
    /// The sheet this tile shows: the leading tick.
    pub current: bool,
    /// Held by another tile: [`OPEN_ELSEWHERE`] in the trailing lane.
    pub open_elsewhere: bool,
}

/// The trailing-lane mark of a sheet another tile holds.
pub(crate) const OPEN_ELSEWHERE: &str = "open";

/// The picker's rows, sorted by name: every known or pending-save name
/// (`known`, retiring names already left out), every name a tile holds,
/// and this tile's own. An unsaved sheet is listed while a tile holds
/// it, so the trader sees why its name is taken.
pub(crate) fn sheet_rows(
    known: &[String],
    open: &BTreeSet<String>,
    current: &str,
) -> Vec<SheetRow> {
    let mut names: BTreeSet<&str> = known.iter().map(String::as_str).collect();
    names.extend(open.iter().map(String::as_str));
    names.insert(current);
    names
        .into_iter()
        .map(|n| SheetRow {
            name: SharedString::from(n.to_string()),
            current: n == current,
            open_elsewhere: n != current && open.contains(n),
        })
        .collect()
}

/// The open sheet picker: its filter field, which holds the keyboard
/// (the tile reads `mode == insert` while it is open), the ranked names,
/// and the rows' marks, parallel to the list's options.
pub(crate) struct SheetPicker {
    pub purpose: PickerPurpose,
    pub input: Entity<InputState>,
    pub list: ChoiceList,
    pub rows: Vec<SheetRow>,
}

impl SheetPicker {
    /// Highlight the current sheet, so an `enter` straight away keeps it
    /// rather than switching to whichever name sorts first.
    pub(crate) fn new(
        purpose: PickerPurpose,
        input: Entity<InputState>,
        rows: Vec<SheetRow>,
    ) -> Self {
        let mut list = ChoiceList::new(
            rows.iter().map(|r| r.name.to_string()).collect(),
            geode_shell::choice::DEFAULT_CAP,
        );
        let current = rows.iter().find(|r| r.current).map(|r| r.name.to_string());
        list.place(current.as_deref());
        Self {
            purpose,
            input,
            list,
            rows,
        }
    }

    /// The highlighted row's name; `None` with nothing matching.
    pub(crate) fn picked(&self) -> Option<String> {
        self.list.highlighted_text().map(str::to_string)
    }
}

/// Paint the sheet picker hung from the header's sheet name: its filter
/// field over a window of ranked names, in the shared popover surface and
/// row frame. The current sheet carries the tick; one another tile holds
/// says [`OPEN_ELSEWHERE`]. A row press picks it (the row frame consumes
/// the press, and `occlude` keeps what is beneath from hearing it), hover
/// moves the highlight, and a press anywhere else closes the picker.
pub(crate) fn render_sheet_picker(
    p: &SheetPicker,
    tile: &Entity<PricerTile>,
    cx: &App,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let paint = MenuPaint::derive(theme);
    let mut list = popover::surface(cx)
        .debug_selector(|| "pricer-sheet-picker".into())
        .key_context(SHEET_PICKER_CONTEXT)
        .occlude()
        .on_key_down({
            let tile = tile.clone();
            move |event: &gpui::KeyDownEvent, window, cx| {
                if tile.update(cx, |t, cx| t.sheet_picker_key(event, window, cx)) {
                    cx.stop_propagation();
                }
            }
        })
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_sheet_picker(window, cx))
        })
        .child(
            div()
                .w_full()
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(theme.border)
                .debug_selector(|| "pricer-sheet-picker-field".into())
                .child(Input::new(&p.input).appearance(false).w_full()),
        );
    if p.list.painted_len() == 0 {
        list = list.child(popover::empty_row(theme, crate::tile::NO_SHEET_MATCHES));
    }
    let lit = p.list.highlighted();
    for (i, ranked) in p.list.painted().iter().enumerate() {
        let row = &p.rows[ranked.row];
        let highlighted = i == lit;
        let lane = if highlighted {
            paint.active_text
        } else {
            paint.muted
        };
        list = list.child(
            popover::row_shell(
                theme,
                paint.rest_pointer.hover,
                ElementId::NamedInteger(SharedString::new_static("pricer-sheet-row"), i as u64),
                highlighted,
                move || format!("pricer-sheet-row-{i}"),
                {
                    let tile = tile.clone();
                    move |window, cx| tile.update(cx, |t, cx| t.sheet_picker_pick(i, window, cx))
                },
            )
            .justify_between()
            .on_mouse_move({
                let tile = tile.clone();
                move |_, _, cx| tile.update(cx, |t, cx| t.sheet_picker_hover(i, cx))
            })
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .w(scale::design(geode_tile::menu::TICK_SLOT))
                            .flex_shrink_0()
                            .child(if row.current { "\u{2713}" } else { "" }),
                    )
                    .child(row.name.clone()),
            )
            .child(
                div()
                    .text_color(lane)
                    .debug_selector(move || format!("pricer-sheet-lane-{i}"))
                    .child(if row.open_elsewhere {
                        OPEN_ELSEWHERE
                    } else {
                        ""
                    }),
            ),
        );
    }
    popover::anchor_popup(list, Anchor::TopLeft)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_rows_list_known_and_held_names_once_sorted_with_their_marks() {
        let known = vec!["zeta".to_string(), "alpha".to_string(), "book".to_string()];
        let open: BTreeSet<String> = ["book", "held", "zeta"].map(String::from).into();
        let rows = sheet_rows(&known, &open, "book");
        let spelled: Vec<(String, bool, bool)> = rows
            .iter()
            .map(|r| (r.name.to_string(), r.current, r.open_elsewhere))
            .collect();
        assert_eq!(
            spelled,
            vec![
                ("alpha".into(), false, false),
                ("book".into(), true, false),
                ("held".into(), false, true),
                ("zeta".into(), false, true),
            ],
            "the current sheet is ticked, not marked open; an unsaved held name is listed"
        );
        let unsaved = sheet_rows(&[], &BTreeSet::new(), "untitled-1");
        assert_eq!(unsaved.len(), 1, "the current sheet is listed unsaved");
        assert!(unsaved[0].current);
    }
}
