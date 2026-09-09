//! The keybinding dialog: a list of every registered action with its
//! currently-effective binding, **modal** — normal mode by default, `/`
//! for the filter — and editable in place
//! (`docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md`,
//! which supersedes the filter-first model of
//! `2026-09-01-dialog-filter-input-design.md` for this dialog).
//!
//! ## The two modes
//!
//! The dialog opens in [`DialogMode::Normal`] with `ShellView::
//! dialog_input` **blurred**, so a bare letter is a verb rather than
//! filter text — which is the whole reason this dialog can grow `d`
//! (unbind) and `r` (reset) at all: while the filter owned every
//! printable key, no letter could ever mean anything else. `/` enters
//! [`DialogMode::Filter`], which is exactly the always-focused filter
//! that shipped before: typing narrows, and the rows are ranked fresh by
//! [`visible_rows`] over each row's [`searchable_text`] — the displayed
//! title and category, never the invisible action id.
//!
//! Both modes share one navigation vocabulary
//! ([`crate::listfilter::nav_command`], reached in normal mode through
//! [`crate::dialogmode::normal_command`]): `up`/`down`/`ctrl+p`/`ctrl+n`
//! ∓1, `ctrl+d`/`ctrl+u` ±5, `ctrl+f`/`ctrl+b`/`pageup`/`pagedown` ±10,
//! with `j`/`k`/`g`/`shift+g` added in normal mode where the letters are
//! free. `enter` starts a rebind capture in either. `tab`/`shift+tab` are
//! claimed in both, but only to be dropped — they are the settings
//! dialog's stepping keys, reserved and inert here (see [`handle_key`]'s
//! own doc comment for why claiming, not just ignoring, is what actually
//! makes them inert).
//!
//! ## The two verbs (spec §8)
//!
//! `d` **unbinds** the selected row's currently-effective binding and
//! `r` **resets** it — the capability that could not exist while the
//! filter owned every letter, and the reason this dialog went modal at
//! all. Both write through [`crate::keymap_edit::apply_unbind`], which
//! only ever edits the *user* layer, so the two verbs differ exactly
//! where the layers do:
//!
//! - a binding from builtin or desk cannot be removed, so `d` silences
//!   it with the documented `"none"` shadow in the user entry;
//! - a binding that IS the user's own is removed outright, which is also
//!   the whole of what `r` does — `r` is the removal branch on its own,
//!   guarded by the row's binding actually coming from the user layer.
//!
//! Which branch a write takes is decided by [`BoundKey::layer`], carried
//! onto the row by [`derive_rows`] from the very [`Binding`] the row
//! displays — never re-inferred at the call site. See
//! [`unbind_selected`] for why getting that backwards is destructive in
//! two different directions.
//!
//! Both verbs report in [`KeybindingsState::notice`], a line painted in
//! the footer and dropped at the next keystroke or click. Both use it to
//! acknowledge the write *immediately*, naming the key and the way back
//! ([`recovery`]) — one bare, unmodified key performing a disk write
//! needs an acknowledgement that does not wait on the ~500ms reload
//! before the row relabels. Those acknowledgements are in the present
//! tense ("silencing …", "removing …") on purpose: the write is on the
//! background executor and can still come back `removed: false`, so a
//! completed tense would assert an outcome the dialog has not confirmed.
//! A verb that finds nothing to do writes nothing and says that instead.
//! A key that visibly does nothing is the defect class this interaction
//! model exists to remove, so "nothing happened" is stated in the footer
//! rather than left to be inferred from an unchanged screen.
//!
//! Neither verb prompts for confirmation. `d` is recoverable — in the
//! dialog itself for a binding with no context ([`RECOVERY`]), by hand
//! in `keymap.toml` for a contexted one ([`RECOVERY_CONTEXTED`], the
//! common case) — so a confirm step would tax every deliberate unbind to
//! guard against a mistake that can be undone.
//!
//! `escape` walks the ladder of [`crate::dialogmode::escape_step`], one
//! visible change per press: filter → normal (keeping the query
//! applied), → clear the query, → close the modal. The close rung is the
//! one this module does NOT handle — it returns `false` and lets
//! `handle_key_down`'s modal branch close the dialog, the same door a
//! backdrop click uses.
//!
//! ## Rebind capture
//!
//! `enter` (or a click on the already-selected row) starts "listening"
//! for a new binding on the selected row; every keystroke while
//! listening appends to a pending sequence (multi-keystroke bindings,
//! e.g. `"g g"`, are supported); `enter` commits it, `escape` cancels.
//! Capture is a third, *momentary* mode, deliberately checked before the
//! mode routing and never passed through `normal_command`: while it is
//! open every keystroke is the capture's, verbs included.
//!
//! Listening **blurs the filter input** and hands focus to the shell
//! root, because a focused single-line `Input` consumes bare letters as
//! text before any raw key listener sees them — without the blur, a
//! capture could never read a plain `j`. That blur is the same switch
//! normal mode holds open permanently; cancelling or committing hands
//! focus back **to whichever surface the current mode owns** (the filter
//! in `Filter`, the shell root in `Normal`) — hardcoding the filter here
//! would silently focus it under a dialog still claiming to be in normal
//! mode, and the next `d` would type a `d` instead of unbinding. While
//! listening — and, for the same reason, throughout normal mode — the
//! filter row renders the query as static muted text rather than a live
//! caret (see [`dialog::filter_row`]): a caret there would be a lie
//! about where keystrokes are going.
//!
//! A committed capture is written to the user keymap document via
//! [`crate::keymap_edit::apply_rebind`], same as every other config
//! write in this crate, off the UI thread (spec PHILOSOPHY: "nothing may
//! stall the render thread").
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
//! [`KeybindingsState`] — `selected`, the mode, the in-progress capture
//! sequence, and the mirrored filter query — is pure (no `gpui`), stored on
//! `ShellView` as `keybindings: Option<KeybindingsState>`, exactly like
//! `palette: Option<PaletteState>`. Its two `gpui` siblings live directly
//! on `ShellView` rather than inside this struct: `keybindings_scroll`
//! (the row list's `gpui::ScrollHandle`) and `dialog_input` (the shared
//! filter field, whose `InputEvent::Change` subscription feeds
//! [`KeybindingsState::set_query`]). That is the same split
//! `palette`/`palette_scroll`/`palette_input` already use, for the same
//! reason: it keeps this module's own state fully unit-testable without a
//! window (see the `tests` module at the bottom), while `build`'s render
//! closure and this module's [`handle_key`] both still reach them
//! (`shell.keybindings_scroll`, `shell.dialog_input`) exactly the way
//! `ShellView`'s own methods reach `self.palette_scroll`.
//!
//! Opens through [`dialog::open_shell_dialog_with_key`] (the one
//! mandatory modal door, alongside `dialog::open_shell_dialog`) — this
//! dialog needs first refusal on the keystrokes it claims, and on *every*
//! keystroke while listening, which is exactly what
//! `dialog::ModalKeyHandler` exists for. `settings_view` has adopted the
//! same door and the same interaction model wholesale (the
//! settings-dialog rewrite), and this module's chip/highlight render
//! helpers ([`key_chip`], [`highlighted_text`]) are `pub(crate)` for the
//! same reason.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, FontWeight, HighlightStyle,
    Hsla, MouseButton, StyledText, Window, div, px,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use geode_core::config::Layer;

use crate::actions::{ActionId, ActionRegistry};
use crate::dialogmode::{self, DialogMode, EscapeStep, NormalCommand};
use crate::keymap::{Binding, Keymap, Keystroke, Modifiers};
use crate::keymap_edit::{Displacement, Rebind, Unbind, apply_rebind, apply_unbind};
use crate::listfilter::{self, Ranked};
use crate::palette;
use crate::vimnav;

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

/// Persistent state for one open keybinding dialog session — the
/// analogue of `palette::PaletteState`. Holds no `gpui` types (see the
/// module doc's "Architecture" section for why the scroll handle lives
/// beside this instead of inside it), so every transition here is
/// unit-testable without a window.
#[derive(Debug)]
pub struct KeybindingsState {
    /// Index into the **filtered** row list ([`visible_rows`]), not into
    /// the full one — the palette's convention, and what
    /// `vimnav::apply` clamps against. Row identity for clicks and
    /// rebinds is resolved through `visible_rows(..)[selected].row`.
    pub selected: usize,
    /// `Some(pending)` while listening for a new binding — `pending` is
    /// the keystroke sequence captured so far, appended to by
    /// [`press_while_listening`] on every keystroke except a bare
    /// `enter`/`escape`. `None` in ordinary list-navigation mode. While
    /// this is `Some`, `ShellView::dialog_input` is deliberately blurred
    /// so raw keystrokes reach this dialog instead of the filter (see
    /// the module doc's "Rebind capture" note).
    pub listening: Option<Vec<Keystroke>>,
    /// The filter query, mirrored here from `ShellView::dialog_input` by
    /// the `InputEvent::Change` subscription in `ShellView::new`. The
    /// `Input` owns the text; this is the pure copy the row list is
    /// ranked against. It survives leaving filter mode — the first rung
    /// of the `escape` ladder keeps the query applied, because leaving a
    /// search should leave you on the match, not undo the search.
    pub query: String,
    /// Which mode this dialog is in
    /// (`docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md`).
    /// `Normal` on open: bare letters are verbs, and `dialog_input` is
    /// blurred to `shell.focus_handle` so they reach [`handle_key`] —
    /// the same switch rebind capture has always performed, held open
    /// rather than momentary. Every focus decision in this module (open,
    /// capture cancel/commit, a selecting click) reads this rather than
    /// assuming the filter, so the focused surface and the painted mode
    /// can never disagree.
    pub mode: DialogMode,
    /// A one-line report about the keystroke *just* pressed, painted in
    /// the footer above the hint row and cleared by the next normal-mode
    /// command. It exists for exactly one situation: a verb the user
    /// pressed deliberately that had nothing to do — `r` on a row with
    /// no user override, `d` on a row that is already unbound. A verb
    /// that silently does nothing is the defect class this whole model
    /// exists to remove, so "nothing happened" is *said*, not implied by
    /// an unchanged screen.
    ///
    /// This is **not** the forbidden cache of a write's outcome (see the
    /// module doc's "What this dialog does NOT do"): it never records
    /// what a binding now is, and no row is ever rendered from it. It
    /// only records that a keystroke declined to write, which is
    /// knowledge no later re-derivation of the rows could recover —
    /// precisely because nothing was written.
    pub notice: Option<String>,
}

/// Hand-written rather than derived so the opening mode is one explicit,
/// greppable line: `DialogMode` has no `Default` of its own on purpose
/// (a filter-only surface has no mode at all — see [`DialogMode`]), and
/// deriving one there would quietly make "normal" the answer for every
/// future surface instead of this dialog's own stated choice.
impl Default for KeybindingsState {
    fn default() -> Self {
        Self {
            selected: 0,
            listening: None,
            query: String::new(),
            mode: DialogMode::Normal,
            notice: None,
        }
    }
}

impl KeybindingsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the query — the pure half of the `InputEvent::Change`
    /// subscription. Resets the selection to the top match (the
    /// palette's `set_query` semantics: after an edit the old index
    /// points at an unrelated row) and cancels any in-progress capture,
    /// since typing is an external interruption to it exactly as a click
    /// is ([`click_selects_or_listens`]).
    /// A notice names the row a verb was pressed on ("Toggle command
    /// palette has no user override to reset"); an edit re-ranks the
    /// list and moves the selection off that row, so keeping it would
    /// leave a complaint pointing at nothing.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.selected = 0;
        self.listening = None;
        self.notice = None;
    }
}

/// The rows this dialog currently shows, ranked — [`crate::listfilter::rank`]
/// over each row's [`searchable_text`]. Derived fresh at every call site
/// (render, key handling, click resolution), never cached: the same
/// no-caching contract [`derive_rows`] itself has, and the row counts
/// here are small enough that one ranking pass costs nothing measurable.
pub fn visible_rows(state: &KeybindingsState, rows: &[KeybindingRow]) -> Vec<Ranked> {
    let texts: Vec<String> = rows.iter().map(searchable_text).collect();
    listfilter::rank(&texts, &state.query)
}

/// Where the row for `clicked` currently sits in the *filtered* list, or
/// `None` if the filter is hiding it. Row click handlers stay keyed by
/// [`ActionId`] — identity, never position, so a row survives the list
/// being reordered under it — and this is the one place that identity is
/// turned back into the index [`KeybindingsState::selected`] speaks.
pub fn filtered_position(
    visible: &[Ranked],
    rows: &[KeybindingRow],
    clicked: &ActionId,
) -> Option<usize> {
    visible
        .iter()
        .position(|m| rows.get(m.row).is_some_and(|r| &r.action == clicked))
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

/// What a click on filtered position `clicked_ix` does to already-open
/// dialog state (brief: "clicking the already-selected row starts
/// listening"). Clicking any *other* row just selects it — and,
/// symmetrically, cancels an in-progress capture on the previously
/// selected row rather than leaving it dangling on a row that's no longer
/// selected. Clicking the currently *listening* row again (an edge case
/// the brief doesn't spell out) resolves the same way as clicking away:
/// the click always changes something about the row it lands on, so the
/// only case that leaves the dialog holding a `listening` state afterward
/// is a fresh click on an already-selected, not-yet-listening row.
pub fn click_selects_or_listens(state: &mut KeybindingsState, clicked_ix: usize) {
    if state.selected == clicked_ix && state.listening.is_none() {
        state.listening = Some(Vec::new());
    } else {
        state.selected = clicked_ix;
        state.listening = None;
    }
}

/// The text one row exposes to the filter: title and category — exactly
/// what the row displays, and nothing more. The action id deliberately
/// does NOT participate (a user-visible bug caught in review: `mo`
/// matched "Toggle light/dark theme" via the invisible `theme::
/// toggle_mode`, with no highlight to explain why, while "Move down" glowed
/// further down the list) — matching text the user can't see breaks the
/// rank/highlight agreement this feature depends on.
pub fn searchable_text(row: &KeybindingRow) -> String {
    format!("{} {}", row.title, row.category)
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
        // `false`: this dialog opens in normal mode, so the filter must
        // NOT own focus — a focused `Input` would eat every bare letter
        // as text before [`handle_key`] could read it as a verb. The
        // shared field is still emptied on open (`open_shell_dialog_
        // with_key` does that unconditionally), so the first `/` session
        // starts from the same blank slate `state.query` does.
        false,
    );
}

/// One keystroke as a small muted pill — the look of gpui-component's
/// `Kbd`, but labeled with [`palette::render_keystroke`]'s lowercase
/// `ctrl+k` text in the data face, matching how the palette and which-key
/// render bindings (`Kbd` itself hardwires uppercase key names, so this
/// dialog stopped routing through it). `pub(crate)` since the
/// settings-dialog rewrite: `settings_view`'s footer hints reuse the exact
/// same chip rather than growing a second, drifting copy.
pub(crate) fn key_chip(ks: &Keystroke, fg: Hsla, bg: Hsla) -> AnyElement {
    div()
        .font_family(crate::fonts::MONO)
        .text_xs()
        .text_color(fg)
        .bg(bg)
        .px_1()
        .py_0p5()
        .min_w_5()
        .text_center()
        .rounded(px(4.))
        .flex_shrink_0()
        .child(palette::render_keystroke(ks))
        .into_any_element()
}

/// The [`dialog::ModalKeyHandler`] for this dialog. Priority order:
///
/// 1. while listening, every keystroke is offered to
///    [`press_while_listening`] and swallowed unconditionally (`true`) —
///    even `escape`, which must cancel the capture rather than falling
///    through to `handle_key_down`'s "escape closes the modal". Capture
///    keeps first refusal deliberately: it is a third, momentary mode
///    whose whole job is to read raw keystrokes, so routing it through
///    [`dialogmode::normal_command`] would turn the `j` a user is trying
///    to bind into a motion;
/// 2. in [`DialogMode::Normal`], `escape` walks
///    [`dialogmode::escape_step`]'s ladder and every other keystroke goes
///    through [`dialogmode::normal_command`] — including keys it does not
///    claim, which are swallowed (`true`) rather than passed on: normal
///    mode's contract is that a stray letter does nothing, and letting it
///    fall through would hand it to whatever the shell does with that key
///    next;
/// 3. in [`DialogMode::Filter`] the pre-modal behaviour is unchanged,
///    with one addition: `escape` leaves filter mode (keeping the query)
///    instead of closing the dialog;
/// 4. bare `enter` starts listening on the selected row and blurs the
///    filter input, so the capture sees raw keystrokes (see the module
///    doc's "Rebind capture") — reached as
///    [`NormalCommand::Commit`] in normal mode and directly in filter
///    mode;
/// 5. [`listfilter::nav_command`] motions move the selection within the
///    *filtered* list, in both modes;
/// 6. bare `tab`/`shift+tab` are claimed and dropped — returns `true`
///    without acting. They are the settings dialog's stepping keys,
///    reserved and deliberately inert here. Claiming them (not just
///    falling through) is what actually makes them inert: with a focused
///    `Input`, an unclaimed key continues to the window's text-input
///    phase (spec §3) rather than simply vanishing, and
///    `InputState::normalize_input` strips only `\n`/`\r` from an
///    inserted edit — not `\t` — so an unclaimed `tab` would land in the
///    filter as a literal tab character and collapse the list to "no
///    matches";
/// 7. in filter mode, everything else returns `false`, unhandled — which
///    for a printable key is exactly right: the modal branch in
///    `handle_key_down` only stops propagation for keys this handler
///    claims, so an unclaimed character goes on to the focused `Input`'s
///    own text-insertion phase (the same reasoning
///    `handle_palette_key`'s catch-all arm carries). The one other
///    `false` is the ladder's last rung — `escape` in normal mode with an
///    empty query — which is how the shell's own modal branch gets to
///    close the dialog.
///
/// A commit that exactly re-captures the row's already-effective binding
/// ([`is_same_key_recapture`]) skips [`spawn_rebind`] entirely — nothing
/// would change on disk, so there's nothing to write.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let user_dir = shell.user_dir.clone();
    let input = shell.dialog_input.clone();
    let Some(state) = shell.keybindings.as_mut() else {
        return false;
    };
    // A notice reports on the keystroke (or click) that produced it and
    // nothing else, so it is dropped at the DOOR — here, and in
    // [`on_row_clicked`] — rather than in each branch that happens to
    // move the selection. Fix round 1 found four such branches (this
    // function's normal-mode `match`, the `EscapeStep::ClearQuery` rung,
    // the unclaimed-key early return, and the click path) of which only
    // the first cleared it; a complaint about the row you were on,
    // hanging under the row you are on now, is precisely the stale lie
    // the notice exists to avoid. Two doors is the whole surface: every
    // change to `selected` or `mode` arrives through one of them, and an
    // arm that wants to say something sets a fresh notice after this
    // line.
    //
    // `take` + a conditional `notify` rather than a bare assignment: two
    // of those four paths (`normal_command`'s claim-and-drop early
    // return, and a click that resolves to no row) return without
    // notifying, so a cleared notice would stay painted until something
    // else happened to request a frame.
    if state.notice.take().is_some() {
        cx.notify();
    }
    let visible = visible_rows(state, &rows);

    if let Some(pending) = state.listening.as_mut() {
        let outcome = press_while_listening(pending, ks);
        // Read before the arms below borrow `state` again — and read at
        // all rather than assumed: capture is momentary, so ending it
        // must return focus to whichever surface the *underlying* mode
        // owns (see the module doc's "Rebind capture").
        let back_to_filter = state.mode == DialogMode::Filter;
        match outcome {
            CaptureOutcome::Continue => {}
            CaptureOutcome::Cancel => {
                state.listening = None;
                if back_to_filter {
                    input.read(cx).focus_handle(cx).focus(window, cx);
                } else {
                    shell.focus_handle.focus(window, cx);
                }
            }
            CaptureOutcome::Commit(keystrokes) => {
                state.listening = None;
                let selected = state.selected;
                if back_to_filter {
                    input.read(cx).focus_handle(cx).focus(window, cx);
                } else {
                    shell.focus_handle.focus(window, cx);
                }
                if let Some(row) = visible.get(selected).and_then(|m| rows.get(m.row))
                    && !is_same_key_recapture(row, &keystrokes)
                {
                    spawn_rebind(row, keystrokes, user_dir, cx);
                }
            }
        }
        cx.notify();
        return true;
    }

    if state.mode == DialogMode::Normal {
        // Modifiers are ignored on `escape` here and in filter mode
        // below: `handle_key_down`'s own close never looked at them
        // (`event.keystroke.key == "escape"`), so a bare-only guard would
        // turn `shift+escape` from "close the dialog" into a key normal
        // mode claims and drops — visibly nothing. The one place a
        // modified escape still differs is rebind capture, which treats
        // it as a capturable keystroke on purpose (see
        // [`press_while_listening`]) and has already returned above.
        if ks.key == "escape" {
            // `has_previous_stage: false` — this dialog is one flat list,
            // with no nested stage to step back into (the picker's own
            // two-stage shape is what that rung exists for). The
            // `PreviousStage` and `LeaveFilter` arms are therefore
            // unreachable here; they are kept folded into the catch-all
            // rather than special-cased away, because `escape_step` is
            // the one ladder every modal surface walks and forking it per
            // call site is how the rungs drift apart.
            match dialogmode::escape_step(state.mode, state.query.is_empty(), false) {
                EscapeStep::ClearQuery => {
                    state.query.clear();
                    state.selected = 0;
                    // The viewport has to follow the selection here for
                    // the same reason it does on every motion: clearing a
                    // filter re-expands the list under a scroll offset
                    // that is still parked where the *filtered* list left
                    // it, so row 0 would be above the top of the screen
                    // with only the index having moved. `ShellView::new`'s
                    // query-change subscription pairs `set_query` with
                    // this same call for exactly that reason, and it
                    // cannot cover this path: `set_value` deliberately
                    // emits no `InputEvent::Change`.
                    shell.keybindings_scroll.scroll_to_item(0);
                    // The `Input` owns the text; clearing only the
                    // mirrored copy would leave the old query waiting in
                    // the field for the next `/`.
                    input.update(cx, |i, cx| i.set_value("", window, cx));
                    cx.notify();
                    return true;
                }
                _ => return false, // let the shell's modal branch close it
            }
        }
        let Some(cmd) = dialogmode::normal_command(ks) else {
            // Claimed and dropped: in normal mode a key with no meaning
            // does nothing at all, rather than falling through to the
            // shell (which is still listening underneath the modal).
            return true;
        };
        match cmd {
            NormalCommand::Nav(nav) => {
                state.selected = vimnav::apply(state.selected, visible.len(), nav);
                let selected = state.selected;
                shell.keybindings_scroll.scroll_to_item(selected);
            }
            NormalCommand::EnterFilter => {
                state.mode = DialogMode::Filter;
                // The one switch, thrown the other way: the filter takes
                // focus and printable keys become text again.
                input.read(cx).focus_handle(cx).focus(window, cx);
            }
            NormalCommand::Commit => {
                begin_capture(state, visible.len(), &shell.focus_handle, window, cx);
            }
            // The two write verbs (spec §8): `d` silences the selected
            // row's effective binding, `r` removes the user's override
            // so the layer beneath shows through. Both hand back the
            // notice to show when they declined to write — see
            // [`unbind_selected`]/[`reset_selected`].
            NormalCommand::Verb('d') => {
                let row = visible.get(state.selected).and_then(|m| rows.get(m.row));
                state.notice = unbind_selected(row, &user_dir, cx);
            }
            NormalCommand::Verb('r') => {
                let row = visible.get(state.selected).and_then(|m| rows.get(m.row));
                state.notice = reset_selected(row, &user_dir, cx);
            }
            // `Toggle`, `EditText`, `MoveItem` and any other verb belong
            // to surfaces that have something to toggle, edit or reorder;
            // this one has a flat list of actions and does neither.
            _ => {}
        }
        cx.notify();
        return true;
    }

    // ---- Filter mode: the pre-modal behaviour, unchanged -------------

    if ks.key == "escape" {
        // The ladder's first rung ([`EscapeStep::LeaveFilter`]), which
        // must be claimed (`true`) — falling through would close the
        // whole dialog on the escape that was only meant to leave the
        // search. The query stays applied; blurring is what makes the
        // letters verbs again. Modifier-agnostic for the reason given at
        // the normal-mode guard above: a `shift+escape` that skipped
        // straight to the close rung would lose the user's filter.
        state.mode = DialogMode::Normal;
        shell.focus_handle.focus(window, cx);
        cx.notify();
        return true;
    }

    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        begin_capture(state, visible.len(), &shell.focus_handle, window, cx);
        cx.notify();
        return true;
    }

    if let Some(cmd) = listfilter::nav_command(ks) {
        state.selected = vimnav::apply(state.selected, visible.len(), cmd);
        let selected = state.selected;
        shell.keybindings_scroll.scroll_to_item(selected);
        cx.notify();
        return true;
    }

    // `tab`/`shift+tab` are reserved (they step values in the settings
    // dialog, which has nothing to step here) — claimed and dropped
    // rather than left unhandled, because leaving them unhandled would
    // NOT make them inert: an unclaimed key continues past this handler
    // to the filter's own text-input phase (see this function's own doc
    // comment, item 7 — Task 3's renumbering moved that reasoning off
    // item 4, which is now bare `enter`), and a literal tab character in
    // the query would collapse the list to "no matches".
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

/// Start a rebind capture on the selected row: the body both modes'
/// `enter` shares, and the one place the capture's focus contract is
/// stated.
///
/// Hands focus to the shell root so the capture sees raw keystrokes —
/// with the filter focused, a bare letter would be consumed as text by
/// gpui-component's `Input` before ever reaching [`handle_key`] (see the
/// module doc's "Rebind capture"). It does that even from normal mode,
/// where the field is already blurred: the contract is "the shell root
/// owns the keys while capturing", asserted at every entrance rather than
/// inferred from wherever focus happened to be.
///
/// A no-op on an empty list — `enter` must not start listening on a row
/// that is not there (the "no matches" line is not a row).
///
/// Extracted rather than inlined twice because Task 4 edits the
/// normal-mode `match` this is called from: two copies of a body that
/// must stay identical would be a drift risk at exactly the wrong
/// moment. Takes `state` and the focus handle separately rather than
/// `&mut ShellView`, because every caller is already holding a `&mut`
/// borrow of `shell.keybindings` when it gets here.
fn begin_capture(
    state: &mut KeybindingsState,
    visible_len: usize,
    shell_focus: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    if visible_len == 0 {
        return;
    }
    state.listening = Some(Vec::new());
    shell_focus.focus(window, cx);
}

/// Selection/listening logic for a real mouse click on the row for
/// `clicked` (`ActionId`, resolved back to a position in the *filtered*
/// list against freshly derived rows — rows are never cached, see the
/// module doc). The gpui-facing wrapper around the pure
/// [`click_selects_or_listens`], and it moves focus the same way
/// [`handle_key`] does: a click that starts listening blurs the filter so
/// the capture sees raw keystrokes, and one that only selects hands focus
/// back to whichever surface the current mode owns — the filter in
/// [`DialogMode::Filter`] so typing keeps filtering, the shell root in
/// [`DialogMode::Normal`] so the letters stay verbs. Focusing the filter
/// unconditionally here (as this did before the dialog went modal) would
/// let a mouse click silently defeat normal mode.
fn on_row_clicked(
    shell: &mut ShellView,
    clicked: &ActionId,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let input = shell.dialog_input.clone();
    let Some(state) = shell.keybindings.as_mut() else {
        return;
    };
    // The dialog's other door — see [`handle_key`]'s own clear for why
    // this belongs at the entrance rather than beside each selection
    // change. A click is the one state change that never passes through
    // `handle_key` at all.
    if state.notice.take().is_some() {
        cx.notify();
    }
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {
        return;
    };
    click_selects_or_listens(state, ix);
    let listening = state.listening.is_some();
    let filter_mode = state.mode == DialogMode::Filter;
    let selected = state.selected;
    shell.keybindings_scroll.scroll_to_item(selected);
    if listening || !filter_mode {
        shell.focus_handle.focus(window, cx);
    } else {
        input.read(cx).focus_handle(cx).focus(window, cx);
    }
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
        tracing::warn!(
            target: "geode::config",
            "no writable user config dir; the new binding for {} was not saved",
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
                    tracing::warn!(
                        target: "geode::config",
                        "the previous binding for {} was not found where expected while \
                         saving the new one — it may still be reachable from wherever it \
                         actually lives",
                        rebind.action
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    target: "geode::config",
                    "failed to save the new binding for {}: {e}",
                    rebind.action
                ),
            }
        })
        .detach();
}

/// How a silenced binding comes back when the binding carried **no
/// context**. Exact, not a guess, for exactly that case: `begin_capture`
/// starts a capture on any row including an unbound one (it only checks
/// that the list is non-empty), and `apply_rebind`'s step 1 writes
/// through `set_key`, which *overwrites* an existing `keys` entry in
/// place — and with no context on either side, both writes land in the
/// same no-`context` `[[bindings]]` entry, so retyping the original
/// keystroke replaces the `"none"` this dialog just wrote rather than
/// appending beside it. `d` is therefore not a one-way door there, which
/// is why fix round 1 ruled for an acknowledgement instead of a
/// confirmation prompt: taxing every deliberate unbind to guard against
/// a recoverable mistake is the worse trade.
///
/// Use [`recovery`], never this constant directly — most builtin
/// bindings are contexted, and for those this sentence is false.
const RECOVERY: &str = "press enter and type that key again to restore it";

/// The way back for a **contexted** binding, which [`RECOVERY`] is not.
///
/// Whole-branch review, Important 2. `d` writes its `"none"` into the
/// `[[bindings]]` entry whose `context` matches the row's
/// ([`unbind_selected`] passes `bound.context_source`). A recovery
/// rebind, though, runs on a row that is unbound by then, so
/// `apply_rebind`'s `Rebind` takes `context: None`
/// (`row.current.and_then(|b| b.context_source)`) and `set_key` writes
/// the *no-`context`* entry — a different table, not an overwrite. Two
/// consequences, both bad enough to stop promising the retype: the
/// `"none"` shadow survives in the contexted entry, so whether the key
/// works again is decided by array order (the matcher is last-wins), and
/// even when it appears to work the binding has been escalated from
/// contexted to global.
///
/// This is the common case, not a corner: roughly 60 of the ~80 builtin
/// bindings carry a context (`workspace`, `tile`, `blotter && …` in
/// `crate::defaults`).
///
/// Making contexted recovery actually work needs `derive_rows` to carry
/// the row's pre-shadow context onto the row — the same row-vocabulary
/// change as the parked "`r` cannot lift a `"none"` shadow" follow-up
/// (spec §15), and deliberately not built here. Until then the honest
/// thing is to name the door that does open: the file itself.
const RECOVERY_CONTEXTED: &str = "undo that in keymap.toml — retyping the key would rebind it \
                                  globally instead of in that context";

/// Which of the two recovery sentences is true for a binding declared
/// under `context`. The one place that choice is made, so no call site
/// can quietly promise the wrong one.
fn recovery(context: Option<&str>) -> &'static str {
    match context {
        None => RECOVERY,
        Some(_) => RECOVERY_CONTEXTED,
    }
}

/// `d`: silence the selected row's currently-effective binding.
///
/// Returns the notice ([`KeybindingsState::notice`]) to show when the
/// keystroke deliberately declined to write, `None` when a write was
/// actually spawned. `row` is `None` only when the filter hid every row
/// ("no matches"), which is not a row the user was pointing at, so it
/// says nothing.
///
/// **The `is_user_layer` derivation, which is the dangerous input.** It
/// is read off [`BoundKey::layer`] — the layer `derive_rows` copied from
/// the very [`Binding`] this row is currently displaying, resolved by
/// [`effective_binding`]'s shadow-aware last-wins scan — and never
/// inferred from anything else. Getting it wrong is destructive in two
/// different directions (a wrong `true` deletes whatever the user
/// actually had on that key; a wrong `false` buries the user's own entry
/// under a redundant `"none"` so the key stays dead with nothing in the
/// file explaining why — see [`Unbind::is_user_layer`]), which is why
/// the layer travels with the binding from `derive_rows` rather than
/// being recomputed here from the action id.
///
/// One consequence worth stating, since it makes `d` and `r` coincide on
/// exactly one kind of row: when the effective binding IS the user's,
/// `is_user_layer` is `true`, so the write removes the key from the user
/// entry rather than shadowing it — which silences that keystroke (no
/// lower layer binds it, or the user would not have been the effective
/// layer for it) but lets the *action* fall back to whatever lower-layer
/// binding it has. That is the same removal `r` performs, and it is the
/// right shape: writing `"none"` over a key the user themselves put
/// there would leave a self-shadowing entry no reader could explain.
fn unbind_selected(
    row: Option<&KeybindingRow>,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    let row = row?;
    let Some(bound) = row.current.as_ref() else {
        // Nothing to silence. Said out loud rather than dropped: a verb
        // that appears inert is exactly what this interaction model
        // exists to remove.
        return Some(format!("{} is already unbound", row.title));
    };
    let key = palette::render_binding(&bound.keystrokes);
    let unbind = Unbind {
        context: bound.context_source.clone(),
        key: key.clone(),
        is_user_layer: bound.layer == Layer::User,
    };
    let way_back = recovery(bound.context_source.as_deref());
    spawn_unbind(unbind, row.action.0.clone(), user_dir, cx)
        // Present tense on purpose (whole-branch review, Minor 3): the
        // write is still on the background executor and can come back
        // `removed: false` — a stale row, or a user file that spells the
        // key differently from `render_binding` — in which case only
        // stderr ever says so. "silenced" asserted an outcome this
        // keystroke has not confirmed and cannot wait for (the row does
        // not relabel until the ~500ms watcher); "silencing" says what
        // is actually known, which is that the write was dispatched.
        .or_else(|| Some(format!("silencing {key} — {way_back}")))
}

/// `r`: remove the user's own override on the selected row, so the layer
/// beneath it shows through again.
///
/// Unlike [`unbind_selected`], the layer here is a *precondition*, not a
/// branch selector: reset is only meaningful when the row's effective
/// binding actually came from the user layer, and the [`Unbind`] it
/// builds is therefore always `is_user_layer: true`. That `true` is
/// still earned rather than asserted — the guard below is what
/// establishes it, so the one branch that can delete a user's binding is
/// only ever reached on a row whose binding demonstrably IS the user's.
///
/// A row with no user override is the case the brief singles out: a
/// silent no-op there would be a key that visibly does nothing, so it
/// returns a notice and writes nothing at all. Writing anyway would be
/// actively wrong, not merely redundant — the only thing `apply_unbind`
/// could write for a non-user row is the `"none"` shadow, which would
/// *silence* the very binding the user asked to restore.
///
/// It cannot lift a `"none"` shadow a previous `d` left over a builtin
/// binding: such a row derives as unbound (the shadow carries the
/// `"none"` action, not this row's, so [`effective_binding`] reports no
/// binding at all), leaving nothing on the row to name the key that
/// would have to be removed. That is a convenience gap, not a
/// correctness one — [`RECOVERY`] is the way back, and this function
/// says so on exactly that row rather than claiming the override does
/// not exist. Letting `r` lift a shadow directly would need
/// `derive_rows` to carry the suppressing entry onto the row; it is
/// recorded as a follow-up, deliberately not built here.
///
/// That same missing row vocabulary is why the unbound branch's message
/// hedges rather than calling [`recovery`]: with `row.current` gone
/// there is no `context_source` left to decide *which* recovery is true
/// (whole-branch review, Important 2), so it names both doors instead of
/// picking one it cannot justify.
fn reset_selected(
    row: Option<&KeybindingRow>,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    let row = row?;
    let Some(bound) = row.current.as_ref() else {
        // Unbound — which does NOT mean "no user override". The most
        // likely way a row gets here is the user's own `d`, whose
        // `"none"` shadow IS an override; it just carries the `"none"`
        // action rather than this row's, so `effective_binding` reports
        // no binding and the row has nothing left to name the key with.
        // Claiming there is no override would be false AND would steer
        // the user away from the recovery, so this says what is actually
        // true and points at it — at BOTH doors, because the row no
        // longer carries the context that would say which one opens (see
        // this function's own doc comment).
        return Some(format!(
            "{} is unbound — if you silenced it, {RECOVERY}, or undo it \
             in keymap.toml if it was context-scoped",
            row.title
        ));
    };
    if bound.layer != Layer::User {
        // A live binding from a layer this app never writes: there is
        // genuinely nothing of the user's to remove, and this is the one
        // case where "no user override" is the honest sentence.
        return Some(format!("{} has no user override to reset", row.title));
    }
    let key = palette::render_binding(&bound.keystrokes);
    let unbind = Unbind {
        context: bound.context_source.clone(),
        key: key.clone(),
        is_user_layer: true,
    };
    spawn_unbind(unbind, row.action.0.clone(), user_dir, cx)
        // Whole-branch review, Minor 3: saying nothing was the worst of
        // the three outcomes. A reset whose `apply_unbind` comes back
        // `removed: false` looked identical to one that worked (only
        // stderr knew), and even a reset that DID work is invisible
        // until the ~500ms watcher relabels the row — so `r` read as
        // inert, the defect class this interaction model exists to
        // remove. Present tense for the same reason `d` uses it: the
        // write is dispatched, not confirmed.
        .or_else(|| Some(format!("removing your {key} override")))
}

/// Run one [`apply_unbind`] on the background executor — the unbind twin
/// of [`spawn_rebind`], and the same contract in every respect: no file
/// I/O on the render thread (PHILOSOPHY: "nothing may stall the render
/// thread"), args captured, task spawned and detached, failures logged
/// as a warning from inside the task, and no touch of `shell`'s own
/// state — the row picks the change up when the reload watcher applies
/// it.
///
/// `Ok(UnbindOutcome { removed: false })` from a `is_user_layer: true`
/// write means the key was not where the row said it was (a stale read,
/// a `context` that does not match the entry it actually lives in) — the
/// [`Displacement::OldKeyNotFound`] contract restated for unbind, and
/// warned about for the same reason: the binding may still be live.
///
/// Returns a notice only for the one refusal it owns: no writable user
/// config dir at all, in which case nothing was even attempted.
fn spawn_unbind(
    unbind: Unbind,
    action: String,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    let Some(user_dir) = user_dir.clone() else {
        tracing::warn!(target: "geode::config",
            "no writable user config dir; the binding change for {action} was not saved"
        );
        return Some("no writable user config directory — nothing was saved".to_string());
    };
    cx.background_executor()
        .spawn(async move {
            match apply_unbind(&user_dir, &unbind) {
                Ok(outcome) if unbind.is_user_layer && !outcome.removed => {
                    tracing::warn!(target: "geode::config",
                        "the binding for {action} was not found where \
                         expected, so nothing was removed — it may still be reachable from \
                         wherever it actually lives"
                    )
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(target: "geode::config",
                    "failed to change the binding for {action}: {e}"
                ),
            }
        })
        .detach();
    None
}

/// Paint `text` with the fuzzy-match `indices` (char offsets into
/// `text`) highlighted — the index-based twin of the palette's own
/// `highlighted_title`, using the same pure
/// [`palette::highlight_runs`] char-index → merged-byte-range
/// conversion. Replaced this helper's old substring-query signature
/// when the dialogs moved from `/` find to fuzzy filtering: a
/// subsequence match has no single contiguous range to hand
/// `match_range`.
pub(crate) fn highlighted_text(text: &str, indices: &[usize], primary: Hsla) -> AnyElement {
    let runs = palette::highlight_runs(text, indices);
    if runs.is_empty() {
        return div().child(text.to_string()).into_any_element();
    }
    let style = HighlightStyle {
        color: Some(primary),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    StyledText::new(text.to_string())
        .with_highlights(runs.into_iter().map(|r| (r, style)))
        .into_any_element()
}

/// Split ranked `indices` (char offsets into a row's `searchable_text`,
/// `"{title} {category}"` — see `searchable_text` in this module and in
/// `settings_view`) back across the two label lines a row paints them on.
/// `title_len` is the title's own char count; the offset at exactly
/// `title_len` is the separating space and belongs to neither returned
/// list. Shared by both list dialogs' `build` (`keybindings_view` and
/// `settings_view`) rather than duplicated: the arithmetic is only
/// correct as long as *both* modules' `searchable_text` stays
/// `"{title} {category}"`, so one copy is what keeps a future separator
/// change from silently mis-highlighting whichever module didn't get the
/// memo.
pub(crate) fn split_label_indices(indices: &[usize], title_len: usize) -> (Vec<usize>, Vec<usize>) {
    let title_ix = indices.iter().copied().filter(|&i| i < title_len).collect();
    let cat_ix = indices
        .iter()
        .filter(|&&i| i > title_len)
        .map(|&i| i - title_len - 1)
        .collect();
    (title_ix, cat_ix)
}

/// The [`dialog::ShellModal::build`] closure body: a scrollable row list
/// (title + category on the left, the current binding as [`key_chip`]s —
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
    // Copied out so [`key_chip`] and the render closures below don't have
    // to hold the `theme` borrow.
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;

    // The list renders ONLY the rows that survive the filter. Safe
    // because row click handlers are keyed by `ActionId`, not position
    // (see [`filtered_position`]); the `debug_selector` index below stays
    // the row's index in the FULL list, so a row keeps its identity
    // across filtering.
    let visible = visible_rows(state, &rows);

    let mut list = v_flex()
        .id("keybindings-list")
        .w(px(WIDTH))
        .h(px(
            (visible.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT)
        ))
        .overflow_y_scroll()
        .track_scroll(&shell.keybindings_scroll)
        .debug_selector(|| "keybindings-list".to_string());

    for (position, m) in visible.iter().enumerate() {
        let row_ix = m.row;
        let row = &rows[row_ix];
        let is_selected = position == state.selected;
        let is_listening = is_selected && state.listening.is_some();

        let title_len = row.title.chars().count();
        let (title_ix, cat_ix) = split_label_indices(&m.indices, title_len);

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
            .child(highlighted_text(&row.title, &title_ix, theme.primary))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(highlighted_text(&row.category, &cat_ix, theme.primary)),
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
                    .children(pending.iter().map(|ks| key_chip(ks, chip_fg, chip_bg)))
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
                            .map(|ks| key_chip(ks, chip_fg, chip_bg)),
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
            .debug_selector(move || format!("keybindings-row-{row_ix}"))
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                entity_for_row.update(cx, |shell, cx| {
                    on_row_clicked(shell, &action, window, cx);
                });
            });

        list = list.child(row_el);
    }

    if visible.is_empty() {
        // Zero matches: one muted line where the rows would be — the same
        // muted treatment "unbound" gets, so an over-narrow filter reads
        // as a state, not a rendering glitch. Enter is inert here
        // (`handle_key` swallows it rather than listening on a row that
        // isn't there).
        list = list.child(
            div()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("no matches"),
        );
    }

    // Keystroke chips for the footer hints — the same [`key_chip`]
    // rendering the rows use, so key names in helper text look like the
    // keys they mean. A chip renders exactly one keystroke.
    let chip = move |spec: &str| {
        let ks = crate::keymap::parse_keystroke(spec, Modifiers::NONE)
            .expect("footer hint keystrokes are hardcoded valid");
        key_chip(&ks, chip_fg, chip_bg)
    };
    let sep = |text: &'static str| div().child(text).into_any_element();

    // The hint row states the CURRENT mode's vocabulary, not the union of
    // both: a modal surface's whole risk is a user who cannot tell which
    // mode they are in, and a footer listing keys that are inert right
    // now is exactly the lie the mode pill exists to prevent.
    let hint_line: AnyElement = if state.listening.is_some() {
        h_flex()
            .gap_1()
            .items_center()
            .flex_wrap()
            .children(vec![
                sep("Listening — type keys,"),
                chip("enter"),
                sep("to save,"),
                chip("escape"),
                sep("to cancel"),
            ])
            .into_any_element()
    } else {
        // Two rows, one idiom family each: motion, then rebind/escape —
        // so the hints read as a table rather than one wrapped run-on
        // line.
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
                    chip("enter"),
                    sep("rebind ·"),
                    // The two verbs normal mode exists to make room for
                    // (spec §8/§9): a chord has no clickable target and
                    // a bare letter has no visible one, so the footer is
                    // where `d` and `r` are discovered at all.
                    chip("d"),
                    sep("unbind ·"),
                    chip("r"),
                    sep("reset ·"),
                    chip("/"),
                    sep("filter ·"),
                    chip("escape"),
                    // Honest about which rung the next escape takes: with
                    // a query still applied it clears the query, and only
                    // then closes.
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
                vec![
                    chip("enter"),
                    sep("rebind the selected row ·"),
                    chip("escape"),
                    sep("back to normal"),
                ],
            ),
        };
        v_flex()
            .gap_0p5()
            .child(h_flex().gap_1().items_center().flex_wrap().children(motion))
            .child(h_flex().gap_1().items_center().flex_wrap().children(action))
            .into_any_element()
    };

    let footer = v_flex()
        .w(px(WIDTH))
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(theme.border)
        // The notice sits ABOVE the hints, in `theme.warning` rather
        // than the hints' muted grey: it is a report about the keystroke
        // just pressed ("… has no user override to reset"), and a verb
        // that wrote nothing has to say so somewhere the eye is already
        // going. `debug_selector` so a test can prove it painted rather
        // than only that the field was set.
        .children(state.notice.as_ref().map(|notice| {
            div()
                .text_sm()
                .text_color(theme.warning)
                .debug_selector(|| "keybindings-notice".to_string())
                .child(notice.clone())
        }))
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(hint_line),
        )
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .flex_wrap()
                .text_xs()
                .text_color(theme.muted_foreground)
                .children(vec![
                    chip("enter"),
                    sep("and"),
                    chip("escape"),
                    sep("can't be captured — hand-edit keymap.toml for those."),
                ]),
        );

    // The live `Input` is rendered only when it actually owns the
    // keystrokes — filter mode, capture closed. In normal mode, and while
    // listening, the same query paints as static muted text: a caret
    // blinking in a field that is not receiving the keys is the single
    // most misleading thing a modal surface can show.
    let frozen_query = (state.listening.is_some() || state.mode == DialogMode::Normal)
        .then_some(state.query.as_str());

    v_flex()
        .gap_2()
        // The mode badge sits above the filter, right-aligned, where the
        // eye already goes to check what a keystroke will do next. (It is
        // not in the modal's own title row: `dialog::render_modal` paints
        // that chrome from `ShellModal`'s fixed title alone, and it is
        // shared with every other modal — a per-dialog, per-frame mode
        // would have to be threaded through that type to live there.)
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

    // -- the filter (crate::listfilter) ---------------------------------

    /// Rows shaped like the real dialog's: title + category participate in
    /// the searchable text; the action id (invisible in the UI) must not.
    fn test_rows() -> Vec<KeybindingRow> {
        [
            ("a::one", "Close tile", "Workspace"),
            ("a::two", "Focus left", "Workspace"),
            ("a::three", "Toggle palette", "Palette"),
            ("a::four", "Focus right", "Workspace"),
        ]
        .map(|(id, title, category)| KeybindingRow {
            action: ActionId(id.to_string()),
            title: title.to_string(),
            category: category.to_string(),
            current: None,
        })
        .to_vec()
    }

    #[test]
    fn an_empty_query_shows_every_row_in_derivation_order() {
        let rows = test_rows();
        let state = KeybindingsState::new();
        let visible = visible_rows(&state, &rows);
        assert_eq!(
            visible.iter().map(|m| m.row).collect::<Vec<_>>(),
            (0..rows.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_query_narrows_and_reranks_the_visible_rows() {
        let rows = test_rows();
        let mut state = KeybindingsState::new();
        state.set_query("pal".to_string());
        let visible = visible_rows(&state, &rows);
        assert!(!visible.is_empty(), "'pal' must match the palette row");
        assert!(visible.len() < rows.len(), "'pal' must not match every row");
        let titles: Vec<&str> = visible.iter().map(|m| rows[m.row].title.as_str()).collect();
        assert!(
            titles[0].to_lowercase().contains("pal"),
            "best match first, got {titles:?}"
        );
    }

    #[test]
    fn a_query_matching_only_an_invisible_action_id_matches_nothing() {
        // "three" is a subsequence of `a::three` and of no title or
        // category in the fixture. The action id is invisible in the UI,
        // so matching it would select and highlight nothing the user can
        // see — the `/mo` bug [`searchable_text`]'s own doc comment
        // records, and a spec-level property of the match text.
        let rows = test_rows();
        let mut state = KeybindingsState::new();
        state.set_query("three".to_string());
        assert!(
            visible_rows(&state, &rows).is_empty(),
            "the action id is invisible in the UI — matching it would highlight nothing \
             and select a row for no visible reason"
        );
    }

    #[test]
    fn setting_a_query_resets_the_selection_to_the_top_match() {
        let mut state = KeybindingsState::new();
        state.selected = 3;
        state.set_query("z".to_string());
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn a_click_resolves_an_action_id_to_its_filtered_position() {
        // The list the user clicks is the filtered one, so a row's click
        // handler (keyed by ActionId, as it always was) must resolve to a
        // position in THAT list, not in the full one.
        let rows = test_rows();
        let mut state = KeybindingsState::new();
        state.set_query(rows[2].title.clone());
        let visible = visible_rows(&state, &rows);
        assert_eq!(
            filtered_position(&visible, &rows, &rows[2].action),
            Some(0),
            "the only match sits at filtered position 0, whatever its \
             position in the full list"
        );
    }

    #[test]
    fn a_click_on_a_row_the_filter_hid_resolves_to_nothing() {
        let rows = test_rows();
        let mut state = KeybindingsState::new();
        state.set_query(rows[2].title.clone());
        let visible = visible_rows(&state, &rows);
        let hidden = rows
            .iter()
            .find(|r| !visible.iter().any(|m| rows[m.row].action == r.action))
            .expect("the query must hide at least one row");
        assert_eq!(filtered_position(&visible, &rows, &hidden.action), None);
    }

    #[test]
    fn setting_a_query_cancels_an_in_progress_capture() {
        // The filter is always live; a query edit is an external
        // interruption to a capture in exactly the way a click is.
        let mut state = KeybindingsState::new();
        state.listening = Some(vec![key("a")]);
        state.set_query("foc".to_string());
        assert!(state.listening.is_none());
    }

    // -- split_label_indices ---------------------------------------------

    #[test]
    fn split_label_indices_partitions_around_the_separating_space() {
        // "Toggle palette Palette" — title "Toggle palette" is 14 chars
        // (indices 0..=13), index 14 is the separating space, category
        // "Palette" starts at 15.
        let title_len = "Toggle palette".chars().count();
        assert_eq!(title_len, 14);
        // One index from the title (0), the separator itself (14, must be
        // dropped by both sides), and one from the category (15, the
        // category's own first char).
        let (title_ix, cat_ix) = split_label_indices(&[0, 14, 15], title_len);
        assert_eq!(
            title_ix,
            vec![0],
            "the separator index must not land in the title half"
        );
        assert_eq!(
            cat_ix,
            vec![0],
            "a category-side index is rebased to be relative to the category's own start"
        );
    }

    #[test]
    fn split_label_indices_on_empty_indices_is_two_empty_lists() {
        let (title_ix, cat_ix) = split_label_indices(&[], 5);
        assert!(title_ix.is_empty());
        assert!(cat_ix.is_empty());
    }
}
