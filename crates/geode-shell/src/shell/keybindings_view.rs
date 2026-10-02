//! The keybinding dialog lists registered actions and their effective bindings, with
//! rebinding, unbind, and reset operations.
//!
//! Normal mode keeps the shared input blurred so bare letters act as commands. `/`
//! enters fuzzy list filtering and captures the entry query. Escape restores that
//! query; bare Enter keeps the edited query. Both return to Normal without starting
//! capture or closing the dialog. A subsequent Normal Enter starts capture on the
//! selected row. Clicking a row can start capture directly.
//!
//! Capture owns raw keys ahead of list commands: bare Enter commits a nonempty sequence
//! and bare Escape cancels. Pure dialog state owns mode, query, selection, and capture;
//! `dialog::sync_dialog_text` reconciles focus and input afterward.
//!
//! Rebinding validates and persists the complete user keymap through the ordered
//! configuration writer. Rows are prepared from the registry and keymap and
//! re-derived after every applied reload (`ShellView::config_revision`).

use std::path::PathBuf;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, HighlightStyle, Hsla, MouseButton, SharedString,
    StyledText, Window, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};

use geode_core::config::Layer;

use crate::actions::{ActionId, ActionRegistry};
use crate::dialogmode::{self, DialogMode, EscapeStep, NormalCommand};
use crate::footer::{Hint, HintRow};
use crate::keymap::{
    Binding, Keymap, Keystroke, Modifiers, UserOverride, effective_binding,
    effective_lower_binding, user_overrides_for,
};
use crate::keymap_edit::{
    Displacement, Rebind, ResetOutcome, Unbind, apply_rebind_clearing, apply_reset,
    apply_reset_all, apply_unbind_clearing,
};
use crate::listfilter::{self, Ranked};
use crate::palette;
use crate::prepared::{Prepared, RowText};
// Share the palette's title/category match-index splitter with settings.
pub(crate) use crate::palette::split_label_indices;
use crate::vimnav;

use super::ShellView;
use super::dialog;
use super::kbd;
use super::scale;

// ---------------------------------------------------------------------
// Pure core — no gpui. Row derivation and capture-state transitions.
// ---------------------------------------------------------------------

/// Displayed binding with the context, layer, and original key spelling needed to
/// construct a rebind or removal. The layer determines whether the old key is removed
/// from the user document or shadowed there.
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
    /// For a shared motion row only: the builtin/desk binding the action
    /// falls back to once `overrides` are gone ([`effective_lower_binding`]).
    /// A Motion row's rebind and unbind displace this key in the shared
    /// context, whatever `current` shows. `None` on every other row.
    pub base: Option<BoundKey>,
}

/// Derive registered actions with their display binding and user override set, sorted
/// by `(category, title)`. [`effective_binding`] accounts for shadows by key and
/// context source; it does not evaluate predicates against a live context stack.
/// [`user_overrides_for`] finds reset targets beyond the displayed key.
///
/// This is the derivation the prepared rows ([`KeybindingsState::rows`]) are built
/// from; tests and benches compare the prepared list against it.
pub fn derive_rows(registry: &ActionRegistry, keymap: &Keymap) -> Vec<KeybindingRow> {
    let bindings = keymap.bindings();
    let mut rows: Vec<KeybindingRow> = registry
        .iter()
        .map(|def| {
            let bound = |b: &Binding| BoundKey {
                keystrokes: b.keystrokes.clone(),
                context_source: b.context_source.clone(),
                layer: b.layer,
                key_source: b.key_source.clone(),
            };
            let base = crate::defaults::shared_motion_context(&def.id)
                .and_then(|_| effective_lower_binding(bindings, &def.id))
                .map(bound);
            KeybindingRow {
                action: def.id.clone(),
                title: def.title.clone(),
                category: def.category.clone(),
                current: effective_binding(bindings, &def.id).map(bound),
                overrides: user_overrides_for(bindings, &def.id),
                base,
            }
        })
        .collect();
    rows.sort_by(|a, b| (&a.category, &a.title).cmp(&(&b.category, &b.title)));
    rows
}

/// Confirmation for unbinding, resetting one action, or resetting all user keys. Arm
/// only when the loaded keymap identifies a target; otherwise report a notice. Unbind
/// removes a user key or shadows a lower-layer key. Reset removes the action's entire
/// user override set; ResetAll drops the user bindings array.
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

/// Build the displayed confirmation from the selected row and loaded keymap. Reset
/// names the key or override count; ResetAll names the parsed user-key count.
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

/// Count parsed user bindings for ResetAll's prompt and availability. Invalid keys or
/// unknown actions omitted by `build_keymap` are not counted, although
/// `apply_reset_all` would remove them too. A file containing only rejected bindings
/// therefore reports nothing to reset through this dialog.
pub fn user_binding_count(bindings: &[Binding]) -> usize {
    bindings.iter().filter(|b| b.layer == Layer::User).count()
}

/// Pure state for one open keybinding dialog. The scroll handle stays on the shell,
/// keeping state transitions testable without a window.
#[derive(Debug)]
pub struct KeybindingsState {
    /// Index into [`visible_rows`], bounded by filtered length. Bare single steps wrap;
    /// larger or counted moves clamp. Resolve row identity through the ranked match's
    /// `row` index before rebinding.
    pub selected: usize,
    /// Captured sequence, extended by every key except bare Enter/Escape. Capture owns
    /// routing before list-filter exits and forces the shared input to blur through
    /// `sync_dialog_text`, so letters arrive as raw binding keys.
    pub listening: Option<Vec<Keystroke>>,
    /// List-filter query mirrored from input changes and used to rank rows. Bare Enter
    /// keeps it when leaving Filter; Escape restores `filter_entry_query`. These exits
    /// neither start capture nor close the dialog.
    pub query: String,
    /// Query captured on entering list Filter and restored by Escape. This snapshot
    /// does not restore selection or participate in capture Enter/Escape handling.
    pub filter_entry_query: String,
    /// List mode, Normal on open. Normal sends commands to the shell root; Filter
    /// accepts text input. Capture takes precedence over either mode and forces input
    /// blur. `sync_dialog_text` reconciles these states with actual focus.
    pub mode: DialogMode,
    /// Latest refusal or dispatched-write notice, cleared by the next command or row
    /// click. It does not confirm write completion or supply displayed bindings; those
    /// still derive from the loaded keymap.
    pub notice: Option<String>,
    /// Armed destructive question. Other keys and list/action clicks are consumed until
    /// it is answered.
    pub confirm: Option<KeybindingConfirm>,
    /// The rows derived from the registry and keymap at `config_revision`, ranked
    /// for `query`. Render and every handler read this one list; the shell
    /// refreshes it (`ShellView::refresh_dialog_rows`).
    pub rows: Prepared<u64, String, KeybindingRow>,
    /// `user_binding_count` at the same derivation, for `shift+r` and its button.
    pub user_bindings: usize,
}

/// Opening mode is explicit per dialog; `DialogMode` has no global default.
impl Default for KeybindingsState {
    fn default() -> Self {
        Self {
            selected: 0,
            listening: None,
            query: String::new(),
            filter_entry_query: String::new(),
            mode: DialogMode::Normal,
            notice: None,
            confirm: None,
            rows: Prepared::new(),
            user_bindings: 0,
        }
    }
}

impl KeybindingsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mirror an input change into the query and reset selection to the top match.
    /// Cancel capture and clear notices because they refer to the previous row or
    /// query. Capture normally keeps this Input blurred and renders frozen text, so
    /// user typing cannot reach the Change subscription while it is listening.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.selected = 0;
        self.listening = None;
        self.notice = None;
    }

    /// Re-key the prepared rows. The registry is not part of the key: it is fixed
    /// once the shell is built.
    pub fn refresh_rows(
        &mut self,
        registry: &ActionRegistry,
        keymap: &Keymap,
        config_revision: u64,
    ) {
        let refreshed = self.rows.refresh(
            &config_revision,
            &self.query,
            || {
                derive_rows(registry, keymap)
                    .into_iter()
                    .map(|row| {
                        let text = RowText {
                            primary: row.title.clone().into(),
                            secondary: row.category.clone().into(),
                        };
                        (row, text)
                    })
                    .collect()
            },
            RowText::two_line,
            |_, texts, query| listfilter::rank(texts, query),
        );
        if refreshed == crate::prepared::Refreshed::Derived {
            self.user_bindings = user_binding_count(keymap.bindings());
        }
    }
}

/// Rank rows against their visible title/category text: the fresh ranking the
/// prepared rows must agree with. Render and handlers read [`KeybindingsState::rows`].
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

/// Select a clicked row and start a fresh capture, discarding any partial sequence.
/// This works in either list mode; keyboard Enter starts capture only in Normal, after
/// accepting or cancelling any active list-filter session.
pub fn click_listens(state: &mut KeybindingsState, clicked_ix: usize) {
    state.selected = clicked_ix;
    state.listening = Some(Vec::new());
}

/// Search title and category, matching the text available for highlighting. Exclude the
/// invisible action ID so a match always has visible evidence.
pub fn searchable_text(row: &KeybindingRow) -> String {
    format!("{} {}", row.title, row.category)
}

/// Detect a capture equal to the displayed binding. The caller skips persistence and
/// its reload cycle for this no-op. Any sequence on an unbound row is new. A
/// Motion row with an override outside its shared context is never a no-op:
/// the capture still has that override to clear ([`rebind_plan`]).
pub fn is_same_key_recapture(row: &KeybindingRow, new_keystrokes: &[Keystroke]) -> bool {
    let all_shared = match crate::defaults::shared_motion_context(&row.action) {
        None => true,
        Some(shared) => row
            .overrides
            .iter()
            .all(|o| o.context_source.as_deref() == Some(shared)),
    };
    all_shared
        && row
            .current
            .as_ref()
            .is_some_and(|bound| bound.keystrokes == new_keystrokes)
}

/// One dialog edit: the user overrides removed first, then an optional write,
/// in one transaction.
#[derive(Debug, Clone)]
pub struct EditPlan<W> {
    pub clear: Vec<UserOverride>,
    /// `None` when clearing alone completes the edit.
    pub write: Option<W>,
}

/// What committing `new_keystrokes` on `row` writes.
///
/// An ordinary row rebinds inside its displayed binding's context. A Motion
/// row's edit is global: it clears every user override of the action (old
/// per-module ids, their `"none"` shadows, and earlier shared edits), then
/// writes the new key under the shared context and shadows the fallback
/// ([`KeybindingRow::base`]) there. Writing into the displayed context would
/// land a module context while an old-id override is displayed, leaving the
/// other grid tiles on the old key. Capturing the fallback key itself only
/// clears.
pub fn rebind_plan(row: &KeybindingRow, new_keystrokes: &[Keystroke]) -> EditPlan<Rebind> {
    let new_key = palette::render_binding(new_keystrokes);
    let action = row.action.0.clone();
    if let Some(shared) = crate::defaults::shared_motion_context(&row.action) {
        let write = (row.base.as_ref().map(|b| b.keystrokes.as_slice()) != Some(new_keystrokes))
            .then(|| Rebind {
                context: Some(shared.to_string()),
                new_key,
                action,
                old_key: row
                    .base
                    .as_ref()
                    .map(|b| palette::render_binding(&b.keystrokes)),
                old_key_is_user_layer: false,
            });
        return EditPlan {
            clear: row.overrides.clone(),
            write,
        };
    }
    let rebind = Rebind {
        context: row.current.as_ref().and_then(|b| b.context_source.clone()),
        new_key,
        action,
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
    EditPlan {
        clear: Vec::new(),
        write: Some(rebind),
    }
}

/// What `d` on `row` writes; `None` on an unbound row.
///
/// An ordinary row removes its displayed user key or shadows its displayed
/// lower-layer key, in that binding's context. A Motion row clears every user
/// override of the action and shadows the fallback key under the shared
/// context, so the motion is silenced in every tile rather than only where an
/// old-id override was displayed. With no fallback, clearing alone unbinds it.
pub fn unbind_plan(row: &KeybindingRow) -> Option<EditPlan<Unbind>> {
    let bound = row.current.as_ref()?;
    if let Some(shared) = crate::defaults::shared_motion_context(&row.action) {
        return Some(EditPlan {
            clear: row.overrides.clone(),
            write: row.base.as_ref().map(|b| Unbind {
                context: Some(shared.to_string()),
                key: palette::render_binding(&b.keystrokes),
                is_user_layer: false,
            }),
        });
    }
    let is_user_layer = bound.layer == Layer::User;
    Some(EditPlan {
        clear: Vec::new(),
        write: Some(Unbind {
            context: bound.context_source.clone(),
            key: if is_user_layer {
                bound.key_source.clone()
            } else {
                palette::render_binding(&bound.keystrokes)
            },
            is_user_layer,
        }),
    })
}

// ---------------------------------------------------------------------
// Rendering (gpui) — everything above this line is the pure core.
// ---------------------------------------------------------------------

/// Estimated two-line row height for viewport sizing. Keyboard scroll-follow uses
/// `ScrollHandle::scroll_to_item`, which measures the actual layout.
const ROW_HEIGHT: f32 = 44.0;
/// Rows visible before the list scrolls — see `palette::VISIBLE_ROWS`.
const VISIBLE_ROWS: usize = 10;
/// Target dialog content width, in pixels at the design rem
/// (`shell::scale`).
const WIDTH: f32 = 640.0;

/// Open a fresh keybinding dialog through the shared modal lifecycle. Leave an
/// already-open modal intact.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if !dialog::can_open(view, dialog::DialogKind::Keybindings) {
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
        dialog::DialogKind::Keybindings,
        "Keyboard shortcuts",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        // Mode state is already installed, so shared synchronization parks Normal focus
        // on the shell. `focus_filter` is only for surfaces without a mode.
        false,
    );
    // Build the mode pill in the modal's title-extra slot.
    dialog::set_title_extra(view, |shell, cx| {
        shell
            .keybindings
            .as_ref()
            .map(|s| dialog::mode_pill(s.mode, cx))
            .unwrap_or_else(|| div().into_any_element())
    });
}

/// Whether the displayed binding gives `d` a removal or shadow target. An unbound row
/// reports a notice without arming confirmation.
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

/// Shared keyboard and pointer decision for `d`, `r`, and Shift-R. Arm a confirmation
/// when the loaded keymap supplies a target; otherwise populate the same refusal notice
/// without writing.
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

/// Route pending confirmation and capture before list-mode commands. Capture consumes
/// raw keys, including its own bare Enter/Escape controls, so they cannot also accept
/// or cancel a list filter.
///
/// Normal Enter starts capture; Normal Escape clears an applied query or lets the shell
/// close. Filter Escape restores the entry query and bare Enter keeps the edited query.
/// Both filter exits return to Normal without capture or closure. Restoring different
/// text resets selection and scrolling; keeping it retains the current match.
/// Navigation remains bounded by the filtered rows.
///
/// Consume Tab/Shift-Tab so they cannot insert literal tabs into the focused filter.
/// Other Filter input passes through for text entry; in Normal mode an unrecognized
/// bare key is consumed and an unrecognized chord is declined, so a dialog-opening
/// action stacked over this dialog can still reach the shell. Re-capturing the
/// effective binding skips persistence. Focus and input text are reconciled by
/// `sync_dialog_text` after this handler.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    _window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let user_dir = shell.user_dir.clone();
    let Some(state) = shell.keybindings.as_mut() else {
        return false;
    };
    // Clear the previous command's notice before any routing or early return. Notify on
    // removal so paths that otherwise request no frame cannot leave it painted under a
    // different selection. Pointer row entry does the same.
    if state.notice.take().is_some() {
        cx.notify();
    }
    let visible_len = state.rows.len();

    if let Some(pending) = state.listening.as_mut() {
        let outcome = press_while_listening(pending, ks);
        // Ending capture clears `listening`; shared input synchronization restores
        // focus according to the underlying list mode. A capture started by clicking in
        // Filter therefore returns to its filter input.
        match outcome {
            CaptureOutcome::Continue => {}
            CaptureOutcome::Cancel => {
                state.listening = None;
            }
            CaptureOutcome::Commit(keystrokes) => {
                state.listening = None;
                if let Some(row) = state.rows.at(state.selected)
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
                let row = state.rows.at(state.selected).cloned();
                let user_bindings = state.user_bindings;
                state.notice = match confirm {
                    KeybindingConfirm::Unbind => unbind_selected(row.as_ref(), &user_dir, cx),
                    KeybindingConfirm::Reset => reset_selected(row.as_ref(), &user_dir, cx),
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
        // Outside capture, Escape ignores modifiers just like the shell's modal-close
        // fallback. Capture has already returned above and can record modified Escape.
        if ks.key == "escape" {
            // This flat Normal-mode list has neither a previous stage nor Filter focus;
            // those ladder outcomes are unreachable here.
            match dialogmode::escape_step(state.mode, state.query.is_empty(), false) {
                EscapeStep::ClearQuery => {
                    state.query.clear();
                    state.selected = 0;
                    // Clearing the filter changes row indices; reset both selection and
                    // viewport.
                    shell.keybindings_scroll.scroll_to_item(0);
                    // The shell's modal key tail reconciles Input text and focus from
                    // state.
                    cx.notify();
                    return true;
                }
                _ => return false, // let the shell's modal branch close it
            }
        }
        let Some(cmd) = dialogmode::normal_command(ks) else {
            // A bare key with no meaning is claimed and dropped, same as
            // browse mode elsewhere. A chord is declined instead: capture
            // (`state.listening`, above) already claims every key when a
            // binding is being recorded, so this decline is reached only
            // outside capture, and is how the shell reaches a
            // dialog-opening action stacked over this dialog.
            return !ks.mods.is_chord();
        };
        match cmd {
            NormalCommand::Nav(nav) => {
                state.selected = vimnav::apply(state.selected, visible_len, nav);
                let selected = state.selected;
                shell.keybindings_scroll.scroll_to_item(selected);
            }
            NormalCommand::EnterFilter => {
                // Enter through the shared helper to snapshot the current query.
                dialogmode::enter_filter(
                    &mut state.mode,
                    &mut state.filter_entry_query,
                    &state.query,
                );
            }
            NormalCommand::Commit => {
                begin_capture(state, visible_len);
            }
            // Destructive verbs share confirmation and refusal handling with buttons.
            NormalCommand::Verb(key @ ('d' | 'r' | 'R')) => {
                let row = state.rows.at(state.selected).cloned();
                let user_bindings = state.user_bindings;
                arm_verb(state, key, row.as_ref(), user_bindings, &user_dir, cx);
            }
            // `Toggle`, `EditText`, `MoveItem` and any other verb belong
            // to surfaces that have something to toggle, edit or reorder;
            // this one has a flat list of actions and does neither.
            _ => {}
        }
        cx.notify();
        return true;
    }

    // ---- Filter mode -------------------------------------------------

    if let Some(exit) = dialogmode::filter_exit(ks) {
        // Claim both list-filter exits: Escape restores entry text and bare Enter keeps
        // the current query. Return to Normal without starting capture or closing.
        // Shared input synchronization restores text and blurs the input afterward.
        let changed = dialogmode::exit_filter(
            &mut state.mode,
            &state.filter_entry_query,
            &mut state.query,
            exit,
        );
        if changed {
            // The list re-expands under a scroll offset still parked
            // where the narrowed list left it, so the cursor goes to the
            // top match and the viewport follows — exactly what the
            // `ClearQuery` rung does for the same reason.
            state.selected = 0;
            shell.keybindings_scroll.scroll_to_item(0);
        }
        cx.notify();
        return true;
    }

    if let Some(cmd) = listfilter::nav_command(ks) {
        state.selected = vimnav::apply(state.selected, visible_len, cmd);
        let selected = state.selected;
        shell.keybindings_scroll.scroll_to_item(selected);
        cx.notify();
        return true;
    }

    // Consume Tab/Shift-Tab without acting. Passing them through would insert a literal
    // tab into the focused filter and change its matches.
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

/// Start capture on the selected row from Normal Enter. An empty filtered list has no
/// target. Setting `listening` makes focus synchronization blur the input regardless of
/// list mode, allowing raw keys to reach capture. Filter Enter returns to Normal first
/// and does not call this function.
fn begin_capture(state: &mut KeybindingsState, visible_len: usize) {
    if visible_len == 0 {
        return;
    }
    state.listening = Some(Vec::new());
}

/// Resolve the clicked ActionId against the prepared filtered rows and start a fresh
/// capture through `click_listens`. Unlike list-filter Enter, a row click can start
/// capture directly in either list mode. Synchronize input and focus afterward so
/// capture receives raw keys.
fn on_row_clicked(
    shell: &mut ShellView,
    clicked: &ActionId,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let Some(state) = shell.keybindings.as_mut() else {
        return;
    };
    // Clear command notices on pointer entry too, including paths that return without
    // selecting a row.
    if state.notice.take().is_some() {
        cx.notify();
    }
    // An armed question owns input until answered; ignore list clicks.
    if state.confirm.is_some() {
        return;
    }
    let Some(ix) = state.rows.position(|r| &r.action == clicked) else {
        return;
    };
    click_listens(state, ix);
    let selected = state.selected;
    shell.keybindings_scroll.scroll_to_item(selected);
    shell.refresh_dialog_rows(cx);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Submit a rebind through the ordered configuration writer. Capture arguments before
/// detaching; no file I/O or optimistic keymap mutation occurs on the UI thread. Errors
/// and [`Displacement::OldKeyNotFound`] are logged. The accepted watcher reload updates
/// bindings and dialog rows. [`rebind_plan`] decides the write.
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
    let EditPlan { clear, write } = rebind_plan(row, &new_keystrokes);
    let action = row.action.0.clone();
    crate::config_write::submit(&user_dir.clone(), cx.background_executor(), move || {
        let Some(rebind) = write else {
            // The capture is the shared fallback key: clearing is the edit.
            if let Err(e) = apply_reset(&user_dir, &clear) {
                tracing::warn!(target: "geode::config", "failed to reset {action}: {e}");
            }
            return;
        };
        match apply_rebind_clearing(&user_dir, &clear, &rebind) {
            Ok(outcome) if outcome.displacement == Displacement::OldKeyNotFound => {
                tracing::warn!(
                    target: "geode::config",
                    "the previous binding for {action} was not found where expected while \
                     saving the new one — it may still be reachable from wherever it \
                     actually lives"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(
                target: "geode::config",
                "failed to save the new binding for {action}: {e}"
            ),
        }
    })
    .detach();
}

/// Reset removes a lower-layer binding's user `"none"` shadow in its original context.
/// This recovery hint is not offered when Unbind removes a user key: reset cannot
/// reconstruct that deleted user entry.
const RECOVERY: &str = "press r to restore it";

/// Remove the selected user key or shadow a displayed lower-layer key with `"none"`.
/// The binding's own provenance determines which operation is valid; user removal uses
/// its original document spelling.
///
/// Removing a user key can reveal a lower-layer binding on the same key. It does not
/// reset the action's other overrides, and Reset cannot reconstruct the removed user
/// entry. A lower-layer shadow can be removed by Reset. A Motion row is the exception:
/// it clears every override and silences the shared fallback key ([`unbind_plan`]).
///
/// Return a refusal or dispatched-write notice; no visible row yields no notice.
/// Completion is asynchronous and rows change only after an accepted reload.
fn unbind_selected(
    row: Option<&KeybindingRow>,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    let row = row?;
    let Some(bound) = row.current.as_ref() else {
        // Report the missing target without submitting a write.
        return Some(format!("{} is already unbound", row.title));
    };
    let plan = unbind_plan(row).expect("a bound row has an unbind plan");
    let notice = match &plan.write {
        Some(u) if u.is_user_layer => format!("removing your {} binding", u.key),
        Some(u) => format!("silencing {} — {RECOVERY}", u.key),
        // A Motion row with no fallback: clearing the overrides unbinds it.
        None => format!("removing your {} binding", bound.key_source),
    };
    spawn_unbind(plan, row.action.0.clone(), user_dir, cx)
        // Describe a dispatched write, not a confirmed result. A stale on-disk key or
        // write failure is logged; the watcher reload supplies the eventual row.
        .or(Some(notice))
}

/// Remove the selected action's complete user override set in one write. This includes
/// both replacement keys and `"none"` shadows, even if no binding is currently
/// displayed. With no overrides, report a notice and write nothing.
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
        // The notice describes dispatch; only an accepted reload changes the row.
        .or_else(|| Some(format!("removing {what} on {}", row.title)))
}

/// Drop the user bindings array, exposing desk and builtin bindings. The caller gates
/// confirmation on the parsed `user_bindings` count; zero yields a notice. Other keymap
/// document fields are retained.
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
    crate::config_write::submit(&user_dir.clone(), cx.background_executor(), move || {
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

/// Submit one action's reset through the ordered configuration writer. Keep shell state
/// unchanged until reload; log failures or a removal count smaller than the requested
/// override set, which indicates the loaded keymap differs from disk.
fn spawn_reset(
    overrides: Vec<UserOverride>,
    action: String,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    let Some(user_dir) = user_dir.clone() else {
        return Some(no_user_dir_notice(&action));
    };
    crate::config_write::submit(&user_dir.clone(), cx.background_executor(), move || {
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

/// Report that no user configuration directory is available for the write.
fn no_user_dir_notice(what: &str) -> String {
    tracing::warn!(target: "geode::config",
        "no writable user config dir; the binding change for {what} was not saved"
    );
    "no writable user config directory — nothing was saved".to_string()
}

/// Submit unbind through the ordered configuration writer, leaving live bindings
/// unchanged until reload. Log errors and user-key removals that find no matching
/// entry; a stale key or context can leave the binding reachable.
///
/// Return a notice only if the user configuration directory is unavailable. The
/// detached writer reports its result through logs.
fn spawn_unbind(
    plan: EditPlan<Unbind>,
    action: String,
    user_dir: &Option<PathBuf>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    let Some(user_dir) = user_dir.clone() else {
        return Some(no_user_dir_notice(&action));
    };
    crate::config_write::submit(&user_dir.clone(), cx.background_executor(), move || {
        let EditPlan { clear, write } = plan;
        let Some(unbind) = write else {
            if let Err(e) = apply_reset(&user_dir, &clear) {
                tracing::warn!(target: "geode::config",
                    "failed to change the binding for {action}: {e}"
                );
            }
            return;
        };
        match apply_unbind_clearing(&user_dir, &clear, &unbind) {
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

/// Highlight fuzzy-match character offsets using [`palette::highlight_runs`] to merge
/// them into byte ranges. Subsequence matches can produce disjoint runs.
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

/// Handle an action-button click through the same [`arm_verb`] decision as keys. Refuse
/// while capture or confirmation owns input; otherwise clear the previous notice, arm a
/// question or report a refusal, and synchronize the dialog.
fn press_verb(shell: &mut ShellView, key: char, window: &mut Window, cx: &mut Context<ShellView>) {
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
    let row = state.rows.at(state.selected).cloned();
    let user_bindings = state.user_bindings;
    arm_verb(state, key, row.as_ref(), user_bindings, &user_dir, cx);
    shell.refresh_dialog_rows(cx);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Action buttons for available unbind/reset targets and all-user reset. Each uses
/// `keybindings-action-{key}`; an armed confirmation replaces the bar.
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
    // (chip spelling, the verb `arm_verb` reads, label)
    let mut verbs: Vec<(&'static str, char, &'static str)> = Vec::new();
    if state.listening.is_none() {
        if can_unbind(row) {
            verbs.push(("d", 'd', "Unbind"));
        }
        if can_reset(row) {
            verbs.push(("r", 'r', "Reset to lower layer"));
        }
        if state.user_bindings > 0 {
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
                    .child(kbd::chip(&ks))
                    .child(label),
            )
            .on_click(move |_event, window, cx| {
                entity_for_action.update(cx, |shell, cx| press_verb(shell, verb, window, cx));
            });
        bar = bar.child(div().debug_selector(move || selector.clone()).child(button));
    }
    bar.into_any_element()
}

/// Build the filtered binding list, capture chips, actions, and mode-specific hints.
/// Read render state from the borrowed shell; capture `entity` only for pointer
/// callbacks that run after rendering.
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.keybindings.as_ref() else {
        return div().into_any_element();
    };
    #[cfg(debug_assertions)]
    shell.assert_rows_current(cx);
    let theme = cx.theme();
    // Copied out so the render closures below don't have to hold the
    // `theme` borrow.
    let row_paint = super::listrow::row_paint(theme);

    // The list renders ONLY the rows that survive the filter. Safe
    // because row click handlers are keyed by `ActionId`, not position
    // (see [`Prepared::position`]); the `debug_selector` index below stays
    // the row's index in the FULL list, so a row keeps its identity
    // across filtering.

    let mut list = v_flex()
        .id("keybindings-list")
        .w(scale::design(WIDTH))
        .h(scale::design(
            (state.rows.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT),
        ))
        .overflow_y_scroll()
        .track_scroll(&shell.keybindings_scroll)
        .debug_selector(|| "keybindings-list".to_string());

    for (position, shown) in state.rows.shown().iter().enumerate() {
        let row_ix = shown.row;
        let row = &state.rows.rows()[row_ix];
        let text = &state.rows.texts()[row_ix];
        let is_selected = position == state.selected;
        let is_listening = is_selected && state.listening.is_some();

        let row_el = h_flex()
            .id(("keybindings-row", row_ix))
            .w_full()
            .justify_between()
            .items_center()
            .gap_3()
            .px_2()
            .py_1()
            .rounded(theme.radius);
        let row_el = super::listrow::paint_row(row_el, row_paint, is_selected);

        let label = v_flex()
            .gap_0p5()
            .child(palette::highlighted_runs(
                &text.primary,
                &shown.primary,
                row_paint.accent,
            ))
            .child(div().text_xs().text_color(theme.muted_foreground).child(
                palette::highlighted_runs(&text.secondary, &shown.secondary, row_paint.accent),
            ));

        let binding_el: AnyElement = if is_listening {
            let pending = state.listening.as_ref().expect("is_listening implies Some");
            if pending.is_empty() {
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("listening…")
                    .into_any_element()
            } else {
                kbd::binding(pending).into_any_element()
            }
        } else {
            match row.current.as_ref() {
                Some(bound) => kbd::binding(&bound.keystrokes).into_any_element(),
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

    if state.rows.is_empty() {
        // An empty result displays a message and supplies no capture target. Filter
        // Enter still accepts the query and returns to Normal.
        list = list.child(
            div()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("no matches"),
        );
    }

    // Keystroke chips for the footer hints — the same [`kbd::chip`]
    // the rows use, so key names in helper text look like the keys they
    // mean. A chip renders exactly one keystroke.
    let chip = move |spec: &str| {
        let ks = crate::keymap::parse_keystroke(spec, Modifiers::NONE)
            .expect("footer hint keystrokes are hardcoded valid");
        kbd::chip(&ks).into_any_element()
    };
    let sep = |text: &'static str| div().child(text).into_any_element();

    // Show only the active mode's key vocabulary, tagged with each hint's category.
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
                // Teach keyboard access to the same verbs exposed by the action
                // buttons.
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
                // Both keys return to Normal without capture: keep the query or restore
                // its entry text. A subsequent Normal Enter starts rebinding.
                Hint::new(HintRow::Go, &["enter"], "keep the filter"),
                Hint::new(HintRow::Go, &["escape"], "discard the filter"),
            ],
        }
    };
    let hint_line: AnyElement = super::dialog::hint_rows(&hints);
    let footer = v_flex()
        .w(scale::design(WIDTH))
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(theme.border)
        // Paint refusal and dispatched-write notices above the hints. The selector lets
        // tests check the visible notice as well as retained state.
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

    // Render the live Input only in Filter with no capture. Otherwise echo frozen query
    // text. Hide the slash hint during capture because `/` records a key.
    let frozen_query = (state.listening.is_some() || state.mode == DialogMode::Normal).then_some(
        dialog::FrozenFilter {
            query: state.query.as_str(),
            slash_filters: state.listening.is_none(),
            entity: entity.clone(),
        },
    );

    let row = state.rows.at(state.selected);

    v_flex()
        .gap_2()
        // The mode pill is supplied by the modal title-extra builder.
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

    // A same-key `"none"` entry hides the displayed binding for both bare and contexted
    // keys. Looking up only the action ID would miss that shadow.

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

    /// Module fragment bindings enter through the roster and splice as Builtin, with
    /// their predicates intact. The dialog can shadow them using ordinary layer
    /// provenance without a module-specific branch.
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

    /// Any row click starts a fresh capture, discarding a partial sequence even when
    /// clicking the same row again.
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
            base: None,
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
            base: None,
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
            base: None,
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
        // Only the invisible action ID contains "three" in this fixture. It must not
        // produce a visible row match.
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
        // Resolve clicked ActionId against the filtered list, not the full row order.
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
        // Query replacement cancels capture; a row click instead starts a fresh one.
        let mut state = KeybindingsState::new();
        state.listening = Some(vec![key("a")]);
        state.set_query("foc".to_string());
        assert!(state.listening.is_none());
    }

    /// The dialog lists each shared motion once, in one "Motion" run, showing
    /// the vim key under the one shipped context a rebind writes into.
    #[test]
    fn each_motion_is_listed_once_under_motion_with_its_vim_key() {
        let mut reg = ActionRegistry::default();
        crate::defaults::register_builtin_actions(&mut reg);
        let doc = LayerDoc::builtin("keymap", crate::defaults::BUILTIN_KEYMAP).unwrap();
        let (keymap, diags) = build_keymap(&[doc], crate::defaults::default_mod(), &reg);
        assert!(diags.is_empty(), "{diags:?}");
        let rows = derive_rows(&reg, &keymap);
        let motion: Vec<&KeybindingRow> = rows
            .iter()
            .filter(|r| r.action.0.starts_with("motion::"))
            .collect();
        assert_eq!(motion.len(), crate::defaults::MOTION_ACTIONS.len());
        assert!(motion.iter().all(|r| r.category == "Motion"), "{motion:?}");
        let first = rows.iter().position(|r| r.category == "Motion").unwrap();
        assert!(
            rows[first..first + motion.len()]
                .iter()
                .all(|r| r.category == "Motion"),
            "the Motion rows form one run"
        );
        let shown = |id: &str| {
            let row = motion.iter().find(|r| r.action.0 == id).unwrap();
            let b = row.current.as_ref().expect("bound");
            (b.key_source.clone(), b.context_source.clone().unwrap())
        };
        let grid = crate::defaults::GRID_MOTION_CONTEXT.to_string();
        assert_eq!(shown("motion::down"), ("j".to_string(), grid.clone()));
        assert_eq!(shown("motion::left"), ("h".to_string(), grid.clone()));
        assert_eq!(
            shown("motion::page_down"),
            ("ctrl+f".to_string(), grid.clone())
        );
        assert_eq!(shown("motion::line_start"), ("^".to_string(), grid));
        assert_eq!(
            shown("motion::menu_down"),
            ("j".to_string(), "tilelist".to_string())
        );
    }
}
