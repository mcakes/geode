//! Tile-owned menu, underlying picker, and cell-choice popups. Deferred
//! anchoring lets them escape tile/table clips, and occlusion prevents pointer
//! hits reaching the grid beneath. Menu and underlying picker anchor at the
//! header; Choice anchors at its target cell. Picker and Choice own focused
//! inputs and use insert routing. The tile owns opening, commits, and closing;
//! these painters consume row presses and forward picks and hover selection.

use crate::core::menu::MenuRow;
use crate::tile::MarketDataTile;
use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Div, Entity, IntoElement, MouseButton, SharedString,
    anchored, deferred, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, ThemeStyled as _, h_flex, v_flex};
use std::rc::Rc;

/// Popup-row height in design pixels, scaled with the shell's rem size.
const ROW_HEIGHT: f32 = 26.0;
/// Horizontal row inset in design pixels.
const ROW_INSET: f32 = 8.0;
/// The popup's minimum width at the design rem.
const MIN_WIDTH: f32 = 240.0;

/// Shared popover treatment, minimum width, and content spacing for all variants.
fn popover_surface(cx: &App) -> Div {
    v_flex()
        .min_w(scale::design(MIN_WIDTH))
        .p_1()
        .gap_y_0p5()
        .text_sm()
        .popover_style(cx)
}
use std::collections::BTreeMap;

/// The tile's mutually exclusive popup state. Menu uses menu-mode routing;
/// Picker and Choice own focused inputs and use insert-mode routing.
pub(crate) enum Popup {
    Menu(MenuState),
    Picker(PickerState),
    Choice(ChoicePopup),
}

/// Choice-cell typeahead anchored below the edited cell. Declared options use
/// static labels; ChoiceList owns ranking, selection, and the moving window.
///
/// The target stores both coordinates and row/column labels captured at open.
/// Commit checks these identities before writing so a changed document cannot
/// redirect the edit. prepare() snapshots painted rows into an Rc shared with
/// the table delegate after each query, selection, or hover change.
pub(crate) struct ChoicePopup {
    pub input: Entity<InputState>,
    pub list: ChoiceList,
    cell: (usize, usize),
    labels: (SharedString, SharedString),
    /// One static `SharedString` per DECLARED option — what
    /// [`Self::prepare`] indexes by `Ranked::row`.
    option_labels: Vec<SharedString>,
    paint: Rc<ChoicePaint>,
}

/// The rows [`render_choice`] paints — the painted WINDOW of the ranked
/// list as prepared labels, and which of them is lit — plus the field,
/// so the delegate can paint the whole popup from this one `Rc`.
pub(crate) struct ChoicePaint {
    pub input: Entity<InputState>,
    pub rows: Vec<SharedString>,
    pub highlighted: usize,
}

impl ChoicePopup {
    /// Every option ranked in declared order under an empty query, the
    /// highlight on `current` (row 0 when the cell is NULL or holds a
    /// value the vocabulary no longer lists — a hole is still editable).
    pub(crate) fn new(
        input: Entity<InputState>,
        options: &'static [&'static str],
        current: &str,
        cell: (usize, usize),
        labels: (SharedString, SharedString),
    ) -> Self {
        let mut list =
            ChoiceList::new(options.iter().map(|s| s.to_string()).collect(), DEFAULT_CAP);
        list.place(Some(current));
        let option_labels = options
            .iter()
            .map(|s| SharedString::new_static(s))
            .collect();
        let mut popup = Self {
            paint: Rc::new(ChoicePaint {
                input: input.clone(),
                rows: Vec::new(),
                highlighted: 0,
            }),
            input,
            list,
            cell,
            labels,
            option_labels,
        };
        popup.prepare();
        popup
    }

    /// The cell this popup edits and its labels at open — what a pick
    /// hands to `commit_cell_value`.
    pub(crate) fn target(&self) -> ((usize, usize), (SharedString, SharedString)) {
        (self.cell, self.labels.clone())
    }

    /// The prepared paint, for the delegate's mirror — an `Rc` bump.
    pub(crate) fn paint(&self) -> Rc<ChoicePaint> {
        Rc::clone(&self.paint)
    }

    /// Re-prepare [`Self::paint`] from the list's painted window. Called
    /// after every change to the list; `render_choice` reads the result
    /// and formats nothing. Each row is a static-string clone.
    pub(crate) fn prepare(&mut self) {
        self.paint = Rc::new(ChoicePaint {
            input: self.input.clone(),
            rows: self
                .list
                .painted()
                .iter()
                .map(|r| self.option_labels[r.row].clone())
                .collect(),
            highlighted: self.list.highlighted(),
        });
    }
}

/// The action list's own state: the prepared rows ([`crate::core::menu::rows`],
/// built once when the menu opens — never in `render`) and which one is
/// highlighted.
pub(crate) struct MenuState {
    pub rows: Vec<MenuRow>,
    pub highlighted: usize,
}

/// Maximum painted picker rows. The shared ChoiceList moves this window to
/// keep selection visible while retaining every ranked result. The constant is
/// checked against DEFAULT_CAP; no separate scrolling state is stored here.
pub(crate) const PICKER_ROWS: usize = 12;
const _: () = assert!(PICKER_ROWS == geode_shell::choice::DEFAULT_CAP);

/// Underlying-picker state over ChoiceList. Selection survives reranking and
/// catalogue replacement by bare key text, not positional index. Prepared labels
/// are parallel to declared options and avoid string decoration during render.
///
/// Marks snapshot parked-draft count phrases at open, producing labels such as
/// `NKY.Z · 1 cell, spot_ref`. Matching uses bare keys only. Replacing the catalogue
/// reuses those marks; callers that change parked drafts must close or refresh
/// the picker rather than leave its decorations stale.
pub(crate) struct PickerRows {
    /// Ranking, selection identity, and the moving window are owned by ChoiceList.
    list: geode_shell::choice::ChoiceList,
    pub labels: Vec<SharedString>,
    pub marks: BTreeMap<String, String>,
}

/// The underlying picker's own state: its filter field, which HOLDS the
/// keyboard (what makes `key_context()` report `mode == insert` while the
/// picker is open), and the pure rows beneath it.
pub(crate) struct PickerState {
    pub input: Entity<InputState>,
    pub rows: PickerRows,
}

impl PickerRows {
    /// Every key ranked in catalog order under an empty query, the
    /// highlight on row 0, each label decorated with its parked-draft
    /// mark where `marks` names it (the struct's own doc comment).
    pub(crate) fn with_marks(all: Vec<String>, marks: BTreeMap<String, String>) -> Self {
        let labels = Self::labels_for(&all, &marks);
        Self {
            list: geode_shell::choice::ChoiceList::new(all, PICKER_ROWS),
            labels,
            marks,
        }
    }

    /// The prepared row text, one per `all` entry — built here and in
    /// [`Self::replace_all`], never in `render_picker`. A key with a
    /// parked draft is spelled `<key> · <count phrase>`; every other key
    /// is bare.
    fn labels_for(all: &[String], marks: &BTreeMap<String, String>) -> Vec<SharedString> {
        all.iter()
            .map(|s| match marks.get(s) {
                Some(phrase) => SharedString::from(format!("{s} \u{b7} {phrase}")),
                None => SharedString::from(s.as_str()),
            })
            .collect()
    }

    /// The catalog itself, in declared order.
    pub(crate) fn all(&self) -> &[String] {
        self.list.options()
    }

    /// The text the ranking was last built against — a test-only
    /// accessor; production code only ever sets it, through
    /// [`Self::refilter`].
    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        self.list.query()
    }

    /// How many ranked rows are painted — [`geode_shell::choice::ChoiceList::painted_len`].
    pub(crate) fn painted_len(&self) -> usize {
        self.list.painted_len()
    }

    /// The declared indices of the painted rows, in ranked order — what
    /// [`render_picker`] paints, and what [`Self::highlighted`] indexes
    /// into.
    pub(crate) fn painted(&self) -> impl Iterator<Item = usize> + '_ {
        self.list.painted().iter().map(|r| r.row)
    }

    /// The highlighted row, WINDOW-relative — an index into
    /// [`Self::painted`], never into the whole ranked list.
    pub(crate) fn highlighted(&self) -> usize {
        self.list.highlighted()
    }

    /// The catalog key currently highlighted, `None` with an empty
    /// ranked list — the identity every re-rank preserves. A test-only
    /// accessor (`tile::picker_highlighted_key`'s own door); production
    /// code resolves a pick through [`Self::painted`] and [`Self::all`]
    /// instead ([`crate::tile::MarketDataTile::picker_pick`]).
    #[cfg(test)]
    pub(crate) fn highlighted_key(&self) -> Option<&str> {
        self.list.highlighted_text()
    }

    /// A click or hover on painted row `row` (window-relative, matching
    /// [`Self::highlighted`]): refused past the painted range, which a
    /// click or hover cannot reach anyway.
    pub(crate) fn set_highlighted(&mut self, row: usize) -> bool {
        self.list.set_highlighted(row)
    }

    /// Rerank a changed query while preserving selection by key text. An unchanged
    /// query is a no-op, allowing commit to reread Input without resetting selection.
    pub(crate) fn refilter(&mut self, new_query: &str) {
        self.list.set_query(new_query);
    }

    /// Replace catalogue options and prepared labels. ChoiceList captures selected
    /// key text before replacement so insertions or reordering do not change identity.
    pub(crate) fn replace_all(&mut self, all: Vec<String>) {
        self.labels = Self::labels_for(&all, &self.marks);
        self.list.replace_options(all);
    }

    /// Test helper: rank the current catalogue/query and place a matching key,
    /// falling back to the first ranked row when absent. The moving window follows
    /// the selection even when it lies beyond the initial twelve rows.
    #[cfg(test)]
    pub(crate) fn place(&mut self, key: Option<&str>) {
        self.list.place(key);
    }

    /// Move through the full ranked list with both ends clamped. The painted
    /// window follows selection; it does not limit the navigable result set.
    pub(crate) fn step_highlighted(&mut self, delta: isize) {
        self.list
            .nav_clamped(geode_shell::vimnav::NavCommand::Move(delta as i64));
    }
}

/// Paint the header-anchored action list. Hover updates selection; a row
/// press uses the same pick path as Enter. Outside presses close the popup.
pub(crate) fn render_menu(
    m: &MenuState,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(move || format!("marketdata-menu-{tile_id}"))
        // Occlude the grid so popup hover and press events do not also hit its rows.
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_popup_with_window(window, cx))
        });
    for (i, row) in m.rows.iter().enumerate() {
        list = list.child(match row {
            // `PopupMenu`'s own separator: a hairline-class rule bleeding
            // into the container's inset, half a step of air either side.
            MenuRow::Separator => div()
                .my_0p5()
                .mx_neg_1()
                .border_b(px(2.))
                .border_color(theme.border)
                .into_any_element(),
            MenuRow::Section(s) => div()
                .px(scale::design(ROW_INSET))
                .pt_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(s.clone())
                .into_any_element(),
            MenuRow::Action {
                title,
                hint,
                enabled,
                checked,
                ..
            } => {
                let disabled = enabled.is_err();
                // A disabled row says why; an enabled one shows its key
                // (or its `:` verb).
                let reason = match enabled {
                    Err(r) => div().child(*r).into_any_element(),
                    Ok(()) => geode_shell::shell::kbd::spec(hint),
                };
                // A choice row carries a tick or a same-width blank
                // ahead of its title, so the group's titles align
                // whichever one is in force. Two static strings — a
                // frame formats nothing here.
                let tick: Option<&'static str> = checked.map(|on| if on { "\u{2713}" } else { "" });
                h_flex()
                    .h(scale::design(ROW_HEIGHT))
                    .px(scale::design(ROW_INSET))
                    .rounded(theme.radius)
                    .items_center()
                    .justify_between()
                    .gap_4()
                    // Selected enabled rows use accent colors. Navigation can land on a
                    // disabled row, but it remains muted and picking it reports the refusal.
                    .when(i == m.highlighted && !disabled, |d| {
                        d.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .when(i != m.highlighted || disabled, |d| {
                        d.text_color(if disabled {
                            theme.muted_foreground
                        } else {
                            theme.popover_foreground
                        })
                    })
                    .debug_selector(move || format!("marketdata-menu-row-{tile_id}-{i}"))
                    // Consume row presses so a pick cannot also trigger the shell's tile-level
                    // handling for the surface beneath this popup. The header toggle deliberately
                    // allows that propagation because it must also focus its tile.
                    .on_mouse_down(MouseButton::Left, {
                        let tile = tile.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                        }
                    })
                    // Hovering a row is the mouse form of `j`/`k`: the
                    // highlight follows the pointer (greyed rows too — a
                    // hover is a hover, and `enter` on one is a notice).
                    // gpui gates this on the row's own hitbox, so it never
                    // fires for the row beneath the pointer's neighbour.
                    .on_mouse_move({
                        let tile = tile.clone();
                        move |_, _, cx| tile.update(cx, |t, cx| t.menu_hover(i, cx))
                    })
                    .child(
                        h_flex()
                            .gap_1()
                            .when_some(tick, |d, tick| {
                                d.child(div().w(scale::design(14.)).flex_shrink_0().child(tick))
                            })
                            .child(title.clone()),
                    )
                    .child(div().text_color(theme.muted_foreground).child(reason))
                    .into_any_element()
            }
        });
    }
    deferred(
        anchored()
            .anchor(Anchor::TopRight)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(8.))
            .child(list),
    )
    .with_priority(1)
}

/// Paint the underlying picker's focused input and moving window of prepared
/// catalogue labels. Click commits; hover changes selection. An empty match list
/// shows the same fallback message as an empty or unavailable catalogue.
pub(crate) fn render_picker(
    p: &PickerState,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(move || format!("marketdata-picker-{tile_id}"))
        // Occludes for the same reason the menu does (see `render_menu`).
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_popup_with_window(window, cx))
        })
        .child(
            div()
                .w_full()
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(theme.border)
                // open_picker configures the InputState placeholder once.
                .child(Input::new(&p.input).appearance(false).w_full()),
        );
    let rows = &p.rows;
    if rows.painted_len() == 0 {
        list = list.child(
            div()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .flex()
                .items_center()
                .text_color(theme.muted_foreground)
                .child("no underlyings known"),
        );
    } else {
        // Only the painted WINDOW is painted (the constant's own doc
        // comment): the query narrows the ranked list, and stepping past
        // the cap slides the window rather than adding rows.
        for (row_i, i) in rows.painted().enumerate() {
            // Clone the prepared label; avoid constructing decorated text during paint.
            let text = rows.labels[i].clone();
            list = list.child(
                h_flex()
                    .h(scale::design(ROW_HEIGHT))
                    .px(scale::design(ROW_INSET))
                    .rounded(theme.radius)
                    .items_center()
                    .when(row_i == rows.highlighted(), |d| {
                        d.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .when(row_i != rows.highlighted(), |d| {
                        d.text_color(theme.popover_foreground)
                    })
                    .debug_selector(move || format!("marketdata-picker-row-{tile_id}-{row_i}"))
                    .on_mouse_down(MouseButton::Left, {
                        let tile = tile.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            tile.update(cx, |t, cx| t.picker_pick(row_i, window, cx))
                        }
                    })
                    // The mouse form of `up`/`down` (see `render_menu`).
                    .on_mouse_move({
                        let tile = tile.clone();
                        move |_, _, cx| tile.update(cx, |t, cx| t.picker_hover(row_i, cx))
                    })
                    .child(text)
                    .into_any_element(),
            );
        }
    }
    deferred(
        anchored()
            .anchor(Anchor::TopRight)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(8.))
            .child(list),
    )
    .with_priority(1)
}

/// Paint a Choice cell's prepared input and option window. Click picks, hover
/// selects, and an outside press closes. The delegate supplies a bottom-left cell
/// anchor; this TopLeft deferred panel escapes the table clip and snaps inside
/// the window. Rendering performs no ranking or option-label formatting.
pub(crate) fn render_choice(
    p: &ChoicePaint,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(move || format!("marketdata-choice-{tile_id}"))
        // Occludes for the same reason the menu does (see `render_menu`).
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_popup_with_window(window, cx))
        })
        .child(
            div()
                .w_full()
                .pb_1()
                .mb_1()
                .border_b_1()
                .border_color(theme.border)
                .child(Input::new(&p.input).appearance(false).w_full()),
        );
    if p.rows.is_empty() {
        // Asked and answered, never a blank rectangle (`render_picker`'s
        // own rule): `enter` on this refuses with the same words.
        list = list.child(
            div()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .flex()
                .items_center()
                .text_color(theme.muted_foreground)
                .child("no option matches"),
        );
    } else {
        for (row_i, text) in p.rows.iter().enumerate() {
            list = list.child(
                h_flex()
                    .h(scale::design(ROW_HEIGHT))
                    .px(scale::design(ROW_INSET))
                    .rounded(theme.radius)
                    .items_center()
                    .when(row_i == p.highlighted, |d| {
                        d.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .when(row_i != p.highlighted, |d| {
                        d.text_color(theme.popover_foreground)
                    })
                    .debug_selector(move || format!("marketdata-choice-row-{tile_id}-{row_i}"))
                    // `stop_propagation` for the menu row's reason (see
                    // `render_menu`): a click that means "pick a row"
                    // must not also be a click on the grid beneath.
                    .on_mouse_down(MouseButton::Left, {
                        let tile = tile.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            tile.update(cx, |t, cx| t.choice_pick(row_i, window, cx))
                        }
                    })
                    // The mouse form of `up`/`down` (see `render_menu`).
                    .on_mouse_move({
                        let tile = tile.clone();
                        move |_, _, cx| tile.update(cx, |t, cx| t.choice_hover(row_i, cx))
                    })
                    .child(text.clone())
                    .into_any_element(),
            );
        }
    }
    deferred(
        anchored()
            .anchor(Anchor::TopLeft)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(8.))
            .child(list),
    )
    .with_priority(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(keys: &[&str]) -> PickerRows {
        PickerRows::with_marks(
            keys.iter().map(|k| k.to_string()).collect(),
            BTreeMap::new(),
        )
    }

    #[test]
    fn a_new_picker_ranks_every_key_in_catalog_order_with_the_top_row_highlighted() {
        let p = rows(&["AAA.Z", "BBB.Z", "CCC.Z"]);
        assert_eq!(p.painted().collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(p.highlighted(), 0);
        assert_eq!(p.highlighted_key(), Some("AAA.Z"));
        assert_eq!(p.labels.len(), 3);
    }

    /// Parked-draft phrases decorate labels without becoming searchable. Catalogue
    /// replacement keeps those marks while reranking the bare keys.
    #[test]
    fn a_parked_key_is_marked_in_its_label_but_ranked_by_the_bare_key() {
        let marks: BTreeMap<String, String> =
            [("SPX.Z".to_string(), "1 cell, spot_ref".to_string())].into();
        let mut p = PickerRows::with_marks(
            ["NKY.Z", "SPX.Z"].iter().map(|k| k.to_string()).collect(),
            marks,
        );
        assert_eq!(p.labels[0].as_ref(), "NKY.Z");
        assert_eq!(p.labels[1].as_ref(), "SPX.Z \u{b7} 1 cell, spot_ref");
        // `sp` is in SPX's phrase ("spot_ref") AND its key; `cell` is in
        // the phrase alone — only the key is searchable.
        p.refilter("cell");
        assert_eq!(p.painted_len(), 0, "the phrase is not searchable");
        p.refilter("sp");
        assert_eq!(p.painted().collect::<Vec<_>>(), vec![1]);
        p.refilter("");
        p.replace_all(
            ["NDX.Z", "NKY.Z", "SPX.Z"]
                .iter()
                .map(|k| k.to_string())
                .collect(),
        );
        assert_eq!(p.labels[2].as_ref(), "SPX.Z \u{b7} 1 cell, spot_ref");
        assert_eq!(p.labels[0].as_ref(), "NDX.Z");
    }

    /// An unchanged query leaves a manually moved highlight intact.
    #[test]
    fn an_unchanged_query_keeps_the_highlight() {
        let mut p = rows(&["AAA.Z", "BBB.Z", "CCC.Z"]);
        p.step_highlighted(2);
        assert_eq!(p.highlighted_key(), Some("CCC.Z"));
        p.refilter("");
        assert_eq!(p.highlighted(), 2, "same query, same row");
        assert_eq!(p.highlighted_key(), Some("CCC.Z"));
        assert_eq!(p.query(), "");
    }

    /// A changed query re-ranks and re-finds the highlighted KEY at its
    /// new ranked position.
    #[test]
    fn a_changed_query_re_places_the_highlight_by_key() {
        let mut p = rows(&["AAA.Z", "BBB.Z", "BBC.Z"]);
        p.step_highlighted(2);
        assert_eq!(p.highlighted_key(), Some("BBC.Z"));
        p.refilter("bb");
        assert_eq!(p.query(), "bb");
        let painted: Vec<usize> = p.painted().collect();
        assert!(!painted.contains(&0), "AAA.Z does not match");
        assert_eq!(
            p.highlighted_key(),
            Some("BBC.Z"),
            "the highlight followed the key, not its old index: {painted:?}"
        );
    }

    /// A kept key the query filters OUT has no row to land on: the
    /// highlight falls to row 0 rather than pointing past the ranking.
    #[test]
    fn a_key_the_query_filtered_out_falls_to_row_0() {
        let mut p = rows(&["AAA.Z", "BBB.Z", "CCC.Z"]);
        p.step_highlighted(1);
        assert_eq!(p.highlighted_key(), Some("BBB.Z"));
        p.refilter("CCC");
        assert_eq!(p.painted().collect::<Vec<_>>(), vec![2]);
        assert_eq!(p.highlighted(), 0);
        assert_eq!(p.highlighted_key(), Some("CCC.Z"));
    }

    /// Reordering preserves the selected key; removing it selects the first match.
    #[test]
    fn replace_all_keeps_the_key_across_a_resort_and_falls_back_on_removal() {
        let mut p = rows(&["BBB.Z", "CCC.Z"]);
        p.step_highlighted(1);
        p.replace_all(vec!["AAA.Z".into(), "BBB.Z".into(), "CCC.Z".into()]);
        assert_eq!(p.highlighted(), 2, "CCC.Z shifted from index 1 to 2");
        assert_eq!(p.highlighted_key(), Some("CCC.Z"));
        assert_eq!(p.labels.len(), 3, "labels follow the catalog");
        assert_eq!(
            p.all().to_vec(),
            vec![
                "AAA.Z".to_string(),
                "BBB.Z".to_string(),
                "CCC.Z".to_string()
            ]
        );
        p.replace_all(vec!["BBB.Z".into()]);
        assert_eq!(p.highlighted(), 0);
        assert_eq!(p.highlighted_key(), Some("BBB.Z"));
    }

    /// `place` is a thin, test-only door directly onto
    /// [`geode_shell::choice::ChoiceList::place`] (production reaches the
    /// same logic through `refilter`/`replace_all`, which call it on the
    /// list itself) — exercised here on its own so the wrapper is proven
    /// to forward, not just the list underneath it.
    #[test]
    fn place_jumps_the_highlight_to_a_named_key() {
        let mut p = rows(&["AAA.Z", "BBB.Z", "CCC.Z"]);
        p.place(Some("CCC.Z"));
        assert_eq!(p.highlighted_key(), Some("CCC.Z"));
        p.place(None);
        assert_eq!(
            p.highlighted_key(),
            Some("AAA.Z"),
            "None falls back to row 0"
        );
    }

    #[test]
    fn an_empty_catalog_has_no_highlighted_key_and_step_stays_at_0() {
        let mut p = rows(&[]);
        assert_eq!(p.highlighted_key(), None);
        p.step_highlighted(3);
        assert_eq!(p.highlighted(), 0);
        p.step_highlighted(-3);
        assert_eq!(p.highlighted(), 0);
        assert_eq!(p.highlighted_key(), None);
        p.refilter("x");
        assert_eq!(p.highlighted_key(), None);
    }

    /// Picker navigation clamps even a one-row step, unlike ChoiceList's wrapping
    /// nav method. The wrapper uses nav_clamped for every delta.
    #[test]
    fn step_clamps_at_both_ends() {
        let mut p = rows(&["AAA.Z", "BBB.Z", "CCC.Z"]);
        p.step_highlighted(-5);
        assert_eq!(p.highlighted(), 0);
        p.step_highlighted(-1);
        assert_eq!(p.highlighted(), 0, "a bare -1 clamps at row 0, never wraps");
        p.step_highlighted(50);
        assert_eq!(p.highlighted(), 2);
        p.step_highlighted(1);
        assert_eq!(
            p.highlighted(),
            2,
            "a bare +1 clamps at the last row, never wraps"
        );
    }

    /// Navigation beyond the initial cap moves the painted window to keep the
    /// selected result visible. The underlying ranked list retains all matches.
    #[test]
    fn the_highlight_never_leaves_the_painted_rows() {
        let keys: Vec<String> = (0..20).map(|i| format!("K{i:02}.Z")).collect();
        let mut p = PickerRows::with_marks(keys.clone(), BTreeMap::new());
        assert_eq!(p.painted_len(), PICKER_ROWS, "the window opens at the top");
        p.step_highlighted(100);
        assert_eq!(
            p.highlighted_key(),
            Some(keys[19].as_str()),
            "clamped at the last DECLARED row, not the last painted one"
        );
        assert_eq!(
            p.highlighted(),
            PICKER_ROWS - 1,
            "the last row OF the window"
        );
        assert_eq!(p.painted_len(), PICKER_ROWS, "still a full window");

        // A new key before the selected one changes its index; the window follows
        // the preserved key into view.
        let mut all = keys.clone();
        all.insert(0, "A00.Z".into());
        p.replace_all(all);
        assert_eq!(
            p.highlighted_key(),
            Some("K19.Z"),
            "still found and kept in view past the cap"
        );
    }
}
