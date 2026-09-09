//! The object dialog's gpui half: opening it, the one
//! [`dialog::ModalKeyHandler`] both its stages come through, and the
//! painted browse list, field rows and action bar.
//!
//! Everything here is the shell around [`super`]'s pure core, and it is
//! deliberately the same shell `keybindings_view` grew — same door
//! ([`dialog::open_shell_dialog_with_key`]), same two-mode routing, same
//! mode pill above the same shared filter row, same muted footer stating
//! only the *current* mode's vocabulary. A second routing shape would be
//! a second set of edge cases (which key blurs the filter, which escape
//! rung closes the modal) for a user to learn twice and a maintainer to
//! fix twice.
//!
//! ## The one switch: normal mode is a blurred filter
//!
//! This dialog opens in [`DialogMode::Normal`] with
//! `ShellView::dialog_input` **blurred** (`focus_filter: false`), because
//! a focused gpui-component `Input` consumes bare letters as text before
//! any raw key listener sees them — which is the whole reason `j`/`k` can
//! move here at all, and the reason the edit stage's `s`/`d`/`r` verbs
//! are reachable. `/` focuses the field and enters [`DialogMode::Filter`];
//! `escape` blurs it again. While the field is blurred the query paints
//! as static muted text rather than a live caret
//! ([`dialog::filter_row`]'s `frozen` argument): a caret in a field that
//! is not receiving the keys is the single most misleading thing a modal
//! surface can show.
//!
//! ## Two stages, one routing shape
//!
//! `enter` on a browse row opens the **edit stage** over a [`super::Draft`]
//! of that object. The two stages share this file's one
//! [`dialog::ModalKeyHandler`] and split at its front door
//! ([`handle_key`]), because everything below the split — the notice's two
//! doors, the `escape` ladder, the claim-and-drop contract — has to behave
//! identically in both or a user learns two dialogs.
//!
//! The edit stage does **not** filter its own rows, which the browse stage
//! does. `/` there would need a second cursor space (a filtered position
//! beside the draft's own row index) and would make `shift+j` ambiguous —
//! moving an item past a neighbour the filter is hiding. Entering the
//! stage therefore drops the browse query, which also keeps the `escape`
//! ladder honest: with no query, `escape` reaches
//! [`EscapeStep::PreviousStage`] rather than silently spending itself on
//! `ClearQuery`.
//!
//! ## Applying is instant; it is still the watcher's applier
//!
//! There is no save key. A field edit moves memory on the keystroke —
//! [`super::apply::commit_edit`] merges the change through the loader's
//! own `Config::from_docs` and hands the result to `hot_reload::
//! apply_reload`, the same applier the 500 ms watcher uses — and queues
//! the file write on a debounce behind it (spec §7.1, and
//! [`super::apply`]'s module doc for the merge measurement, the
//! self-write reasoning and the failed-write revert). Both stages still
//! derive fresh from `Config`, so what they show after the keystroke is
//! the merged truth rather than the draft's opinion of it.
//!
//! One edit still asks first, and only one: a change that would **fork**
//! the object into the user layer (spec §4.1), because a fork freezes the
//! desk's copy out. That is [`Confirm::Fork`], and it is why the action
//! bar's remaining verbs are exactly the destructive and structural
//! ones.

use std::rc::Rc;

use geode_core::config::Layer;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Focusable as _, MouseButton, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};

use super::{
    Confirm, Destination, Domain, Draft, EditRow, FieldKind, ObjectDialogState, ObjectRow, Stage,
};
use crate::config_write;
use crate::dialogmode::{self, DialogMode, EscapeStep, NormalCommand};
use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;
use crate::vimnav;

use super::super::ShellView;
use super::super::dialog;
use super::super::keybindings_view::{highlighted_text, key_chip, split_label_indices};

/// Row height estimate for sizing the scrollable viewport (two lines:
/// name plus muted summary) — the same non-load-bearing estimate
/// `keybindings_view::ROW_HEIGHT` is, since scroll-follow goes through
/// `ScrollHandle::scroll_to_item`, which measures real layout.
const ROW_HEIGHT: f32 = 44.0;
/// The same estimate for the edit stage's rows, which are one line rather
/// than two — a field's label and its value sit side by side.
const FIELD_ROW_HEIGHT: f32 = 28.0;
/// Rows visible before the list scrolls — see `palette::VISIBLE_ROWS`.
const VISIBLE_ROWS: usize = 10;
/// Target dialog content width in pixels — the keybinding dialog's, so
/// the two modals are the same object on screen.
const WIDTH: f32 = 640.0;

/// Open the object dialog on `domain` (`config::views`, palette-only —
/// see `defaults::register_builtin_actions`). A no-op if a modal is
/// already open, mirroring the other dialogs' own guard.
pub fn open(
    view: &mut ShellView,
    domain: Domain,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if view.modal.is_some() {
        return;
    }
    // Fresh state every open — nothing survives a close/reopen, the same
    // contract `palette` and both list dialogs hold.
    view.object_dialog = Some(ObjectDialogState::new(domain));
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        domain.title(),
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        // `false`: normal mode. A focused filter would eat every bare
        // letter as text before [`handle_key`] could read it as a motion
        // or as one of the edit stage's verbs.
        false,
    );
}

/// The [`dialog::ModalKeyHandler`] for this dialog: the front door both
/// stages come through, splitting on the stage and on nothing else.
///
/// The split is here rather than inside each branch so the two stages
/// cannot drift on the things they must agree about — which keys are
/// claimed, when the notice is dropped, and which `escape` rung applies.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let editing = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| matches!(state.stage, Stage::Edit { .. }));
    if editing {
        handle_edit_key(shell, ks, window, cx)
    } else {
        handle_browse_key(shell, ks, window, cx)
    }
}

/// The browse stage's keys — the same priority
/// order `keybindings_view::handle_key` documents, minus its rebind
/// capture (this dialog has nothing to capture):
///
/// 1. in [`DialogMode::Normal`], `escape` walks
///    [`dialogmode::escape_step`]'s ladder and every other keystroke goes
///    through [`dialogmode::normal_command`] — including keys it does not
///    claim, which are swallowed (`true`) rather than passed on: normal
///    mode's contract is that a stray letter does nothing, and letting it
///    fall through would hand it to whatever the shell does with that key
///    next;
/// 2. in [`DialogMode::Filter`] the behaviour is the filter-first one
///    every other dialog has, with `escape` leaving filter mode (keeping
///    the query) instead of closing the dialog;
/// 3. bare `enter` is claimed in both modes and opens the edit stage on
///    the selected row, so the two modes route it identically;
/// 4. bare `tab`/`shift+tab` are claimed and dropped. Claiming them is
///    what makes them inert: with the filter focused, an unclaimed key
///    continues to the window's own text-input phase, and
///    `InputState::normalize_input` strips `\n`/`\r` but not `\t`, so an
///    unclaimed `tab` would land in the query as a literal tab and
///    collapse the list to "no matches";
/// 5. in filter mode everything else returns `false`, unhandled — which
///    for a printable key is exactly right: the modal branch in
///    `handle_key_down` only stops propagation for keys this handler
///    claims, so an unclaimed character goes on to the focused `Input`'s
///    own text insertion. The one other `false` is the ladder's last
///    rung, which is how the shell's modal branch gets to close the
///    dialog.
fn handle_browse_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let rows = derive_rows(shell);
    let input = shell.dialog_input.clone();
    let Some(state) = shell.object_dialog.as_mut() else {
        return false;
    };
    // A notice reports on the keystroke that produced it and nothing
    // else, so it is dropped at the DOOR — here and in
    // [`on_row_clicked`] — rather than in whichever branch happens to
    // move the selection. `take` plus a conditional `notify` rather than
    // a bare assignment, because the claim-and-drop early return below
    // returns without notifying, and a cleared notice would otherwise
    // stay painted until something else requested a frame. (The same
    // reasoning, and the same two doors, as `keybindings_view`.)
    if state.notice.take().is_some() {
        cx.notify();
    }
    let visible = super::visible_rows(state, &rows);

    if state.mode == DialogMode::Normal {
        // Modifier-agnostic on `escape`, exactly as `handle_key_down`'s
        // own close is (`event.keystroke.key == "escape"`): a bare-only
        // guard would turn `shift+escape` into a key normal mode claims
        // and drops — visibly nothing.
        if ks.key == "escape" {
            // `has_previous_stage()` is a predicate over the stage, not
            // a literal `false`, and this dialog is the design's first
            // consumer of the `PreviousStage` rung at all. From THIS
            // branch it is always false — `Browse` is where the ladder
            // ends and the modal closes — but the predicate is what let
            // the edit stage turn the rung on by merely constructing
            // `Stage::Edit`, with no call site to remember to change.
            // See `ObjectDialogState::has_previous_stage`.
            match dialogmode::escape_step(
                state.mode,
                state.query.is_empty(),
                state.has_previous_stage(),
            ) {
                EscapeStep::ClearQuery => {
                    state.query.clear();
                    state.selected = 0;
                    // The viewport has to follow: clearing a filter
                    // re-expands the list under a scroll offset still
                    // parked where the filtered list left it, so row 0
                    // would sit above the top of the screen with only the
                    // index having moved.
                    shell.object_dialog_scroll.scroll_to_item(0);
                    // The `Input` owns the text; clearing only the
                    // mirrored copy would leave the old query waiting in
                    // the field for the next `/`.
                    input.update(cx, |i, cx| i.set_value("", window, cx));
                    cx.notify();
                    return true;
                }
                // `LeaveFilter` cannot be reached from normal mode, and
                // `PreviousStage` cannot be reached from `Browse`, which
                // is the only stage that arrives here — the edit stage
                // has its own escape branch and takes that rung there.
                // Both are folded into the catch-all rather than
                // special-cased away, because `escape_step` is the one
                // ladder every modal surface walks and forking it per
                // call site is how the rungs drift apart.
                _ => return false, // let the shell's modal branch close it
            }
        }
        let Some(cmd) = dialogmode::normal_command(ks) else {
            // Claimed and dropped: in normal mode a key with no meaning
            // does nothing at all, rather than falling through to the
            // shell still listening underneath the modal.
            return true;
        };
        match cmd {
            NormalCommand::Nav(nav) => {
                state.selected = vimnav::apply(state.selected, visible.len(), nav);
                let selected = state.selected;
                shell.object_dialog_scroll.scroll_to_item(selected);
            }
            NormalCommand::EnterFilter => {
                state.mode = DialogMode::Filter;
                // The one switch, thrown the other way: the filter takes
                // focus and printable keys become text again.
                input.read(cx).focus_handle(cx).focus(window, cx);
            }
            NormalCommand::Commit => {
                open_selected(shell, window, cx);
                return true;
            }
            // `Toggle`, `EditText`, `MoveItem` and the letter verbs are
            // the edit stage's, and the browse footer advertises none of
            // them — so they are claimed and dropped here like any other
            // unclaimed key.
            _ => {}
        }
        cx.notify();
        return true;
    }

    // ---- Filter mode -------------------------------------------------

    if ks.key == "escape" {
        // The ladder's first rung, which must be claimed (`true`):
        // falling through would close the whole dialog on the escape that
        // was only meant to leave the search. The query stays applied;
        // blurring is what makes the letters motions again.
        state.mode = DialogMode::Normal;
        shell.focus_handle.focus(window, cx);
        cx.notify();
        return true;
    }

    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        // `enter` is `NormalCommand::Commit`, and normal mode routes it
        // to exactly the same place (the `Commit` arm above). Claimed
        // here so the two modes open an object the same way — the
        // keybinding dialog's own `enter` branch is what this mirrors,
        // and a routing difference between the two dialogs is a
        // difference somebody eventually has to debug.
        open_selected(shell, window, cx);
        return true;
    }

    if let Some(cmd) = listfilter::nav_command(ks) {
        state.selected = vimnav::apply(state.selected, visible.len(), cmd);
        let selected = state.selected;
        shell.object_dialog_scroll.scroll_to_item(selected);
        cx.notify();
        return true;
    }

    // `tab`/`shift+tab`: reserved and inert, claimed rather than left
    // unhandled — see this function's own doc comment, item 4.
    if ks.mods == Modifiers::NONE && ks.key == "tab" {
        return true;
    }
    if ks.key == "tab"
        && ks.mods
            == (Modifiers {
                shift: true,
                ..Modifiers::NONE
            })
    {
        return true;
    }

    false
}

/// The rows for whatever domain is open, derived fresh from the live
/// config — never cached, see [`super`]'s module doc. `Vec::new()` when
/// no dialog is open, which only happens if a keystroke arrives between
/// the modal closing and this handler being dropped.
fn derive_rows(shell: &ShellView) -> Vec<ObjectRow> {
    shell
        .object_dialog
        .as_ref()
        .map(|state| state.domain.objects(&shell.services.config))
        .unwrap_or_default()
}

/// Selection logic for a real mouse click on the row for `clicked`
/// (resolved back to a position in the *filtered* list against freshly
/// derived rows). It moves focus the same way [`handle_key`] does — to
/// whichever surface the current mode owns — because focusing the filter
/// unconditionally here would let a mouse click silently defeat normal
/// mode, and the next keystroke would type instead of act.
fn on_row_clicked(
    shell: &mut ShellView,
    clicked: &str,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let rows = derive_rows(shell);
    let input = shell.dialog_input.clone();
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    // The dialog's other door — see [`handle_key`]'s own clear.
    if state.notice.take().is_some() {
        cx.notify();
    }
    let visible = super::visible_rows(state, &rows);
    let Some(ix) = super::filtered_position(&visible, &rows, clicked) else {
        return;
    };
    state.selected = ix;
    let filter_mode = state.mode == DialogMode::Filter;
    shell.object_dialog_scroll.scroll_to_item(ix);
    if filter_mode {
        input.read(cx).focus_handle(cx).focus(window, cx);
    } else {
        shell.focus_handle.focus(window, cx);
    }
    cx.notify();
}

// ---- The edit stage ---------------------------------------------------

/// `enter`: open the selected browse row's object in the edit stage.
///
/// Resolves the row through the same filtered walk everything else here
/// uses, so what opens is the row the user is looking at even mid-filter.
/// The shared `Input` is emptied along with the mirrored query (see this
/// module's own "Two stages" note) and focus goes back to the shell, which
/// is what makes the edit stage's letters verbs.
fn open_selected(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let rows = derive_rows(shell);
    let name = shell.object_dialog.as_ref().and_then(|state| {
        let visible = super::visible_rows(state, &rows);
        visible
            .get(state.selected)
            .and_then(|m| rows.get(m.row))
            .map(|row| row.name.clone())
    });
    let Some(name) = name else {
        // Only reachable with the filter hiding every row, which is not a
        // row the user was pointing at — said out loud rather than
        // dropped, like every other deliberately inert keystroke here.
        set_notice(shell, "no object is selected".to_string());
        cx.notify();
        return;
    };
    enter_edit_stage(shell, &name, window, cx);
}

/// **The one door into the edit stage.** Every way in goes through here —
/// `enter` from either browse mode today, and Part 2's `n` tomorrow.
///
/// It exists because the transition has two halves that are worthless
/// apart: [`ObjectDialogState::enter_edit`] sets the mode and drops the
/// query, and only this function empties the shared `Input` and moves
/// focus off it to match. Setting one without the other is not a cosmetic
/// slip — it is the defect this task shipped and fixed
/// (`an_object_opened_from_filter_mode_still_escapes_back_a_stage`): a
/// stage whose mode and focus disagree sends the next `escape` down a
/// rung the edit handler does not claim, and the shell closes the whole
/// dialog with the draft still unsaved.
///
/// So the halves are not offered separately: `enter_edit` is visible only
/// inside this module's subtree and its doc points here, and this is the
/// only function in that subtree that calls it. A new call site gets both
/// halves or neither.
fn enter_edit_stage(
    shell: &mut ShellView,
    name: &str,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut() {
        state.enter_edit(&shell.services.config, name);
    }
    // The `Input` owns the text; clearing only the mirrored query would
    // leave the old one waiting in the field for the next `/`.
    let input = shell.dialog_input.clone();
    input.update(cx, |i, cx| i.set_value("", window, cx));
    // And the blur, which is what makes the edit stage's letters verbs.
    shell.focus_handle.focus(window, cx);
    shell.object_dialog_scroll.scroll_to_item(0);
    cx.notify();
}

/// The edit stage's keys, in the one order they can be read in:
///
/// 1. an armed [`Confirm`] owns **every** keystroke until it is answered
///    (`enter`/`y`) or cancelled (`escape`/`n`). It replaces the action
///    bar rather than adding a row, so nothing above it moves;
/// 2. `escape` walks the ladder, whose `PreviousStage` rung this stage
///    exists to reach — going back a stage, with nothing to discard
///    because every edit already applied;
/// 3. everything else goes through [`dialogmode::normal_command`], and a
///    key it does not claim is swallowed, exactly as in browse.
///
/// Every branch that changes the draft ends in [`revalidate`]: validation
/// is a parse of a few hundred bytes (spec §7.2), so it runs synchronously
/// on every change with no debounce, and the diagnostics on screen are
/// never one keystroke behind the value they describe.
fn handle_edit_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    // The same notice door `handle_browse_key` opens with, for the same
    // reason: a notice reports on the keystroke that produced it.
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }

    let armed = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .and_then(|draft| draft.confirm);
    if let Some(confirm) = armed {
        let bare = ks.mods == Modifiers::NONE;
        if bare && matches!(ks.key.as_str(), "enter" | "y") {
            disarm_confirm(shell);
            run_confirmed(shell, confirm, window, cx);
        } else if ks.key == "escape" || (bare && ks.key == "n") {
            cancel_confirm(shell);
        }
        // Anything else is claimed and dropped: while a destructive
        // question is on screen, a stray letter must not act on the
        // object behind it.
        cx.notify();
        return true;
    }

    if ks.key == "escape" {
        let step = shell.object_dialog.as_ref().map(|state| {
            dialogmode::escape_step(
                state.mode,
                state.query.is_empty(),
                state.has_previous_stage(),
            )
        });
        if step == Some(EscapeStep::PreviousStage) {
            // Straight back, with no discard question: there is nothing
            // unsaved to discard. Every field edit applied on the
            // keystroke that made it, and a fork the user has not
            // confirmed was taken back off the draft when they declined
            // it, so leaving the stage abandons exactly nothing.
            leave_edit(shell, window, cx);
            return true;
        }
        // `LeaveFilter` and `ClearQuery` are both unreachable here, and
        // by construction rather than by luck: `enter_edit` forces the
        // mode to `Normal` and empties the query, so this stage is always
        // the ladder's third rung. They are still folded into one `false`
        // rather than special-cased away, because `escape_step` is the
        // one ladder every modal surface walks and forking it per call
        // site is how the rungs drift apart. The `false` hands the
        // keystroke to the shell's modal branch, which closes the dialog.
        return false;
    }

    let Some(cmd) = dialogmode::normal_command(ks) else {
        return true;
    };
    match cmd {
        NormalCommand::Nav(nav) => {
            let selected = shell.object_dialog.as_mut().and_then(|state| {
                let draft = state.draft.as_mut()?;
                draft.selected = vimnav::apply(draft.selected, draft.rows().len(), nav);
                Some(draft.selected)
            });
            if let Some(selected) = selected {
                shell.object_dialog_scroll.scroll_to_item(selected);
            }
        }
        NormalCommand::Toggle => {
            let changed = draft_mut(shell).is_some_and(|draft| draft.toggle_selected());
            if changed {
                revalidate(shell);
                commit_or_confirm(shell, cx);
            } else {
                set_notice(shell, "nothing on this row changes with space".to_string());
            }
        }
        NormalCommand::MoveItem(delta) => {
            let moved = draft_mut(shell).is_some_and(|draft| draft.move_item(delta));
            if moved {
                let selected = shell
                    .object_dialog
                    .as_ref()
                    .and_then(|state| state.draft.as_ref())
                    .map(|draft| draft.selected)
                    .unwrap_or(0);
                shell.object_dialog_scroll.scroll_to_item(selected);
                commit_or_confirm(shell, cx);
            } else {
                set_notice(shell, "that is as far as this row goes".to_string());
            }
        }
        NormalCommand::Verb('d') => arm_delete(shell),
        NormalCommand::Verb('r') => arm_revert(shell),
        NormalCommand::EnterFilter => {
            // The one key the browse stage has that this one does not —
            // said out loud, because a `/` that silently did nothing
            // would read as the dialog having stopped responding.
            set_notice(
                shell,
                "the object's own rows are not filtered — escape goes back to the list".to_string(),
            );
        }
        // `enter` and `i` have no row to act on in a Views draft: its
        // fields are a choice and a list, and both are `space`'s.
        NormalCommand::Commit | NormalCommand::EditText => {
            set_notice(shell, "press space to change the selected row".to_string());
        }
        // A letter this stage has no verb for. Named rather than
        // dropped: `d` and `r` have just taught the user that
        // letters act here, so a silent `x` reads as the dialog having
        // stopped responding — and it is the one branch where the key
        // that did nothing is not otherwise on screen to explain itself.
        NormalCommand::Verb(letter) => {
            set_notice(shell, format!("{letter} is not a verb here"));
        }
    }
    cx.notify();
    true
}

/// The draft under the cursor, mutably, if the edit stage is open.
fn draft_mut(shell: &mut ShellView) -> Option<&mut Draft> {
    shell
        .object_dialog
        .as_mut()
        .and_then(|state| state.draft.as_mut())
}

/// Set the footer notice, if a dialog is open at all.
fn set_notice(shell: &mut ShellView, notice: String) {
    if let Some(state) = shell.object_dialog.as_mut() {
        state.notice = Some(notice);
    }
}

fn disarm_confirm(shell: &mut ShellView) {
    if let Some(draft) = draft_mut(shell) {
        draft.confirm = None;
    }
}

/// Say no to whatever is armed — the one door for `escape`, `n` and the
/// Cancel button, because a declined [`Confirm::Fork`] has a second half:
/// the keystroke that armed it already changed the draft, and leaving
/// that showing would be the one value on screen that is neither applied
/// nor persisted.
fn cancel_confirm(shell: &mut ShellView) {
    let armed = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .and_then(|draft| draft.confirm);
    disarm_confirm(shell);
    if armed == Some(Confirm::Fork)
        && let Some(draft) = draft_mut(shell)
    {
        draft.revert_to_baseline();
    }
}

/// Apply the change the keystroke just made — **now** — or ask first if
/// applying it would fork the object.
///
/// The one door every field edit leaves through, so there is one answer
/// to "does this apply instantly?" and one place the fork question is
/// asked. [`super::apply::commit_edit`] does the applying: the loader's
/// merge into memory, then the debounced file write behind it.
///
/// The fork check runs on the draft *after* the keystroke has changed it
/// (that is what makes `writes_by_destination` able to see a `Doc`
/// change at all), so declining the confirm has to put the field back —
/// `handle_edit_key`'s cancel branch does, from the baseline.
fn commit_or_confirm(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(domain) = shell.object_dialog.as_ref().map(|state| state.domain) else {
        return;
    };
    let dirty = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .is_some_and(|draft| draft.is_dirty());
    if !dirty {
        return;
    }
    if super::apply::would_fork(shell, domain) {
        if let Some(draft) = draft_mut(shell) {
            draft.confirm = Some(Confirm::Fork);
        }
        cx.notify();
        return;
    }
    if let Some(notice) = super::apply::commit_edit(shell, cx) {
        set_notice(shell, notice);
    }
}

/// Re-run [`Domain::validate`] over the draft as it now stands.
fn revalidate(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    let domain = state.domain;
    let Some(draft) = state.draft.as_mut() else {
        return;
    };
    // Validated, then stored: `validate` needs the draft immutably and
    // the config from a sibling field, which is exactly the disjoint
    // borrow the compiler allows here and a `&mut self` method would not.
    let diagnostics = domain.validate(draft, &shell.services.config);
    if let Some(draft) = draft_mut(shell) {
        draft.diagnostics = diagnostics;
    }
}

/// Back to the browse list, with the cursor put back on the object just
/// edited — by name, because the list it returns to is unfiltered and so
/// is a different list from the one the object was opened out of.
fn leave_edit(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let name = match shell.object_dialog.as_ref().map(|state| &state.stage) {
        Some(Stage::Edit { object }) => object.clone(),
        _ => String::new(),
    };
    if let Some(state) = shell.object_dialog.as_mut() {
        state.leave_edit();
    }
    let rows = derive_rows(shell);
    if let Some(state) = shell.object_dialog.as_mut() {
        let visible = super::visible_rows(state, &rows);
        state.selected = super::filtered_position(&visible, &rows, &name).unwrap_or(0);
    }
    let selected = shell
        .object_dialog
        .as_ref()
        .map(|state| state.selected)
        .unwrap_or(0);
    shell.object_dialog_scroll.scroll_to_item(selected);
    shell.focus_handle.focus(window, cx);
    cx.notify();
}

/// The browse row for the object being edited — where `layer` and
/// `overridden` come from, so `d` and `r` are gated by the one tested
/// derivation rather than by a second guess made here.
fn editing_row(shell: &ShellView) -> Option<ObjectRow> {
    let name = match shell.object_dialog.as_ref().map(|state| &state.stage) {
        Some(Stage::Edit { object }) => object.clone(),
        _ => return None,
    };
    derive_rows(shell).into_iter().find(|row| row.name == name)
}

/// Remove `name` from each of `docs` in the user layer, off the render
/// thread. Only docs whose **user layer** actually contains the object are
/// opened, so a delete never creates an empty file to say nothing.
fn spawn_removals(
    shell: &mut ShellView,
    docs: &[&'static str],
    cx: &mut Context<ShellView>,
) -> Result<Vec<String>, String> {
    let name = match shell.object_dialog.as_ref().map(|state| &state.stage) {
        Some(Stage::Edit { object }) => object.clone(),
        _ => return Err("nothing is open".to_string()),
    };
    let touched: Vec<&'static str> = docs
        .iter()
        .copied()
        .filter(|doc| {
            shell
                .services
                .config
                .layered_docs(doc)
                .iter()
                .any(|layered| layered.layer == Layer::User && layered.table.contains_key(&name))
        })
        .collect();
    if touched.is_empty() {
        return Err(format!("nothing of yours defines {name}"));
    }
    let Some(user_dir) = shell.user_dir.clone() else {
        return Err("no writable user config directory — nothing was removed".to_string());
    };
    let files: Vec<String> = touched.iter().map(|doc| format!("{doc}.toml")).collect();
    cx.background_executor()
        .spawn(async move {
            for doc in touched {
                if let Err(e) = config_write::edit(&user_dir, Layer::User, doc, |document| {
                    document.remove(&name);
                }) {
                    eprintln!("[config] warning: {e}");
                }
            }
        })
        .detach();
    Ok(files)
}

/// `d`: arm the delete confirm, or say why there is nothing to delete.
///
/// Only the user layer is ever written (spec §5.3), so deleting is only
/// meaningful on an object the user's own layer defines. On a desk or
/// builtin object the write would remove nothing and the object would
/// still be there — a verb that appears to have failed, which is worse
/// than one that explains itself.
///
/// The gate is the winning layer, not `overridden`, because presentation
/// forks nothing: a desk view a trader has hidden a column on is still
/// the desk's, and there is no *view* of theirs to delete. What there is
/// is an override to revert, so the refusal names `r` rather than
/// claiming they have nothing — the object reached this branch with
/// `overridden` set only via `view_presentation.toml`, since a user-layer
/// doc override would have made the user the winning layer above.
fn arm_delete(shell: &mut ShellView) {
    match editing_row(shell) {
        Some(row) if row.layer == Layer::User => {
            if let Some(draft) = draft_mut(shell) {
                draft.confirm = Some(Confirm::Delete);
            }
        }
        Some(row) => {
            let tail = if row.overridden {
                " — but r reverts your changes to it"
            } else {
                ""
            };
            set_notice(
                shell,
                format!(
                    "{} comes from the {} layer — there is nothing of yours to delete{tail}",
                    row.name,
                    row.layer.name()
                ),
            )
        }
        None => set_notice(shell, "nothing is open".to_string()),
    }
}

/// `r`: arm the revert confirm, or say why there is nothing to revert.
///
/// Gated on `overridden`, not on the winning layer: reverting deletes the
/// user's copy, and on an object only the user layer defines that would
/// delete the object outright instead of restoring anything.
///
/// `overridden` counts a user-layer `view_presentation.toml` entry as an
/// override (`objectdialog::derive_rows`), which is what makes `r` the
/// undo for hiding a column — the commonest edit §4.1's split exists to
/// make cheap, and the one whose override never reaches `views.toml`.
fn arm_revert(shell: &mut ShellView) {
    match editing_row(shell) {
        Some(row) if row.overridden => {
            if let Some(draft) = draft_mut(shell) {
                draft.confirm = Some(Confirm::Revert);
            }
        }
        Some(row) => set_notice(
            shell,
            format!("{} has no user override to revert", row.name),
        ),
        None => set_notice(shell, "nothing is open".to_string()),
    }
}

/// Carry out the destructive act the second keystroke just confirmed.
///
/// Delete and revert remove the same two things — the object from the
/// domain's own user-layer doc, and its presentation table — and differ
/// only in which precondition let the user reach them and what the notice
/// says. The presentation table goes with the object on purpose: leaving
/// it behind would strand a table naming a view that no longer exists,
/// which is exactly the stale entry `ViewPresentationSpec::apply` warns
/// about at startup and nowhere else.
fn run_confirmed(
    shell: &mut ShellView,
    confirm: Confirm,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let domain = match shell.object_dialog.as_ref() {
        Some(state) => state.domain,
        None => return,
    };
    match confirm {
        // The fork was the point of the question; answering yes applies
        // exactly the edit that armed it, down the one commit path.
        Confirm::Fork => {
            if let Some(notice) = super::apply::commit_edit(shell, cx) {
                set_notice(shell, notice);
            }
            cx.notify();
        }
        Confirm::Delete | Confirm::Revert => {
            let docs = [
                Destination::Doc.doc(domain),
                Destination::Presentation.doc(domain),
            ];
            match spawn_removals(shell, &docs, cx) {
                Ok(files) => {
                    let verb = if confirm == Confirm::Delete {
                        "deleting"
                    } else {
                        "reverting"
                    };
                    let name = match shell.object_dialog.as_ref().map(|state| &state.stage) {
                        Some(Stage::Edit { object }) => object.clone(),
                        _ => String::new(),
                    };
                    leave_edit(shell, window, cx);
                    set_notice(shell, format!("{verb} {name} in {}…", files.join(" and ")));
                }
                Err(message) => set_notice(shell, message),
            }
        }
    }
}

/// One action the edit stage's bar offers: its key, its label, and whether
/// it is the destructive one. Built as data so the bar and the footer hint
/// cannot disagree about which verbs are live.
struct Action {
    key: &'static str,
    label: String,
    destructive: bool,
}

/// The actions available on the object being edited, in the order they are
/// painted.
///
/// **No save.** There is nothing to save: a field edit applied on the
/// keystroke that made it. What is left is exactly the verbs that are
/// destructive or structural — `d` deletes the user's copy, `r` throws a
/// personal override away — plus, as a confirm rather than a standing
/// button, `Copy to user layer` ([`Confirm::Fork`]), which the first
/// definitional edit to a desk object arms.
///
/// An earlier build put `Save changes` here whenever the draft was dirty,
/// relabelled `Copy to user layer` when saving would fork. The fork
/// warning survives; the save does not, because "dirty" is no longer a
/// state this dialog can be in.
fn actions(shell: &ShellView) -> Vec<Action> {
    let Some(state) = shell.object_dialog.as_ref() else {
        return Vec::new();
    };
    if state.draft.is_none() {
        return Vec::new();
    }
    let row = editing_row(shell);
    let mut out = Vec::new();
    if row.as_ref().is_some_and(|r| r.layer == Layer::User) {
        out.push(Action {
            key: "d",
            label: format!("Delete this {}", object_word(state.domain)),
            destructive: true,
        });
    }
    if row.as_ref().is_some_and(|r| r.overridden) {
        out.push(Action {
            key: "r",
            label: "Revert to desk".to_string(),
            destructive: true,
        });
    }
    out
}

/// The singular noun one of this domain's objects goes by, for a label
/// that has to read as English (`Delete this view`).
fn object_word(domain: Domain) -> String {
    let title = domain.title().to_lowercase();
    title.strip_suffix('s').unwrap_or(&title).to_string()
}

/// The [`dialog::ShellModal::build`] closure body: the mode pill, the
/// shared filter row, the scrollable browse list, and a muted footer
/// stating the current mode's vocabulary. `entity` is what each row's
/// click handler captures to reach [`on_row_clicked`] later, at click
/// time — `shell` is this call's own plain-borrow read (see
/// `ShellModal::build`'s doc comment for why the two are both needed).
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.object_dialog.as_ref() else {
        return div().into_any_element();
    };
    if matches!(state.stage, Stage::Edit { .. }) {
        return build_edit(shell, entity, cx);
    }
    // The same one derivation path `handle_key` uses — a second spelling
    // here is how a render and its key handling come to disagree about
    // which rows exist.
    let rows = derive_rows(shell);
    let theme = cx.theme();
    // Copied out so the row closures below don't hold the `theme` borrow.
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;

    let visible = super::visible_rows(state, &rows);

    let mut list = v_flex()
        .id("objectdialog-list")
        .w(px(WIDTH))
        .h(px(
            (visible.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT)
        ))
        .overflow_y_scroll()
        .track_scroll(&shell.object_dialog_scroll)
        .debug_selector(|| "objectdialog-list".to_string());

    for (position, m) in visible.iter().enumerate() {
        let Some(row) = rows.get(m.row) else { continue };
        let is_selected = position == state.selected;

        let name_len = row.name.chars().count();
        // The same `"{a} {b}"` split both list dialogs use — this is its
        // third consumer, and the reason it lives in one place: the
        // arithmetic is only correct while every `searchable_text` in the
        // crate keeps that exact shape.
        let (name_ix, summary_ix) = split_label_indices(&m.indices, name_len);

        let mut row_el = h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .gap_3()
            .px_2()
            .py_1()
            .rounded(px(4.));
        if is_selected {
            row_el = row_el.bg(theme.selection).text_color(theme.primary);
        }

        let label = v_flex()
            .gap_0p5()
            .child(highlighted_text(&row.name, &name_ix, theme.primary))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(highlighted_text(&row.summary, &summary_ix, theme.primary)),
            );

        // Provenance, right-aligned: the layer that won as plain muted
        // text, and `overridden` as a muted pill beside it. Both muted
        // and neither coloured — an override is a classification, not a
        // warning, and spending a semantic colour on it here would leave
        // nothing louder for the states that mean something is wrong.
        let mut markers = h_flex().gap_1().items_center().child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(row.layer.name()),
        );
        if row.overridden {
            markers = markers.child(
                div()
                    .text_xs()
                    .text_color(chip_fg)
                    .bg(chip_bg)
                    .px_1()
                    .py_0p5()
                    .rounded(px(4.))
                    .flex_shrink_0()
                    .debug_selector({
                        let name = row.name.clone();
                        move || format!("objectdialog-overridden-{name}")
                    })
                    .child("overridden"),
            );
        }

        let entity_for_row = entity.clone();
        let clicked = row.name.clone();
        let selector_name = row.name.clone();
        let row_el = row_el
            .child(label)
            .child(markers)
            // Keyed by the object's own name, not its index: the list is
            // re-ranked under the cursor by every keystroke, so a
            // position-keyed selector (or click handler) would name a
            // different row from one frame to the next.
            .debug_selector(move || format!("objectdialog-row-{selector_name}"))
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                entity_for_row.update(cx, |shell, cx| {
                    on_row_clicked(shell, &clicked, window, cx);
                });
            });

        list = list.child(row_el);
    }

    if visible.is_empty() {
        // Two different empty states, said differently on purpose: an
        // over-narrow filter is a state the user can back out of, while
        // a domain with nothing in it is a fact about the config.
        let message = if rows.is_empty() {
            format!("no {} are configured", state.domain.title().to_lowercase())
        } else {
            "no matches".to_string()
        };
        list = list.child(
            div()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "objectdialog-empty".to_string())
                .child(message),
        );
    }

    let chip = move |spec: &str| {
        let ks = crate::keymap::parse_keystroke(spec, Modifiers::NONE)
            .expect("footer hint keystrokes are hardcoded valid");
        key_chip(&ks, chip_fg, chip_bg)
    };
    let sep = |text: &'static str| div().child(text).into_any_element();

    // The hint row states the CURRENT mode's vocabulary, never the union
    // of both: a modal surface's whole risk is a user who cannot tell
    // which mode they are in, and a footer listing keys that are inert
    // right now is exactly the lie the mode pill exists to prevent.
    let (motion, action): (Vec<AnyElement>, Vec<AnyElement>) = match state.mode {
        DialogMode::Normal => (
            vec![
                chip("j"),
                chip("k"),
                sep("move ·"),
                chip("ctrl+d"),
                chip("ctrl+u"),
                sep("±5 ·"),
                chip("ctrl+f"),
                chip("ctrl+b"),
                sep("±10"),
            ],
            vec![
                chip("/"),
                sep("filter ·"),
                chip("escape"),
                // Honest about which rung the next escape takes: with a
                // query still applied it clears the query, and only then
                // closes.
                sep(if state.query.is_empty() {
                    "close"
                } else {
                    "clear the filter"
                }),
            ],
        ),
        DialogMode::Filter => (
            vec![
                sep("type to filter ·"),
                chip("up"),
                chip("down"),
                sep("move ·"),
                chip("ctrl+d"),
                chip("ctrl+u"),
                sep("±5 ·"),
                chip("ctrl+f"),
                chip("ctrl+b"),
                sep("±10"),
            ],
            vec![chip("escape"), sep("back to normal")],
        ),
    };

    let footer = v_flex()
        .w(px(WIDTH))
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(theme.border)
        // A notice sits above the hints in `theme.warning`: it reports a
        // keystroke that deliberately did nothing, and the eye is already
        // going to the footer for the hints.
        .children(state.notice.as_ref().map(|notice| {
            div()
                .text_sm()
                .text_color(theme.warning)
                .debug_selector(|| "objectdialog-notice".to_string())
                .child(notice.clone())
        }))
        .child(
            div().text_sm().text_color(theme.muted_foreground).child(
                v_flex()
                    .gap_0p5()
                    .child(h_flex().gap_1().items_center().flex_wrap().children(motion))
                    .child(h_flex().gap_1().items_center().flex_wrap().children(action)),
            ),
        );

    // The live `Input` renders only when it actually owns the keystrokes.
    // In normal mode the same query paints as static muted text — see
    // this module's own "one switch" note.
    let frozen_query = (state.mode == DialogMode::Normal).then_some(state.query.as_str());

    v_flex()
        .gap_2()
        .child(
            h_flex()
                .w(px(WIDTH))
                .items_center()
                .justify_end()
                .child(dialog::mode_pill(state.mode, cx)),
        )
        .child(dialog::filter_row(&shell.dialog_input, frozen_query, cx))
        .child(list)
        .child(footer)
        .into_any_element()
}

/// The edit stage: the object's header, its diagnostics, the scrolling
/// row list, and — **outside** that scroll — the action bar.
///
/// The bar sits outside the list on purpose (spec §3.2). An earlier
/// design made every verb a row, and the list then changed length the
/// moment a draft went dirty: a `Save changes` row appeared under the
/// cursor and the row the user was aiming at moved. Keeping the verbs
/// below the scroll means nothing above them ever shifts.
fn build_edit(shell: &ShellView, entity: &Entity<ShellView>, cx: &mut App) -> AnyElement {
    let Some(state) = shell.object_dialog.as_ref() else {
        return div().into_any_element();
    };
    let Some(draft) = state.draft.as_ref() else {
        return div().into_any_element();
    };
    // Built before the theme is borrowed, because both halves of it want
    // `cx` mutably and `cx.theme()` holds it immutably for the rest of
    // this function.
    let action_block = match draft.confirm {
        Some(confirm) => confirm_row(confirm, &draft.name, entity, cx),
        None => action_bar(shell, entity, cx),
    };
    let theme = cx.theme();
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    let row = editing_row(shell);

    // The object header: its name, and the same two provenance markers
    // the browse row carries, so opening an object never loses the
    // context the list gave it.
    let mut header = h_flex()
        .w(px(WIDTH))
        .items_center()
        .justify_between()
        .gap_3()
        .child(div().text_lg().child(draft.name.clone()))
        .debug_selector(|| "objectdialog-edit-header".to_string());
    if let Some(row) = row.as_ref() {
        let mut markers = h_flex().gap_1().items_center().child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(row.layer.name()),
        );
        if row.overridden {
            markers = markers.child(
                div()
                    .text_xs()
                    .text_color(chip_fg)
                    .bg(chip_bg)
                    .px_1()
                    .py_0p5()
                    .rounded(px(4.))
                    .flex_shrink_0()
                    .child("overridden"),
            );
        }
        header = header.child(markers);
    }

    let rows = draft.rows();
    let mut list = v_flex()
        .id("objectdialog-fields")
        .w(px(WIDTH))
        .h(px(
            (rows.len().max(1) as f32 * FIELD_ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT)
        ))
        .overflow_y_scroll()
        .track_scroll(&shell.object_dialog_scroll)
        .debug_selector(|| "objectdialog-fields".to_string());

    for (position, edit_row) in rows.iter().enumerate() {
        let is_selected = position == draft.selected;
        let mut element = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .px_2()
            .py_1()
            .rounded(px(4.));
        if is_selected {
            element = element.bg(theme.selection).text_color(theme.primary);
        }
        let (selector, label, value) = match *edit_row {
            EditRow::Field(index) => {
                let field = &draft.fields[index];
                (
                    format!("objectdialog-field-{}", field.key),
                    div().child(field.label.clone()).into_any_element(),
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(field_value(field))
                        .into_any_element(),
                )
            }
            EditRow::Item { field, item } => {
                let FieldKind::OrderedList { items } = &draft.fields[field].kind else {
                    continue;
                };
                let Some(entry) = items.get(item) else {
                    continue;
                };
                // The tick is the inclusion state, and a hidden item is
                // muted as well as unticked — one signal is a thing a
                // glance misses on a 30-row list.
                let mark = if entry.included { "[x]" } else { "[ ]" };
                let name = div()
                    .pl_4()
                    .when(!entry.included, |d| d.text_color(theme.muted_foreground))
                    .child(format!("{mark}  {}", entry.name));
                let width = match entry.width {
                    Some(width) => format!("{width:.0}px"),
                    None => "auto".to_string(),
                };
                (
                    format!("objectdialog-item-{}", entry.name),
                    name.into_any_element(),
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(width)
                        .into_any_element(),
                )
            }
        };
        let entity_for_row = entity.clone();
        let clicked = position;
        list = list.child(
            element
                .child(label)
                .child(value)
                .debug_selector(move || selector.clone())
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    entity_for_row.update(cx, |shell, cx| {
                        on_edit_row_clicked(shell, clicked, window, cx);
                    });
                }),
        );
    }

    // Diagnostics for the object as a whole. Attaching each to the field
    // whose `key` matches its `path` is spec §8.5's shape and needs
    // `Diagnostic::path`, which no reader carries yet — see
    // `Draft::diagnostics`.
    let diagnostics = v_flex().w(px(WIDTH)).gap_0p5().children(
        draft
            .diagnostics
            .iter()
            .map(|diagnostic| {
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .debug_selector(|| "objectdialog-diagnostic".to_string())
                    .child(diagnostic.message.clone())
            })
            .collect::<Vec<_>>(),
    );

    let chip = move |spec: &str| {
        let ks = crate::keymap::parse_keystroke(spec, Modifiers::NONE)
            .expect("footer hint keystrokes are hardcoded valid");
        key_chip(&ks, chip_fg, chip_bg)
    };
    let sep = |text: &'static str| div().child(text).into_any_element();
    // The hint row states this stage's vocabulary and only this stage's —
    // the same rule the browse footer keeps.
    let (motion, action): (Vec<AnyElement>, Vec<AnyElement>) = if draft.confirm.is_some() {
        (
            vec![sep("this needs an answer first")],
            vec![
                chip("enter"),
                sep("go ahead ·"),
                chip("escape"),
                sep("leave it alone"),
            ],
        )
    } else {
        (
            vec![
                chip("j"),
                chip("k"),
                sep("move ·"),
                chip("space"),
                sep("change ·"),
                chip("shift+j"),
                chip("shift+k"),
                sep("reorder"),
            ],
            vec![chip("escape"), sep("back to the list")],
        )
    };

    let footer = v_flex()
        .w(px(WIDTH))
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(theme.border)
        .children(state.notice.as_ref().map(|notice| {
            div()
                .text_sm()
                .text_color(theme.warning)
                .debug_selector(|| "objectdialog-notice".to_string())
                .child(notice.clone())
        }))
        .child(
            div().text_sm().text_color(theme.muted_foreground).child(
                v_flex()
                    .gap_0p5()
                    .child(h_flex().gap_1().items_center().flex_wrap().children(motion))
                    .child(h_flex().gap_1().items_center().flex_wrap().children(action)),
            ),
        );

    v_flex()
        .gap_2()
        .child(header)
        .child(diagnostics)
        .child(list)
        .child(action_block)
        .child(footer)
        .into_any_element()
}

/// What a field row shows on its right-hand side.
fn field_value(field: &super::Field) -> String {
    match &field.kind {
        FieldKind::Text(text) => text.clone(),
        FieldKind::Number { value, .. } => value.to_string(),
        FieldKind::Bool(value) => if *value { "yes" } else { "no" }.to_string(),
        FieldKind::Choice { options, selected } => options
            .get(*selected)
            .cloned()
            .unwrap_or_else(|| "—".to_string()),
        FieldKind::MultiChoice { ticked, .. } => match ticked.len() {
            0 => "none".to_string(),
            n => format!("{n} selected"),
        },
        FieldKind::OrderedList { items } => {
            let hidden = items.iter().filter(|i| !i.included).count();
            match (items.len(), hidden) {
                (1, 0) => "1 column".to_string(),
                (n, 0) => format!("{n} columns"),
                (n, h) => format!("{n} columns · {h} hidden"),
            }
        }
    }
}

/// The action bar: every live verb as a button showing its own letter.
///
/// Buttons, not rows and not bare keys. A key alone has no clickable
/// target, and every other verb in Geode's dialogs has one; a `ghost`
/// button is the quiet variant the design guide calls for in a local
/// command bar, and the destructive ones are `danger` rather than merely
/// worded strongly.
fn action_bar(shell: &ShellView, entity: &Entity<ShellView>, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    let mut bar = h_flex()
        .w(px(WIDTH))
        .gap_2()
        .items_center()
        .debug_selector(|| "objectdialog-actions".to_string());
    for action in actions(shell) {
        let ks = crate::keymap::parse_keystroke(action.key, Modifiers::NONE)
            .expect("action keys are hardcoded valid");
        let entity_for_action = entity.clone();
        let key = action.key;
        let selector = format!("objectdialog-action-{key}");
        let mut button = Button::new(gpui::SharedString::from(format!("objectdialog-{key}")))
            .small()
            .ghost()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(key_chip(&ks, chip_fg, chip_bg))
                    .child(action.label.clone()),
            )
            .on_click(move |_event, window, cx| {
                entity_for_action.update(cx, |shell, cx| {
                    press_verb(shell, key, window, cx);
                });
            });
        if action.destructive {
            button = button.danger();
        }
        bar = bar.child(div().debug_selector(move || selector.clone()).child(button));
    }
    bar.into_any_element()
}

/// The confirm block, which **replaces** the action bar rather than
/// joining it: one question, two answers, and nothing above it moves.
fn confirm_row(
    confirm: Confirm,
    name: &str,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let go_ahead = entity.clone();
    let leave_it = entity.clone();
    h_flex()
        .w(px(WIDTH))
        .gap_3()
        .items_center()
        .debug_selector(|| "objectdialog-confirm".to_string())
        .child(
            div()
                .text_sm()
                .text_color(theme.warning)
                .child(confirm.prompt(name)),
        )
        .child(
            Button::new("objectdialog-confirm-yes")
                .small()
                .danger()
                .label(match confirm {
                    Confirm::Delete => "Delete",
                    Confirm::Revert => "Revert",
                    Confirm::Fork => "Copy to user layer",
                })
                .on_click(move |_event, window, cx| {
                    go_ahead.update(cx, |shell, cx| {
                        let armed = shell
                            .object_dialog
                            .as_ref()
                            .and_then(|state| state.draft.as_ref())
                            .and_then(|draft| draft.confirm);
                        if let Some(confirm) = armed {
                            disarm_confirm(shell);
                            run_confirmed(shell, confirm, window, cx);
                        }
                    });
                }),
        )
        .child(
            Button::new("objectdialog-confirm-no")
                .small()
                .ghost()
                .label("Cancel")
                .on_click(move |_event, _window, cx| {
                    leave_it.update(cx, |shell, cx| {
                        cancel_confirm(shell);
                        cx.notify();
                    });
                }),
        )
        .into_any_element()
}

/// A verb pressed with the mouse instead of the keyboard. One door, so a
/// button and its letter can never do different things.
fn press_verb(shell: &mut ShellView, key: &str, _window: &mut Window, cx: &mut Context<ShellView>) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    match key {
        "d" => arm_delete(shell),
        "r" => arm_revert(shell),
        _ => {}
    }
    cx.notify();
}

/// A click on an edit-stage row moves the draft's cursor there — the
/// mouse's half of `j`/`k`, and the reason a click never also acts: the
/// verb is a second, deliberate keystroke or button press.
fn on_edit_row_clicked(
    shell: &mut ShellView,
    position: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    if let Some(draft) = draft_mut(shell) {
        if position >= draft.rows().len() {
            return;
        }
        draft.selected = position;
    }
    shell.object_dialog_scroll.scroll_to_item(position);
    shell.focus_handle.focus(window, cx);
    cx.notify();
}
