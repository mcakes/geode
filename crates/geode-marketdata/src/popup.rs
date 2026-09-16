//! The tile-owned anchored popup (market-data spec 2026-09-14 §6.1): the
//! panel's own overlay, painted with gpui's `deferred(anchored(..))` so
//! it escapes the tile's own clip and paints above neighbouring tiles —
//! instant, no animation, Geode's own chrome rather than a gpui-component
//! `Dialog` or `Popover`. `Popup::Menu` is Task 6's variant; `Popup::Picker`
//! (Task 7, spec §7) is the underlying picker `u` and the menu row open.

use crate::core::menu::MenuRow;
use crate::tile::MarketDataTile;
use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, Entity, IntoElement, MouseButton, SharedString, anchored,
    deferred, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{Theme, h_flex, v_flex};

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

/// The underlying picker's own state (spec §7): the dataset's catalog
/// keys (`all`, taken once at open — a fresh catalog while the picker
/// stays open is folded back in by the tile's own diagnostics observer,
/// never read here), the current ranking of `all`'s indices
/// (`geode_shell::listfilter::rank`, over `display_key`'s own spelling),
/// and which ranked row is highlighted.
///
/// `all: Vec<String>`, not `Vec<SharedString>`, because
/// [`geode_shell::listfilter::rank`] takes `&[String]`. `labels` is the
/// separate, PREPARED `SharedString` for each `all` entry (review fix
/// round 1, IMPORTANT-3) — `open_picker` and the diagnostics observer
/// both fill it off the render thread, so [`render_picker`] only ever
/// clones an `Arc` per row; `SharedString::from(&str)` is a real
/// allocation (there is no inline small-string form at the pinned rev),
/// so doing that conversion once per row PER FRAME, as the first build
/// did, violated "nothing allocates in render" on every repaint while a
/// picker was open.
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
pub(crate) struct PickerState {
    pub input: Entity<InputState>,
    pub all: Vec<String>,
    pub labels: Vec<SharedString>,
    pub ranked: Vec<usize>,
    pub highlighted: usize,
    pub query: String,
}

impl PickerState {
    /// Re-rank against `new_query`, but ONLY if it actually differs from
    /// the query `ranked` was last built against — a no-op otherwise, so
    /// a defensive re-rank at commit time (the field's current text may
    /// never have reached this struct through a real `Change` event)
    /// costs nothing when nothing changed, and never resets the
    /// highlight out from under a trader who typed nothing at all.
    /// Delegates the real rebuild to [`Self::rerank`], which is also the
    /// door for a re-rank the query itself does NOT gate (the diagnostics
    /// observer's own catalog refresh, review fix round 1, IMPORTANT-2).
    pub(crate) fn refilter(&mut self, new_query: &str) {
        if new_query == self.query {
            return;
        }
        self.query = new_query.to_string();
        self.rerank();
    }

    /// Rebuild `ranked` against the CURRENT `query`, preserving which
    /// KEY was highlighted (by its index into `all`, not its position in
    /// `ranked`) across the rebuild — falling back to the top row only
    /// when that key dropped out of the new ranking entirely. A re-rank
    /// must never silently move the trader's selection, whether it is
    /// triggered by a new query ([`Self::refilter`]) or by `all` itself
    /// changing under an unchanged query (the diagnostics observer).
    pub(crate) fn rerank(&mut self) {
        let was_highlighted = self.ranked.get(self.highlighted).copied();
        self.ranked = geode_shell::listfilter::rank(&self.all, &self.query)
            .into_iter()
            .map(|r| r.row)
            .collect();
        self.highlighted = was_highlighted
            .and_then(|all_index| self.ranked.iter().position(|&r| r == all_index))
            .unwrap_or(0);
    }

    /// Move the highlight `delta` steps over `ranked`, clamped at either
    /// end (never wrapping — `core::menu::step`'s own rule, spec §7's
    /// `up`/`down`).
    pub(crate) fn step_highlighted(&mut self, delta: isize) {
        if self.ranked.is_empty() {
            self.highlighted = 0;
            return;
        }
        let moved = (self.highlighted as isize + delta).clamp(0, self.ranked.len() as isize - 1);
        self.highlighted = moved as usize;
    }
}

/// Paint the action list, anchored at the header's own right edge (spec
/// §6.1). `tile`/`tile_id` are this popup's own mouse door — a row click
/// picks it, exactly as `enter` on the highlighted row would — and a
/// click anywhere outside closes it.
pub(crate) fn render_menu(
    m: &MenuState,
    theme: &Theme,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
) -> impl IntoElement {
    let mut list = v_flex()
        .min_w(px(240.))
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .text_sm()
        .shadow_md()
        .debug_selector(move || format!("marketdata-menu-{tile_id}"))
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, _, cx| tile.update(cx, |t, cx| t.close_popup(cx))
        });
    for (i, row) in m.rows.iter().enumerate() {
        list = list.child(match row {
            MenuRow::Separator => div().h(px(1.)).my_1().bg(theme.border).into_any_element(),
            MenuRow::Section(s) => div()
                .px_3()
                .pt_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(s.clone())
                .into_any_element(),
            MenuRow::Action {
                title,
                hint,
                enabled,
                ..
            } => {
                let disabled = enabled.is_err();
                let reason: SharedString = match enabled {
                    Err(r) => (*r).into(),
                    Ok(()) => hint.clone(),
                };
                h_flex()
                    .px_3()
                    .py_0p5()
                    .justify_between()
                    .gap_4()
                    .when(i == m.highlighted, |d| d.bg(theme.list_active))
                    .text_color(if disabled {
                        theme.muted_foreground
                    } else {
                        theme.popover_foreground
                    })
                    .debug_selector(move || format!("marketdata-menu-row-{tile_id}-{i}"))
                    .on_mouse_down(MouseButton::Left, {
                        let tile = tile.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            tile.update(cx, |t, cx| t.menu_pick(i, window, cx))
                        }
                    })
                    .child(title.clone())
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
    theme: &Theme,
    tile: &Entity<MarketDataTile>,
    tile_id: u64,
) -> impl IntoElement {
    let mut list = v_flex()
        .min_w(px(240.))
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .text_sm()
        .shadow_md()
        .debug_selector(move || format!("marketdata-picker-{tile_id}"))
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
    if p.ranked.is_empty() {
        list = list.child(
            div()
                .px_3()
                .py_0p5()
                .text_color(theme.muted_foreground)
                .child("no underlyings known"),
        );
    } else {
        for (row_i, &i) in p.ranked.iter().enumerate() {
            // `labels[i]` is prepared off-render (`PickerState`'s own
            // doc comment, review fix round 1, IMPORTANT-3) — this is a
            // refcount clone, never a conversion.
            let text = p.labels[i].clone();
            list = list.child(
                h_flex()
                    .px_3()
                    .py_0p5()
                    .when(row_i == p.highlighted, |d| d.bg(theme.list_active))
                    .text_color(theme.popover_foreground)
                    .debug_selector(move || format!("marketdata-picker-row-{tile_id}-{row_i}"))
                    .on_mouse_down(MouseButton::Left, {
                        let tile = tile.clone();
                        move |_, window, cx| {
                            cx.stop_propagation();
                            tile.update(cx, |t, cx| t.picker_pick(row_i, window, cx))
                        }
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
