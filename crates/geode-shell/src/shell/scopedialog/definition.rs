//! The Scope dialog's definition step: one saved expression's text, edited
//! in place, or a new one's text before it is named.
//!
//! The field is the shared input, its draft the source of truth
//! `sync_dialog_text` mirrors. Column and value suggestions come from
//! `expr_suggest`, which owns the step's completion while it is the top
//! layer; no named rows are offered, since one definition never refers to
//! another. `enter` checks the text (empty, parse, unknown column) and
//! writes it through the pending batch, then resolves it in the frame at
//! once, so every scope naming it reads the new text before the flush. A new
//! expression's step turns into a name prompt in place instead.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::exprcomplete::ExprCompletion;
use crate::footer::{Hint, HintRow};
use crate::keymap::Keystroke;
use crate::shell::objectdialog::apply;
use crate::shell::{ShellView, dialog, expr_suggest, scale};

use super::prompt::{NamePrompt, Purpose};
use super::state::{Layer, Step};
use super::view::ScopeDialogState;

/// Refusal of an empty definition: a reference to it would narrow nothing.
pub(crate) const EMPTY: &str = "an empty named expression would match everything";
/// The note when no saved scope and not the lane names the expression.
const NOT_USED: &str = "not used";
/// The field's label for a new expression, which has no name yet.
const NEW_LABEL: &str = "New expression";

/// The step's state, carried while the top layer is `Step::Definition`.
pub(crate) struct DefinitionStep {
    /// The expression edited; `None` is a new one.
    pub name: Option<String>,
    pub draft: String,
    /// The last refusal of the draft; typing clears it.
    pub error: Option<String>,
    /// Who uses the expression, prepared at open; `None` for a new one.
    pub note: Option<SharedString>,
    /// Never given named offers: a definition names columns only.
    pub completion: ExprCompletion,
}

/// Whether the definition step is the top layer: its field owns the input
/// and its completion the suggestion keys.
pub(crate) fn in_definition(state: &ScopeDialogState) -> bool {
    state.definition.is_some() && matches!(state.layers.top(), Layer::Step(Step::Definition { .. }))
}

/// The step for `name`, seeded with its text and a note naming its users,
/// or empty for a new expression. A name the frame no longer defines
/// refuses (an invalid definition is still defined: this is where it gets
/// fixed).
pub(super) fn step_for(
    shell: &ShellView,
    name: Option<&str>,
    cx: &App,
) -> Result<DefinitionStep, &'static str> {
    let Some(name) = name else {
        return Ok(DefinitionStep {
            name: None,
            draft: String::new(),
            error: None,
            note: None,
            completion: ExprCompletion::default(),
        });
    };
    let text = shell
        .target_frame()
        .read(cx)
        .named_expressions()
        .get(name)
        .map(|def| def.text().to_string())
        .ok_or(super::saved_view::EXPRESSION_GONE)?;
    let note = crate::shell::objectdialog::render::named_expression_users(shell, name, cx)
        .unwrap_or_else(|| NOT_USED.to_string());
    Ok(DefinitionStep {
        name: Some(name.to_string()),
        draft: text,
        error: None,
        note: Some(note.into()),
        completion: ExprCompletion::default(),
    })
}

/// Push the step over the screen on top (Current or Saved). A vanished
/// name refuses into the dialog's error line and opens nothing.
pub(super) fn push(
    shell: &mut ShellView,
    name: Option<String>,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let step = step_for(shell, name.as_deref(), cx);
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    match step {
        Ok(step) => {
            state.error = None;
            state.definition = Some(step);
            state.layers.push(Layer::Step(Step::Definition { name }));
            begin(shell, window, cx);
        }
        Err(refusal) => {
            state.error = Some(refusal.into());
            shell.refresh_dialog_rows(cx);
            cx.notify();
        }
    }
}

/// The step was just made the top layer: mirror its draft into the field
/// and compute the suggestions for it. The input observer would not run
/// when the field already held the same text.
pub(super) fn begin(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    dialog::sync_dialog_text(shell, window, cx);
    expr_suggest::refresh(shell, cx);
    cx.notify();
}

/// The Change arm's half: the field is the draft.
pub(super) fn on_draft_changed(state: &mut ScopeDialogState, text: &str) {
    if let Some(step) = state.definition.as_mut() {
        step.draft = text.to_string();
        step.error = None;
    }
}

/// Keys while the step is on top: the suggestion keys first, then `enter`
/// submits and `escape` leaves. Every other key is the field's.
pub(super) fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if expr_suggest::handle_key(shell, ks, window, cx) {
        return true;
    }
    if ks.mods.is_chord() {
        return false;
    }
    match ks.key.as_str() {
        "enter" => submit(shell, window, cx),
        "escape" => leave(shell, window, cx),
        _ => return false,
    }
    super::prompt::after_key(shell, window, cx);
    true
}

fn leave(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    state.definition = None;
    let after = state.layers.escape();
    super::saved_view::finish(shell, after, window, cx);
}

fn set_error(shell: &mut ShellView, error: String) {
    if let Some(step) = shell
        .scope_dialog
        .as_mut()
        .and_then(|s| s.definition.as_mut())
    {
        step.error = Some(error);
    }
}

/// Check the trimmed draft, then write it over the expression it edits, or
/// hand a new expression's text to a name prompt in this step's place. An
/// expression deleted under the step refuses, and an unchanged text leaves
/// with nothing written.
fn submit(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some((name, text)) = shell
        .scope_dialog
        .as_ref()
        .and_then(|s| s.definition.as_ref())
        .map(|d| (d.name.clone(), d.draft.trim().to_string()))
    else {
        return;
    };
    if text.is_empty() {
        return set_error(shell, EMPTY.into());
    }
    if let Err(refusal) = crate::shell::scope_expr_view::commit_text(&text, &shell.expr_vocab) {
        return set_error(shell, refusal);
    }
    let Some(name) = name else {
        // Naming replaces this step, so its commit still returns to the
        // screen the step was opened from.
        if let Some(state) = shell.scope_dialog.as_mut() {
            state.definition = None;
            state.prompt = Some(NamePrompt {
                purpose: Purpose::NameExpression { text: text.clone() },
                draft: String::new(),
                error: None,
            });
            state
                .layers
                .replace_top(Layer::Step(Step::NameExpression { text }));
        }
        return;
    };
    // Deleted while the step was open: writing would bring it back.
    let doc = geode_core::config::EXPRESSIONS_DOC;
    if apply::definition_owner(shell, doc, &name) == apply::Owner::Absent {
        return set_error(shell, super::saved_view::EXPRESSION_GONE.into());
    }
    // Unchanged: a write would fork an inherited entry for nothing and
    // freeze it against the lower layer's later updates.
    if current_text(shell, &name).as_deref() == Some(text.as_str()) {
        if let Some(state) = shell.scope_dialog.as_mut() {
            state.definition = None;
            let after = state.layers.commit_step();
            super::saved_view::finish(shell, after, window, cx);
        }
        return;
    }
    let fork = match write(shell, &name, &text, cx) {
        Ok(fork) => fork,
        Err(refusal) => return set_error(shell, refusal),
    };
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    state.definition = None;
    let after = state.layers.commit_step();
    super::saved_view::finish(shell, after, window, cx);
    // On the status bar: a one-shot step has closed by now.
    if let Some(fork) = fork {
        shell.notice = Some(fork.into());
    }
}

/// `name`'s definition text as the configuration holds it now, pending
/// writes included, trimmed as a submitted draft is.
fn current_text(shell: &ShellView, name: &str) -> Option<String> {
    let pending = apply::config_with_pending(shell);
    let config = pending.as_ref().unwrap_or(&shell.services.config);
    config
        .doc(geode_core::config::EXPRESSIONS_DOC)
        .and_then(|doc| doc.value.get(name))
        .and_then(|v| v.get("expression"))
        .and_then(|v| v.as_str())
        .map(|t| t.trim().to_string())
}

/// Write `text` as `name`'s definition through the pending batch and
/// resolve it in the frame at once, so the next key and every scope naming
/// it read the new text before the flush. The definition's other keys are
/// kept. `Ok(Some(notice))` when the write forked an inherited entry.
pub(super) fn write(
    shell: &mut ShellView,
    name: &str,
    text: &str,
    cx: &mut Context<ShellView>,
) -> Result<Option<String>, String> {
    let mut table = {
        let pending = apply::config_with_pending(shell);
        let config = pending.as_ref().unwrap_or(&shell.services.config);
        config
            .doc(geode_core::config::EXPRESSIONS_DOC)
            .and_then(|doc| doc.value.get(name))
            .and_then(|v| v.as_table())
            .cloned()
            .unwrap_or_default()
    };
    table.insert(
        "expression".to_string(),
        toml::Value::String(text.to_string()),
    );
    let fork = apply::queue_definition(
        shell,
        geode_core::config::EXPRESSIONS_DOC,
        name,
        toml::Value::Table(table),
        cx,
    )?;
    apply::refresh_definitions_now(shell, cx);
    Ok(fork)
}

/// The step's body: the field, its suggestions, the used-by note, the
/// refusal and the step's keys.
pub(super) fn build(
    shell: &ShellView,
    state: &ScopeDialogState,
    entity: &Entity<ShellView>,
    cx: &App,
) -> AnyElement {
    let Some(step) = state.definition.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let label = step.name.as_deref().unwrap_or(NEW_LABEL);
    let entity = entity.clone();
    let mut body = v_flex()
        .gap_2()
        .w(scale::design(super::view::WIDTH))
        .child(
            div()
                .debug_selector(|| "scope-dialog-definition-field".to_string())
                .child(dialog::name_row(&shell.dialog_input, label, cx)),
        )
        .child(expr_suggest::render(
            &step.completion,
            &shell.expr_scroll,
            theme,
            move |named, label, window, cx| {
                entity.update(cx, |shell, cx| {
                    expr_suggest::accept_row(shell, named, label, window, cx)
                });
            },
        ));
    if let Some(note) = step.note.clone() {
        body = body.child(
            div()
                .px_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "scope-dialog-definition-note".to_string())
                .child(note),
        );
    }
    if let Some(error) = step.error.clone() {
        body = body.child(
            div()
                .px_2()
                .text_xs()
                .text_color(theme.danger)
                .debug_selector(|| "scope-dialog-error".to_string())
                .child(error),
        );
    }
    body.child(
        v_flex()
            .w_full()
            .gap_1()
            .pt_2()
            .border_t_1()
            .border_color(theme.border)
            .child(dialog::hint_rows(&hints(state, step))),
    )
    .into_any_element()
}

fn hints(state: &ScopeDialogState, step: &DefinitionStep) -> Vec<Hint> {
    let enter = if step.name.is_some() {
        "save"
    } else {
        "name…"
    };
    let leave = if state.layers.depth() > 1 {
        "back"
    } else {
        "close"
    };
    vec![
        Hint::new(HintRow::Go, &["enter"], enter).selector("scope-dialog-hint-definition"),
        Hint::new(HintRow::Go, &["tab"], "insert"),
        Hint::new(HintRow::Go, &["escape"], leave),
    ]
}
