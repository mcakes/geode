//! The tile-owned anchored popup (market-data spec 2026-09-14 §6.1): the
//! panel's own overlay, painted with gpui's `deferred(anchored(..))` so
//! it escapes the tile's own clip and paints above neighbouring tiles —
//! instant, no animation, Geode's own chrome rather than a gpui-component
//! `Dialog` or `Popover`. `Popup::Menu` is Task 6's variant; `Popup::Picker`
//! (Task 7, spec §7) is the underlying picker `u` and the menu row open.
//! [`PickerRows`] is the picker's pure half, tested below without a window.

use crate::core::menu::MenuRow;
use crate::tile::MarketDataTile;
use geode_shell::shell::scale;
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, Div, Entity, IntoElement, MouseButton, SharedString,
    anchored, deferred, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, ThemeStyled as _, h_flex, v_flex};

/// A menu row's height, in pixels at the design rem — gpui-component's
/// own `PopupMenu` item height at its default size, so this popup keeps
/// the menu family's geometry (design guide: "preserve the component
/// family's geometry… do not imitate one menu with a custom popup whose
/// spacing only approximates the system") while following Geode's rem.
const ROW_HEIGHT: f32 = 26.0;
/// A menu row's horizontal inset — `PopupMenu`'s `INNER_PADDING`.
const ROW_INSET: f32 = 8.0;
/// The popup's minimum width at the design rem.
const MIN_WIDTH: f32 = 240.0;

/// The popup surface both the menu and the picker paint on: gpui-
/// component's own popover treatment (`popover_style` — `popover`
/// background and foreground, the ring-in-shadow edge, `theme.radius`),
/// so this surface and the crate's own `PopupMenu`/`Select`/`DatePicker`
/// popovers cannot drift apart; then the item container's `p_1` inset.
fn popover_surface(cx: &App) -> Div {
    v_flex()
        .min_w(scale::design(MIN_WIDTH))
        .p_1()
        .gap_y_0p5()
        .text_sm()
        .popover_style(cx)
}
use std::collections::BTreeMap;

/// What the tile currently has open. `Menu` is the action list (spec
/// §6.1); `Picker` is the underlying picker (spec §7) — its `input`
/// HOLDS the keyboard, which is what makes `key_context()` report
/// `mode == insert` while it is open rather than a third mode of its
/// own.
pub(crate) enum Popup {
    Menu(MenuState),
    Picker(PickerState),
}

/// The action list's own state: the prepared rows ([`crate::core::menu::rows`],
/// built once when the menu opens — never in `render`) and which one is
/// highlighted.
pub(crate) struct MenuState {
    pub rows: Vec<MenuRow>,
    pub highlighted: usize,
}

/// How many ranked rows the picker PAINTS (final review, A3). The
/// ranked list itself is unbounded — a dataset's catalog can hold hundreds
/// of keys — but the popup paints only the top `PICKER_ROWS` and the
/// query narrows the rest: a cap rather than a bounded scroll container,
/// because a cap needs no scroll state and the picker is a type-to-narrow
/// surface, never a browse-by-scrolling one. The highlight is clamped to
/// the painted range ([`PickerRows::place`], [`PickerRows::step_highlighted`])
/// so `enter` can never load a row the trader cannot see.
pub(crate) const PICKER_ROWS: usize = 12;

/// The underlying picker's PURE half (spec §7; split out by the final
/// review, A1, so it is testable without a window): the dataset's catalog
/// keys (`all`, taken once at open — a fresh catalog while the picker
/// stays open is folded back in by the tile's own diagnostics observer
/// through [`Self::replace_all`], never read here), the current ranking
/// of `all`'s indices (`geode_shell::listfilter::rank`, over
/// `display_key`'s own spelling), and which ranked row is highlighted.
///
/// `all: Vec<String>`, not `Vec<SharedString>`, because
/// [`geode_shell::listfilter::rank`] takes `&[String]`. `labels` is the
/// separate, PREPARED `SharedString` for each `all` entry (review fix
/// round 1, IMPORTANT-3) — [`Self::with_marks`] and [`Self::replace_all`] both
/// fill it off the render thread, so [`render_picker`] only ever clones a
/// prepared `SharedString` per row (an inline copy or an `Arc` bump, never
/// an allocation); at the pinned release `SharedString` wraps
/// `smol_str::SmolStr` (`gpui-pre-shared-string-0.3.5/
/// gpui_shared_string.rs`), which stores up to 23 bytes inline, so
/// `SharedString::from(&str)` heap-allocates only for a longer string
/// — it had no inline form at the old git rev — but an underlying key
/// can exceed that, and the charter's "nothing allocates in render" is
/// about the rule, not the byte count, so the conversion still happens
/// once per row when `all` changes, never per frame (as the first
/// build did on every repaint while a picker was open).
///
/// `query` is the text `ranked` was last built against — kept so
/// [`Self::refilter`] can tell "the trader typed something new" from
/// "nothing changed, don't touch the highlight" (review fix round 1,
/// CRITICAL): the first build re-ranked (and reset the highlight to 0)
/// on every call, including the ONE `commit`/`enter` always makes to
/// cover a test harness's `set_value` (which fires no `Change` event at
/// all) — so `u`, `down`, `down`, `enter` always loaded the TOP match,
/// silently discarding whichever row the trader had actually
/// highlighted.
///
/// `marks` (2026-09-19, per-underlying drafts): the underlyings that
/// carry PARKED edits, keyed by `display_key` and valued with the
/// draft's own `count_phrase` ("1 cell, spot_ref"), taken once at open
/// from the tile's parked map. A marked row's label reads `NKY.Z · 1
/// cell, spot_ref`; ranking still runs over the bare key in `all`, never
/// the decorated label, so typing `sp` cannot match a phrase's own
/// letters. Kept here so [`Self::replace_all`] can re-decorate a fresh
/// catalog without asking the tile again — the parked map only changes
/// through `set_key`, and both of its callers (`picker_pick`, the `:`
/// line) close this popup before calling it, so the marks can never go
/// stale while the picker is open; a third caller would owe the same.
pub(crate) struct PickerRows {
    pub all: Vec<String>,
    pub labels: Vec<SharedString>,
    pub ranked: Vec<usize>,
    pub highlighted: usize,
    pub query: String,
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
        let ranked = (0..all.len()).collect();
        Self {
            all,
            labels,
            ranked,
            highlighted: 0,
            query: String::new(),
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

    /// How many ranked rows are painted — the first [`PICKER_ROWS`] of
    /// them.
    pub(crate) fn painted_len(&self) -> usize {
        self.ranked.len().min(PICKER_ROWS)
    }

    /// The catalog key currently highlighted, `None` with an empty
    /// `ranked` list — the identity every re-rank preserves.
    pub(crate) fn highlighted_key(&self) -> Option<&str> {
        self.ranked
            .get(self.highlighted)
            .map(|&i| self.all[i].as_str())
    }

    /// Re-rank against `new_query`, but ONLY if it actually differs from
    /// the query `ranked` was last built against — a no-op otherwise, so
    /// a defensive re-rank at commit time (the field's current text may
    /// never have reached this struct through a real `Change` event)
    /// costs nothing when nothing changed, and never resets the
    /// highlight out from under a trader who typed nothing at all.
    /// Captures the currently highlighted KEY before rebuilding and
    /// hands it to [`Self::place`], the one door every re-rank path
    /// (this one, and [`Self::replace_all`], review fix round 1,
    /// IMPORTANT-2) goes through.
    pub(crate) fn refilter(&mut self, new_query: &str) {
        if new_query == self.query {
            return;
        }
        let keep = self.highlighted_key().map(str::to_string);
        self.query = new_query.to_string();
        self.place(keep.as_deref());
    }

    /// Swap in a fresh catalog (the diagnostics observer's door), keeping
    /// the highlighted KEY across it. Review fix round 2: the key is
    /// captured BEFORE `all` is overwritten, and by string rather than by
    /// index — `catalog_keys()` returns a freshly SORTED list, so a new
    /// underlying that sorts ahead of the highlighted one shifts every
    /// later index, and re-placing by the OLD index once `all` has
    /// already changed would silently highlight a different row. Forced
    /// through [`Self::place`] rather than [`Self::refilter`]: the query
    /// has not changed, but `all` has, and a re-rank must run regardless.
    pub(crate) fn replace_all(&mut self, all: Vec<String>) {
        let keep = self.highlighted_key().map(str::to_string);
        self.labels = Self::labels_for(&all, &self.marks);
        self.all = all;
        self.place(keep.as_deref());
    }

    /// Rebuild `ranked` against the CURRENT `all`/`query`, then put the
    /// highlight on `key` — falling back to row 0 when `key` is `None`,
    /// no longer present in `all` at all, or ranked past the painted
    /// range (a row the trader cannot see is not one `enter` may load).
    ///
    /// **Identity is the KEY STRING, never a positional index** (review
    /// fix round 2, the bug the first fix's own `rerank` reintroduced by
    /// capturing `was_highlighted` as an ALL-index and looking that same
    /// number up in the NEW `all`): see [`Self::replace_all`]. This
    /// method reads the key back out of `all` as it stands when called —
    /// never before — which is why `replace_all` captures first.
    pub(crate) fn place(&mut self, key: Option<&str>) {
        self.ranked = geode_shell::listfilter::rank(&self.all, &self.query)
            .into_iter()
            .map(|r| r.row)
            .collect();
        self.highlighted = key
            .and_then(|k| self.all.iter().position(|s| s == k))
            .and_then(|all_index| self.ranked.iter().position(|&r| r == all_index))
            .filter(|&row| row < PICKER_ROWS)
            .unwrap_or(0);
    }

    /// Move the highlight `delta` steps over the PAINTED rows, clamped at
    /// either end (never wrapping — `core::menu::step`'s own rule, spec
    /// §7's `up`/`down`).
    pub(crate) fn step_highlighted(&mut self, delta: isize) {
        let painted = self.painted_len();
        if painted == 0 {
            self.highlighted = 0;
            return;
        }
        let moved = (self.highlighted as isize + delta).clamp(0, painted as isize - 1);
        self.highlighted = moved as usize;
    }
}

/// Paint the action list, anchored at the header's own right edge (spec
/// §6.1). `tile`/`tile_id` are this popup's own mouse door — a row click
/// picks it, exactly as `enter` on the highlighted row would — and a
/// click anywhere outside closes it.
pub(crate) fn render_menu(
    m: &MenuState,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover_surface(cx)
        .debug_selector(move || format!("marketdata-menu-{tile_id}"))
        // The popup OCCLUDES (user report 2026-09-17): without this, gpui
        // keeps hit-testing the grid painted beneath it, so hovering a
        // menu row lit up the table row under the pointer instead. The
        // shell's own modal (`dialog.rs`) makes the same call.
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
                let reason: SharedString = match enabled {
                    Err(r) => (*r).into(),
                    Ok(()) => hint.clone(),
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
                    // The family's selected treatment (`MenuItemElement`):
                    // `accent` under `accent_foreground`, never a second
                    // list token.
                    .when(i == m.highlighted, |d| {
                        d.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .when(i != m.highlighted, |d| {
                        d.text_color(if disabled {
                            theme.muted_foreground
                        } else {
                            theme.popover_foreground
                        })
                    })
                    .debug_selector(move || format!("marketdata-menu-row-{tile_id}-{i}"))
                    // `stop_propagation` here is NOT load-bearing for
                    // focus any more (it was, per the final review's B5,
                    // until the 2026-09-17 insert-focus rule): "Load
                    // underlying…" opens the picker and focuses its field
                    // inside this very handler, and even were the click
                    // to bubble on, `render`'s `pending_focus_restore`
                    // consumption now SKIPS the restore whenever the
                    // focused tile's occupant holds its own input in
                    // insert mode (`occupant_holds_insert_focus`) — the
                    // field keeps the keyboard either way.
                    //
                    // The stop is kept for a different reason: a click
                    // that means "pick a row" must not ALSO run the
                    // shell's ordinary tile-level click handling (drag
                    // arming, dock focus) for the tile underneath the
                    // popup — the same reason the popup occludes what is
                    // painted beneath it. Contrast the `⋯` button
                    // (`header.rs`), which must NOT stop propagation: no
                    // field is focused after it, so the shell's
                    // click-to-focus is exactly what should run. Do not
                    // be fooled by the grid case: the pinned
                    // `TableState::set_selected_row` (run by `sync_cursor`
                    // at the end of every `dispatch`) stops propagation
                    // of its own, so with the cursor in the grid this
                    // stop looks redundant — with the cursor in the
                    // strip (`clear_selection`, which stops nothing) it
                    // is the only one there is.
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

/// Paint the underlying picker (spec §7): the same anchored shell
/// [`render_menu`] uses, a filter `Input` on top and one row per ranked
/// catalog key below (a click loads it, the way a menu row click picks
/// it). An empty ranked list paints one muted row rather than nothing,
/// so an unconfigured dataset or a catalog that has not arrived yet
/// still reads as "asked and answered" instead of a blank rectangle.
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
                // The placeholder itself ("underlying" — the trader's own
                // word, spec §7) is set once, on the `InputState`, at
                // `open_picker` — an `Input` element has no such builder
                // of its own.
                .child(Input::new(&p.input).appearance(false).w_full()),
        );
    let rows = &p.rows;
    if rows.ranked.is_empty() {
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
        // Only the first `PICKER_ROWS` ranked rows are painted (the
        // constant's own doc comment): the query narrows the rest.
        for (row_i, &i) in rows.ranked.iter().take(PICKER_ROWS).enumerate() {
            // `labels[i]` is prepared off-render (`PickerRows`'s own
            // doc comment, review fix round 1, IMPORTANT-3) — this is a
            // refcount clone, never a conversion.
            let text = rows.labels[i].clone();
            list = list.child(
                h_flex()
                    .h(scale::design(ROW_HEIGHT))
                    .px(scale::design(ROW_INSET))
                    .rounded(theme.radius)
                    .items_center()
                    .when(row_i == rows.highlighted, |d| {
                        d.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .when(row_i != rows.highlighted, |d| {
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
        assert_eq!(p.ranked, vec![0, 1, 2]);
        assert_eq!(p.highlighted, 0);
        assert_eq!(p.highlighted_key(), Some("AAA.Z"));
        assert_eq!(p.labels.len(), 3);
    }

    /// Per-underlying drafts (2026-09-19): a parked key's label carries
    /// its count phrase, every other label is bare, ranking runs over the
    /// bare key alone, and a fresh catalog is re-decorated from the same
    /// marks.
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
        assert!(p.ranked.is_empty(), "the phrase is not searchable");
        p.refilter("sp");
        assert_eq!(p.ranked, vec![1]);
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

    /// The round-1 CRITICAL: an unchanged query is a no-op, so the
    /// defensive re-rank `commit` always makes never resets a highlight
    /// the trader moved.
    #[test]
    fn an_unchanged_query_keeps_the_highlight() {
        let mut p = rows(&["AAA.Z", "BBB.Z", "CCC.Z"]);
        p.step_highlighted(2);
        assert_eq!(p.highlighted_key(), Some("CCC.Z"));
        p.refilter("");
        assert_eq!(p.highlighted, 2, "same query, same row");
        assert_eq!(p.highlighted_key(), Some("CCC.Z"));
    }

    /// A changed query re-ranks and re-finds the highlighted KEY at its
    /// new ranked position.
    #[test]
    fn a_changed_query_re_places_the_highlight_by_key() {
        let mut p = rows(&["AAA.Z", "BBB.Z", "BBC.Z"]);
        p.step_highlighted(2);
        assert_eq!(p.highlighted_key(), Some("BBC.Z"));
        p.refilter("bb");
        assert!(!p.ranked.contains(&0), "AAA.Z does not match");
        assert_eq!(
            p.highlighted_key(),
            Some("BBC.Z"),
            "the highlight followed the key, not its old index: {:?}",
            p.ranked
        );
    }

    /// A kept key the query filters OUT has no row to land on: the
    /// highlight falls to row 0 rather than pointing past `ranked`.
    #[test]
    fn a_key_the_query_filtered_out_falls_to_row_0() {
        let mut p = rows(&["AAA.Z", "BBB.Z", "CCC.Z"]);
        p.step_highlighted(1);
        assert_eq!(p.highlighted_key(), Some("BBB.Z"));
        p.refilter("CCC");
        assert_eq!(p.ranked, vec![2]);
        assert_eq!(p.highlighted, 0);
        assert_eq!(p.highlighted_key(), Some("CCC.Z"));
    }

    /// The round-2 fix, pure: a re-sorted catalog keeps the KEY, and a
    /// genuine removal falls back to row 0.
    #[test]
    fn replace_all_keeps_the_key_across_a_resort_and_falls_back_on_removal() {
        let mut p = rows(&["BBB.Z", "CCC.Z"]);
        p.step_highlighted(1);
        p.replace_all(vec!["AAA.Z".into(), "BBB.Z".into(), "CCC.Z".into()]);
        assert_eq!(p.highlighted, 2, "CCC.Z shifted from index 1 to 2");
        assert_eq!(p.highlighted_key(), Some("CCC.Z"));
        assert_eq!(p.labels.len(), 3, "labels follow the catalog");
        p.replace_all(vec!["BBB.Z".into()]);
        assert_eq!(p.highlighted, 0);
        assert_eq!(p.highlighted_key(), Some("BBB.Z"));
    }

    #[test]
    fn an_empty_catalog_has_no_highlighted_key_and_step_stays_at_0() {
        let mut p = rows(&[]);
        assert_eq!(p.highlighted_key(), None);
        p.step_highlighted(3);
        assert_eq!(p.highlighted, 0);
        p.step_highlighted(-3);
        assert_eq!(p.highlighted, 0);
        assert_eq!(p.highlighted_key(), None);
        p.refilter("x");
        assert_eq!(p.highlighted_key(), None);
    }

    #[test]
    fn step_clamps_at_both_ends() {
        let mut p = rows(&["AAA.Z", "BBB.Z", "CCC.Z"]);
        p.step_highlighted(-5);
        assert_eq!(p.highlighted, 0);
        p.step_highlighted(50);
        assert_eq!(p.highlighted, 2);
        p.step_highlighted(-1);
        assert_eq!(p.highlighted, 1);
    }

    /// A3: the ranked list is unbounded, but the highlight lives inside
    /// the painted `PICKER_ROWS` — `step` cannot pass the last painted
    /// row, and a kept key ranked past it falls to row 0 rather than
    /// highlighting a row nobody can see.
    #[test]
    fn the_highlight_never_leaves_the_painted_rows() {
        let keys: Vec<String> = (0..20).map(|i| format!("K{i:02}.Z")).collect();
        let mut p = PickerRows::with_marks(keys, BTreeMap::new());
        assert_eq!(p.ranked.len(), 20, "ranking is unbounded");
        assert_eq!(p.painted_len(), PICKER_ROWS);
        p.step_highlighted(100);
        assert_eq!(
            p.highlighted,
            PICKER_ROWS - 1,
            "step stops at the last painted row"
        );
        // Highlight K11 (painted row 11), then a catalog with a key that
        // sorts ahead of it pushes it to ranked position 12 — unpainted.
        let mut all: Vec<String> = (0..20).map(|i| format!("K{i:02}.Z")).collect();
        all.insert(0, "A00.Z".into());
        p.replace_all(all);
        assert_eq!(
            p.highlighted, 0,
            "a key ranked past the painted rows falls to 0"
        );
    }
}
