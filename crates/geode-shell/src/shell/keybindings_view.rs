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
//! ## The three verbs (spec §8, §20.1; reset-all 2026-09-19)
//!
//! `d` **unbinds** the selected row's currently-effective binding, `r`
//! **resets** the row's action to what the layers beneath say, and
//! `shift+r` **resets every action** — the capability that could not exist
//! while the filter owned every letter, and the reason this dialog went
//! modal at all. All three write only the *user* layer
//! (`crate::keymap_edit`), so they differ exactly where the layers do:
//!
//! - a binding from builtin or desk cannot be removed, so `d` silences
//!   it with the documented `"none"` shadow in the user entry
//!   ([`crate::keymap_edit::apply_unbind`]); a binding that IS the user's
//!   own is removed outright. Which branch is decided by
//!   [`BoundKey::layer`], carried onto the row by [`derive_rows`] from the
//!   very [`Binding`] the row displays — never re-inferred at the call
//!   site. See [`unbind_selected`] for why getting that backwards is
//!   destructive in two different directions;
//! - `r` removes **every** user-layer entry that overrides the row's
//!   action ([`KeybindingRow::overrides`], from
//!   [`crate::keymap::user_overrides_for`] over the whole keymap): a
//!   rebind's new key AND the `"none"` shadow the rebind left over the
//!   old one, or the bare shadow a `d` left. This is not the row's
//!   displayed binding — that is `None` on a shadowed row and only the new
//!   key on a rebound one, and a reset that read it (the rule before
//!   2026-09-19) removed the new key alone and left the action *unbound*
//!   rather than reset. One [`crate::keymap_edit::apply_reset`] write,
//!   keyed by the file's own key spelling ([`Binding::key_source`]), so
//!   `mod+h` is removed as `mod+h`, not as the `alt+h` it renders to;
//! - `shift+r` drops every `[[bindings]]` entry from the user keymap
//!   ([`crate::keymap_edit::apply_reset_all`]) — dialog-written and
//!   hand-written alike, by user ruling, since nothing tells them apart —
//!   and leaves the rest of the file (`config_version`, `mod`, comments
//!   outside the entries) as it was. Desk and builtin are never touched.
//!
//! Every verb reports in [`KeybindingsState::notice`], a line painted in
//! the footer and dropped at the next keystroke or click, and uses it to
//! acknowledge the write *immediately*, naming the key(s) and, for `d`
//! on a lower-layer binding, the way back ([`RECOVERY`]: `r`) — a disk
//! write needs an acknowledgement
//! that does not wait on the ~500ms reload before the row relabels.
//! Those acknowledgements are in the present tense ("silencing …",
//! "removing …") on purpose: the write is on the background executor and
//! can still come back short, so a completed tense would assert an
//! outcome the dialog has not confirmed. A verb that finds nothing to do
//! writes nothing and says that instead — `r` on a row with no user
//! override, `shift+r` with no user bindings. A key that visibly does
//! nothing is the defect class this interaction model exists to remove,
//! so "nothing happened" is stated in the footer rather than left to be
//! inferred from an unchanged screen.
//!
//! Each verb asks first where it would write ([`KeybindingConfirm`],
//! spec §20.1: `y`/`enter` or the yes button; `n`/`escape` withdraws),
//! and is unarmed — notice only — where it would not. The question is
//! [`confirm_prompt`]'s sentence, from the row and the live keymap, so
//! `r` names the key it removes (or how many) and `shift+r` the count.
//! The action bar paints each verb as a button only while it can act:
//! `d` on a bound row, `r` on a row with overrides, `shift+r` while the
//! user layer binds anything.
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
//! `enter` on the selected row, or a single click on any row (§17.1 rule
//! 2 — a click does what `enter` would, selecting and listening in one
//! step rather than the superseded two-click rule where a first click
//! only selected), starts "listening" for a new binding on the clicked
//! or selected row; every keystroke while listening appends to a pending
//! sequence (multi-keystroke bindings, e.g. `"g g"`, are supported);
//! `enter` commits it, `escape` cancels. A click on a different row
//! mid-capture retargets the capture to that row; a click on the row
//! already being listened on restarts the capture, dropping whatever
//! partial sequence had been typed.
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
//! in `Filter`, the shell root in `Normal`). No site in this module
//! performs either move: setting `listening` is the whole transition, and
//! [`dialog::sync_dialog_text`] reconciles gpui to it (spec §16.1) from
//! `dialogmode::focus_target`'s one decision, which reads `listening`
//! ahead of the mode. That is why the surface a capture returns to can no
//! longer be a hardcoded guess — a hardcoded filter would silently focus
//! it under a dialog still claiming to be in normal mode, and the next
//! `d` would type a `d` instead of unbinding. While
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
    AnyElement, App, Context, Entity, FontWeight, HighlightStyle, Hsla, MouseButton, Pixels,
    SharedString, StyledText, Window, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};

use geode_core::config::Layer;

use crate::actions::{ActionId, ActionRegistry};
use crate::dialogmode::{self, DialogMode, EscapeStep, NormalCommand};
use crate::footer::{Hint, HintRow};
use crate::keymap::{
    Binding, Keymap, Keystroke, Modifiers, UserOverride, effective_binding, user_overrides_for,
};
use crate::keymap_edit::{
    Displacement, Rebind, ResetOutcome, Unbind, apply_rebind, apply_reset, apply_reset_all,
    apply_unbind,
};
use crate::listfilter::{self, Ranked};
use crate::palette;
// The title/category index splitter lives in the palette's pure core
// (2026-09-12: the palette matches `"{title} {category}"` too) and is
// re-exported here for `settings_view`, which imported it from this module.
pub(crate) use crate::palette::split_label_indices;
use crate::vimnav;

use super::ShellView;
use super::dialog;
use super::scale;

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
    /// The key's spelling in its source document ([`Binding::key_source`]).
    /// A removal from the USER document must name the key the way that
    /// file does — `render_binding` says `alt+y` for a file that wrote
    /// `mod+y`, and `keys.remove("alt+y")` finds nothing.
    pub key_source: String,
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
    /// Every user-layer entry that overrides this action
    /// ([`user_overrides_for`]): a rebind's new key AND its `"none"`
    /// shadow, or the bare shadow a `d` left. What `r` removes — read
    /// from the whole keymap, never inferred from `current`, which is
    /// `None` on a shadowed row and only the new key on a rebound one.
    pub overrides: Vec<UserOverride>,
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
                key_source: b.key_source.clone(),
            });
            KeybindingRow {
                action: def.id.clone(),
                title: def.title.clone(),
                category: def.category.clone(),
                current,
                overrides: user_overrides_for(bindings, &def.id),
            }
        })
        .collect();
    rows.sort_by(|a, b| (&a.category, &a.title).cmp(&(&b.category, &b.title)));
    rows
}

/// The destructive question `d`/`r`/`shift+r` arm (spec §20.1): the same
/// ask-then-act shape the object dialog's `Confirm` has, on this dialog's
/// three verbs. `Unbind` writes the `"none"` shadow (or removes the
/// user's own entry); `Reset` removes every user override of the row's
/// action; `ResetAll` drops every user-layer binding. Armed only where
/// the write would actually happen — `d` on an unbound row, `r` on a row
/// with no user override and `shift+r` with no user bindings keep giving
/// their notices unarmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindingConfirm {
    Unbind,
    Reset,
    ResetAll,
}

impl KeybindingConfirm {
    pub fn yes_label(self) -> &'static str {
        match self {
            KeybindingConfirm::Unbind => "Unbind",
            KeybindingConfirm::Reset => "Reset",
            KeybindingConfirm::ResetAll => "Reset all",
        }
    }
}

/// The question the confirm row asks, from the selected row and the live
/// keymap. Pure and public so a test can read the exact sentence the
/// painter paints: `Reset` names the one key it removes or the count when
/// a rebind left two; `ResetAll` names the user-layer key count and says
/// what it leaves alone.
pub fn confirm_prompt(
    confirm: KeybindingConfirm,
    row: Option<&KeybindingRow>,
    bindings: &[Binding],
) -> String {
    let title = row.map(|r| r.title.as_str()).unwrap_or_default();
    match confirm {
        KeybindingConfirm::Unbind => {
            let key = row
                .and_then(|r| r.current.as_ref())
                .map(|b| palette::render_binding(&b.keystrokes))
                .unwrap_or_default();
            format!("Silence {key} for '{title}'?")
        }
        KeybindingConfirm::Reset => match row.map(|r| r.overrides.as_slice()) {
            Some([one]) => format!("Remove your {} override on '{title}'?", one.key),
            Some(many) => format!("Remove your {} overrides on '{title}'?", many.len()),
            None => format!("Remove your override on '{title}'?"),
        },
        KeybindingConfirm::ResetAll => format!(
            "Remove all {} of your keyboard shortcut overrides? Desk and built-in bindings \
             are untouched.",
            user_binding_count(bindings)
        ),
    }
}

/// How many keys the user layer binds — what `shift+r` would remove, and
/// `0` is where it gives its notice unarmed and paints no button. Counts
/// PARSED bindings: a user key that failed to parse, or names an unknown
/// action, is skipped by `build_keymap` with a diagnostic and is not
/// counted here, though `apply_reset_all` would remove it too. A file
/// whose only entries are such keys therefore reads as "nothing to
/// reset" — the diagnostics section names them, and the gap is accepted
/// over gating a destructive verb on unparsed text.
pub fn user_binding_count(bindings: &[Binding]) -> usize {
    bindings.iter().filter(|b| b.layer == Layer::User).count()
}

/// Persistent state for one open keybinding dialog session — the
/// analogue of `palette::PaletteState`. Holds no `gpui` types (see the
/// module doc's "Architecture" section for why the scroll handle lives
/// beside this instead of inside it), so every transition here is
/// unit-testable without a window.
#[derive(Debug)]
pub struct KeybindingsState {
    /// Index into the **filtered** row list ([`visible_rows`]), not into
    /// the full one — the palette's convention, and the `len` bound
    /// `vimnav::apply` moves within (wrapping a bare ±1, clamping a
    /// larger or counted step — spec §20.5). Row identity for clicks and
    /// rebinds is resolved through `visible_rows(..)[selected].row`.
    pub selected: usize,
    /// `Some(pending)` while listening for a new binding — `pending` is
    /// the keystroke sequence captured so far, appended to by
    /// [`press_while_listening`] on every keystroke except a bare
    /// `enter`/`escape`. `None` in ordinary list-navigation mode. While
    /// this is `Some`, `ShellView::dialog_input` is blurred — not by any
    /// site here, but because `dialogmode::focus_target` reads this ahead
    /// of the mode and [`dialog::sync_dialog_text`] applies it — so raw
    /// keystrokes reach this dialog instead of the filter (see the module
    /// doc's "Rebind capture" note).
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
    /// blurred in favour of the shell root so they reach [`handle_key`] —
    /// the same switch rebind capture has always performed, held open
    /// rather than momentary. This field is the whole truth about the
    /// focused surface: nothing in this module moves focus itself, and
    /// [`dialog::sync_dialog_text`] reconciles gpui to this after every
    /// transition (spec §16.1), so the focused surface and the painted
    /// mode cannot disagree.
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
    /// `Some` while `d`/`r`'s question stands (spec §20.1). Every other
    /// key is claimed and dropped until it is answered; a row click, the
    /// frozen-row click and the action buttons are dropped too.
    pub confirm: Option<KeybindingConfirm>,
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
            confirm: None,
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
    /// since typing is an external interruption to it — unlike a click
    /// ([`click_listens`]), which always starts a fresh capture rather
    /// than cancelling one.
    /// A notice names the row a verb was pressed on ("Toggle command
    /// palette has no user override to reset"); an edit re-ranks the
    /// list and moves the selection off that row, so keeping it would
    /// leave a complaint pointing at nothing.
    ///
    /// Clearing `listening` here is reached from the `Change`
    /// subscription, which is not one of `sync_dialog_text`'s seams — so
    /// it relies on an invariant rather than a reconcile: no `Change` can
    /// fire while a capture is listening, because the sync keeps the
    /// `Input` blurred for the whole capture and `dialog::filter_row`
    /// paints frozen text (not the live field) while the query is
    /// frozen, so nothing can type into it. If either of those ever
    /// changes, this line becomes a fourth, unreconciled seam.
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

/// §17.1 rule 2: a click on a row is the mouse form of moving the cursor
/// there and pressing `enter` — it selects and starts listening in one
/// step. A click on a different row mid-capture retargets the capture;
/// a click on the same row restarts it with the partial sequence
/// dropped. (Until 2026-09-12 the first click only selected and a
/// second on the same row listened — one step short for a mouse user.)
pub fn click_listens(state: &mut KeybindingsState, clicked_ix: usize) {
    state.selected = clicked_ix;
    state.listening = Some(Vec::new());
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
/// Target dialog content width, in pixels at the design rem
/// (`shell::scale`).
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
        // `false`: `focus_filter` is for a dialog with no mode, and this
        // one has one. Its initial focus comes from `open_shell_dialog_
        // with_key`'s own `dialog::sync_dialog_text` call instead (spec
        // §16.1) — which is why `view.keybindings` is set *above*, before
        // the door runs: the sync reads the `DialogMode::Normal` this
        // dialog opens in and parks the keys on the shell root, so a bare
        // letter reaches [`handle_key`] as a verb rather than being eaten
        // as text by a focused `Input`. The shared field is still emptied
        // on open (the door does that unconditionally), so the first `/`
        // session starts from the same blank slate `state.query` does.
        false,
    );
    // §18.1: the mode pill now lives in the modal's own title row rather
    // than in this dialog's content — see `dialog::render_modal`'s
    // `title_extra` slot.
    dialog::set_title_extra(view, |shell, cx| {
        shell
            .keybindings
            .as_ref()
            .map(|s| dialog::mode_pill(s.mode, cx))
            .unwrap_or_else(|| div().into_any_element())
    });
}

/// One keystroke as a small muted pill — the look of gpui-component's
/// `Kbd`, but labeled with [`palette::render_keystroke`]'s lowercase
/// `ctrl+k` text in the data face, matching how the palette and which-key
/// render bindings (`Kbd` itself hardwires uppercase key names, so this
/// dialog stopped routing through it). `pub(crate)` since the
/// settings-dialog rewrite: `settings_view`'s footer hints reuse the exact
/// same chip rather than growing a second, drifting copy.
pub(crate) fn key_chip(ks: &Keystroke, fg: Hsla, bg: Hsla, radius: Pixels) -> AnyElement {
    div()
        .font_family(crate::fonts::MONO)
        .text_xs()
        .text_color(fg)
        .bg(bg)
        .px_1()
        .py_0p5()
        .min_w_5()
        .text_center()
        .rounded(radius)
        .flex_shrink_0()
        .child(palette::render_keystroke(ks))
        .into_any_element()
}

/// Whether `d` on `row` would actually silence something — the row has
/// *some* effective binding, from any layer. `false` is where `d` gives
/// its "already unbound" notice unarmed rather than asking a question
/// with nothing behind it.
fn can_unbind(row: Option<&KeybindingRow>) -> bool {
    row.is_some_and(|r| r.current.is_some())
}

/// Whether `r` on `row` would actually remove something — the user layer
/// holds at least one override of the row's action ([`KeybindingRow::
/// overrides`]: a rebind, a shadow, or both). `false` (a builtin/desk
/// binding the user never touched, or an action bound nowhere) is where
/// `r` gives its "no user override" notice unarmed, since there is
/// nothing of the user's to remove. The row's displayed binding does not
/// decide this: a shadowed row shows nothing and still resets.
fn can_reset(row: Option<&KeybindingRow>) -> bool {
    row.is_some_and(|r| !r.overrides.is_empty())
}

/// The ONE arm-or-notice decision for `d`/`r`/`shift+r` (spec §20.1, controller
/// ruling: this was spelled three times — [`handle_key`]'s two `Verb`
/// arms and [`press_verb`] — and is now spelled once, called from both).
/// Arms the matching [`KeybindingConfirm`] where [`can_unbind`]/
/// [`can_reset`] holds; otherwise runs the existing unarmed writers
/// ([`unbind_selected`]/[`reset_selected`]) into `state.notice`, exactly
/// as before this dialog grew a confirm at all — so a keystroke or a
/// button press on a row with nothing to change still says so rather
/// than silently asking a question about doing nothing.
fn arm_verb(
    state: &mut KeybindingsState,
    key: char,
    row: Option<&KeybindingRow>,
    user_bindings: usize,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) {
    match key {
        'd' if can_unbind(row) => {
            state.confirm = Some(KeybindingConfirm::Unbind);
        }
        'd' => state.notice = unbind_selected(row, user_dir, cx),
        'r' if can_reset(row) => {
            state.confirm = Some(KeybindingConfirm::Reset);
        }
        'r' => state.notice = reset_selected(row, user_dir, cx),
        'R' if user_bindings > 0 => {
            state.confirm = Some(KeybindingConfirm::ResetAll);
        }
        'R' => state.notice = reset_all(user_bindings, user_dir, cx),
        _ => {}
    }
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
/// 4. bare `enter` starts listening on the selected row, which blurs the
///    filter input — through [`dialog::sync_dialog_text`] on this
///    handler's return, not here — so the capture sees raw keystrokes
///    (see the module doc's "Rebind capture"). Reached as
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
///
/// Every arm here is a **pure mutation** of `KeybindingsState` (spec
/// §16.1): none touches gpui focus or the shared `Input`'s text, which is
/// why `window` is unused. `ShellView::handle_key_down` calls
/// [`dialog::sync_dialog_text`] the moment this returns, claimed or not,
/// and that is the one place either is moved.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    _window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let user_bindings = user_binding_count(shell.services.keymap.bindings());
    let user_dir = shell.user_dir.clone();
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
        // Both ending arms are pure: clearing `listening` is the whole
        // transition, and `dialog::sync_dialog_text` hands focus back to
        // whichever surface the *underlying* mode owns (see the module
        // doc's "Rebind capture") on this handler's return.
        match outcome {
            CaptureOutcome::Continue => {}
            CaptureOutcome::Cancel => {
                state.listening = None;
            }
            CaptureOutcome::Commit(keystrokes) => {
                state.listening = None;
                let selected = state.selected;
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

    if let Some(confirm) = state.confirm {
        match dialog::ConfirmAnswer::from_key(ks) {
            Some(dialog::ConfirmAnswer::Yes) => {
                state.confirm = None;
                let row = visible.get(state.selected).and_then(|m| rows.get(m.row));
                state.notice = match confirm {
                    KeybindingConfirm::Unbind => unbind_selected(row, &user_dir, cx),
                    KeybindingConfirm::Reset => reset_selected(row, &user_dir, cx),
                    KeybindingConfirm::ResetAll => reset_all(user_bindings, &user_dir, cx),
                };
            }
            Some(dialog::ConfirmAnswer::No) => state.confirm = None,
            // Claimed and dropped while the question stands.
            None => {}
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
                    // The `Input` is emptied by `dialog::sync_dialog_text`
                    // on this handler's return — the mirrored copy is the
                    // only thing this rung touches (spec §16.1).
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
                // The one switch, thrown the other way — a pure mutation:
                // `dialog::sync_dialog_text` gives the filter focus on
                // this handler's return, and printable keys become text
                // again.
                state.mode = DialogMode::Filter;
            }
            NormalCommand::Commit => {
                begin_capture(state, visible.len());
            }
            // The two write verbs (spec §8, §20.1): [`arm_verb`] is the
            // ONE arm-or-notice decision, shared with [`press_verb`] so
            // the keyboard and the mouse can never disagree on which rows
            // arm a question.
            NormalCommand::Verb(key @ ('d' | 'r' | 'R')) => {
                let row = visible.get(state.selected).and_then(|m| rows.get(m.row));
                arm_verb(state, key, row, user_bindings, &user_dir, cx);
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
        // search. The query stays applied; the blur
        // `dialog::sync_dialog_text` performs on this handler's return is
        // what makes the letters verbs again. Modifier-agnostic for the
        // reason given at the normal-mode guard above: a `shift+escape`
        // that skipped straight to the close rung would lose the user's
        // filter.
        state.mode = DialogMode::Normal;
        cx.notify();
        return true;
    }

    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        begin_capture(state, visible.len());
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
/// Setting `listening` is the whole transition: `dialogmode::focus_target`
/// reads it and hands the keys to the shell root whatever the mode says,
/// so [`dialog::sync_dialog_text`] performs the blur on the handler's
/// return. That is what lets the capture see raw keystrokes — with the
/// filter focused, a bare letter would be consumed as text by
/// gpui-component's `Input` before ever reaching [`handle_key`] (see the
/// module doc's "Rebind capture").
///
/// A no-op on an empty list — `enter` must not start listening on a row
/// that is not there (the "no matches" line is not a row).
///
/// Extracted rather than inlined twice because Task 4 edits the
/// normal-mode `match` this is called from: two copies of a body that
/// must stay identical would be a drift risk at exactly the wrong
/// moment. Takes `state` rather than `&mut ShellView`, because every
/// caller is already holding a `&mut` borrow of `shell.keybindings` when
/// it gets here.
fn begin_capture(state: &mut KeybindingsState, visible_len: usize) {
    if visible_len == 0 {
        return;
    }
    state.listening = Some(Vec::new());
}

/// Selection/listening logic for a real mouse click on the row for
/// `clicked` (`ActionId`, resolved back to a position in the *filtered*
/// list against freshly derived rows — rows are never cached, see the
/// module doc). The gpui-facing wrapper around the pure
/// [`click_listens`], and it moves focus the same way [`handle_key`]
/// does — through [`dialog::sync_dialog_text`], the row-click seam of
/// that function's five seam classes (spec §16.1/§16.6/§17.1 rule 3), which a click
/// needs because it never passes through the key path at all. Every
/// click now starts listening (§17.1 rule 2), so this always blurs the
/// filter to let the capture see raw keystrokes — focusing the filter
/// unconditionally here (as this did before the dialog went modal) would
/// let a mouse click silently defeat normal mode.
fn on_row_clicked(
    shell: &mut ShellView,
    clicked: &ActionId,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
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
    // Spec §20.1: a click is claimed and dropped while a question stands —
    // the object dialog's tick-click rule (§18.9.2) on this surface.
    if state.confirm.is_some() {
        return;
    }
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {
        return;
    };
    click_listens(state, ix);
    let selected = state.selected;
    shell.keybindings_scroll.scroll_to_item(selected);
    dialog::sync_dialog_text(shell, window, cx);
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
        // A user-layer old key is REMOVED from the user file, so it must
        // be named by the file's spelling; a lower-layer one is shadowed
        // under a fresh key, where the rendered spelling is as good.
        old_key: row.current.as_ref().map(|b| {
            if b.layer == Layer::User {
                b.key_source.clone()
            } else {
                palette::render_binding(&b.keystrokes)
            }
        }),
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

/// How a silenced builtin or desk binding comes back: `r` on the same
/// row lifts the `"none"` shadow `d` wrote (2026-09-19 —
/// [`user_overrides_for`] finds the shadow by keystroke and context, so
/// the row does not need to display a key for `r` to know what to
/// remove). Exact for a contexted binding as well as a bare one: the
/// shadow lands in the entry whose `context` is the binding's own, and
/// that is the context the override set matches on. Before 2026-09-19
/// the recovery was `enter`-then-retype for a bare binding and "undo it
/// in keymap.toml" for a contexted one (the retype wrote the no-context
/// entry, escalating the binding to global); both sentences are gone
/// because both doors were worse than the one that now opens.
///
/// Not offered for `d` on the USER's own binding: that write removes the
/// key, and there is no lower entry for `r` to uncover it from — see
/// [`unbind_selected`].
const RECOVERY: &str = "press r to restore it";

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
/// One consequence worth stating: when the effective binding IS the
/// user's, `is_user_layer` is `true`, so the write removes the key from
/// the user entry rather than shadowing it — which silences that
/// keystroke (no lower layer binds it, or the user would not have been
/// the effective layer for it) but lets the *action* fall back to
/// whatever lower-layer binding it has. It is the right shape: writing
/// `"none"` over a key the user themselves put there would leave a
/// self-shadowing entry no reader could explain. It is NOT the same
/// write as `r` — `r` removes the action's whole override set (a rebind's
/// shadow included), this removes one displayed key — and `r` cannot
/// undo it, since the removed key was the user's and no lower entry
/// holds it; so this branch's notice does not promise [`RECOVERY`].
/// The removal names the key by [`BoundKey::key_source`], the file's own
/// spelling, for the reason that field's doc gives.
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
    let is_user_layer = bound.layer == Layer::User;
    let key = if is_user_layer {
        bound.key_source.clone()
    } else {
        palette::render_binding(&bound.keystrokes)
    };
    let unbind = Unbind {
        context: bound.context_source.clone(),
        key: key.clone(),
        is_user_layer,
    };
    spawn_unbind(unbind, row.action.0.clone(), user_dir, cx)
        // Present tense on purpose (whole-branch review, Minor 3): the
        // write is still on the background executor and can come back
        // `removed: false` — a stale row — in which case only stderr
        // ever says so. "silenced" asserted an outcome this keystroke
        // has not confirmed and cannot wait for (the row does not
        // relabel until the ~500ms watcher); "silencing" says what is
        // actually known, which is that the write was dispatched.
        .or_else(|| {
            Some(if is_user_layer {
                format!("removing your {key} binding")
            } else {
                format!("silencing {key} — {RECOVERY}")
            })
        })
}

/// `r`: remove every user-layer override of the selected row's action, so
/// the layers beneath show through again.
///
/// The set is [`KeybindingRow::overrides`] ([`user_overrides_for`] over
/// the whole keymap), not the row's displayed binding — the two differ in
/// exactly the cases that made the old `r` not reset: a rebind of a
/// builtin leaves the new key on the row and the `"none"` shadow off it
/// (removing only the new key left the action UNBOUND), and a `d` on a
/// builtin leaves the row unbound with the shadow as the only thing to
/// remove (the old `r` could only point at the recovery). Both go in one
/// [`apply_reset`] write.
///
/// A row with nothing of the user's is the case the brief singles out: a
/// silent no-op there would be a key that visibly does nothing, so it
/// returns a notice and writes nothing at all. That sentence is honest
/// now — the whole user layer was consulted, not one row's binding.
fn reset_selected(
    row: Option<&KeybindingRow>,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    let row = row?;
    if row.overrides.is_empty() {
        return Some(format!("{} has no user override to reset", row.title));
    }
    let what = match row.overrides.as_slice() {
        [one] => format!("your {} override", one.key),
        many => format!("your {} overrides", many.len()),
    };
    spawn_reset(row.overrides.clone(), row.action.0.clone(), user_dir, cx)
        // Present tense, as `d` and the old `r`: the write is dispatched,
        // not confirmed, and the row relabels only on the ~500ms watcher.
        .or_else(|| Some(format!("removing {what} on {}", row.title)))
}

/// `shift+r`: drop every user-layer binding, so every action is back on
/// what desk and builtin say. `user_bindings` is the count the caller
/// already gated the confirm on; `0` is the unarmed notice.
fn reset_all(
    user_bindings: usize,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    if user_bindings == 0 {
        return Some("no user keyboard shortcut overrides to reset".to_string());
    }
    let Some(user_dir) = user_dir.clone() else {
        return Some(no_user_dir_notice("every binding"));
    };
    cx.background_executor()
        .spawn(async move {
            match apply_reset_all(&user_dir) {
                Ok(ResetOutcome { removed }) => {
                    tracing::info!(target: "geode::config",
                        "reset all keyboard shortcuts: removed {removed} user bindings"
                    );
                }
                Err(e) => {
                    tracing::warn!(target: "geode::config", "failed to reset all keyboard shortcuts: {e}");
                }
            }
        })
        .detach();
    Some(format!(
        "removing all {user_bindings} of your keyboard shortcut overrides"
    ))
}

/// Run one [`apply_reset`] on the background executor — [`spawn_unbind`]'s
/// contract for the reset of one action: no file I/O on the render thread,
/// args captured, task detached, failures logged from inside the task, no
/// touch of `shell`'s own state. A `removed` short of the request is the
/// stale-belief warning [`UnbindOutcome::removed`] `false` is.
fn spawn_reset(
    overrides: Vec<UserOverride>,
    action: String,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    let Some(user_dir) = user_dir.clone() else {
        return Some(no_user_dir_notice(&action));
    };
    cx.background_executor()
        .spawn(async move {
            let asked = overrides.len();
            match apply_reset(&user_dir, &overrides) {
                Ok(ResetOutcome { removed }) if removed == asked => {
                    tracing::info!(target: "geode::config",
                        "reset {action}: removed {removed} user overrides"
                    );
                }
                Ok(ResetOutcome { removed }) => {
                    tracing::warn!(target: "geode::config",
                        "reset {action}: removed {removed} of {asked} user overrides — the \
                         keymap on disk differs from the one loaded"
                    );
                }
                Err(e) => {
                    tracing::warn!(target: "geode::config", "failed to reset {action}: {e}");
                }
            }
        })
        .detach();
    None
}

/// The one refusal every spawner owns: no writable user config dir at all,
/// in which case nothing was even attempted.
fn no_user_dir_notice(what: &str) -> String {
    tracing::warn!(target: "geode::config",
        "no writable user config dir; the binding change for {what} was not saved"
    );
    "no writable user config directory — nothing was saved".to_string()
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
        return Some(no_user_dir_notice(&action));
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

/// A verb pressed with the mouse (spec §20.1) — one door with the key
/// via [`arm_verb`] (the shared arm-or-notice decision [`handle_key`]'s
/// `Verb` arm also calls), so a button and its letter can never differ:
/// arms, never writes. Keeps its own guards a keystroke does not need —
/// clearing a standing notice, and refusing outright while a question is
/// already armed or a capture is listening — because a button click, and
/// so a second `press_verb` call before the first's write lands, can
/// happen with no keystroke in between at all.
fn press_verb(shell: &mut ShellView, key: char, window: &mut Window, cx: &mut Context<ShellView>) {
    let rows = derive_rows(&shell.services.registry, &shell.services.keymap);
    let user_bindings = user_binding_count(shell.services.keymap.bindings());
    let user_dir = shell.user_dir.clone();
    let Some(state) = shell.keybindings.as_mut() else {
        return;
    };
    if state.notice.take().is_some() {
        cx.notify();
    }
    if state.confirm.is_some() || state.listening.is_some() {
        return;
    }
    let visible = visible_rows(state, &rows);
    let row = visible.get(state.selected).and_then(|m| rows.get(m.row));
    arm_verb(state, key, row, user_bindings, &user_dir, cx);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// The action bar under the list: `d` where the row has a binding to
/// silence, `r` where it has a user override to remove, `shift+r` where
/// the user layer binds anything at all — `danger` outline buttons
/// showing their key chip, `keybindings-action-{key}`. Replaced by the
/// confirm row while a question stands.
fn action_block(
    state: &KeybindingsState,
    row: Option<&KeybindingRow>,
    bindings: &[Binding],
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    if let Some(confirm) = state.confirm {
        let on_yes: dialog::ConfirmHandler = Rc::new(|shell, window, cx| {
            let ks = Keystroke {
                mods: Modifiers::NONE,
                key: "y".to_string(),
            };
            handle_key(shell, &ks, window, cx);
        });
        let on_no: dialog::ConfirmHandler = Rc::new(|shell, _window, _cx| {
            if let Some(state) = shell.keybindings.as_mut() {
                state.confirm = None;
            }
        });
        return dialog::confirm_row(
            confirm_prompt(confirm, row, bindings),
            confirm.yes_label(),
            "keybindings",
            entity,
            on_yes,
            on_no,
            cx,
        );
    }
    let theme = cx.theme();
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    let chip_radius = theme.radius;
    // (chip spelling, the verb `arm_verb` reads, label)
    let mut verbs: Vec<(&'static str, char, &'static str)> = Vec::new();
    if state.listening.is_none() {
        if can_unbind(row) {
            verbs.push(("d", 'd', "Unbind"));
        }
        if can_reset(row) {
            verbs.push(("r", 'r', "Reset to lower layer"));
        }
        if user_binding_count(bindings) > 0 {
            verbs.push(("shift+r", 'R', "Reset all"));
        }
    }
    let mut bar = h_flex()
        .w(scale::design(WIDTH))
        .gap_2()
        .items_center()
        .debug_selector(|| "keybindings-actions".to_string());
    for (key, verb, label) in verbs {
        let ks = crate::keymap::parse_keystroke(key, Modifiers::NONE).expect("valid");
        let entity_for_action = entity.clone();
        let selector = format!("keybindings-action-{key}");
        let button = Button::new(SharedString::from(format!("keybindings-{key}")))
            .small()
            .outline()
            .danger()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(key_chip(&ks, chip_fg, chip_bg, chip_radius))
                    .child(label),
            )
            .on_click(move |_event, window, cx| {
                entity_for_action.update(cx, |shell, cx| press_verb(shell, verb, window, cx));
            });
        bar = bar.child(div().debug_selector(move || selector.clone()).child(button));
    }
    bar.into_any_element()
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
    let row_paint = super::listrow::row_paint(theme);
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    let chip_radius = theme.radius;

    // The list renders ONLY the rows that survive the filter. Safe
    // because row click handlers are keyed by `ActionId`, not position
    // (see [`filtered_position`]); the `debug_selector` index below stays
    // the row's index in the FULL list, so a row keeps its identity
    // across filtering.
    let visible = visible_rows(state, &rows);

    let mut list = v_flex()
        .id("keybindings-list")
        .w(scale::design(WIDTH))
        .h(scale::design(
            (visible.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT),
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
            .rounded(theme.radius);
        if is_selected {
            row_el = row_el.bg(row_paint.active).text_color(row_paint.text);
        } else {
            row_el = row_el.hover(|s| s.bg(row_paint.hover));
        }

        let label = v_flex()
            .gap_0p5()
            .child(highlighted_text(&row.title, &title_ix, row_paint.accent))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(highlighted_text(&row.category, &cat_ix, row_paint.accent)),
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
                            .map(|ks| key_chip(ks, chip_fg, chip_bg, chip_radius)),
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
                            .map(|ks| key_chip(ks, chip_fg, chip_bg, chip_radius)),
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
        key_chip(&ks, chip_fg, chip_bg, chip_radius)
    };
    let sep = |text: &'static str| div().child(text).into_any_element();

    // The hint rows state the CURRENT mode's vocabulary, not the union of
    // both: a modal surface's whole risk is a user who cannot tell which
    // mode they are in, and a footer listing keys that are inert right
    // now is exactly the lie the mode pill exists to prevent. Which row
    // a hint paints on is `crate::footer`'s call (move / edit / go, spec
    // §19), not this dialog's.
    let hints: Vec<Hint> = if state.confirm.is_some() {
        vec![
            Hint::prose(HintRow::Go, "this needs an answer first"),
            Hint::new(HintRow::Go, &["enter"], "go ahead"),
            Hint::new(HintRow::Go, &["escape"], "leave it alone"),
        ]
    } else if state.listening.is_some() {
        vec![
            Hint::prose(HintRow::Go, "listening — type keys"),
            Hint::new(HintRow::Go, &["enter"], "save"),
            Hint::new(HintRow::Go, &["escape"], "cancel"),
        ]
    } else {
        match state.mode {
            DialogMode::Normal => vec![
                Hint::new(HintRow::Move, &["j", "k"], "move"),
                Hint::new(HintRow::Move, &["ctrl+d", "ctrl+u"], "±5"),
                Hint::new(HintRow::Move, &["ctrl+f", "ctrl+b"], "±10"),
                Hint::new(HintRow::Edit, &["enter"], "rebind"),
                // The two verbs normal mode exists to make room for
                // (spec §8/§9). Since spec §20.1 they are buttons too
                // (`action_block`), so the footer is where the KEYS are
                // discovered — a bare letter has no visible target of
                // its own — rather than the verbs' only door.
                Hint::new(HintRow::Edit, &["d"], "unbind"),
                Hint::new(HintRow::Edit, &["r"], "reset"),
                Hint::new(HintRow::Edit, &["shift+r"], "reset all"),
                Hint::new(HintRow::Go, &["/"], "filter"),
                // Honest about which rung the next escape takes: with a
                // query still applied it clears the query, and only then
                // closes.
                Hint::new(
                    HintRow::Go,
                    &["escape"],
                    if state.query.is_empty() {
                        "close"
                    } else {
                        "clear the filter"
                    },
                ),
            ],
            DialogMode::Filter => vec![
                Hint::prose(HintRow::Move, "type to filter"),
                Hint::new(HintRow::Move, &["up", "down"], "move"),
                Hint::new(HintRow::Move, &["ctrl+d", "ctrl+u"], "±5"),
                Hint::new(HintRow::Move, &["ctrl+f", "ctrl+b"], "±10"),
                Hint::new(HintRow::Edit, &["enter"], "rebind the selected row"),
                Hint::new(HintRow::Go, &["escape"], "back to normal"),
            ],
        }
    };
    let hint_line: AnyElement = super::dialog::hint_rows(&hints, chip_fg, chip_bg, chip_radius);
    let footer = v_flex()
        .w(scale::design(WIDTH))
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
    //
    // `slash_filters` is false for the whole of a capture: `handle_key`
    // hands every keystroke to `press_while_listening` there, so `/` is
    // the binding being recorded, not the filter's key. This is the one
    // frozen state in the app where §18.1's `press / to filter`
    // placeholder would be a lie — see `dialog::FrozenFilter`.
    let frozen_query = (state.listening.is_some() || state.mode == DialogMode::Normal).then_some(
        dialog::FrozenFilter {
            query: state.query.as_str(),
            slash_filters: state.listening.is_none(),
            entity: entity.clone(),
        },
    );

    let row = visible.get(state.selected).and_then(|m| rows.get(m.row));

    v_flex()
        .gap_2()
        // §18.1: the mode badge lives in the modal's own title row now
        // (`dialog::render_modal`'s `title_extra` slot, set in `open` via
        // `dialog::set_title_extra`) rather than as a row here — the pill
        // was moved out of this dialog's content entirely.
        .child(dialog::filter_row(&shell.dialog_input, frozen_query, cx))
        .child(list)
        .child(action_block(
            state,
            row,
            shell.services.keymap.bindings(),
            entity,
            cx,
        ))
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

    /// A module's fragment binding (market-data documents §8.4) lists as
    /// `Layer::Builtin`, carrying its predicate like any other binding —
    /// which is what makes this dialog's `d` write a user-layer `"none"`
    /// shadow over it and its `r` leave it alone, with no branch here
    /// that has to know a module exists. Built through the roster and
    /// `splice`, the real path `main.rs` uses, so a fragment that stopped
    /// reporting `Builtin` (or stopped reaching the keymap at all) fails
    /// here rather than in a dialog test that could only say "no row".
    #[test]
    fn a_modules_fragment_binding_lists_as_a_builtin_layer_binding() {
        use crate::keymap::fragments::splice;
        let reg = registry_with(&[
            ("rec::noop", "Recording no-op", "Test"),
            ("palette::toggle", "Toggle command palette", "Palette"),
        ]);
        let mut factory = crate::module::recording::RecordingFactory::new("rec");
        factory.fragment =
            Some("[[bindings]]\ncontext = \"rec\"\n[bindings.keys]\n\"q\" = \"rec::noop\"\n");
        let mut roster = crate::module::ModuleRoster::new();
        roster.add(Box::new(factory));
        let (fragments, diags) = roster.keymap_fragments();
        assert!(diags.is_empty(), "{diags:?}");
        let builtin = doc(
            Layer::Builtin,
            "[[bindings]]\n[bindings.keys]\n\"ctrl+k\" = \"palette::toggle\"\n",
        );
        let layered = splice(&[builtin], &fragments);
        let (keymap, diags) = build_keymap(&layered, Modifiers::ALT, &reg);
        assert!(diags.is_empty(), "{diags:?}");
        let rows = derive_rows(&reg, &keymap);
        let row = rows
            .iter()
            .find(|r| r.action.0 == "rec::noop")
            .expect("the module's action has a row");
        let bound = row.current.as_ref().expect("and a binding");
        assert_eq!(bound.keystrokes[0].key, "q");
        assert_eq!(
            bound.layer,
            Layer::Builtin,
            "a fragment binding is part of what the binary shipped, so `r` restores it \
             and `d` shadows it exactly as for a shell default"
        );
        assert_eq!(bound.context_source.as_deref(), Some("rec"));
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

    // -- click_listens ----------------------------------------------------

    /// §17.1 rule 2: one click, capture at once — on any row, selected
    /// or not, listening or not (a repeat click restarts the capture
    /// with the partial sequence dropped).
    #[test]
    fn a_click_selects_and_listens_in_one_step() {
        let mut state = KeybindingsState {
            selected: 0,
            ..Default::default()
        };
        click_listens(&mut state, 3);
        assert_eq!(state.selected, 3);
        assert_eq!(state.listening, Some(Vec::new()));
        state.listening = Some(vec![key("a")]);
        click_listens(&mut state, 3);
        assert_eq!(
            state.listening,
            Some(Vec::new()),
            "a repeat click restarts the capture"
        );
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
                key_source: "ctrl+h".to_string(),
            }),
            overrides: Vec::new(),
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
            overrides: Vec::new(),
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
            overrides: Vec::new(),
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
        // interruption that cancels an in-progress capture outright —
        // unlike a click (`click_listens`), which never cancels one,
        // only ever starts a fresh capture on the row it landed on.
        let mut state = KeybindingsState::new();
        state.listening = Some(vec![key("a")]);
        state.set_query("foc".to_string());
        assert!(state.listening.is_none());
    }
}
