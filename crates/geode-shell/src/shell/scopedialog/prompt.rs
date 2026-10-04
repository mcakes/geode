//! The Scope dialog's name prompt and its pending question.
//!
//! A name prompt is a step drawn on Current's body: the shared input above
//! the rows, its draft the source of truth `sync_dialog_text` mirrors. A
//! pending question (an overwrite, a delete, a revert) takes the footer's
//! place and every key until it is answered: `y`/`enter` carry it out,
//! `n`/`escape` drop it, and anything else is claimed and dropped, so a
//! stray letter neither types nor acts on the rows behind the question.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use crate::footer::{Hint, HintRow};
use crate::keymap::Keystroke;
use crate::shell::objectdialog::apply::{self, Owner};
use crate::shell::{ShellView, dialog};
use dialog::{ConfirmAnswer, ConfirmHandler};

use super::saved::SavedId;
use super::state::{Layer, Step};
use super::view::{ScopeDialogState, edit_lane};

/// Refusal when there is no scope to save: at the door, and at the write
/// if the scope was cleared under the prompt.
pub(crate) const NOTHING_TO_SAVE: &str = "nothing to save — the scope is empty";
const SAVE_LABEL: &str = "Save scope as";
const NAME_EXPRESSION_LABEL: &str = "Name this expression";
const SCOPES_DOC: &str = "scopes";

/// What a name prompt names.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Purpose {
    /// The lane's scope, saved under the name.
    SaveScope,
    /// A new expression whose text was just accepted.
    NameExpression { text: String },
    /// A copy of a saved definition.
    #[allow(dead_code)] // Raised by the Saved screen's copy verb, not routed yet.
    Copy { from: SavedId },
}

impl Purpose {
    fn label(&self) -> &'static str {
        match self {
            Purpose::SaveScope => SAVE_LABEL,
            Purpose::NameExpression { .. } => NAME_EXPRESSION_LABEL,
            Purpose::Copy { .. } => "Copy as",
        }
    }

    /// What `enter` does, as the footer says it.
    fn enter_hint(&self) -> &'static str {
        match self {
            Purpose::SaveScope => "save",
            Purpose::NameExpression { .. } => "create",
            Purpose::Copy { .. } => "copy",
        }
    }

    /// Each prompt previews what it names. The save prompt names the lane's
    /// scope, so it paints over Current's rows; a new expression and a copy
    /// name something else (the expression joins no scope), so they paint
    /// alone with their own preview line.
    fn previews_current(&self) -> bool {
        matches!(self, Purpose::SaveScope)
    }

    /// The preview line of a prompt painted alone.
    fn preview(&self) -> Option<SharedString> {
        match self {
            Purpose::SaveScope => None,
            Purpose::NameExpression { text } => Some(text.clone().into()),
            Purpose::Copy { from } => Some(
                match from {
                    SavedId::Scope(name) | SavedId::Expression(name) => name.clone(),
                }
                .into(),
            ),
        }
    }
}

/// Whether a name prompt on top paints alone rather than over Current's
/// rows (see [`Purpose::previews_current`]).
pub(crate) fn paints_alone(state: &ScopeDialogState) -> bool {
    in_name_prompt(state)
        && state
            .prompt
            .as_ref()
            .is_some_and(|p| !p.purpose.previews_current())
}

/// A prompt painted alone: the field, what it names as a mono line, the
/// refusal, and the footer (or the question in its place).
pub(super) fn build_alone(
    shell: &ShellView,
    state: &ScopeDialogState,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    let Some(prompt) = state.prompt.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
    let mut body = v_flex()
        .gap_2()
        .w(crate::shell::scale::design(super::view::WIDTH))
        .children(field(shell, state, cx));
    if let Some(preview) = prompt.purpose.preview() {
        body = body.child(
            div()
                .px_2()
                .text_sm()
                .font_family(crate::fonts::MONO)
                .text_color(muted)
                .truncate()
                .debug_selector(|| "scope-dialog-name-preview".to_string())
                .child(preview),
        );
    }
    if let Some(error) = prompt.error.clone() {
        body = body.child(
            div()
                .px_2()
                .text_xs()
                .text_color(danger)
                .debug_selector(|| "scope-dialog-error".to_string())
                .child(error),
        );
    }
    let footer = match pending_footer(state, entity, cx) {
        Some(question) => question,
        None => dialog::hint_rows(&hints(state)),
    };
    body.child(
        v_flex()
            .w_full()
            .gap_1()
            .pt_2()
            .border_t_1()
            .border_color(border)
            .child(footer),
    )
    .into_any_element()
}

/// A name being typed, carried while the top layer is its step.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NamePrompt {
    pub purpose: Purpose,
    pub draft: String,
    /// The last refusal of the draft; typing clears it.
    pub error: Option<String>,
}

/// What a `yes` carries out.
#[allow(dead_code)] // Delete and Revert are raised by Saved-screen verbs not routed yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PendingAction {
    OverwriteScope { name: String },
    Delete { id: SavedId },
    Revert { id: SavedId },
}

/// A question awaiting `y`/`n`, painted in the footer's place.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pending {
    pub question: String,
    /// A second line under the question (what a delete would break).
    pub detail: Option<String>,
    pub yes_label: &'static str,
    pub action: PendingAction,
}

/// Whether a name prompt is the top layer: its field owns the input.
pub(crate) fn in_name_prompt(state: &ScopeDialogState) -> bool {
    state.prompt.is_some()
        && matches!(
            state.layers.top(),
            Layer::Step(Step::SaveScope | Step::NameExpression { .. } | Step::CopyName { .. })
        )
}

/// The save prompt's starting state: seeded with where the scope came from,
/// so saving a loaded scope back over itself is `s` then `enter`.
pub(super) fn save_prompt(seed: Option<String>) -> NamePrompt {
    NamePrompt {
        purpose: Purpose::SaveScope,
        draft: seed.unwrap_or_default(),
        error: None,
    }
}

/// `s` on Current: push the save prompt, or refuse into the dialog's error
/// line when the scope is empty. The caller re-derives and syncs the input.
pub(super) fn push_save(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let (empty, seed) = {
        let frame = shell.target_frame().read(cx);
        (
            frame.scope().is_empty(),
            frame.loaded_from().map(str::to_string),
        )
    };
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    if empty {
        state.error = Some(NOTHING_TO_SAVE.into());
        return;
    }
    state.error = None;
    state.prompt = Some(save_prompt(seed));
    state.layers.push(Layer::Step(Step::SaveScope));
}

/// The Change arm's half for a name prompt: the field is the draft.
pub(super) fn on_draft_changed(state: &mut ScopeDialogState, text: &str) {
    if let Some(prompt) = state.prompt.as_mut() {
        prompt.draft = text.to_string();
        prompt.error = None;
    }
}

/// Every key while a question is up. Recognised answers carry it out or
/// drop it; every other key is claimed and dropped.
pub(super) fn pending_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if let Some(answer) = ConfirmAnswer::from_key(ks) {
        answer_pending(shell, answer, window, cx);
    }
    after_key(shell, window, cx);
    true
}

/// Keys while a name prompt is on top: `enter` submits the draft, `escape`
/// leaves the step. Every other key is the field's.
pub(super) fn prompt_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if ks.mods.is_chord() {
        return false;
    }
    match ks.key.as_str() {
        "enter" => submit(shell, window, cx),
        "escape" => leave(shell, window, cx),
        _ => return false,
    }
    after_key(shell, window, cx);
    true
}

/// Re-derive and resync while the dialog is still the top one; a commit
/// that closed it handed focus to whatever is beneath.
pub(super) fn after_key(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if shell.top_kind() == Some(dialog::DialogKind::Scope) {
        shell.refresh_dialog_rows(cx);
        dialog::sync_dialog_text(shell, window, cx);
    }
    cx.notify();
}

fn leave(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    state.prompt = None;
    let after = state.layers.escape();
    super::saved_view::finish(shell, after, window, cx);
}

fn set_prompt_error(shell: &mut ShellView, error: String) {
    if let Some(prompt) = shell.scope_dialog.as_mut().and_then(|s| s.prompt.as_mut()) {
        prompt.error = Some(error);
    }
}

/// The draft as a usable, unreserved name, or the refusal.
fn checked_name(draft: &str) -> Result<String, String> {
    let name = geode_core::config::check_object_name(draft)?.to_string();
    if geode_core::scopes::RESERVED_NAMES.contains(&name.as_str()) {
        return Err(format!("'{name}' is reserved"));
    }
    Ok(name)
}

/// Check the draft as a name, then carry out the prompt's purpose.
fn submit(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(prompt) = shell.scope_dialog.as_ref().and_then(|s| s.prompt.clone()) else {
        return;
    };
    let name = match checked_name(&prompt.draft) {
        Ok(name) => name,
        Err(refusal) => return set_prompt_error(shell, refusal),
    };
    match prompt.purpose {
        Purpose::SaveScope => submit_save(shell, name, window, cx),
        Purpose::NameExpression { text } => name_expression(shell, name, &text, window, cx),
        Purpose::Copy { .. } => {}
    }
}

/// A new expression takes a name no layer holds: naming it is not editing
/// someone else's. Written and resolved at once, and not applied to the
/// lane's scope; the step leaves for the screen it was opened from.
fn name_expression(
    shell: &mut ShellView,
    name: String,
    text: &str,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let doc = geode_core::config::EXPRESSIONS_DOC;
    if apply::definition_owner(shell, doc, &name) != Owner::Absent {
        return set_prompt_error(shell, format!("'{name}' already exists"));
    }
    if let Err(refusal) = super::definition::write(shell, &name, text, cx) {
        return set_prompt_error(shell, refusal);
    }
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    state.prompt = None;
    let after = state.layers.commit_step();
    super::saved_view::finish(shell, after, window, cx);
}

/// Save the lane's scope as `name`: at once over a new or inherited name
/// (a fork, announced), after a question over the user's own.
fn submit_save(
    shell: &mut ShellView,
    name: String,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    // Pending-aware: a scope saved a moment ago is already the user's.
    match apply::definition_owner(shell, SCOPES_DOC, &name) {
        Owner::User { .. } => {
            if let Some(state) = shell.scope_dialog.as_mut() {
                state.pending = Some(Pending {
                    question: format!("Replace '{name}' with the current scope?"),
                    detail: None,
                    yes_label: "Replace",
                    action: PendingAction::OverwriteScope { name },
                });
            }
        }
        Owner::Absent | Owner::Inherited(_) => save_scope_as(shell, name, window, cx),
    }
}

/// `y`/Replace carries the question's action out; `n`/Cancel drops it, and
/// the prompt beneath shows again with its draft as it was.
fn answer_pending(
    shell: &mut ShellView,
    answer: ConfirmAnswer,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let Some(pending) = shell.scope_dialog.as_mut().and_then(|s| s.pending.take()) else {
        return;
    };
    if answer == ConfirmAnswer::No {
        return;
    }
    match pending.action {
        PendingAction::OverwriteScope { name } => save_scope_as(shell, name, window, cx),
        // Nothing raises these yet.
        PendingAction::Delete { .. } | PendingAction::Revert { .. } => {}
    }
}

/// Write the lane's scope as `name` through the pending batch, resolve it
/// in the frame at once (so the next `s` sees it as the user's), record it
/// as the lane's provenance and leave the step. A fork's announcement goes
/// to the status bar, since a one-shot prompt has closed by then. A refused
/// write keeps the prompt open with the refusal under the field.
fn save_scope_as(
    shell: &mut ShellView,
    name: String,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let scope = shell.target_frame().read(cx).scope().clone();
    if scope.is_empty() {
        return set_prompt_error(shell, NOTHING_TO_SAVE.into());
    }
    let value = toml::Value::Table(apply::scope_as_toml(&scope));
    let fork = match apply::queue_definition(shell, SCOPES_DOC, &name, value, cx) {
        Ok(fork) => fork,
        Err(refusal) => return set_prompt_error(shell, refusal),
    };
    apply::refresh_definitions_now(shell, cx);
    edit_lane(shell, cx, |f| f.note_saved_as(name));
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    state.prompt = None;
    state.pending = None;
    let after = state.layers.commit_step();
    super::saved_view::finish(shell, after, window, cx);
    if let Some(fork) = fork {
        shell.notice = Some(fork.into());
    }
}

/// The name field, drawn above Current's rows while a prompt is on top.
pub(super) fn field(shell: &ShellView, state: &ScopeDialogState, cx: &App) -> Option<AnyElement> {
    let prompt = state.prompt.as_ref().filter(|_| in_name_prompt(state))?;
    Some(
        div()
            .debug_selector(|| "scope-dialog-name-field".to_string())
            .child(dialog::name_row(
                &shell.dialog_input,
                prompt.purpose.label(),
                cx,
            ))
            .into_any_element(),
    )
}

/// The refusal under the field while a prompt is on top.
pub(super) fn error(state: &ScopeDialogState) -> Option<&String> {
    state
        .prompt
        .as_ref()
        .filter(|_| in_name_prompt(state))
        .and_then(|p| p.error.as_ref())
}

/// The question and its buttons, in the footer's place, while one is up.
pub(super) fn pending_footer(
    state: &ScopeDialogState,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> Option<AnyElement> {
    let pending = state.pending.as_ref()?;
    let muted = cx.theme().muted_foreground;
    let on_yes: ConfirmHandler = Rc::new(|shell, window, cx| {
        answer_pending(shell, ConfirmAnswer::Yes, window, cx);
    });
    let on_no: ConfirmHandler = Rc::new(|shell, window, cx| {
        answer_pending(shell, ConfirmAnswer::No, window, cx);
    });
    let row = dialog::confirm_row(
        pending.question.clone(),
        pending.yes_label,
        "scope-dialog",
        entity,
        on_yes,
        on_no,
        cx,
    );
    Some(
        v_flex()
            .w_full()
            .gap_1()
            .child(row)
            .when_some(pending.detail.clone(), |el, detail| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .debug_selector(|| "scope-dialog-confirm-detail".to_string())
                        .child(detail),
                )
            })
            .into_any_element(),
    )
}

/// The prompt's own keys: everything else types.
pub(super) fn hints(state: &ScopeDialogState) -> Vec<Hint> {
    let enter = state
        .prompt
        .as_ref()
        .map_or("save", |p| p.purpose.enter_hint());
    let leave = if state.layers.depth() > 1 {
        "back"
    } else {
        "close"
    };
    vec![
        Hint::new(HintRow::Go, &["enter"], enter).selector("scope-dialog-hint-save"),
        Hint::new(HintRow::Go, &["escape"], leave),
    ]
}
