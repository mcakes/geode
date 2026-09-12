//! The settings modal (`ctrl+,`, `settings::open`): a keyboard-driven flat
//! row list in the exact mold of the keybinding dialog
//! ([`super::keybindings_view`]) — the settings-dialog rewrite that
//! REPLACED the gpui-component `Settings`/`SettingPage`/`SettingGroup`/
//! `SettingItem`/`SettingField` composite this module used to wrap.
//!
//! ## Why the composite went away
//!
//! The old dialog was the one surface in Geode that didn't speak the
//! shell's own language: mouse-first controls (a dropdown popup, a switch,
//! button groups), its own search input with its own focus, none of the
//! vim vocabulary every other list surface here HAD AT THE TIME
//! (`crate::vimnav` motions, `/` find via `crate::vimfind` — both since
//! retired from every dialog by the filter-first rewrite, see
//! `keybindings_view`'s module doc), and a stack of documented
//! layout workarounds just to keep the composite from collapsing inside
//! our own modal chrome (see `dialog::render_modal`'s height-contract doc
//! comment for the scar tissue that remains). The keybinding dialog
//! established the house pattern — pure unit-testable state + a
//! [`dialog::ModalKeyHandler`] with first refusal on every keystroke + a
//! `build` closure painting a flat scrollable row list — and this module
//! now applies that pattern to settings wholesale.
//!
//! ## The row model
//!
//! Five rows, derived FRESH from `ShellView` state on every render and
//! every keystroke ([`derive_rows`] via [`rows_for`] — same no-caching
//! contract as `keybindings_view::derive_rows`): Theme, Font size and
//! Line numbers under **Appearance**, Find style under **Keyboard**, Add
//! tile under **Tiling**. (A Dark mode row used to sit beside Theme; it
//! was retired 2026-09-12 because every bundled theme name already
//! carries its mode — `Molokai Dark`, `Gruvbox Light` — so the row only
//! ever restated the Theme row's own value. `mod+shift+t` and `[theme]
//! mode` are untouched.) Each row
//! is one enumerated setting — an ordered list of value labels plus the
//! index of the currently-active one — and editing is *stepping*:
//! `tab`/`shift+tab` step the selected row's value forward/back (wrapping
//! at both ends, [`step`]), and a click on the already-selected row
//! cycles forward too (the mirror of keybindings' click-to-listen). A
//! step applies IMMEDIATELY through the same apply-then-persist seams the
//! old dialog's controls used
//! ([`set_theme`]/[`set_font_size`]/[`set_find_style`]'s
//! shared `*_on` cores) — theme stepping is a live preview, and
//! persistence stays on the existing background paths
//! (`ShellView::persist_theme` and friends; no I/O lands on the render
//! thread here). `enter` is deliberately inert and reserved (a step
//! already applies the instant it happens, so there is nothing for enter
//! to confirm) — see the "Filter" section below for why `tab` rather than
//! the retired `h`/`l`/arrow keys is what reaches this dialog at all.
//!
//! The old dialog's read-only content — the "Mod key: …" line and the
//! "saved to your app.toml" caption — survives as muted inert footer
//! lines, not rows: there is nothing to step on either.
//!
//! ## What deliberately did NOT change
//!
//! The four setter helpers ([`set_theme`], [`set_font_size`],
//! [`set_find_style`], [`set_add_direction`]) keep their exact
//! `Entity<ShellView>`-taking signatures and semantics — they are the
//! seam `shell::mod`'s tests drive directly, and nothing about *applying*
//! a setting changed, only the control surface in front of it. Each now
//! delegates to a `*_on(&mut ShellView, ...)` core so this module's own
//! [`handle_key`] (which already holds `&mut ShellView` mid-key-dispatch
//! and must not reenter the entity via `Entity::update`) can apply the
//! identical path.
//!
//! ## Filter
//!
//! `docs/superpowers/specs/2026-09-01-dialog-filter-input-design.md`
//! replaced this dialog's vim motions and `/` find (both styles) with a
//! focused fuzzy filter: opening the dialog focuses
//! `ShellView::dialog_input`, so the first character typed narrows the
//! list ([`visible_rows`] over [`searchable_text`]) rather than falling
//! on the floor, and what this dialog itself still claims is
//! the short vocabulary a focused `Input` leaves free
//! ([`crate::listfilter::nav_command`]: `up`/`down`/`ctrl+p`/`ctrl+n` ∓1,
//! `ctrl+d`/`ctrl+u` ±5, `ctrl+f`/`ctrl+b`/`pageup`/`pagedown` ±10), plus
//! `tab`/`shift+tab` for stepping — reaching this dialog at all needs
//! more than the `Input`'s own gating (see [`handle_key`]'s doc comment
//! and `dialog::init_reclaimed_keybindings`). The `[ui] find_style`
//! setting and its
//! row are untouched by this — `FindStyle` still exists, is still
//! rendered and still steppable like any other row (spec §8) — this
//! dialog (and the keybinding dialog) simply stop *reading* it to steer
//! their own navigation.
//!
//! **This dialog is still filter-first; the keybinding dialog no longer
//! is.** The dialog interaction model
//! (`docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md`,
//! `crate::dialogmode`) gave that dialog a normal mode where bare letters
//! are verbs and `/` enters filter mode, so it now opens with
//! `dialog_input` BLURRED (`open_shell_dialog_with_key`'s `focus_filter:
//! false`). Settings kept the filter-first shape — it has no verbs to
//! collide with — so it still passes `focus_filter: true` and its filter
//! still holds focus for the life of the dialog. Two dialogs, one shared
//! `dialog_input`, two different opening states: do not read either
//! one's behaviour off the other.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, MouseButton, Window, div, px};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::fontsize::FontSize;
use crate::keymap::Keystroke;
use crate::keymap::Modifiers;
use crate::linenumbers::LineNumbers;
use crate::listfilter::{self, Ranked};
use crate::shell::ShellView;
use crate::shell::dialog;
use crate::tileadd::AddDirection;
use crate::vimfind::FindStyle;
use crate::vimnav;

use super::keybindings_view::{highlighted_text, key_chip, split_label_indices};

// ---------------------------------------------------------------------
// Pure core — no gpui. Row derivation, value stepping, session state.
// ---------------------------------------------------------------------

/// Which setting a row edits — the row's stable identity (what click
/// handlers are keyed by, mirroring how keybindings rows are keyed by
/// `ActionId` rather than list position) and what [`apply_setting`]
/// dispatches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingId {
    Theme,
    FontSize,
    FindStyle,
    LineNumbers,
    AddDirection,
}

/// One row of the settings dialog: an enumerated setting — its displayed
/// title/category, every value it can take (as display labels, in
/// stepping order), and the index of the currently-active value. Derived
/// fresh per render/keystroke by [`derive_rows`]; never cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    pub id: SettingId,
    pub title: &'static str,
    pub category: &'static str,
    pub values: Vec<String>,
    pub current: usize,
}

/// Build the dialog's four rows from plain inputs (no gpui, no
/// `ShellView` — [`rows_for`] is the thin shell-reading wrapper), in the
/// dialog's fixed display order: the Appearance rows first (Theme, Font
/// size), then Keyboard (Find style), then Line numbers and Add tile in
/// the order they were added. A two-value setting (Find style) needs no
/// special case — wrapping a two-element list IS a toggle.
///
/// An `active_theme` not present in `theme_names` (impossible via the UI —
/// `ThemeService::apply` only ever activates a bundled name — but cheap to
/// be deterministic about) marks the first theme as current rather than
/// panicking or carrying an out-of-range index into [`step`].
pub fn derive_rows(
    theme_names: &[String],
    active_theme: &str,
    font_size: FontSize,
    find_style: FindStyle,
    line_numbers: LineNumbers,
    add_direction: AddDirection,
) -> Vec<SettingRow> {
    vec![
        SettingRow {
            id: SettingId::Theme,
            title: "Theme",
            category: "Appearance",
            current: theme_names
                .iter()
                .position(|n| n == active_theme)
                .unwrap_or(0),
            values: theme_names.to_vec(),
        },
        SettingRow {
            id: SettingId::FontSize,
            title: "Font size",
            category: "Appearance",
            values: FontSize::ALL
                .iter()
                .map(|s| s.label().to_string())
                .collect(),
            current: FontSize::ALL
                .iter()
                .position(|&s| s == font_size)
                .expect("font_size is always one of FontSize::ALL"),
        },
        SettingRow {
            id: SettingId::FindStyle,
            title: "Find style",
            category: "Keyboard",
            values: FindStyle::ALL
                .iter()
                .map(|s| s.label().to_string())
                .collect(),
            current: FindStyle::ALL
                .iter()
                .position(|&s| s == find_style)
                .expect("find_style is always one of FindStyle::ALL"),
        },
        SettingRow {
            id: SettingId::LineNumbers,
            title: "Line numbers",
            category: "Appearance",
            values: LineNumbers::ALL
                .iter()
                .map(|m| m.label().to_string())
                .collect(),
            current: LineNumbers::ALL
                .iter()
                .position(|&m| m == line_numbers)
                .expect("line_numbers is always one of LineNumbers::ALL"),
        },
        SettingRow {
            id: SettingId::AddDirection,
            title: "Add tile",
            category: "Tiling",
            values: AddDirection::ALL
                .iter()
                .map(|d| d.label().to_string())
                .collect(),
            current: AddDirection::ALL
                .iter()
                .position(|&d| d == add_direction)
                .expect("add_direction is always one of AddDirection::ALL"),
        },
    ]
}

/// Which way a value step goes — `tab` (forward, `Right`) vs. `shift+tab`
/// (back, `Left`), and a click on the already-selected row (forward,
/// mirroring `tab`). The old dialog's `h`/`left`/`l`/`right` motions and
/// its `enter`/`space` cycle-forward keys are retired along with the vim
/// vocabulary this dialog no longer speaks (see the module doc); `enter`
/// is deliberately inert now rather than aliased onto `Right` — see
/// [`handle_key`]'s own doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepDirection {
    Left,
    Right,
}

/// The value index one step from `current` in a `len`-value list,
/// WRAPPING at both ends — deliberately unlike `vimnav::apply`'s clamped
/// list navigation: a settings value is a cycle (fzf → vim → fzf), not a
/// list with ends, and wrap is what lets a run of `tab` presses (or
/// repeated clicks on the same row) reach every value without ever
/// hitting a dead end. `len == 0` yields 0 (unreachable for real rows —
/// every setting has at least two values — but deterministic).
pub fn step(len: usize, current: usize, dir: StepDirection) -> usize {
    if len == 0 {
        return 0;
    }
    match dir {
        StepDirection::Right => (current + 1) % len,
        StepDirection::Left => (current + len - 1) % len,
    }
}

/// Persistent state for one open settings dialog session — the exact
/// shape of `KeybindingsState` minus the rebind-capture field (there is
/// nothing to "listen" for here; editing is stepping, which is
/// instantaneous). Fresh on every open ([`open`]), holds no gpui types
/// (the scroll handle lives on `ShellView::settings_scroll`, the same
/// split every other dialog uses), so every transition is unit-testable
/// without a window.
#[derive(Debug, Default)]
pub struct SettingsState {
    /// Index into the **filtered** row list ([`visible_rows`]), not the
    /// full one — the same convention `KeybindingsState::selected` uses.
    pub selected: usize,
    /// The filter query, mirrored from `ShellView::dialog_input`.
    pub query: String,
}

impl SettingsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the query and reset the selection to the top match — the
    /// pure half of the `InputEvent::Change` subscription in
    /// `ShellView::new`.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.selected = 0;
    }
}

/// The text one row exposes to the filter: title and category — exactly
/// what the row displays as its identity, and nothing more. The value
/// labels deliberately do NOT participate (same philosophy as
/// keybindings' `searchable_text` excluding the invisible action id, one
/// step further: values ARE visible, but `gruvbox` matching the Theme
/// row only when Gruvbox happens to be active — and `large` matching Font
/// size only sometimes — would make matching depend on current state
/// rather than on what the row *is*).
pub fn searchable_text(row: &SettingRow) -> String {
    format!("{} {}", row.title, row.category)
}

/// The rows this dialog currently shows, ranked — the settings twin of
/// `keybindings_view::visible_rows`, over [`searchable_text`] (title and
/// category; the value labels deliberately do not participate — see that
/// function's doc comment).
pub fn visible_rows(state: &SettingsState, rows: &[SettingRow]) -> Vec<Ranked> {
    let texts: Vec<String> = rows.iter().map(searchable_text).collect();
    listfilter::rank(&texts, &state.query)
}

/// Where the row for `clicked` currently sits in the *filtered* list, or
/// `None` if the filter is hiding it — the settings twin of
/// `keybindings_view::filtered_position`, keyed by [`SettingId`] rather
/// than `ActionId` for the same identity-not-position reason.
pub fn filtered_position(
    visible: &[Ranked],
    rows: &[SettingRow],
    clicked: SettingId,
) -> Option<usize> {
    visible
        .iter()
        .position(|m| rows.get(m.row).is_some_and(|r| r.id == clicked))
}

/// What a click on filtered position `clicked_ix` does to already-open
/// dialog state: clicking any other row selects it (the click wins, same
/// as keybindings' `click_selects_or_listens`); clicking the
/// already-selected row means "cycle this row's value forward" — returns
/// `true` so the gpui caller ([`on_row_clicked`]) applies the step,
/// keeping this function pure.
pub fn click_selects_or_steps(state: &mut SettingsState, clicked_ix: usize) -> bool {
    if state.selected == clicked_ix {
        true
    } else {
        state.selected = clicked_ix;
        false
    }
}

// ---------------------------------------------------------------------
// Applying a value — the same seams the old dialog's controls drove.
// ---------------------------------------------------------------------

/// Apply value `value_ix` of the setting `id` names to the live shell —
/// the one place [`handle_key`]'s stepping and [`on_row_clicked`]'s
/// cycle-forward both land. Dispatches to the `*_on` core of the matching
/// setter helper, so a keyboard step is byte-for-byte the same apply +
/// persist path a direct [`set_theme`]/[`set_font_size`]/... call takes.
/// An out-of-range `value_ix` (unreachable — [`step`] wraps within the
/// row's own `values`) is a no-op rather than a panic.
fn apply_setting(
    shell: &mut ShellView,
    id: SettingId,
    value_ix: usize,
    cx: &mut Context<ShellView>,
) {
    match id {
        SettingId::Theme => {
            let names = shell.services.theme.names();
            if let Some(name) = names.get(value_ix) {
                set_theme_on(shell, &name.clone(), cx);
            }
        }
        SettingId::FontSize => {
            if let Some(&size) = FontSize::ALL.get(value_ix) {
                set_font_size_on(shell, size, cx);
            }
        }
        SettingId::FindStyle => {
            if let Some(&style) = FindStyle::ALL.get(value_ix) {
                set_find_style_on(shell, style, cx);
            }
        }
        SettingId::LineNumbers => {
            if let Some(&m) = LineNumbers::ALL.get(value_ix) {
                shell.set_line_numbers(m, cx);
            }
        }
        SettingId::AddDirection => {
            if let Some(&d) = AddDirection::ALL.get(value_ix) {
                set_add_direction_on(shell, d, cx);
            }
        }
    }
}

/// Apply `name` at the theme's currently active mode via
/// `ThemeService::apply`, then persist it (`ShellView::persist_theme`) —
/// the core both [`set_theme`] (the `Entity`-taking seam `shell::mod`'s
/// tests drive) and [`apply_setting`] (this dialog's own stepping, which
/// already holds `&mut ShellView` mid-key-dispatch and must not reenter
/// the entity) share.
fn set_theme_on(shell: &mut ShellView, name: &str, cx: &mut Context<ShellView>) {
    let mode = shell.services.theme.active_mode();
    shell.services.theme.apply(name, mode, cx);
    shell.persist_theme(cx);
    cx.notify();
}

/// [`set_theme_on`]'s sibling for font size. The `notify` triggers a
/// re-render, and `ShellView::render` applies the new rem size there
/// (this core has no `Window` to apply it here — see the `fontsize`
/// module doc).
fn set_font_size_on(shell: &mut ShellView, size: FontSize, cx: &mut Context<ShellView>) {
    shell.font_size = size;
    shell.persist_font_size(cx);
    cx.notify();
}

/// [`set_theme_on`]'s sibling for find style. Nothing to apply beyond the
/// state itself, and honestly nothing downstream either: neither dialog
/// reads `ShellView::find_style` for behaviour any more (the filter-first
/// rewrite retired both `/` sessions it used to choose between — spec
/// `2026-09-01-dialog-filter-input-design.md` §8). Stepping this row only
/// re-labels its own value and persists the setting; it steers nothing
/// until Phase 3's blotter reads it (§9).
fn set_find_style_on(shell: &mut ShellView, style: FindStyle, cx: &mut Context<ShellView>) {
    shell.find_style = style;
    shell.persist_find_style(cx);
    cx.notify();
}

/// [`set_theme_on`]'s sibling for the add direction (spec 2026-09-08
/// add-tile §5). Read by `ShellView::add_tile` on the next add.
fn set_add_direction_on(shell: &mut ShellView, d: AddDirection, cx: &mut Context<ShellView>) {
    shell.add_direction = d;
    shell.persist_add_direction(cx);
    cx.notify();
}

/// Apply a theme by name — the `Entity<ShellView>`-taking seam kept from
/// the pre-rewrite dialog (its doc'd purpose — a directly drivable
/// handler for tests, since the old dropdown's popup overlay couldn't be
/// clicked from a `#[gpui::test]` — still holds, and `shell::mod`'s tests
/// still call it). Now a thin `Entity::update` wrapper over
/// [`set_theme_on`], the same core this dialog's `tab`/`shift+tab`
/// stepping applies through. Only test code calls these four wrappers
/// since the rewrite (the dialog itself already holds `&mut ShellView`
/// mid-key-dispatch and must use the cores directly), hence the not-test
/// `dead_code` allowance rather than `#[cfg(test)]`: they stay compiled,
/// documented, and reachable for any future non-modal caller.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_theme(view: &Entity<ShellView>, name: &str, cx: &mut App) {
    view.update(cx, |shell, cx| set_theme_on(shell, name, cx));
}

/// Set the UI font size and persist it (`[ui] font_size`) — same survival
/// story as [`set_theme`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_font_size(view: &Entity<ShellView>, size: FontSize, cx: &mut App) {
    view.update(cx, |shell, cx| set_font_size_on(shell, size, cx));
}

/// Set the find style and persist it (`[ui] find_style`) — same survival
/// story as [`set_theme`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_find_style(view: &Entity<ShellView>, style: FindStyle, cx: &mut App) {
    view.update(cx, |shell, cx| set_find_style_on(shell, style, cx));
}

/// Set the add direction and persist it (`[tiles] add`) — same survival
/// story as [`set_theme`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_add_direction(view: &Entity<ShellView>, d: AddDirection, cx: &mut App) {
    view.update(cx, |shell, cx| set_add_direction_on(shell, d, cx));
}

/// The `[keymap] mod` value that produces `mods` (see
/// `defaults::mod_alias_from_config`) — falls back to `"alt"`, matching
/// `defaults::default_mod`, for anything that isn't exactly one of the
/// three named aliases.
fn mod_alias_label(mods: Modifiers) -> &'static str {
    if mods == Modifiers::CTRL {
        "ctrl"
    } else if mods == Modifiers::CMD {
        "cmd"
    } else {
        "alt"
    }
}

// ---------------------------------------------------------------------
// gpui wiring — everything above this line is the pure core (plus the
// apply seams, which touch ShellView but no rendering).
// ---------------------------------------------------------------------

/// Row height estimate for sizing the viewport (two lines: title plus
/// muted category) — same non-load-bearing caveat as
/// `keybindings_view::ROW_HEIGHT`.
const ROW_HEIGHT: f32 = 44.0;
/// Rows visible before the list scrolls. Five rows today, so nothing
/// scrolls — kept anyway so the list's sizing arithmetic stays identical
/// to keybindings' and a sixth setting never needs layout thought.
const VISIBLE_ROWS: usize = 10;
/// Target dialog content width in pixels — same as the keybinding
/// dialog's, so the two sibling dialogs read as one family.
const WIDTH: f32 = 640.0;

/// [`derive_rows`] over the live shell state — the one place the pure row
/// model meets `ShellView`. Called fresh on every render ([`build`]) and
/// every keystroke ([`handle_key`]); rows are never cached, so a step's
/// effect (or a config hot reload's) is visible on the very next derive.
fn rows_for(shell: &ShellView) -> Vec<SettingRow> {
    derive_rows(
        &shell.services.theme.names(),
        shell.services.theme.active_name(),
        shell.font_size,
        shell.find_style,
        shell.line_numbers,
        shell.add_direction,
    )
}

/// Open the settings modal (`settings::open`: `ctrl+,`, the palette
/// entry, and the sidebar profile icon all reach this). A no-op if a
/// modal is already open — re-triggering the action must not clobber
/// whatever's up. Fresh [`SettingsState`] every open, nothing survives a
/// close/reopen — the same contract as the palette and the keybinding
/// dialog. Goes through [`dialog::open_shell_dialog_with_key`] (the one
/// standard door, with the Part B key seam): this dialog needs first
/// refusal on every keystroke for the filter-nav vocabulary
/// ([`listfilter::nav_command`]) and `tab`/`shift+tab` value stepping.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    view.settings = Some(SettingsState::new());
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        "Settings",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
}

/// The [`dialog::ModalKeyHandler`] for this dialog, mirroring
/// `keybindings_view::handle_key` with value stepping in place of rebind
/// capture:
///
/// 1. [`listfilter::nav_command`] motions move the selection within the
///    *filtered* list;
/// 2. `tab` / `shift+tab` step the selected row's value forward / back,
///    wrapping, applied immediately through [`apply_setting`]. `left`/
///    `right`, the old stepping keys, are swallowed unconditionally by a
///    focused single-line `Input` and can never reach this handler again.
///    `tab` itself clears the `Input`'s own gating for the same reason
///    (single-line, spec §2b) but is NOT enough on its own — gpui-
///    component's `Root` also binds it unconditionally to its own
///    focus-cycling, above `ShellView` in the dispatch tree, and that
///    would swallow it too were it not for `dialog::
///    init_reclaimed_keybindings`'s scoped `NoAction` reclaim (spec
///    §2b2), which the modal panel's `"GeodeModal"` key context makes
///    possible;
/// 3. bare `enter` is claimed and dropped — returns `true` without acting.
///    It is deliberately inert and reserved: settings apply the instant
///    they are stepped, so there is nothing to confirm. Claiming it (not
///    just ignoring it) is required, not cosmetic: with a focused `Input`,
///    an unclaimed key continues to the window's text-input phase (spec
///    §3) rather than simply vanishing, and `enter` reaching the input
///    fires an `InputEvent::Change` that would reset `selected` back to
///    the top match even though the text itself is unchanged;
/// 4. everything else, bare `escape` included, returns `false`.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    _window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let rows = rows_for(shell);
    let Some(state) = shell.settings.as_mut() else {
        return false;
    };
    let visible = visible_rows(state, &rows);

    if let Some(cmd) = listfilter::nav_command(ks) {
        state.selected = vimnav::apply(state.selected, visible.len(), cmd);
        let selected = state.selected;
        shell.settings_scroll.scroll_to_item(selected);
        cx.notify();
        return true;
    }

    // Bare `enter` is reserved and deliberately inert (see this
    // function's own doc comment, item 3) — claimed and dropped rather
    // than left unhandled, because leaving it unhandled would NOT make it
    // inert: an unclaimed key continues past this handler to the filter's
    // own text-input phase, where `enter` fires an `InputEvent::Change`
    // that resets `selected` back to the top match.
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        return true;
    }

    let dir = match (ks.mods, ks.key.as_str()) {
        (Modifiers::NONE, "tab") => StepDirection::Right,
        (m, "tab")
            if m == (Modifiers {
                shift: true,
                ..Modifiers::NONE
            }) =>
        {
            StepDirection::Left
        }
        _ => return false,
    };
    let selected = state.selected;
    let Some(row) = visible.get(selected).and_then(|m| rows.get(m.row)) else {
        return false;
    };
    let (id, new_ix) = (row.id, step(row.values.len(), row.current, dir));
    apply_setting(shell, id, new_ix, cx);
    cx.notify();
    true
}

/// Selection/step logic for a real mouse click on the row for `clicked`
/// (`SettingId`, resolved back to a position in the *filtered* list
/// against freshly derived rows — same identity-not-position keying as
/// keybindings' `on_row_clicked`). The gpui-facing wrapper around the
/// pure [`click_selects_or_steps`]: a `true` (already-selected row)
/// cycles that row's value forward.
fn on_row_clicked(shell: &mut ShellView, clicked: SettingId, cx: &mut Context<ShellView>) {
    let rows = rows_for(shell);
    let Some(state) = shell.settings.as_mut() else {
        return;
    };
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {
        return;
    };
    let cycle = click_selects_or_steps(state, ix);
    let selected = state.selected;
    shell.settings_scroll.scroll_to_item(selected);
    if cycle && let Some(row) = visible.get(ix).and_then(|m| rows.get(m.row)) {
        let new_ix = step(row.values.len(), row.current, StepDirection::Right);
        apply_setting(shell, row.id, new_ix, cx);
    }
    cx.notify();
}

/// The [`dialog::ShellModal::build`] closure body: the shared filter row
/// (`dialog::filter_row`, never frozen — this dialog has no listening
/// state to freeze it for) over a scrollable row list (title + muted
/// category on the left, with fuzzy-match highlighting; the current value
/// label on the right in the mono data face) plus a footer hint. `entity`
/// is the `Entity<ShellView>` every row's click handler captures to reach
/// [`on_row_clicked`] at click time; `shell` is this call's own
/// plain-borrow read (see `ShellModal::build`'s doc comment for why both).
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.settings.as_ref() else {
        return div().into_any_element();
    };
    let rows = rows_for(shell);
    let theme = cx.theme();
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;

    // The list renders ONLY the rows that survive the filter. Safe
    // because row click handlers are keyed by `SettingId`, not position
    // (see [`filtered_position`]).
    let visible = visible_rows(state, &rows);

    let mut list = v_flex()
        .id("settings-list")
        .w(px(WIDTH))
        .h(px(
            (visible.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT)
        ))
        .overflow_y_scroll()
        .track_scroll(&shell.settings_scroll)
        .debug_selector(|| "settings-list".to_string());

    for (position, m) in visible.iter().enumerate() {
        let row_ix = m.row;
        let row = &rows[row_ix];
        let is_selected = position == state.selected;

        // `split_label_indices` (shared with `keybindings_view::build` —
        // see its own doc comment for why this is one function, not two
        // copies) splits the ranked char offsets back across the title
        // and category lines they're painted on.
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
            .child(highlighted_text(row.title, &title_ix, theme.primary))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(highlighted_text(row.category, &cat_ix, theme.primary)),
            );

        // The current value, in the data face — a value readout, not
        // prose, same register as the binding chips across the hall.
        let value_el = div()
            .font_family(crate::fonts::MONO)
            .text_sm()
            .flex_shrink_0()
            .child(row.values[row.current].clone());

        let entity_for_row = entity.clone();
        let id = row.id;
        let row_el = row_el
            .child(label)
            .child(value_el)
            .debug_selector(move || format!("settings-row-{row_ix}"))
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                entity_for_row.update(cx, |shell, cx| {
                    on_row_clicked(shell, id, cx);
                });
            });

        list = list.child(row_el);
    }

    if visible.is_empty() {
        // Zero matches: one muted line where the rows would be — the same
        // muted treatment keybindings' empty filter gets, so an
        // over-narrow filter reads as a state, not a rendering glitch.
        list = list.child(
            div()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("no matches"),
        );
    }

    // Footer hint chips — `keybindings_view::key_chip`, reused so key
    // names in helper text look identical across the two sibling dialogs.
    let chip = move |spec: &str| {
        let ks = crate::keymap::parse_keystroke(spec, Modifiers::NONE)
            .expect("footer hint keystrokes are hardcoded valid");
        key_chip(&ks, chip_fg, chip_bg)
    };
    let sep = |text: &'static str| div().child(text).into_any_element();

    let hint_line: AnyElement = v_flex()
        .gap_0p5()
        .child(h_flex().gap_1().items_center().flex_wrap().children(vec![
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
        ]))
        .child(h_flex().gap_1().items_center().flex_wrap().children(vec![
            chip("tab"),
            sep("next value ·"),
            chip("shift+tab"),
            sep("previous value ·"),
            chip("escape"),
            sep("close"),
        ]))
        .into_any_element();

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
        // The old dialog's read-only content, now two muted inert lines
        // (module doc): the mod key is set in config, not here; and every
        // row above saves through the same file.
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!(
                    "Mod key: {} — set via [keymap] mod in config",
                    mod_alias_label(shell.services.mod_alias)
                )),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("saved to your app.toml"),
        );

    v_flex()
        .gap_2()
        .child(dialog::filter_row(&shell.dialog_input, None, cx))
        .child(list)
        .child(footer)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn rows() -> Vec<SettingRow> {
        derive_rows(
            &names(&["Default Light", "Gruvbox Dark"]),
            "Gruvbox Dark",
            FontSize::Medium,
            FindStyle::Vim,
            LineNumbers::Off,
            AddDirection::Auto,
        )
    }

    // -- derive_rows --------------------------------------------------

    #[test]
    fn rows_come_in_display_order_with_their_categories() {
        let rows = rows();
        let identity: Vec<(SettingId, &str, &str)> =
            rows.iter().map(|r| (r.id, r.title, r.category)).collect();
        assert_eq!(
            identity,
            vec![
                (SettingId::Theme, "Theme", "Appearance"),
                (SettingId::FontSize, "Font size", "Appearance"),
                (SettingId::FindStyle, "Find style", "Keyboard"),
                (SettingId::LineNumbers, "Line numbers", "Appearance"),
                (SettingId::AddDirection, "Add tile", "Tiling"),
            ]
        );
    }

    #[test]
    fn each_row_carries_its_value_labels_and_current_index() {
        let rows = rows();
        assert_eq!(rows[0].values, names(&["Default Light", "Gruvbox Dark"]));
        assert_eq!(rows[0].current, 1, "the active theme is current");
        assert_eq!(rows[1].values, names(&["Small", "Medium", "Large"]));
        assert_eq!(rows[1].current, 1, "FontSize::Medium is ALL[1]");
        assert_eq!(rows[2].values, names(&["Vim", "Fzf"]));
        assert_eq!(rows[2].current, 0, "FindStyle::Vim is ALL[0]");
        assert_eq!(rows[3].values, names(&["Off", "On", "Relative"]));
        assert_eq!(rows[3].current, 0, "LineNumbers::Off is ALL[0]");
        assert_eq!(rows[4].values, vec!["Horizontal", "Vertical", "Auto"]);
        assert_eq!(rows[4].current, 2, "AddDirection::Auto is ALL[2]");
    }

    #[test]
    fn every_row_reads_its_own_current_value() {
        let rows = derive_rows(
            &names(&["A"]),
            "A",
            FontSize::Small,
            FindStyle::Fzf,
            LineNumbers::Relative,
            AddDirection::Auto,
        );
        assert_eq!(rows[1].current, 0, "FontSize::Small is ALL[0]");
        assert_eq!(rows[2].current, 1, "FindStyle::Fzf is ALL[1]");
        assert_eq!(rows[3].current, 2, "LineNumbers::Relative is ALL[2]");
    }

    #[test]
    fn there_is_no_dark_mode_row() {
        // Retired 2026-09-12: every theme name already carries its mode.
        let rows = rows();
        assert!(rows.iter().all(|r| r.title != "Dark mode"));
        let mut state = SettingsState::new();
        state.set_query("dark".to_string());
        assert!(visible_rows(&state, &rows).is_empty());
    }

    #[test]
    fn an_unknown_active_theme_falls_back_to_the_first_name() {
        let rows = derive_rows(
            &names(&["A", "B"]),
            "no-such-theme",
            FontSize::Medium,
            FindStyle::Vim,
            LineNumbers::Off,
            AddDirection::Auto,
        );
        assert_eq!(
            rows[0].current, 0,
            "deterministic fallback, never an out-of-range index"
        );
    }

    #[test]
    fn the_last_row_is_the_add_direction_in_the_tiling_category() {
        let rows = derive_rows(
            &names(&["A"]),
            "A",
            FontSize::Small,
            FindStyle::Vim,
            LineNumbers::Off,
            AddDirection::Vertical,
        );
        assert_eq!(rows.len(), 5);
        let row = &rows[4];
        assert_eq!(row.id, SettingId::AddDirection);
        assert_eq!(row.title, "Add tile");
        assert_eq!(row.category, "Tiling");
        assert_eq!(row.values, vec!["Horizontal", "Vertical", "Auto"]);
        assert_eq!(row.current, 1);
    }

    // -- step ----------------------------------------------------------

    #[test]
    fn step_moves_one_value_in_each_direction() {
        assert_eq!(step(3, 1, StepDirection::Right), 2);
        assert_eq!(step(3, 1, StepDirection::Left), 0);
    }

    #[test]
    fn step_wraps_at_both_ends() {
        assert_eq!(step(3, 2, StepDirection::Right), 0, "right past the end");
        assert_eq!(step(3, 0, StepDirection::Left), 2, "left past the start");
    }

    #[test]
    fn stepping_a_two_value_row_is_a_toggle_either_way() {
        // Bool rows are two-value enums — wrap makes tab and shift+tab
        // both toggle.
        assert_eq!(step(2, 0, StepDirection::Right), 1);
        assert_eq!(step(2, 1, StepDirection::Right), 0);
        assert_eq!(step(2, 0, StepDirection::Left), 1);
        assert_eq!(step(2, 1, StepDirection::Left), 0);
    }

    #[test]
    fn step_on_an_empty_list_stays_at_zero() {
        assert_eq!(step(0, 0, StepDirection::Right), 0);
        assert_eq!(step(0, 0, StepDirection::Left), 0);
    }

    // -- searchable_text ------------------------------------------------

    #[test]
    fn searchable_text_is_title_and_category_never_the_values() {
        let rows = rows();
        assert_eq!(searchable_text(&rows[0]), "Theme Appearance");
        assert_eq!(searchable_text(&rows[2]), "Find style Keyboard");
        assert!(
            !searchable_text(&rows[0]).contains("Gruvbox"),
            "value labels must not participate — matching would depend on \
             the current value rather than what the row is"
        );
    }

    // -- the filter (crate::listfilter) ---------------------------------

    #[test]
    fn an_empty_query_shows_every_settings_row() {
        let rows = rows();
        let state = SettingsState::new();
        assert_eq!(
            visible_rows(&state, &rows)
                .iter()
                .map(|m| m.row)
                .collect::<Vec<_>>(),
            (0..rows.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_query_narrows_to_the_matching_rows() {
        let rows = rows();
        let mut state = SettingsState::new();
        state.set_query("font".to_string());
        let visible = visible_rows(&state, &rows);
        assert_eq!(visible.len(), 1);
        assert_eq!(rows[visible[0].row].title, "Font size");
    }

    #[test]
    fn setting_a_query_resets_the_selection_to_the_top_match() {
        let mut state = SettingsState::new();
        state.selected = 2;
        state.set_query("font".to_string());
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn a_click_resolves_a_setting_id_to_its_filtered_position() {
        // The list the user clicks is the filtered one, so a row's click
        // handler (keyed by SettingId, as it always was) must resolve to a
        // position in THAT list, not in the full one — the settings twin
        // of keybindings' `a_click_resolves_an_action_id_to_its_filtered_position`.
        let rows = rows();
        let mut state = SettingsState::new();
        state.set_query(rows[2].title.to_string());
        let visible = visible_rows(&state, &rows);
        assert_eq!(
            filtered_position(&visible, &rows, rows[2].id),
            Some(0),
            "the only match sits at filtered position 0, whatever its \
             position in the full list"
        );
    }

    #[test]
    fn a_click_on_a_row_the_filter_hid_resolves_to_nothing() {
        let rows = rows();
        let mut state = SettingsState::new();
        state.set_query(rows[2].title.to_string());
        let visible = visible_rows(&state, &rows);
        let hidden = rows
            .iter()
            .find(|r| !visible.iter().any(|m| rows[m.row].id == r.id))
            .expect("the query must hide at least one row");
        assert_eq!(filtered_position(&visible, &rows, hidden.id), None);
    }

    // -- click_selects_or_steps ------------------------------------------

    #[test]
    fn a_click_on_a_different_row_selects_it_without_stepping() {
        let mut state = SettingsState::new();
        assert!(!click_selects_or_steps(&mut state, 2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn a_click_on_the_selected_row_asks_for_a_forward_cycle() {
        let mut state = SettingsState::new();
        state.selected = 1;
        assert!(click_selects_or_steps(&mut state, 1));
        assert_eq!(
            state.selected, 1,
            "a cycle click leaves the selection where it was"
        );
    }

    // -- mod_alias_label -------------------------------------------------

    #[test]
    fn mod_alias_label_matches_each_named_alias() {
        assert_eq!(mod_alias_label(Modifiers::CTRL), "ctrl");
        assert_eq!(mod_alias_label(Modifiers::CMD), "cmd");
        assert_eq!(mod_alias_label(Modifiers::ALT), "alt");
    }

    #[test]
    fn mod_alias_label_falls_back_to_alt_for_an_unnamed_combination() {
        assert_eq!(
            mod_alias_label(Modifiers::NONE),
            "alt",
            "an unrecognized alias should read as the same default \
             defaults::default_mod uses, not silently mislabel"
        );
    }
}
