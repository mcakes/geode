//! The object dialog's gpui half: opening it, its
//! [`dialog::ModalKeyHandler`], and the painted browse list.
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
//! move here at all, and the reason Task 5's `s`/`d`/`r` verbs will be
//! reachable. `/` focuses the field and enters [`DialogMode::Filter`];
//! `escape` blurs it again. While the field is blurred the query paints
//! as static muted text rather than a live caret
//! ([`dialog::filter_row`]'s `frozen` argument): a caret in a field that
//! is not receiving the keys is the single most misleading thing a modal
//! surface can show.
//!
//! ## What this stage does not do
//!
//! Browse only. `enter` has nothing to open until Task 5 adds the edit
//! stage, and nothing here writes — so the footer advertises exactly the
//! keys that work today, and a key that is not advertised is claimed and
//! dropped rather than passed down to the shell underneath the modal.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Focusable as _, MouseButton, Window, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use super::{Domain, ObjectDialogState, ObjectRow};
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
        // letter as text before [`handle_key`] could read it as a
        // motion or (Task 5) a verb.
        false,
    );
}

/// The [`dialog::ModalKeyHandler`] for this dialog — the same priority
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
/// 3. bare `enter` is claimed in both modes and does nothing yet — the
///    key that opens the edit stage in Task 5, claimed now so the two
///    modes route it identically;
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
fn handle_key(
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
            // `has_previous_stage()` is a predicate over the stage, not a
            // literal `false`, and this dialog is the first consumer of
            // the `PreviousStage` rung at all. Today `Browse` is the only
            // stage anything constructs, so the rung is unreachable and
            // the answer is always `false` — but Task 5's edit stage
            // turns it on by existing, with no call site to remember to
            // change. See `ObjectDialogState::has_previous_stage`.
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
                // `PreviousStage` cannot be reached until Task 5
                // constructs `Stage::Edit`. Both are folded into the
                // catch-all rather than special-cased away, because
                // `escape_step` is the one ladder every modal surface
                // walks and forking it per call site is how the rungs
                // drift apart.
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
            // `Commit` (enter) opens the edit stage in Task 5; `Toggle`,
            // `EditText`, `MoveItem` and the letter verbs belong to that
            // stage too. Until then this dialog browses, and the footer
            // advertises only the keys that work — so these are claimed
            // and dropped like any other unclaimed key.
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
        // `enter` is `NormalCommand::Commit`, and normal mode already
        // claims it (the catch-all arm above). Claimed here too so the
        // two modes route the same key the same way — the keybinding
        // dialog's own `enter` branch is what this mirrors, and a
        // routing difference between the two dialogs is a difference
        // somebody eventually has to debug. It does nothing yet: Task 5
        // opens the edit stage from exactly this branch and its
        // normal-mode twin. Today it is inert either way — an unclaimed
        // `enter` reaches the focused `Input`, whose
        // `normalize_input` strips `\n`/`\r` — so claiming it changes
        // no behaviour, and there is deliberately no mutation entry for
        // a line no test can distinguish.
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
