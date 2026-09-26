//! GPUI integration for configuration-object dialogs: opening, modal key routing, stage
//! transitions, browse rows, fields, and actions. Pure dialog state owns mode, queries,
//! drafts, and confirmations; `dialog::sync_dialog_text` reconciles the shared input
//! and focus after keyboard and pointer transitions.
//!
//! Normal mode keeps the input blurred so bare letters reach navigation and edit verbs.
//! Filter and field-entry modes give text to the focused input. Stage entry clears
//! unrelated queries and sets its mode explicitly. Pointer handlers use the same
//! edit/commit helpers and run input synchronization themselves.
//!
//! Object, column, and Values stages paint the draft immediately. Accepted changes join
//! the shared persistence batch; there is no save key. Forking an inherited definition
//! is announced, while deletion, reversion, and overwriting a user-owned scope require
//! confirmation. Confirmation blocks unrelated keyboard and pointer mutations until
//! answered.
//!
//! Stage entry derives from active configuration with pending edits folded in,
//! preventing a reopened draft from overwriting changes still awaiting promotion. An
//! already-open draft remains its edit buffer; browse rows follow configuration and can
//! trail changes until the flush. Persistence outcomes and recovery are documented in
//! `apply`.

use std::rc::Rc;

use geode_core::config::{Layer, Severity, check_object_name};
use geode_core::query::{DistinctOutcome, DistinctParams};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Div, Entity, MouseButton, Window, div, rems};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};

use super::apply;
use super::colours;
use super::dataset_columns;
use super::schema;
use super::scopes;
use super::sources;
use super::views;
use super::{
    ColumnContext, ColumnDoor, ColumnLayers, Completions, Confirm, Destination, Domain, Draft,
    EditRow, FellTo, FieldKind, Fold, NameSeed, ObjectDialogState, ObjectRow, READ_ONLY_NOTICE,
    RowDrag, RowVocabulary, Stage, Step,
};
use crate::dialogmode::{self, DialogMode, EscapeStep, NormalCommand};
use crate::footer::{Hint, HintRow};
use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;
use crate::vimnav;

use super::super::{SCOPES_KEY, ShellEvent, ShellView};
// Aliased: `colours` (unqualified, `use super::colours;` above) is the
// `Domain::Colors` adapter; this is `shell::colours`, the gpui<->pure theme bridge — a
// different module, one directory further out, that the adapter itself never touches.
use super::super::colours as colour_theme;
use super::super::control::{self, PointerStates as _};
use super::super::dialog;
use super::super::kbd;
use super::super::keybindings_view::{highlighted_text, split_label_indices};
use super::super::scale;

/// Browse row height estimate used to size its capped viewport. Scroll following uses
/// measured layout through `scroll_to_item`. Edit rows can include section headers, so
/// their viewport uses actual content height with a cap rather than multiplying this
/// estimate by the row count.
const ROW_HEIGHT: f32 = 44.0;
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
        // This dialog's mode owns focus. Install its state before opening so shared
        // input synchronization sees Normal mode and blurs the input for command keys.
        false,
    );
    // the crumb plus the pill, sharing the same title-row slot every Geode modal has —
    // see `crumb_text`'s own doc for what the crumb says in each stage.
    dialog::set_title_extra(view, |shell, cx| {
        let state = shell.object_dialog.as_ref();
        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .font_family(crate::fonts::MONO)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .debug_selector(|| "objectdialog-crumb".to_string())
                    .child(crumb_text(shell)),
            )
            // a value field runs in `Filter` (that is what gives it the keys), but
            // "filter" is the wrong word for a field whose text is the value it will
            // apply — the pill says `edit`, or `chain` for the chain field's own case.
            // A `Choice` row's typeahead reads `choose`.
            .children(
                state.map(|s| match s.draft.as_ref().and_then(|d| d.text_entry) {
                    Some(entry) if entry.completions == Completions::Chain => {
                        dialog::chain_pill(cx)
                    }
                    Some(entry) if entry.completions == Completions::Choice => {
                        dialog::choose_pill(cx)
                    }
                    Some(_) => dialog::edit_pill(cx),
                    None => dialog::mode_pill(s.mode, cx),
                }),
            )
            .into_any_element()
    });
}

/// The title-row crumb: a count in browse and naming, the slot's chord in a Groupings
/// edit, the object and column in the column stage, nothing otherwise. Pure so a test
/// can read it without laying out a window.
pub(crate) fn crumb_text(shell: &ShellView) -> String {
    let Some(state) = shell.object_dialog.as_ref() else {
        return String::new();
    };
    match &state.stage {
        Stage::Edit { object } if state.domain == Domain::Groupings => format!("ctrl+{object}"),
        Stage::Edit { .. } => String::new(),
        // The one crumb that is a PATH rather than a count or a chord:
        // the edit header still paints the object's name alone, so
        // without this the stage would say nothing about which column it
        // is editing. Same arrow the blotter's own rollup path uses.
        Stage::Column { object, column } => format!("{object} › {column}"),
        // Same shape as the column stage's crumb ("Crumb `<object> › <column>`") — the
        // Values stage is a projection over one column exactly as the column stage is.
        Stage::Values { object, column } => format!("{object} › {column}"),
        Stage::Browse | Stage::Naming => {
            let n = derive_rows(shell).len();
            format!("{n} {}", state.domain.crumb_noun())
        }
    }
}

/// The [`dialog::ModalKeyHandler`] for this dialog: the front door every
/// stage comes through, splitting on the stage and on nothing else.
///
/// The split is here rather than inside each branch so the three stages
/// cannot drift on the things they must agree about — which keys are
/// claimed, when the notice is dropped, and which `escape` rung applies.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    _window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    match shell.object_dialog.as_ref().map(|s| &s.stage) {
        // Column and Values stages use the edit key table over their projected draft
        // fields; their Enter and Escape targets depend on the current projection.
        Some(Stage::Edit { .. } | Stage::Column { .. } | Stage::Values { .. }) => {
            handle_edit_key(shell, ks, cx)
        }
        Some(Stage::Naming) => handle_naming_key(shell, ks, cx),
        _ => handle_browse_key(shell, ks, cx),
    }
}

/// Browse key routing. Normal mode consumes stray keys and uses the Escape ladder;
/// Filter mode leaves printable text to the input. Escape restores the entry query;
/// bare Enter keeps the typed query. Both return to Normal without opening a row.
/// Normal-mode Enter opens the selected object. Tab is consumed so it cannot insert
/// a literal tab into the filter. The ladder's close rung remains unclaimed for the
/// shell's modal handler.
fn handle_browse_key(shell: &mut ShellView, ks: &Keystroke, cx: &mut Context<ShellView>) -> bool {
    let rows = derive_rows(shell);
    // read before `state` takes its `&mut` borrow of `shell.object_dialog` below, whose
    // lifetime spans the rest of this function — `seed_dataset_under_cursor` needs a
    // plain `&ShellView`, which a live sibling `&mut` borrow would refuse. `None` on
    // any domain but Sources (the function's own first check), so this costs every
    // other domain nothing but the check itself.
    let seed = seed_dataset_under_cursor(shell);
    let seed_taken = seed
        .as_deref()
        .is_some_and(|d| Domain::Sources.name_taken(&shell.services.config, d));
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

    // A confirmation owns input in browse as well as edit, ahead of the Escape ladder
    // and ordinary commands.
    if state.confirm.is_some() {
        match dialog::ConfirmAnswer::from_key(ks) {
            Some(dialog::ConfirmAnswer::Yes) => answer_confirm(shell, true, cx),
            Some(dialog::ConfirmAnswer::No) => answer_confirm(shell, false, cx),
            // Claimed and dropped: a stray letter must not act on the
            // object behind the question — nor move the cursor off it,
            // which is what keeps `target_object` the same row at answer
            // time as at arming time.
            None => {}
        }
        cx.notify();
        return true;
    }

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
                    // Clearing the pure query is the whole rung:
                    // `dialog::sync_dialog_text` empties the shared `Input` from it on
                    // this handler's return, so the old query cannot be left waiting in
                    // the field for the next `/`.
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
                // The one switch, thrown the other way — a pure mutation:
                // `dialog::sync_dialog_text` gives the filter focus on
                // this handler's return, and printable keys become text
                // again. Through `enter_filter` so the query is recorded
                // for the `escape` that backs out of the search.
                state.enter_filter();
            }
            NormalCommand::Commit => {
                open_selected(shell, cx);
                return true;
            }
            // `n`: the naming stage, or the domain's refusal — `begin_new_object`, the
            // one door the browse bar's button takes too. `state`'s borrow ends at the
            // match arm, so the door can take `shell` whole.
            NormalCommand::Verb('n') => {
                begin_new_object(shell, seed.clone(), seed_taken);
                cx.notify();
                return true;
            }
            // `c`: copy the row under the cursor under a new name —
            // `Domain::duplicable` alone, so an unclaimed `c` on every other domain
            // falls through to the `_` arm below like any other letter with no meaning
            // here.
            NormalCommand::Verb('c') if state.domain.duplicable() => {
                let name = selected_row(state, &rows, &visible).map(|r| r.name.clone());
                match name {
                    Some(name) => begin_copy(shell, name),
                    None => set_notice(shell, "nothing selected to copy".to_string()),
                }
                cx.notify();
                return true;
            }
            // Browse deletion and reversion use the same target gates as edit actions.
            NormalCommand::Verb(verb @ ('d' | 'r')) => {
                if !state.domain.writable(&state.stage) {
                    set_notice(shell, READ_ONLY_NOTICE.to_string());
                } else if verb == 'd' {
                    arm_delete(shell);
                } else {
                    arm_revert(shell);
                }
                cx.notify();
                return true;
            }
            // a bare digit names a slot on the one domain whose objects are numbered;
            // elsewhere it is dropped below.
            NormalCommand::Digit(n) if state.domain == Domain::Groupings => {
                jump_to_slot(shell, n, cx);
                return true;
            }
            // `Toggle`, `EditText`, `MoveItem` and the rest of the letter
            // verbs are the edit stage's, and the browse footer
            // advertises none of them — so they are claimed and dropped
            // here like any other unclaimed key.
            _ => {}
        }
        cx.notify();
        return true;
    }

    // ---- Filter mode -------------------------------------------------

    if let Some(exit) = dialogmode::filter_exit(ks) {
        // The ladder's first rung and its twin, both claimed (`true`):
        // falling through on `escape` would close the whole dialog on the
        // keystroke that was only meant to leave the search. `escape`
        // puts the entry query back, `enter` keeps what was typed, and
        // neither opens the row under the cursor — that is normal mode's
        // own `enter`, one keystroke later. The
        // blur `dialog::sync_dialog_text` performs on this handler's
        // return is what makes the letters motions again, and it writes
        // whichever query survives back into the `Input`.
        if state.exit_filter(exit) {
            // The list re-expands under a scroll offset still parked
            // where the narrowed list left it; `exit_filter` has already
            // put the cursor on the top match.
            shell.object_dialog_scroll.scroll_to_item(0);
        }
        cx.notify();
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

/// The naming stage's keys: `escape` backs out to browse with nothing written; `enter`
/// checks the name and creates; everything else is the focused `Input`'s to type. The
/// name is `state.query` — mirrored from the field by the same subscription a filter
/// uses.
fn handle_naming_key(shell: &mut ShellView, ks: &Keystroke, cx: &mut Context<ShellView>) -> bool {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    if ks.key == "escape" {
        // `cancel_naming` is the whole transition — `Stage::Browse`,
        // `DialogMode::Normal`, an empty `query` — and `dialog::sync_dialog_text`
        // empties the field and blurs it to match on this handler's return.
        if let Some(state) = shell.object_dialog.as_mut() {
            state.cancel_naming();
        }
        cx.notify();
        return true;
    }
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        create_from_name(shell, cx);
        return true;
    }
    if ks.key == "tab" {
        return true;
    }
    false
}

/// Validate the name before creating: syntax, reserved names, existing layered
/// definitions, and orphaned user presentation entries. An existing definition must be
/// opened explicitly rather than silently overwritten by creation.
fn create_from_name(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let domain = state.domain;
    let name = match check_object_name(&state.query) {
        Ok(name) => name.to_string(),
        Err(reason) => {
            set_notice(shell, reason);
            cx.notify();
            return;
        }
    };
    if domain.name_taken(&shell.services.config, &name) {
        // Reserved names, existing browse objects, and orphaned presentations need
        // different remedies. Only an existing object can be opened from this list.
        let notice = if domain.is_reserved(&name) {
            format!("'{name}' is reserved")
        } else if derive_rows(shell).iter().any(|row| row.name == name) {
            format!("'{name}' already exists — open it instead")
        } else {
            format!(
                "'{name}' has a saved presentation — remove it from view_presentation.toml first"
            )
        };
        set_notice(shell, notice);
        cx.notify();
        return;
    }
    let mut draft = domain.new_draft(&shell.services.config, &name);
    if domain == Domain::Sources
        && let Some(dataset) = shell
            .object_dialog
            .as_ref()
            .and_then(|s| s.naming_dataset.clone())
    {
        // the dataset `n` seeded from the cursor row (`seed_dataset_under_cursor`)
        // becomes the new source's own `dataset` field — revalidated so the idle-source
        // warning shows in the edit stage immediately rather than one debounce late.
        sources::seed_dataset(&mut draft, &dataset);
        draft.diagnostics = domain.validate(&draft, &shell.services.config);
    }
    // `c`'s copy: `n` leaves `naming_seed` at its default `Empty`, so `new_draft`'s own
    // empty object stands unchanged — this block only ever fires behind `begin_copy`'s
    // arm.
    let seed = shell
        .object_dialog
        .as_ref()
        .map(|s| s.naming_seed.clone())
        .unwrap_or(NameSeed::Empty);
    // One `match` rather than two `if let`s: `seed` is an owned, non-`Copy` value with
    // exactly one consumer, and a second `if let` reading it after a first one already
    // moved it needed a defensive `.clone()` that a single exhaustive match makes
    // unnecessary — the compiler enforces there is nothing a fourth `NameSeed` variant
    // could add without a reminder here too.
    match seed {
        NameSeed::Empty => {}
        NameSeed::CopyOf(source) => {
            // Verbatim, from the pending-aware config: inside the 250 ms
            // write debounce `services.config` is the source as it stood
            // before its last edit (`apply::config_with_pending`'s own
            // doc has the trace) — the same reason `enter_edit_stage`
            // derives from it rather than from `services.config` alone.
            let folded = apply::config_with_pending(shell);
            let config = folded.as_ref().unwrap_or(&shell.services.config);
            let Some(table) = config
                .doc(domain.doc())
                .and_then(|doc| doc.value.get(&source))
                .and_then(|v| v.as_table())
                .cloned()
            else {
                set_notice(shell, format!("'{source}' is gone — nothing to copy"));
                cx.notify();
                return;
            };
            draft.source = table;
            draft.fields = domain.fields_from_source(config, &draft.source);
            draft.diagnostics = domain.validate(&draft, config);
        }
        NameSeed::FromFrame => {
            // Capture current frame scope at confirmation time. Recheck emptiness
            // because frame scope can change while the naming prompt is open.
            let scope = shell.frame.read(cx).scope().clone();
            if scope.is_empty() {
                set_notice(shell, EMPTY_SCOPE_NOTICE.to_string());
                cx.notify();
                return;
            }
            // Pending-aware, like `c`'s copy just above: inside the
            // 250 ms write debounce `services.config` alone is the
            // config as it stood before the last edit, which would
            // build the available dimensions list one keystroke stale.
            let folded = apply::config_with_pending(shell);
            let config = folded.as_ref().unwrap_or(&shell.services.config);
            scopes::overwrite_with(&mut draft, &scope, config);
            draft.diagnostics = domain.validate(&draft, config);
        }
    }
    enter_edit_stage(shell, &name, Some(draft), cx);
    if let Some(notice) = apply::commit_create(shell, cx) {
        set_notice(shell, notice);
    }
    cx.notify();
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

/// Begin naming through the same route for keyboard and pointer actions. Refuse
/// read-only domains and fixed rosters. Capture the Sources dataset seed before
/// borrowing state, clear the old filter, and let shared input synchronization focus
/// the new empty name field.
fn begin_new_object(shell: &mut ShellView, seed: Option<String>, seed_taken: bool) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    if !state.domain.writable(&state.stage) {
        state.notice = Some(READ_ONLY_NOTICE.to_string());
    } else if state.domain.roster().is_some() {
        state.notice = Some("the slots are fixed — open one to fill it".to_string());
    } else {
        state.begin_naming();
        // `n` always creates the domain's EMPTY object — `begin_copy`'s own arm is the
        // only place `naming_seed` becomes `CopyOf`, and this door must set it back to
        // `Empty` explicitly rather than trust `begin_naming`'s own reset, since a
        // stale `CopyOf` from an earlier `c` would otherwise survive an `escape` and
        // land on this door's `n`.
        state.naming_seed = NameSeed::Empty;
        // `n` on Sources seeds the new source's dataset from the browse row under the
        // cursor, and pre-fills the name field with it too when no source already holds
        // that name — one source per dataset is the common case, so the trader's next
        // keystroke is usually just `enter`. `seed` is `None` on every domain but
        // Sources, so this is a no-op everywhere else.
        state.naming_dataset = seed.clone();
        if let Some(dataset) = seed
            && !seed_taken
        {
            state.query = dataset;
        }
    }
}

/// `c`: the naming stage seeded to copy `source` — `begin_new_object`'s twin, without
/// Sources' dataset seed, since only a duplicable domain (`Domain::duplicable`) ever
/// reaches this door — `handle_browse_key`'s own guard on the verb.
fn begin_copy(shell: &mut ShellView, source: String) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    if !state.domain.writable(&state.stage) {
        state.notice = Some(READ_ONLY_NOTICE.to_string());
        return;
    }
    state.begin_naming();
    state.naming_seed = NameSeed::CopyOf(source);
}

/// The refusal `open_save_scope` and `create_from_name`'s `FromFrame`
/// arm share: there is nothing on the frame to name and save. One
/// string so the two sites cannot drift apart.
const EMPTY_SCOPE_NOTICE: &str = "the frame's scope is empty — nothing to save";

/// Open Scopes naming for the current frame scope from palette or toolbar. An empty
/// frame scope opens browsing with a notice instead. Creation checks emptiness again
/// because the frame can change while naming is open.
pub(in crate::shell) fn open_save_scope(
    shell: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    // `open`'s own guard (below) refuses to open a SECOND modal, but it returns
    // silently — this door kept going past that refusal and mutated whatever
    // `object_dialog` was already there instead (a Views dialog's
    // `begin_naming`/`naming_seed`, say), because a second `open(..)` call two lines
    // down is a no-op while the first branch's `state.notice = ..` and this function's
    // own `begin_naming` read `shell.object_dialog` regardless of whose it is. Guarding
    // here, before either branch touches it, is what makes "no modal is already open"
    // the one precondition both branches share with `open` itself.
    if shell.modal.is_some() {
        return;
    }
    if shell.frame.read(cx).scope().is_empty() {
        open(shell, Domain::Scopes, window, cx);
        if let Some(state) = shell.object_dialog.as_mut() {
            state.notice = Some(EMPTY_SCOPE_NOTICE.to_string());
        }
        cx.notify();
        return;
    }
    open(shell, Domain::Scopes, window, cx);
    if let Some(state) = shell.object_dialog.as_mut() {
        state.begin_naming();
        state.naming_seed = NameSeed::FromFrame;
    }
    // `begin_naming` puts the dialog in `DialogMode::Filter`, which is what gives the
    // shared `Input` the keys — `sync_dialog_text` is the only thing allowed to move
    // focus onto it, and `open` above already called it once for the browse stage it
    // opened in, so this second call is what actually focuses the name field.
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// the dataset of the browse row under the cursor, for `n` on Sources — `None` on every
/// other domain, or with no row (an empty list, or a keystroke racing the modal
/// closing).
fn seed_dataset_under_cursor(shell: &ShellView) -> Option<String> {
    let state = shell.object_dialog.as_ref()?;
    if state.domain != Domain::Sources {
        return None;
    }
    let rows = derive_rows(shell);
    let visible = super::visible_rows(state, &rows);
    let row = visible.get(state.selected).and_then(|m| rows.get(m.row))?;
    row.prefix.clone()
}

/// Resolve the clicked object's name against freshly derived filtered rows, then open
/// it through the same transition as Enter. Synchronize text and focus afterward so
/// pointer input obeys the current mode.
fn on_row_clicked(
    shell: &mut ShellView,
    clicked: &str,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let rows = derive_rows(shell);
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    // The dialog's other door — see [`handle_key`]'s own clear.
    if state.notice.take().is_some() {
        cx.notify();
    }
    // a question owns the mouse as well as the keys. A click here would both open the
    // row (answering the question with a shrug — `enter_edit` clears the confirm) and,
    // worse, move the cursor off the row the question is about, so `enter` would then
    // act on a different object from the one the prompt names.
    if state.confirm.is_some() {
        return;
    }
    let visible = super::visible_rows(state, &rows);
    let Some(ix) = super::filtered_position(&visible, &rows, clicked) else {
        return;
    };
    state.selected = ix;
    // A row click opens its object as Enter does; the transition sets Normal mode.
    let opens = state.stage != Stage::Naming;
    let name = clicked.to_string();
    shell.object_dialog_scroll.scroll_to_item(ix);
    if opens {
        enter_edit_stage(shell, &name, None, cx);
        // The browse list is a door too (`ObjectDialogState::
        // click_opened_stage`): a double-click's second half lands on
        // whatever edit row the new stage painted under the pointer, and
        // must not open it.
        if let Some(state) = shell.object_dialog.as_mut() {
            state.click_opened_stage = true;
        }
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

// ---- The edit stage ---------------------------------------------------

/// `enter`: open the selected browse row's object in the edit stage.
///
/// Resolves the row through the same filtered walk everything else here
/// uses, so what opens is the row the user is looking at even mid-filter.
/// The browse query is dropped with the stage change (see this module's
/// own "Three stages" note) and the mode goes back to `Normal`, which is
/// what makes the edit stage's letters verbs — the shared `Input` is
/// emptied and blurred to match by [`dialog::sync_dialog_text`], never
/// here.
fn open_selected(shell: &mut ShellView, cx: &mut Context<ShellView>) {
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
    enter_edit_stage(shell, &name, None, cx);
}

/// Shared object-stage entry for opening and creation. Existing objects derive from
/// configuration with the pending batch folded in; new objects retain their
/// already-built draft until the queued creation reaches active configuration. Reset
/// the viewport and request repaint. Input text and focus are synchronized by the
/// caller's keyboard or pointer path after the pure state transition.
fn enter_edit_stage(
    shell: &mut ShellView,
    name: &str,
    new: Option<Draft>,
    cx: &mut Context<ShellView>,
) {
    // Derive from the config WITH the pending batch folded in, never from
    // `services.config` alone: inside the write debounce the latter is
    // the object as it stood before the last tick, and a draft built
    // from it both hides that tick and, outliving the flush, writes the
    // object without it on the next one (`apply::config_with_pending`'s
    // own doc has the trace). This is the one door, so the digit jump,
    // `enter` from browse and `escape`-then-`enter` are all covered.
    let folded = apply::config_with_pending(shell);
    if let Some(state) = shell.object_dialog.as_mut() {
        match new {
            Some(draft) => state.enter_edit_with(draft),
            None => match folded.as_ref() {
                Some(config) => state.enter_edit(config, name),
                None => state.enter_edit(&shell.services.config, name),
            },
        }
    }
    shell.object_dialog_scroll.scroll_to_item(0);
    cx.notify();
}

/// Open the shared column stage from a Views member or declared Schema column. Its
/// context carries the destination, baseline layers, and fold target: Views updates its
/// list item and view presentation; Schema updates a scratch item and dataset
/// presentation.
///
/// Read colors and overlay baselines from the pending-aware config. Clear mode, query,
/// confirmation, and viewport for the new stage. If the column cannot be resolved,
/// discard the prepared context and leave the current stage intact.
fn enter_column_stage(shell: &mut ShellView, column: &str, cx: &mut Context<ShellView>) {
    // Use pending edits for the named-color choices and presentation baselines. A
    // stage opened during debounce must not overwrite a value it cannot yet see.
    let pending = apply::config_with_pending(shell);
    let config = pending.as_ref().unwrap_or(&shell.services.config);
    let colours: Vec<String> = config
        .doc(colours::DOC)
        .map(|doc| {
            geode_core::colour::NamedColours::from_doc(doc)
                .0
                .names()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let domain = state.domain;
    let object = match &state.stage {
        Stage::Edit { object } | Stage::Column { object, .. } => object.clone(),
        _ => return,
    };
    // Read the Schema seed from the same pending-aware config before borrowing the
    // draft mutably. Preserve the dataset's other edited columns in its overlay.
    let schema_seed = (domain == Domain::Schema).then(|| {
        let schema = config
            .doc("datasets")
            .map(|doc| geode_core::schema::SchemaSpec::from_doc(doc).0)
            .unwrap_or_default();
        (schema, dataset_columns::overlay_object(config, &object))
    });
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    let Some(draft) = state.draft.as_mut() else {
        return;
    };
    let (fields, ctx) = match domain {
        Domain::Views => {
            let Some(item) = draft
                .list_items("columns")
                .and_then(|items| items.iter().find(|i| i.name == column))
                .cloned()
            else {
                return;
            };
            // The layer between the desk view and this view's own overlay, refreshed
            // from the PENDING-aware config before the context is built: a
            // dataset-level edit made in the Schema dialog inside the 250 ms write
            // debounce is otherwise invisible here, and a stage that cannot see it both
            // hides that edit and, on its own next keystroke, writes the column back
            // without it.
            draft.dataset_layer = views::dataset_layer_for(config, &object);
            (
                views::column_fields(&item, &colours, Destination::Presentation),
                // The one builder of this door's context — shared with
                // the two test openers that mirror this function, so
                // they cannot drift from it (`views::column_context`).
                views::column_context(draft, column, item),
            )
        }
        // the dataset overlay is the only layer this door has, and it is both the seed
        // and the `dataset` layer — a Schema field therefore reads `dataset` or
        // nothing, never `desk`, since the desk's own keys vary per view and sit BELOW
        // this one.
        Domain::Schema => {
            let Some((schema, overlay_object)) = schema_seed else {
                return;
            };
            let Some(dataset) = schema.dataset(&object) else {
                return;
            };
            let Some(item) = dataset_columns::item_for(dataset, column, &overlay_object) else {
                return;
            };
            (
                views::column_fields(&item, &colours, Destination::DatasetPresentation),
                ColumnContext {
                    door: ColumnDoor::Dataset,
                    layers: ColumnLayers {
                        dataset: item.presentation.clone(),
                        ..ColumnLayers::default()
                    },
                    overlay_object,
                    item: Some(item),
                },
            )
        }
        // No other domain has a column stage: Groupings, Scopes, Sources
        // and Colors have no per-column presentation to open, and
        // `column_stage_target` never names a row on one.
        _ => return,
    };
    draft.column_ctx = Some(ctx);
    if !draft.enter_column(column, fields) {
        draft.column_ctx = None;
        return;
    }
    // Settle selection at stage entry because pointer activation bypasses the keyboard
    // handler's final settle.
    draft.settle_selection(domain);
    state.stage = Stage::Column {
        object,
        column: column.to_string(),
    };
    state.mode = DialogMode::Normal;
    state.notice = None;
    // A confirm armed over the view's rows has nothing to answer for
    // once the seven column fields replace them; leaving it armed would
    // hold the new stage's keystrokes hostage to a question about a
    // stage that has closed. Unreachable today — the armed block claims
    // every key ahead of `enter` — which is exactly why it is cleared
    // rather than relied on.
    state.disarm();
    // The row list is now seven fields with the cursor on the first, so
    // the viewport goes with it — `enter_edit_stage`'s own reset.
    shell.object_dialog_scroll.scroll_to_item(0);
    cx.notify();
}

/// Fold the column fields and return to the parent object, selecting the column. Schema
/// re-derives its read-only summaries from pending-aware configuration and moves their
/// baseline with them, avoiding a schema-definition write. Views uses the parent item
/// already updated by the fold. The Escape ladder reaches this transition after leaving
/// filter mode and clearing the query.
fn leave_column_stage(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let domain = state.domain;
    let Stage::Column { object, .. } = state.stage.clone() else {
        return;
    };
    // Only the Schema door re-derives, so only it pays for the folded
    // config — `config_with_pending` re-merges every layered document,
    // which is real work to do on a keystroke that, for Views, wants
    // nothing from it.
    let pending = (domain == Domain::Schema)
        .then(|| apply::config_with_pending(shell))
        .flatten();
    let reseed = (domain == Domain::Schema).then(|| {
        schema::fields(
            pending.as_ref().unwrap_or(&shell.services.config),
            Some(&object),
        )
    });
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    if let Some(draft) = state.draft.as_mut() {
        draft.leave_column();
        if let Some(fields) = reseed {
            draft.reseed_fields(fields);
        }
    }
    state.stage = Stage::Edit { object };
    state.notice = None;
    // Symmetric with `enter_column_stage`'s own clear, for its reason.
    state.disarm();
    scroll_to_cursor(shell);
    cx.notify();
}

/// Open Values with a loading row and request distinct values for the draft scope minus
/// this column, using the frame's as-of selection. Store a monotonic request tag for
/// delivery validation and enter Normal mode.
fn enter_values_stage(shell: &mut ShellView, column: &str, cx: &mut Context<ShellView>) {
    // Through the folded config, `enter_column_stage`'s own reason: a
    // dimension selection made in this same dialog inside the 250 ms
    // write debounce must still be reflected in the scope the request
    // carries.
    let pending = apply::config_with_pending(shell);
    let config = pending.as_ref().unwrap_or(&shell.services.config);
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let Stage::Edit { object } = &state.stage else {
        return;
    };
    let object = object.clone();
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    let scope = scopes::draft_scope(draft, config, column);
    let as_of = shell.frame.read(cx).as_of().clone();
    shell.next_picker_tag += 1;
    let tag = shell.next_picker_tag;
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    let Some(draft) = state.draft.as_mut() else {
        return;
    };
    if !draft.enter_values(column, scopes::loading_field()) {
        return;
    }
    // The loading placeholder has no cursor stop, so settling leaves it selected.
    // Delivery settles again after installing the value rows.
    let domain = state.domain;
    draft.settle_selection(domain);
    state.stage = Stage::Values {
        object,
        column: column.to_string(),
    };
    state.mode = DialogMode::Normal;
    state.notice = None;
    state.disarm();
    state.values_tag = tag;
    shell.object_dialog_scroll.scroll_to_item(0);
    cx.emit(ShellEvent::DistinctRequested(DistinctParams {
        key: SCOPES_KEY,
        tag,
        column: column.to_string(),
        scope,
        as_of,
    }));
    cx.notify();
}

/// `escape` out of the Values stage: fold one last time, restore the
/// scope's fields, and put the cursor on the column's own row — its item
/// row if the selection survived, its available row if it was emptied.
fn leave_values_stage(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    let Stage::Values { object, column } = state.stage.clone() else {
        return;
    };
    if let Some(draft) = state.draft.as_mut() {
        scopes::fold_values(draft);
        draft.leave_values();
        let rows = draft.rows();
        let target = draft
            .visible_rows()
            .iter()
            .position(|m| {
                matches!(
                    rows.get(m.row),
                    Some(row @ (EditRow::Item { .. } | EditRow::Available { .. }))
                        if draft.row_label(*row) == column
                )
            })
            .unwrap_or(0);
        draft.selected = target;
    }
    state.stage = Stage::Edit { object };
    state.notice = None;
    state.disarm();
    scroll_to_cursor(shell);
    cx.notify();
}

/// Open a grouping slot by its digit from browse or another slot. Its number is the
/// object name, including unconfigured slots. Repeating the current slot's digit leaves
/// the existing draft intact and reports that it is already open.
fn jump_to_slot(shell: &mut ShellView, slot: u8, cx: &mut Context<ShellView>) {
    let name = slot.to_string();
    let already = matches!(
        shell.object_dialog.as_ref().map(|state| &state.stage),
        Some(Stage::Edit { object }) if *object == name
    );
    if already {
        set_notice(shell, format!("already editing slot {name}"));
        cx.notify();
        return;
    }
    enter_edit_stage(shell, &name, None, cx);
}

/// Shared routing for object, column, and Values editing. An armed confirmation owns
/// input first; an open value field uses its own text/completion handler. Filter mode
/// routes navigation and stepping while leaving text to the input. Normal mode uses the
/// command table and consumes unrecognized keys.
///
/// In filter mode, Escape restores the entry query and bare Enter keeps the typed
/// query; both return to Normal without opening or committing a row. Value fields
/// handle their own commit/cancel keys before filter routing. Normal-mode Escape
/// clears a query, returns to a parent stage, or closes according to the shared
/// ladder. Closing or going back does not cancel queued edits.
/// Changes normally revalidate before committing. Reordering commits without
/// revalidation because current reorderable domains have no order-sensitive draft
/// diagnostic; adding one would require revalidation at that branch.
fn handle_edit_key(shell: &mut ShellView, ks: &Keystroke, cx: &mut Context<ShellView>) -> bool {
    // Settle after every return path, including mutations that add, remove, or re-rank
    // rows. Motion already snaps in its direction through move_selection; this final
    // settle preserves that stop and covers other selection changes.
    let claimed = handle_edit_key_inner(shell, ks, cx);
    settle_edit_cursor(shell);
    claimed
}

/// Settle the retained draft's cursor after keyboard handling, including Column and
/// Values projections. Skip while text entry owns selection, and do nothing when no
/// draft remains.
fn settle_edit_cursor(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    let domain = state.domain;
    if let Some(draft) = state.draft.as_mut()
        && draft.text_entry.is_none()
    {
        draft.settle_selection(domain);
    }
}

fn handle_edit_key_inner(
    shell: &mut ShellView,
    ks: &Keystroke,
    cx: &mut Context<ShellView>,
) -> bool {
    // The same notice door `handle_browse_key` opens with, for the same
    // reason: a notice reports on the keystroke that produced it.
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }

    if armed_confirm(shell).is_some() {
        match dialog::ConfirmAnswer::from_key(ks) {
            Some(dialog::ConfirmAnswer::Yes) => answer_confirm(shell, true, cx),
            Some(dialog::ConfirmAnswer::No) => answer_confirm(shell, false, cx),
            // Claimed and dropped: while a destructive question is on
            // screen, a stray letter must not act on the object behind it.
            None => {}
        }
        cx.notify();
        return true;
    }

    // ---- Text field --------------------------------------------
    //
    // Checked before filter mode, which it shares a focused `Input` with: the field is
    // open only in `Filter` (that is what gives it the keys), and every key filter mode
    // would claim means something else here. The chain field is `handle_text_key`'s
    // `Completions::Chain` case, not a separate dispatch.
    let text_entry = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .is_some_and(|draft| draft.text_entry.is_some());
    if text_entry {
        return handle_text_key(shell, ks, cx);
    }

    // ---- Values stage -------------------------
    //
    // `ctrl+a` ticks every value the filter currently shows, `ctrl+x`
    // clears the selection — the dimension picker's own pair, reclaimed
    // inside `GeodeModal` already (`dialog::init_reclaimed_keybindings`),
    // which is what lets the chord reach this handler in either mode
    // rather than being eaten by the shared `Input` in filter mode. Placed
    // ahead of the filter-mode split below so both modes share it — a
    // trader mid-filter still wants "tick everything this narrowed to".
    let in_values = draft_ref(shell).is_some_and(|d| d.values().is_some());
    if in_values && ks.mods == Modifiers::CTRL && (ks.key == "a" || ks.key == "x") {
        let tick_all = ks.key == "a";
        let changed = draft_mut(shell).is_some_and(|draft| {
            let shown: Vec<usize> = draft
                .visible_rows()
                .iter()
                .filter_map(|m| match draft.rows().get(m.row) {
                    Some(EditRow::Item { item, .. }) => Some(*item),
                    _ => None,
                })
                .collect();
            let Some(field) = draft.fields.iter_mut().find(|f| f.key == "values") else {
                return false;
            };
            let FieldKind::OrderedList { items, .. } = &mut field.kind else {
                return false;
            };
            let mut changed = false;
            for (i, item) in items.iter_mut().enumerate() {
                let want = if tick_all {
                    shown.contains(&i) || item.included
                } else {
                    false
                };
                if item.included != want {
                    item.included = want;
                    changed = true;
                }
            }
            changed
        });
        if changed {
            revalidate(shell);
            commit_change(shell, cx);
        } else {
            set_notice(shell, "nothing to change".to_string());
        }
        cx.notify();
        return true;
    }

    // ---- Filter mode ------------------------------------------
    //
    // The one switch, exactly as browse's own: while the shared `Input`
    // holds focus, bare letters are text, so this branch claims only the
    // handful of keys that input does not consume first.
    let filtering = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.mode == DialogMode::Filter);
    if filtering {
        if let Some(exit) = dialogmode::filter_exit(ks) {
            // Escape restores the entry query; Enter keeps the typed query.
            // Neither commits or opens a row. Opening a member's column stage
            // requires another Enter in normal mode. `sync_dialog_text` restores
            // shell focus on return so letters become commands again.
            let changed = shell
                .object_dialog
                .as_mut()
                .is_some_and(|state| state.exit_filter(exit));
            if changed {
                // `exit_filter` has already put the draft's cursor on the
                // top match; the viewport follows it for the reason every
                // other query change does.
                shell.object_dialog_scroll.scroll_to_item(0);
            }
            cx.notify();
            return true;
        }
        if let Some(cmd) = listfilter::nav_command(ks) {
            let selected = shell.object_dialog.as_mut().and_then(|state| {
                let domain = state.domain;
                let draft = state.draft.as_mut()?;
                draft.move_selection(domain, cmd);
                Some(draft.selected)
            });
            if let Some(selected) = selected {
                shell.object_dialog_scroll.scroll_to_item(selected);
            }
            cx.notify();
            return true;
        }
        // `tab`/`shift+tab` STEP the selected row here, which is the settings dialog's
        // own "tab steps in both modes" rule arriving at this stage: exactly the path
        // `space`/`shift+space` take in normal mode, so a step is a step whichever mode
        // the trader is in. The `Input` keeps focus and the query is untouched —
        // `dialog::sync_dialog_text` writes the unchanged `effective_query` back on
        // this handler's return — because this is a value change, not a filter
        // keystroke. `h` and `l` are NOT claimed: they are letters on their way to the
        // field, which a trader typing `hidden` depends on.
        //
        // An open text field never reaches here: the `text_entry` branch above claims
        // every key first, so the chain field's `tab` still completes a segment and a
        // plain field's stays inert.
        if ks.key == "tab" {
            let forward = match ks.mods {
                Modifiers::NONE => true,
                m if m
                    == (Modifiers {
                        shift: true,
                        ..Modifiers::NONE
                    }) =>
                {
                    false
                }
                // Any other modifier: claimed and dropped, as it was
                // before this key did anything at all here.
                _ => return true,
            };
            // Apply the stage's write gate before filter-mode stepping.
            if !shell
                .object_dialog
                .as_ref()
                .is_some_and(|s| s.domain.writable(&s.stage))
            {
                set_notice(shell, READ_ONLY_NOTICE.to_string());
            } else {
                step_selected_row(shell, forward, true, cx);
            }
            cx.notify();
            return true;
        }
        return false;
    }

    // ---- Normal mode ----------------------------------------------------

    if ks.key == "escape" {
        let step = shell.object_dialog.as_ref().map(|state| {
            let query_is_empty = state.draft.as_ref().is_some_and(|d| d.query.is_empty());
            dialogmode::escape_step(state.mode, query_is_empty, state.has_previous_stage())
        });
        match step {
            Some(EscapeStep::ClearQuery) => {
                // A query retained by filter-mode Enter clears here. Filter-mode
                // Escape restores the entry query, so this rung is skipped if the
                // restored query is empty.
                if let Some(draft) = draft_mut(shell) {
                    draft.query.clear();
                    draft.selected = 0;
                }
                shell.object_dialog_scroll.scroll_to_item(0);
                // Clearing the draft's own query is the whole rung: the shared `Input`
                // is emptied from it by `dialog::sync_dialog_text`, which reads
                // `effective_query` and so takes the draft's copy while this stage is
                // open.
                cx.notify();
                return true;
            }
            Some(EscapeStep::PreviousStage) => {
                // Straight back, with no discard question: there is
                // nothing to discard. Every field edit is already on the
                // pending batch, which outlives this stage and the whole
                // dialog (`ShellView::pending_config_write`) and still
                // merges, applies and writes on its own timer; and a
                // fork the user has not confirmed was taken back off the
                // draft when they declined it. Leaving abandons exactly
                // nothing.
                //
                // from the Values stage this rung goes back to the scope whose fields
                // `leave_values_stage` restores, cursor on the dimension just edited —
                // checked ahead of the column stage's own rung, since the two stages
                // never overlap but the check has to name one first.
                let in_values = shell
                    .object_dialog
                    .as_ref()
                    .is_some_and(|state| matches!(state.stage, Stage::Values { .. }));
                if in_values {
                    leave_values_stage(shell, cx);
                    return true;
                }
                // from the column stage this rung goes back one stage, not all the way
                // out — to the view whose fields `leave_column_stage` restores, cursor
                // on the column just edited.
                let in_column = shell
                    .object_dialog
                    .as_ref()
                    .is_some_and(|state| matches!(state.stage, Stage::Column { .. }));
                if in_column {
                    leave_column_stage(shell, cx);
                } else {
                    leave_edit(shell, cx);
                }
                return true;
            }
            // `LeaveFilter` is unreachable at this match — the
            // `filtering` branch above intercepts `escape` before the
            // mode can still read `Filter` here — and `Close` is
            // unreachable too, since `has_previous_stage()` is always
            // true for `Stage::Edit`. Both are folded into one `false`
            // rather than special-cased away, because `escape_step` is
            // the one ladder every modal surface walks and forking it per
            // call site is how the rungs drift apart. The `false` hands
            // the keystroke to the shell's modal branch, which — since
            // neither rung can actually fire here — never gets called for
            // this reason in practice.
            _ => return false,
        }
    }

    let Some(cmd) = dialogmode::normal_command(ks) else {
        return true;
    };

    // every verb that would change the object is refused here, in one place, on a
    // `Domain::writable() == false` surface (Schema is the one today) — rather than by
    // each arm below remembering to check. `Nav`, `EnterFilter`, `Commit` and a bare
    // unbound letter all stay live: reading and filtering are exactly what a read-only
    // inspector is for.
    let writable = shell
        .object_dialog
        .as_ref()
        .is_some_and(|s| s.domain.writable(&s.stage));
    if !writable
        && matches!(
            cmd,
            NormalCommand::Toggle
                | NormalCommand::ToggleBack
                | NormalCommand::EditText
                | NormalCommand::MoveItem(_)
                | NormalCommand::Verb('d' | 'r' | 'x' | 'n' | 'o')
        )
    {
        set_notice(shell, READ_ONLY_NOTICE.to_string());
        cx.notify();
        return true;
    }

    match cmd {
        NormalCommand::Nav(nav) => {
            let selected = shell.object_dialog.as_mut().and_then(|state| {
                let domain = state.domain;
                let draft = state.draft.as_mut()?;
                draft.move_selection(domain, nav);
                Some(draft.selected)
            });
            if let Some(selected) = selected {
                shell.object_dialog_scroll.scroll_to_item(selected);
            }
        }
        // `space`, `l` and `tab` forward; `shift+space`, `h` and `shift+tab` back — the
        // aliases arrive already resolved from `dialogmode::normal_command`, so there
        // is one arm per direction rather than one per spelling. The notice names
        // `space`/`shift+space` whichever alias was pressed: naming the alias would
        // need the keystroke down here, and the two canonical keys are the ones the
        // footer teaches.
        NormalCommand::Toggle => step_selected_row(shell, true, false, cx),
        NormalCommand::ToggleBack => step_selected_row(shell, false, false, cx),
        NormalCommand::MoveItem(delta) if in_column_stage(shell) => {
            // there is no list in this stage to reorder, so the ordinary "that is as
            // far as this row goes" would answer about rows that are not on screen.
            // Both directions get the same sentence, with the key they actually pressed
            // in it.
            let key = if delta < 0 { "shift+k" } else { "shift+j" };
            not_a_column_verb(shell, key);
        }
        // Scope selections are unordered, so both dimensions and Values refuse
        // reorders.
        NormalCommand::MoveItem(_) if is_scopes(shell) => {
            set_notice(shell, scopes::NO_ORDER_NOTICE.to_string())
        }
        NormalCommand::MoveItem(delta) => {
            let skipped = draft_mut(shell).and_then(|draft| draft.move_item(delta));
            match skipped {
                Some(skipped) => {
                    scroll_to_cursor(shell);
                    // Said out loud only when there was something to
                    // skip — a plain adjacent-item move under no filter
                    // (or under a filter that hides nothing between the
                    // two) is the ordinary case and needs no comment.
                    if skipped > 0 {
                        set_notice(shell, format!("moved past {skipped} hidden"));
                    }
                    commit_change(shell, cx);
                }
                None => set_notice(shell, "that is as far as this row goes".to_string()),
            }
        }
        NormalCommand::Verb('d') => arm_delete(shell),
        NormalCommand::Verb('r') => arm_revert(shell),
        NormalCommand::Verb('o') => overwrite_scope(shell, cx),
        // `x` removes a member into its catalogue. Route its own refusal directly to
        // the notice because it differs from a final-untick refusal. Selection stays at
        // the next visible row, so no follow-scroll is needed. Column projections have
        // no member list to demote from and refuse before reaching this branch.
        NormalCommand::Verb('x') if in_column_stage(shell) => not_a_column_verb(shell, "x"),
        // `x` drops a selected dimension outright — the definitional twin of the Values
        // stage's ticks — but only on the dimensions list itself; on an available row
        // there is nothing to drop (`enter` picks its values instead), and inside the
        // Values stage a value's own row answers with `space`'s own wording rather than
        // this domain's.
        NormalCommand::Verb('x') if is_scopes(shell) => {
            match draft_ref(shell).map(Draft::selected_row) {
                Some(Some(EditRow::Available { .. })) => {
                    set_notice(shell, "not selected — enter picks its values".to_string())
                }
                Some(Some(EditRow::Item { .. })) if !in_values_stage(shell) => {
                    match draft_mut(shell).map(Draft::remove_selected) {
                        Some(Step::Changed) => {
                            revalidate(shell);
                            commit_change(shell, cx);
                        }
                        Some(Step::Refused(reason)) => set_notice(shell, reason),
                        _ => {}
                    }
                }
                _ => set_notice(
                    shell,
                    "x drops a selected dimension — here, space unticks".to_string(),
                ),
            }
        }
        NormalCommand::Verb('x') => match draft_mut(shell).map(Draft::remove_selected) {
            Some(Step::Changed) => {
                revalidate(shell);
                commit_change(shell, cx);
            }
            Some(Step::Refused(reason)) => set_notice(shell, reason),
            _ => set_notice(
                shell,
                "x removes a column from the view — here, space unticks".to_string(),
            ),
        },
        NormalCommand::EnterFilter => {
            // Snapshot the active stage's query through the shared filter entry.
            // `dialog::sync_dialog_text` gives the filter focus on return.
            if let Some(state) = shell.object_dialog.as_mut() {
                state.enter_filter();
            }
        }
        // Use the same field-opening route as the `i` action button.
        NormalCommand::EditText => open_field(shell),
        NormalCommand::Commit => commit_selected_row(shell, cx),
        // from one slot's edit stage a digit jumps straight to another's. On any other
        // domain it is named like an unbound letter would be — the edit stage's rule
        // for a key that did nothing.
        NormalCommand::Digit(n) => {
            let domain = shell.object_dialog.as_ref().map(|state| state.domain);
            if domain == Some(Domain::Groupings) {
                jump_to_slot(shell, n, cx);
                return true;
            }
            set_notice(shell, format!("{n} is not a verb here"));
        }
        // A letter this stage has no verb for. Named rather than
        // dropped: `d` and `r` have just taught the user that
        // letters act here, so a silent `z` reads as the dialog having
        // stopped responding — and it is the one branch where the key
        // that did nothing is not otherwise on screen to explain itself.
        NormalCommand::Verb(letter) => {
            set_notice(shell, format!("{letter} is not a verb here"));
        }
    }
    cx.notify();
    true
}

/// Enter opens the selected Views or Schema column, or the selected Scopes dimension's
/// Values stage. Other rows receive their ordinary commit notice. The route is shared
/// by Normal and Filter modes.
fn commit_selected_row(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    if let Some(name) = column_stage_target(shell) {
        enter_column_stage(shell, &name, cx);
    } else if let Some(column) = values_stage_target(shell) {
        enter_values_stage(shell, &column, cx);
    } else {
        edit_commit_notice(shell);
    }
}

/// Resolve the selected row's column-stage target for both Enter and clicks. Views
/// accepts its own member rows, not available candidates. Schema accepts
/// `columns.<name>` field rows, not derived dimensions. An existing column projection
/// has no nested column target.
fn column_stage_target(shell: &ShellView) -> Option<String> {
    let state = shell.object_dialog.as_ref()?;
    let draft = state.draft.as_ref()?;
    draft.column_stage_target(state.domain, draft.selected_row()?)
}

/// Resolve a Scopes dimension member or candidate to its Values-stage target. No target
/// is available while a Values projection is already open.
fn values_stage_target(shell: &ShellView) -> Option<String> {
    let state = shell.object_dialog.as_ref()?;
    let draft = state.draft.as_ref()?;
    draft.values_stage_target(state.domain, draft.selected_row()?)
}

/// `enter`'s answer for a row with nothing to open
/// ([`commit_selected_row`]'s normal-mode fallback), and `i`'s for a
/// row it cannot open ([`open_text_field`]): name the verb that DOES
/// change the selected row, or say the row has none.
///
/// Three answers, because there are three kinds of row here: `space` for a choice, a
/// bool or a list entry; `i` for a `Number` or a `Text` the domain marks editable
/// (`Domain::text_editable`); and "read-only" for the display-only `Text`s (Groupings'
/// `slot`, Scopes' two summaries). The `i` answer arrived with the column stage's
/// `label` and `width`, which are the first Views rows `i` can open — before them,
/// every editable `Text` in this crate was a Sources row, where `enter` gave the
/// read-only wording about a row `i` opens perfectly well. That was the two verbs
/// disagreeing about the same row, which is exactly what [`open_text_field`]'s own doc
/// says they must not do.
fn edit_commit_notice(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let (domain, stage) = (state.domain, state.stage.clone());
    if !domain.writable(&stage) {
        // agree with `i` and every other verb's refusal on a read-only domain rather
        // than falling back to the ordinary "this row is read-only" wording, which
        // names the ROW, not the whole surface, and would read as a truth about this
        // one field that a writable neighbour lacks.
        set_notice(shell, READ_ONLY_NOTICE.to_string());
        return;
    }
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    let typeable = match draft.selected_row() {
        Some(EditRow::Field(i)) => match &draft.fields[i].kind {
            FieldKind::Number { .. } => true,
            FieldKind::Text(_) => domain.text_editable(&draft.fields[i].key),
            _ => false,
        },
        _ => false,
    };
    let notice = if selected_field_is_steppable(draft) {
        "press space to change the selected row"
    } else if typeable {
        "press i to type a value"
    } else {
        "this row is read-only — nothing here has a verb"
    };
    set_notice(shell, notice.to_string());
}

/// Keep the lit option of an open choice field inside its viewport — the
/// list is a scroll container the wheel can move freely, so every key
/// that can move the highlight (nav, `tab`, a keystroke's re-rank)
/// points the handle back at it, exactly as the palette's own
/// `sync_palette_scroll` does.
pub(crate) fn scroll_to_choice(shell: &ShellView) {
    if let Some(row) = shell
        .object_dialog
        .as_ref()
        .and_then(|s| s.draft.as_ref())
        .and_then(Draft::choice_ranked_highlighted)
    {
        shell.object_dialog_scroll.scroll_to_item(row);
    }
}

/// Shared `i` route for keyboard and action buttons. Groupings opens its whole chain
/// field; other domains open the selected row's permitted value editor.
fn open_field(shell: &mut ShellView) {
    let groupings = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.domain == Domain::Groupings);
    if !groupings {
        open_text_field(shell);
        return;
    }
    if let Some(state) = shell.object_dialog.as_mut()
        && let Some(draft) = state.draft.as_mut()
    {
        draft.begin_chain_entry();
        // Gated on a field having actually opened, the way
        // `open_text_field`'s own `Step::Changed` check is:
        // `begin_chain_entry` returns without setting `text_entry` on a
        // draft with no `dimensions` field, and filter mode with no
        // field open means the next `escape` reverts the draft's query
        // to a filter snapshot instead of cancelling a field.
        if draft.text_entry.is_some() {
            state.mode = DialogMode::Filter;
        }
    }
    // The row list just became the (shorter) completion list with the
    // cursor on row 0; the viewport follows.
    shell.object_dialog_scroll.scroll_to_item(0);
}

/// Open numeric entry, permitted text entry, or multi-option choice typeahead.
/// Read-only text and one-option choices receive a notice. The same row capability
/// rules drive the footer and value buttons.
fn open_text_field(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let domain = state.domain;
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    let (editable, choice) = match draft.selected_row() {
        Some(EditRow::Field(i)) => match &draft.fields[i].kind {
            FieldKind::Number { .. } => (true, false),
            FieldKind::Text(_) => (domain.text_editable(&draft.fields[i].key), false),
            // a multi-option `Choice` opens a typeahead; a one-option one has nothing
            // to choose between and falls to the notice below, as stepping it would.
            FieldKind::Choice { options, .. } => (options.len() >= 2, true),
            _ => (false, false),
        },
        _ => (false, false),
    };
    if !editable {
        edit_commit_notice(shell);
        return;
    }
    if let Some(state) = shell.object_dialog.as_mut()
        && let Some(draft) = state.draft.as_mut()
    {
        let step = if choice {
            draft.begin_choice_entry()
        } else {
            draft.begin_text_entry()
        };
        if step == Step::Changed {
            state.mode = DialogMode::Filter;
        }
    }
}

/// Route an open value field. Escape discards the typed buffer; Enter validates and
/// applies it or keeps the field open with a refusal. Changed values follow the same
/// revalidate-and-commit path as stepping.
///
/// Choice entry picks the highlighted option, Tab completes it, and navigation moves
/// its highlight. Grouping-chain entry uses chain completion and validation. Plain
/// fields leave text insertion to the input. After closing, shared input
/// synchronization applies the draft's new query and mode to text and focus.
fn handle_text_key(shell: &mut ShellView, ks: &Keystroke, cx: &mut Context<ShellView>) -> bool {
    // ---- Choice field ------------------------
    //
    // Dispatched ahead of the chain and plain branches: `enter` here
    // picks the LIT option rather than applying typed text, `tab`
    // completes, and the nav keys move the highlight — one key table,
    // `choice::route`, shared with the settings dialog. Every other key
    // is the focused `Input`'s to type (`false`).
    let choosing = draft_mut(shell).is_some_and(|d| d.choice_entry());
    if choosing {
        let Some(key) = crate::choice::route(ks) else {
            return false;
        };
        match key {
            crate::choice::ChoiceKey::Cancel => {
                if let Some(state) = shell.object_dialog.as_mut()
                    && let Some(draft) = state.draft.as_mut()
                {
                    draft.cancel_text_entry();
                    state.mode = DialogMode::Normal;
                }
                scroll_to_cursor(shell);
            }
            crate::choice::ChoiceKey::Pick => {
                // The field's live text may never have reached the draft
                // through a `Change` event (`set_value` emits none), so
                // the ranking is refreshed from it before the pick.
                let live = shell.dialog_input.read(cx).value().to_string();
                let step = draft_mut(shell).map(|draft| {
                    draft.set_query(live);
                    draft.apply_choice()
                });
                match step {
                    Some(Step::Changed) => {
                        if let Some(state) = shell.object_dialog.as_mut() {
                            state.mode = DialogMode::Normal;
                        }
                        scroll_to_cursor(shell);
                        // The cursor is still on the row `apply_choice`'s own
                        // `follow(row)` left it on, which is exactly what
                        // `maybe_refresh_available`'s dataset-row check keys on — the
                        // same reason `space` and the action bar's tick run it ahead of
                        // `revalidate`.
                        maybe_refresh_available(shell);
                        revalidate(shell);
                        commit_change(shell, cx);
                    }
                    Some(Step::Inert) => {
                        if let Some(state) = shell.object_dialog.as_mut() {
                            state.mode = DialogMode::Normal;
                        }
                        scroll_to_cursor(shell);
                    }
                    Some(Step::Refused(reason)) => set_notice(shell, reason),
                    None => {}
                }
            }
            crate::choice::ChoiceKey::Complete => {
                if !draft_mut(shell).is_some_and(Draft::complete_choice) {
                    set_notice(shell, "nothing to complete here".to_string());
                }
                scroll_to_choice(shell);
            }
            crate::choice::ChoiceKey::Nav(cmd) => {
                if let Some(draft) = draft_mut(shell) {
                    draft.choice_nav(cmd);
                }
                scroll_to_choice(shell);
            }
        }
        cx.notify();
        return true;
    }
    let completions = draft_mut(shell).is_some_and(|d| d.chain_entry());
    // Chain completion rows disappear when the field closes, so follow the reset
    // cursor. Plain entry retains its unfiltered row list and edited-row selection;
    // follow that cursor instead of jumping to the first row.
    if ks.key == "escape" {
        if let Some(state) = shell.object_dialog.as_mut()
            && let Some(draft) = state.draft.as_mut()
        {
            draft.cancel_text_entry();
            state.mode = DialogMode::Normal;
        }
        if completions {
            shell.object_dialog_scroll.scroll_to_item(0);
        } else {
            scroll_to_cursor(shell);
        }
        cx.notify();
        return true;
    }
    let bare = ks.mods == Modifiers::NONE;
    if bare && ks.key == "enter" {
        let domain = shell.object_dialog.as_ref().map(|state| state.domain);
        let step = draft_mut(shell).map(|draft| {
            if completions {
                draft.apply_chain()
            } else {
                let domain = domain.expect("a draft implies an open dialog");
                draft.apply_text_entry(&|key, text| domain.parse_text(key, text))
            }
        });
        match step {
            Some(Step::Changed) => {
                if let Some(state) = shell.object_dialog.as_mut() {
                    state.mode = DialogMode::Normal;
                }
                if completions {
                    shell.object_dialog_scroll.scroll_to_item(0);
                } else {
                    scroll_to_cursor(shell);
                }
                revalidate(shell);
                commit_change(shell, cx);
            }
            Some(Step::Inert) => {
                if let Some(state) = shell.object_dialog.as_mut() {
                    state.mode = DialogMode::Normal;
                }
                if completions {
                    shell.object_dialog_scroll.scroll_to_item(0);
                }
            }
            Some(Step::Refused(reason)) => set_notice(shell, reason),
            None => {}
        }
        cx.notify();
        return true;
    }
    if ks.key == "tab" {
        // Claimed whatever the modifiers — `shift+tab` included — and a
        // claimed key that does nothing says so, this stage's own rule.
        // Only the chain field has anything for it to complete; a plain
        // field says so rather than silently eating the keystroke.
        if completions && bare && draft_mut(shell).is_some_and(Draft::complete_chain) {
            shell.object_dialog_scroll.scroll_to_item(0);
        } else {
            set_notice(shell, "nothing to complete here".to_string());
        }
        cx.notify();
        return true;
    }
    if completions && let Some(cmd) = listfilter::nav_command(ks) {
        let selected = draft_mut(shell).map(|draft| {
            draft.selected = vimnav::apply(draft.selected, draft.visible_rows().len(), cmd);
            draft.selected
        });
        if let Some(selected) = selected {
            shell.object_dialog_scroll.scroll_to_item(selected);
        }
        cx.notify();
        return true;
    }
    // A plain field: the arrows and ctrl-steps are the caret's, so they
    // reach the focused `Input` (`false`), like every other key.
    false
}

/// Shared stepping path for forward/backward keys and tick clicks. The caller has
/// checked write permission. Notices name keys available in the current mode: Space
/// steps in Normal but types text in Filter, where Tab steps instead.
fn step_selected_row(
    shell: &mut ShellView,
    forward: bool,
    filtering: bool,
    cx: &mut Context<ShellView>,
) {
    // Scopes' edit stage: `space` on an available dimension opens its values rather
    // than adding an empty selection — a fresh selection with nothing ticked is not a
    // state this domain can save — and on a selected one it names the door, since the
    // values themselves are the Values stage's to change. Checked ahead of the ordinary
    // step below and never inside the Values stage itself, where a tick is exactly what
    // `space` already means.
    if !in_values_stage(shell) && is_scopes(shell) {
        match draft_ref(shell).and_then(Draft::selected_row) {
            Some(EditRow::Available { .. }) => {
                if let Some(column) = values_stage_target(shell) {
                    enter_values_stage(shell, &column, cx);
                }
                return;
            }
            Some(EditRow::Item { .. }) => {
                set_notice(shell, "enter opens this dimension's values".to_string());
                return;
            }
            _ => {}
        }
    }
    let stepped = draft_mut(shell).map(|draft| {
        if forward {
            draft.toggle_selected()
        } else {
            draft.toggle_selected_back()
        }
    });
    match stepped {
        Some(Step::Changed) => {
            maybe_refresh_available(shell);
            revalidate(shell);
            scroll_to_cursor(shell);
            commit_change(shell, cx);
        }
        Some(Step::Refused(reason)) => refuse_step(shell, reason),
        _ => {
            let key = match (filtering, forward) {
                (false, true) => "space",
                (false, false) => "shift+space",
                (true, true) => "tab",
                (true, false) => "shift+tab",
            };
            set_notice(shell, format!("nothing on this row changes with {key}"));
        }
    }
}

/// A step the draft declined ([`Step::Refused`]), said with the verb that
/// does what the trader was reaching for.
///
/// The refusal this exists for is unticking a grouping slot's last
/// dimension (`Draft::step_selected`'s own doc has the model reason). The
/// trader wants that slot to stop grouping by the chain they just cleared,
/// and the config model has exactly one way to say it: get rid of the
/// slot's user-layer copy, which is `r` when the desk has one underneath
/// and `d` when the slot is the user's own. Naming the wrong verb would be
/// worse than naming none, so the hint is read off the same
/// [`target_row`] the action bar builds `d` and `r` from — a row that
/// offers neither (a desk-owned slot the user has not forked) gets the
/// reason alone rather than an invented verb.
fn refuse_step(shell: &mut ShellView, reason: String) {
    let hint = match target_row(shell) {
        // `overridden` implies the user layer has it too, so both verbs
        // are live here; `r` is the one that restores what is underneath,
        // which is what an emptied override is asking for.
        Some(row) if row.overridden => " — r restores the desk's copy",
        Some(row) if row.layer == Some(Layer::User) => " — d deletes it",
        _ => "",
    };
    set_notice(shell, format!("{reason}{hint}"));
}

/// The draft under the cursor, mutably, if the edit stage is open.
fn draft_mut(shell: &mut ShellView) -> Option<&mut Draft> {
    shell
        .object_dialog
        .as_mut()
        .and_then(|state| state.draft.as_mut())
}

/// [`draft_mut`], immutably — for a caller that only needs to read the
/// row under the cursor before deciding whether to mutate at all.
fn draft_ref(shell: &ShellView) -> Option<&Draft> {
    shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
}

/// After a `Toggle`/`ToggleBack` step that just changed a Views draft's `dataset`
/// field, rebuild the `columns` field's available catalogue for the newly chosen
/// dataset ("changing the dataset empties Available and repopulates it; members that
/// the new dataset lacks stay listed... so the diagnostic can name them").
///
/// Checked by which row the cursor is STILL on, not by domain alone: a
/// `Field` row's value changes in place (`Draft::step_selected` never
/// moves the cursor off it, unlike an item row's add), so after the step
/// the cursor is the one reliable way to ask "was that the dataset field"
/// without threading the answer through every caller. Every other
/// domain's fields, and every other Views field, leave `views::
/// refresh_available` untouched — it only ever rebuilds `columns`.
fn maybe_refresh_available(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    if state.domain != Domain::Views {
        return;
    }
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    let is_dataset_row = matches!(
        draft.selected_row(),
        Some(EditRow::Field(i)) if draft.fields.get(i).is_some_and(|f| f.key == "dataset")
    );
    if !is_dataset_row {
        return;
    }
    // Folded, not `services.config` alone: `views::dataset_catalogue` seeds each
    // catalogue row's presentation from `dataset_presentation.toml`, so inside the 250
    // ms write debounce — a Schema column-stage edit, escape out, open this dialog,
    // step this row — the plain read is the overlay as it stood BEFORE the last
    // keystroke, and a column promoted off that stale catalogue carries the stale layer
    // into the writer's comparison. Every other read of this layer on this branch is
    // folded.
    let folded = apply::config_with_pending(shell);
    let config = folded.as_ref().unwrap_or(&shell.services.config);
    let Some(draft) = shell
        .object_dialog
        .as_mut()
        .and_then(|state| state.draft.as_mut())
    else {
        return;
    };
    views::refresh_available(draft, config);
}

/// Read-only check of whether stepping would change this row. Notices use this instead
/// of mutating the draft to discover its capability.
fn selected_field_is_steppable(draft: &Draft) -> bool {
    match draft.selected_row() {
        Some(EditRow::Field(i)) => !matches!(
            draft.fields[i].kind,
            FieldKind::Text(_) | FieldKind::MultiChoice { .. } | FieldKind::OrderedList { .. }
        ),
        // Both list rows have a verb under `space`: an item's own inclusion toggles, an
        // available row is added.
        Some(EditRow::Item { .. } | EditRow::Available { .. }) => true,
        None => false,
    }
}

/// Is the column stage open? Read off the DRAFT, never the stage, for
/// [`commit_selected_row`]'s reason: the projection is what the verbs below actually
/// act on, so asking the thing that carries it keeps the two from ever disagreeing.
fn in_column_stage(shell: &ShellView) -> bool {
    shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .is_some_and(|draft| draft.column().is_some())
}

/// Is the Values stage open? [`in_column_stage`]'s own mirror, off [`Draft::values`]
/// for the same reason: the projection is what the verbs below actually act on.
fn in_values_stage(shell: &ShellView) -> bool {
    shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .is_some_and(|draft| draft.values().is_some())
}

/// Is this dialog open on `Domain::Scopes`, whichever of its stages is on
/// screen — the guard `MoveItem`'s and `Verb('x')`'s Scopes arms share,
/// since both this domain's lists (the dimensions list and the Values
/// stage's own) are unreorderable.
fn is_scopes(shell: &ShellView) -> bool {
    shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.domain == Domain::Scopes)
}

/// The one answer for a verb the column stage does not own: `x`, `shift+j` and
/// `shift+k` all reorder or demote rows of a list this stage does not install, and
/// `d`/`r` act on the whole view the crumb has narrowed away from — so each says the
/// same thing with its own key in it, rather than the edit stage's answer about an
/// object or rows that are not on screen.
fn not_a_column_verb(shell: &mut ShellView, key: &str) {
    set_notice(shell, format!("{key} is not a verb in a column's stage"));
}

/// The one answer for `d`/`r`/`o` inside the Values stage: the crumb has narrowed the
/// object to one dimension's values, and none of the three destructive verbs act on
/// those — the same reasoning [`not_a_column_verb`] gives for the column stage, with
/// its own wording since the remedy here (`escape`) is a single key rather than a stage
/// to name.
fn not_a_values_verb(shell: &mut ShellView) {
    set_notice(
        shell,
        "not a verb while picking values — escape first".to_string(),
    );
}

/// Set the footer notice, if a dialog is open at all.
fn set_notice(shell: &mut ShellView, notice: String) {
    if let Some(state) = shell.object_dialog.as_mut() {
        state.notice = Some(notice);
    }
}

/// Answer the question on screen: `yes` carries it out through
/// [`run_confirmed`] against the target recorded when it was armed; `no`
/// drops it. The one door the three answer sites (either stage's key
/// handler, the confirm row's buttons) go through, so the recorded
/// target is read out before the disarm clears it at every one of them.
fn answer_confirm(shell: &mut ShellView, yes: bool, cx: &mut Context<ShellView>) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    let Some(confirm) = state.confirm else {
        return;
    };
    let target = state.confirm_target.take();
    state.disarm();
    if yes {
        run_confirmed(shell, confirm, target, cx);
    }
}

/// Put a destructive question on screen. One door for the three arming
/// verbs, in either stage — the confirm lives on the dialog state, not
/// the draft, so the browse list can ask it too (see
/// `ObjectDialogState::confirm`).
fn arm_confirm(shell: &mut ShellView, confirm: Confirm) {
    // Resolved BEFORE the borrow below, and stored beside the question:
    // `run_confirmed` compares it against the target as it stands when
    // the answer arrives (see `ObjectDialogState::confirm_target`).
    let target = target_object(shell);
    if let Some(state) = shell.object_dialog.as_mut() {
        state.confirm = Some(confirm);
        state.confirm_target = target;
    }
}

/// The question currently on screen, if any.
fn armed_confirm(shell: &ShellView) -> Option<Confirm> {
    shell.object_dialog.as_ref().and_then(|state| state.confirm)
}

/// Queue the changed draft and announce a definition fork if one is accepted. Read
/// `would_fork` before the commit advances its baseline. A validation or directory
/// refusal takes precedence over the fork notice. Memory application and disk writes
/// happen during the shared flush; the draft already shows the edit.
fn commit_change(shell: &mut ShellView, cx: &mut Context<ShellView>) {
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
    let fork =
        super::apply::would_fork(shell, domain).then(|| super::apply::fork_notice(shell, domain));
    match super::apply::commit_edit(shell, cx) {
        Some(refusal) => set_notice(shell, refusal),
        None => {
            if let Some(notice) = fork {
                set_notice(shell, notice);
            }
        }
    }
}

/// Put the edit list's viewport back over the draft's cursor.
///
/// Every verb that moves the row the cursor is on has to call this, not
/// just the ones that look like motions: `space`/`shift+space` promote a
/// row to the end of the object's own list and `x` demotes one to the
/// end of the available catalogue, both of which are routinely a
/// screenful away on a list with more rows than the panel can show — and
/// a cursor left off screen makes the next `j` look like a jump.
/// `shift+j`'s arm was the
/// only one that did call it, inline; all four go through here now, so
/// the next verb that moves a row has one obvious thing to call rather
/// than a snippet to copy from whichever arm happens to have it.
fn scroll_to_cursor(shell: &mut ShellView) {
    let selected = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .map(|draft| draft.selected)
        .unwrap_or(0);
    shell.object_dialog_scroll.scroll_to_item(selected);
}

/// Re-run [`Domain::validate`] over the draft as it now stands — and,
/// first, fold the column stage's fields back onto the item they came
/// from.
///
/// The fold lives here because this is the one function every changed
/// value passes through on its way to [`commit_change`]: the step
/// arms (`space`/`shift+space`), a committed text field, `x`, and the
/// tick click all call it, and none of them knows or should know that a
/// projection is open. A fold anywhere else would be a fold each of those
/// call sites had to remember.
fn revalidate(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    let domain = state.domain;
    let Some(draft) = state.draft.as_mut() else {
        return;
    };
    // The key a cleared `label`/`width` handed back, and the layer it
    // fell to — see the notice at the end of this function.
    let mut fold = None;
    // Fold column fields into the item before validating or rendering a write.
    if draft.column().is_some() {
        fold = draft.fold_column();
    }
    // Scopes: fields → source on every change, so the validator and the writer read
    // this keystroke. The Values stage's own fold (`fold_values`) runs FIRST, ahead of
    // `fold`: it is what keeps the stashed `dimensions` list's note in step with the
    // ticked values, and `fold`'s own `kept` guard already knows to leave that stashed
    // list's `source` entry alone while the stage is open (its own doc comment).
    if domain == Domain::Scopes {
        if draft.values().is_some() {
            scopes::fold_values(draft);
        }
        scopes::fold(draft);
    }
    // Validated, then stored: `validate` needs the draft immutably and
    // the config from a sibling field, which is exactly the disjoint
    // borrow the compiler allows here and a `&mut self` method would not.
    let diagnostics = domain.validate(draft, &shell.services.config);
    if let Some(draft) = draft_mut(shell) {
        draft.diagnostics = diagnostics;
    }
    // Explain a cleared key's inherited value after reseeding its field. Name the
    // actual fallback layer: dataset presentation takes precedence over the view
    // definition. No lower layer means the field uses its default without a notice.
    if let Some(Fold { key, to: Some(to) }) = fold {
        let layer = match to {
            FellTo::Desk => "the desk",
            FellTo::Dataset => "the dataset",
            FellTo::EachView => "each view",
        };
        set_notice(shell, format!("{key} follows {layer} again"));
    }
}

/// Return to browse after an accepted removal. From an edit stage, restore the
/// surviving object's row by name or choose a neighbour if it was deleted. From browse,
/// preserve the query and clamp selection against pending-aware rows. Keep the current
/// mode; input synchronization settles focus without changing a filtered confirmation
/// into Normal mode.
fn after_removal(shell: &mut ShellView, name: &str, cx: &mut Context<ShellView>) {
    let in_browse = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.stage == Stage::Browse);
    if !in_browse {
        leave_edit(shell, cx);
        return;
    }
    let rows = landing_rows(shell);
    if let Some(state) = shell.object_dialog.as_mut() {
        let visible = super::visible_rows(state, &rows);
        // By name where the object survives (a revert — and on Sources
        // it may have re-sorted, if the override moved `dataset`), else
        // the clamp (a delete).
        state.selected = super::filtered_position(&visible, &rows, name)
            .unwrap_or_else(|| state.selected.min(visible.len().saturating_sub(1)));
        let selected = state.selected;
        shell.object_dialog_scroll.scroll_to_item(selected);
    }
    cx.notify();
}

/// The rows a landing after a write resolves the cursor against: the PENDING-aware
/// config (`apply::config_with_pending`), never `services.config` alone.
/// `commit_removal` (and `commit_edit`) apply to memory from a spawned task, AFTER the
/// handler that queued them returns — so at the moment `after_removal` or `leave_edit`
/// runs, `services.config` still lists the object just deleted, and a clamp against it
/// is a no-op that leaves `selected` one past the end once the flush lands. The same
/// fold `enter_edit_stage` derives its draft from, for the same reason.
fn landing_rows(shell: &ShellView) -> Vec<ObjectRow> {
    let Some(state) = shell.object_dialog.as_ref() else {
        return Vec::new();
    };
    match apply::config_with_pending(shell) {
        Some(config) => state.domain.objects(&config),
        None => derive_rows(shell),
    }
}

fn leave_edit(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let name = match shell.object_dialog.as_ref().map(|state| &state.stage) {
        Some(Stage::Edit { object } | Stage::Column { object, .. }) => object.clone(),
        _ => String::new(),
    };
    if let Some(state) = shell.object_dialog.as_mut() {
        state.leave_edit();
    }
    let rows = landing_rows(shell);
    // By name where the object survives; where it is gone (a delete,
    // whose name the pending-aware rows no longer hold) the neighbour —
    // its position in the rows as they still stand, clamped to the list
    // the removal leaves — so an edit-stage delete lands exactly where a
    // browse delete does (`after_removal`), never on row 0.
    let before = derive_rows(shell);
    if let Some(state) = shell.object_dialog.as_mut() {
        let visible = super::visible_rows(state, &rows);
        state.selected = super::filtered_position(&visible, &rows, &name).unwrap_or_else(|| {
            let was = super::visible_rows(state, &before);
            super::filtered_position(&was, &before, &name)
                .unwrap_or(0)
                .min(visible.len().saturating_sub(1))
        });
    }
    let selected = shell
        .object_dialog
        .as_ref()
        .map(|state| state.selected)
        .unwrap_or(0);
    shell.object_dialog_scroll.scroll_to_item(selected);
    cx.notify();
}

/// Resolve the open object or selected browse row for confirmation. Naming and Values
/// return no target. Column projections resolve their parent object, but mutating verbs
/// reject that stage before acting. Record the target on arming and compare again on
/// confirmation because reload can reorder browse rows.
fn target_object(shell: &ShellView) -> Option<String> {
    let state = shell.object_dialog.as_ref()?;
    match &state.stage {
        Stage::Edit { object } | Stage::Column { object, .. } => Some(object.clone()),
        Stage::Browse => {
            let rows = derive_rows(shell);
            let visible = super::visible_rows(state, &rows);
            selected_row(state, &rows, &visible).map(|row| row.name.clone())
        }
        // Values-stage rows do not expose whole-scope destructive operations.
        Stage::Naming | Stage::Values { .. } => None,
    }
}

/// The browse row for [`target_object`] — where `layer` and `overridden`
/// come from, so `d` and `r` are gated by the one tested derivation
/// rather than by a second guess made here. One `derive_rows` per call,
/// whichever stage: a paint-path caller that already holds the rows
/// (`browse_action_bar`) reads [`selected_row`] instead.
fn target_row(shell: &ShellView) -> Option<ObjectRow> {
    let state = shell.object_dialog.as_ref()?;
    let rows = derive_rows(shell);
    match &state.stage {
        Stage::Edit { object } | Stage::Column { object, .. } => {
            rows.into_iter().find(|row| &row.name == object)
        }
        Stage::Browse => {
            let visible = super::visible_rows(state, &rows);
            selected_row(state, &rows, &visible).cloned()
        }
        // Same reasoning as `target_object`'s own `Values` arm.
        Stage::Naming | Stage::Values { .. } => None,
    }
}

/// Whether the dialog is in its browse stage.
fn in_browse(shell: &ShellView) -> bool {
    shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.stage == Stage::Browse)
}

/// What `d`/`r` say when [`target_row`] answers `None`: in browse the
/// filter is hiding every row; in the edit stage the object is open but
/// not yet a row `services.config` can produce (the tick after `n`
/// creates it — see `actions()`).
fn no_target_notice(shell: &ShellView) -> String {
    if in_browse(shell) {
        "no object is selected".to_string()
    } else {
        "nothing is open".to_string()
    }
}

/// The browse row under the cursor — `state.selected` is an index into
/// the FILTERED list, resolved back through `rows`. Pure over what the
/// caller already derived, so `build` can hand its own `rows`/`visible`
/// to the bar without deriving them a second time per frame.
fn selected_row<'a>(
    state: &ObjectDialogState,
    rows: &'a [ObjectRow],
    visible: &[crate::listfilter::Ranked],
) -> Option<&'a ObjectRow> {
    visible.get(state.selected).and_then(|m| rows.get(m.row))
}

/// Collect keys actually present in the user layer for deletion or reversion, including
/// a recorded override sidecar entry. Missing entries produce no write. Return keys
/// only; `commit_removal` constructs removals and cannot accept an ungated replacement
/// value. All keys join the same application/write batch.
fn removal_edits(
    shell: &ShellView,
    docs: &[&'static str],
) -> Result<Vec<(&'static str, String)>, String> {
    // Same rule `target_row` states: a removal armed from the column
    // stage removes the OBJECT, which is what `d`/`r` mean there too —
    // and from browse, the row under the cursor.
    let Some(name) = target_object(shell) else {
        return Err("no object is selected".to_string());
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
    let mut keys: Vec<(&'static str, String)> =
        touched.into_iter().map(|doc| (doc, name.clone())).collect();
    // the sidecar entry rides the same removal — never created just to remove nothing,
    // hence only the keys the sidecar actually holds rather than an unconditional key.
    if let Some(domain) = shell.object_dialog.as_ref().map(|state| state.domain) {
        for okey in super::override_keys_of(&shell.services.config, domain.doc(), &name) {
            keys.push((super::OVERRIDES_DOC, okey));
        }
    }
    Ok(keys)
}

/// Arm deletion only for a user-defined object. Inherited objects cannot be deleted by
/// a user-layer write. A presentation-only override instead points to revert; an
/// unconfigured grouping slot reports that it is empty.
fn arm_delete(shell: &mut ShellView) {
    // Object deletion is unavailable inside column and Values projections. Apply the
    // guard here so pointer and keyboard actions enforce the same rule.
    if in_column_stage(shell) {
        not_a_column_verb(shell, "d");
        return;
    }
    // the same reasoning, one stage over — `d`'s confirmed effect is on the whole
    // SCOPE, not the one dimension's values the crumb has narrowed to.
    if in_values_stage(shell) {
        not_a_values_verb(shell);
        return;
    }
    match target_row(shell) {
        Some(row) if row.layer == Some(Layer::User) => {
            arm_confirm(shell, Confirm::Delete);
        }
        // The remedy names the stage: from the list there is nothing to
        // tick, the slot has to be opened first.
        Some(row) if row.layer.is_none() => {
            let remedy = if in_browse(shell) {
                "open it to fill it"
            } else {
                "tick a dimension to fill it"
            };
            set_notice(shell, format!("{} is empty — {remedy}", row.name))
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
                    row.layer.map(Layer::name).unwrap_or("no")
                ),
            )
        }
        None => set_notice(shell, no_target_notice(shell)),
    }
}

/// Arm reversion when a user definition or presentation overlays an inherited object.
/// User-only objects cannot revert because no inherited object remains. The view
/// overlay counts even when its definition was never copied.
fn arm_revert(shell: &mut ShellView) {
    // Revert affects the whole object and associated view presentation, so it is
    // unavailable from a single-column projection.
    if in_column_stage(shell) {
        not_a_column_verb(shell, "r");
        return;
    }
    // `arm_delete`'s own values-stage guard, for the same reason.
    if in_values_stage(shell) {
        not_a_values_verb(shell);
        return;
    }
    match target_row(shell) {
        Some(row) if row.overridden => {
            arm_confirm(shell, Confirm::Revert);
        }
        Some(row) => set_notice(
            shell,
            format!("{} has no user override to revert", row.name),
        ),
        None => set_notice(shell, no_target_notice(shell)),
    }
}

/// Replace a saved scope with the current frame scope. A user-owned target requires
/// confirmation because its previous contents will be lost. An inherited target is
/// copied into the user layer and announced; its lower-layer copy remains available to
/// revert. Check target ownership before mutating the draft.
fn overwrite_scope(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(state) = shell.object_dialog.as_ref() else {
        set_notice(shell, "nothing is open".to_string());
        return;
    };
    if state.domain != Domain::Scopes {
        set_notice(shell, "o is not a verb here".to_string());
        return;
    }
    if state.draft.is_none() {
        set_notice(shell, "nothing is open".to_string());
        return;
    }
    // `arm_delete`'s own values-stage guard: `o` overwrites the whole
    // SCOPE, not the one dimension's values the crumb has narrowed to.
    if in_values_stage(shell) {
        not_a_values_verb(shell);
        return;
    }
    let forks = target_row(shell).is_some_and(|row| row.layer != Some(Layer::User));
    if !forks {
        arm_confirm(shell, Confirm::Overwrite);
        return;
    }
    let notice = super::apply::fork_notice(shell, Domain::Scopes);
    if run_overwrite(shell, cx) {
        set_notice(shell, format!("replaced with the frame's scope; {notice}"));
    }
}

/// Replace source and fields with current frame scope, revalidate, and use the shared
/// edit commit path. Refuse other domains. Detect no-op overwrites explicitly so the
/// notice distinguishes an unchanged scope from a queued change. Return whether
/// anything was queued; all outcomes set an appropriate notice.
fn run_overwrite(shell: &mut ShellView, cx: &mut Context<ShellView>) -> bool {
    if shell
        .object_dialog
        .as_ref()
        .is_none_or(|state| state.domain != Domain::Scopes)
    {
        cx.notify();
        return false;
    }
    let scope = shell.frame.read(cx).scope().clone();
    let config = shell.services.config.clone();
    let changed = draft_mut(shell).is_some_and(|draft| {
        scopes::overwrite_with(draft, &scope, &config);
        draft.is_dirty()
    });
    revalidate(shell);
    let queued = if !changed {
        set_notice(
            shell,
            "already matches the frame's scope — nothing to write".to_string(),
        );
        false
    } else if let Some(notice) = super::apply::commit_edit(shell, cx) {
        set_notice(shell, notice);
        false
    } else {
        true
    };
    cx.notify();
    queued
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
    armed_target: Option<String>,
    cx: &mut Context<ShellView>,
) {
    let domain = match shell.object_dialog.as_ref() {
        Some(state) => state.domain,
        None => return,
    };
    // The browse cursor is an INDEX, and a config reload landing between
    // the arming keystroke and this one (a desk push, an external editor
    // — never this dialog's own writes, which keep the name) re-ranks
    // the list under the question: the prompt named one object and the
    // answer would remove another, irreversibly. So the question is
    // answered for the object it was asked about or not at all. In the
    // edit stage the target is the stage's own object and the two
    // always agree, so the check costs a compare there and nothing else.
    if armed_target != target_object(shell) {
        set_notice(
            shell,
            "the list changed under the question — nothing was removed".to_string(),
        );
        return;
    }
    match confirm {
        // `o`'s confirmed answer on a user-owned scope — the same act the
        // desk-owned case runs unasked from `overwrite_scope`.
        Confirm::Overwrite => {
            run_overwrite(shell, cx);
        }
        // `d`/`r` share `apply::commit_removal` with `commit_change`'s
        // `commit_edit`: same batch, same flush, same failure
        // revert — deliberately without `blocking_diagnostic`'s gate (see
        // `commit_removal`'s own doc for why a removal must never be
        // blocked by the very diagnostic it would resolve).
        Confirm::Delete | Confirm::Revert => {
            // The domain's own doc always applies; its presentation doc
            // only when it has one at all (`Domain::presentation_doc` —
            // Groupings has none, so there is nothing else to remove).
            let mut docs = vec![Destination::Doc.doc(domain)];
            if let Some(presentation) = domain.presentation_doc() {
                docs.push(presentation);
            }
            match removal_edits(shell, &docs) {
                Ok(keys) => {
                    // named in the notice only when `removal_edits` actually found an
                    // entry to remove — `keys` decides, same as every other doc in this
                    // list.
                    docs.push(super::OVERRIDES_DOC);
                    // Preserves `docs`' own order rather than whatever
                    // order `keys` happens to hold, so a notice naming
                    // both files reads "views and view_presentation" the
                    // way it always has.
                    // The same target `removal_edits` just resolved —
                    // `keys` carries it too, but reading it back off a
                    // doc key would tie the notice to the sidecar's
                    // spelling.
                    let name = target_object(shell).unwrap_or_default();
                    let files: Vec<String> = docs
                        .into_iter()
                        .filter(|doc| keys.iter().any(|(d, _)| d == doc))
                        .map(|doc| format!("{doc}.toml"))
                        .collect();
                    let verb = if confirm == Confirm::Delete {
                        "deleted"
                    } else {
                        "reverted"
                    };
                    let outcome = apply::commit_removal(shell, keys, cx);
                    after_removal(shell, &name, cx);
                    match outcome {
                        // The removal joined the batch; the flush (no
                        // debounce of its own) is already under way, so
                        // the outcome is said plainly — not hedged with
                        // an ellipsis the way a pending write once was.
                        None => {
                            set_notice(shell, format!("{verb} {name} in {}", files.join(" and ")))
                        }
                        Some(notice) => set_notice(shell, notice),
                    }
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

/// Available action-bar verbs in display order. Field edits persist through the shared
/// batch without a Save action. Deletion and reversion require an effective target row;
/// they can therefore be unavailable until a new object's zero-delay creation reaches
/// active configuration. The `is_new` header badge remains a property of the draft for
/// its lifetime.
fn actions(shell: &ShellView) -> Vec<Action> {
    let Some(state) = shell.object_dialog.as_ref() else {
        return Vec::new();
    };
    let Some(draft) = state.draft.as_ref() else {
        return Vec::new();
    };
    if !state.domain.writable(&state.stage) {
        return Vec::new();
    }
    // Object delete, revert, and overwrite do not act within column or Values
    // projections. A column's editable label or width can still expose `i`.
    let in_column = draft.column().is_some() || draft.values().is_some();
    let row = target_row(shell);
    let mut out = Vec::new();
    if !in_column && row.as_ref().is_some_and(|r| r.layer == Some(Layer::User)) {
        out.push(Action {
            key: "d",
            label: format!("Delete this {}", object_word(state.domain)),
            destructive: true,
        });
    }
    if !in_column && row.as_ref().is_some_and(|r| r.overridden) {
        out.push(Action {
            key: "r",
            label: "Revert to desk".to_string(),
            destructive: true,
        });
    }
    // `i` is a button wherever the selected row is one it opens — the footer's own test
    // (`RowVocabulary`), plus Groupings' whole-chain `i` which is live on every row
    // there. Never while a text field is open, though `build_edit` withdraws the whole
    // bar there anyway.
    let types = matches!(
        draft.selected_vocabulary(state.domain),
        RowVocabulary::Types | RowVocabulary::StepsAndTypes
    ) || state.domain == Domain::Groupings;
    if types && draft.text_entry.is_none() {
        out.push(Action {
            key: "i",
            label: "Edit value".to_string(),
            destructive: false,
        });
    }
    // The one new verb this domain adds (this module's `arm_overwrite`
    // doc has the full reasoning): available whenever a scope is open,
    // regardless of layer or override, since `Confirm::Overwrite`'s own
    // commit forks a desk-owned scope the same way any other definitional
    // edit would.
    if !in_column && state.domain == Domain::Scopes {
        out.push(Action {
            key: "o",
            label: "Overwrite from frame".to_string(),
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

/// Build the current dialog stage from the borrowed shell state. Handlers capture the
/// entity for later input; they must not re-read it during this render borrow.
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.object_dialog.as_ref() else {
        return div().into_any_element();
    };
    // Edit, Column, and Values all paint the current draft. Routing a projection to
    // browse would display objects while keystrokes mutate unseen draft fields.
    if matches!(
        state.stage,
        Stage::Edit { .. } | Stage::Column { .. } | Stage::Values { .. }
    ) {
        return build_edit(shell, entity, cx);
    }
    // The same one derivation path `handle_key` uses — a second spelling
    // here is how a render and its key handling come to disagree about
    // which rows exist.
    let rows = derive_rows(shell);
    let theme = cx.theme();
    // Copied out so the row closures below don't hold the `theme` borrow.
    let row_paint = super::super::listrow::row_paint(theme);

    // Read named colours and theme anchors/tokens once for the whole Colors
    // list. Swatches share these inputs, avoiding repeated theme conversions
    // per row. Other domains do not load the colour document.
    let named_colours: Option<(
        geode_core::colour::NamedColours,
        geode_core::colour::Anchors,
        geode_core::colour::Tokens,
    )> = (state.domain == Domain::Colors).then(|| {
        let empty = geode_core::config::MergedDoc::default();
        let doc = shell.services.config.doc(colours::DOC).unwrap_or(&empty);
        (
            geode_core::colour::NamedColours::from_doc(doc).0,
            colour_theme::anchors_from_theme(theme),
            colour_theme::tokens_from_theme(theme),
        )
    });

    let visible = super::visible_rows(state, &rows);

    let mut list = v_flex()
        .id("objectdialog-list")
        .w(scale::design(WIDTH))
        .h(scale::design(
            (visible.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT),
        ))
        .overflow_y_scroll()
        .track_scroll(&shell.object_dialog_scroll)
        .debug_selector(|| "objectdialog-list".to_string());

    for (position, m) in visible.iter().enumerate() {
        let Some(row) = rows.get(m.row) else { continue };
        let is_selected = position == state.selected;

        let display = row.display_name();
        let name_len = display.chars().count();
        // The same `"{a} {b}"` split both list dialogs use — this is its
        // third consumer, and the reason it lives in one place: the
        // arithmetic is only correct while every `searchable_text` in the
        // crate keeps that exact shape.
        let (name_ix, summary_ix) = split_label_indices(&m.indices, name_len);

        let row_el = h_flex()
            .id(("objectdialog-row", m.row))
            .w_full()
            .justify_between()
            .items_center()
            .gap_3()
            .px_2()
            .py_1()
            .rounded(theme.radius);
        let row_el = super::super::listrow::paint_row(row_el, row_paint, is_selected);

        // a prefixed row paints `<prefix> · ` dimmed and the name after it, as two runs
        // of one highlighted label — the indices are split at the prefix's end so a hit
        // inside the dataset still highlights there. `cut` is the prefix run's length
        // in the painted `display` text (`"{prefix} · "`, three chars for the
        // separator), matching `ObjectRow::display_name`'s own join.
        let head: AnyElement = match &row.prefix {
            Some(prefix) => {
                let cut = prefix.chars().count() + 3;
                let (in_prefix, in_name): (Vec<usize>, Vec<usize>) =
                    name_ix.iter().copied().partition(|i| *i < cut);
                let in_name: Vec<usize> = in_name.into_iter().map(|i| i - cut).collect();
                h_flex()
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(highlighted_text(
                                &format!("{prefix} · "),
                                &in_prefix,
                                row_paint.accent,
                            )),
                    )
                    .child(highlighted_text(&row.name, &in_name, row_paint.accent))
                    .into_any_element()
            }
            None => highlighted_text(&row.name, &name_ix, row_paint.accent),
        };

        let label = v_flex().gap_0p5().child(head).child(
            div()
                .font_family(crate::fonts::MONO)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(highlighted_text(
                    &row.summary,
                    &summary_ix,
                    row_paint.accent,
                )),
        );

        // Show the winning layer, user override, and drift markers. Unconfigured roster
        // entries have no winning layer and receive no fabricated provenance.
        let mut markers = h_flex().gap_1().items_center();
        if let Some(layer) = row.layer {
            markers = markers.child(dialog::badge(
                layer.name(),
                theme.muted_foreground,
                theme.border,
                Some(format!("objectdialog-layer-{}", row.name)),
                cx,
            ));
        }
        if row.overridden {
            markers = markers.child(dialog::badge(
                "overridden",
                theme.primary,
                theme.primary,
                Some(format!("objectdialog-overridden-{}", row.name)),
                cx,
            ));
        }
        // real once `derive_rows` has a sidecar entry to compare against
        // (`ObjectRow::drifted`'s own doc has the full rule) — this row simply paints
        // whatever it is handed.
        if row.drifted {
            markers = markers.child(dialog::badge(
                "drifted",
                theme.muted_foreground,
                theme.border,
                Some(format!("objectdialog-drifted-{}", row.name)),
                cx,
            ));
        }

        // a swatch before the label, resolved from this row's own saved color —
        // painted only when the color actually resolves (a dropped or invalid one
        // paints no swatch, never a fallback that would misrepresent it).
        let swatch = named_colours
            .as_ref()
            .and_then(|(named, anchors, tokens)| {
                named.get(&row.name).map(|def| (def, anchors, tokens))
            })
            .map(|(def, anchors, tokens)| {
                let hsla = colour_theme::to_hsla(geode_core::colour::resolve(def, anchors, tokens));
                dialog::swatch(hsla, format!("objectdialog-swatch-{}", row.name), cx)
            });

        let entity_for_row = entity.clone();
        let clicked = row.name.clone();
        let selector_name = row.name.clone();
        let row_el = row_el
            .children(swatch)
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
        // a domain with nothing in it is a fact about the config — and
        // an empty state names the next action (design guide) where
        // there is one: `n` creates an object on every writable domain,
        // while the schema's rows come from `datasets.toml` alone.
        let message = if rows.is_empty() {
            let word = state.domain.title().to_lowercase();
            if state.domain.writable(&Stage::Browse) {
                format!("no {word} are configured — n creates one")
            } else {
                format!("no {word} are configured")
            }
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

    // the naming stage replaces the filter row with the name field and states its own
    // two-verb vocabulary — never the browse footer's, even though `begin_naming`
    // leaves `state.mode` at `Filter` (the `Input` really does own the keys) — a footer
    // offering `/` or `j`/`k` while the name field is focused would be advertising keys
    // the field, not this handler, would consume.
    let naming = matches!(state.stage, Stage::Naming);

    // Advertise only keys supported by the current stage and mode. Filter-mode
    // Enter keeps the query; normal-mode Enter opens the selected object.
    let hints: Vec<Hint> = if state.confirm.is_some() {
        // The edit footer's own three lines: a question on screen is the
        // whole vocabulary until it is answered.
        vec![
            Hint::prose(HintRow::Go, "this needs an answer first"),
            Hint::new(HintRow::Go, &["enter"], "go ahead"),
            Hint::new(HintRow::Go, &["escape"], "leave it alone"),
        ]
    } else if naming {
        vec![
            Hint::new(HintRow::Go, &["enter"], "create"),
            Hint::new(HintRow::Go, &["escape"], "cancel"),
        ]
    } else {
        match state.mode {
            DialogMode::Normal => {
                let mut hints = vec![
                    Hint::new(HintRow::Move, &["j", "k"], "move"),
                    Hint::new(HintRow::Move, &["ctrl+d", "ctrl+u"], "±5"),
                    Hint::new(HintRow::Move, &["ctrl+f", "ctrl+b"], "±10"),
                ];
                // Creation is unavailable for fixed rosters and read-only domains, so
                // no creation hint is painted there.
                if state.domain.writable(&state.stage) && state.domain.roster().is_none() {
                    hints.push(Hint::new(HintRow::Edit, &["n"], "new"));
                }
                // `c`: Scopes alone, beside `n`.
                if state.domain.duplicable() {
                    hints.push(Hint::new(HintRow::Edit, &["c"], "copy"));
                }
                // a digit opens that slot — Groupings only, the one domain whose
                // objects are numbered.
                if state.domain == Domain::Groupings {
                    hints.push(Hint::range(HintRow::Go, "1", "9", "open slot"));
                }
                hints.push(
                    Hint::new(HintRow::Go, &["enter"], "open").selector("objectdialog-hint-enter"),
                );
                hints.push(Hint::new(HintRow::Go, &["/"], "filter"));
                // Honest about which rung the next escape takes: with a
                // query still applied it clears the query, and only then
                // closes.
                hints.push(Hint::new(
                    HintRow::Go,
                    &["escape"],
                    if state.query.is_empty() {
                        "close"
                    } else {
                        "clear the filter"
                    },
                ));
                hints
            }
            DialogMode::Filter => vec![
                Hint::prose(HintRow::Move, "type to filter"),
                Hint::new(HintRow::Move, &["up", "down"], "move"),
                Hint::new(HintRow::Move, &["ctrl+d", "ctrl+u"], "±5"),
                Hint::new(HintRow::Move, &["ctrl+f", "ctrl+b"], "±10"),
                // Filter exits affect the query only; opening a match requires
                // another Enter in normal mode.
                Hint::new(HintRow::Go, &["enter"], "keep the filter")
                    .selector("objectdialog-hint-enter"),
                Hint::new(HintRow::Go, &["escape"], "discard the filter"),
            ],
        }
    };
    let hint_line = dialog::hint_rows(&hints);
    let footer = v_flex()
        .w(scale::design(WIDTH))
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
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(hint_line),
        );

    // The live `Input` renders only when it actually owns the keystrokes.
    // In normal mode the same query paints as static muted text — see
    // this module's own "one switch" note. Naming always focuses the
    // field (`begin_naming` sets `Filter`), so `frozen_query` is moot
    // there — the naming row below is a `name_row`, never a `filter_row`.
    // `slash_filters: true` unconditionally — this dialog has no capture
    // state, so `/` enters filter mode from every frozen moment it has
    // (see `dialog::FrozenFilter`).
    let frozen_query = (state.mode == DialogMode::Normal).then_some(dialog::FrozenFilter {
        query: state.query.as_str(),
        slash_filters: true,
        entity: entity.clone(),
    });

    let top_row = if naming {
        // Name the copy source or current-frame seed in the naming header.
        let label = match &state.naming_seed {
            NameSeed::CopyOf(src) => format!("Copy of {src} · name"),
            NameSeed::Empty => format!("New {} · name", object_word(state.domain)),
            NameSeed::FromFrame => "Save scope · name".to_string(),
        };
        dialog::name_row(&shell.dialog_input, &label, cx)
    } else {
        dialog::filter_row(&shell.dialog_input, frozen_query, cx)
    };

    // the verbs as buttons, between the list and the footer — the browse stage's own
    // action bar, in the edit stage's place for it (outside the list, so the rows never
    // shift under it) — and, while a question stands, the confirm row in the bar's
    // place, exactly as `build_edit` swaps them.
    let action_block = match state.confirm {
        // The RECORDED target, never the cursor's current answer: after
        // a reload re-ranks the list the index names a different row,
        // and the prompt must name the object the answer is about — the
        // one `run_confirmed` will refuse for otherwise. Also the one
        // read here that derives nothing per frame.
        Some(confirm) => {
            let name = state.confirm_target.clone().unwrap_or_default();
            confirm_row(confirm, &name, entity, cx)
        }
        None => browse_action_bar(state, &rows, &visible, entity),
    };

    v_flex()
        .gap_2()
        .child(top_row)
        .child(list)
        .child(action_block)
        .child(footer)
        .into_any_element()
}

/// Typed-entry label shared by footer sites: choice rows invite choosing a value; other
/// editable rows invite typing one.
pub(crate) fn i_hint_word(row: Option<EditRow>, draft: &Draft) -> &'static str {
    let chooses = matches!(
        row,
        Some(EditRow::Field(i)) if matches!(draft.fields[i].kind, FieldKind::Choice { .. })
    );
    if chooses {
        "choose a value"
    } else {
        "type a value"
    }
}

/// The edit stage: the object's header, its diagnostics, the scrolling
/// row list, and — **outside** that scroll — the action bar.
///
/// The bar sits outside the list on purpose. An earlier design made every verb a row,
/// and the list then changed length the moment a draft went dirty: a `Save changes` row
/// appeared under the cursor and the row the user was aiming at moved. Keeping the
/// verbs below the scroll means nothing above them ever shifts.
fn build_edit(shell: &ShellView, entity: &Entity<ShellView>, cx: &mut App) -> AnyElement {
    let Some(state) = shell.object_dialog.as_ref() else {
        return div().into_any_element();
    };
    let Some(draft) = state.draft.as_ref() else {
        return div().into_any_element();
    };
    // Build actions before borrowing the theme. Hide the action bar while a value field
    // is open: pointer actions must not arm a destructive question over a focused text
    // input when keyboard actions cannot reach those verbs.
    let action_block = match (draft.text_entry.is_some(), state.confirm) {
        // `min_h_6` for the same reason `action_bar` itself carries it: this
        // placeholder sits where that bar would, and a bare `div()` with no children
        // has no height of its own, so opening a field (`i`) would shift the footer up
        // by a button's height and `escape` would shift it back.
        (true, _) => div().min_h_6().into_any_element(),
        (false, Some(confirm)) => confirm_row(confirm, &draft.name, entity, cx),
        (false, None) => action_bar(shell, entity),
    };
    let theme = cx.theme();
    let row_paint = super::super::listrow::row_paint(theme);
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    // Pointer states for the two controls a row carries besides itself:
    // the steppable value chip (a filled chip) and the tick (a bare glyph).
    // Both sit on `popover`, the modal panel's fill. The tick hands in
    // its UNTICKED text; a ticked tick's `success` is painted per row
    // below, so the ticked form is derived here too.
    let chip_states = control::paint(
        theme,
        control::Rest::Filled(chip_bg),
        theme.popover,
        chip_fg,
    );
    let tick_states = control::paint(theme, control::Rest::Bare, theme.popover, chip_fg);
    let ticked_states = control::paint(theme, control::Rest::Bare, theme.popover, theme.success);
    let row = target_row(shell);

    // on Colors, the swatch beside the name — resolved from the draft's own live
    // fields (`colours::definition_of`), not from the saved `colours.toml`, so stepping
    // the hue repaints it before any write lands. `None` (no swatch) only if the draft
    // somehow lacks a `hue` row, which `colours::fields` never produces. A color that
    // tints by sign paints a triad — negative, base, positive — since the tint is the
    // thing the trader ticked the row to see.
    let name_child = match (state.domain, colours::definition_of(draft)) {
        (Domain::Colors, Some(def)) => {
            let anchors = colour_theme::anchors_from_theme(theme);
            let tokens = colour_theme::tokens_from_theme(theme);
            let variant = |sign: geode_core::colour::Sign| {
                colour_theme::to_hsla(geode_core::colour::resolve_signed(
                    &def, sign, &anchors, &tokens,
                ))
            };
            let base = variant(geode_core::colour::Sign::Zero);
            h_flex()
                .gap_2()
                .items_center()
                .when(def.tint_sign, |el| {
                    el.child(dialog::swatch(
                        variant(geode_core::colour::Sign::Negative),
                        "objectdialog-swatch-header-negative".to_string(),
                        cx,
                    ))
                })
                .child(dialog::swatch(
                    base,
                    "objectdialog-swatch-header".to_string(),
                    cx,
                ))
                .when(def.tint_sign, |el| {
                    el.child(dialog::swatch(
                        variant(geode_core::colour::Sign::Positive),
                        "objectdialog-swatch-header-positive".to_string(),
                        cx,
                    ))
                })
                .child(div().text_lg().child(draft.name.clone()))
                .into_any_element()
        }
        _ => div().text_lg().child(draft.name.clone()).into_any_element(),
    };

    // The object header: its name, and the same two provenance markers
    // the browse row carries, so opening an object never loses the
    // context the list gave it.
    let mut header = h_flex()
        .w(scale::design(WIDTH))
        .items_center()
        .justify_between()
        .gap_3()
        .child(name_child)
        .debug_selector(|| "objectdialog-edit-header".to_string());
    if row.is_some() || draft.is_new {
        let mut markers = h_flex().gap_1().items_center();
        if let Some(row) = row.as_ref() {
            if let Some(layer) = row.layer {
                markers = markers.child(dialog::badge(
                    layer.name(),
                    theme.muted_foreground,
                    theme.border,
                    None,
                    cx,
                ));
            }
            if row.overridden {
                markers = markers.child(dialog::badge(
                    "overridden",
                    theme.primary,
                    theme.primary,
                    None,
                    cx,
                ));
            }
            // the same tokens the browse row's own `drifted` badge uses — one
            // classification, one set of colors.
            if row.drifted {
                markers = markers.child(dialog::badge(
                    "drifted",
                    theme.muted_foreground,
                    theme.border,
                    None,
                    cx,
                ));
            }
        }
        // `n` this session, and still true for the whole life of the stage regardless
        // of `row` — `target_row` derives from `services.config`, which stays behind
        // `commit_create`'s own zero-debounce flush for at least one executor tick, so
        // `row` is `None` right after creation even though the object is already queued
        // to exist. Keying on `draft.is_new` alone (never also `row.is_none()`) is what
        // keeps the badge painted through that tick instead of flickering off the
        // moment the row derives.
        if draft.is_new {
            markers = markers.child(dialog::badge(
                "new",
                theme.primary,
                theme.primary,
                Some("objectdialog-new-badge".to_string()),
                cx,
            ));
        }
        header = header.child(markers);
    }
    // under the header, not on it — a badge says WHAT the row is, this says what to DO
    // about it, and only `r` (never `d`, which deletes the whole override rather than
    // restoring a shadow) does.
    let drift_note = row.as_ref().filter(|r| r.drifted).map(|_| {
        div()
            .text_xs()
            .text_color(theme.warning)
            .debug_selector(|| "objectdialog-drift-note".to_string())
            .child("the desk's copy has changed since you copied it — r restores it")
            .into_any_element()
    });

    let domain = state.domain;
    let writable = domain.writable(&state.stage);
    // Schema's rows name the layer their value came from. When any row does, every
    // row reserves the slot, so a row without a layer keeps its value in line.
    let layer_slots = draft.fields.iter().any(|field| field.layer.is_some());
    // whether any chip may carry a handler this frame. The four inert cases mirror the
    // keys' own: read-only domain, armed confirm, open text field (and per row, a
    // one-option `Choice`).
    let chips_live = writable && state.confirm.is_none() && draft.text_entry.is_none();
    let rows = draft.rows();
    let visible = draft.visible_rows();
    // The row under the cursor, resolved once from the two lists above
    // for everything the footer asks about it (its vocabulary, its help)
    // — `Draft::selected_row` would rebuild both per question.
    let selected_row = visible
        .get(draft.selected)
        .and_then(|m| rows.get(m.row).copied());
    // which rows a current diagnostic names, computed once per render rather than per
    // row — `Draft::flagged_rows` is a linear scan of the diagnostic list, and doing it
    // once here keeps the per-row work below to a single `Vec` lookup.
    let flagged = draft.flagged_rows(domain.doc());
    // the column stage's layers, resolved once per render. `None` off the stage — see
    // `provenance_slot`.
    let provenance_inputs = draft
        .column_ctx
        .as_ref()
        .map(dataset_columns::ProvenanceInputs::new);
    // Choice entry paints its ranked options instead of the object's field rows.
    let list: AnyElement = if let Some(choice) =
        draft.choice.as_ref().filter(|_| draft.choice_entry())
    {
        let entity_for_click = entity.clone();
        dialog::choice_rows(
            choice,
            "objectdialog",
            &shell.object_dialog_scroll,
            theme,
            move |row, window, cx| {
                entity_for_click.update(cx, |shell, cx| {
                    on_choice_row_clicked(shell, row, window, cx)
                });
            },
        )
    } else {
        let mut list = v_flex()
            .id("objectdialog-fields")
            .w(scale::design(WIDTH))
            .max_h(scale::design(VISIBLE_ROWS as f32 * ROW_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&shell.object_dialog_scroll)
            .debug_selector(|| "objectdialog-fields".to_string());

        // The last list row's `(field, is_own_item)` — the boundary a section
        // header marks. `None` so the very first list row always opens one.
        let mut last_item_section: Option<(usize, bool)> = None;

        for (position, m) in visible.iter().enumerate() {
            let Some(edit_row) = rows.get(m.row).copied() else {
                continue;
            };
            let is_selected = position == draft.selected;
            // Every row carries the same 2px top border, transparent unless
            // `drag_over` recolours it — reserving the space up front means a
            // hover only repaints the color, never reflows the rows below it.
            let element = h_flex()
                .id(("objectdialog-field-row", m.row))
                .w_full()
                .items_center()
                .justify_between()
                .gap_3()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .border_t_2()
                .border_color(gpui::transparent_black());
            let element = super::super::listrow::paint_row(element, row_paint, is_selected);
            // Set only for a list row that opens a new block — see this
            // loop's own comment on `last_item_section`.
            let mut section_header: Option<AnyElement> = None;
            let (selector, label, value) = match edit_row {
                EditRow::Field(index) => {
                    let field = &draft.fields[index];
                    // the value is a chip — the mouse form of `space`/`shift+space` —
                    // on exactly the rows the keys step (`vocabulary_of`, the footer's
                    // own answer), and plain text with no handler everywhere else,
                    // including every row while `chips_live` is off. The handler runs
                    // `on_value_chip_clicked`, whose one step path is
                    // `step_selected_row`'s.
                    let steps = matches!(
                        draft.vocabulary_of(Some(edit_row), state.domain),
                        RowVocabulary::Steps | RowVocabulary::StepsAndTypes
                    );
                    let on_step: Option<dialog::StepHandler> = (chips_live && steps).then(|| {
                        let entity = entity.clone();
                        Rc::new(move |forward: bool, window: &mut Window, cx: &mut App| {
                            entity.update(cx, |shell, cx| {
                                on_value_chip_clicked(shell, position, forward, window, cx);
                            });
                        }) as dialog::StepHandler
                    });
                    let value_chip = dialog::value_chip(
                        field_value(field),
                        format!("objectdialog-value-{}", field.key),
                        theme.muted_foreground,
                        theme.muted,
                        theme.radius,
                        chip_states,
                        on_step,
                    );
                    (
                        format!("objectdialog-field-{}", field.key),
                        highlighted_text(&field.label, &m.indices, row_paint.accent),
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(value_chip)
                            // the layer in force on a column-stage field, in place of
                            // the destination badge: every field there writes the same
                            // overlay, so the layer is the only per-row fact. Schema
                            // fills `layer` below on its own rows, which are not
                            // column-stage rows, so the two never both appear.
                            .children(provenance_inputs.as_ref().map(|inputs| {
                                badge_slot(
                                    &PROVENANCE_NAMES,
                                    dataset_columns::provenance_of(inputs, field).map(|p| {
                                        (
                                            p.name(),
                                            format!("objectdialog-field-provenance-{}", field.key),
                                        )
                                    }),
                                    theme,
                                    cx,
                                )
                            }))
                            // the layer a schema row's value came from — `None` on
                            // every writable domain (`Field:: layer`'s own doc has the
                            // reasoning).
                            .children(layer_slots.then(|| {
                                badge_slot(
                                    &LAYER_NAMES,
                                    field.layer.map(|layer| {
                                        (
                                            layer.name(),
                                            format!("objectdialog-field-layer-{}", field.key),
                                        )
                                    }),
                                    theme,
                                    cx,
                                )
                            }))
                            .into_any_element(),
                    )
                }
                // Resolve member and available rows through their distinct variants.
                // The catalogue is not part of the object's persisted member list.
                EditRow::Item { field, item } | EditRow::Available { field, item } => {
                    let own = matches!(edit_row, EditRow::Item { .. });
                    let FieldKind::OrderedList { items, available } = &draft.fields[field].kind
                    else {
                        continue;
                    };
                    let list = if own {
                        items.as_slice()
                    } else {
                        available.as_deref().unwrap_or_default()
                    };
                    let Some(entry) = list.get(item) else {
                        continue;
                    };
                    let section_key = (field, own);
                    if last_item_section != Some(section_key) {
                        last_item_section = Some(section_key);
                        // the Values stage installs its own list under the very same
                        // field key (`"values"`) the ordinary edit stage's `dimensions`
                        // list would carry, so `own` alone cannot tell the two apart —
                        // `section_header_text(Domain::Scopes, true)` would paint
                        // "DIMENSIONS" over a values list. `draft.values().is_some()`
                        // is the stage-aware override every other Values-stage site
                        // already reads by.
                        let (text, suffix) = if draft.values().is_some() {
                            (
                                "VALUES — `space` ticks · `ctrl+a` all shown · `ctrl+x` none",
                                "members",
                            )
                        } else {
                            section_header_text(domain, own)
                        };
                        let field_key = draft.fields[field].key.clone();
                        section_header = Some(
                            div()
                                .font_family(crate::fonts::MONO)
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .pt_2()
                                .pb_0p5()
                                .debug_selector(move || {
                                    format!("objectdialog-section-{suffix}-{field_key}")
                                })
                                .child(kbd::marked(text))
                                .into_any_element(),
                        );
                    }
                    // Member rows show inclusion ticks and, outside Scopes, reorder
                    // grips. Available rows reserve the grip width for alignment. Open
                    // value fields hide ticks and grips because their rows serve
                    // completion or text entry. Only the name is filter-highlighted;
                    // controls are not searchable text.
                    let draggable = domain != Domain::Scopes;
                    let grip_and_tick = if draft.text_entry.is_some() {
                        None
                    } else {
                        // A member row's grip is its drag handle. The row body opens
                        // a Views column's stage on mouse-down, which would leave the
                        // list before a drag could start, so the gesture starts here
                        // and the grip's press stops before the row's own handler.
                        // gpui bubbles an element's drag arming before its
                        // `on_mouse_down`, so stopping propagation there still arms
                        // the drag.
                        let payload = draggable.then(|| draft.row_drag(edit_row)).flatten();
                        let grip = if own && let Some(payload) = payload {
                            let grip_id = format!("objectdialog-grip-{}", entry.name);
                            div()
                                .id(gpui::SharedString::from(format!(
                                    "objectdialog-grip-{}-{}",
                                    payload.field, payload.name
                                )))
                                .text_color(theme.muted_foreground)
                                .w(scale::design(11.))
                                .cursor_grab()
                                .debug_selector(move || grip_id)
                                .on_drag(payload, |drag: &RowDrag, _offset, _window, cx| {
                                    DragGhost::build(drag, cx)
                                })
                                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                    cx.stop_propagation();
                                })
                                .child("⋮")
                                .into_any_element()
                        } else {
                            div().w(scale::design(11.)).into_any_element()
                        };
                        // the tick is the toggle. Its mouse-down stops propagation so
                        // the row's own select does not double-fire; the handler moves
                        // the cursor here itself and then walks `space`'s path, so the
                        // mouse and the key cannot disagree.
                        let entity_for_tick = entity.clone();
                        let tick_position = position;
                        let tick_id = format!("objectdialog-tick-{}", entry.name);
                        let tick = div()
                            .id(gpui::SharedString::from(tick_id.clone()))
                            .font_family(crate::fonts::MONO)
                            .w(scale::design(13.))
                            .text_center()
                            .rounded(theme.radius_tokens().sm)
                            .text_color(if entry.included {
                                theme.success
                            } else {
                                theme.muted_foreground
                            })
                            .pointer_states(if entry.included {
                                ticked_states
                            } else {
                                tick_states
                            })
                            .debug_selector(move || tick_id)
                            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                                cx.stop_propagation();
                                entity_for_tick.update(cx, |shell, cx| {
                                    on_tick_clicked(shell, tick_position, window, cx);
                                });
                            })
                            .child(if entry.included { "✓" } else { "·" })
                            .into_any_element();
                        Some((grip, tick))
                    };
                    let mut name_row = h_flex().pl_4().gap_1().items_center();
                    if let Some((grip, tick)) = grip_and_tick {
                        name_row = name_row.child(grip).child(tick);
                    }
                    if !entry.included {
                        name_row = name_row.text_color(theme.muted_foreground);
                    }
                    name_row =
                        name_row.child(highlighted_text(&entry.name, &m.indices, row_paint.accent));
                    // Show adapter notes when present, otherwise a member's column
                    // summary. Available candidates may carry dataset presentation but
                    // do not paint a column summary here. Notes remain separate from
                    // the searchable row label.
                    let note = entry.note.clone().or_else(|| {
                        own.then(|| {
                            views::column_summary(&views::kind_default(entry), &entry.presentation)
                        })
                        .filter(|s| !s.is_empty())
                    });
                    if let Some(note) = note {
                        name_row = name_row.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(note),
                        );
                    }
                    let name = name_row.into_any_element();
                    (
                        format!("objectdialog-item-{}", entry.name),
                        name,
                        div().into_any_element(),
                    )
                }
            };
            // a glyph before the label, painted only when a current diagnostic names
            // this row — an inert `div` of the same width otherwise, so every row keeps
            // its height and the label column stays aligned whether or not anything is
            // flagged. Built after the match above (rather than folded into each arm)
            // because the selector string — this row's identity for `debug_selector` —
            // is computed there, and one glyph rule for both arms is simpler than two
            // copies of the same match on `flag`.
            let flag = flagged
                .iter()
                .find(|(r, _)| *r == edit_row)
                .map(|(_, s)| *s);
            let glyph = match flag {
                Some(Severity::Error) => {
                    let diag_selector = format!("objectdialog-diag-{selector}");
                    div()
                        .w(scale::design(12.))
                        .text_color(theme.danger)
                        .debug_selector(move || diag_selector.clone())
                        .child("!")
                        .into_any_element()
                }
                Some(Severity::Warning) => {
                    let diag_selector = format!("objectdialog-diag-{selector}");
                    div()
                        .w(scale::design(12.))
                        .text_color(theme.warning)
                        .debug_selector(move || diag_selector.clone())
                        .child("!")
                        .into_any_element()
                }
                None => div().w(scale::design(12.)).into_any_element(),
            };
            let entity_for_row = entity.clone();
            let clicked = position;
            // while a field is open, the mouse agrees with the keys — a plain field
            // owns the row list too (moving the cursor under it would leave
            // `TextEntry.row` pointing at a row the trader is no longer on), so only
            // the chain field's own completion click does anything. `None` (no field
            // open at all) is the ordinary click-to-edit path.
            let open: Option<Completions> = draft.text_entry.map(|t| t.completions);
            // The glyph and label share ONE child so the row still has exactly two
            // children under `justify_between` — a third direct child splits the row's
            // free space into two gaps and floats the label toward the middle of the
            // row on every row of every domain, flagged or not. Carries its own
            // selector so a window test can compare a flagged row's label position
            // against an unflagged one's — there is otherwise no way to address just
            // the label, since the row's own selector spans the whole row (glyph, label
            // and value together) and would read the same width whichever child ate the
            // bug.
            let label_selector = format!("objectdialog-label-{selector}");
            let label_block = h_flex()
                .gap_1()
                .items_center()
                .debug_selector(move || label_selector.clone())
                .child(glyph)
                .child(label)
                .into_any_element();
            let row_el = element
                .child(label_block)
                .child(value)
                .debug_selector(move || selector.clone())
                .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    entity_for_row.update(cx, |shell, cx| match open {
                        Some(Completions::Chain) => {
                            on_completion_clicked(shell, clicked, window, cx)
                        }
                        Some(_) => {}
                        None => on_edit_row_clicked(shell, clicked, event.click_count, window, cx),
                    });
                });
            // Only list rows with no open value field carry drag handlers. Exclude
            // Scopes explicitly: its lists represent unordered selections even though
            // the shared draft shape can produce a drag payload. Erase the two branch
            // types because adding an ID changes the element's concrete type.
            let row_el = match (draft.text_entry.is_none() && domain != Domain::Scopes)
                .then(|| draft.row_drag(edit_row))
                .flatten()
            {
                Some(payload) => {
                    let entity_for_drop = entity.clone();
                    // `target` is THIS row — the drop's destination. The
                    // payload that arrives at `on_drop` is the dragged row's
                    // own, built by whichever row started the gesture.
                    let target = payload.clone();
                    // The id carries everything that identifies the row,
                    // because gpui keys per-element state (the pending
                    // mouse-down a drag starts from) on it: the field, so
                    // two lists in one draft cannot collide; `own`, so a
                    // name cannot collide with itself across the two
                    // blocks (a demoted column keeps its name); and the
                    // name, which is unique within a block.
                    let row_el = row_el.id(gpui::SharedString::from(format!(
                        "objectdialog-drag-{}-{}-{}",
                        payload.field, payload.own, payload.name
                    )));
                    // A member row drags from its grip (above); an Available
                    // row has no grip and opens nothing on a press, so its
                    // whole body stays the handle.
                    let row_el = if payload.own {
                        row_el
                    } else {
                        row_el
                            .cursor_grab()
                            .on_drag(payload, |drag: &RowDrag, _offset, _window, cx| {
                                DragGhost::build(drag, cx)
                            })
                    };
                    row_el
                        // Only this dialog's own payload: a tile drag or any
                        // other dragged value passing over the modal must not
                        // land on a column list.
                        .can_drop(|value, _window, _cx| value.downcast_ref::<RowDrag>().is_some())
                        // Only the color changes here — the 2px top border
                        // itself is reserved on every row unconditionally
                        // above, so a hover never reflows the rows below it.
                        .drag_over::<RowDrag>(move |style, _drag, _window, cx| {
                            style.border_color(cx.theme().primary)
                        })
                        .on_drop(move |dropped: &RowDrag, window, cx| {
                            let dropped = dropped.clone();
                            entity_for_drop.update(cx, |shell, cx| {
                                on_row_dropped(shell, &dropped, &target, window, cx);
                            });
                        })
                        .into_any_element()
                }
                None => row_el.into_any_element(),
            };
            // The header rides on the first item's own element so the list's
            // child count still equals its row count (`visible.len()`) —
            // `scroll_to_item` indexes children by that count, and a header
            // emitted as its own `list.child(header)` before the row would
            // make every following index off by one.
            list = list.child(match section_header {
                Some(header) => v_flex().child(header).child(row_el).into_any_element(),
                None => row_el,
            });
        }

        if visible.is_empty() {
            // Unlike browse, this can only ever be "no matches" — every draft
            // this scaffold builds has at least a `Dataset`/`Slot` field row,
            // so an empty `rows()` never happens here.
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .debug_selector(|| "objectdialog-empty".to_string())
                    .child("no matches"),
            );
        }
        list.into_any_element()
    };

    // Prefix a diagnostic with its resolved field label when possible, using the same
    // path lookup as row glyphs. Unmatched object-level diagnostics retain their
    // message without a prefix.
    let diagnostics = v_flex().w(scale::design(WIDTH)).gap_0p5().children(
        draft
            .diagnostics
            .iter()
            .map(|diagnostic| {
                let prefix = diagnostic
                    .path
                    .as_deref()
                    .and_then(|p| draft.row_for_path(domain.doc(), p))
                    .map(|row| format!("{}: ", draft.row_label(row)))
                    .unwrap_or_default();
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .debug_selector(|| "objectdialog-diagnostic".to_string())
                    .child(format!("{prefix}{}", diagnostic.message))
            })
            .collect::<Vec<_>>(),
    );

    // Derive stepping and typing hints from the selected row's capabilities.
    let vocabulary = draft.vocabulary_of(selected_row, state.domain);
    // Can `i` open a field on this row? Groupings is the exception the vocabulary
    // cannot answer for: there `i` opens the slot's whole chain rather than the
    // selected row's own value, so it is live on every row and that arm states it
    // unconditionally below.
    let types = matches!(
        vocabulary,
        RowVocabulary::StepsAndTypes | RowVocabulary::Types
    );
    // Show only stepping keys available in this mode and on this row. Filter mode
    // advertises Tab/Shift-Tab; Space and bare letters insert input text there. List
    // rows name the membership operation rather than a generic step.
    let change_hint = |filtering: bool| -> Option<Hint> {
        let word = match vocabulary {
            RowVocabulary::Steps | RowVocabulary::StepsAndTypes => "change",
            RowVocabulary::Item => "toggle",
            RowVocabulary::Available => "add",
            RowVocabulary::Inert | RowVocabulary::Types => return None,
        };
        let steps = matches!(
            vocabulary,
            RowVocabulary::Steps | RowVocabulary::StepsAndTypes
        );
        let keys: &[&'static str] = match (filtering, steps) {
            (true, true) => &["tab", "shift+tab"],
            (true, false) => &["tab"],
            (false, true) => &["space", "shift+space", "tab", "h", "l"],
            (false, false) => &["space"],
        };
        Some(Hint::new(HintRow::Edit, keys, word).selector("objectdialog-hint-change"))
    };
    // `/` filter and `escape` with its honest rung — the same rule browse's own footer
    // keeps: with a query still applied it clears the query, and only then goes back.
    let leave = |back: String| -> [Hint; 2] {
        [
            Hint::new(HintRow::Go, &["/"], "filter"),
            Hint::new(
                HintRow::Go,
                &["escape"],
                if draft.query.is_empty() {
                    back
                } else {
                    "clear the filter".to_string()
                },
            ),
        ]
    };
    let filter_motion = || {
        vec![
            Hint::prose(HintRow::Move, "type to filter"),
            Hint::new(HintRow::Move, &["up", "down"], "move"),
            Hint::new(HintRow::Move, &["ctrl+d", "ctrl+u"], "±5"),
            Hint::new(HintRow::Move, &["ctrl+f", "ctrl+b"], "±10"),
        ]
    };
    // Show only this stage's active verbs. Enter is advertised only when the row has a
    // nested stage to open.
    let opens_column = column_stage_target(shell).is_some();
    let open_column =
        || Hint::new(HintRow::Go, &["enter"], "open column").selector("objectdialog-hint-enter");
    let i_hint = |row: Option<EditRow>| -> Hint {
        Hint::new(HintRow::Edit, &["i"], i_hint_word(row, draft)).selector("objectdialog-hint-i")
    };
    let hints: Vec<Hint> = if state.confirm.is_some() {
        vec![
            Hint::prose(HintRow::Go, "this needs an answer first"),
            Hint::new(HintRow::Go, &["enter"], "go ahead"),
            Hint::new(HintRow::Go, &["escape"], "leave it alone"),
        ]
    } else if let Some(entry) = draft.text_entry {
        // a value field's own vocabulary — never filter mode's, even though the `Input`
        // is focused the same way, because `enter` means "apply this value" here rather
        // than "narrow the list". The chain field is `Completions::Chain` and
        // additionally has `tab` to complete a segment and the nav keys to move the
        // highlight; the choice field is `Completions::Choice`, with the same two
        // extras but its own words — `enter` PICKS the lit option rather than applying
        // typed text — and a plain field has neither.
        let mut hints = Vec::new();
        match entry.completions {
            Completions::Chain => {
                hints.push(Hint::prose(HintRow::Move, "type a chain · book / lhu"));
                hints.push(Hint::new(HintRow::Move, &["up", "down"], "move"));
                hints.push(Hint::new(HintRow::Go, &["tab"], "complete"));
                hints.push(Hint::new(HintRow::Go, &["enter"], "apply"));
            }
            Completions::Choice => {
                hints.push(Hint::prose(HintRow::Move, "type to narrow"));
                hints.push(Hint::new(HintRow::Move, &["up", "down"], "move"));
                hints.push(Hint::new(HintRow::Go, &["tab"], "complete"));
                hints.push(Hint::new(HintRow::Go, &["enter"], "choose"));
            }
            Completions::None => {
                hints.push(Hint::prose(HintRow::Move, "type a value"));
                hints.push(Hint::new(HintRow::Go, &["enter"], "apply"));
            }
        }
        hints.push(Hint::new(HintRow::Go, &["escape"], "cancel"));
        hints
    } else if state.mode == DialogMode::Filter {
        // Filter mode keeps navigation hints and Tab stepping. Space and bare letters
        // remain text input, so they are not advertised as edit commands here.
        let mut hints = filter_motion();
        if state.domain.writable(&state.stage) {
            hints.extend(change_hint(true));
        }
        // Use the same query-only exit labels as Browse.
        hints.push(Hint::new(HintRow::Go, &["enter"], "keep the filter"));
        hints.push(Hint::new(HintRow::Go, &["escape"], "discard the filter"));
        hints
    } else if !state.domain.writable(&state.stage) {
        // a read-only domain's normal-mode vocabulary is reading and filtering alone —
        // no `space`/`shift+space` to change a row, no `shift+j`/`shift+k` to reorder,
        // none of `d`/`r`/`x`/`n`/`o`, since every one of those is refused by the gate
        // above.
        let mut hints = vec![Hint::new(HintRow::Move, &["j", "k"], "move")];
        if opens_column {
            hints.push(open_column());
        }
        hints.extend(leave("back to the list".to_string()));
        hints
    } else if draft.column().is_some() {
        // Column fields have no member reorder or removal operations. Text fields
        // permit typing, booleans step, and numbers or multi-option choices do both.
        // Escape names the parent object stage rather than the browse list.
        let mut hints = vec![Hint::new(HintRow::Move, &["j", "k"], "move")];
        hints.extend(change_hint(false));
        if types {
            hints.push(i_hint(selected_row));
        }
        hints.extend(leave(format!("back to {}", draft.name)));
        hints
    } else {
        // Show reordering only for member rows outside Scopes. Show `x` only where
        // membership can be changed through a catalogue. Read-only fields and list
        // headers have no stepping hint.
        let reorders = vocabulary == RowVocabulary::Item && state.domain != Domain::Scopes;
        let mut hints = vec![Hint::new(HintRow::Move, &["j", "k"], "move")];
        hints.extend(change_hint(false));
        if reorders {
            hints.push(
                Hint::new(HintRow::Edit, &["shift+j", "shift+k"], "reorder")
                    .selector("objectdialog-hint-reorder"),
            );
            if state.domain == Domain::Views {
                hints.push(Hint::new(HintRow::Edit, &["x"], "remove"));
            }
        }
        // Groupings exposes its slot-jump and chain-entry verbs where they work. Other
        // domains advertise `i` only for a selected row that accepts entry.
        if state.domain == Domain::Groupings {
            hints.push(
                Hint::new(HintRow::Edit, &["i"], "type a chain").selector("objectdialog-hint-i"),
            );
            hints.push(Hint::range(HintRow::Go, "1", "9", "jump to slot"));
        } else if types {
            hints.push(i_hint(selected_row));
        }
        // the Values stage's own pair, advertised only while it is open; `enter`'s own
        // row opens it, advertised only while it is NOT — the two can never both paint,
        // since `values_stage_target` is `None` the instant the stage is.
        if state.domain == Domain::Scopes && draft.values().is_some() {
            hints.push(Hint::new(
                HintRow::Edit,
                &["ctrl+a", "ctrl+x"],
                "all shown / none",
            ));
        }
        if state.domain == Domain::Scopes
            && draft.values().is_none()
            && values_stage_target(shell).is_some()
        {
            hints.push(
                Hint::new(HintRow::Go, &["enter"], "open values")
                    .selector("objectdialog-hint-enter"),
            );
        }
        if opens_column {
            hints.push(open_column());
        }
        // The Values stage's own escape rung names the scope it returns
        // to, exactly as the column stage's `leave(format!("back to
        // {}", draft.name))` does above — `draft.name` is the scope, not
        // the dimension, since the crumb already narrows to the column.
        let back = if draft.values().is_some() {
            format!("back to {}", draft.name)
        } else {
            "back to the list".to_string()
        };
        hints.extend(leave(back));
        hints
    };
    let hint_line = dialog::hint_rows(&hints);
    // One fixed-height line shows the latest notice, otherwise selected-field help.
    // Keep it blank during confirmation, but retain grammar help during text entry.
    // Fixed line height and truncation prevent footer movement.
    let help = if state.confirm.is_some() {
        ""
    } else {
        selected_row
            .and_then(|row| draft.field_key_of(row))
            .map(|key| domain.help(&state.stage, key))
            .unwrap_or("")
    };
    let one_line = |el: Div| el.text_sm().line_height(rems(1.25)).min_h_5().truncate();
    // A notice can carry a newline — `regex::Error`'s Display is
    // multi-line and `check_batch_pattern` forwards it verbatim — and
    // gpui breaks a line on `\n` whatever the wrap mode, which would
    // grow the slot. Flattened here, at the one paint site, and only
    // when there is one to flatten: a notice stands for one keystroke,
    // so the rare allocation is not per-frame churn on the common path.
    let notice = state.notice.as_ref().map(|notice| {
        if notice.contains('\n') {
            notice.split_whitespace().collect::<Vec<_>>().join(" ")
        } else {
            notice.clone()
        }
    });
    let slot = match notice {
        Some(notice) => one_line(div())
            .text_color(theme.warning)
            .debug_selector(|| "objectdialog-notice".to_string())
            .child(notice),
        None => {
            let line = one_line(div()).text_color(theme.muted_foreground);
            if help.is_empty() {
                line
            } else {
                line.debug_selector(|| "objectdialog-help".to_string())
                    .child(help)
            }
        }
    };
    let footer = v_flex()
        .w(scale::design(WIDTH))
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(theme.border)
        .child(slot)
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(hint_line),
        );

    // The live `Input` renders only when it actually owns the keystrokes — see this
    // module's own "one switch" note, now also the edit stage's rule. `slash_filters:
    // true` for the same reason as browse's own call.
    let frozen_query = (state.mode == DialogMode::Normal).then_some(dialog::FrozenFilter {
        query: draft.query.as_str(),
        slash_filters: true,
        entity: entity.clone(),
    });
    // while a value field is open it takes the filter row's place — the same shared
    // `Input`, labelled for what its text now is, exactly as browse's naming stage
    // swaps in `name_row`. The chain field keeps its own slot-and-chain label; a plain
    // field names the object and the row it is editing.
    let filter = if let Some(entry) = draft.text_entry {
        let label = if entry.completions == Completions::Chain {
            format!("slot {} · chain", draft.name)
        } else {
            // Use the field label when the entry names a field; other row shapes use
            // the shared row label rather than assuming all entries are field rows.
            match entry.row {
                EditRow::Field(index) => {
                    format!("{} · {}", draft.name, draft.fields[index].label)
                }
                other => format!("{} · {}", draft.name, draft.row_label(other)),
            }
        };
        dialog::name_row(&shell.dialog_input, &label, cx)
    } else {
        dialog::filter_row(&shell.dialog_input, frozen_query, cx)
    };

    v_flex()
        .gap_2()
        .child(header)
        .children(drift_note)
        .child(diagnostics)
        .child(filter)
        .child(list)
        .child(action_block)
        .child(footer)
        .into_any_element()
}

/// The small-caps text and selector suffix for the section header that opens an ordered
/// list's own items or its available catalogue. Backtick-quoted runs are keys, painted
/// as `Kbd` chips by `kbd::marked`. `own` distinguishes Views' own columns
/// from the rest of its dataset's; Groupings' `dimensions` has no catalogue at all
/// (`groupings.rs`'s own module doc), so only the first arm there is ever reached.
fn section_header_text(domain: Domain, own: bool) -> (&'static str, &'static str) {
    match (domain, own) {
        (Domain::Views, true) => (
            "COLUMNS — `space` hides · `shift+j` / `shift+k` reorder · `x` removes",
            "members",
        ),
        (Domain::Views, false) => ("AVAILABLE — `space` adds", "available"),
        (Domain::Groupings, _) => (
            "DIMENSIONS — `space` includes · `shift+j` / `shift+k` reorder",
            "members",
        ),
        (Domain::Scopes, true) => ("DIMENSIONS — `enter` opens values · `x` drops", "members"),
        (Domain::Scopes, false) => ("AVAILABLE — `enter` picks values", "available"),
        // None of Schema, Sources or Colors has an `OrderedList` field
        // at all (`schema.rs`'s, `sources.rs`'s and `colours.rs`'s own
        // module docs — every field on any of the three is a plain
        // scalar), so this arm is unreachable for all three; kept only
        // to stay exhaustive as domains are added.
        (Domain::Schema | Domain::Sources | Domain::Colors, _) => ("", "members"),
    }
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
        FieldKind::OrderedList { items, .. } => {
            // `items` alone — the available catalogue is not the object's,
            // so it must not inflate this count or read as "hidden", and
            // that is now a matter of which list is read rather than of a
            // filter this could forget.
            let total = items.len();
            let hidden = items.iter().filter(|i| !i.included).count();
            match (total, hidden) {
                (1, 0) => "1 column".to_string(),
                (n, 0) => format!("{n} columns"),
                (n, h) => format!("{n} columns · {h} hidden"),
            }
        }
    }
}

/// The action bar: every live verb as a button showing its own letter.
///
/// Buttons, not rows and not bare keys. A key alone has no clickable target, and every
/// other verb in Geode's dialogs has one; an `outline` button is the mock's own
/// local-command-bar look, and the destructive ones are `danger` rather than merely
/// worded strongly.
fn action_bar(shell: &ShellView, entity: &Entity<ShellView>) -> AnyElement {
    let mut bar = h_flex()
        .w(scale::design(WIDTH))
        .gap_2()
        .items_center()
        // `min_h_6` matches a `.small()` `Button`'s own labelled height (`Size::Small
        // => h_6()` at the pinned rev) so this row holds its place on a row `actions()`
        // offers nothing for — an `h_flex` with zero children otherwise has zero height
        // of its own, and everything the footer paints below this bar shifted up by a
        // button's height on such a row.
        .min_h_6()
        .debug_selector(|| "objectdialog-actions".to_string());
    for action in actions(shell) {
        let ks = crate::keymap::parse_keystroke(action.key, Modifiers::NONE)
            .expect("action keys are hardcoded valid");
        let entity_for_action = entity.clone();
        let key = action.key;
        let selector = format!("objectdialog-action-{key}");
        let mut button = Button::new(gpui::SharedString::from(format!("objectdialog-{key}")))
            .small()
            .outline()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(kbd::chip(&ks))
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

/// Browse actions follow the same gates as keyboard input: create for writable domains
/// without a fixed roster, copy for eligible Scopes rows, and delete/revert from the
/// selected row's provenance. Hide them while naming. Each pointer action uses its
/// key's transition and then synchronizes input text and focus.
fn browse_action_bar(
    state: &ObjectDialogState,
    rows: &[ObjectRow],
    visible: &[crate::listfilter::Ranked],
    entity: &Entity<ShellView>,
) -> AnyElement {
    let naming = matches!(state.stage, Stage::Naming);
    let writable = state.domain.writable(&state.stage);
    let offers_n = !naming && writable && state.domain.roster().is_none();
    // Delete and revert use the same provenance gates as the edit action bar.
    let row = (!naming && writable)
        .then(|| selected_row(state, rows, visible))
        .flatten();
    let offers_d = row.is_some_and(|r| r.layer == Some(Layer::User));
    let offers_r = row.is_some_and(|r| r.overridden);
    // `c`: the mouse form of the `c` key, offered under the same three conditions the
    // key checks — not naming, the domain writable, and a row under the cursor to copy.
    let offers_c = state.domain.duplicable() && row.is_some();
    if !offers_n && !offers_c && !offers_d && !offers_r {
        return div().into_any_element();
    }
    let mut bar = h_flex().w(scale::design(WIDTH)).gap_2().items_center();
    if offers_n {
        let ks = crate::keymap::parse_keystroke("n", Modifiers::NONE).expect("valid");
        let entity = entity.clone();
        let label = format!("New {}", object_word(state.domain));
        bar = bar.child(
            div()
                .debug_selector(|| "objectdialog-action-n".to_string())
                .child(
                    Button::new("objectdialog-n")
                        .small()
                        .outline()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(kbd::chip(&ks))
                                .child(label),
                        )
                        .on_click(move |_event, window, cx| {
                            entity.update(cx, |shell, cx| {
                                if let Some(state) = shell.object_dialog.as_mut()
                                    && state.notice.take().is_some()
                                {
                                    cx.notify();
                                }
                                // Capture the selected source's dataset when the click
                                // occurs, not when this button was rendered; reload or
                                // navigation may have changed the selected row.
                                let seed = seed_dataset_under_cursor(shell);
                                let seed_taken = seed.as_deref().is_some_and(|d| {
                                    Domain::Sources.name_taken(&shell.services.config, d)
                                });
                                begin_new_object(shell, seed, seed_taken);
                                dialog::sync_dialog_text(shell, window, cx);
                                cx.notify();
                            });
                        }),
                ),
        );
    }
    if offers_c {
        let ks = crate::keymap::parse_keystroke("c", Modifiers::NONE).expect("valid");
        let entity = entity.clone();
        // Owned, to move into the closure: `row` borrows `rows`, which the
        // caller derived for this one frame and does not outlive it.
        let copy_name = row.map(|r| r.name.clone()).unwrap_or_default();
        bar = bar.child(
            div()
                .debug_selector(|| "objectdialog-action-c".to_string())
                .child(
                    Button::new("objectdialog-c")
                        .small()
                        .outline()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(kbd::chip(&ks))
                                .child("Copy this scope"),
                        )
                        .on_click(move |_event, window, cx| {
                            entity.update(cx, |shell, cx| {
                                if let Some(state) = shell.object_dialog.as_mut()
                                    && state.notice.take().is_some()
                                {
                                    cx.notify();
                                }
                                begin_copy(shell, copy_name.clone());
                                dialog::sync_dialog_text(shell, window, cx);
                                cx.notify();
                            });
                        }),
                ),
        );
    }
    // The two destructive buttons take `press_verb`, the edit bar's own
    // click door, which arms through `arm_delete`/`arm_revert` exactly
    // as the keys do.
    let destructive: [(&'static str, String, bool); 2] = [
        (
            "d",
            format!("Delete this {}", object_word(state.domain)),
            offers_d,
        ),
        ("r", "Revert to desk".to_string(), offers_r),
    ];
    for (key, label, offered) in destructive {
        if !offered {
            continue;
        }
        let ks = crate::keymap::parse_keystroke(key, Modifiers::NONE).expect("valid");
        let entity = entity.clone();
        let selector = format!("objectdialog-action-{key}");
        bar = bar.child(
            div().debug_selector(move || selector.clone()).child(
                Button::new(gpui::SharedString::from(format!("objectdialog-{key}")))
                    .small()
                    .outline()
                    .danger()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(kbd::chip(&ks))
                            .child(label),
                    )
                    .on_click(move |_event, window, cx| {
                        entity.update(cx, |shell, cx| {
                            press_verb(shell, key, window, cx);
                        });
                    }),
            ),
        );
    }
    bar.into_any_element()
}

/// The confirm block, which **replaces** the action bar rather than joining it —
/// `dialog::confirm_row` with this dialog's question, verb and handlers. The yes
/// handler's `run_confirmed` delete/revert arm walks all the way back to browse through
/// `leave_edit`, which is why the shared row syncs after it.
fn confirm_row(
    confirm: Confirm,
    name: &str,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    let yes_label = match confirm {
        Confirm::Delete => "Delete",
        Confirm::Revert => "Revert",
        Confirm::Overwrite => "Overwrite",
    };
    let on_yes: dialog::ConfirmHandler =
        Rc::new(|shell, _window, cx| answer_confirm(shell, true, cx));
    let on_no: dialog::ConfirmHandler =
        Rc::new(|shell, _window, cx| answer_confirm(shell, false, cx));
    let selector = format!("objectdialog-confirm-prompt-{name}");
    div()
        // Names the object the prompt is about, so a test can tell which
        // one the row was painted for — `dialog::confirm_row`'s own
        // selectors are per surface, not per object.
        .debug_selector(move || selector.clone())
        .child(dialog::confirm_row(
            confirm.prompt(name),
            yes_label,
            "objectdialog",
            entity,
            on_yes,
            on_no,
            cx,
        ))
        .into_any_element()
}

/// Every name a column-stage row's layer badge can carry.
const PROVENANCE_NAMES: [&str; 3] = [
    super::Provenance::Desk.name(),
    super::Provenance::Dataset.name(),
    super::Provenance::View.name(),
];

/// Every name a Schema row's layer badge can carry.
const LAYER_NAMES: [&str; 3] = [
    Layer::Builtin.name(),
    Layer::Desk.name(),
    Layer::User.name(),
];

/// A right-aligned badge in a slot as wide as the widest of `names`, laid out by an
/// invisible badge. A row with no badge, or one whose badge appears or changes
/// mid-edit, keeps its value in line with every other row's.
///
/// The column stage's caller builds its `ProvenanceInputs` once before the row loop:
/// both halves allocate, and a clone per painted field is per-frame heap churn.
fn badge_slot(
    names: &[&'static str],
    badge: Option<(&'static str, String)>,
    theme: &gpui_component::Theme,
    cx: &App,
) -> AnyElement {
    let widest = names
        .iter()
        .copied()
        .max_by_key(|name| name.len())
        .unwrap_or_default();
    let sizer = div().invisible().child(dialog::badge(
        widest,
        theme.muted_foreground,
        theme.border,
        None,
        cx,
    ));
    let badge = badge.map(|(name, selector)| {
        // Out of flow, the badge would otherwise wrap to the slot's width.
        div()
            .absolute()
            .top_0()
            .right_0()
            .whitespace_nowrap()
            .child(dialog::badge(
                name,
                theme.muted_foreground,
                theme.border,
                Some(selector),
                cx,
            ))
    });
    div()
        .relative()
        .flex_shrink_0()
        .child(sizer)
        .children(badge)
        .into_any_element()
}

/// Run the keyboard verb's handler for an action-button click, then synchronize shared
/// input state. This is required when `i` changes mode and focuses a field;
/// confirmation-only transitions leave unchanged input state alone.
fn press_verb(shell: &mut ShellView, key: &str, window: &mut Window, cx: &mut Context<ShellView>) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    // belt-and-braces — `actions()` already paints an empty bar on a read-only domain,
    // so this button is unreachable by the mouse in practice, but a test (or a future
    // caller) can still call this door directly, and it must refuse exactly as the
    // keyboard does.
    let writable = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.domain.writable(&state.stage));
    if !writable {
        set_notice(shell, READ_ONLY_NOTICE.to_string());
        cx.notify();
        return;
    }
    match key {
        "d" => arm_delete(shell),
        "r" => arm_revert(shell),
        "i" => open_field(shell),
        "o" => overwrite_scope(shell, cx),
        _ => {}
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Select the clicked draft row and open it when Enter would open a nested stage. Other
/// rows open their editable field on double-click. If the first click opened a stage,
/// consume its second click so it cannot activate an unrelated new row.
///
/// Use the same target resolver and stage transition as keyboard input. Synchronize
/// input text and focus after the mutation so filtering remains focused unless the
/// transition explicitly changed mode.
fn on_edit_row_clicked(
    shell: &mut ShellView,
    position: usize,
    click_count: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    // Ignore row clicks while a confirmation owns input, before moving selection.
    if armed_confirm(shell).is_some() {
        return;
    }
    let domain = shell.object_dialog.as_ref().map(|state| state.domain);
    if let Some(draft) = draft_mut(shell) {
        // `position` is the FILTERED index `build_edit` painted this row at, so the
        // bound to check — and the value to store, unchanged — is against
        // `visible_rows`, not the unfiltered `rows`.
        if position >= draft.visible_rows().len() {
            return;
        }
        // Ignore non-stop rows without moving selection or opening a stage. This also
        // applies when a filter leaves no stops and keyboard motion can traverse inert
        // rows.
        let rows = draft.rows();
        let clicked = draft
            .visible_rows()
            .get(position)
            .and_then(|m| rows.get(m.row).copied());
        match (domain, clicked) {
            (Some(domain), Some(row)) if !draft.is_cursor_stop(domain, row) => return,
            _ => {}
        }
        draft.selected = position;
    }
    shell.object_dialog_scroll.scroll_to_item(position);
    // A fresh click sequence forgets what the last one opened — see
    // `ObjectDialogState::click_opened_stage`.
    if click_count <= 1
        && let Some(state) = shell.object_dialog.as_mut()
    {
        state.click_opened_stage = false;
    }
    // After the cursor has moved, never before: the target is the row
    // that was just clicked, which is what `enter` would be acting on had
    // the trader pressed it instead.
    if let Some(name) = column_stage_target(shell) {
        enter_column_stage(shell, &name, cx);
        if let Some(state) = shell.object_dialog.as_mut() {
            state.click_opened_stage = true;
        }
    } else if let Some(column) = values_stage_target(shell) {
        // a dimension row is a door row exactly as a member column is — the click opens
        // the Values stage and nothing more, so the same guard keeps the pair's second
        // click from also opening a field there.
        enter_values_stage(shell, &column, cx);
        if let Some(state) = shell.object_dialog.as_mut() {
            state.click_opened_stage = true;
        }
    } else if click_count == 2
        && !shell
            .object_dialog
            .as_ref()
            .is_some_and(|state| state.click_opened_stage)
    {
        // A double-click on a value row is `i`: the first mouse-down selected the row
        // above, and this second one opens its field through the one door the key and
        // the action-bar button share — so a `Choice` row's typeahead, a
        // `Number`/`Text` row's field, Groupings' chain, or `i`'s own notice on a row
        // that has none. Gated on `writable` exactly as `press_verb` and the key path
        // are.
        let writable = shell
            .object_dialog
            .as_ref()
            .is_some_and(|state| state.domain.writable(&state.stage));
        if writable {
            open_field(shell);
        } else {
            set_notice(shell, READ_ONLY_NOTICE.to_string());
        }
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// The value chip's click: move the cursor to the row, then exactly the key's path —
/// [`step_selected_row`] with `filtering` from the mode, so the "nothing changes with
/// …" notice names the right key. Claimed and dropped while a confirm is armed or a
/// text field is open (the chip paints without a handler then, but a test can still
/// call this door). The read-only gate is `step_selected_row`'s callers' — applied here
/// too, since this is one.
///
/// `pub(in crate::shell)` so the window tests can drive it directly: no
/// Schema row ever paints a chip (every row there is a display-only
/// `Text`, so `vocabulary_of` answers `Inert`), which leaves this door's
/// own read-only refusal reachable only by a direct call.
pub(in crate::shell) fn on_value_chip_clicked(
    shell: &mut ShellView,
    position: usize,
    forward: bool,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    if state.confirm.is_some() || draft.text_entry.is_some() {
        return;
    }
    if position >= draft.visible_rows().len() {
        return;
    }
    let filtering = state.mode == DialogMode::Filter;
    let writable = state.domain.writable(&state.stage);
    if let Some(draft) = draft_mut(shell) {
        draft.selected = position;
    }
    shell.object_dialog_scroll.scroll_to_item(position);
    if writable {
        step_selected_row(shell, forward, filtering, cx);
    } else {
        set_notice(shell, READ_ONLY_NOTICE.to_string());
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Select the tick's row and use the shared stepping path, including Scopes'
/// Values-stage entry for an available dimension. Synchronize text and focus after
/// mutation. While confirmation is armed, ignore the click before moving selection or
/// changing any value; the visible rows cannot bypass the pending question.
fn on_tick_clicked(
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
    // the tick is `space`'s exact mouse path (this function's own doc comment) — a
    // read-only domain refuses it the same way the key does, in the same words.
    let writable = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.domain.writable(&state.stage));
    if !writable {
        set_notice(shell, READ_ONLY_NOTICE.to_string());
        cx.notify();
        return;
    }
    if armed_confirm(shell).is_some() {
        return;
    }
    let Some(draft) = draft_mut(shell) else {
        return;
    };
    if position >= draft.visible_rows().len() {
        return;
    }
    draft.selected = position;
    step_selected_row(shell, true, false, cx);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Clicking a chain completion selects it and completes its text as Tab does. Keep
/// Filter mode and synchronize the focused input. A failed completion leaves selection
/// on the clicked row. Completion and confirmation cannot coexist: opening a field
/// hides the action bar, and confirmation blocks field entry.
fn on_completion_clicked(
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
    let completed = draft_mut(shell).is_some_and(|draft| {
        if position >= draft.visible_rows().len() {
            return false;
        }
        draft.selected = position;
        draft.complete_chain()
    });
    if completed {
        shell.object_dialog_scroll.scroll_to_item(0);
    } else {
        set_notice(shell, "nothing to complete here".to_string());
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// A choice-row click completes that option as Tab does, then synchronizes input.
fn on_choice_row_clicked(
    shell: &mut ShellView,
    row: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(draft) = draft_mut(shell) {
        draft.choice_click(row);
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// The ghost gpui paints under the cursor during a row drag: the dragged name in the
/// row's own type, on the popover surface so it reads as lifted off the list rather
/// than as one more row of it.
///
/// An entity of its own because that is the shape `on_drag`'s
/// constructor has to return; it holds the name alone, since a drag is
/// over in a second and nothing about the row that started it can change
/// underneath a ghost that is already painted.
struct DragGhost {
    name: gpui::SharedString,
}

impl DragGhost {
    /// The ghost's name comes off the dragged value the constructor is
    /// handed, not a captured copy: a capture would clone a `String` per
    /// list row per frame for a ghost that exists only once a gesture
    /// actually starts.
    fn build(drag: &RowDrag, cx: &mut App) -> Entity<Self> {
        let name = gpui::SharedString::from(drag.name.clone());
        cx.new(|_| DragGhost { name })
    }
}

impl gpui::Render for DragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .child(self.name.clone())
    }
}

/// Resolve drag payloads by name because keyboard edits can move or remove rows while a
/// drag is in progress. Changed drops follow the moved item, revalidate, scroll, and
/// commit through the normal batch path.
///
/// Self-drops stay silent. Explain catalogue-to-catalogue drops and names removed
/// mid-drag rather than making those refusals look like successful changes. Ignore all
/// drops while confirmation is armed, before even moving the cursor. The crate-visible
/// handler also permits tests to exercise the production drop route without native drag
/// machinery.
pub(in crate::shell) fn on_row_dropped(
    shell: &mut ShellView,
    src: &RowDrag,
    dst: &RowDrag,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    // a drop is a reorder or a promotion/demotion — a write, same as the tick — so a
    // read-only domain refuses it identically.
    let writable = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.domain.writable(&state.stage));
    if !writable {
        set_notice(shell, READ_ONLY_NOTICE.to_string());
        cx.notify();
        return;
    }
    if armed_confirm(shell).is_some() {
        return;
    }
    let Some(draft) = draft_mut(shell) else {
        return;
    };
    let resolves = draft.locate(src).is_some() && draft.locate(dst).is_some();
    match draft.drop_row(src, dst) {
        Step::Changed => {
            revalidate(shell);
            scroll_to_cursor(shell);
            commit_change(shell, cx);
        }
        Step::Refused(reason) => set_notice(shell, reason),
        // A row dropped back on itself is a grab that went nowhere, and
        // it is silent from EITHER block: said first, ahead of the
        // catalogue arm, because two identical available payloads
        // satisfy that arm's test too and "the catalogue has no order"
        // is no answer to a trader who simply put a row back down.
        Step::Inert if src == dst => {}
        Step::Inert if !src.own && !dst.own => {
            set_notice(shell, "the catalogue has no order".to_string());
        }
        Step::Inert if !resolves => set_notice(shell, "that row is gone".to_string()),
        Step::Inert => {}
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// A `DistinctOutcome` addressed to `SCOPES_KEY`, routed here by
/// `ShellView::deliver_distinct`. Applied only when a Scopes dialog is
/// open in the Values stage for `outcome.column` and the tag is the
/// latest one handed out — the picker's own three guards, so a reply to
/// a stage the trader has already left, or to a superseded request,
/// changes nothing. `Ok` installs the ticked list as a CLEAN baseline
/// (delivered ticks are the saved scope, not dirt); `Err` installs the
/// failure row.
pub(in crate::shell) fn deliver_values(
    shell: &mut ShellView,
    outcome: DistinctOutcome,
    cx: &mut Context<ShellView>,
) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    if state.domain != Domain::Scopes {
        return;
    }
    let Stage::Values { column, .. } = &state.stage else {
        return;
    };
    if *column != outcome.column || outcome.tag != state.values_tag {
        return;
    }
    let Some(draft) = state.draft.as_mut() else {
        return;
    };
    let saved: Vec<String> = draft
        .source
        .get("dimensions")
        .and_then(|v| v.as_table())
        .and_then(|d| d.get(&outcome.column))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let fields = match &outcome.values {
        Ok(values) => scopes::values_fields(&saved, values),
        Err(message) => scopes::failed_field(message),
    };
    draft.reseed_fields(fields);
    draft.selected = 0;
    // Delivery replaces the loading row outside keyboard handling. Settle onto the
    // first available stop, skipping the Values header when there are value rows.
    draft.settle_selection(Domain::Scopes);
    // Scroll to the settled cursor, which can differ from the initial index zero.
    let selected = draft.selected;
    shell.object_dialog_scroll.scroll_to_item(selected);
    cx.notify();
}
