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
//! Nothing an edit does asks first. A change that **forks** the object
//! into the user layer (spec §4.1) is applied on the keystroke like any
//! other and *announced* — the notice names the copy and the `r` that
//! undoes it — because the confirm it used to arm was more distracting
//! than the fork it disclosed (user ruling 2026-09-14). The action bar's
//! verbs are exactly the destructive ones, and only those confirm.

use std::rc::Rc;

use geode_core::config::{Layer, Severity, check_object_name};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, MouseButton, Window, div, px};
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
    ColumnContext, ColumnDoor, ColumnLayers, Confirm, Destination, Domain, Draft, EditRow, FellTo,
    Field, FieldKind, Fold, ObjectDialogState, ObjectRow, READ_ONLY_NOTICE, RowDrag, RowVocabulary,
    Stage, Step,
};
use crate::dialogmode::{self, DialogMode, EscapeStep, NormalCommand};
use crate::footer::{Hint, HintRow};
use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;
use crate::vimnav;

use super::super::ShellView;
// Aliased: `colours` (unqualified, `use super::colours;` above) is the
// `Domain::Colours` adapter; this is `shell::colours`, the gpui<->pure
// theme bridge (§6.1) — a different module, one directory further out,
// that the adapter itself never touches.
use super::super::colours as colour_theme;
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
            // §19.1: a value field runs in `Filter` (that is what gives
            // it the keys), but "filter" is the wrong word for a field
            // whose text is the value it will apply — the pill says
            // `edit`, or `chain` for the chain field's own case.
            .children(
                state.map(|s| match s.draft.as_ref().and_then(|d| d.text_entry) {
                    Some(entry) if entry.completions => dialog::chain_pill(cx),
                    Some(_) => dialog::edit_pill(cx),
                    None => dialog::mode_pill(s.mode, cx),
                }),
            )
            .into_any_element()
    });
}

/// The title-row crumb (§18.1): a count in browse and naming, the slot's
/// chord in a Groupings edit, the object and column in the column stage
/// (Part 2c §5.2), nothing otherwise. Pure so a test can read it without
/// laying out a window.
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
        // The column stage shares the edit stage's whole key table (Part
        // 2c §5.2): it is the same draft with different fields installed,
        // so `space`, `i`, `/`, `d`/`r`/`o` and the escape ladder all mean
        // what they already mean — the only key that behaves differently
        // is `enter`, and it branches inside the `Commit` arm on what the
        // cursor is on rather than on the stage.
        Some(Stage::Edit { .. } | Stage::Column { .. }) => handle_edit_key(shell, ks, cx),
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
    // §19.3: read before `state` takes its `&mut` borrow of
    // `shell.object_dialog` below, whose lifetime spans the rest of this
    // function — `seed_dataset_under_cursor` needs a plain `&ShellView`,
    // which a live sibling `&mut` borrow would refuse. `None` on any
    // domain but Sources (the function's own first check), so this costs
    // every other domain nothing but the check itself.
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
            // `n`: the naming stage (§18.2), or the domain's refusal —
            // `begin_new_object`, the one door the browse bar's button
            // takes too (spec §20.3). `state`'s borrow ends at the match
            // arm, so the door can take `shell` whole.
            NormalCommand::Verb('n') => {
                begin_new_object(shell, seed.clone(), seed_taken);
                cx.notify();
                return true;
            }
            // §18.8: a bare digit names a slot on the one domain whose
            // objects are numbered; elsewhere it is dropped below.
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
        // Three ways a name can be taken, and they need different
        // instructions. Reserved (Colours' `none`/`sign`, Part 2c §6.1)
        // is checked first — no row and no orphaned presentation could
        // ever explain it, so it gets its own message rather than
        // falling into either of the other two, both of which point the
        // trader at something that does not exist for a reserved name. A
        // name with a row is one `escape` and an `enter` away; a name
        // only the presentation overlay holds (`Domain::name_taken`'s
        // own doc) has nothing on this list to open at all, so pointing
        // the trader at the list would be a dead end — the orphaned
        // `view_presentation.toml` entry is the thing in their way, and
        // it is the thing the notice names.
        let notice = if domain.reserved_names().contains(&name.as_str()) {
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
        // §19.3: the dataset `n` seeded from the cursor row
        // (`seed_dataset_under_cursor`) becomes the new source's own
        // `dataset` field — revalidated so the idle-source warning shows
        // in the edit stage immediately rather than one debounce late.
        sources::seed_dataset(&mut draft, &dataset);
        draft.diagnostics = domain.validate(&draft, &shell.services.config);
    }
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

/// `n` (§18.2, spec §20.3): the naming stage, unless the domain refuses
/// it outright — read-only (`Domain::writable`, §19.4 — Schema), or a
/// roster that already names every row there is (Groupings' nine fixed
/// slots, where nothing can be created that is not already on the list,
/// `Domain::roster`'s own doc). Called from the key and the browse
/// stage's `n` button alike. `seed` and `seed_taken` are §19.3's Sources
/// seeding ([`seed_dataset_under_cursor`]), computed by the caller
/// before `object_dialog` is borrowed.
///
/// `begin_naming` is the whole transition: it clears `query` and sets
/// `DialogMode::Filter`, and `dialog::sync_dialog_text` empties the
/// shared `Input` and focuses it to match on the caller's return. That
/// clear is what keeps a stale browse filter (typed, then `escape`'d
/// back to normal mode without clearing it) out of the name field — the
/// sync writes the field from `effective_query`, so whatever `query`
/// still held would otherwise be written straight back into it.
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
        // §19.3: `n` on Sources seeds the new source's dataset from the
        // browse row under the cursor, and pre-fills the name field with
        // it too when no source already holds that name — one source per
        // dataset is the common case, so the trader's next keystroke is
        // usually just `enter`. `seed` is `None` on every domain but
        // Sources, so this is a no-op everywhere else.
        state.naming_dataset = seed.clone();
        if let Some(dataset) = seed
            && !seed_taken
        {
            state.query = dataset;
        }
    }
}

/// §19.3: the dataset of the browse row under the cursor, for `n` on
/// Sources — `None` on every other domain, or with no row (an empty
/// list, or a keystroke racing the modal closing).
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

/// A real mouse click on the row for `clicked` (resolved back to a
/// position in the *filtered* list against freshly derived rows): §17.1
/// rule 2 makes it do what `enter` on that row would — select it and
/// open [`enter_edit_stage`] — except in [`Stage::Naming`], where `enter`
/// creates instead of opens and a click must therefore only select, or a
/// stray click while typing a name would silently discard it. Either way
/// it moves focus the same way [`handle_key`] does — to whichever
/// surface the current mode owns — through [`dialog::sync_dialog_text`]
/// (spec §16.1), which a mouse handler needs of its own because a click
/// never passes through the key path at all. Focusing the filter
/// unconditionally here would let a mouse click silently defeat normal
/// mode, and the next keystroke would type instead of act.
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
    // §17.1 rule 2: a click does what `enter` would — open the row —
    // except while naming, where `enter` creates and a click must not
    // discard the typed name. Same door as `open_selected`, and by
    // name rather than index for the same reason the selector is.
    let opens = state.stage != Stage::Naming;
    let name = clicked.to_string();
    shell.object_dialog_scroll.scroll_to_item(ix);
    if opens {
        enter_edit_stage(shell, &name, None, cx);
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

/// **The one door into the column stage** (Part 2c §5.2) — `enter`, or a
/// click, on a member row of a Views draft or on a column row of the
/// Schema inspector (dataset-presentation spec §4.1).
///
/// Two doors, one stage: the seven fields are `views::column_fields`
/// either way and every verb in the stage means the same thing. What
/// differs is entirely carried by the [`ColumnContext`] this function
/// installs — which door, which layers sit under the column, and, for
/// Schema, the dataset overlay table and the scratch item the fold
/// writes into — so nothing below this function has to ask which dialog
/// it is in. The Views door folds into the item its own `columns` list
/// holds and writes `view_presentation.toml`; the Schema door folds into
/// the context's item and writes `dataset_presentation.toml`.
///
/// A pure mutation, like [`enter_edit_stage`]: the mode is set to
/// `Normal` here and `dialog::sync_dialog_text` empties and blurs the
/// shared `Input` on the handler's return (spec §16.1). The stage opens
/// in normal mode whatever mode `enter` arrived in — the same reason
/// `enter_edit` gives for its own explicit set: a stage left reading
/// `Filter` sends the next `escape` down a rung this handler does not
/// claim, and the shell closes the whole dialog instead of stepping back
/// to the view.
///
/// The colour names come from the live `colours` doc — with the pending
/// write batch folded in, [`enter_edit_stage`]'s own rule — read at open
/// time rather than carried on the draft: the doc can be reloaded while
/// the dialog stands, and the `Choice` is built once per stage, so a
/// name added to `colours.toml` shows up the next time a column is
/// opened. That is the same freshness every other choice list here has.
///
/// [`Draft::enter_column`] answers `false` for a name neither the
/// object's list nor its fields hold, and then nothing moves: no stage
/// change, no cursor change, and the row keeps 2b's notice — with the
/// context dropped again, so a refused open cannot leave a door's
/// bookkeeping behind for the next fold to read. That is the
/// available-row case (the caller's own `EditRow::Item` guard already
/// refuses it) and, more usefully, the case where a future caller aims at
/// a stale name.
fn enter_column_stage(shell: &mut ShellView, column: &str, cx: &mut Context<ShellView>) {
    // Through the folded config, exactly as `enter_edit_stage` reads it
    // (the final review's M-6): inside the 250 ms write debounce
    // `services.config` is still the documents as they stood before the
    // last tick, so a colour just created in the Colours dialog would be
    // missing from this `Choice` for as long as that window is open. The
    // two doors into a stage now agree about what "the live config"
    // means.
    // Named `pending` rather than `folded` (the spelling `enter_edit_stage`
    // uses) only so the mutation harness's anchor on that line stays
    // unambiguous — one entry, one site.
    let pending = apply::config_with_pending(shell);
    let config = pending.as_ref().unwrap_or(&shell.services.config);
    let colours: Vec<String> = config
        .doc("colours")
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
    // Read here, while `config` is still borrowed, because everything
    // below wants the draft mutably. Both halves come from the SAME
    // pending-aware config the colours did — the Schema door's whole
    // write is rendered from `overlay_object`, so seeding it from
    // `services.config` inside the debounce would render the dataset's
    // other columns as they stood before the last keystroke and undo it
    // (§18.8's own Major, one layer over).
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
            // The layer between the desk view and this view's own
            // overlay, refreshed from the PENDING-aware config before
            // the context is built (dataset-presentation spec §5.1): a
            // dataset-level edit made in the Schema dialog inside the
            // 250 ms write debounce is otherwise invisible here, and a
            // stage that cannot see it both hides that edit and, on its
            // own next keystroke, writes the column back without it.
            draft.dataset_layer = views::dataset_layer_for(config, &object);
            (
                views::column_fields(&item, &colours, Destination::Presentation),
                // The one builder of this door's context — shared with
                // the two test openers that mirror this function, so
                // they cannot drift from it (`views::column_context`).
                views::column_context(draft, column, item),
            )
        }
        // §4.3: the dataset overlay is the only layer this door has, and
        // it is both the seed and the `dataset` layer — a Schema field
        // therefore reads `dataset` or nothing, never `desk`, since the
        // desk's own keys vary per view and sit BELOW this one.
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
        // and Colours have no per-column presentation to open, and
        // `column_stage_target` never names a row on one.
        _ => return,
    };
    draft.column_ctx = Some(ctx);
    if !draft.enter_column(column, fields) {
        draft.column_ctx = None;
        return;
    }
    state.stage = Stage::Column {
        object,
        column: column.to_string(),
    };
    state.mode = DialogMode::Normal;
    state.notice = None;
    // The row list is now seven fields with the cursor on the first, so
    // the viewport goes with it — `enter_edit_stage`'s own reset.
    shell.object_dialog_scroll.scroll_to_item(0);
    cx.notify();
}

/// `escape` out of the column stage (Part 2c §5.2): fold, restore the
/// view's fields, and put the cursor back on the column's own row.
///
/// The mirror of [`enter_column_stage`], and the reason the escape
/// ladder's `PreviousStage` rung branches rather than the stage machine
/// growing a second ladder: from here "the previous stage" is the view's
/// own edit stage, not the browse list.
///
/// No mode is set, for [`leave_edit`]'s reason: this rung is reachable
/// only from normal mode with an empty query, so the keys are already on
/// the shell root.
///
/// **The Schema door re-derives the rows it returns to** (dataset-
/// presentation §4.7): a Schema column row carries the dataset overlay's
/// summary in its own text, and the stage the trader is leaving is what
/// changed that overlay — so the restored rows are `schema::fields` over
/// the pending-aware config, not the ones stashed on the way in, which
/// would keep painting the summary as it stood before the edit.
/// `Draft::reseed_fields` moves the baseline with them, so the restored
/// stage is not dirty and no `datasets` write is queued. Views needs none
/// of this: its member row is painted from the `ListItem` the fold has
/// already written through.
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
    scroll_to_cursor(shell);
    cx.notify();
}

/// A bare `1`–`9` on the Groupings dialog (§18.8): open that slot's edit
/// stage, from the browse list or from another slot's edit stage alike.
/// The slot number IS the object's name (`groupings.rs`'s own doc), so
/// the digit maps straight onto [`enter_edit_stage`] with no lookup — an
/// unfilled slot opens exactly as `enter` on its `empty` row would.
///
/// The one digit that does not jump is the open slot's own: re-entering
/// would rebuild the draft from `services.config`, which can still be a
/// debounce window behind the last tick (`apply`'s own module doc), so
/// the stage would visibly lose an edit that is in fact already queued.
/// Saying "already editing" is the honest answer, and it is a notice
/// rather than silence because a key that appears inert is the defect
/// class this interaction model exists to remove.
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

/// The edit stage's keys — **and the column stage's**, which shares this
/// whole table (Part 2c §5.2: it is the same draft with one column's
/// seven fields installed, so every verb here means what it already
/// meant). Two arms below read the projection rather than the stage:
/// `Commit`, which opens the column stage from a member row and gives
/// the ordinary notice everywhere else, and the `PreviousStage` rung,
/// which goes back to the view rather than to the browse list. `enter` is
/// therefore the only key whose behaviour the column stage changes, and
/// it changes it by looking at the row under the cursor.
///
/// In the one order they can be read in:
///
/// 1. an armed [`Confirm`] owns **every** keystroke until it is answered
///    (`enter`/`y`) or cancelled (`escape`/`n`). It replaces the action
///    bar rather than adding a row, so nothing above it moves;
/// 2. in [`DialogMode::Filter`] (§18.3, entered by `/` the same as
///    browse) `escape` leaves filter mode keeping the query, `enter` goes
///    through [`commit_selected_row`] exactly as normal mode's `Commit`
///    does — one meaning for one key, and the way a trader reaches one
///    column of a thirty-column view — navigation goes through
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
/// commits without revalidating — and it is safe because none of the four
/// `Domain::validate` implementations is order-sensitive: each renders the
/// object and hands it to its own loader (`ViewSpec::from_doc`,
/// `GroupingSlots::from_doc`, `saved_scopes_from_doc`, `SchemaSpec::
/// from_doc`), none of which has a diagnostic a reorder can produce or
/// resolve — and Schema's own `MoveItem` never reaches here at all, gated
/// out by the `writable()` check above with every other mutating verb, so
/// its validator's order-sensitivity is moot regardless. That is a
/// property of today's validators rather than of the dispatch table, so a
/// future order-sensitive one has to add the call; the branch is also the
/// only way a test can reach the commit gate with an injected diagnostic
/// still standing (`an_edit_the_reader_rejects_does_not_join_the_batch`
/// depends on exactly that).
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
        match dialog::ConfirmAnswer::from_key(ks) {
            Some(dialog::ConfirmAnswer::Yes) => {
                disarm_confirm(shell);
                run_confirmed(shell, confirm, cx);
            }
            Some(dialog::ConfirmAnswer::No) => disarm_confirm(shell),
            // Claimed and dropped: while a destructive question is on
            // screen, a stray letter must not act on the object behind it.
            None => {}
        }
        cx.notify();
        return true;
    }

    // ---- Text field (§19.1) --------------------------------------------
    //
    // Checked before filter mode, which it shares a focused `Input` with:
    // the field is open only in `Filter` (that is what gives it the keys),
    // and every key filter mode would claim means something else here.
    // The chain field (§18.8) is `handle_text_key`'s `completions: true`
    // case, not a separate dispatch.
    let text_entry = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .is_some_and(|draft| draft.text_entry.is_some());
    if text_entry {
        return handle_text_key(shell, ks, cx);
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
            // Exactly what normal mode's `Commit` does, so `enter` means
            // one thing in either mode — the browse stage's own rule
            // ("bare `enter` is claimed in both modes and opens the edit
            // stage on the selected row"). Until Part 2c there was
            // nothing here to open and both modes could only give the
            // notice; now a member row opens its column stage, and it
            // opens from the filtered list too — which is how a trader
            // reaches one column of a thirty-column view.
            commit_selected_row(shell, cx);
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
        // `tab`/`shift+tab` STEP the selected row here (user ruling
        // 2026-09-13), which is the settings dialog's own "tab steps in
        // both modes" rule arriving at this stage: exactly the path
        // `space`/`shift+space` take in normal mode, so a step is a step
        // whichever mode the trader is in. The `Input` keeps focus and
        // the query is untouched — `dialog::sync_dialog_text` writes the
        // unchanged `effective_query` back on this handler's return —
        // because this is a value change, not a filter keystroke. `h`
        // and `l` are NOT claimed: they are letters on their way to the
        // field, which a trader typing `hidden` depends on.
        //
        // An open text field never reaches here: the `text_entry` branch
        // above claims every key first, so the chain field's `tab` still
        // completes a segment (§18.8) and a plain field's stays inert.
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
            // §19.4's gate, which the normal-mode arms get from the
            // `writable` check below: a read-only domain refuses every
            // key that would change the object, and reaching this one
            // through filter mode must not be the way around it.
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
                //
                // Part 2c §5.2: from the column stage this rung goes back
                // one stage, not all the way out — to the view whose
                // fields `leave_column_stage` restores, cursor on the
                // column just edited.
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

    // §19.4: every verb that would change the object is refused here, in
    // one place, on a `Domain::writable() == false` surface (Schema is
    // the one today) — rather than by each arm below remembering to
    // check. `Nav`, `EnterFilter`, `Commit` and a bare unbound letter all
    // stay live: reading and filtering are exactly what a read-only
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
                let draft = state.draft.as_mut()?;
                draft.selected = vimnav::apply(draft.selected, draft.visible_rows().len(), nav);
                Some(draft.selected)
            });
            if let Some(selected) = selected {
                shell.object_dialog_scroll.scroll_to_item(selected);
            }
        }
        // `space`, `l` and `tab` forward; `shift+space`, `h` and
        // `shift+tab` back (user ruling 2026-09-13) — the aliases arrive
        // already resolved from `dialogmode::normal_command`, so there
        // is one arm per direction rather than one per spelling. The
        // notice names `space`/`shift+space` whichever alias was
        // pressed: naming the alias would need the keystroke down here,
        // and the two canonical keys are the ones the footer teaches.
        NormalCommand::Toggle => step_selected_row(shell, true, false, cx),
        NormalCommand::ToggleBack => step_selected_row(shell, false, false, cx),
        NormalCommand::MoveItem(delta) if in_column_stage(shell) => {
            // Part 2c §5.2: there is no list in this stage to reorder, so
            // the ordinary "that is as far as this row goes" would answer
            // about rows that are not on screen. Both directions get the
            // same sentence, with the key they actually pressed in it.
            let key = if delta < 0 { "shift+k" } else { "shift+j" };
            not_a_column_verb(shell, key);
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
        // §18.2: take the column under the cursor out of the view.
        // Views-only by what `Draft::remove_selected` itself decides —
        // whether the field's list has an available catalogue at all
        // (`Some`, even if empty), never its `dest` and never a scan of
        // the list's contents; see its own doc — not by a check here, so
        // its two `Refused`
        // reasons are routed straight to the footer rather than through
        // `refuse_step`: that helper's `d`/`r` hint is for the "must keep
        // at least one entry" refusal `space` can also produce, and
        // neither of `x`'s own reasons is asking for either verb.
        // No `scroll_to_cursor` here, unlike the `space` arms: a removal
        // leaves the cursor at its own visible index (or one above it),
        // which was on screen before the keystroke and so still is — the
        // demoted row is the one that travels, and the cursor no longer
        // travels with it (`Draft::remove_selected`'s own comment).
        // Part 2c §5.2: same reasoning as `MoveItem`'s own column-stage
        // arm just above — `x` demotes a column into the catalogue, and
        // neither list is on screen here, so the notice names the stage
        // rather than a list the trader cannot see.
        NormalCommand::Verb('x') if in_column_stage(shell) => not_a_column_verb(shell, "x"),
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
            // §18.3: the same switch browse's own `/` throws, and the
            // same pure mutation — `dialog::sync_dialog_text` gives the
            // filter focus on this handler's return.
            if let Some(state) = shell.object_dialog.as_mut() {
                state.mode = DialogMode::Filter;
            }
        }
        // `enter` says whether `space` would do anything here
        // (`edit_commit_notice`); `i` opens a field (`open_field`, the
        // one door the `i` button takes too — spec §20.3).
        NormalCommand::EditText => open_field(shell),
        NormalCommand::Commit => commit_selected_row(shell, cx),
        // §18.8: from one slot's edit stage a digit jumps straight to
        // another's. On any other domain it is named like an unbound
        // letter would be — the edit stage's rule for a key that did
        // nothing.
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

/// `enter` in the edit stage, from either mode — the one door both
/// spellings go through, so the two cannot drift about what `enter`
/// means (the browse stage's own rule for the same key).
///
/// Part 2c §5.2 and dataset-presentation §4.1: on one of the view's OWN
/// column rows, or on a Schema column row, it opens that column's
/// presentation; anywhere else it is [`edit_commit_notice`]'s answer.
fn commit_selected_row(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    match column_stage_target(shell) {
        Some(name) => enter_column_stage(shell, &name, cx),
        None => edit_commit_notice(shell),
    }
}

/// The column the row under the cursor would open a stage for, if any —
/// the one answer [`commit_selected_row`] and [`on_edit_row_clicked`]
/// both read, so `enter` and a click cannot disagree about which rows are
/// doors (4c §18.9's mouse-parity rule).
///
/// Gated on three things and **no stage check**: the domain (only Views
/// and Schema have a column stage), that no column stage is open already
/// (neither door's stage installs a row this function would name, so it
/// cannot fire twice — reading it off the draft rather than the stage is
/// what keeps the two from ever disagreeing), and the shape of the row.
///
/// The two domains name a column differently, and each names it the way
/// its own rows are built:
///
/// * Views — an `EditRow::Item`, whose label IS its column name
///   (`Draft::row_label`). Never an `EditRow::Available`, which is a
///   column the view does not have and so has no presentation to edit
///   (§5.2 keeps 2b's notice there);
/// * Schema — an `EditRow::Field` keyed `columns.<col>` (`schema::fields`),
///   which is also exactly what `Draft::enter_column`'s membership check
///   recognises. A `derived.<name>` row is not a dataset column at all,
///   so it falls through to the read-only notice the rest of that dialog
///   gives.
fn column_stage_target(shell: &ShellView) -> Option<String> {
    let state = shell.object_dialog.as_ref()?;
    let draft = state.draft.as_ref()?;
    if draft.column().is_some() {
        return None;
    }
    match (state.domain, draft.selected_row()?) {
        (Domain::Views, row @ EditRow::Item { .. }) => Some(draft.row_label(row)),
        (Domain::Schema, EditRow::Field(i)) => draft
            .fields
            .get(i)?
            .key
            .strip_prefix("columns.")
            .map(str::to_string),
        _ => None,
    }
}

/// `enter`'s answer for a row with nothing to open
/// ([`commit_selected_row`]'s fallback, in either mode), and `i`'s for a
/// row it cannot open ([`open_text_field`]): name the verb that DOES
/// change the selected row, or say the row has none.
///
/// Three answers, because there are three kinds of row here: `space` for
/// a choice, a bool or a list entry; `i` for a `Number` or a `Text` the
/// domain marks editable (`Domain::text_editable`); and "read-only" for
/// the display-only `Text`s (Groupings' `slot`, Scopes' two summaries).
/// The `i` answer arrived with the column stage's `label` and `width`
/// (Part 2c §5.3), which are the first Views rows `i` can open — before
/// them, every editable `Text` in this crate was a Sources row, where
/// `enter` gave the read-only wording about a row `i` opens perfectly
/// well. That was the two verbs disagreeing about the same row, which is
/// exactly what [`open_text_field`]'s own doc says they must not do.
fn edit_commit_notice(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let (domain, stage) = (state.domain, state.stage.clone());
    if !domain.writable(&stage) {
        // §19.4: agree with `i` and every other verb's refusal on a
        // read-only domain rather than falling back to the ordinary
        // "this row is read-only" wording, which names the ROW, not the
        // whole surface, and would read as a truth about this one field
        // that a writable neighbour lacks.
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

/// `i`, from the key and from the action bar's button alike (spec
/// §20.3 — one door, so the two can never open different things). §19.1:
/// a value field on a `Number` or an editable `Text` row
/// (`open_text_field`), or that function's own notice when the row has
/// none. Groupings is the one domain where `i` reaches past a single row
/// to the slot's whole object (§18.8) — the typed line is the fast way to
/// set a chain, entered from the chooser the slot opens in (user ruling
/// 2026-09-14) — so it is checked first and given its own path through
/// `begin_chain_entry` rather than `Draft::begin_text_entry`.
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
        state.mode = DialogMode::Filter;
    }
    // The row list just became the (shorter) completion list with the
    // cursor on row 0; the viewport follows.
    shell.object_dialog_scroll.scroll_to_item(0);
}

/// `i` off Groupings (§19.1): open the value field on the selected row,
/// or say why not. A `Number` is always typeable; a `Text` only where
/// the domain says so (`Domain::text_editable`) — a display-only `Text`
/// gets the read-only notice `enter` gives, so the two verbs agree about
/// the same row. Any other row gets `edit_commit_notice`'s answer.
fn open_text_field(shell: &mut ShellView) {
    let Some(state) = shell.object_dialog.as_ref() else {
        return;
    };
    let domain = state.domain;
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    let editable = match draft.selected_row() {
        Some(EditRow::Field(i)) => match &draft.fields[i].kind {
            FieldKind::Number { .. } => true,
            FieldKind::Text(_) => domain.text_editable(&draft.fields[i].key),
            _ => false,
        },
        _ => false,
    };
    if !editable {
        edit_commit_notice(shell);
        return;
    }
    if let Some(state) = shell.object_dialog.as_mut()
        && let Some(draft) = state.draft.as_mut()
        && draft.begin_text_entry() == Step::Changed
    {
        state.mode = DialogMode::Filter;
    }
}

/// The value field's keys (§19.1), while one is open: `escape` closes it
/// with nothing applied; `enter` applies the typed text — a `Number`
/// parses and range-checks in [`Draft::apply_text_entry`] itself, a
/// `Text` goes through the domain's `parse_text` — refusing with the
/// field left open, or closing it, and a `Step::Changed` then rides
/// exactly the path a tick does, [`revalidate`] and [`commit_change`],
/// so a desk field forks and says so; everything else is the focused
/// `Input`'s to type (`false`).
///
/// The chain field (§18.8, Groupings' `i`) is this same field with
/// `completions: true`: `tab` and the nav keys only mean anything there
/// — a plain field has no completion list below it to move a highlight
/// through, so those two branches are gated on `completions` and `enter`
/// dispatches to [`Draft::apply_chain`] instead of
/// [`Draft::apply_text_entry`].
///
/// Closing the field — cancel, apply, or an inert apply — is a pure
/// mutation of `text_entry`, `query` and the mode; `dialog::
/// sync_dialog_text` empties and blurs the shared `Input` from those on
/// this handler's return (spec §16.1), the same way every other
/// transition in this dialog is settled.
fn handle_text_key(shell: &mut ShellView, ks: &Keystroke, cx: &mut Context<ShellView>) -> bool {
    let completions = draft_mut(shell).is_some_and(|d| d.chain_entry());
    // The chain field's list changes length on every transition out of
    // it (the completions give way to the full row list) with the
    // cursor put back on row 0, so the viewport follows — the
    // `ClearQuery` rung's own reasoning. A plain field's row list is the
    // same length and order throughout (`Draft::visible_rows`, §19.1),
    // and `Draft::cancel_text_entry` leaves `selected` on the row that
    // was open, so the viewport should follow the CURSOR there, the same
    // way applying does, not jump to row 0.
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

/// Step the selected row and record it — the ONE path every stepping
/// key takes: `space`/`shift+space` and their 2026-09-13 aliases
/// `l`/`h`/`tab`/`shift+tab` in normal mode, and `tab`/`shift+tab` in
/// filter mode. Factored out when the filter-mode spelling arrived,
/// because two copies of this five-line sequence is exactly the drift
/// this crate keeps paying for (`Draft::toggle_selected_back`'s own doc
/// records the same lesson one layer down).
///
/// `filtering` exists for the `Step::Inert` notice alone, which has to
/// name a key the trader can actually press *here*: `space` types in
/// filter mode, so "nothing on this row changes with space" would be a
/// sentence about a key that puts a character in the query. Each mode
/// gets the pair its own footer teaches — the aliases (`l`, `h`) are
/// deliberately never named, since a notice about a key the footer did
/// not advertise explains nothing.
///
/// The caller has already decided the row is writable.
fn step_selected_row(
    shell: &mut ShellView,
    forward: bool,
    filtering: bool,
    cx: &mut Context<ShellView>,
) {
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
    // Folded, not `services.config` alone: `views::dataset_catalogue`
    // seeds each catalogue row's presentation from
    // `dataset_presentation.toml` (§5.4), so inside the 250 ms write
    // debounce — a Schema column-stage edit, escape out, open this
    // dialog, step this row — the plain read is the overlay as it stood
    // BEFORE the last keystroke, and a column promoted off that stale
    // catalogue carries the stale layer into the writer's comparison.
    // Every other read of this layer on this branch is folded (the
    // final whole-branch review's Minor 4).
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

/// Is the column stage open (Part 2c §5.2)? Read off the DRAFT, never
/// the stage, for [`commit_selected_row`]'s reason: the projection is
/// what the verbs below actually act on, so asking the thing that carries
/// it keeps the two from ever disagreeing.
fn in_column_stage(shell: &ShellView) -> bool {
    shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .is_some_and(|draft| draft.column().is_some())
}

/// The one answer for a verb the column stage does not own: `x`,
/// `shift+j` and `shift+k` all reorder or demote rows of a list this
/// stage does not install, and `d`/`r` (the final review's I-2) act on
/// the whole view the crumb has narrowed away from — so each says the
/// same thing with its own key in it, rather than the edit stage's
/// answer about an object or rows that are not on screen.
fn not_a_column_verb(shell: &mut ShellView, key: &str) {
    set_notice(shell, format!("{key} is not a verb in a column's stage"));
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

/// Record the change the keystroke just made.
///
/// The one door every field edit leaves through, so there is one place a
/// change joins the pending batch and one place a fork is announced.
/// [`super::apply::commit_edit`] does the recording; the merge, the
/// application and the file write happen together on the debounce behind
/// it. The trader sees the change immediately regardless — the row under
/// the cursor is painted from the draft this function was called after
/// mutating.
///
/// A change that forks the object into the user layer (spec §4.1) is
/// applied exactly like one that does not, and *said* rather than asked
/// (user ruling 2026-09-14): the notice names the copy and the `r` that
/// undoes it. `would_fork` is read before `commit_edit` moves the
/// baseline, because it is answered from the draft's pending writes,
/// which the commit empties; and a refused commit (`commit_edit`'s own
/// `blocking_diagnostic` gate, or a shell with nowhere to write) reports
/// its refusal instead, since nothing was copied.
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
    // 2c §5.2: the column stage is a projection; fold it into the item
    // FIRST so the validator and the overlay writer see this keystroke.
    if draft.column().is_some() {
        fold = draft.fold_column();
    }
    // Validated, then stored: `validate` needs the draft immutably and
    // the config from a sibling field, which is exactly the disjoint
    // borrow the compiler allows here and a `&mut self` method would not.
    let diagnostics = domain.validate(draft, &shell.services.config);
    if let Some(draft) = draft_mut(shell) {
        draft.diagnostics = diagnostics;
    }
    // §5.3's clear verb said out loud. The field under the cursor has
    // just been re-seeded with the desk's own value (`Draft::fold_column`),
    // so without this the trader would watch what they typed be replaced
    // by something else with no explanation — a screen that appears to
    // have ignored the keystroke rather than one that honoured it
    // exactly. Set here, not in the arms: every path that changes a value
    // comes through this function, and only this function knows the fold
    // happened.
    //
    // The layer is named rather than assumed (dataset-presentation spec
    // §5.2): a cleared view key meets the dataset level before the desk,
    // and a cleared DATASET key falls to whatever each view says. A fold
    // with nothing below it (`to: None`) says nothing at all — there is
    // no layer to name and the field simply went back to the kind
    // default, which is on screen already.
    if let Some(Fold { key, to: Some(to) }) = fold {
        let layer = match to {
            FellTo::Desk => "the desk",
            FellTo::Dataset => "the dataset",
            FellTo::EachView => "each view",
        };
        set_notice(shell, format!("{key} follows {layer} again"));
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
        Some(Stage::Edit { object } | Stage::Column { object, .. }) => object.clone(),
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
        // The column stage's verbs act on the OBJECT, not the column
        // (Part 2c §5.2): `d`, `r` and `o` are the view's, and the row
        // they are gated by is the view's row.
        Some(Stage::Edit { object } | Stage::Column { object, .. }) => object.clone(),
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
///
/// Also removes `doc.object`'s `overrides.toml` entry, if any (§19.6):
/// same gate — a missing sidecar is never created just to remove nothing
/// from it — and the same batch, so a fork's drift record never outlives
/// the fork it describes.
fn removal_edits(
    shell: &ShellView,
    docs: &[&'static str],
) -> Result<Vec<(&'static str, String)>, String> {
    let name = match shell.object_dialog.as_ref().map(|state| &state.stage) {
        // Same rule `editing_row` states: a removal armed from the column
        // stage removes the OBJECT, which is what `d`/`r` mean there too.
        Some(Stage::Edit { object } | Stage::Column { object, .. }) => object.clone(),
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
    let mut keys: Vec<(&'static str, String)> =
        touched.into_iter().map(|doc| (doc, name.clone())).collect();
    // §19.6: the sidecar entry rides the same removal — never created
    // just to remove nothing, hence the `has_override_entry` gate rather
    // than an unconditional key.
    if let Some(domain) = shell.object_dialog.as_ref().map(|state| state.domain) {
        let okey = super::override_key(domain.doc(), &name);
        if super::has_override_entry(&shell.services.config, domain.doc(), &name) {
            keys.push((super::OVERRIDES_DOC, okey));
        }
    }
    Ok(keys)
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
    // Part 2c final review, I-2: refused in the column stage, through
    // the very notice `x`/`shift+j`/`shift+k` already answer with. The
    // crumb has narrowed the object to one column, and `d`'s confirmed
    // effect is on the whole view — it deletes the user-layer view
    // outright. A destructive verb must not answer about an object the
    // trader has navigated away from, which is the same rule the three
    // list verbs were refused under; leaving these two live where those
    // three were refused is the asymmetry that reads as an oversight.
    // The cost is one keystroke: `escape` first, then `d`.
    //
    // Guarded here rather than at the dispatch arm (where `x`'s own
    // guard sits) because this function has two callers — the `d`
    // keystroke and `press_verb`'s action-bar click (§18.9 made the bar
    // the mouse form of these letters), and a guard on only the keyboard
    // one would leave the stage destructible with a mouse, exactly the
    // Part 2b review Major that `Domain::writable`'s ten sites answer.
    if in_column_stage(shell) {
        not_a_column_verb(shell, "d");
        return;
    }
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
    // Part 2c final review, I-2 — `arm_delete`'s guard, for the same
    // reason and with the same reach over both callers. `r`'s confirmed
    // effect is a removal across `views` AND `view_presentation`, so
    // from a stage crumbed `tree > npv` it would throw away the
    // trader's personalisation of every column of the view, not the one
    // the crumb names.
    if in_column_stage(shell) {
        not_a_column_verb(shell, "r");
        return;
    }
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
/// **Whether `o` asks first is decided by who owns the scope, not by how
/// destructive it is.** On a scope the user layer already owns the
/// previous selection is lost for good, so it arms [`Confirm::Overwrite`].
/// On a desk- or builtin-owned scope nothing is lost — the desk's copy is
/// still there and `r` restores it — and the write lands through
/// `apply::commit_edit` exactly like any other definitional edit, which
/// forks it into the user layer (spec §4.1); by the 2026-09-14 ruling a
/// fork is announced, not asked about, so that case runs at once
/// ([`run_overwrite`]) and its notice says both what was replaced and
/// what was copied. `editing_row` (already `arm_delete`'s and
/// `arm_revert`'s own test for "does the user layer own this") answers
/// ownership directly; `apply::would_fork` cannot be used here, because
/// it reads `draft.writes_by_destination()`, which is still empty before
/// `overwrite_with` has run, so it would always say `false`.
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
    let forks = editing_row(shell).is_some_and(|row| row.layer != Some(Layer::User));
    if !forks {
        if let Some(draft) = draft_mut(shell) {
            draft.confirm = Some(Confirm::Overwrite);
        }
        return;
    }
    let notice = super::apply::fork_notice(shell, Domain::Scopes);
    if run_overwrite(shell, cx) {
        set_notice(shell, format!("replaced with the frame's scope; {notice}"));
    }
}

/// Overwrite the open scope with the frame's current one, through the
/// one commit door. `true` when a change was queued; `false` when there
/// was nothing to write or the commit refused, with the notice already
/// set to say which.
///
/// `shell.frame` (an `Entity<Frame>`) is read here, at the gpui call
/// site, precisely so `Domain`'s pure core never has to know a `Frame`
/// exists — the only place a `Frame` is read for this dialog at all (an
/// `Entity<InputState>` is read elsewhere, e.g. the shared filter field,
/// so this is narrower than "the only entity"). Re-checks the domain
/// rather than trusting its callers' own gates — the same "the gate
/// lives in the acting function, not just the one that arms it" rule
/// `commit_edit`'s own `blocking_diagnostic` check follows. The new
/// value replaces the draft's source and fields (`scopes::overwrite_with`),
/// which is what makes `commit_edit` see a change to write; it goes
/// through that same door rather than a direct `config_write` call, so a
/// failed write still reverts the way any other edit's does, and
/// `revalidate` runs first so the diagnostics on screen describe the new
/// content rather than the old. `commit_edit` answers `None` both for
/// "queued" and for "nothing changed", so the no-op case is identified
/// here instead: a saved scope that already equals the frame's — the
/// ordinary state straight after `:scope load` — would otherwise answer
/// a deliberate keystroke with no write, no config change and nothing on
/// screen. A verb that does visibly nothing is the defect class this
/// interaction model exists to remove.
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
    let changed = draft_mut(shell).is_some_and(|draft| {
        scopes::overwrite_with(draft, &scope);
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
fn run_confirmed(shell: &mut ShellView, confirm: Confirm, cx: &mut Context<ShellView>) {
    let domain = match shell.object_dialog.as_ref() {
        Some(state) => state.domain,
        None => return,
    };
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
                    // §19.6: named in the notice only when `removal_edits`
                    // actually found an entry to remove — `keys` decides,
                    // same as every other doc in this list.
                    docs.push(super::OVERRIDES_DOC);
                    // Preserves `docs`' own order rather than whatever
                    // order `keys` happens to hold, so a notice naming
                    // both files reads "views and view_presentation" the
                    // way it always has.
                    let name = match shell.object_dialog.as_ref().map(|state| &state.stage) {
                        Some(Stage::Edit { object } | Stage::Column { object, .. }) => {
                            object.clone()
                        }
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
/// personal override away.
///
/// An earlier build put `Save changes` here whenever the draft was dirty,
/// relabelled `Copy to user layer` when saving would fork; a later one
/// kept the fork as a confirm. Neither survives: the save, because a
/// dirty draft never means "there is something a save key could do from
/// here" — [`Draft::is_dirty`](super::Draft::is_dirty) enumerates the two
/// ways it can be true, and in each one the value is either already
/// recorded or refused by a diagnostic a save key could not get past
/// either — and the fork confirm because it was more distracting than
/// the fork it disclosed (user ruling 2026-09-14); the fork is announced
/// in the notice instead.
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
    let Some(draft) = state.draft.as_ref() else {
        return Vec::new();
    };
    if !state.domain.writable(&state.stage) {
        return Vec::new();
    }
    // None of the three destructive verbs is a verb in a column's stage
    // — `d`, `r` and `o` all refuse through `in_column_stage`/
    // `arm_overwrite` — so BOTH doors paint none of them rather than
    // advertising a button that can only answer "not a verb in a
    // column's stage". The Views door has always offered `Delete this
    // view` there; on the Schema door, whose whole promise is
    // "read-only", offering `Delete this dataset` read worse still (the
    // final whole-branch review's Minor 6). `i` is the exception (spec
    // §20.3): the stage's `label` and `width` rows are exactly the rows
    // it opens, so it is decided per row below, on either stage.
    let in_column = draft.column().is_some();
    let row = editing_row(shell);
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
    // Spec §20.3: `i` is a button wherever the selected row is one it
    // opens — the footer's own test (`RowVocabulary`), plus Groupings'
    // whole-chain `i` (§18.8) which is live on every row there. Never
    // while a text field is open, though `build_edit` withdraws the whole
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
    // The column stage paints the edit stage's own chrome (Part 2c §5.2)
    // — header, filter row, row list, action bar — over the seven fields
    // it installed; only the crumb tells them apart.
    if matches!(state.stage, Stage::Edit { .. } | Stage::Column { .. }) {
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

    // §6.1: the merged `colours.toml` AND the theme's own
    // anchors/tokens, read once for the whole list rather than once per
    // row — every browse row's swatch resolves its own saved colour
    // against them. `None` for every other domain, so a non-Colours
    // dialog never even asks `Config` for a doc it will never read the
    // rest of the row loop for.
    //
    // The pair is hoisted with the doc (the final review's M-8):
    // `resolve_named` (since deleted — this hoist left it with no caller) read the theme inside itself, so
    // resolving per row cost M x 28 `Hsla -> Rgb` conversions for a list
    // of M colours. Bounded by colour count in a modal rather than by row
    // count on the paint path, so it is tidiness rather than budget — but
    // it is the same shape the blotter's I-1 memo answers, and the list
    // was already hoisting the doc.
    let named_colours: Option<(
        geode_core::colour::NamedColours,
        geode_core::colour::Anchors,
        geode_core::colour::Tokens,
    )> = (state.domain == Domain::Colours).then(|| {
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

        let display = row.display_name();
        let name_len = display.chars().count();
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

        // §19.3: a prefixed row paints `<prefix> · ` dimmed and the name
        // after it, as two runs of one highlighted label — the indices
        // are split at the prefix's end so a hit inside the dataset still
        // highlights there. `cut` is the prefix run's length in the
        // painted `display` text (`"{prefix} · "`, three chars for the
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
                                theme.primary,
                            )),
                    )
                    .child(highlighted_text(&row.name, &in_name, theme.primary))
                    .into_any_element()
            }
            None => highlighted_text(&row.name, &name_ix, theme.primary),
        };

        let label = v_flex().gap_0p5().child(head).child(
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
        // §19.6: real once `derive_rows` has a sidecar entry to compare
        // against (`ObjectRow::drifted`'s own doc has the full rule) —
        // this row simply paints whatever it is handed.
        if row.drifted {
            markers = markers.child(dialog::badge(
                "drifted",
                theme.muted_foreground,
                theme.border,
                Some(format!("objectdialog-drifted-{}", row.name)),
                cx,
            ));
        }

        // §6.1: a swatch before the label, resolved from this row's own
        // saved colour — painted only when the colour actually resolves
        // (a dropped or invalid one paints no swatch, never a fallback
        // that would misrepresent it).
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

    // §18.2: the naming stage replaces the filter row with the name field
    // and states its own two-verb vocabulary — never the browse footer's,
    // even though `begin_naming` leaves `state.mode` at `Filter` (the
    // `Input` really does own the keys) — a footer offering `/` or `j`/`k`
    // while the name field is focused would be advertising keys the
    // field, not this handler, would consume.
    let naming = matches!(state.stage, Stage::Naming);

    // The hint rows state the CURRENT mode's vocabulary, never the union
    // of both: a modal surface's whole risk is a user who cannot tell
    // which mode they are in, and a footer listing keys that are inert
    // right now is exactly the lie the mode pill exists to prevent. Which
    // row a hint paints on is `crate::footer`'s call (move / edit / go,
    // spec §19), never this stage's. `enter` opens the selected row's
    // edit stage in BOTH modes (the browse rule) and carries the
    // `objectdialog-hint-enter` selector so a test can find it, like the
    // edit footer's `i`.
    let hints: Vec<Hint> = if naming {
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
                // `n` is not on the footer at all for a domain whose
                // roster is fixed (Groupings) — advertising a key that
                // only ever says "the slots are fixed" teaches a verb
                // with nothing behind it — nor for a read-only domain
                // (Schema, §19.4), for the same reason: `n` there only
                // ever says the surface cannot be written to.
                if state.domain.writable(&state.stage) && state.domain.roster().is_none() {
                    hints.push(Hint::new(HintRow::Edit, &["n"], "new"));
                }
                // §18.8: a digit opens that slot — Groupings only, the
                // one domain whose objects are numbered.
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
                Hint::new(HintRow::Go, &["enter"], "open").selector("objectdialog-hint-enter"),
                Hint::new(HintRow::Go, &["escape"], "back to normal"),
            ],
        }
    };
    let hint_line = dialog::hint_rows(&hints, chip_fg, chip_bg);
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
        dialog::name_row(
            &shell.dialog_input,
            &format!("New {} · name", object_word(state.domain)),
            cx,
        )
    } else {
        dialog::filter_row(&shell.dialog_input, frozen_query, cx)
    };

    // Spec §20.3: `n` as a button, between the list and the footer —
    // the browse stage's own action bar, in the edit stage's place for
    // it (outside the list, so the rows never shift under it).
    let action_block = browse_action_bar(state, entity, cx);

    v_flex()
        .gap_2()
        .child(top_row)
        .child(list)
        .child(action_block)
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
    // §19.1: no verbs at all while a value field is open. The keyboard
    // cannot reach `d`/`r` there (every printable key is text), and a
    // CLICKED one would arm a confirm over a live, focused value field —
    // every keystroke then claimed and dropped with the caret still
    // blinking, and `y` leaving the field's `Filter` mode behind in
    // browse. Withdrawing the bar is what makes the mouse agree with
    // the keys. A `confirm` cannot be armed here for the same reason,
    // so that arm is unreachable with the field open.
    let action_block = match (draft.text_entry.is_some(), draft.confirm) {
        (true, _) => div().into_any_element(),
        (false, Some(confirm)) => confirm_row(confirm, &draft.name, entity, cx),
        (false, None) => action_bar(shell, entity, cx),
    };
    let theme = cx.theme();
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    let row = editing_row(shell);

    // §6.1: on Colours, the swatch beside the name — resolved from the
    // draft's own live fields (`colours::definition_of`), not from the
    // saved `colours.toml`, so stepping the hue repaints it before any
    // write lands. `None` (no swatch) only if the draft somehow lacks a
    // `hue` row, which `colours::fields` never produces.
    let name_child = match (state.domain, colours::definition_of(draft)) {
        (Domain::Colours, Some(def)) => {
            let anchors = colour_theme::anchors_from_theme(theme);
            let tokens = colour_theme::tokens_from_theme(theme);
            let hsla = colour_theme::to_hsla(geode_core::colour::resolve(&def, &anchors, &tokens));
            h_flex()
                .gap_2()
                .items_center()
                .child(dialog::swatch(
                    hsla,
                    "objectdialog-swatch-header".to_string(),
                    cx,
                ))
                .child(div().text_lg().child(draft.name.clone()))
                .into_any_element()
        }
        _ => div().text_lg().child(draft.name.clone()).into_any_element(),
    };

    // The object header: its name, and the same two provenance markers
    // the browse row carries, so opening an object never loses the
    // context the list gave it.
    let mut header = h_flex()
        .w(px(WIDTH))
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
            // §19.6: the same tokens the browse row's own `drifted` badge
            // uses — one classification, one set of colours.
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
    // §19.6: under the header, not on it — a badge says WHAT the row is,
    // this says what to DO about it, and only `r` (never `d`, which
    // deletes the whole override rather than restoring a shadow) does.
    let drift_note = row.as_ref().filter(|r| r.drifted).map(|_| {
        div()
            .text_xs()
            .text_color(theme.warning)
            .debug_selector(|| "objectdialog-drift-note".to_string())
            .child("the desk's copy has changed since you copied it — r restores it")
            .into_any_element()
    });

    let domain = state.domain;
    // §19.4 / dataset-presentation §4.2: whether a `doc`/`pres`/`dataset`
    // badge may be painted at all, read once per render rather than per
    // row — Schema answers `true` here only inside its column stage.
    let writable = domain.writable(&state.stage);
    let dest_badges = writable;
    // Spec §20.3: whether any chip may carry a handler this frame. The
    // four inert cases mirror the keys' own: read-only domain, armed
    // confirm, open text field (and per row, a one-option `Choice`).
    let chips_live = writable && draft.confirm.is_none() && draft.text_entry.is_none();
    let rows = draft.rows();
    let visible = draft.visible_rows();
    // §19.5: which rows a current diagnostic names, computed once per
    // render rather than per row — `Draft::flagged_rows` is a linear scan
    // of the diagnostic list, and doing it once here keeps the per-row
    // work below to a single `Vec` lookup.
    let flagged = draft.flagged_rows(domain.doc());
    // dataset-presentation §5.3: the column stage's layers, resolved once
    // per render. `None` off the stage — see `provenance_chip`.
    let provenance_inputs = draft
        .column_ctx
        .as_ref()
        .map(dataset_columns::ProvenanceInputs::new);
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
        // Every row carries the same 2px top border, transparent unless
        // `drag_over` recolours it — reserving the space up front means a
        // hover only repaints the colour, never reflows the rows below it.
        let mut element = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap_3()
            .px_2()
            .py_1()
            .rounded(px(4.))
            .border_t_2()
            .border_color(gpui::transparent_black());
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
                    Destination::DatasetPresentation => "dataset",
                };
                // Spec §20.3: the value is a chip — the mouse form of
                // `space`/`shift+space` — on exactly the rows the keys
                // step (`vocabulary_of`, the footer's own answer), and
                // plain text with no handler everywhere else, including
                // every row while `chips_live` is off. The handler runs
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
                    on_step,
                );
                (
                    format!("objectdialog-field-{}", field.key),
                    highlighted_text(&field.label, &m.indices, theme.primary),
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(value_chip)
                        // dataset-presentation §5.3: the layer in force
                        // on a column-stage field. The same slot as the
                        // layer badge below, and the two never both
                        // appear: Schema fills `layer` on its own rows
                        // (which are not column-stage rows), the column
                        // stage fills this.
                        .children(provenance_chip(
                            provenance_inputs.as_ref(),
                            field,
                            theme,
                            cx,
                        ))
                        // §19.4: the layer a schema row's value came from
                        // — `None` on every writable domain (`Field::
                        // layer`'s own doc has the reasoning).
                        .children(field.layer.map(|layer| {
                            dialog::badge(
                                layer.name(),
                                theme.muted_foreground,
                                theme.border,
                                Some(format!("objectdialog-field-layer-{}", field.key)),
                                cx,
                            )
                        }))
                        // A `doc`/`pres` badge promises a write this row
                        // can make — painting it on a read-only domain
                        // would promise one the scaffold refuses outright.
                        .children(dest_badges.then(|| {
                            dialog::badge(
                                dest_label,
                                theme.muted_foreground,
                                theme.border,
                                Some(format!("objectdialog-dest-{}", field.key)),
                                cx,
                            )
                        }))
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
                //
                // In the chain field (§18.8) the rows are completions, not
                // list members: §18.9.2 says such a row carries no tick or
                // grip at all, so both are withdrawn outright here rather
                // than painted as an inert placeholder — there is no
                // second list for a spacer to keep aligned with once the
                // whole column is gone. This branch only ever reaches an
                // item row (the tick/grip belong to the object's own
                // list, per this comment's opening line) — a plain value
                // field never opens on one (§19.1 only opens it on a
                // `Field` row) — but the withdrawal still reads "is any
                // field open" rather than "is the chain field open", so
                // a future item-level text field (a column's width, Part
                // 2c) withdraws the same way without a second condition
                // to remember.
                let grip_and_tick = if draft.text_entry.is_some() {
                    None
                } else {
                    let grip = if own {
                        div()
                            .text_color(theme.muted_foreground)
                            .w(px(11.))
                            .child("⋮")
                            .into_any_element()
                    } else {
                        div().w(px(11.)).into_any_element()
                    };
                    // §18.9.2: the tick is the toggle. Its mouse-down
                    // stops propagation so the row's own select does not
                    // double-fire; the handler moves the cursor here
                    // itself and then walks `space`'s path, so the mouse
                    // and the key cannot disagree.
                    let entity_for_tick = entity.clone();
                    let tick_position = position;
                    let tick_id = format!("objectdialog-tick-{}", entry.name);
                    let tick = div()
                        .id(gpui::SharedString::from(tick_id.clone()))
                        .font_family(crate::fonts::MONO)
                        .w(px(13.))
                        .text_color(if entry.included {
                            theme.success
                        } else {
                            theme.muted_foreground
                        })
                        .debug_selector(move || tick_id)
                        .cursor_pointer()
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
                name_row = name_row.child(highlighted_text(&entry.name, &m.indices, theme.primary));
                // The compact per-column summary (Part 2c §5.4) is painted
                // after the name, muted, on a member row only — an
                // available row's presentation is always the empty
                // default (nothing has ever overridden a column not yet
                // in the view), so `column_summary` would paint nothing
                // for one anyway, but `own` says so rather than relying
                // on that coincidence. It is deliberately part of the
                // NAME element, not `row_label` — `Draft::row_label`
                // stays the name alone, so the filter still matches only
                // what it always matched.
                if own {
                    let summary =
                        views::column_summary(&views::kind_default(entry), &entry.presentation);
                    if !summary.is_empty() {
                        name_row = name_row.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(summary),
                        );
                    }
                }
                let name = name_row.into_any_element();
                (
                    format!("objectdialog-item-{}", entry.name),
                    name,
                    div().into_any_element(),
                )
            }
        };
        // §19.5: a glyph before the label, painted only when a current
        // diagnostic names this row — an inert `div` of the same width
        // otherwise, so every row keeps its height and the label column
        // stays aligned whether or not anything is flagged. Built after
        // the match above (rather than folded into each arm) because the
        // selector string — this row's identity for `debug_selector` —
        // is computed there, and one glyph rule for both arms is simpler
        // than two copies of the same match on `flag`.
        let flag = flagged
            .iter()
            .find(|(r, _)| *r == edit_row)
            .map(|(_, s)| *s);
        let glyph = match flag {
            Some(Severity::Error) => {
                let diag_selector = format!("objectdialog-diag-{selector}");
                div()
                    .w(px(12.))
                    .text_color(theme.danger)
                    .debug_selector(move || diag_selector.clone())
                    .child("!")
                    .into_any_element()
            }
            Some(Severity::Warning) => {
                let diag_selector = format!("objectdialog-diag-{selector}");
                div()
                    .w(px(12.))
                    .text_color(theme.warning)
                    .debug_selector(move || diag_selector.clone())
                    .child("!")
                    .into_any_element()
            }
            None => div().w(px(12.)).into_any_element(),
        };
        let entity_for_row = entity.clone();
        let clicked = position;
        // §19.1: while a field is open, the mouse agrees with the keys —
        // a plain field owns the row list too (moving the cursor under
        // it would leave `TextEntry.row` pointing at a row the trader is
        // no longer on), so only the chain field's own completion click
        // does anything. `None` (no field open at all) is the ordinary
        // click-to-edit path.
        let open = draft.text_entry.map(|t| t.completions);
        // The glyph and label share ONE child so the row still has
        // exactly two children under `justify_between` — a third direct
        // child splits the row's free space into two gaps and floats the
        // label toward the middle of the row on every row of every
        // domain, flagged or not (review round 1's Important finding).
        // Carries its own selector so a window test can compare a
        // flagged row's label position against an unflagged one's —
        // there is otherwise no way to address just the label, since the
        // row's own selector spans the whole row (glyph, label and value
        // together) and would read the same width whichever child ate
        // the bug.
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
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                entity_for_row.update(cx, |shell, cx| match open {
                    Some(true) => on_completion_clicked(shell, clicked, window, cx),
                    Some(false) => {}
                    None => on_edit_row_clicked(shell, clicked, window, cx),
                });
            });
        // §18.9.1: a list row is both a drag source and a drop target;
        // a field row (`Dataset`, `Slot`, Scopes' two `Text` rows) is
        // neither — which is `Draft::row_drag` returning `None`, not a
        // second rule stated here — and while any value field is open
        // (§19.1) the rows carry no drag either, the same withdrawal
        // the tick and the action bar make there.
        //
        // The branch is built into an `AnyElement` on both sides because
        // `.id()` turns the `Div` into a `Stateful<Div>`: the two arms
        // have different types and only the erased form can be one
        // value. (`list.child(..)` below already takes an `AnyElement`,
        // so nothing downstream notices.)
        let row_el = match draft
            .text_entry
            .is_none()
            .then(|| draft.row_drag(edit_row))
            .flatten()
        {
            Some(payload) => {
                let entity_for_drop = entity.clone();
                // `target` is THIS row — the drop's destination. The
                // payload that arrives at `on_drop` is the dragged row's
                // own, built by whichever row started the gesture.
                let target = payload.clone();
                row_el
                    // The id carries everything that identifies the row,
                    // because gpui keys per-element state (the pending
                    // mouse-down a drag starts from) on it: the field, so
                    // two lists in one draft cannot collide; `own`, so a
                    // name cannot collide with itself across the two
                    // blocks (a demoted column keeps its name); and the
                    // name, which is unique within a block.
                    .id(gpui::SharedString::from(format!(
                        "objectdialog-drag-{}-{}-{}",
                        payload.field, payload.own, payload.name
                    )))
                    .cursor_grab()
                    // The ghost's name comes off the dragged value the
                    // constructor is handed, not a captured copy: a
                    // capture would clone a `String` per list row per
                    // frame for a ghost that exists only once a gesture
                    // actually starts.
                    .on_drag(payload, move |drag: &RowDrag, _offset, _window, cx| {
                        let name = gpui::SharedString::from(drag.name.clone());
                        cx.new(|_| DragGhost { name })
                    })
                    // Only this dialog's own payload: a tile drag or any
                    // other dragged value passing over the modal must not
                    // land on a column list.
                    .can_drop(|value, _window, _cx| value.downcast_ref::<RowDrag>().is_some())
                    // Only the colour changes here — the 2px top border
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

    // Diagnostics for the object as a whole, each prefixed with the row it
    // names when `Diagnostic::path` resolves to one (§19.5, §8.5) — the
    // same lookup `flagged_rows` above makes, but here for the LABEL a
    // diagnostic's own row carries rather than the glyph. An object-level
    // diagnostic (no matching row — `row_for_path` returns `None`, e.g. a
    // cross-dataset check with no single field to blame) prints with no
    // prefix at all, exactly as before this field existed.
    let diagnostics = v_flex().w(px(WIDTH)).gap_0p5().children(
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

    // User ruling 2026-09-13: the change group and `i` are computed from
    // the row under the CURSOR, not from the domain — see
    // [`RowVocabulary`]. Computed once, here, so the column stage and the
    // object stage cannot drift about what a row offers.
    let vocabulary = draft.selected_vocabulary(state.domain);
    // Can `i` open a field on this row? Groupings is the exception the
    // vocabulary cannot answer for: there `i` opens the slot's whole
    // chain (§18.8) rather than the selected row's own value, so it is
    // live on every row and that arm states it unconditionally below.
    let types = matches!(
        vocabulary,
        RowVocabulary::StepsAndTypes | RowVocabulary::Types
    );
    // `space` `shift+space` `tab` `h` `l` · change — the five spellings
    // `Draft::step_selected` answers to, on a row that has a value to
    // step. A list row takes the forward key alone, with the word that
    // says which way the row travels. `None` on a row nothing changes,
    // which is what makes the group droppable. It carries the
    // `objectdialog-hint-change` selector on its first chip, for the same
    // reason `i` carries one: whether it is painted at all is a per-row
    // decision, and a test has to be able to read it.
    //
    // In FILTER mode the group shrinks to `tab`/`shift+tab`: those are
    // the only two spellings that step there, and `space`, `h` and `l`
    // are characters on their way to the focused `Input` — naming them
    // would be this footer's own lie told the other way round, about
    // keys that type rather than keys that are dead.
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
    // `/` filter and `escape` with its honest rung — the same rule
    // browse's own footer keeps (§18.3): with a query still applied it
    // clears the query, and only then goes back.
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
    // The hint rows state this stage's vocabulary and only this stage's —
    // the same rule the browse footer keeps — and which row a hint paints
    // on is `crate::footer`'s call (move / edit / go, spec §19).
    // `enter` is named only while the SELECTED row opens a column stage —
    // the same test `commit_selected_row` makes — never by domain alone:
    // on Views' `dataset` row or a Schema derived row it only gives a
    // notice, and a chip there is the inert-key lie this footer exists to
    // avoid (review 2026-09-13). It carries `objectdialog-hint-enter`.
    let opens_column = column_stage_target(shell).is_some();
    let open_column =
        || Hint::new(HintRow::Go, &["enter"], "open column").selector("objectdialog-hint-enter");
    let hints: Vec<Hint> = if draft.confirm.is_some() {
        vec![
            Hint::prose(HintRow::Go, "this needs an answer first"),
            Hint::new(HintRow::Go, &["enter"], "go ahead"),
            Hint::new(HintRow::Go, &["escape"], "leave it alone"),
        ]
    } else if let Some(entry) = draft.text_entry {
        // §19.1: a value field's own vocabulary — never filter mode's,
        // even though the `Input` is focused the same way, because
        // `enter` means "apply this value" here rather than "narrow the
        // list". The chain field (§18.8) is `completions: true` and
        // additionally has `tab` to complete a segment and the nav keys
        // to move the highlight; a plain field has neither.
        let mut hints = Vec::new();
        if entry.completions {
            hints.push(Hint::prose(HintRow::Move, "type a chain · book / lhu"));
            hints.push(Hint::new(HintRow::Move, &["up", "down"], "move"));
            hints.push(Hint::new(HintRow::Go, &["tab"], "complete"));
        } else {
            hints.push(Hint::prose(HintRow::Move, "type a value"));
        }
        hints.push(Hint::new(HintRow::Go, &["enter"], "apply"));
        hints.push(Hint::new(HintRow::Go, &["escape"], "cancel"));
        hints
    } else if state.mode == DialogMode::Filter {
        // §18.3: the same filter-mode hints browse paints, since the
        // vocabulary — type to narrow, the shared nav keys, `escape` back
        // to normal — is identical in both stages. Plus, since
        // 2026-09-13, the one stepping pair that survives a focused
        // `Input` (`tab`/`shift+tab`), on a writable domain and a row
        // that has something to step; browse has no such row, which is
        // why only this copy of the hints grew it.
        let mut hints = filter_motion();
        if state.domain.writable(&state.stage) {
            hints.extend(change_hint(true));
        }
        hints.push(Hint::new(HintRow::Go, &["escape"], "back to normal"));
        hints
    } else if !state.domain.writable(&state.stage) {
        // §19.4: a read-only domain's normal-mode vocabulary is reading
        // and filtering alone — no `space`/`shift+space` to change a row,
        // no `shift+j`/`shift+k` to reorder, none of `d`/`r`/`x`/`n`/`o`,
        // since every one of those is refused by the gate above.
        let mut hints = vec![Hint::new(HintRow::Move, &["j", "k"], "move")];
        if opens_column {
            hints.push(open_column());
        }
        hints.extend(leave("back to the list".to_string()));
        hints
    } else if draft.column().is_some() {
        // Part 2c §5.2: the column stage's own vocabulary. No
        // `shift+j`/`shift+k` and no `x` — there is no list here to
        // reorder or demote from, and this footer's standing rule is to
        // name only the keys that act on THESE rows. Its seven rows are
        // exactly where the 2026-09-13 ruling bites: `label` and `width`
        // take `i` and nothing else, `scale` steps and `i` would only
        // refuse, `precision` does both — so both groups come from
        // `vocabulary` rather than being stated unconditionally the way
        // `i` was here before. `escape` names the object it goes back
        // to, since "back to the list" would be a lie about a rung that
        // stops at the view.
        let mut hints = vec![Hint::new(HintRow::Move, &["j", "k"], "move")];
        hints.extend(change_hint(false));
        if types {
            hints.push(
                Hint::new(HintRow::Edit, &["i"], "type a value").selector("objectdialog-hint-i"),
            );
        }
        hints.extend(leave(format!("back to {}", draft.name)));
        hints
    } else {
        // The change group is row-sensitive (2026-09-13) and can be
        // empty — Scopes' two display-only summaries, Groupings' `slot`,
        // a list's own header row. So is the reorder group after it:
        // `shift+j`/`shift+k` move a LIST ITEM, and on any other row
        // `Draft::move_item` answers "that is as far as this row goes",
        // which is the same inert-key class the change group's own gate
        // closed (review 2026-09-13). `x` keeps its extra Views-only
        // condition on top (§18.2 — see `mod.rs`'s
        // `Draft::remove_selected` doc): every other domain's `x` merely
        // refuses, so a chip there would teach a trader on Groupings or
        // Scopes a key that does nothing.
        let reorders = vocabulary == RowVocabulary::Item;
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
        // §18.8: Groupings' two extra verbs, advertised only where they
        // work — the same rule that keeps `n` off Groupings' browse
        // footer and `x` off every non-Views edit footer. §19.1's `i`,
        // elsewhere, is advertised only where the row UNDER THE CURSOR
        // can take it (user ruling 2026-09-13; it used to ask whether
        // the object had such a row anywhere, which put the chip on
        // Sources' `Dataset` row, a `Choice` `i` refuses).
        if state.domain == Domain::Groupings {
            hints.push(
                Hint::new(HintRow::Edit, &["i"], "type a chain").selector("objectdialog-hint-i"),
            );
            hints.push(Hint::range(HintRow::Go, "1", "9", "jump to slot"));
        } else if types {
            hints.push(
                Hint::new(HintRow::Edit, &["i"], "type a value").selector("objectdialog-hint-i"),
            );
        }
        if opens_column {
            hints.push(open_column());
        }
        hints.extend(leave("back to the list".to_string()));
        hints
    };
    let hint_line = dialog::hint_rows(&hints, chip_fg, chip_bg);
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
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(hint_line),
        );

    // The live `Input` renders only when it actually owns the keystrokes
    // — see this module's own "one switch" note, now also the edit
    // stage's rule (§18.3).
    // `slash_filters: true` for the same reason as browse's own call.
    let frozen_query = (state.mode == DialogMode::Normal).then_some(dialog::FrozenFilter {
        query: draft.query.as_str(),
        slash_filters: true,
        entity: entity.clone(),
    });
    // §19.1: while a value field is open it takes the filter row's
    // place — the same shared `Input`, labelled for what its text now
    // is, exactly as browse's naming stage swaps in `name_row`. The
    // chain field (§18.8) keeps its own slot-and-chain label; a plain
    // field names the object and the row it is editing.
    let filter = if let Some(entry) = draft.text_entry {
        let label = if entry.completions {
            format!("slot {} · chain", draft.name)
        } else {
            // `TextEntry.row`'s own doc anticipates an item-level field
            // (a column's width, Part 2c) as one more `EditRow` arm, not
            // a second mechanism — no adapter opens one today, but the
            // render thread must not assume that stays true. `Field`
            // still gets its field label; any other row falls back to
            // `Draft::row_label` rather than panicking.
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
        // None of Scopes, Schema, Sources or Colours has an `OrderedList`
        // field at all (`scopes.rs`'s, `schema.rs`'s, `sources.rs`'s and
        // `colours.rs`'s own module docs — every field on any of the
        // four is a plain scalar), so this arm is unreachable for all
        // four; kept only to stay exhaustive as domains are added.
        (Domain::Scopes | Domain::Schema | Domain::Sources | Domain::Colours, _) => ("", "members"),
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

/// The browse stage's one button (spec §20.3): `n`, where the domain can
/// create — the same two conditions the footer's `n` hint keys on
/// (writable, no fixed roster), and never while naming, where the row
/// it opens is already open. Same button shape as the edit stage's bar;
/// an empty `div` otherwise, so the stage's child order never changes.
///
/// The click takes [`begin_new_object`], the key's own door, and ends in
/// [`dialog::sync_dialog_text`] because it never passes through the key
/// path — `begin_naming` sets `DialogMode::Filter`, and the sync is what
/// focuses the name field (§17.1 rule 3).
fn browse_action_bar(
    state: &ObjectDialogState,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    let offers_n = !matches!(state.stage, Stage::Naming)
        && state.domain.writable(&state.stage)
        && state.domain.roster().is_none();
    if !offers_n {
        return div().into_any_element();
    }
    let theme = cx.theme();
    let ks = crate::keymap::parse_keystroke("n", Modifiers::NONE).expect("valid");
    let entity = entity.clone();
    let label = format!("New {}", object_word(state.domain));
    h_flex()
        .w(px(WIDTH))
        .gap_2()
        .items_center()
        .child(
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
                                .child(key_chip(&ks, theme.muted_foreground, theme.muted))
                                .child(label),
                        )
                        .on_click(move |_event, window, cx| {
                            entity.update(cx, |shell, cx| {
                                if let Some(state) = shell.object_dialog.as_mut()
                                    && state.notice.take().is_some()
                                {
                                    cx.notify();
                                }
                                // §19.3's seeding, read at CLICK time from
                                // the row under the cursor — the same two
                                // values `handle_browse_key` computes at
                                // keystroke time, and here for the same
                                // reason plus one: reading them at paint
                                // would derive the Sources rows a second
                                // time on every frame the bar is up.
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
        )
        .into_any_element()
}

/// The confirm block, which **replaces** the action bar rather than
/// joining it — `dialog::confirm_row` (spec §20.1) with this dialog's
/// question, verb and handlers. The yes handler's `run_confirmed`
/// delete/revert arm walks all the way back to browse through
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
    let on_yes: dialog::ConfirmHandler = Rc::new(|shell, _window, cx| {
        let armed = shell
            .object_dialog
            .as_ref()
            .and_then(|state| state.draft.as_ref())
            .and_then(|draft| draft.confirm);
        if let Some(confirm) = armed {
            disarm_confirm(shell);
            run_confirmed(shell, confirm, cx);
        }
    });
    let on_no: dialog::ConfirmHandler = Rc::new(|shell, _window, _cx| disarm_confirm(shell));
    dialog::confirm_row(
        confirm.prompt(name),
        yes_label,
        "objectdialog",
        entity,
        on_yes,
        on_no,
        cx,
    )
}

/// §5.3: the chip naming the layer in force on a column-stage field,
/// computed from the draft's layers at paint so a stepped field reads
/// `view` (or `dataset`, from the Schema door) on the same frame.
///
/// `None` off the column stage, where `Draft::column_ctx` is `None` and
/// there are no layers to name — which is why `inputs` is an `Option`
/// the caller builds ONCE before the row loop rather than a value this
/// function derives per field: both halves of
/// [`dataset_columns::ProvenanceInputs`] allocate, and seven clones per
/// painted frame is per-frame heap churn on the render thread.
fn provenance_chip(
    inputs: Option<&dataset_columns::ProvenanceInputs<'_>>,
    field: &Field,
    theme: &gpui_component::Theme,
    cx: &App,
) -> Option<AnyElement> {
    let provenance = dataset_columns::provenance_of(inputs?, field)?;
    Some(dialog::badge(
        provenance.name(),
        theme.muted_foreground,
        theme.border,
        Some(format!("objectdialog-field-provenance-{}", field.key)),
        cx,
    ))
}

/// A verb pressed with the mouse instead of the keyboard. One door, so a
/// button and its letter can never do different things.
///
/// Ends in [`dialog::sync_dialog_text`] like every other mouse handler
/// that mutates the dialog (§17.1 rule 3). Until spec §20.3 this was
/// §16.6's one audited exception — `d`/`r`/`o` only ever arm a confirm,
/// which moves neither mode nor query — but `i` opens a field
/// ([`open_field`]), which sets `DialogMode::Filter` and needs the sync
/// to focus the `Input`; so the sync runs after every verb now, and on
/// the three arming verbs it costs one comparison and moves nothing.
/// (The read-only refusal above the match posts a notice and nothing
/// the sync reads, so it returns without one.)
fn press_verb(shell: &mut ShellView, key: &str, window: &mut Window, cx: &mut Context<ShellView>) {
    if let Some(state) = shell.object_dialog.as_mut()
        && state.notice.take().is_some()
    {
        cx.notify();
    }
    // §19.4: belt-and-braces — `actions()` already paints an empty bar on
    // a read-only domain, so this button is unreachable by the mouse in
    // practice, but a test (or a future caller) can still call this door
    // directly, and it must refuse exactly as the keyboard does.
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

/// A click on an edit-stage row moves the draft's cursor there — the
/// mouse's half of `j`/`k` — and, on a row `enter` would OPEN, opens it.
///
/// That last part is 4c §18.9's mouse-parity rule applied to the column
/// stage's door (dataset-presentation §4.1): browse's own row click has
/// opened an edit stage since §18.9, and a member row that only moved the
/// cursor was the 2c ledger's standing minor — the one row in this dialog
/// whose `enter` did something a click would not. Both now go through
/// [`column_stage_target`], so there is no second rule about which rows
/// are doors. Every other row still only moves the cursor: a verb there
/// is a second, deliberate keystroke or button press.
///
/// The open runs regardless of mode, because `enter` opens in either mode
/// too ([`commit_selected_row`] is the one door both spellings take). A
/// click in filter mode on a member row therefore leaves filter mode —
/// which is [`enter_column_stage`]'s own `DialogMode::Normal`, the same
/// outcome the key gives.
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
    // Spec §20.6 fallout: claimed and dropped while a question stands —
    // `on_tick_clicked`'s own guard. Without it a door row's click would
    // open a column stage whose `Draft::enter_column` silently clears
    // the confirm, answering the question with a shrug.
    if shell
        .object_dialog
        .as_ref()
        .and_then(|s| s.draft.as_ref())
        .is_some_and(|d| d.confirm.is_some())
    {
        return;
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
    // After the cursor has moved, never before: the target is the row
    // that was just clicked, which is what `enter` would be acting on had
    // the trader pressed it instead.
    if let Some(name) = column_stage_target(shell) {
        enter_column_stage(shell, &name, cx);
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// The value chip's click (spec §20.3): move the cursor to the row, then
/// exactly the key's path — [`step_selected_row`] with `filtering` from
/// the mode, so the "nothing changes with …" notice names the right key.
/// Claimed and dropped while a confirm is armed or a text field is open
/// (the chip paints without a handler then, but a test can still call
/// this door). The read-only gate is `step_selected_row`'s callers' —
/// applied here too, since this is one.
fn on_value_chip_clicked(
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
    if draft.confirm.is_some() || draft.text_entry.is_some() {
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

/// §18.9.2: a click on a row's tick. The cursor moves to the row first,
/// then exactly `space`'s path runs — `Draft::toggle_selected`, the
/// available-block refresh, revalidation, the scroll and
/// `commit_change` — so every write and every refusal the key gives,
/// the tick gives. Ends in [`dialog::sync_dialog_text`] like every mouse
/// handler that mutates the draft (§17.1 rule 3).
///
/// **Claimed and dropped while a confirm is armed**, mirroring
/// `handle_edit_key`'s own `armed` block: a stray keystroke other than
/// enter/y/escape/n does nothing while a destructive question is on
/// screen, but the row list — ticks included — keeps painting underneath
/// the confirm row, since it is not itself replaced by one. Without this
/// guard a tick click there would reach `Draft::toggle_selected` and
/// `commit_change` regardless, committing an unrelated write (a fork
/// included) to disk while the question the trader is looking at
/// is still unanswered — the mouse disagreeing with the key on the one
/// thing that must never be ambiguous. So this returns before touching
/// the draft at all — not even the cursor moves, the same "claimed and
/// dropped" the key path gives a bare letter. Task 6's drop handler
/// reaches the same row list and needs the identical guard.
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
    // §19.4: the tick is `space`'s exact mouse path (this function's own
    // doc comment) — a read-only domain refuses it the same way the key
    // does, in the same words.
    let writable = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.domain.writable(&state.stage));
    if !writable {
        set_notice(shell, READ_ONLY_NOTICE.to_string());
        cx.notify();
        return;
    }
    let Some(draft) = draft_mut(shell) else {
        return;
    };
    if draft.confirm.is_some() {
        return;
    }
    if position >= draft.visible_rows().len() {
        return;
    }
    draft.selected = position;
    match draft.toggle_selected() {
        Step::Changed => {
            maybe_refresh_available(shell);
            revalidate(shell);
            scroll_to_cursor(shell);
            commit_change(shell, cx);
        }
        Step::Refused(reason) => refuse_step(shell, reason),
        Step::Inert => set_notice(shell, "nothing on this row changes with a tick".to_string()),
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// §18.9.4: a click on a completion row while the chain field is open —
/// the mouse form of `tab`. The cursor moves to the row and
/// `Draft::complete_chain` runs unchanged; the mode stays `Filter`, so
/// the sync writes the new text into the field and keeps it focused.
///
/// No armed-confirm guard, unlike [`on_tick_clicked`] and
/// `on_row_dropped`: the chain field and an armed confirm can never
/// coexist by construction — the action bar (where `d`/`r`/`o` arm one)
/// is withdrawn while any text field is open (`Draft::text_entry` is
/// `Some`), and `i` itself is dropped while a confirm is armed — so
/// there is nothing here for a stray click to clobber.
///
/// On a failed completion (`complete_chain` returns `false` — the typed
/// segment matched nothing, say) `selected` stays on the clicked row,
/// mirroring what a failed `tab` leaves behind.
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

/// The ghost gpui paints under the cursor during a row drag (§18.9.1):
/// the dragged name in the row's own type, on the popover surface so it
/// reads as lifted off the list rather than as one more row of it.
///
/// An entity of its own because that is the shape `on_drag`'s
/// constructor has to return; it holds the name alone, since a drag is
/// over in a second and nothing about the row that started it can change
/// underneath a ghost that is already painted.
struct DragGhost {
    name: gpui::SharedString,
}

impl gpui::Render for DragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded(px(4.))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .child(self.name.clone())
    }
}

/// §18.9.3: a row was dropped on another. Both ends are resolved by NAME
/// through [`Draft::drop_row`] — the keyboard stays live during a drag,
/// so between the grab and the drop a keystroke can have reordered or
/// removed either row — and on a change the draft's own cursor follows
/// the dropped item, so this then revalidates, scrolls to it and commits
/// through the same `commit_change` a keystroke takes: a desk-owned
/// view is forked (and says so) by a `Doc` write, and the write joins
/// the same batch behind the same debounce.
///
/// Inert drops say nothing, with two exceptions — the pair a trader
/// could otherwise read as the app having failed: a
/// catalogue-to-catalogue drop (there is no order there to change) and a
/// payload whose name has left the list mid-drag. A row dropped on
/// ITSELF is neither, and is checked first so that the commonest inert
/// gesture of all stays silent from either block. `resolves` is read
/// BEFORE the drop, because `drop_row` mutates the very lists the answer
/// depends on.
///
/// A [`Step::Refused`] takes `set_notice` rather than [`refuse_step`]:
/// that hint names `r`/`d` because the refusal it exists for is
/// unticking a slot's last dimension, and the nearest refusal a drop
/// could ever carry is `remove_selected`'s "space unticks here", which
/// those verbs are no answer to. (`drop_row` reaches neither today — its
/// own doc says why — so this arm is a safety net, not a path.)
///
/// **Claimed and dropped while a confirm is armed**, the identical guard
/// [`on_tick_clicked`] carries and for the identical reason: the row
/// list keeps painting underneath the confirm row, so a drop there would
/// otherwise commit an unrelated write (a fork included) while the
/// question on screen is still unanswered. It returns before touching the draft at
/// all — not even the cursor moves.
///
/// `pub(in crate::shell)` so the window tests can drive it directly:
/// gpui's drag machinery does not run under `TestAppContext`, so this is
/// the lowest rung the gesture can be tested on (§18.9.5).
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
    // §19.4: a drop is a reorder or a promotion/demotion — a write, same
    // as the tick — so a read-only domain refuses it identically.
    let writable = shell
        .object_dialog
        .as_ref()
        .is_some_and(|state| state.domain.writable(&state.stage));
    if !writable {
        set_notice(shell, READ_ONLY_NOTICE.to_string());
        cx.notify();
        return;
    }
    let Some(draft) = draft_mut(shell) else {
        return;
    };
    if draft.confirm.is_some() {
        return;
    }
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
