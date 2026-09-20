//! The tile-owned anchored popup (market-data spec 2026-09-14 §6.1): the
//! panel's own overlay, painted with gpui's `deferred(anchored(..))` so
//! it escapes the tile's own clip and paints above neighbouring tiles —
//! instant, no animation, Geode's own chrome rather than a gpui-component
//! `Dialog` or `Popover`. `Popup::Menu` is Task 6's variant; `Popup::Picker`
//! (Task 7, spec §7) is the underlying picker `u` and the menu row open;
//! `Popup::Choice` (dividend spec §4.4) is a `Choice` cell's typeahead,
//! the picker's shape hung under the cell it edits.
//! [`PickerRows`] is the picker's pure half, tested below without a window.

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
/// own; `Choice` is a `Choice` cell's typeahead (dividend spec §4.4),
/// whose field holds the keyboard exactly as the picker's does.
pub(crate) enum Popup {
    Menu(MenuState),
    Picker(PickerState),
    Choice(ChoicePopup),
}

/// A `Choice` cell's typeahead (dividend spec §4.4): the picker's shape —
/// a field that HOLDS the keyboard over a ranked list — opened by
/// `i`/`enter`/a double-click on a cell whose column declares a fixed
/// vocabulary, placed on the cell's current value, and hung UNDER that
/// cell rather than off the header (spec §3.4: "anchored at the cell").
///
/// The list is [`geode_shell::choice::ChoiceList`] directly rather than
/// [`PickerRows`]: the options are a column's `&'static [&'static str]`,
/// so every row label is a static `SharedString` (no marks, no catalog
/// to swap) and the picker's label preparation has nothing to do here.
///
/// `cell`/`labels` are the cell it opened on and that cell's labels at
/// the time — `EditTarget::Cell`'s own identity pair, checked by
/// `commit_cell_value` at the pick so a document that moved under the
/// popup refuses rather than writing to whatever cell now sits there.
///
/// `paint` is the delegate's paint-time COPY of the rows (the cursor
/// and editor mirrors' own rule): the popup is painted from
/// `MatrixDelegate::render_td`, inside the cell it hangs under, and the
/// delegate never reads the tile — so [`Self::prepare`] re-prepares this
/// `Rc` on every change to the list (a keystroke, an arrow, a hover) and
/// the tile mirrors the `Rc` across. Never in `render`.
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

/// How many ranked rows the picker PAINTS at once (final review, A3;
/// window semantics, 2026-09-19): a cap rather than a bounded scroll
/// container, because a cap needs no scroll state and the picker is a
/// type-to-narrow surface, never a browse-by-scrolling one. This IS
/// [`geode_shell::choice::DEFAULT_CAP`]'s own value — one cap for every
/// choice surface, spec 2026-09-19 §3.1 — and the assert below is what
/// keeps the two from drifting apart. The painted rows are a WINDOW that
/// follows the highlight (`ChoiceList::follow`), not a truncation of the
/// ranked list, so `enter` can never load a row the trader cannot see
/// even when the current value ranks past row `PICKER_ROWS`.
pub(crate) const PICKER_ROWS: usize = 12;
const _: () = assert!(PICKER_ROWS == geode_shell::choice::DEFAULT_CAP);

/// The underlying picker's PURE half (spec §7; split out by the final
/// review, A1, so it is testable without a window): a thin wrapper over
/// [`geode_shell::choice::ChoiceList`] (spec 2026-09-19 §3.1), which owns
/// the ranking, the highlight, the identity-by-key-string rule across a
/// re-rank and the painted-window cap — one core shared with the object
/// and settings dialogs' own `Choice` fields, so those rules are spelled
/// once. This struct adds only what the picker needs beyond a plain
/// choice field.
///
/// `labels` is the separate, PREPARED `SharedString` for each catalog key
/// (review fix round 1, IMPORTANT-3) — [`Self::with_marks`] and
/// [`Self::replace_all`] both fill it off the render thread, so
/// [`render_picker`] only ever clones a prepared `SharedString` per row
/// (an inline copy or an `Arc` bump, never an allocation); at the pinned
/// release `SharedString` wraps `smol_str::SmolStr`
/// (`gpui-pre-shared-string-0.3.5/gpui_shared_string.rs`), which stores
/// up to 23 bytes inline, so `SharedString::from(&str)` heap-allocates
/// only for a longer string — it had no inline form at the old git rev —
/// but an underlying key can exceed that, and the charter's "nothing
/// allocates in render" is about the rule, not the byte count, so the
/// conversion still happens once per row when the catalog changes, never
/// per frame (as the first build did on every repaint while a picker was
/// open).
///
/// `marks` (2026-09-19, per-underlying drafts): the underlyings that
/// carry PARKED edits, keyed by `display_key` and valued with the
/// draft's own `count_phrase` ("1 cell, spot_ref"), taken once at open
/// from the tile's parked map. A marked row's label reads `NKY.Z · 1
/// cell, spot_ref`; ranking still runs over the bare key, never the
/// decorated label, so typing `sp` cannot match a phrase's own letters.
/// Kept here so [`Self::replace_all`] can re-decorate a fresh catalog
/// without asking the tile again — the parked map only changes through
/// `set_key`, and both of its callers (`picker_pick`, the `:` line)
/// close this popup before calling it, so the marks can never go stale
/// while the picker is open; a third caller would owe the same.
pub(crate) struct PickerRows {
    /// The ranking and the highlight (spec 2026-09-19 §3.1): one core
    /// with the dialogs' choice fields, so the cap, the identity rule
    /// and the re-rank guard are spelled once.
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

    /// Re-rank against `new_query`, a no-op when it is unchanged from the
    /// query the ranking was last built against — so a defensive re-rank
    /// at commit time (the field's current text may never have reached
    /// this struct through a real `Change` event) costs nothing when
    /// nothing changed, and never resets the highlight out from under a
    /// trader who typed nothing at all
    /// ([`geode_shell::choice::ChoiceList::set_query`]'s own doc comment
    /// has the full story, including the keep-by-text rule review fix
    /// round 1, CRITICAL, established).
    pub(crate) fn refilter(&mut self, new_query: &str) {
        self.list.set_query(new_query);
    }

    /// Swap in a fresh catalog (the diagnostics observer's door), keeping
    /// the highlighted KEY across it —
    /// [`geode_shell::choice::ChoiceList::replace_options`]'s own
    /// identity-by-string rule (review fix round 2: the key is captured
    /// BEFORE the option list is overwritten, and by string rather than
    /// by index — `catalog_keys()` returns a freshly SORTED list, so a
    /// new underlying that sorts ahead of the highlighted one shifts
    /// every later index, and re-placing by the OLD index once the
    /// catalog has already changed would silently highlight a different
    /// row).
    pub(crate) fn replace_all(&mut self, all: Vec<String>) {
        self.labels = Self::labels_for(&all, &self.marks);
        self.list.replace_options(all);
    }

    /// Rebuild the ranking against the CURRENT catalog/query, then put
    /// the highlight on `key` — falling back to row 0 when `key` is
    /// `None` or no longer present in the catalog at all. Unlike the old
    /// truncating cap, a `key` that ranks past `PICKER_ROWS` is still
    /// found and lit, with the window dragged along so it is actually
    /// visible ([`geode_shell::choice::ChoiceList::place`]'s own doc
    /// comment).
    ///
    /// **Identity is the KEY STRING, never a positional index** (review
    /// fix round 2, the bug the first fix's own `rerank` reintroduced by
    /// capturing `was_highlighted` as an ALL-index and looking that same
    /// number up in the NEW catalog): see [`Self::replace_all`]. A
    /// test-only door directly onto [`geode_shell::choice::ChoiceList::place`] —
    /// [`Self::refilter`] and [`Self::replace_all`] go straight to the
    /// list's own `set_query`/`replace_options` in production, which
    /// call it internally.
    #[cfg(test)]
    pub(crate) fn place(&mut self, key: Option<&str>) {
        self.list.place(key);
    }

    /// Move the highlight `delta` steps over the WHOLE ranked list,
    /// clamped at either end (never wrapping — `core::menu::step`'s own
    /// rule, spec §7's `up`/`down`;
    /// [`geode_shell::choice::ChoiceList::nav_clamped`]). The window
    /// follows, so the highlight is always painted even when it steps
    /// past the old cap.
    pub(crate) fn step_highlighted(&mut self, delta: isize) {
        self.list
            .nav_clamped(geode_shell::vimnav::NavCommand::Move(delta as i64));
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
                    // list token — and, as there, never on a disabled
                    // row: the highlight still LANDS on one (`step` does
                    // not skip them, so `enter` can answer with the
                    // reason), but it paints muted, not enabled.
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

/// Paint a `Choice` cell's typeahead (dividend spec §4.4): the picker's
/// own surface — the field on top, one row per painted option below, the
/// lit one in `accent`, a row click picks it, a hover lights it, a click
/// anywhere outside closes it — anchored by its TOP-LEFT corner at the
/// point it is painted from, which `MatrixDelegate::render_td` places at
/// the edited cell's bottom-left, so the list hangs under the cell like
/// a `Select`'s. `deferred` escapes the table's own clip exactly as the
/// header popups escape the tile's, and `snap_to_window_with_margin`
/// keeps a bottom-row popup on screen.
///
/// Reads only the prepared [`ChoicePaint`] (a static-string clone per
/// row): nothing here formats or ranks.
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

    /// The round-1 CRITICAL: an unchanged query is a no-op, so the
    /// defensive re-rank `commit` always makes never resets a highlight
    /// the trader moved.
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

    /// The round-2 fix, pure: a re-sorted catalog keeps the KEY, and a
    /// genuine removal falls back to row 0.
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

    /// Spec §20.5's own rule, on the picker: a bare ±1 step is CLAMPED
    /// here, never wrapping — `step_highlighted` goes through
    /// [`geode_shell::choice::ChoiceList::nav_clamped`], not `nav`, which
    /// is the one thing distinguishing the picker from a dialog's
    /// `Choice` field (whose bare step wraps).
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

    /// A3, amended for the window-following cap (2026-09-19 ruling): the
    /// ranked list is unbounded, but a step that lands the highlight past
    /// the cap drags the WINDOW along rather than clamping the highlight
    /// to whatever the window last showed — `enter` can never load a row
    /// the trader cannot see, even when the current value ranks past row
    /// `PICKER_ROWS`.
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

        // A catalog with a key that sorts ahead of the highlighted one
        // shifts its declared index — the window follows it into view
        // rather than dropping it to row 0 the way the old truncating cap
        // once did.
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
