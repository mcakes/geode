//! Cell choice typeahead and the tile's action menu. Prepared rows are refreshed when
//! list state changes; rendering consumes those rows.

use crate::core::complete::Completion;
use crate::paint::Paints;
use crate::tile::PricerTile;
use geode_shell::choice::ChoiceList;
use geode_shell::shell::{kbd, scale};
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Div, Entity, Hsla, MouseButton, SharedString, anchored,
    deferred, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, ThemeStyled as _, h_flex, v_flex};
use std::collections::BTreeSet;

const ROW_HEIGHT: f32 = 26.0;
const ROW_INSET: f32 = 8.0;
/// Minimum popup width, including space for action titles and trailing key hints.
const MIN_WIDTH: f32 = 240.0;
/// The leading tick slot on a `View` row: the same width ticked or not,
/// so the view names share one leading edge.
const TICK_SLOT: f32 = 14.0;
/// Minimum distance from the window edge when a popup is snapped on screen, in pixels.
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
    let mut list = popover_surface(cx)
        .debug_selector(|| "pricer-entry-list".into())
        .occlude();
    if c.no_underlyings() {
        list = list.child(
            div()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .flex()
                .items_center()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "pricer-entry-none".into())
                .child(NO_UNDERLYINGS),
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
    Some(
        deferred(
            anchored()
                .anchor(Anchor::TopLeft)
                .position_mode(AnchoredPositionMode::Local)
                .snap_to_window_with_margin(px(SNAP_MARGIN))
                .child(list),
        )
        .with_priority(1),
    )
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
/// field over a window of ranked names. The current sheet carries the
/// tick; one another tile holds says [`OPEN_ELSEWHERE`]. A row press
/// picks it (`stop_propagation`, and `occlude` so nothing beneath hears
/// the press), hover moves the highlight, and a press anywhere else
/// closes the picker.
pub(crate) fn render_sheet_picker(
    p: &SheetPicker,
    paints: &Paints,
    tile: &Entity<PricerTile>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
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
        list = list.child(
            div()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .flex()
                .items_center()
                .text_color(paints.menu_muted)
                .child(crate::tile::NO_SHEET_MATCHES),
        );
    }
    let lit = p.list.highlighted();
    for (i, ranked) in p.list.painted().iter().enumerate() {
        let row = &p.rows[ranked.row];
        let paint = menu_row_paint(i == lit, true, paints, theme.accent);
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .rounded(theme.radius)
                .items_center()
                .justify_between()
                .gap_4()
                .when_some(paint.fill, |d, fill| d.bg(fill))
                .text_color(paint.text)
                .debug_selector(move || format!("pricer-sheet-row-{i}"))
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.sheet_picker_pick(i, window, cx))
                    }
                })
                .on_mouse_move({
                    let tile = tile.clone();
                    move |_, _, cx| tile.update(cx, |t, cx| t.sheet_picker_hover(i, cx))
                })
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            div()
                                .w(scale::design(TICK_SLOT))
                                .flex_shrink_0()
                                .child(if row.current { "\u{2713}" } else { "" }),
                        )
                        .child(row.name.clone()),
                )
                .child(
                    div()
                        .text_color(paint.lane)
                        .debug_selector(move || format!("pricer-sheet-lane-{i}"))
                        .child(if row.open_elsewhere {
                            OPEN_ELSEWHERE
                        } else {
                            ""
                        }),
                ),
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
                    // An enabled row's key (or `:` verb) as menu chips; a
                    // disabled row's reason as text.
                    .child(if enabled {
                        kbd::menu_spec(lane, paint.lane)
                    } else {
                        lane.into_any_element()
                    }),
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
