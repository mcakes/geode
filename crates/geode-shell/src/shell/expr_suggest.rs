//! Suggestions for the scope expression fields: the frame's expression
//! dialog and the Scopes dialog's open `expression` field. Both edit the
//! shared `dialog_input`. The pure state is `crate::exprcomplete`; this
//! module feeds it the live text and caret, claims its keys, requests
//! categorical values under [`super::EXPR_KEY`] and paints it.
//!
//! An accepted row is written as one range replace (select, then
//! replace). Unlike `set_value`, that stays in the input's undo history.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, MouseButton, ScrollHandle, SharedString, Window, div};
use gpui_component::{Theme, h_flex, v_flex};

use geode_core::query::{DistinctOutcome, DistinctParams};
use geode_core::scope::Scope;

use crate::exprcomplete::{ExprCompletion, MAX_ROWS, Refresh};
use crate::keymap::Keystroke;
use crate::listfilter;
use crate::vimnav::NavCommand;

use super::{EXPR_KEY, ShellEvent, ShellView, chip, scale, scope_expr_view};

/// The open expression field's completion, whichever surface holds it.
pub(crate) fn completion_mut(view: &mut ShellView) -> Option<&mut ExprCompletion> {
    if let Some(state) = view.scope_expr_dialog.as_mut() {
        return Some(&mut state.completion);
    }
    let state = view.object_dialog.as_mut()?;
    if !super::objectdialog::expression_entry_open(state) {
        return None;
    }
    Some(state.expr.get_or_insert_with(ExprCompletion::default))
}

/// The scope the finished expression will be ANDed with, which is what
/// the values request is narrowed by.
fn values_scope(view: &ShellView, cx: &App) -> Option<Scope> {
    let current = view.frame.read(cx).scope();
    if let Some(state) = view.scope_expr_dialog.as_ref() {
        return Some(scope_expr_view::request_scope(&state.mode, current));
    }
    let state = view
        .object_dialog
        .as_ref()
        .filter(|state| super::objectdialog::expression_entry_open(state))?;
    match state.domain {
        // A named expression has no enclosing scope: it is ANDed into
        // whichever scope ticks it, so its values are the whole dataset's.
        super::objectdialog::Domain::Expressions => Some(Scope::default()),
        super::objectdialog::Domain::Scopes => {
            let draft = state.draft.as_ref()?;
            let pending = super::objectdialog::apply::config_with_pending(view);
            let config = pending.as_ref().unwrap_or(&view.services.config);
            Some(super::objectdialog::scopes::expression_scope(draft, config))
        }
        _ => None,
    }
}

/// Re-read the field's text and caret from input notifications, on open,
/// after accepting a row, and before Tab inserts one. Unchanged text and
/// caret skip rebuilding the suggestions. This does not run during render.
pub(crate) fn refresh(view: &mut ShellView, cx: &mut Context<ShellView>) {
    // The observer fires for every dialog sharing the input (and every
    // cursor blink); with no expression field open, copy nothing.
    if completion_mut(view).is_none() {
        return;
    }
    let (text, caret) = {
        let input = view.dialog_input.read(cx);
        (input.value().to_string(), input.cursor())
    };
    let vocab = view.expr_vocab.clone();
    let Some(c) = completion_mut(view) else {
        return;
    };
    match c.refresh(&text, caret, &vocab) {
        Refresh::Unchanged => return,
        Refresh::Changed => {}
        Refresh::Request(column) => request_values(view, column, cx),
    }
    view.expr_scroll.scroll_to_item(0);
    cx.notify();
}

fn request_values(view: &mut ShellView, column: String, cx: &mut Context<ShellView>) {
    let Some(scope) = values_scope(view, cx) else {
        return;
    };
    let as_of = view.frame.read(cx).as_of().clone();
    view.next_picker_tag += 1;
    let tag = view.next_picker_tag;
    let vocab = view.expr_vocab.clone();
    let resolved = scope.resolve(view.frame.read(cx).named_expressions());
    let Some(c) = completion_mut(view) else {
        return;
    };
    c.mark_loading(&column, tag, &vocab);
    // An unresolved name is the column's error row, never a request:
    // dropping it would widen the suggestions to values outside the scope.
    let scope = match resolved {
        Ok(scope) => scope,
        Err(message) => {
            c.deliver(&column, tag, Err(message), &vocab);
            return;
        }
    };
    cx.emit(ShellEvent::DistinctRequested(DistinctParams {
        key: EXPR_KEY,
        tag,
        column,
        scope,
        as_of,
    }));
}

/// An `EXPR_KEY` reply. It is dropped when no expression field is open
/// or the tag is not the column's latest.
pub(crate) fn deliver(view: &mut ShellView, outcome: DistinctOutcome, cx: &mut Context<ShellView>) {
    let vocab = view.expr_vocab.clone();
    let Some(c) = completion_mut(view) else {
        return;
    };
    if c.deliver(&outcome.column, outcome.tag, outcome.values, &vocab) {
        cx.notify();
    }
}

/// The suggestion keys, ahead of the field. A bare `tab` inserts the
/// highlighted row; `shift-tab`, `up`/`down` and `ctrl-p`/`ctrl-n` move
/// the highlight. Both tabs are claimed even with nothing to insert, so
/// focus never leaves the field. Every other key is the field's,
/// including the list pages' larger steps (`ctrl-f`/`ctrl-b` and the
/// like), which are caret keys inside a text field.
pub(crate) fn handle_key(
    view: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if completion_mut(view).is_none() {
        return false;
    }
    let delta = if ks.key == "tab" && !ks.mods.is_chord() {
        if !ks.mods.shift {
            // A caret moved since the last observe must be read before
            // the write, or the write would land on a stale range.
            refresh(view, cx);
            let i = completion_mut(view).map_or(0, |c| c.highlighted());
            accept(view, i, window, cx);
            return true;
        }
        -1
    } else {
        match listfilter::nav_command(ks) {
            Some(NavCommand::Move(d)) if d.abs() == 1 => d,
            _ => return false,
        }
    };
    if let Some(c) = completion_mut(view) {
        c.step(delta);
        let h = c.highlighted();
        view.expr_scroll.scroll_to_item(h);
    }
    cx.notify();
    true
}

/// Write ranked row `i` over its token, keep the keyboard in the field,
/// and re-read the new position.
pub(crate) fn accept(
    view: &mut ShellView,
    i: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let Some(write) = completion_mut(view).and_then(|c| c.accept(i)) else {
        return;
    };
    view.dialog_input.update(cx, |s, cx| {
        s.set_selected_range(write.range.clone(), cx);
        s.replace(write.text.clone(), window, cx);
        s.focus(window, cx);
    });
    // The object dialog's draft is its text's source of truth:
    // `sync_dialog_text` runs after the key and would put the old query
    // back unless the draft already holds the new text. A pointer accept
    // passes no key branch, so it syncs here; with the texts equal, the
    // sync writes nothing.
    let text = view.dialog_input.read(cx).value().to_string();
    if let Some(state) = view.object_dialog.as_mut()
        && super::objectdialog::expression_entry_open(state)
        && let Some(draft) = state.draft.as_mut()
    {
        draft.set_query(text);
        super::dialog::sync_dialog_text(view, window, cx);
    }
    refresh(view, cx);
}

/// A pointer accept: write the ranked row labelled `label` as the list
/// stands at the press. The row is found by label, not by its painted
/// position, so a list rebuilt between paint and press never accepts a
/// different row; a label no longer listed does nothing.
pub(crate) fn accept_label(
    view: &mut ShellView,
    label: &str,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let Some(i) = completion_mut(view).and_then(|c| c.rows().iter().position(|r| r.label == label))
    else {
        return;
    };
    accept(view, i, window, cx);
}

/// Row height in design units, scaled with the window rem size.
const ROW_HEIGHT: f32 = 26.0;
/// Maximum rows visible before the suggestion list scrolls.
const VISIBLE_ROWS: usize = 8;

/// The hint line, the ranked rows (or "no matches") and the warning line.
/// Selectors: `scope-expr-hint`, `scope-expr-row-{label}`,
/// `scope-expr-no-matches` and `scope-expr-warning`.
pub(crate) fn render(
    c: &ExprCompletion,
    scroll: &ScrollHandle,
    theme: &Theme,
    on_click: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let paint = super::listrow::row_paint(theme);
    let mut column = v_flex().gap_1().w_full().child(
        div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .debug_selector(|| "scope-expr-hint".to_string())
            .child(SharedString::from(c.hint().to_string())),
    );
    if !c.rows().is_empty() {
        let shown = c.rows().len().min(VISIBLE_ROWS);
        let mut rows = v_flex()
            .id("scope-expr-rows")
            .w_full()
            .h(scale::design(shown as f32 * ROW_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(scroll);
        for (position, row) in c.rows().iter().enumerate().take(MAX_ROWS) {
            let selector = format!("scope-expr-row-{}", row.label);
            let on_click = on_click.clone();
            let label = row.label.clone();
            let element = h_flex()
                .id(SharedString::from(selector.clone()))
                .w_full()
                .h(scale::design(ROW_HEIGHT))
                .flex_shrink_0()
                .px_3()
                .items_center()
                .justify_between()
                .rounded(theme.radius)
                .debug_selector(move || selector.clone())
                .child(div().font_family(crate::fonts::MONO).text_sm().child(
                    super::keybindings_view::highlighted_text(
                        &row.label,
                        &row.indices,
                        paint.accent,
                    ),
                ))
                .child(
                    div()
                        .font_family(crate::fonts::MONO)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(row.detail.clone())),
                )
                .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    // A double-click's second press lands on the list its
                    // first accept rebuilt; it must not accept again.
                    if event.click_count > 1 {
                        return;
                    }
                    on_click(&label, window, cx);
                });
            rows = rows.child(super::listrow::paint_row(
                element,
                paint,
                position == c.highlighted(),
            ));
        }
        column = column.child(rows);
    } else if c.no_matches() {
        column = column.child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "scope-expr-no-matches".to_string())
                .child("no matches"),
        );
    }
    if let Some(warning) = c.warning() {
        column = column.child(
            div()
                .text_sm()
                .text_color(chip::chip_paint(theme, chip::Tone::WarningText).text)
                .debug_selector(|| "scope-expr-warning".to_string())
                .child(SharedString::from(warning.to_string())),
        );
    }
    column.into_any_element()
}
