//! The two-stage dimension picker (Phase 4a spec §3.3, §3.4, §3.11): a
//! keyed modal in [`keybindings_view`](super::keybindings_view)'s mould —
//! same "pure core + gpui shell" split, same
//! [`dialog::open_shell_dialog_with_key`] door — that lets the user pick a
//! column (`Stage::Columns`) and then a set of its values (`Stage::
//! Values`), fed by `Request::Distinct` through the app bridge
//! (`geode-shell` cannot depend on `geode-data`, so the request leaves as
//! [`ShellEvent::DistinctRequested`] and the outcome comes back through
//! [`ShellView::deliver_distinct`]).
//!
//! ## Two amendments to the spec (recorded here, to land in spec §3.3 in
//! Task 9)
//!
//! Keys: `tab` toggles the highlighted value — a printable `space` would
//! be typed into the filter input instead, and `tab` is already reclaimed
//! to `NoAction` inside every Geode modal
//! (`dialog::init_reclaimed_keybindings`). "Clear all" is `ctrl+x`, not
//! `ctrl+c` or a bare "clear" key — `ctrl+n` is already "down" on every
//! list surface in this shell, and `ctrl+x` reads as "cut everything",
//! pairing naturally with `ctrl+a` ("tick every value the filter
//! currently shows" — also reclaimed, see
//! [`super::dialog::init_reclaimed_keybindings`]'s own doc comment,
//! bullet 3).
//!
//! ## Architecture
//!
//! [`PickerState`] — `stage`, `selected`, `query`, `values`, `ticked`,
//! `tag` — is pure (no `gpui`), stored on `ShellView` as `picker:
//! Option<PickerState>`, exactly like `palette`/`keybindings`/`settings`.
//! Unlike those three, it has no sibling `gpui::ScrollHandle`: the values
//! list is a `uniform_list` (self-virtualizing; see [`build_values`]),
//! which needs none.
//!
//! [`open`] is the only entry point (`frame::pick`/`frame::pick_<column>`,
//! a chip body click) and the only place a `PickerState` is constructed —
//! nothing survives a close/reopen, the same contract every other modal
//! here keeps. [`request_values`] is what turns "the values stage just
//! opened" into a `ShellEvent::DistinctRequested` — it also pre-ticks
//! whatever the scope already selects for that column, so opening the
//! picker on an already-scoped dimension shows its current selection
//! rather than a blank slate.

use std::collections::BTreeSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Hsla, Window, div, px, uniform_list};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use geode_core::query::DistinctParams;
use geode_core::scope::{DimensionSelection, Scope};

use crate::fonts;
use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;
use crate::palette;

use super::dialog;
use super::{PICKER_KEY, Pickable, ShellEvent, ShellView};

// ---------------------------------------------------------------------
// Pure core — no gpui.
// ---------------------------------------------------------------------

/// Which half of the picker is showing. `Values` carries the column it's
/// showing values for — the reason this whole modal exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Columns,
    Values { column: String },
}

/// Persistent state for one open picker session — the analogue of
/// `KeybindingsState`/`SettingsState`. Holds no `gpui` types, so every
/// transition is unit-testable without a window (see the `tests` module
/// below).
#[derive(Debug, Clone)]
pub struct PickerState {
    pub stage: Stage,
    /// Index into the **filtered** list for the current stage —
    /// [`PickerState::columns`] on `Columns`, [`PickerState::shown`] on
    /// `Values`. The palette's own convention.
    pub selected: usize,
    /// The dialog filter's query, mirrored here from `ShellView::
    /// dialog_input`'s `InputEvent::Change` subscription — the same
    /// routing the keybinding/settings dialogs already use (`shell/mod.rs`'s
    /// own subscription, set up once in `ShellView::new`).
    pub query: String,
    /// `None` while a `Values`-stage request is in flight (or the stage
    /// hasn't requested anything yet); `Some(Ok(..))` once
    /// [`ShellView::deliver_distinct`] delivers a fresh result;
    /// `Some(Err(..))` on a query failure. Never populated on the
    /// `Columns` stage — nothing has been requested yet.
    pub values: Option<Result<Vec<(String, u64)>, String>>,
    /// Values ticked so far — a `BTreeSet` both for de-duplication and so
    /// `apply`'s `Scope::dimensions` selection comes out in a stable,
    /// sorted order regardless of tick order.
    pub ticked: BTreeSet<String>,
    /// Bumped once per [`request_values`] call; a `DistinctOutcome`
    /// whose tag doesn't match the *latest* bump is stale and dropped
    /// (§7.3) — see [`ShellView::deliver_distinct`].
    pub tag: u64,
}

impl PickerState {
    /// Filtered indices into `pickable`, plus each match's char offsets,
    /// best match first — [`crate::listfilter::rank`] over the column
    /// names, the same fuzzy-filter rule (and empty-query "keep schema
    /// order") both list dialogs and the palette already share.
    pub fn columns(pickable: &[Pickable], query: &str) -> Vec<(usize, Vec<usize>)> {
        let texts: Vec<String> = pickable.iter().map(|p| p.column.clone()).collect();
        listfilter::rank(&texts, query)
            .into_iter()
            .map(|r| (r.row, r.indices))
            .collect()
    }

    /// Filtered indices into `self.values`'s value list, plus each
    /// match's char offsets, best match first — empty when `self.values`
    /// isn't `Some(Ok(..))` (loading, or a failed request has nothing to
    /// show). `DistinctOutcome::values` is already sorted by value, so a
    /// non-empty query's stable sort-by-score keeps that order as the
    /// tie-break for free — no separate secondary sort key needed.
    pub fn shown(&self) -> Vec<(usize, Vec<usize>)> {
        let Some(Ok(values)) = &self.values else {
            return Vec::new();
        };
        let texts: Vec<String> = values.iter().map(|(v, _)| v.clone()).collect();
        listfilter::rank(&texts, &self.query)
            .into_iter()
            .map(|r| (r.row, r.indices))
            .collect()
    }

    /// Toggle the tick on the currently-selected **shown** value — a
    /// no-op if there's nothing shown at `self.selected` (an empty list,
    /// or a stale selection past its end).
    pub fn toggle_selected(&mut self) {
        let shown = self.shown();
        let Some((idx, _)) = shown.get(self.selected) else {
            return;
        };
        let idx = *idx;
        let Some(Ok(values)) = &self.values else {
            return;
        };
        let value = values[idx].0.clone();
        if !self.ticked.remove(&value) {
            self.ticked.insert(value);
        }
    }

    /// Tick every value the filter currently shows (`ctrl+a`) — additive,
    /// never clears an existing tick outside the shown set.
    pub fn tick_all_shown(&mut self) {
        let shown = self.shown();
        let Some(Ok(values)) = &self.values else {
            return;
        };
        for (idx, _) in &shown {
            self.ticked.insert(values[*idx].0.clone());
        }
    }

    /// Clear every tick (`ctrl+x`) — the whole set, not just what the
    /// filter currently shows.
    pub fn clear(&mut self) {
        self.ticked.clear();
    }

    /// Move the selection by `delta` (±1 for up/down/ctrl+p/ctrl+n),
    /// wrapping at both ends — `PaletteState::move_selection`'s own rule,
    /// not the dialogs' clamping one: this is a short, per-keystroke list
    /// the user steps through quickly, same as the palette.
    pub fn move_selection(&mut self, delta: i32, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        let next = ((self.selected as i32 + delta) % len as i32 + len as i32) % len as i32;
        self.selected = next as usize;
    }

    /// Replace `scope`'s selection for this stage's column with whatever
    /// is ticked — an empty tick set drops the column's selection
    /// entirely rather than writing an empty `DimensionSelection` (spec:
    /// empty `values` already means "no constraint", so a dropped
    /// selection and an explicit "everything" selection must not be
    /// conflated). A no-op clone of `scope` on the `Columns` stage — there
    /// is no column to apply anything to yet.
    pub fn apply(&self, scope: &Scope) -> Scope {
        let Stage::Values { column } = &self.stage else {
            return scope.clone();
        };
        let mut out = scope.clone();
        out.dimensions.retain(|d| &d.column != column);
        if !self.ticked.is_empty() {
            out.dimensions.push(DimensionSelection {
                column: column.clone(),
                values: self.ticked.iter().cloned().collect(),
            });
        }
        out
    }
}

// ---------------------------------------------------------------------
// gpui shell.
// ---------------------------------------------------------------------

/// Target picker panel width in pixels — between the palette's 560px and
/// the two list dialogs' 640px, since the values list's rows (a tick, a
/// value, a right-aligned count) are narrower than either.
const WIDTH: f32 = 480.0;

/// Open the picker (`frame::pick` with `column: None`, `frame::
/// pick_<column>` or a chip body click with `column: Some(..)`). A no-op
/// if a modal is already open, mirroring every other `open` here.
/// `column` names an unrecognised column (stale palette state from before
/// a reload dropped it, say) falls back to the `Columns` stage rather than
/// opening on a column that no longer exists.
pub fn open(
    view: &mut ShellView,
    column: Option<String>,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if view.modal.is_some() {
        return;
    }
    let stage = match column {
        Some(c) if view.pickable.iter().any(|p| p.column == c) => Stage::Values { column: c },
        Some(_) | None => Stage::Columns,
    };
    view.picker = Some(PickerState {
        stage,
        selected: 0,
        query: String::new(),
        values: None,
        ticked: BTreeSet::new(),
        tag: 0,
    });
    if let Some(Stage::Values { column }) = view.picker.as_ref().map(|p| p.stage.clone()) {
        request_values(view, &column, cx);
    }
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        "Pick",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
}

/// Submit a fresh `Request::Distinct` for `column` (spec §3.4): bumps the
/// picker's tag, clears any previous values (back to "loading…"),
/// pre-ticks whatever the current scope already selects for `column` (so
/// re-opening an already-scoped dimension shows its live selection), and
/// emits [`ShellEvent::DistinctRequested`] with that column's own
/// selection removed from the scope the request carries — the query asks
/// "how many rows would each value leave, ignoring what this column
/// itself currently narrows to", which is what makes a value's count
/// meaningful rather than self-referential.
fn request_values(view: &mut ShellView, column: &str, cx: &mut Context<ShellView>) {
    let Some(p) = view.picker.as_mut() else {
        return;
    };
    p.tag += 1;
    p.values = None;
    // Pre-tick the current selection.
    let (scope, as_of) = {
        let f = view.frame.read(cx);
        (f.scope().clone(), f.as_of().clone())
    };
    p.ticked = scope
        .dimensions
        .iter()
        .find(|d| d.column == column)
        .map(|d| d.values.iter().cloned().collect())
        .unwrap_or_default();
    let mut minus_own = scope;
    minus_own.dimensions.retain(|d| d.column != column);
    let tag = p.tag;
    cx.emit(ShellEvent::DistinctRequested(DistinctParams {
        key: PICKER_KEY,
        tag,
        column: column.to_string(),
        scope: minus_own,
        as_of,
    }));
}

/// Move from `Columns` to `Values` on the column at filtered position
/// `position` — shared by [`handle_key`]'s `enter` arm and a column row's
/// mouse click, so both go through exactly one path: clear the shared
/// dialog filter (a fresh query for the new stage), switch the stage, and
/// submit the values request. A stale `position` (past the end of the
/// current filtered list) is a silent no-op.
fn commit_column(
    shell: &mut ShellView,
    position: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let query = shell
        .picker
        .as_ref()
        .map(|p| p.query.clone())
        .unwrap_or_default();
    let matches = PickerState::columns(&shell.pickable, &query);
    let Some((row_ix, _)) = matches.get(position) else {
        return;
    };
    let column = shell.pickable[*row_ix].column.clone();
    if let Some(p) = shell.picker.as_mut() {
        p.stage = Stage::Values {
            column: column.clone(),
        };
        p.selected = 0;
    }
    shell.dialog_input.update(cx, |input, cx| {
        input.set_value("", window, cx);
    });
    request_values(shell, &column, cx);
    cx.notify();
}

/// `up`/`down`/`ctrl+p`/`ctrl+n` as a signed step, or `None` for anything
/// else — shared by both stages' [`handle_key`] arms.
fn nav_delta(ks: &Keystroke) -> Option<i32> {
    if ks.mods == Modifiers::NONE && ks.key == "up" {
        return Some(-1);
    }
    if ks.mods == Modifiers::NONE && ks.key == "down" {
        return Some(1);
    }
    if ks.mods == Modifiers::CTRL && ks.key == "p" {
        return Some(-1);
    }
    if ks.mods == Modifiers::CTRL && ks.key == "n" {
        return Some(1);
    }
    None
}

fn handle_columns_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        let position = shell.picker.as_ref().map(|p| p.selected).unwrap_or(0);
        commit_column(shell, position, window, cx);
        return true;
    }
    if let Some(delta) = nav_delta(ks) {
        let query = shell
            .picker
            .as_ref()
            .map(|p| p.query.clone())
            .unwrap_or_default();
        let len = PickerState::columns(&shell.pickable, &query).len();
        if let Some(p) = shell.picker.as_mut() {
            p.move_selection(delta, len);
        }
        cx.notify();
        return true;
    }
    false
}

fn handle_values_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if ks.mods == Modifiers::NONE && ks.key == "tab" {
        if let Some(p) = shell.picker.as_mut() {
            p.toggle_selected();
        }
        cx.notify();
        return true;
    }
    if ks.mods == Modifiers::CTRL && ks.key == "a" {
        if let Some(p) = shell.picker.as_mut() {
            p.tick_all_shown();
        }
        cx.notify();
        return true;
    }
    if ks.mods == Modifiers::CTRL && ks.key == "x" {
        if let Some(p) = shell.picker.as_mut() {
            p.clear();
        }
        cx.notify();
        return true;
    }
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        let scope = shell.frame.read(cx).scope().clone();
        let new_scope = shell.picker.as_ref().map(|p| p.apply(&scope));
        if let Some(new_scope) = new_scope {
            shell.frame.update(cx, |f, cx| {
                if f.set_scope(new_scope) {
                    cx.notify();
                }
            });
        }
        shell.close_modal(window, cx); // the existing close path used by escape
        return true;
    }
    if let Some(delta) = nav_delta(ks) {
        let len = shell.picker.as_ref().map(|p| p.shown().len()).unwrap_or(0);
        if let Some(p) = shell.picker.as_mut() {
            p.move_selection(delta, len);
        }
        cx.notify();
        return true;
    }
    false
}

/// The [`dialog::ModalKeyHandler`] for this modal — dispatches to
/// [`handle_columns_key`]/[`handle_values_key`] by the open picker's
/// current stage. `escape` is claimed by neither arm (both fall through
/// their final `false`), so it reaches `handle_key_down`'s own modal
/// branch, which closes the modal exactly as it does for every other
/// dialog — and `ShellView::close_modal` clears `self.picker`, so an
/// escaped picker leaves nothing behind.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let Some(stage) = shell.picker.as_ref().map(|p| p.stage.clone()) else {
        return false;
    };
    match stage {
        Stage::Columns => handle_columns_key(shell, ks, window, cx),
        Stage::Values { .. } => handle_values_key(shell, ks, window, cx),
    }
}

fn empty_row(text: &str, muted: Hsla) -> AnyElement {
    div()
        .px_2()
        .py_1()
        .text_sm()
        .text_color(muted)
        .child(text.to_string())
        .into_any_element()
}

/// The `Columns` stage's row list: `column · role · datasets`, the
/// selected row highlighted — the palette's own row styling
/// (`h_flex().justify_between()...`, `theme.selection`/`theme.primary`
/// when selected). A row click commits that column
/// ([`commit_column`]), the mouse-driven twin of `enter`.
fn build_columns(
    shell: &ShellView,
    picker: &PickerState,
    entity: &Entity<ShellView>,
    primary: Hsla,
    muted: Hsla,
    selection: Hsla,
) -> AnyElement {
    let matches = PickerState::columns(&shell.pickable, &picker.query);
    if matches.is_empty() {
        return empty_row("no matches", muted);
    }
    let mut list = v_flex()
        .id("picker-columns")
        .w(px(WIDTH))
        .gap_1()
        .debug_selector(|| "picker-columns".to_string());
    for (position, (row_ix, indices)) in matches.iter().enumerate() {
        let p = &shell.pickable[*row_ix];
        let is_selected = position == picker.selected;
        let mut row = h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .gap_3()
            .px_2()
            .py_1()
            .rounded(px(4.));
        if is_selected {
            row = row.bg(selection).text_color(primary);
        }
        let datasets = if p.datasets.is_empty() {
            "derived".to_string()
        } else {
            p.datasets.join(", ")
        };
        let label = h_flex()
            .gap_2()
            .items_center()
            .child(div().child(palette::highlighted_title(&p.column, indices, primary)))
            .child(div().text_color(muted).child(p.role.to_string()));
        let meta = div().text_color(muted).child(datasets);

        let entity = entity.clone();
        let row = row
            .child(label)
            .child(meta)
            .debug_selector(move || format!("picker-column-{}", p.column))
            .on_mouse_down(gpui::MouseButton::Left, move |_event, window, cx| {
                entity.update(cx, |shell, cx| {
                    commit_column(shell, position, window, cx);
                });
            });
        list = list.child(row);
    }
    list.into_any_element()
}

/// The `Values` stage's body: `loading…`, the error text, or a
/// `uniform_list` of `[tick] value    count` rows — the tick is `✓` in
/// `theme.primary` when ticked, a muted `·` otherwise; the count is
/// right-aligned in the mono face (§data face convention). `shown` is
/// computed once here (a `Vec` allocation per render — acceptable in a
/// modal that only re-renders on a keystroke or a delivery, the same
/// concession the palette's own per-keystroke filter makes) and moved
/// into the `uniform_list` closure alongside the value list itself, so
/// the closure stays `'static` without re-reading `picker` from `shell`
/// on every range it's asked to render.
fn build_values(
    picker: &PickerState,
    entity: &Entity<ShellView>,
    primary: Hsla,
    muted: Hsla,
    selection: Hsla,
) -> AnyElement {
    match &picker.values {
        None => empty_row("loading…", muted),
        Some(Err(e)) => empty_row(e, muted),
        Some(Ok(values)) => {
            let shown = picker.shown();
            let count = shown.len();
            if count == 0 {
                return empty_row("no matches", muted);
            }
            let values = Rc::new(values.clone());
            let shown = Rc::new(shown);
            let ticked = picker.ticked.clone();
            let selected = picker.selected;
            let entity = entity.clone();
            let list = uniform_list("picker-values", count, move |range, _window, _cx| {
                range
                    .map(|i| {
                        let (idx, indices) = &shown[i];
                        let (value, n) = &values[*idx];
                        let is_ticked = ticked.contains(value);
                        let is_selected = i == selected;
                        let mut row = h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .gap_3()
                            .px_2()
                            .py_1()
                            .rounded(px(4.));
                        if is_selected {
                            row = row.bg(selection).text_color(primary);
                        }
                        let tick = if is_ticked {
                            div().text_color(primary).child("✓")
                        } else {
                            div().text_color(muted).child("·")
                        };
                        let label = h_flex()
                            .gap_2()
                            .items_center()
                            .child(tick)
                            .child(palette::highlighted_title(value, indices, primary));
                        let count_el = div()
                            .font_family(fonts::MONO)
                            .text_color(muted)
                            .child(n.to_string());
                        let value_for_selector = value.clone();
                        let entity = entity.clone();
                        row.child(label)
                            .child(count_el)
                            .debug_selector(move || format!("picker-value-{value_for_selector}"))
                            .on_mouse_down(gpui::MouseButton::Left, move |_event, _window, cx| {
                                entity.update(cx, |shell, cx| {
                                    if let Some(p) = shell.picker.as_mut() {
                                        p.selected = i;
                                        p.toggle_selected();
                                    }
                                    cx.notify();
                                });
                            })
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            })
            .h(px(
                (count.min(palette::VISIBLE_ROWS) as f32) * palette::ROW_HEIGHT
            ))
            .w(px(WIDTH));
            list.into_any_element()
        }
    }
}

/// The [`dialog::ShellModal::build`] closure body: the shared dialog
/// filter row on top, then either [`build_columns`] or [`build_values`]
/// below it, by the open picker's stage.
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(picker) = shell.picker.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let primary = theme.primary;
    let muted = theme.muted_foreground;
    let selection = theme.selection;

    let filter = dialog::filter_row(&shell.dialog_input, None, cx);
    let body = match &picker.stage {
        Stage::Columns => build_columns(shell, picker, entity, primary, muted, selection),
        Stage::Values { .. } => build_values(picker, entity, primary, muted, selection),
    };

    v_flex()
        .gap_2()
        .w(px(WIDTH))
        .child(filter)
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Config, ConfigSources, LayerDoc};

    fn state_with(values: &[(&str, u64)]) -> PickerState {
        PickerState {
            stage: Stage::Values {
                column: "book".into(),
            },
            selected: 0,
            query: String::new(),
            values: Some(Ok(values
                .iter()
                .map(|(v, n)| (v.to_string(), *n))
                .collect())),
            ticked: BTreeSet::new(),
            tag: 1,
        }
    }

    #[test]
    fn toggling_ticks_and_unticks_the_selected_shown_value() {
        let mut s = state_with(&[("BK000", 1), ("BK001", 2), ("XX", 3)]);
        s.query = "bk".into();
        assert_eq!(s.shown().len(), 2);
        s.selected = 1;
        s.toggle_selected();
        assert!(s.ticked.contains("BK001"));
        s.toggle_selected();
        assert!(s.ticked.is_empty());
    }

    #[test]
    fn tick_all_shown_respects_the_filter_and_clear_empties() {
        let mut s = state_with(&[("BK000", 1), ("BK001", 2), ("XX", 3)]);
        s.query = "bk".into();
        s.tick_all_shown();
        assert_eq!(
            s.ticked.iter().cloned().collect::<Vec<_>>(),
            vec!["BK000", "BK001"]
        );
        s.clear();
        assert!(s.ticked.is_empty());
    }

    #[test]
    fn apply_replaces_the_columns_selection_and_an_empty_tick_set_drops_it() {
        let mut s = state_with(&[("BK000", 1), ("BK001", 2)]);
        let base = Scope {
            dimensions: vec![
                DimensionSelection {
                    column: "lhu".into(),
                    values: vec!["A".into()],
                },
                DimensionSelection {
                    column: "book".into(),
                    values: vec!["OLD".into()],
                },
            ],
            ..Scope::default()
        };
        s.ticked.insert("BK001".into());
        let out = s.apply(&base);
        assert_eq!(out.dimensions.len(), 2);
        assert_eq!(out.dimensions[1].values, vec!["BK001".to_string()]);
        s.clear();
        let out = s.apply(&base);
        assert_eq!(out.dimensions.len(), 1, "book dropped");
        assert_eq!(out.dimensions[0].column, "lhu");
    }

    #[test]
    fn columns_stage_filters_by_fuzzy_match_and_keeps_schema_order_on_empty_query() {
        let p = vec![
            Pickable {
                column: "book".into(),
                role: "dimension",
                datasets: vec!["risk".into()],
            },
            Pickable {
                column: "currency".into(),
                role: "dimension",
                datasets: vec!["risk".into()],
            },
            Pickable {
                column: "desk".into(),
                role: "derived",
                datasets: vec![],
            },
        ];
        assert_eq!(
            PickerState::columns(&p, "")
                .iter()
                .map(|(i, _)| *i)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            PickerState::columns(&p, "cur")
                .iter()
                .map(|(i, _)| *i)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    /// A trimmed dataset shaped like `examples/demo-config/datasets.toml`'s
    /// `risk_snapshot` — the same subset `geode_core::schema`'s own tests
    /// use as `CARRIED` (`book`/`lhu`/`position_ref`/`counterparty`/
    /// `instrument_ref`/`underlying_ref`/`currency`/`expiry`/`npv`/
    /// `delta01`), rather than a literal copy of the full real file: the
    /// full file's column ORDER differs (`underlying_ref` precedes
    /// `counterparty`, `currency` precedes `expiry` there), which would
    /// change `categorical_columns()`'s schema-declaration-order output
    /// and so the exact assertion below — this subset is what reproduces
    /// it. Chosen deliberately over "read the real file at test time"
    /// (the brief's other option) for that reason.
    const CARRIED_DEMO: &str = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk_snapshot.columns.expiry]
type = "utf8"
role = "attribute"
grain = "instrument"
categorical = true
[risk_snapshot.columns.npv]
type = "f64"
role = "measure"
grain = "position"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
"#;

    fn config_from(docs: &[(&str, &str)]) -> Config {
        Config::load(&ConfigSources {
            builtin: docs
                .iter()
                .map(|(name, text)| LayerDoc::builtin(name, text).unwrap())
                .collect(),
            desk: None,
            user: None,
        })
    }

    #[test]
    fn pickable_columns_are_every_categorical_column_plus_derived_dimensions() {
        let config = config_from(&[
            ("datasets", CARRIED_DEMO),
            (
                "dimensions",
                "desk = { from = \"book\", values = { BK000 = \"Flow\" } }",
            ),
        ]);
        let p = super::super::pickable_columns(&config);
        let names: Vec<&str> = p.iter().map(|c| c.column.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "book",
                "lhu",
                "counterparty",
                "underlying_ref",
                "currency",
                "expiry",
                "desk"
            ]
        );
        assert_eq!(p[6].role, "derived");
        assert!(!names.contains(&"position_ref"), "keys are not categorical");
    }
}
