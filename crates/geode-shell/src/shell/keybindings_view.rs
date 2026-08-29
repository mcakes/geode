//! The keybinding dialog (Part B): a list of every registered action with
//! its currently-effective binding, navigable with the vim subset from
//! [`crate::vimnav`] and editable in place — `space`/`enter`/a click on the
//! already-selected row starts "listening" for a new binding; every
//! keystroke while listening appends to a pending sequence (multi-keystroke
//! bindings, e.g. `"g g"`, are supported); `enter` commits it, `escape`
//! cancels back to ordinary list navigation. A committed capture is written
//! to the user keymap document via [`crate::keymap_edit::apply_rebind`],
//! same as every other config write in this crate, off the UI thread
//! (spec PHILOSOPHY: "nothing may stall the render thread").
//!
//! ## What this dialog does NOT do
//!
//! It never updates its own rows optimistically after a commit. Instead,
//! [`derive_rows`] re-resolves every row's effective binding, fresh, from
//! `ShellView::services` on **every render** — so a just-written rebind
//! becomes visible in the dialog only once the background reload watcher's
//! ~500ms poll (`shell::ShellView::new`'s loop) has picked the file change
//! back up and applied it to `services.keymap`. In practice this is a
//! sub-second delay, not a user-visible bug, but it is real — a rebind does
//! not *instantly* relabel its own row, and this module has no side channel
//! back into `services.keymap` to make it do so; it goes through the one
//! ordinary reload path every other config edit already goes through.
//!
//! ## Architecture: two-part state (mirrors `palette`/`PaletteState`)
//!
//! [`KeybindingsState`] — `selected`, the [`vimnav::VimListNav`] pending-
//! gesture accumulator, and the in-progress capture sequence — is pure
//! (no `gpui`), stored on `ShellView` as `keybindings: Option<
//! KeybindingsState>`, exactly like `palette: Option<PaletteState>`. A
//! sibling `gpui::ScrollHandle` field, `keybindings_scroll`, lives directly
//! on `ShellView` rather than inside this struct — the same split
//! `palette`/`palette_scroll` already use, for the same reason: it keeps
//! this module's own state fully unit-testable without a window (see the
//! `tests` module at the bottom), while `build`'s render closure and this
//! module's [`handle_key`] both still reach it (`shell.keybindings_scroll`)
//! exactly the way `ShellView`'s own methods reach `self.palette_scroll`.
//!
//! Opens through [`dialog::open_shell_dialog_with_key`] (Part B's addition
//! to the one mandatory modal door, `dialog::open_shell_dialog`) rather
//! than the plain `open_shell_dialog` `settings_view::open` uses — this
//! dialog needs first refusal on every keystroke (vim nav, `space`/`enter`
//! to start listening, then every keystroke while listening), which is
//! exactly what `dialog::ModalKeyHandler` exists for.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, MouseButton, Window, div, px};
use gpui_component::kbd::Kbd;
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use geode_core::config::Layer;

use crate::actions::{ActionId, ActionRegistry};
use crate::keymap::{Binding, Keymap, Keystroke, Modifiers};
use crate::keymap_edit::{Displacement, Rebind, apply_rebind};
use crate::palette;
use crate::vimnav::{self, NavResult, VimListNav};

use super::ShellView;
use super::dialog;

// ---------------------------------------------------------------------
// Pure core — no gpui. Row derivation and capture-state transitions.
// ---------------------------------------------------------------------

/// One row's currently-effective binding: the keystroke sequence itself,
/// plus the two pieces of provenance a rebind needs to reconstruct a
/// [`Rebind`] later — [`crate::keymap::Binding::context_source`] (Part A's
/// seam for this exact purpose) and the layer it came from (`Layer::User`
/// decides `Rebind::old_key_is_user_layer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundKey {
    pub keystrokes: Vec<Keystroke>,
    pub context_source: Option<String>,
    pub layer: Layer,
}

/// One row of the keybinding dialog: one registered action, its palette-
/// facing title/category, and its currently-effective binding (`None` =
/// unbound — the dialog shows "unbound"). See [`derive_rows`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingRow {
    pub action: ActionId,
    pub title: String,
    pub category: String,
    pub current: Option<BoundKey>,
}

/// Build the dialog's row list: every action in `registry`, each paired
/// with its currently *effective* binding — see [`effective_binding`] for
/// the exact resolution rule, which mirrors the real `Matcher`'s own
/// keystroke-keyed last-wins (not a plain "last binding for this action
/// id", which the first cut of this function used and which a review round
/// caught: it let a bare unbind — `"mod+h" = "none"` with no replacement
/// key — leave the row showing the OLD binding as if still live, because an
/// action-id-only search skips `"none"` entries outright rather than
/// letting them shadow the real binding they were written to silence).
///
/// Sorted by `(category, title)` — the dialog's own display order (unlike
/// `palette::build_items`, which keeps `ActionRegistry::iter`'s id-sorted
/// order as-is). Also unlike `palette::build_binding_index`, which keeps
/// the FIRST-declared binding for a quick display hint next to a palette
/// row — this dialog deliberately resolves the actual effective (last,
/// shadow-aware) binding instead, since it exists to show, and let the
/// user edit, what a keystroke actually fires today, not just a hint.
///
/// Called fresh on every render (`build`) and on every keystroke
/// (`handle_key`) rather than cached in [`KeybindingsState`] — see the
/// module doc's "What this dialog does NOT do". The action list itself
/// never changes at runtime (every action is registered before the window
/// opens), so `rows.len()` — and therefore the valid range of
/// `KeybindingsState::selected` — is stable across every call within one
/// open dialog session, even though individual rows' `current` can change
/// under it.
pub fn derive_rows(registry: &ActionRegistry, keymap: &Keymap) -> Vec<KeybindingRow> {
    let bindings = keymap.bindings();
    let mut rows: Vec<KeybindingRow> = registry
        .iter()
        .map(|def| {
            let current = effective_binding(bindings, &def.id).map(|b| BoundKey {
                keystrokes: b.keystrokes.clone(),
                context_source: b.context_source.clone(),
                layer: b.layer,
            });
            KeybindingRow {
                action: def.id.clone(),
                title: def.title.clone(),
                category: def.category.clone(),
                current,
            }
        })
        .collect();
    rows.sort_by(|a, b| (&a.category, &a.title).cmp(&(&b.category, &b.title)));
    rows
}

/// Resolve `action`'s effective binding within `bindings` (`keymap.
/// bindings()`'s own layer-then-declaration order), or `None` if unbound.
///
/// Scans candidates whose `action` matches, **most recently declared
/// first**; a candidate only counts as effective if no *later* binding in
/// the full list — declared after it, any action, including `"none"` —
/// shares its exact keystroke sequence AND carries a context that would
/// apply whenever the candidate's own does. A shadowed candidate is
/// skipped in favor of an earlier one for the same action (which may in
/// turn be shadowed by something else); if every candidate is shadowed (or
/// there are none), the action is unbound. This mirrors the real
/// `Matcher::press`'s own keystroke-keyed last-wins rule (spec §3.4): for
/// one keystroke, the *last* matching entry across the whole document
/// stack wins, regardless of which action it names — a plain "last binding
/// for this action id" search (the pre-review-round version of this
/// function) missed exactly this: a bare `"mod+h" = "none"` unbind (no
/// replacement key) never carries the real action id, so an action-id-only
/// search skips it and reports the old binding as if still live.
///
/// **Context approximation, stated honestly**: the real `Matcher` decides
/// "would this later binding actually apply" by evaluating a compiled
/// `Predicate` against a live context stack; this free function has no
/// such stack (there is no notion of "the current UI context" for a
/// dialog listing every action at once), so it approximates with the
/// *source spelling* of `context_source` instead: a later same-keystroke
/// binding shadows the candidate when its `context_source` is `None` (a
/// no-context entry is always active, so it always shadows) or is
/// string-equal to the candidate's own (the common real case — a rebind
/// shadowing its own prior entry within the same context, `keymap_edit`'s
/// own documented assumption). This is exact for every shape this crate's
/// own tooling ever writes. It can only drift from the real predicate
/// evaluation for a hand-written keymap pairing two *different but
/// overlapping* context strings on the same keystroke (e.g. `"workspace"`
/// and `"workspace && !modal"`) — string comparison would under-mark that
/// as not-shadowing even though the predicates do overlap at runtime.
/// Accepted as a documented approximation for a display-only resolution,
/// not the authoritative dispatch path (`Matcher::press` remains that).
fn effective_binding<'a>(bindings: &'a [Binding], action: &ActionId) -> Option<&'a Binding> {
    bindings
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| b.action == *action)
        .find(|(i, b)| !is_shadowed(bindings, *i, b))
        .map(|(_, b)| b)
}

/// True if some binding declared AFTER `bindings[index]` (`candidate`)
/// shares its exact keystroke sequence and carries a context that would
/// apply whenever `candidate`'s own would — see [`effective_binding`]'s
/// doc comment for the full reasoning and the stated context-equality
/// approximation.
fn is_shadowed(bindings: &[Binding], index: usize, candidate: &Binding) -> bool {
    bindings[index + 1..].iter().any(|later| {
        later.keystrokes == candidate.keystrokes
            && (later.context_source.is_none() || later.context_source == candidate.context_source)
    })
}

/// Persistent state for one open keybinding dialog session — the analogue
/// of `palette::PaletteState`. Holds no `gpui` types (see the module doc's
/// "Architecture" section for why the scroll handle lives beside this
/// instead of inside it), so every transition here is unit-testable
/// without a window.
#[derive(Debug, Default)]
pub struct KeybindingsState {
    pub selected: usize,
    pub nav: VimListNav,
    /// `Some(pending)` while listening for a new binding — `pending` is the
    /// keystroke sequence captured so far, appended to by
    /// [`press_while_listening`] on every keystroke except a bare
    /// `enter`/`escape`. `None` in ordinary list-navigation mode.
    pub listening: Option<Vec<Keystroke>>,
}

impl KeybindingsState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// What one keystroke does to an in-progress capture sequence — see
/// [`press_while_listening`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The keystroke was appended to the sequence; still listening.
    Continue,
    /// A bare `enter` committed a non-empty sequence — these are the
    /// keystrokes to bind (the caller renders them space-joined via
    /// `palette::render_binding`, the same rendering every binding in this
    /// crate uses).
    Commit(Vec<Keystroke>),
    /// A bare `escape` was pressed, or a bare `enter` was pressed on an
    /// empty sequence (empty capture + enter = cancel) — listening ends
    /// with nothing captured.
    Cancel,
}

/// Feed one keystroke to an in-progress rebind capture.
///
/// A bare (no modifiers) `enter` or `escape` is never appended to
/// `pending` — those two keys are the capture's own control keys
/// (commit / cancel), so a binding can never be *composed of* a bare
/// enter or escape through this dialog (surfaced as a muted hint in the
/// dialog's footer: hand-edit `keymap.toml` for those). A **modified**
/// enter/escape (`shift+enter`, `ctrl+escape`, ...) is a different
/// keystroke from the bare key by this crate's own `Keystroke` equality
/// (mods are part of the value) and so IS capturable — only the exact bare
/// presses are reserved.
pub fn press_while_listening(pending: &mut Vec<Keystroke>, ks: &Keystroke) -> CaptureOutcome {
    let bare = ks.mods == Modifiers::NONE;
    if bare && ks.key == "escape" {
        return CaptureOutcome::Cancel;
    }
    if bare && ks.key == "enter" {
        return if pending.is_empty() {
            CaptureOutcome::Cancel
        } else {
            CaptureOutcome::Commit(std::mem::take(pending))
        };
    }
    pending.push(ks.clone());
    CaptureOutcome::Continue
}

/// What a click on row `clicked_ix` does to already-open dialog state
/// (brief: "clicking the already-selected row starts listening"). Clicking
/// any *other* row just selects it — and, symmetrically, cancels an
/// in-progress capture on the previously selected row rather than leaving
/// it dangling on a row that's no longer selected. Clicking the currently
/// *listening* row again (an edge case the brief doesn't spell out)
/// resolves the same way as clicking away: the click always changes
/// something about the row it lands on, so the only case that leaves the
/// dialog holding a `listening` state afterward is a fresh click on an
/// already-selected, not-yet-listening row.
pub fn click_selects_or_listens(state: &mut KeybindingsState, clicked_ix: usize) {
    if state.selected == clicked_ix && state.listening.is_none() {
        state.listening = Some(Vec::new());
    } else {
        state.selected = clicked_ix;
        state.listening = None;
        state.nav.cancel();
    }
}

/// True when a just-committed capture (`new_keystrokes`) is byte-for-byte
/// the same keystroke sequence `row` is already effectively bound to — a
/// "recapture" that would only ever produce a no-op rebind (the same key,
/// re-typed, e.g. a user opening the dialog to confirm what a row is
/// already bound to rather than to change it). The caller
/// ([`handle_key`]) skips scheduling [`apply_rebind`] entirely in this
/// case, saving a pointless disk write plus the reload watcher's next
/// pickup cycle for a change that wouldn't have changed anything. An
/// unbound row (`row.current: None`) is never a recapture — any capture
/// there is a genuine new binding.
pub fn is_same_key_recapture(row: &KeybindingRow, new_keystrokes: &[Keystroke]) -> bool {
    row.current
        .as_ref()
        .is_some_and(|bound| bound.keystrokes == new_keystrokes)
}

// ---------------------------------------------------------------------
// Rendering (gpui) — everything above this line is the pure core.
// ---------------------------------------------------------------------

/// Row height estimate for sizing the scrollable viewport (two lines: title
/// plus muted category). Not load-bearing for correctness, same caveat as
/// `palette::ROW_HEIGHT`: scroll-follow goes through the real
/// `ScrollHandle::scroll_to_item`, which measures actual layout.
const ROW_HEIGHT: f32 = 44.0;
/// Rows visible before the list scrolls — see `palette::VISIBLE_ROWS`.
const VISIBLE_ROWS: usize = 10;
/// Target dialog content width in pixels.
const WIDTH: f32 = 640.0;

/// Open the keybinding dialog (`keybindings::open`, palette-only today —
/// see `defaults::register_builtin_actions`'s doc comment on that action).
/// A no-op if a modal is already open, mirroring `settings_view::open`'s
/// own guard.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    // Fresh state every open — nothing survives a close/reopen, same
    // contract as `palette`/`PaletteState`.
    view.keybindings = Some(KeybindingsState::new());
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        "Keyboard shortcuts",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
    );
}

/// Convert this crate's own [`Keystroke`] into `gpui::Keystroke` for
/// [`Kbd::new`] — the reverse of `shell::keys::convert_keystroke`, and the
/// only place this module touches `gpui::Keystroke`'s fields directly.
fn to_gpui_keystroke(ks: &Keystroke) -> gpui::Keystroke {
    gpui::Keystroke {
        modifiers: gpui::Modifiers {
            control: ks.mods.ctrl,
            alt: ks.mods.alt,
            shift: ks.mods.shift,
            platform: ks.mods.cmd,
            function: false,
        },
        key: ks.key.clone(),
        key_char: None,
    }
}

/// The [`dialog::ModalKeyHandler`] for this dialog: while listening, every
/// keystroke is offered to [`press_while_listening`] first (swallowed
/// unconditionally, `true`, regardless of outcome — even `escape`, which
/// must cancel the capture rather than falling through to `handle_key_down`
/// 's own "escape closes the modal" fallback); otherwise `space`/bare
/// `enter` start listening, and everything else goes to
/// [`vimnav::VimListNav::press`] — a `NavResult::NotNav` (which includes a
/// bare `escape`, letting it fall through to close the modal) returns
/// `false`, unhandled, exactly as the modal-branch contract requires. A
/// commit that exactly re-captures the row's already-effective binding
/// ([`is_same_key_recapture`]) skips [`spawn_rebind`] entirely — nothing
/// would change on disk, so there's nothing to write.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    _window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let user_dir = shell.user_dir.clone();
    let Some(state) = shell.keybindings.as_mut() else {
        return false;
    };

    if let Some(pending) = state.listening.as_mut() {
        match press_while_listening(pending, ks) {
            CaptureOutcome::Continue => {}
            CaptureOutcome::Cancel => {
                state.listening = None;
            }
            CaptureOutcome::Commit(keystrokes) => {
                state.listening = None;
                if let Some(row) = rows.get(state.selected)
                    && !is_same_key_recapture(row, &keystrokes)
                {
                    spawn_rebind(row, keystrokes, user_dir, cx);
                }
            }
        }
        cx.notify();
        return true;
    }

    if ks.mods == Modifiers::NONE && matches!(ks.key.as_str(), "space" | "enter") {
        state.listening = Some(Vec::new());
        cx.notify();
        return true;
    }

    match state.nav.press(ks) {
        NavResult::Command(cmd) => {
            state.selected = vimnav::apply(state.selected, rows.len(), cmd);
            let selected = state.selected;
            shell.keybindings_scroll.scroll_to_item(selected);
            cx.notify();
            true
        }
        NavResult::Pending => {
            cx.notify();
            true
        }
        NavResult::NotNav => false,
    }
}

/// Selection/listening logic for a real mouse click on row `clicked`
/// (`ActionId`, resolved back to an index against a freshly derived row
/// list — rows are never cached, see the module doc). The gpui-facing
/// wrapper around the pure [`click_selects_or_listens`].
fn on_row_clicked(shell: &mut ShellView, clicked: &ActionId, cx: &mut Context<ShellView>) {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let Some(ix) = rows.iter().position(|r| &r.action == clicked) else {
        return;
    };
    let Some(state) = shell.keybindings.as_mut() else {
        return;
    };
    click_selects_or_listens(state, ix);
    let selected = state.selected;
    shell.keybindings_scroll.scroll_to_item(selected);
    cx.notify();
}

/// Build a [`Rebind`] for `row` from the just-committed `new_keystrokes`
/// and run [`apply_rebind`] on the background executor (philosophy: no
/// file I/O on the UI thread) — args captured, task spawned and detached, a
/// warning logged from inside the task on `Err` or on
/// [`Displacement::OldKeyNotFound`]. See the module doc's "What this dialog
/// does NOT do": this never touches `shell`'s own state — the dialog's rows
/// pick the change up on their own once the reload watcher applies it.
fn spawn_rebind(
    row: &KeybindingRow,
    new_keystrokes: Vec<Keystroke>,
    user_dir: Option<PathBuf>,
    cx: &mut Context<ShellView>,
) {
    let Some(user_dir) = user_dir else {
        eprintln!(
            "[keybindings] warning: no writable user config dir; the new binding for {} was not saved",
            row.action
        );
        return;
    };
    let rebind = Rebind {
        context: row.current.as_ref().and_then(|b| b.context_source.clone()),
        new_key: palette::render_binding(&new_keystrokes),
        action: row.action.0.clone(),
        old_key: row
            .current
            .as_ref()
            .map(|b| palette::render_binding(&b.keystrokes)),
        old_key_is_user_layer: row.current.as_ref().is_some_and(|b| b.layer == Layer::User),
    };
    cx.background_executor()
        .spawn(async move {
            match apply_rebind(&user_dir, &rebind) {
                Ok(outcome) if outcome.displacement == Displacement::OldKeyNotFound => {
                    eprintln!(
                        "[keybindings] warning: the previous binding for {} was not found \
                         where expected while saving the new one — it may still be \
                         reachable from wherever it actually lives",
                        rebind.action
                    );
                }
                Ok(_) => {}
                Err(e) => eprintln!(
                    "[keybindings] warning: failed to save the new binding for {}: {e}",
                    rebind.action
                ),
            }
        })
        .detach();
}

/// The [`dialog::ShellModal::build`] closure body: a scrollable row list
/// (title + category on the left, the current binding as [`Kbd`] chips —
/// or "unbound" — on the right, live capture chips while listening) plus a
/// muted footer hint. `entity` is the `Entity<ShellView>` every row's click
/// handler captures to reach [`on_row_clicked`] later, at click time —
/// `shell` itself is only this call's own plain-borrow read of whatever's
/// needed right now (see `ShellModal::build`'s doc comment for why the two
/// are both needed rather than one substituting for the other).
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.keybindings.as_ref() else {
        return div().into_any_element();
    };
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let theme = cx.theme();

    let mut list = v_flex()
        .id("keybindings-list")
        .w(px(WIDTH))
        .h(px(
            (rows.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT)
        ))
        .overflow_y_scroll()
        .track_scroll(&shell.keybindings_scroll)
        .debug_selector(|| "keybindings-list".to_string());

    for (i, row) in rows.iter().enumerate() {
        let is_selected = i == state.selected;
        let is_listening = is_selected && state.listening.is_some();

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
            .child(div().child(row.title.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(row.category.clone()),
            );

        let binding_el: AnyElement = if is_listening {
            let pending = state.listening.as_ref().expect("is_listening implies Some");
            if pending.is_empty() {
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("listening…")
                    .into_any_element()
            } else {
                h_flex()
                    .gap_1()
                    .children(
                        pending
                            .iter()
                            .map(|ks| Kbd::new(to_gpui_keystroke(ks)).into_any_element()),
                    )
                    .into_any_element()
            }
        } else {
            match row.current.as_ref() {
                Some(bound) => h_flex()
                    .gap_1()
                    .children(
                        bound
                            .keystrokes
                            .iter()
                            .map(|ks| Kbd::new(to_gpui_keystroke(ks)).into_any_element()),
                    )
                    .into_any_element(),
                None => div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("unbound")
                    .into_any_element(),
            }
        };

        let entity_for_row = entity.clone();
        let action = row.action.clone();
        let row_el = row_el
            .child(label)
            .child(binding_el)
            .debug_selector(move || format!("keybindings-row-{i}"))
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                entity_for_row.update(cx, |shell, cx| {
                    on_row_clicked(shell, &action, cx);
                });
            });

        list = list.child(row_el);
    }

    let hint_line = if state.listening.is_some() {
        "Listening — type keys, Enter to save, Esc to cancel".to_string()
    } else if let Some(pending) = state.nav.pending_display() {
        format!("{pending}…")
    } else {
        "j/k move · 5j/3k count · gg/G top/bottom · ctrl+d/u/f/b page · space/enter to rebind"
            .to_string()
    };

    let footer = v_flex()
        .w(px(WIDTH))
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(theme.border)
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(hint_line),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Enter and Escape can't be captured — hand-edit keymap.toml for those."),
        );

    v_flex()
        .gap_2()
        .child(list)
        .child(footer)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::build_keymap;
    use geode_core::config::LayerDoc;

    fn registry_with(actions: &[(&str, &str, &str)]) -> ActionRegistry {
        let mut reg = ActionRegistry::default();
        for (id, title, category) in actions {
            reg.register(crate::actions::ActionDef {
                id: ActionId(id.to_string()),
                title: title.to_string(),
                category: category.to_string(),
            })
            .unwrap();
        }
        reg
    }

    fn doc(layer: Layer, text: &str) -> LayerDoc {
        LayerDoc {
            layer,
            name: "keymap".to_string(),
            file: format!("{}/keymap.toml", layer.name()).into(),
            table: text.parse().unwrap(),
        }
    }

    // -- derive_rows --------------------------------------------------

    #[test]
    fn an_action_with_no_binding_at_all_is_unbound() {
        let reg = registry_with(&[("workspace::focus_left", "Focus left", "Workspace")]);
        let (keymap, diags) = build_keymap(&[], Modifiers::ALT, &reg);
        assert!(diags.is_empty());
        let rows = derive_rows(&reg, &keymap);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].current, None);
    }

    #[test]
    fn a_user_layer_binding_wins_over_a_builtin_one_for_the_same_action() {
        let reg = registry_with(&[("workspace::focus_left", "Focus left", "Workspace")]);
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"ctrl+shift+h\" = \"workspace::focus_left\"\n",
        );
        let (keymap, diags) = build_keymap(&[builtin, user], Modifiers::ALT, &reg);
        assert!(diags.is_empty());
        let rows = derive_rows(&reg, &keymap);
        let bound = rows[0].current.as_ref().expect("should be bound");
        assert_eq!(bound.keystrokes[0].key, "h");
        assert!(bound.keystrokes[0].mods.ctrl && bound.keystrokes[0].mods.shift);
        assert_eq!(bound.layer, Layer::User);
    }

    #[test]
    fn a_none_shadow_entry_for_a_different_key_does_not_hide_the_real_binding() {
        // The user layer shadows an unrelated key spec with "none" (a
        // realistic post-rebind document shape: displacing an old key
        // while a NEW key still carries the real action). The shadow
        // entry's own action id ("none") can never match the real row's
        // action id, so it must not affect this row at all.
        let reg = registry_with(&[("workspace::focus_left", "Focus left", "Workspace")]);
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"mod+h\" = \"none\"\n\"ctrl+shift+h\" = \"workspace::focus_left\"\n",
        );
        let (keymap, diags) = build_keymap(&[builtin, user], Modifiers::ALT, &reg);
        assert!(diags.is_empty());
        let rows = derive_rows(&reg, &keymap);
        let bound = rows[0].current.as_ref().expect("should still be bound");
        assert_eq!(bound.keystrokes[0].key, "h");
        assert!(bound.keystrokes[0].mods.ctrl && bound.keystrokes[0].mods.shift);
    }

    // Regression (review round): a bare unbind — the SAME key rewritten to
    // "none", no replacement key at all — must hide the row's binding,
    // exactly as it hides it from the real `Matcher` (proved for the
    // matcher itself by `keymap::matcher::tests::unbind_swallows_key`). The
    // first cut of `derive_rows` searched by action id alone, which skips a
    // "none" entry outright (it never carries the real action id) and so
    // reported the OLD binding as still live — the bug these two tests
    // pin down, one variant per `context_source` shape.

    #[test]
    fn a_bare_unbind_in_the_same_context_hides_the_binding() {
        let reg = registry_with(&[("workspace::focus_left", "Focus left", "Workspace")]);
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"none\"\n",
        );
        let (keymap, diags) = build_keymap(&[builtin, user], Modifiers::ALT, &reg);
        assert!(diags.is_empty());
        let rows = derive_rows(&reg, &keymap);
        assert_eq!(
            rows[0].current, None,
            "a same-context bare unbind must hide the row's binding entirely, \
             not leave the old builtin binding showing as still live"
        );
    }

    #[test]
    fn a_bare_unbind_in_a_no_context_entry_hides_the_binding() {
        let reg = registry_with(&[("palette::toggle", "Toggle command palette", "Palette")]);
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\n[bindings.keys]\n\"ctrl+k\" = \"palette::toggle\"\n",
        );
        let user = doc(
            Layer::User,
            "[[bindings]]\n[bindings.keys]\n\"ctrl+k\" = \"none\"\n",
        );
        let (keymap, diags) = build_keymap(&[builtin, user], Modifiers::ALT, &reg);
        assert!(diags.is_empty());
        let rows = derive_rows(&reg, &keymap);
        assert_eq!(
            rows[0].current, None,
            "a no-context bare unbind must hide the row's binding entirely — \
             this is the realistic shape of BUILTIN_KEYMAP's own no-context \
             entries (palette::toggle, settings::open, ...)"
        );
    }

    #[test]
    fn context_source_and_layer_are_carried_onto_the_row() {
        let reg = registry_with(&[("workspace::focus_left", "Focus left", "Workspace")]);
        let user = doc(
            Layer::User,
            "[[bindings]]\ncontext = \"workspace\"\n[bindings.keys]\n\"mod+h\" = \"workspace::focus_left\"\n",
        );
        let (keymap, diags) = build_keymap(&[user], Modifiers::ALT, &reg);
        assert!(diags.is_empty());
        let rows = derive_rows(&reg, &keymap);
        let bound = rows[0].current.as_ref().unwrap();
        assert_eq!(bound.context_source.as_deref(), Some("workspace"));
        assert_eq!(bound.layer, Layer::User);
    }

    #[test]
    fn rows_sort_by_category_then_title_not_by_action_id() {
        let reg = registry_with(&[
            ("z::action", "Zeta", "Alpha category"),
            ("a::action", "Alpha", "Alpha category"),
            ("b::action", "Beta", "Zeta category"),
        ]);
        let (keymap, diags) = build_keymap(&[], Modifiers::ALT, &reg);
        assert!(diags.is_empty());
        let rows = derive_rows(&reg, &keymap);
        let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Alpha", "Zeta", "Beta"],
            "Alpha category's two rows (sorted by title within it) must come \
             before Zeta category's one row"
        );
    }

    // -- press_while_listening -----------------------------------------

    fn key(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::NONE,
            key: k.to_string(),
        }
    }

    fn ctrl(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::CTRL,
            key: k.to_string(),
        }
    }

    #[test]
    fn ordinary_keystrokes_append_to_the_pending_capture() {
        let mut pending = Vec::new();
        assert_eq!(
            press_while_listening(&mut pending, &ctrl("x")),
            CaptureOutcome::Continue
        );
        assert_eq!(pending, vec![ctrl("x")]);
        assert_eq!(
            press_while_listening(&mut pending, &key("g")),
            CaptureOutcome::Continue
        );
        assert_eq!(pending, vec![ctrl("x"), key("g")]);
    }

    #[test]
    fn bare_enter_commits_a_non_empty_capture() {
        let mut pending = vec![key("g"), key("g")];
        let outcome = press_while_listening(&mut pending, &key("enter"));
        assert_eq!(outcome, CaptureOutcome::Commit(vec![key("g"), key("g")]));
        assert!(
            pending.is_empty(),
            "the committed sequence must be taken out of pending, not left behind"
        );
    }

    #[test]
    fn bare_enter_on_an_empty_capture_cancels() {
        let mut pending = Vec::new();
        assert_eq!(
            press_while_listening(&mut pending, &key("enter")),
            CaptureOutcome::Cancel
        );
    }

    #[test]
    fn bare_escape_always_cancels() {
        let mut pending = vec![key("g")];
        assert_eq!(
            press_while_listening(&mut pending, &key("escape")),
            CaptureOutcome::Cancel
        );
    }

    #[test]
    fn a_modified_enter_or_escape_is_captured_rather_than_treated_as_a_control_key() {
        let mut pending = Vec::new();
        assert_eq!(
            press_while_listening(&mut pending, &ctrl("enter")),
            CaptureOutcome::Continue
        );
        assert_eq!(
            press_while_listening(&mut pending, &ctrl("escape")),
            CaptureOutcome::Continue
        );
        assert_eq!(pending, vec![ctrl("enter"), ctrl("escape")]);
    }

    // -- click_selects_or_listens ---------------------------------------

    #[test]
    fn clicking_the_selected_not_listening_row_starts_listening() {
        let mut state = KeybindingsState {
            selected: 2,
            ..Default::default()
        };
        click_selects_or_listens(&mut state, 2);
        assert_eq!(state.selected, 2);
        assert_eq!(state.listening, Some(Vec::new()));
    }

    #[test]
    fn clicking_a_different_row_selects_it_and_cancels_any_capture() {
        let mut state = KeybindingsState {
            selected: 2,
            listening: Some(vec![key("g")]),
            ..Default::default()
        };
        click_selects_or_listens(&mut state, 5);
        assert_eq!(state.selected, 5);
        assert_eq!(state.listening, None);
    }

    #[test]
    fn clicking_the_already_listening_row_again_cancels_listening() {
        let mut state = KeybindingsState {
            selected: 2,
            listening: Some(vec![key("g")]),
            ..Default::default()
        };
        click_selects_or_listens(&mut state, 2);
        assert_eq!(state.selected, 2);
        assert_eq!(state.listening, None);
    }

    // -- is_same_key_recapture -------------------------------------------

    fn row_bound_to(keystrokes: Vec<Keystroke>) -> KeybindingRow {
        KeybindingRow {
            action: ActionId("workspace::focus_left".to_string()),
            title: "Focus left".to_string(),
            category: "Workspace".to_string(),
            current: Some(BoundKey {
                keystrokes,
                context_source: None,
                layer: Layer::Builtin,
            }),
        }
    }

    #[test]
    fn recapturing_the_exact_same_binding_is_a_recapture() {
        let row = row_bound_to(vec![ctrl("h")]);
        assert!(is_same_key_recapture(&row, &[ctrl("h")]));
    }

    #[test]
    fn capturing_a_different_binding_is_not_a_recapture() {
        let row = row_bound_to(vec![ctrl("h")]);
        assert!(!is_same_key_recapture(&row, &[ctrl("l")]));
        assert!(!is_same_key_recapture(&row, &[ctrl("h"), key("g")]));
    }

    #[test]
    fn capturing_anything_on_an_unbound_row_is_never_a_recapture() {
        let row = KeybindingRow {
            action: ActionId("workspace::focus_left".to_string()),
            title: "Focus left".to_string(),
            category: "Workspace".to_string(),
            current: None,
        };
        assert!(!is_same_key_recapture(&row, &[ctrl("h")]));
    }
}
