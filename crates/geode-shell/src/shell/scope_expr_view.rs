//! The scope expression dialog (command-line locality spec §4.1): the
//! typed door onto the frame's expression layer now that `:scope <expr>`
//! is a refusal. In [`asof_view`](super::asof_view)'s mould: the shared
//! `dialog_input` IS the value being edited, `enter` commits, a parse
//! error paints inline and keeps the dialog open, an empty field clears
//! the expression. Opened by `frame::scope_expression` (the palette's
//! "Set scope expression…") and by a click on the scope bar's expression
//! chip (`toolbar::toolbar`'s `on_expr`).
//!
//! No dataset validation here (spec §4.1): `:scope <expr>` validated
//! against the typing tile's dataset, arbitrary for a frame-wide value;
//! the compiler drops a conjunct naming a column a dataset has no
//! storage for, and a saved scope's expression arrives unvalidated too.
//!
//! [`open`] is the only entry point and the only place a
//! [`ScopeExprState`] is constructed — nothing survives a close/reopen.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use geode_core::scope::{Expr, parse_expr};

use crate::keymap::{Keystroke, Modifiers};

use super::ShellView;
use super::chip;
use super::dialog;
use super::picker::{Hint, hint_row};
use super::scale;

// ---------------------------------------------------------------------
// Pure core — no gpui.
// ---------------------------------------------------------------------

/// The dialog's state: only the last failed commit's message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeExprState {
    pub error: Option<String>,
}

/// What `enter` does with the field's text: empty clears the expression
/// (`Ok(None)`), anything else must parse. The message is the one the
/// `:` line used to show (`"{message} at column {caret}"`).
pub fn commit_text(text: &str) -> Result<Option<Expr>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    parse_expr(text)
        .map(Some)
        .map_err(|e| format!("{} at column {}", e.message, e.caret + 1))
}

// ---------------------------------------------------------------------
// gpui: the modal.
// ---------------------------------------------------------------------

const WIDTH: f32 = 640.0;

const HINTS: &[Hint] = &[
    Hint::Key("enter"),
    Hint::Text("set · empty clears ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

/// Open the dialog seeded with the frame's current expression source. A
/// no-op if a modal is already open, like every other `open` here. The
/// seed is written AFTER the door (`open_shell_dialog_with_key` resets
/// the field to empty), and `set_value` emits no `Change`, so the state
/// starts with no error regardless.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    view.scope_expr_dialog = Some(ScopeExprState::default());
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        "Scope expression",
        build,
        Some(Rc::new(handle_key)),
        true,
    );
    let seed = view
        .frame
        .read(cx)
        .scope()
        .expression
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    view.dialog_input
        .update(cx, |input, cx| input.set_value(seed, window, cx));
}

/// Typing clears the last error (the `Change` subscription arm in
/// `shell/mod.rs` calls this).
pub(crate) fn on_query_changed(state: &mut ScopeExprState) {
    state.error = None;
}

fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if ks.mods != Modifiers::NONE || ks.key != "enter" {
        return false;
    }
    let text = shell.dialog_input.read(cx).value().to_string();
    match commit_text(&text) {
        Ok(expression) => {
            shell.frame.update(cx, |f, cx| {
                let mut scope = f.scope().clone();
                scope.expression = expression;
                if f.set_scope(scope) {
                    cx.notify();
                }
            });
            shell.close_modal(window, cx);
        }
        Err(message) => {
            if let Some(state) = shell.scope_expr_dialog.as_mut() {
                state.error = Some(message);
            }
            cx.notify();
        }
    }
    true
}

fn build(shell: &ShellView, _window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(state) = shell.scope_expr_dialog.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let mut column = v_flex()
        .gap_2()
        .w(scale::design(WIDTH))
        .child(dialog::filter_row(&shell.dialog_input, None, cx));
    if let Some(err) = &state.error {
        // Through the chip door (`shell::chip`), not a raw `theme.danger`:
        // that token bypasses the readability floor, and the parse error
        // is the single thing in this dialog a trader must be able to
        // read (review finding, Task 6 fix round 1).
        column = column.child(
            div()
                .text_sm()
                .text_color(chip::chip_paint(theme, chip::Tone::DangerText).text)
                .debug_selector(|| "scope-expr-error".to_string())
                .child(err.clone()),
        );
    }
    column
        .child(hint_row(
            HINTS,
            "scope-expr-hints",
            WIDTH,
            theme.muted_foreground,
            theme.muted,
            theme.border,
            theme.radius,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_clears_and_a_broken_expression_names_the_column() {
        assert_eq!(commit_text("   ").unwrap(), None);
        assert!(commit_text("book = 'BK000'").unwrap().is_some());
        let err = commit_text("book =").unwrap_err();
        assert!(err.contains("at column"), "{err}");
    }
}
