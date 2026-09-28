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

use crate::exprcomplete::{Accept, ExprCompletion, MAX_ROWS, Refresh, RowKind};
use crate::keymap::Keystroke;
use crate::listfilter;
use crate::vimnav::NavCommand;

use super::dialog::DialogKind;
use super::{EXPR_KEY, ShellEvent, ShellView, chip, scale, scope_expr_view};

/// The live dialog's expression completion. The `dialog_input` observer calls
/// `refresh` on every notify, so a covered dialog's completion must not be
/// returned here, or another dialog's text would recompute it.
pub(crate) fn completion_mut(view: &mut ShellView) -> Option<&mut ExprCompletion> {
    match view.top_kind()? {
        DialogKind::ScopeExpr => {
            let state = view.scope_expr_dialog.as_mut()?;
            // The name entry shares the field; suggesting columns for a name
            // would offer to write an expression into it.
            if state.naming.is_some() {
                return None;
            }
            Some(&mut state.completion)
        }
        DialogKind::Object => {
            let state = view.object_dialog.as_mut()?;
            if !super::objectdialog::expression_entry_open(state) {
                return None;
            }
            Some(state.expr.get_or_insert_with(ExprCompletion::default))
        }
        _ => None,
    }
}

/// The scope the finished expression will be ANDed with, which is what
/// the values request is narrowed by.
fn values_scope(view: &ShellView, cx: &App) -> Option<Scope> {
    match view.top_kind() {
        Some(DialogKind::ScopeExpr) => {
            let current = view.target_frame().read(cx).scope();
            let state = view.scope_expr_dialog.as_ref()?;
            Some(scope_expr_view::request_scope(
                &state.mode,
                current,
                &state.staged,
            ))
        }
        Some(DialogKind::Object) => {
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

/// The top dialog was just revealed. Every expression field shares one pool
/// key, so a covering dialog's field may have replaced this field's request,
/// which then never replies; kept `Loading`, the field would say "loading
/// values…" and `refresh` would never ask again. Values the covering dialog's
/// actions narrowed differently go too. Drop them all and re-read the field.
pub(crate) fn revealed(view: &mut ShellView, cx: &mut Context<ShellView>) {
    let vocab = view.expr_vocab.clone();
    let Some(c) = completion_mut(view) else {
        return;
    };
    c.forget_values(&vocab);
    refresh(view, cx);
}

fn request_values(view: &mut ShellView, column: String, cx: &mut Context<ShellView>) {
    let Some(scope) = values_scope(view, cx) else {
        return;
    };
    let as_of = view.target_frame().read(cx).as_of().clone();
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

/// An `EXPR_KEY` reply. A covered dialog's field still owns its outstanding
/// request, so the reply is offered to each live completion; the tag decides
/// which one asked. Dropped when none matches.
pub(crate) fn deliver(view: &mut ShellView, outcome: DistinctOutcome, cx: &mut Context<ShellView>) {
    let vocab = view.expr_vocab.clone();
    let mut landed = false;
    if let Some(state) = view.scope_expr_dialog.as_mut() {
        landed =
            state
                .completion
                .deliver(&outcome.column, outcome.tag, outcome.values.clone(), &vocab);
    }
    if !landed {
        // The live object dialog and every parked one: an expression field covered
        // by another domain's dialog still owns its request.
        landed = view
            .object_dialog
            .iter_mut()
            .chain(super::dialog::parked_objects_mut(&mut view.modals))
            .filter(|state| super::objectdialog::expression_entry_open(state))
            .filter_map(|state| state.expr.as_mut())
            .any(|c| c.deliver(&outcome.column, outcome.tag, outcome.values.clone(), &vocab));
    }
    if landed {
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

/// Accept ranked row `i`: write it over its token (a named row erases
/// the token and stages its name), keep the keyboard in the field, and
/// re-read the new position.
pub(crate) fn accept(
    view: &mut ShellView,
    i: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let Some(accepted) = completion_mut(view).and_then(|c| c.accept(i)) else {
        return;
    };
    // The erase goes through the same range replace as an insert, so cmd+z
    // brings the typed prefix back.
    let (write, staged) = match accepted {
        Accept::Write(write) => (write, None),
        Accept::Stage { name, erase } => (erase, Some(name)),
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
    // sync writes nothing. Gated on the top kind: a covered object dialog's
    // draft belongs to it, not to whichever completion the accept above
    // just wrote through the shared input.
    let text = view.dialog_input.read(cx).value().to_string();
    if view.top_kind() == Some(DialogKind::Object)
        && let Some(state) = view.object_dialog.as_mut()
        && super::objectdialog::expression_entry_open(state)
        && let Some(draft) = state.draft.as_mut()
    {
        draft.set_query(text);
        super::dialog::sync_dialog_text(view, window, cx);
    }
    if let Some(name) = staged {
        stage_named(view, &name, cx);
    }
    refresh(view, cx);
}

/// Add `name` to the open frame expression dialog's staged names, so
/// Enter applies it beside the parsed text, and stop offering it. Only
/// that dialog offers named rows.
fn stage_named(view: &mut ShellView, name: &str, cx: &mut Context<ShellView>) {
    scope_expr_view::stage(view, name, cx);
}

/// A pointer accept: accept the ranked row of this kind labelled `label`
/// as the list stands at the press. The row is found by kind and label,
/// not by its painted position, so a list rebuilt between paint and press
/// never accepts a different row, and a named expression sharing a
/// column's name never accepts the column; a row no longer listed does
/// nothing.
pub(crate) fn accept_row(
    view: &mut ShellView,
    named: bool,
    label: &str,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let Some(i) = completion_mut(view).and_then(|c| c.position(named, label)) else {
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
/// `scope-expr-named-row-{name}`, `scope-expr-no-matches` and
/// `scope-expr-warning`. `on_click` gets whether the row is named and its
/// label.
pub(crate) fn render(
    c: &ExprCompletion,
    scroll: &ScrollHandle,
    theme: &Theme,
    on_click: impl Fn(bool, &str, &mut Window, &mut App) + Clone + 'static,
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
            // A named row has its own selector: a name equal to a column's
            // would otherwise share the column row's id.
            let (named, broken) = match row.kind {
                RowKind::Insert => (false, false),
                RowKind::Named { broken } => (true, broken),
            };
            let selector = if named {
                format!("scope-expr-named-row-{}", row.label)
            } else {
                format!("scope-expr-row-{}", row.label)
            };
            let detail_color = if broken {
                chip::chip_paint(theme, chip::Tone::DangerText).text
            } else {
                theme.muted_foreground
            };
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
                .child(
                    h_flex()
                        .gap_1()
                        .font_family(crate::fonts::MONO)
                        .text_sm()
                        .when(named, |label| {
                            label.child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::new_static("≡")),
                            )
                        })
                        .child(super::keybindings_view::highlighted_text(
                            &row.label,
                            &row.indices,
                            paint.accent,
                        )),
                )
                .child(
                    div()
                        .font_family(crate::fonts::MONO)
                        .text_xs()
                        .text_color(detail_color)
                        .child(SharedString::from(row.detail.clone())),
                )
                .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    // A double-click's second press lands on the list its
                    // first accept rebuilt; it must not accept again.
                    if event.click_count > 1 {
                        return;
                    }
                    on_click(named, &label, window, cx);
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
