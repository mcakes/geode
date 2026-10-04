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
    /// A copy of a saved definition. The label (`Copy '<name>' as`) and the
    /// preview (the source's summary or text) are prepared when the prompt
    /// opens, so painting it formats nothing.
    Copy {
        from: SavedId,
        label: SharedString,
        preview: SharedString,
    },
}

impl Purpose {
    pub(crate) fn label(&self) -> &str {
        match self {
            Purpose::SaveScope => SAVE_LABEL,
            Purpose::NameExpression { .. } => NAME_EXPRESSION_LABEL,
            Purpose::Copy { label, .. } => label,
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
    pub(crate) fn preview(&self) -> Option<SharedString> {
        match self {
            Purpose::SaveScope => None,
            Purpose::NameExpression { text } => Some(text.clone().into()),
            Purpose::Copy { preview, .. } => Some(preview.clone()),
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
        Purpose::Copy { from, .. } => copy_definition(shell, name, &from, window, cx),
    }
}

/// The document a saved definition lives in, its name, and the refusal
/// when it is gone.
pub(super) fn doc_of(id: &SavedId) -> (&'static str, &str, &'static str) {
    match id {
        SavedId::Scope(name) => (SCOPES_DOC, name, super::saved_view::SCOPE_GONE),
        SavedId::Expression(name) => (
            geode_core::config::EXPRESSIONS_DOC,
            name,
            super::saved_view::EXPRESSION_GONE,
        ),
    }
}

/// The Copy prompt for the Saved row `row`: empty, labelled with what it
/// copies, previewing the source's summary or text.
pub(super) fn copy_prompt(row: &super::saved::SavedRow) -> NamePrompt {
    let preview = match &row.kind {
        super::saved::SavedKind::Scope { summary } => summary.clone(),
        super::saved::SavedKind::Expression { text, .. } => text.clone(),
    };
    NamePrompt {
        purpose: Purpose::Copy {
            from: row.id.clone(),
            label: format!("Copy '{}' as", row.name).into(),
            preview: preview.into(),
        },
        draft: String::new(),
        error: None,
    }
}

/// Write `from`'s definition, as the configuration holds it now, under the
/// new `name`. A copy never forks: the name must be one no layer holds. The
/// step leaves for Saved with the cursor on the copy.
fn copy_definition(
    shell: &mut ShellView,
    name: String,
    from: &SavedId,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let (doc, source, gone) = doc_of(from);
    if apply::definition_owner(shell, doc, &name) != Owner::Absent {
        return set_prompt_error(shell, format!("'{name}' already exists"));
    }
    // Re-read, pending batch included: the row was prepared earlier and the
    // source may have changed or gone since.
    let value = {
        let pending = apply::config_with_pending(shell);
        let config = pending.as_ref().unwrap_or(&shell.services.config);
        config.doc(doc).and_then(|d| d.value.get(source)).cloned()
    };
    let Some(value) = value else {
        return set_prompt_error(shell, gone.into());
    };
    if let Err(refusal) = apply::queue_definition(shell, doc, &name, value, cx) {
        return set_prompt_error(shell, refusal);
    }
    apply::refresh_definitions_now(shell, cx);
    let copy = match from {
        SavedId::Scope(_) => SavedId::Scope(name),
        SavedId::Expression(_) => SavedId::Expression(name),
    };
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    state.prompt = None;
    state.saved.cursor_id = Some(super::saved_view::StopId::Row(copy));
    let after = state.layers.commit_step();
    super::saved_view::finish(shell, after, window, cx);
}

/// Refusal when the definition a question was asked about is gone, or no
/// longer what the question asked about, when the answer arrives.
pub(crate) const LIST_CHANGED: &str = "the list changed under the question — nothing was removed";

/// `d` on a Saved row: ask before deleting the user's own definition; an
/// inherited one has nothing of the user's to delete. The delete of an
/// expression says under the question who uses it.
pub(super) fn ask_delete(shell: &mut ShellView, id: SavedId, cx: &App) -> Result<(), String> {
    let (doc, name, gone) = doc_of(&id);
    let detail = match &id {
        SavedId::Expression(name) => {
            crate::shell::objectdialog::render::named_expression_users(shell, name, cx)
        }
        SavedId::Scope(_) => None,
    };
    let question = match apply::definition_owner(shell, doc, name) {
        Owner::User { .. } => format!("Delete '{name}' from your config?"),
        Owner::Inherited(layer) => {
            return Err(format!(
                "'{name}' comes from the {} layer — there is nothing of yours to delete",
                layer.name()
            ));
        }
        Owner::Absent => return Err(gone.into()),
    };
    if let Some(state) = shell.scope_dialog.as_mut() {
        state.pending = Some(Pending {
            question,
            detail,
            yes_label: "Delete",
            action: PendingAction::Delete { id },
        });
    }
    Ok(())
}

/// `r` on a Saved row: ask before throwing away the user's copy of a
/// definition a lower layer also holds; anything else has nothing to
/// revert.
pub(super) fn ask_revert(shell: &mut ShellView, id: SavedId) -> Result<(), String> {
    let (doc, name, _) = doc_of(&id);
    let Owner::User { over: Some(_) } = apply::definition_owner(shell, doc, name) else {
        return Err(format!("'{name}' has no changes of yours to revert"));
    };
    let question = format!("Throw away your changes to '{name}'?");
    if let Some(state) = shell.scope_dialog.as_mut() {
        state.pending = Some(Pending {
            question,
            detail: None,
            yes_label: "Revert",
            action: PendingAction::Revert { id },
        });
    }
    Ok(())
}

/// Carry out a delete or revert the user said yes to, against the
/// definition as it is now: still the user's, and for a revert still over a
/// lower copy. Otherwise the question no longer describes what a removal
/// would do (a revert whose lower copy went would delete the user's only
/// copy), and it refuses. The Saved rows re-derive on the way out, so the
/// cursor stays on a reverted row or moves to the next after a delete.
fn remove_saved(shell: &mut ShellView, id: &SavedId, revert: bool, cx: &mut Context<ShellView>) {
    let (doc, name, _) = doc_of(id);
    let still = match apply::definition_owner(shell, doc, name) {
        Owner::User { over } => !revert || over.is_some(),
        Owner::Absent | Owner::Inherited(_) => false,
    };
    let refusal = if !still {
        Some(LIST_CHANGED.to_string())
    } else {
        let removed = apply::remove_definition(shell, doc, name, cx);
        apply::refresh_definitions_now(shell, cx);
        removed.err()
    };
    if let Some(state) = shell.scope_dialog.as_mut() {
        state.error = refusal;
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
        PendingAction::Delete { id } => remove_saved(shell, &id, false, cx),
        PendingAction::Revert { id } => remove_saved(shell, &id, true, cx),
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
