//! The dimension picker: choose a column, then apply a set of its values.
//! [`PickerState`] owns filtering and ticks; the shell sends a tagged
//! [`ShellEvent::DistinctRequested`] and accepts results through
//! [`ShellView::deliver_distinct`]. Requests omit the selected column's
//! own dimension constraint so its value counts reflect the other filters.
//!
//! Values starts with the current column selection ticked. Tab toggles the
//! highlight, Ctrl+A adds all shown values, and Ctrl+X clears every tick.
//! Enter applies visible pre-ticks or edited ticks. Only an untouched empty
//! tick set falls back to the highlighted value; if no value is highlighted,
//! that empty result removes the column constraint. Loading and query errors
//! do not disable Enter. Row clicks move the highlight; tick clicks toggle.
//!
//! This dialog is filter-only. Escape from Values discards that stage's query
//! and ticks and returns to Columns; Escape there closes the modal. Neither
//! step applies a draft. Closing and reopening starts with fresh state.
//! The Values list uses a uniform-list scroll handle to follow selection.

use std::collections::BTreeSet;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, Hsla, Pixels, ScrollStrategy, UniformListScrollHandle,
    Window, div, uniform_list,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use geode_core::query::DistinctParams;
use geode_core::scope::{DimensionSelection, Scope};

use crate::fonts;
use crate::keymap::{Keystroke, Modifiers};
use crate::palette;
use crate::{listfilter, vimnav};

use super::dialog;
use super::listrow::{self, RowPaint};
use super::scale;
use super::{PICKER_KEY, Pickable, ShellEvent, ShellView};

// ---------------------------------------------------------------------
// Pure core — no gpui.
// ---------------------------------------------------------------------

/// The current stage; Values carries the column being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Columns,
    Values { column: String },
}

/// Pure state for one open picker session.
#[derive(Debug, Clone)]
pub struct PickerState {
    pub stage: Stage,
    /// Index into the current filtered list: [`PickerState::columns`] or
    /// [`PickerState::shown`].
    pub selected: usize,
    /// Query mirrored from the shared dialog Input's change subscription.
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
    /// Whether a successful toggle, nonempty select-all, or explicit clear
    /// has operated on the ticks. Pre-ticking the current scope leaves this
    /// false. An untouched empty set applies the highlight; a touched empty
    /// set removes the column constraint.
    pub ticks_touched: bool,
    /// Latest distinct-request tag, allocated from the shell-wide counter.
    /// [`ShellView::deliver_distinct`] rejects outcomes with an older tag.
    pub tag: u64,
}

impl PickerState {
    /// Fuzzy-ranked column-name indices and matched character offsets.
    /// An empty query preserves schema order.
    pub fn columns(pickable: &[Pickable], query: &str) -> Vec<(usize, Vec<usize>)> {
        let texts: Vec<String> = pickable.iter().map(|p| p.column.clone()).collect();
        listfilter::rank(&texts, query)
            .into_iter()
            .map(|r| (r.row, r.indices))
            .collect()
    }

    /// Fuzzy-ranked value indices and matched character offsets; empty while
    /// loading or failed. Equal scores preserve the incoming value order.
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
        // A key with no selectable row must leave highlight fallback armed.
        self.ticks_touched = true;
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
        if shown.is_empty() {
            return; // see `toggle_selected` on why this is not a touch
        }
        self.ticks_touched = true;
        for (idx, _) in &shown {
            self.ticked.insert(values[*idx].0.clone());
        }
    }

    /// Clear every tick (`ctrl+x`) — the whole set, not just what the
    /// filter currently shows.
    ///
    /// Unlike its two siblings this counts as a touch even when it
    /// changes nothing: `ctrl+x` on an already-empty set is still the
    /// user saying "select nothing here", and `apply` must honour that by
    /// dropping the column rather than committing the highlight.
    pub fn clear(&mut self) {
        self.ticks_touched = true;
        self.ticked.clear();
    }

    /// The value the `Values` stage is highlighting, or `None` when
    /// nothing is shown at `self.selected` (loading, a failed request, an
    /// empty distinct result, or a filter that matched nothing).
    fn highlighted_value(&self) -> Option<String> {
        let shown = self.shown();
        let (idx, _) = shown.get(self.selected)?;
        let Some(Ok(values)) = &self.values else {
            return None;
        };
        values.get(*idx).map(|(v, _)| v.clone())
    }

    /// Replace all selections for this stage's column in the supplied scope.
    /// Apply sorted ticks, or the highlighted value when ticks are untouched
    /// and empty. If the resulting set is empty, remove the column constraint.
    /// Keep the other scope fields, including `impossible`, unchanged.
    /// Columns returns the scope unchanged.
    pub fn apply(&self, scope: &Scope) -> Scope {
        let Stage::Values { column } = &self.stage else {
            return scope.clone();
        };
        let values: Vec<String> = if self.ticked.is_empty() && !self.ticks_touched {
            self.highlighted_value().into_iter().collect()
        } else {
            self.ticked.iter().cloned().collect()
        };
        let mut out = scope.clone();
        out.dimensions.retain(|d| &d.column != column);
        if !values.is_empty() {
            out.dimensions.push(DimensionSelection {
                column: column.clone(),
                values,
            });
        }
        out
    }
}

// ---------------------------------------------------------------------
// gpui shell.
// ---------------------------------------------------------------------

/// Dialog width on the design scale, sized for value and count rows.
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
    // Zero is a placeholder until `request_values` assigns a shell-wide tag.
    // Direct Values opens request synchronously before any result can arrive.
    view.picker = Some(PickerState {
        stage,
        selected: 0,
        query: String::new(),
        values: None,
        ticked: BTreeSet::new(),
        ticks_touched: false,
        tag: 0,
    });
    sync_picker_scroll(view);
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

/// Request distinct values under the current scope and as-of. Clear prior
/// results, allocate a fresh tag, and pre-tick the first current selection
/// for this column. Remove every selection for the column from the request
/// scope so counts measure the effect of each value under the other filters.
fn request_values(view: &mut ShellView, column: &str, cx: &mut Context<ShellView>) {
    // Tags span picker opens, preventing an outcome from a closed picker
    // from matching a new request for the same column.
    view.next_picker_tag += 1;
    let tag = view.next_picker_tag;
    let Some(p) = view.picker.as_mut() else {
        return;
    };
    p.tag = tag;
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
    // Pre-ticks reflect the current scope. Only an empty pre-tick set can
    // use the untouched-highlight fallback on Enter.
    p.ticks_touched = false;
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
    sync_picker_scroll(shell);
    shell.dialog_input.update(cx, |input, cx| {
        input.set_value("", window, cx);
    });
    request_values(shell, &column, cx);
    cx.notify();
}

/// Return Values to Columns, discarding query, results, and ticks and
/// selecting the column just left. No scope change is applied. Columns
/// itself leaves Escape to the shell, which closes the modal.
fn back_to_columns(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(Stage::Values { column }) = shell.picker.as_ref().map(|p| p.stage.clone()) else {
        return;
    };
    let position = shell
        .pickable
        .iter()
        .position(|p| p.column == column)
        .unwrap_or(0);
    if let Some(p) = shell.picker.as_mut() {
        p.stage = Stage::Columns;
        p.selected = position;
        p.query.clear();
        p.values = None;
        p.ticked.clear();
        p.ticks_touched = false;
    }
    shell.dialog_input.update(cx, |input, cx| {
        input.set_value("", window, cx);
    });
    cx.notify();
}

/// Keep the selected Values row visible through its uniform-list handle.
/// Navigation, row clicks, query changes, and stage entry call this.
/// Columns is an unvirtualized list and needs no scroll synchronization.
pub(super) fn sync_picker_scroll(shell: &ShellView) {
    if let Some(picker) = shell.picker.as_ref()
        && let Stage::Values { .. } = &picker.stage
    {
        shell
            .picker_scroll
            .scroll_to_item(picker.selected, ScrollStrategy::Nearest);
    }
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
    if let Some(cmd) = listfilter::nav_command(ks) {
        let query = shell
            .picker
            .as_ref()
            .map(|p| p.query.clone())
            .unwrap_or_default();
        let len = PickerState::columns(&shell.pickable, &query).len();
        if let Some(p) = shell.picker.as_mut() {
            p.selected = vimnav::apply(p.selected, len, cmd);
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
    // Modifier-agnostic, like the shell's own modal close: a
    // `shift+escape` must not be a key this stage claims and drops.
    if ks.key == "escape" {
        back_to_columns(shell, window, cx);
        return true;
    }
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
        // Apply closes through the same cleanup and focus-return path as cancel.
        shell.close_modal(window, cx);
        return true;
    }
    if let Some(cmd) = listfilter::nav_command(ks) {
        let len = shell.picker.as_ref().map(|p| p.shown().len()).unwrap_or(0);
        if let Some(p) = shell.picker.as_mut() {
            p.selected = vimnav::apply(p.selected, len, cmd); // values
        }
        sync_picker_scroll(shell);
        cx.notify();
        return true;
    }
    false
}

/// Dispatch keys by stage. Escape returns Values to Columns, then the
/// shell closes from Columns. This filter-only dialog has no Normal mode.
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

/// A footer key chip or prose fragment, kept as data for rendering and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    Key(&'static str),
    Text(&'static str),
}

/// The keyboard operations advertised by each stage's footer.
pub fn hints(stage: &Stage) -> &'static [Hint] {
    match stage {
        Stage::Columns => &[
            Hint::Text("type to filter ·"),
            Hint::Key("up"),
            Hint::Key("down"),
            Hint::Text("move ·"),
            Hint::Key("enter"),
            Hint::Text("open ·"),
            Hint::Key("escape"),
            Hint::Text("close"),
        ],
        Stage::Values { .. } => &[
            Hint::Text("type to filter ·"),
            Hint::Key("up"),
            Hint::Key("down"),
            Hint::Text("move ·"),
            Hint::Key("tab"),
            Hint::Text("select ·"),
            Hint::Key("ctrl+a"),
            Hint::Text("all ·"),
            Hint::Key("ctrl+x"),
            Hint::Text("clear ·"),
            Hint::Key("enter"),
            Hint::Text("apply ·"),
            Hint::Key("escape"),
            Hint::Text("back"),
        ],
    }
}

/// Distinguish a filter with no matches, a missing datasets document, and
/// a loaded schema with no categorical columns. Only the first is fixed by
/// editing the query; only the second should name a missing document.
fn columns_empty_message(pickable_is_empty: bool, has_datasets_doc: bool) -> &'static str {
    match (pickable_is_empty, has_datasets_doc) {
        (false, _) => "no matches",
        (true, false) => "nothing to pick — no datasets config is loaded",
        (true, true) => "nothing to pick — this schema declares no categorical columns",
    }
}

/// The `Values` stage's twin of [`columns_empty_message`]: a distinct
/// query that came back with no rows at all is a fact about the data
/// under the current scope, not about the filter the user typed.
fn values_empty_message(values_is_empty: bool) -> &'static str {
    if values_is_empty {
        "no values in scope"
    } else {
        "no matches"
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

/// Column rows show name, role, and datasets. Clicking commits the column,
/// using the same transition as Enter.
fn build_columns(
    shell: &ShellView,
    picker: &PickerState,
    entity: &Entity<ShellView>,
    row_paint: RowPaint,
    muted: Hsla,
    radius: Pixels,
) -> AnyElement {
    let matches = PickerState::columns(&shell.pickable, &picker.query);
    if matches.is_empty() {
        return empty_row(
            columns_empty_message(
                shell.pickable.is_empty(),
                shell.services.config.doc("datasets").is_some(),
            ),
            muted,
        );
    }
    let mut list = v_flex()
        .id("picker-columns")
        .w(scale::design(WIDTH))
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
            .rounded(radius);
        if is_selected {
            row = row.bg(row_paint.active).text_color(row_paint.text);
        } else {
            row = row.hover(|s| s.bg(row_paint.hover));
        }
        let datasets = if p.datasets.is_empty() {
            "derived".to_string()
        } else {
            p.datasets.join(", ")
        };
        let label = h_flex()
            .gap_2()
            .items_center()
            .child(div().child(palette::highlighted_title(
                &p.column,
                indices,
                row_paint.accent,
            )))
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

/// Render loading, the query error, or virtualized value/count rows.
/// Compute the filtered indices once per render and move them and the values
/// into the list closure, avoiding repeated picker reads for each range.
fn build_values(
    picker: &PickerState,
    entity: &Entity<ShellView>,
    row_paint: RowPaint,
    muted: Hsla,
    radius: Pixels,
    scroll_handle: &UniformListScrollHandle,
) -> AnyElement {
    match &picker.values {
        None => empty_row("loading…", muted),
        Some(Err(e)) => empty_row(e, muted),
        Some(Ok(values)) => {
            let shown = picker.shown();
            let count = shown.len();
            if count == 0 {
                return empty_row(values_empty_message(values.is_empty()), muted);
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
                            .rounded(radius);
                        if is_selected {
                            row = row.bg(row_paint.active).text_color(row_paint.text);
                        } else {
                            row = row.hover(|s| s.bg(row_paint.hover));
                        }
                        let value_for_tick = value.clone();
                        let tick_entity = entity.clone();
                        let tick = if is_ticked {
                            div().text_color(row_paint.accent).child("✓")
                        } else {
                            div().text_color(muted).child("·")
                        }
                        .debug_selector(move || format!("picker-tick-{value_for_tick}"))
                        // The tick toggles; the rest of the row only selects.
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            move |_event, _window, cx| {
                                cx.stop_propagation();
                                tick_entity.update(cx, |shell, cx| {
                                    if let Some(p) = shell.picker.as_mut() {
                                        p.selected = i;
                                        p.toggle_selected();
                                    }
                                    sync_picker_scroll(shell);
                                    cx.notify();
                                });
                            },
                        );
                        let label =
                            h_flex().gap_2().items_center().child(tick).child(
                                palette::highlighted_title(value, indices, row_paint.accent),
                            );
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
                                        // row: select only
                                        p.selected = i;
                                    }
                                    sync_picker_scroll(shell);
                                    cx.notify();
                                });
                            })
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            })
            .h(scale::design(
                (count.min(palette::VISIBLE_ROWS) as f32) * palette::ROW_HEIGHT,
            ))
            .w(scale::design(WIDTH))
            .track_scroll(scroll_handle)
            .debug_selector(|| "picker-values-list".to_string());
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
    let row_paint = listrow::row_paint(theme);
    let muted = theme.muted_foreground;
    let radius = theme.radius;

    let filter = dialog::filter_row(&shell.dialog_input, None, cx);
    let body = match &picker.stage {
        Stage::Columns => build_columns(shell, picker, entity, row_paint, muted, radius),
        Stage::Values { .. } => build_values(
            picker,
            entity,
            row_paint,
            muted,
            radius,
            &shell.picker_scroll,
        ),
    };

    v_flex()
        .gap_2()
        .w(scale::design(WIDTH))
        .child(filter)
        .child(body)
        .child(hint_row(
            hints(&picker.stage),
            "picker-hints",
            WIDTH,
            theme.muted_foreground,
            theme.border,
        ))
        .into_any_element()
}

/// Render footer hints as shared key chips interleaved with muted prose.
/// Choice dialogs reuse this helper with their own selector and width.
pub(crate) fn hint_row(
    hints: &[Hint],
    selector: &'static str,
    width: f32,
    fg: Hsla,
    border: Hsla,
) -> AnyElement {
    let children: Vec<AnyElement> = hints
        .iter()
        .map(|hint| match hint {
            Hint::Key(spec) => {
                let ks = crate::keymap::parse_keystroke(spec, Modifiers::NONE)
                    .expect("footer hint keystrokes are hardcoded valid");
                super::kbd::chip(&ks).into_any_element()
            }
            Hint::Text(text) => div().child(*text).into_any_element(),
        })
        .collect();
    div()
        .id(selector)
        .w(scale::design(width))
        .pt_2()
        .border_t_1()
        .border_color(border)
        .text_sm()
        .text_color(fg)
        .debug_selector(move || selector.to_string())
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .flex_wrap()
                .children(children),
        )
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
            ticks_touched: false,
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

    /// Unusable toggle/select-all keys leave untouched-highlight commit enabled.
    #[test]
    fn a_tick_keystroke_that_ticks_nothing_does_not_count_as_touching() {
        // `tab` with a filter that matches nothing.
        let mut s = state_with(&[("BK000", 1), ("BK001", 2)]);
        s.query = "zzzz".into();
        assert!(s.shown().is_empty(), "the fixture must match nothing");
        s.toggle_selected();
        assert!(!s.ticks_touched, "a tab that ticked nothing is not a touch");

        // `ctrl+a` with the same empty shown set.
        s.tick_all_shown();
        assert!(!s.ticks_touched, "ctrl+a over nothing is not a touch");

        // `tab` before the distinct query returns.
        let mut loading = state_with(&[]);
        loading.values = None;
        loading.toggle_selected();
        loading.tick_all_shown();
        assert!(!loading.ticks_touched, "no values yet is not a touch");

        // …so the highlight still commits once the filter is cleared.
        s.query.clear();
        s.selected = 1;
        let out = s.apply(&Scope::default());
        assert_eq!(out.dimensions.len(), 1, "the highlight still commits");
        assert_eq!(out.dimensions[0].values, vec!["BK001".to_string()]);
    }

    /// An untouched empty tick set applies the highlighted value.
    #[test]
    fn enter_on_an_untouched_tick_set_commits_the_highlighted_value() {
        let mut s = state_with(&[("BK000", 1), ("BK001", 2), ("BK002", 3)]);
        s.selected = 1;
        let out = s.apply(&Scope::default());
        assert_eq!(out.dimensions.len(), 1, "the highlighted value commits");
        assert_eq!(out.dimensions[0].column, "book");
        assert_eq!(out.dimensions[0].values, vec!["BK001".to_string()]);
    }

    /// Explicitly clearing ticks removes the dimension on Enter.
    #[test]
    fn an_explicit_clear_makes_enter_drop_the_selection() {
        let mut s = state_with(&[("BK000", 1), ("BK001", 2)]);
        s.ticked.insert("BK000".into()); // as `request_values` pre-ticks it
        s.clear(); // ctrl+x
        let base = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        assert!(
            s.apply(&base).dimensions.is_empty(),
            "an explicitly cleared tick set drops the column"
        );
    }

    /// Missing configuration and unmatched filters need distinct empty states.
    #[test]
    fn the_empty_states_say_which_emptiness_it_is() {
        // A filter that matched nothing, whatever the config looks like.
        assert_eq!(columns_empty_message(false, true), "no matches");
        assert_eq!(columns_empty_message(false, false), "no matches");
        assert_eq!(values_empty_message(false), "no matches");
        assert_ne!(values_empty_message(true), values_empty_message(false));

        // Distinguish a missing datasets document from one with no pickable columns.
        let no_doc = columns_empty_message(true, false);
        let no_categoricals = columns_empty_message(true, true);
        assert_ne!(no_doc, "no matches");
        assert_ne!(no_categoricals, "no matches");
        assert_ne!(no_doc, no_categoricals);
        assert!(no_doc.contains("datasets config"), "{no_doc}");
        assert!(
            !no_categoricals.contains("config is loaded"),
            "a schema that IS loaded must not be reported as missing: {no_categoricals}"
        );
    }

    /// The values stage must advertise `tab`: it is the only key that
    /// selects a value, and nothing else on screen says so.
    #[test]
    fn each_stage_advertises_its_own_vocabulary() {
        let columns = hints(&Stage::Columns);
        let values = hints(&Stage::Values {
            column: "book".into(),
        });
        assert_ne!(columns, values);
        assert!(values.contains(&Hint::Key("tab")), "{values:?}");
        assert!(values.contains(&Hint::Key("ctrl+x")), "{values:?}");
        assert!(
            !columns.contains(&Hint::Key("tab")),
            "the columns stage has nothing to tick"
        );
        // Every `Hint::Key` must be a spelling `kbd::chip` can render, or
        // `hint_row` panics at paint time on a hardcoded string.
        for hint in columns.iter().chain(values) {
            if let Hint::Key(spec) = hint {
                assert!(
                    crate::keymap::parse_keystroke(spec, Modifiers::NONE).is_ok(),
                    "{spec} must parse"
                );
            }
        }
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

    /// A compact carried-dimension schema fixture. Its declaration order fixes
    /// the expected categorical-column order independently of the demo config.
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

    /// The grouping vocabulary is the compiler's, not the picker's: every
    /// declared column some declared grain `carries` — a grain's key
    /// columns and every carried dimension, categorical or not — plus every
    /// derived dimension whose base column is itself groupable. A
    /// categorical *attribute* (`expiry`) is pickable but no grain carries
    /// it as a dimension, so grouping by it would fail to compile and it
    /// is not offered — nor is a derived dimension over it (`bucket`),
    /// which the compiler resolves through `dims.base_column` to the same
    /// refusal; a key column is the reverse.
    #[test]
    fn groupable_columns_are_every_carried_dimension_key_included_plus_derived() {
        let datasets = format!(
            "{CARRIED_DEMO}\n[risk_snapshot.columns.strike]\ntype = \"f64\"\nrole = \"dimension\"\ngrain = \"instrument\"\n"
        );
        let config = config_from(&[
            ("datasets", &datasets),
            (
                "dimensions",
                "desk = { from = \"book\", values = { BK000 = \"Flow\" } }\n\
                 bucket = { from = \"expiry\", values = { \"2026-12\" = \"Q4\" } }",
            ),
        ]);
        let g = super::super::groupable_columns(&config);
        let names: Vec<(&str, &str)> = g.iter().map(|c| (c.column.as_str(), c.role)).collect();
        assert_eq!(
            names,
            vec![
                ("book", "dimension"),
                ("lhu", "dimension"),
                ("position_ref", "key"),
                ("counterparty", "dimension"),
                ("instrument_ref", "key"),
                ("underlying_ref", "dimension"),
                ("currency", "dimension"),
                ("strike", "dimension"),
                ("desk", "derived"),
            ]
        );
        assert_eq!(g[2].datasets, vec!["risk_snapshot"]);
    }

    /// A dataset with no declared grain (no measure or attribute) has no
    /// table the compiler could scan, so it contributes nothing — the same
    /// `finest_carrying` rule that would refuse the query.
    #[test]
    fn a_dataset_declaring_no_grain_contributes_no_groupable_columns() {
        let config = config_from(&[(
            "datasets",
            "[bare.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
        )]);
        assert!(super::super::groupable_columns(&config).is_empty());
    }
}
