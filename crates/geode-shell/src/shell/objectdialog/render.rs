//! The object dialog's gpui half: opening it, the one
//! [`dialog::ModalKeyHandler`] every stage comes through, and the
//! painted browse list, field rows and action bar.
//!
//! Everything here is the shell around [`super`]'s pure core, and it is
//! deliberately the same shell `keybindings_view` grew — same door
//! ([`dialog::open_shell_dialog_with_key`]), same two-mode routing, same
//! shared filter row, same muted footer stating only the *current*
//! mode's vocabulary, and the same mode pill — which since §18.1 is not
//! a row either dialog paints at all, but the modal title row's own
//! `title_extra` slot, filled here in [`open`] via
//! [`dialog::set_title_extra`]. A second routing shape would be a second
//! set of edge cases (which key blurs the filter, which escape rung
//! closes the modal) for a user to learn twice and a maintainer to fix
//! twice.
//!
//! ## The one switch: normal mode is a blurred filter
//!
//! This dialog opens in [`DialogMode::Normal`] with
//! `ShellView::dialog_input` **blurred**, because a focused
//! gpui-component `Input` consumes bare letters as text before any raw
//! key listener sees them — which is the whole reason `j`/`k` can move
//! here at all, and the reason the edit stage's `d`/`r` verbs are
//! reachable. `/` enters [`DialogMode::Filter`] and the field takes the
//! keys; `escape` goes back to `Normal` and it gives them up. While the
//! field is blurred the query paints as static muted text rather than a
//! live caret ([`dialog::filter_row`]'s `frozen` argument): a caret in a
//! field that is not receiving the keys is the single most misleading
//! thing a modal surface can show.
//!
//! **No site in this file moves focus or writes that field** (spec
//! §16.1). Every transition here — `/`, `escape`'s rungs, `n`, opening
//! and leaving the edit stage — is a pure mutation of
//! [`ObjectDialogState`]'s `mode`, `stage` and query, and
//! [`dialog::sync_dialog_text`] reconciles gpui to it afterwards: focus
//! goes where [`dialogmode::focus_target`] says, and the shared `Input`
//! is written from [`ObjectDialogState::effective_query`] — the open
//! stage's own query, so the browse list's and the draft's can never be
//! painted into each other's stage. It runs at the tail of the modal
//! branch in `ShellView::handle_key_down` (claimed or not), at the end of
//! each of this file's two row-click handlers and both confirm-button
//! closures — the paths that never reach the key handler at all — and
//! inside [`dialog::open_shell_dialog_with_key`], which is where this
//! dialog's opening blur comes from (hence `focus_filter: false` in
//! [`open`]: that parameter is for a dialog with no mode). Before that
//! one owner, each transition hand-wrote its own empty-string
//! `set_value` and focus call beside the mutation, and a site that had
//! one and not the other was a mode and a focus disagreeing — the
//! defect class this section names.
//!
//! ## Three stages, one routing shape
//!
//! `enter` on a browse row opens the **edit stage** over a [`super::Draft`]
//! of that object; `n` opens the **naming stage** (§18.2) over a fresh
//! name, on its way to becoming a draft of its own. All three stages
//! share this file's one [`dialog::ModalKeyHandler`] and split at its
//! front door ([`handle_key`]), because everything below the split — the
//! notice's doors, the `escape` ladder, the claim-and-drop contract — has
//! to behave identically across them or a user learns three dialogs.
//!
//! The edit stage filters its own rows too (§18.3), through
//! [`super::Draft::query`] — a second cursor space from the browse
//! stage's own `state.query`, since [`super::Draft::selected`] already
//! indexes [`super::Draft::rows`] rather than [`super::ObjectRow`]s.
//! `shift+j`/`shift+k` resolve the ambiguity a naive reorder-under-a-
//! filter would have (moving an item past a neighbour the filter is
//! hiding) by not treating it as ambiguous at all: [`super::Draft::
//! move_item`] walks the *unfiltered* list for the next VISIBLE neighbour
//! in the object's own list, so a filtered reorder still moves
//! something and reports how many hidden rows it jumped. Entering the
//! stage still drops the BROWSE query (`enter_edit`/`enter_edit_with`
//! clear both `state.query` and the freshly-built draft's own, separate
//! `query`) — the two lists are unrelated, and a browse filter left
//! applied here would rank rows the trader never typed anything to
//! filter. At the moment the stage opens that also keeps the `escape`
//! ladder honest, reaching [`EscapeStep::PreviousStage`] directly rather
//! than spending itself on `ClearQuery` first — though once a trader
//! types into the edit stage's own filter, `ClearQuery` becomes a real,
//! reachable rung again, ahead of `PreviousStage`, exactly as it is in
//! the browse stage.
//!
//! ## The dialog is instant; the merge and the applier ride one timer
//!
//! There is no save key. A keystroke changes the [`super::Draft`] and
//! puts the change on a pending batch ([`super::apply::commit_edit`]);
//! 250 ms later that batch is merged through the loader's own
//! `Config::from_docs`, applied through `hot_reload::apply_reload` — the
//! same applier the 500 ms watcher uses — and written to the file, all
//! together (spec §7.1, and [`super::apply`]'s module doc for the
//! measurements, the self-write reasoning and the failed-write revert).
//!
//! **The edit stage's rows are painted from the draft, and that is what
//! makes the edit instant.** It is not a cache and it must not be
//! "fixed" into reading `services.config`: that config is deliberately up
//! to one debounce behind, so a row derived from it would show the
//! trader's own keystroke a quarter of a second late — the exact lag this
//! design exists to remove. The draft is not an opinion the config might
//! contradict either; it is the same value the flush is about to merge,
//! rendered by the same [`Domain::to_table`](super::Domain::to_table)
//! call, and a failed write rebuilds it from the reverted config
//! (`apply::revert_failed_write`) so the two cannot drift apart.
//!
//! The **browse** stage still derives fresh from `Config`, because it has
//! no draft to derive from and nothing it shows is one keystroke old. Its
//! rows can therefore trail an edit by up to one debounce window; they
//! self-correct on the flush.
//!
//! One edit still asks first, and only one: a change that would **fork**
//! the object into the user layer (spec §4.1), because a fork freezes the
//! desk's copy out. That is [`Confirm::Fork`], and it is why the action
//! bar's remaining verbs are exactly the destructive and structural
//! ones.

use std::rc::Rc;

use geode_core::config::{Layer, check_object_name};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, MouseButton, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};

use super::apply;
use super::scopes;
use super::views;
use super::{
    Confirm, Destination, Domain, Draft, EditRow, FieldKind, ObjectDialogState, ObjectRow, Stage,
    Step,
};
use crate::dialogmode::{self, DialogMode, EscapeStep, NormalCommand};
use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;
use crate::vimnav;

use super::super::ShellView;
use super::super::dialog;
use super::super::keybindings_view::{highlighted_text, key_chip, split_label_indices};

/// Row height estimate (two lines: name plus muted summary) for the
/// browse list's viewport — non-load-bearing for scroll-FOLLOW, since
/// that goes through `ScrollHandle::scroll_to_item`, which measures real
/// layout, the same as `keybindings_view::ROW_HEIGHT`. It IS load-bearing
/// for the browse list's own `.h(..)`, which still sums
/// `visible.len() * ROW_HEIGHT` to size the container exactly — safe
/// there only because a browse row is never folded together with a
/// section header the way an edit-stage item row is (see below).
///
/// The edit stage's list learned that the hard way: it used to size
/// itself the same summing way with a one-line `FIELD_ROW_HEIGHT`
/// estimate (28px), but since spec §18.1 a block's first item row has a
/// section header folded into its own element, so the sum silently
/// undercounted by one header's height per painted block and clipped
/// the last row whenever the list was short enough not to hit the cap
/// below. The edit list now sizes itself to its real content
/// (`max_h` instead of a computed `h`), so `ROW_HEIGHT` there is only
/// the cap's unit — see `build_edit` — and no longer needs to be an
/// exact per-row estimate at all.
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
        // `false`: `focus_filter` is for a dialog with no mode, and this
        // one has one. Its initial focus comes from that door's own
        // `dialog::sync_dialog_text` call (spec §16.1) — which is why
        // `view.object_dialog` is set *above*, before the door runs: the
        // sync reads the `DialogMode::Normal` this dialog opens in and
        // parks the keys on the shell root, so a bare letter reaches
        // [`handle_key`] as a motion or as one of the edit stage's verbs
        // rather than being eaten as text by a focused filter.
        false,
    );
    // §18.1: the crumb plus the pill, sharing the same title-row slot
    // every Geode modal has — see `crumb_text`'s own doc for what the
    // crumb says in each stage.
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
            .children(state.map(|s| dialog::mode_pill(s.mode, cx)))
            .into_any_element()
    });
}

/// The title-row crumb (§18.1): a count in browse and naming, the slot's
/// chord in a Groupings edit, nothing otherwise. Pure so a test can read
/// it without laying out a window.
pub(crate) fn crumb_text(shell: &ShellView) -> String {
    let Some(state) = shell.object_dialog.as_ref() else {
        return String::new();
    };
    match &state.stage {
        Stage::Edit { object } if state.domain == Domain::Groupings => format!("ctrl+{object}"),
        Stage::Edit { .. } => String::new(),
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
        Some(Stage::Edit { .. }) => handle_edit_key(shell, ks, cx),
        Some(Stage::Naming) => handle_naming_key(shell, ks, cx),
        _ => handle_browse_key(shell, ks, cx),
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
fn handle_browse_key(shell: &mut ShellView, ks: &Keystroke, cx: &mut Context<ShellView>) -> bool {
    let rows = derive_rows(shell);
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
                    // Clearing the pure query is the whole rung:
                    // `dialog::sync_dialog_text` empties the shared
                    // `Input` from it on this handler's return (spec
                    // §16.1), so the old query cannot be left waiting in
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
                // again.
                state.mode = DialogMode::Filter;
            }
            NormalCommand::Commit => {
                open_selected(shell, cx);
                return true;
            }
            // `n`: the naming stage (§18.2), unless the domain's own
            // roster already names every row there is — Groupings' nine
            // fixed slots, where nothing can be created that is not
            // already on the list (`Domain::roster`'s own doc).
            NormalCommand::Verb('n') => {
                if state.domain.roster().is_some() {
                    state.notice = Some("the slots are fixed — open one to fill it".to_string());
                } else {
                    // `begin_naming` is the whole transition: it clears
                    // `query` and sets `DialogMode::Filter`, and
                    // `dialog::sync_dialog_text` empties the shared
                    // `Input` and focuses it to match on this handler's
                    // return. That clear is what keeps a stale browse
                    // filter (typed, then `escape`'d back to normal mode
                    // without clearing it) out of the name field — the
                    // sync writes the field from `effective_query`, so
                    // whatever `query` still held would otherwise be
                    // written straight back into it.
                    state.begin_naming();
                }
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

    if ks.key == "escape" {
        // The ladder's first rung, which must be claimed (`true`):
        // falling through would close the whole dialog on the escape that
        // was only meant to leave the search. The query stays applied;
        // the blur `dialog::sync_dialog_text` performs on this handler's
        // return is what makes the letters motions again.
        state.mode = DialogMode::Normal;
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
        open_selected(shell, cx);
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

/// The naming stage's keys (§18.2): `escape` backs out to browse with
/// nothing written; `enter` checks the name and creates; everything
/// else is the focused `Input`'s to type. The name is `state.query` —
/// mirrored from the field by the same subscription a filter uses.
fn handle_naming_key(shell: &mut ShellView, ks: &Keystroke, cx: &mut Context<ShellView>) -> bool {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    if ks.key == "escape" {
        // `cancel_naming` is the whole transition — `Stage::Browse`,
        // `DialogMode::Normal`, an empty `query` — and
        // `dialog::sync_dialog_text` empties the field and blurs it to
        // match on this handler's return (spec §16.1).
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

/// `enter` in the naming row. The one moment a name is typed and the
/// one place it is checked: [`check_object_name`]'s rule (the same one
/// `:scope save` applies), then "nothing already holds it"
/// ([`Domain::name_taken`], which spans the presentation overlay as well
/// as every layer of the domain's own doc) — creating over a desk object
/// would be a fork the trader did not ask for, and creating over an
/// orphaned overlay entry would silently inherit it.
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
        // Two ways a name can be taken, and they need different
        // instructions. A name with a row is one `escape` and an `enter`
        // away; a name only the presentation overlay holds
        // (`Domain::name_taken`'s own doc) has nothing on this list to
        // open at all, so pointing the trader at the list would be a
        // dead end — the orphaned `view_presentation.toml` entry is the
        // thing in their way, and it is the thing the notice names.
        let listed = derive_rows(shell).iter().any(|row| row.name == name);
        let notice = if listed {
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
    if domain == Domain::Scopes {
        // The frame's current scope IS the new scope (§18.2) — the same
        // read `run_confirmed`'s `Confirm::Overwrite` arm makes, for the
        // same reason it is made here and not in the pure core.
        let scope = shell.frame.read(cx).scope().clone();
        if scope.is_empty() {
            set_notice(
                shell,
                "the frame's scope is empty — nothing to save".to_string(),
            );
            cx.notify();
            return;
        }
        scopes::overwrite_with(&mut draft, &scope);
        draft.diagnostics = domain.validate(&draft, &shell.services.config);
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

/// Selection logic for a real mouse click on the row for `clicked`
/// (resolved back to a position in the *filtered* list against freshly
/// derived rows). It moves focus the same way [`handle_key`] does — to
/// whichever surface the current mode owns — through
/// [`dialog::sync_dialog_text`] (spec §16.1), which a mouse handler
/// needs of its own because a click never passes through the key path at
/// all. Focusing the filter unconditionally here
/// would let a mouse click silently defeat normal mode, and the next
/// keystroke would type instead of act.
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
    let visible = super::visible_rows(state, &rows);
    let Some(ix) = super::filtered_position(&visible, &rows, clicked) else {
        return;
    };
    state.selected = ix;
    shell.object_dialog_scroll.scroll_to_item(ix);
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

/// **The one door into the edit stage.** Every way in goes through here —
/// `enter` from either browse mode, and (§18.2) `n`'s committed name.
///
/// It is a **pure mutation** (spec §16.1): it sets the mode and drops the
/// query, and nothing here touches gpui focus or the shared `Input`'s
/// text — [`dialog::sync_dialog_text`] reconciles both to the new state,
/// on the return of whichever key handler or click reached this. That
/// pairing used to be hand-written at each of this file's transitions,
/// and one site missing its half is not a cosmetic slip — it is the
/// defect that shipped and was fixed as
/// `an_object_opened_from_filter_mode_still_escapes_back_a_stage`: a
/// stage whose mode and focus disagree sends the next `escape` down a
/// rung the edit handler does not claim, and the shell closes the whole
/// dialog out from under the object being edited instead of stepping
/// back to the list. One owner is what makes that unreachable.
///
/// `new` is `None` for [`open_selected`]'s existing object — the pure
/// half goes through [`ObjectDialogState::enter_edit`], which derives the
/// draft from `config` — and `Some(draft)` for [`create_from_name`]'s
/// freshly named one, which goes through
/// [`ObjectDialogState::enter_edit_with`] instead so the just-built draft
/// (Scopes' already carries the frame's scope) is what the stage opens
/// on, rather than a fresh derivation from a config that has not been
/// written to yet.
///
/// So the two ways in are not offered separately: both pure-core methods
/// are visible only inside this module's subtree and their docs point
/// here, and this is the only function in that subtree that calls either.
/// A new call site gets the whole transition or none of it.
fn enter_edit_stage(
    shell: &mut ShellView,
    name: &str,
    new: Option<Draft>,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.object_dialog.as_mut() {
        match new {
            Some(draft) => state.enter_edit_with(draft),
            None => state.enter_edit(&shell.services.config, name),
        }
    }
    shell.object_dialog_scroll.scroll_to_item(0);
    cx.notify();
}

/// The edit stage's keys, in the one order they can be read in:
///
/// 1. an armed [`Confirm`] owns **every** keystroke until it is answered
///    (`enter`/`y`) or cancelled (`escape`/`n`). It replaces the action
///    bar rather than adding a row, so nothing above it moves;
/// 2. in [`DialogMode::Filter`] (§18.3, entered by `/` the same as
///    browse) `escape` leaves filter mode keeping the query, `enter`
///    gives the same notice normal mode's `Commit` does (there is
///    nothing here to open), navigation goes through
///    [`listfilter::nav_command`] against [`Draft::visible_rows`], and
///    `tab`/`shift+tab` are claimed and dropped — everything else is
///    unclaimed (`false`), reaching the focused `Input`;
/// 3. in [`DialogMode::Normal`], `escape` walks the ladder, whose
///    `PreviousStage` rung this stage exists to reach — going back a
///    stage, with nothing to discard because every edit already applied,
///    though a non-empty `Draft::query` takes the `ClearQuery` rung
///    first, same as browse;
/// 4. everything else goes through [`dialogmode::normal_command`], and a
///    key it does not claim is swallowed, exactly as in browse.
///
/// Every branch that changes a field's *value* ends in [`revalidate`]:
/// validation is a parse of a few hundred bytes (spec §7.2), so it runs
/// synchronously on every change with no debounce, and the diagnostics on
/// screen are never one keystroke behind the value they describe.
///
/// [`NormalCommand::MoveItem`] is the one deliberate exception — it
/// commits without revalidating — and it is safe because none of the three
/// `Domain::validate` implementations is order-sensitive: each renders the
/// object and hands it to its own loader (`ViewSpec::from_doc`,
/// `GroupingSlots::from_doc`, `saved_scopes_from_doc`), none of which has a
/// diagnostic a reorder can produce or resolve. That is a property of
/// today's validators rather than of the dispatch table, so a future
/// order-sensitive one has to add the call; the branch is also the only way
/// a test can reach the commit gate with an injected diagnostic still
/// standing (`an_edit_the_reader_rejects_does_not_join_the_batch` depends
/// on exactly that).
fn handle_edit_key(shell: &mut ShellView, ks: &Keystroke, cx: &mut Context<ShellView>) -> bool {
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
            run_confirmed(shell, confirm, cx);
        } else if ks.key == "escape" || (bare && ks.key == "n") {
            cancel_confirm(shell);
        }
        // Anything else is claimed and dropped: while a destructive
        // question is on screen, a stray letter must not act on the
        // object behind it.
        cx.notify();
        return true;
    }

    // ---- Filter mode (§18.3) ------------------------------------------
    //
    // The one switch, exactly as browse's own: while the shared `Input`
    // holds focus, bare letters are text, so this branch claims only the
    // handful of keys that input does not consume first.
    let filtering = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.mode == DialogMode::Filter);
    if filtering {
        if ks.key == "escape" {
            // `LeaveFilter`: back to normal, keeping the query applied —
            // leaving a search leaves you on the match. The blur that
            // makes the letters verbs again is
            // `dialog::sync_dialog_text`'s, on this handler's return.
            if let Some(state) = shell.object_dialog.as_mut() {
                state.mode = DialogMode::Normal;
            }
            cx.notify();
            return true;
        }
        if ks.mods == Modifiers::NONE && ks.key == "enter" {
            // Nothing here to open — the same notice normal mode's
            // `Commit` gives, so `enter` says the same thing in either
            // mode.
            edit_commit_notice(shell);
            cx.notify();
            return true;
        }
        if let Some(cmd) = listfilter::nav_command(ks) {
            let selected = shell.object_dialog.as_mut().and_then(|state| {
                let draft = state.draft.as_mut()?;
                draft.selected = vimnav::apply(draft.selected, draft.visible_rows().len(), cmd);
                Some(draft.selected)
            });
            if let Some(selected) = selected {
                shell.object_dialog_scroll.scroll_to_item(selected);
            }
            cx.notify();
            return true;
        }
        // `tab`/`shift+tab`: reserved and inert, same reasoning as browse.
        if ks.key == "tab" {
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
                // Reachable now (§18.3): a filter typed into the edit
                // stage's own query, then `escape` twice — the first
                // rung (handled above, in the `filtering` branch) leaves
                // filter mode keeping the query; this one drops it.
                if let Some(draft) = draft_mut(shell) {
                    draft.query.clear();
                    draft.selected = 0;
                }
                shell.object_dialog_scroll.scroll_to_item(0);
                // Clearing the draft's own query is the whole rung: the
                // shared `Input` is emptied from it by
                // `dialog::sync_dialog_text`, which reads
                // `effective_query` and so takes the draft's copy while
                // this stage is open (spec §16.2).
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
                leave_edit(shell, cx);
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
    match cmd {
        NormalCommand::Nav(nav) => {
            let selected = shell.object_dialog.as_mut().and_then(|state| {
                let draft = state.draft.as_mut()?;
                draft.selected = vimnav::apply(draft.selected, draft.visible_rows().len(), nav);
                Some(draft.selected)
            });
            if let Some(selected) = selected {
                shell.object_dialog_scroll.scroll_to_item(selected);
            }
        }
        NormalCommand::Toggle => match draft_mut(shell).map(Draft::toggle_selected) {
            Some(Step::Changed) => {
                maybe_refresh_available(shell);
                revalidate(shell);
                scroll_to_cursor(shell);
                commit_or_confirm(shell, cx);
            }
            Some(Step::Refused(reason)) => refuse_step(shell, reason),
            _ => set_notice(shell, "nothing on this row changes with space".to_string()),
        },
        NormalCommand::ToggleBack => match draft_mut(shell).map(Draft::toggle_selected_back) {
            Some(Step::Changed) => {
                maybe_refresh_available(shell);
                revalidate(shell);
                scroll_to_cursor(shell);
                commit_or_confirm(shell, cx);
            }
            Some(Step::Refused(reason)) => refuse_step(shell, reason),
            _ => set_notice(
                shell,
                "nothing on this row changes with shift+space".to_string(),
            ),
        },
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
                    commit_or_confirm(shell, cx);
                }
                None => set_notice(shell, "that is as far as this row goes".to_string()),
            }
        }
        NormalCommand::Verb('d') => arm_delete(shell),
        NormalCommand::Verb('r') => arm_revert(shell),
        NormalCommand::Verb('o') => arm_overwrite(shell),
        // §18.2: take the column under the cursor out of the view.
        // Views-only by what `Draft::remove_selected` itself decides (a
        // per-field `dest`, not a scan of the list's current contents —
        // see its own doc), not by a check here, so its two `Refused`
        // reasons are routed straight to the footer rather than through
        // `refuse_step`: that helper's `d`/`r` hint is for the "must keep
        // at least one entry" refusal `space` can also produce, and
        // neither of `x`'s own reasons is asking for either verb.
        // No `scroll_to_cursor` here, unlike the `space` arms: a removal
        // leaves the cursor at its own visible index (or one above it),
        // which was on screen before the keystroke and so still is — the
        // demoted row is the one that travels, and the cursor no longer
        // travels with it (`Draft::remove_selected`'s own comment).
        NormalCommand::Verb('x') => match draft_mut(shell).map(Draft::remove_selected) {
            Some(Step::Changed) => {
                revalidate(shell);
                commit_or_confirm(shell, cx);
            }
            Some(Step::Refused(reason)) => set_notice(shell, reason),
            _ => set_notice(
                shell,
                "x removes a column from the view — here, space unticks".to_string(),
            ),
        },
        NormalCommand::EnterFilter => {
            // §18.3: the same switch browse's own `/` throws, and the
            // same pure mutation — `dialog::sync_dialog_text` gives the
            // filter focus on this handler's return.
            if let Some(state) = shell.object_dialog.as_mut() {
                state.mode = DialogMode::Filter;
            }
        }
        // `enter` and `i` have no row to act on in any draft built so
        // far: every field is a choice, a list, or (Groupings' `slot`,
        // both of Scopes' rows) a read-only `Text` — and `space` only
        // helps for the first two. Checked here rather than always
        // pointing at `space`, because that used to be false on a
        // Scopes row: pressing `space` right after would immediately say
        // "nothing on this row changes with space" — two verbs
        // disagreeing about the same row in the same breath.
        NormalCommand::Commit | NormalCommand::EditText => edit_commit_notice(shell),
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

/// `enter`/`i` in the edit stage — reachable from either mode
/// (normal mode's own `Commit`/`EditText` match arm, and filter mode's
/// `enter`, which gives the identical notice because there is nothing to
/// open either way): say whether `space` would do anything on the
/// selected row, since every field here is a choice, a list, or a
/// read-only `Text`, and `space` only helps for the first two.
fn edit_commit_notice(shell: &mut ShellView) {
    let steppable = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .is_some_and(selected_field_is_steppable);
    let notice = if steppable {
        "press space to change the selected row"
    } else {
        "this row is read-only — nothing here has a verb"
    };
    set_notice(shell, notice.to_string());
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
/// [`editing_row`] the action bar builds `d` and `r` from — a row that
/// offers neither (a desk-owned slot the user has not forked) gets the
/// reason alone rather than an invented verb.
fn refuse_step(shell: &mut ShellView, reason: String) {
    let hint = match editing_row(shell) {
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

/// After a `Toggle`/`ToggleBack` step that just changed a Views draft's
/// `dataset` field, rebuild the `columns` field's available catalogue for
/// the newly chosen dataset (spec §18.2: "changing the dataset empties
/// Available and repopulates it; members that the new dataset lacks stay
/// listed... so the diagnostic can name them").
///
/// Checked by which row the cursor is STILL on, not by domain alone: a
/// `Field` row's value changes in place (`Draft::step_selected` never
/// moves the cursor off it, unlike an item row's add), so after the step
/// the cursor is the one reliable way to ask "was that the dataset field"
/// without threading the answer through every caller. Every other
/// domain's fields, and every other Views field, leave `views::
/// refresh_available` untouched — it only ever rebuilds `columns`.
fn maybe_refresh_available(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_mut() else {
        return;
    };
    if state.domain != Domain::Views {
        return;
    }
    let Some(draft) = state.draft.as_mut() else {
        return;
    };
    let is_dataset_row = matches!(
        draft.selected_row(),
        Some(EditRow::Field(i)) if draft.fields.get(i).is_some_and(|f| f.key == "dataset")
    );
    if is_dataset_row {
        views::refresh_available(draft, &shell.services.config);
    }
}

/// Would `space` change anything on the row the cursor is on? A
/// read-only mirror of [`Draft::step_selected`]'s own dispatch — never
/// calling it, since that mutates — kept in one place so the "press
/// space" notice and the actual stepping behaviour cannot say different
/// things about the same row (the defect this function's own call site
/// fixes: `enter`/`i` used to point at `space` unconditionally, which
/// was false on a Scopes row).
fn selected_field_is_steppable(draft: &Draft) -> bool {
    match draft.selected_row() {
        Some(EditRow::Field(i)) => !matches!(
            draft.fields[i].kind,
            FieldKind::Text(_) | FieldKind::MultiChoice { .. } | FieldKind::OrderedList { .. }
        ),
        // Both list rows have a verb under `space`: an item's own
        // inclusion toggles, an available row is added (§18.7.2).
        Some(EditRow::Item { .. } | EditRow::Available { .. }) => true,
        None => false,
    }
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

/// Record the change the keystroke just made — or ask first, if applying
/// it would fork the object.
///
/// The one door every field edit leaves through, so there is one place
/// the fork question is asked and one place a change joins the pending
/// batch. [`super::apply::commit_edit`] does that recording; the merge,
/// the application and the file write happen together on the debounce
/// behind it. The trader sees the change immediately regardless — the row
/// under the cursor is painted from the draft this function was called
/// after mutating.
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
    // An early, UX-only check: skip the fork question entirely for an
    // edit that can never be saved, rather than asking the trader to
    // confirm a fork and then refusing it. The actual gate is
    // `apply::blocking_diagnostic` itself, checked again inside
    // `apply::commit_edit` — this call and `run_confirmed`'s
    // `Confirm::Fork` arm are both refused by that inner check
    // regardless of this one, so this dialog has exactly one *safety*
    // gate even though it has two call sites into it.
    if let Some(notice) = super::apply::blocking_diagnostic(shell) {
        set_notice(shell, notice);
        cx.notify();
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
///
/// Pure, and — alone among this file's stage transitions — it sets **no**
/// mode, because neither way in has one to change. The `PreviousStage`
/// rung is reachable only from normal mode with an empty query
/// (`dialogmode::escape_step` hands out `LeaveFilter` and `ClearQuery`
/// first), so the keys are already on the shell root; and
/// [`run_confirmed`]'s delete/revert arm answers a question the trader
/// armed in whichever mode they were in — a confirm button clicked while
/// filtering (see [`press_verb`]) keeps them filtering, which is the
/// honest outcome rather than a blur that would leave the pill reading
/// `filter` over a dead field. Either way `dialog::sync_dialog_text`
/// reconciles the field and the focus to the mode that stands, which is
/// why this needs no `Window` of its own — it clears the query and
/// nothing else about the keyboard.
fn leave_edit(shell: &mut ShellView, cx: &mut Context<ShellView>) {
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

/// `name`'s removal from each of `docs` whose **user layer** actually
/// contains it, as the bare `(doc, name)` keys [`apply::commit_removal`]
/// queues onto the same batch a field edit would — so a delete never
/// creates an empty file to say nothing (no key means no entry means no
/// write) and never touches a doc it does not own.
///
/// Deliberately keys only, never a `None`-valued edit map: building the
/// `Option<toml::Value>` is `commit_removal`'s job now, precisely so
/// nothing on this side of the door can hand it a `Some`. See
/// `commit_removal`'s own doc for why that used to be a mere convention.
///
/// This used to write the file itself, straight off the render thread,
/// with no in-memory merge at all — invisible until the 500 ms watcher
/// noticed. Building the touched-key list and handing it to
/// `commit_removal` instead means a delete or revert rides the exact
/// path an edit does: merged through `Config::from_docs`, applied
/// through `apply_reload`, written by the same `run_writes`, reverted by
/// the same `revert_failed_write` if the write fails.
fn removal_edits(
    shell: &ShellView,
    docs: &[&'static str],
) -> Result<Vec<(&'static str, String)>, String> {
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
    Ok(touched.into_iter().map(|doc| (doc, name.clone())).collect())
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
///
/// `row.layer: None` (§18.4 — an unconfigured Groupings slot) gets its
/// own arm rather than falling into the generic refusal below: "comes
/// from the no layer" is nonsense copy for a row nothing defines yet, so
/// the message names the real state ("is empty") and the real remedy
/// (tick a dimension) instead.
fn arm_delete(shell: &mut ShellView) {
    match editing_row(shell) {
        Some(row) if row.layer == Some(Layer::User) => {
            if let Some(draft) = draft_mut(shell) {
                draft.confirm = Some(Confirm::Delete);
            }
        }
        Some(row) if row.layer.is_none() => set_notice(
            shell,
            format!("{} is empty — tick a dimension to fill it", row.name),
        ),
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

/// `o`: arm the confirm to overwrite the saved scope under the cursor
/// with the frame's current one — `Domain::Scopes`'s one genuinely new
/// verb (this crate's Part 2a Task 5). Every other domain has no object
/// this letter could act on, so it falls through to the same "not a
/// verb here" wording the general catch-all in [`handle_edit_key`] uses,
/// worded identically wherever a stray letter fires.
///
/// **Whether the write also forks the object is decided here, not by
/// how destructive it is.** Overwriting a scope the user layer does not
/// already own lands through `apply::commit_edit` exactly like any other
/// definitional edit — which forks it into the user layer (spec §4.1),
/// freezing the desk's copy out. That has to be *disclosed*, in the
/// prompt, or a trader learns it weeks later when the desk's changes
/// stop arriving (spec §16) — it is not enough that the confirm already
/// asks about something else. `editing_row` (already `arm_delete`'s and
/// `arm_revert`'s own test for "does the user layer own this") answers
/// it directly; `apply::would_fork` cannot be used here, because it
/// reads `draft.writes_by_destination()`, which is still empty at arm
/// time — `overwrite_with` has not run yet, so it would always say
/// `false` regardless of who owns the object.
fn arm_overwrite(shell: &mut ShellView) {
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
    let forks = editing_row(shell).is_some_and(|row| row.layer != Some(Layer::User));
    if let Some(draft) = draft_mut(shell) {
        draft.confirm = Some(Confirm::Overwrite { forks });
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
fn run_confirmed(shell: &mut ShellView, confirm: Confirm, cx: &mut Context<ShellView>) {
    let domain = match shell.object_dialog.as_ref() {
        Some(state) => state.domain,
        None => return,
    };
    match confirm {
        // The fork was the point of the question; answering yes applies
        // exactly the edit that armed it, down the one commit path. This
        // is the second of `commit_edit`'s two call sites — it skips
        // `commit_or_confirm`'s early `blocking_diagnostic` peek, but
        // `commit_edit` checks the same thing itself, so an edit an
        // error diagnostic rejects still cannot join the batch from
        // here either.
        Confirm::Fork => {
            if let Some(notice) = super::apply::commit_edit(shell, cx) {
                set_notice(shell, notice);
            }
            cx.notify();
        }
        // `o`'s confirmed answer. `shell.frame` (an `Entity<Frame>`) is
        // read here, at the gpui call site, precisely so `Domain`'s pure
        // core never has to know a `Frame` exists — the only place a
        // `Frame` is read for this dialog at all (an `Entity<InputState>`
        // is read elsewhere, e.g. the shared filter field, so this is
        // narrower than "the only entity"). Re-checks the domain rather
        // than trusting `arm_overwrite`'s own gate — the same "the gate
        // lives in the acting function, not just the one that arms it"
        // rule `commit_edit`'s own `blocking_diagnostic` check follows.
        // The new value replaces the draft's source and fields
        // (`scopes::overwrite_with`), which is what makes `commit_edit`
        // see a change to write; it goes through that same door rather
        // than a direct `config_write` call, so a failed write still
        // reverts the way any other edit's does, and `revalidate` runs
        // first so the diagnostics on screen describe the new content
        // rather than the old. `forks` was already spent on the prompt
        // (`Confirm::prompt`, chosen back in `arm_overwrite`) — nothing
        // here needs it again.
        Confirm::Overwrite { .. } => {
            if domain != Domain::Scopes {
                cx.notify();
                return;
            }
            let scope = shell.frame.read(cx).scope().clone();
            // `commit_edit` answers `None` both for "queued" and for
            // "nothing changed", so the no-op case is identified here
            // instead: a saved scope that already equals the frame's — the
            // ordinary state straight after `:scope load` — would otherwise
            // answer a deliberate second keystroke with no write, no config
            // change and nothing on screen. A confirmed verb that does
            // visibly nothing is the defect class this interaction model
            // exists to remove.
            let changed = draft_mut(shell).is_some_and(|draft| {
                scopes::overwrite_with(draft, &scope);
                draft.is_dirty()
            });
            revalidate(shell);
            if !changed {
                set_notice(
                    shell,
                    "already matches the frame's scope — nothing to write".to_string(),
                );
            } else if let Some(notice) = super::apply::commit_edit(shell, cx) {
                set_notice(shell, notice);
            }
            cx.notify();
        }
        // `d`/`r` share `apply::commit_removal` with `Confirm::Fork`'s
        // `commit_edit` above: same batch, same flush, same failure
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
                    // Preserves `docs`' own order rather than whatever
                    // order `keys` happens to hold, so a notice naming
                    // both files reads "views and view_presentation" the
                    // way it always has.
                    let name = match shell.object_dialog.as_ref().map(|state| &state.stage) {
                        Some(Stage::Edit { object }) => object.clone(),
                        _ => String::new(),
                    };
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
                    leave_edit(shell, cx);
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

/// The actions available on the object being edited, in the order they are
/// painted.
///
/// **No save.** There is nothing to save: a field edit is already
/// recorded, and merges, applies and reaches disk on its own timer with
/// no further keystroke. What is left is exactly the verbs that are
/// destructive or structural — `d` deletes the user's copy, `r` throws a
/// personal override away — plus, as a confirm rather than a standing
/// button, `Copy to user layer` ([`Confirm::Fork`]), which the first
/// definitional edit to a desk object arms.
///
/// An earlier build put `Save changes` here whenever the draft was dirty,
/// relabelled `Copy to user layer` when saving would fork. The fork
/// warning survives; the save does not, because a dirty draft never means
/// "there is something a save key could do from here" —
/// [`Draft::is_dirty`](super::Draft::is_dirty) enumerates the three ways it
/// can be true, and in each one the value is either already recorded,
/// waiting on a confirm that will record it, or refused by a diagnostic a
/// save key could not get past either.
///
/// **`d` and `r` are silent for at least one tick right after `n` creates
/// an object.** Both are gated on [`editing_row`], which derives from
/// `services.config` — and `commit_create` queues its write on the same
/// debounced batch every other edit does, so the object is not yet a row
/// the config can produce when this stage first paints. That is
/// deliberate, not a race to close: the edit header's own `new` chip
/// (`build_edit`) keys on [`Draft::is_new`](super::Draft::is_new) rather
/// than on this same absence, precisely so the header does not flicker
/// off the instant the row derives while these two verbs are still
/// correctly withheld from an object with no layer of its own to delete
/// or revert yet.
fn actions(shell: &ShellView) -> Vec<Action> {
    let Some(state) = shell.object_dialog.as_ref() else {
        return Vec::new();
    };
    if state.draft.is_none() {
        return Vec::new();
    }
    let row = editing_row(shell);
    let mut out = Vec::new();
    if row.as_ref().is_some_and(|r| r.layer == Some(Layer::User)) {
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
    // The one new verb this domain adds (this module's `arm_overwrite`
    // doc has the full reasoning): available whenever a scope is open,
    // regardless of layer or override, since `Confirm::Overwrite`'s own
    // commit forks a desk-owned scope the same way any other definitional
    // edit would.
    if state.domain == Domain::Scopes {
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

/// The [`dialog::ShellModal::build`] closure body: the mode pill, the
/// shared filter row (a [`dialog::name_row`] in [`Stage::Naming`]
/// instead — §18.2), the scrollable browse list, and a muted footer
/// stating the current stage's and mode's vocabulary. `entity` is what
/// each row's click handler captures to reach [`on_row_clicked`] later,
/// at click time — `shell` is this call's own plain-borrow read (see
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
                    .font_family(crate::fonts::MONO)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(highlighted_text(&row.summary, &summary_ix, theme.primary)),
            );

        // Provenance, right-aligned: the layer that won as a muted outlined
        // badge, and `overridden` as a `primary` one beside it — a
        // classification worn on the row, distinct from `overridden`'s
        // sibling colour because unticking the classification would leave
        // nothing louder for the states that mean something is wrong.
        //
        // `row.layer: None` (§18.4 — an unconfigured Groupings slot)
        // paints no layer badge at all: no layer defines the row, so
        // naming one would be a lie about a name it doesn't have.
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
        // Always `false` today (see `ObjectRow::drifted`'s own doc) — the
        // badge exists so the day `overrides.toml` starts recording real
        // drift, nothing here needs to change.
        if row.drifted {
            markers = markers.child(dialog::badge(
                "drifted",
                theme.muted_foreground,
                theme.border,
                Some(format!("objectdialog-drifted-{}", row.name)),
                cx,
            ));
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

    // §18.2: the naming stage replaces the filter row with the name field
    // and states its own two-verb vocabulary — never the browse footer's,
    // even though `begin_naming` leaves `state.mode` at `Filter` (the
    // `Input` really does own the keys) — a footer offering `/` or `j`/`k`
    // while the name field is focused would be advertising keys the
    // field, not this handler, would consume.
    let naming = matches!(state.stage, Stage::Naming);

    // The hint row states the CURRENT mode's vocabulary, never the union
    // of both: a modal surface's whole risk is a user who cannot tell
    // which mode they are in, and a footer listing keys that are inert
    // right now is exactly the lie the mode pill exists to prevent.
    let (motion, action): (Vec<AnyElement>, Vec<AnyElement>) = if naming {
        (
            Vec::new(),
            vec![
                chip("enter"),
                sep("create ·"),
                chip("escape"),
                sep("cancel"),
            ],
        )
    } else {
        match state.mode {
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
                {
                    let mut action = Vec::new();
                    // `n` is not on the footer at all for a domain whose
                    // roster is fixed (Groupings) — advertising a key that
                    // only ever says "the slots are fixed" teaches a verb
                    // with nothing behind it.
                    if state.domain.roster().is_none() {
                        action.push(chip("n"));
                        action.push(sep("new ·"));
                    }
                    action.push(chip("/"));
                    action.push(sep("filter ·"));
                    action.push(chip("escape"));
                    // Honest about which rung the next escape takes: with
                    // a query still applied it clears the query, and only
                    // then closes.
                    action.push(sep(if state.query.is_empty() {
                        "close"
                    } else {
                        "clear the filter"
                    }));
                    action
                },
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
        }
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
    // this module's own "one switch" note. Naming always focuses the
    // field (`begin_naming` sets `Filter`), so `frozen_query` is moot
    // there — the naming row below is a `name_row`, never a `filter_row`.
    // `slash_filters: true` unconditionally — this dialog has no capture
    // state, so `/` enters filter mode from every frozen moment it has
    // (see `dialog::FrozenFilter`).
    let frozen_query = (state.mode == DialogMode::Normal).then_some(dialog::FrozenFilter {
        query: state.query.as_str(),
        slash_filters: true,
    });

    let top_row = if naming {
        dialog::name_row(
            &shell.dialog_input,
            &format!("New {} · name", object_word(state.domain)),
            cx,
        )
    } else {
        dialog::filter_row(&shell.dialog_input, frozen_query, cx)
    };

    v_flex()
        .gap_2()
        .child(top_row)
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
        }
        // §18.2: `n` this session, and still true for the whole life of
        // the stage regardless of `row` — `editing_row` derives from
        // `services.config`, which stays behind `commit_create`'s own
        // zero-debounce flush for at least one executor tick, so `row` is
        // `None` right after creation even though the object is already
        // queued to exist. Keying on `draft.is_new` alone (never also
        // `row.is_none()`) is what keeps the badge painted through that
        // tick instead of flickering off the moment the row derives.
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

    let domain = state.domain;
    let rows = draft.rows();
    let visible = draft.visible_rows();
    let mut list = v_flex()
        .id("objectdialog-fields")
        .w(px(WIDTH))
        .max_h(px(VISIBLE_ROWS as f32 * ROW_HEIGHT))
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
        // Set only for a list row that opens a new block — see this
        // loop's own comment on `last_item_section`.
        let mut section_header: Option<AnyElement> = None;
        let (selector, label, value) = match edit_row {
            EditRow::Field(index) => {
                let field = &draft.fields[index];
                let dest_label = match field.dest {
                    Destination::Doc => "doc",
                    Destination::Presentation => "pres",
                };
                (
                    format!("objectdialog-field-{}", field.key),
                    highlighted_text(&field.label, &m.indices, theme.primary),
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(field_value(field)),
                        )
                        .child(dialog::badge(
                            dest_label,
                            theme.muted_foreground,
                            theme.border,
                            Some(format!("objectdialog-dest-{}", field.key)),
                            cx,
                        ))
                        .into_any_element(),
                )
            }
            // The object's own items and its available catalogue paint
            // the same row shape, so they share one arm — but which list
            // the row came from is read off the VARIANT and named here
            // (`own`), never inferred from the entry itself: an available
            // row is a different thing to `space`, to `x` and to the grip,
            // and §18.7.1's whole point is that no consumer gets to treat
            // one as the other by omission.
            EditRow::Item { field, item } | EditRow::Available { field, item } => {
                let own = matches!(edit_row, EditRow::Item { .. });
                let FieldKind::OrderedList { items, available } = &draft.fields[field].kind else {
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
                    let (text, suffix) = section_header_text(domain, own);
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
                            .child(text)
                            .into_any_element(),
                    );
                }
                // The grip marks a row as reorderable — every item of the
                // object's own list, which for Groupings' `dimensions` is
                // all of them (it has no catalogue); an available row gets
                // an equal-width spacer instead, so the tick beside it
                // still lines up. The tick is the inclusion state, and a
                // hidden item is muted as well as unticked — one signal is
                // a thing a glance misses on a 30-row list. Neither the
                // grip nor the tick is part of what the filter ranked
                // (`Draft::row_label`); only the name itself is
                // highlighted.
                let grip = if own {
                    div()
                        .text_color(theme.muted_foreground)
                        .w(px(11.))
                        .child("⋮")
                        .into_any_element()
                } else {
                    div().w(px(11.)).into_any_element()
                };
                let tick = div()
                    .font_family(crate::fonts::MONO)
                    .w(px(13.))
                    .text_color(if entry.included {
                        theme.success
                    } else {
                        theme.muted_foreground
                    })
                    .child(if entry.included { "✓" } else { "·" })
                    .into_any_element();
                let mut name_row = h_flex()
                    .pl_4()
                    .gap_1()
                    .items_center()
                    .child(grip)
                    .child(tick);
                if !entry.included {
                    name_row = name_row.text_color(theme.muted_foreground);
                }
                let name = name_row
                    .child(highlighted_text(&entry.name, &m.indices, theme.primary))
                    .into_any_element();
                let width = match entry.width {
                    Some(width) => format!("{width:.0}px"),
                    None => "auto".to_string(),
                };
                (
                    format!("objectdialog-item-{}", entry.name),
                    name,
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
        let row_el = element
            .child(label)
            .child(value)
            .debug_selector(move || selector.clone())
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                entity_for_row.update(cx, |shell, cx| {
                    on_edit_row_clicked(shell, clicked, window, cx);
                });
            });
        // The header rides on the first item's own element so the list's
        // child count still equals its row count (`visible.len()`) —
        // `scroll_to_item` indexes children by that count, and a header
        // emitted as its own `list.child(header)` before the row would
        // make every following index off by one.
        list = list.child(match section_header {
            Some(header) => v_flex().child(header).child(row_el).into_any_element(),
            None => row_el.into_any_element(),
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
    } else if state.mode == DialogMode::Filter {
        // §18.3: the same filter-mode hints browse paints, since the
        // vocabulary — type to narrow, the shared nav keys, `escape` back
        // to normal — is identical in both stages.
        (
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
        )
    } else {
        let mut motion = vec![
            chip("j"),
            chip("k"),
            sep("move ·"),
            chip("space"),
            chip("shift+space"),
            sep("change ·"),
            chip("shift+j"),
            chip("shift+k"),
        ];
        // `x` is Views-only (§18.2 — see `mod.rs`'s `Draft::remove_selected`
        // doc): a hint for a verb every other domain's `x` merely refuses
        // would teach a trader on Groupings or Scopes a key that does
        // nothing there.
        if state.domain == Domain::Views {
            motion.push(sep("reorder ·"));
            motion.push(chip("x"));
            motion.push(sep("remove"));
        } else {
            motion.push(sep("reorder"));
        }
        let action = vec![
            chip("/"),
            sep("filter ·"),
            chip("escape"),
            // Honest about which rung the next escape takes — the same
            // rule browse's own footer keeps (§18.3): with a query still
            // applied it clears the query, and only then goes back.
            sep(if draft.query.is_empty() {
                "back to the list"
            } else {
                "clear the filter"
            }),
        ];
        (motion, action)
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

    // The live `Input` renders only when it actually owns the keystrokes
    // — see this module's own "one switch" note, now also the edit
    // stage's rule (§18.3).
    // `slash_filters: true` for the same reason as browse's own call.
    let frozen_query = (state.mode == DialogMode::Normal).then_some(dialog::FrozenFilter {
        query: draft.query.as_str(),
        slash_filters: true,
    });
    let filter = dialog::filter_row(&shell.dialog_input, frozen_query, cx);

    v_flex()
        .gap_2()
        .child(header)
        .child(diagnostics)
        .child(filter)
        .child(list)
        .child(action_block)
        .child(footer)
        .into_any_element()
}

/// The small-caps text and selector suffix for the section header that
/// opens an ordered list's own items or its available catalogue (§18.1).
/// `own` distinguishes Views' own columns from the rest of its dataset's;
/// Groupings' `dimensions` has no catalogue at all (`groupings.rs`'s own
/// module doc), so only the first arm there is ever reached.
fn section_header_text(domain: Domain, own: bool) -> (&'static str, &'static str) {
    match (domain, own) {
        (Domain::Views, true) => (
            "COLUMNS — space hides · shift+j / shift+k reorder · x removes",
            "members",
        ),
        (Domain::Views, false) => ("AVAILABLE — space adds", "available"),
        (Domain::Groupings, _) => (
            "DIMENSIONS — space includes · shift+j / shift+k reorder",
            "members",
        ),
        // Scopes has no `OrderedList` field at all (`scopes.rs`'s module
        // doc — the whole object is a read-only summary), so this arm is
        // unreachable; kept only to stay exhaustive as domains are added.
        (Domain::Scopes, _) => ("", "members"),
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
/// Buttons, not rows and not bare keys. A key alone has no clickable
/// target, and every other verb in Geode's dialogs has one; an `outline`
/// button is the mock's own local-command-bar look (§18.1), and the
/// destructive ones are `danger` rather than merely worded strongly.
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
            .outline()
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
            // Wrapped for the same reason each action-bar button is:
            // `debug_bounds` resolves a `debug_selector`, not a button's
            // element id, so a test that answers with the mouse
            // (`confirming_with_the_mouse_while_filtering_empties_the_
            // field`) has something to aim at.
            div()
                .debug_selector(|| "objectdialog-confirm-yes".to_string())
                .child(
                    Button::new("objectdialog-confirm-yes")
                        .small()
                        .danger()
                        .label(match confirm {
                            Confirm::Delete => "Delete",
                            Confirm::Revert => "Revert",
                            Confirm::Fork => "Copy to user layer",
                            Confirm::Overwrite { .. } => "Overwrite",
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
                                    run_confirmed(shell, confirm, cx);
                                }
                                // A mouse answer never passes through the key
                                // path, so it needs the same seam a row click
                                // does (spec §16.1): `run_confirmed`'s
                                // delete/revert arm walks all the way back to
                                // browse through `leave_edit`, and the field it
                                // was filtering with is emptied here or nowhere.
                                dialog::sync_dialog_text(shell, window, cx);
                            });
                        }),
                ),
        )
        .child(
            div()
                .debug_selector(|| "objectdialog-confirm-no".to_string())
                .child(
                    Button::new("objectdialog-confirm-no")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .on_click(move |_event, window, cx| {
                            leave_it.update(cx, |shell, cx| {
                                cancel_confirm(shell);
                                // Nothing here moves the mode or the query, so
                                // the sync is a no-op today — present for the
                                // same reason its twin above is: this closure is
                                // off the key path, and the seam belongs to the
                                // door rather than to what happens to be behind
                                // it right now.
                                dialog::sync_dialog_text(shell, window, cx);
                                cx.notify();
                            });
                        }),
                ),
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
        "o" => arm_overwrite(shell),
        _ => {}
    }
    cx.notify();
}

/// A click on an edit-stage row moves the draft's cursor there — the
/// mouse's half of `j`/`k`, and the reason a click never also acts: the
/// verb is a second, deliberate keystroke or button press.
///
/// Focus follows the current mode, exactly as browse's [`on_row_clicked`]
/// does and by the same means — [`dialog::sync_dialog_text`], which a
/// click needs of its own because it never passes through the key path
/// (spec §16.1).
///
/// Before §18.3 the edit stage could not be in [`DialogMode::Filter`] at
/// all, so this handler focused the shell unconditionally; now that `/`
/// reaches here, doing so would leave the pill reading `filter` and the
/// caret painted over a blurred `Input` — the "one switch" broken by a
/// mouse click, with every following keystroke going nowhere until
/// `escape`.
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
        // `position` is the FILTERED index `build_edit` painted this row
        // at (§18.3), so the bound to check — and the value to store,
        // unchanged — is against `visible_rows`, not the unfiltered
        // `rows`.
        if position >= draft.visible_rows().len() {
            return;
        }
        draft.selected = position;
    }
    shell.object_dialog_scroll.scroll_to_item(position);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}
