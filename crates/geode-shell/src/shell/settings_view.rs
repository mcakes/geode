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
//! vim vocabulary every other list surface here has (`crate::vimnav`
//! motions, `/` find via `crate::vimfind`), and a stack of documented
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
//! Four rows, derived FRESH from `ShellView` state on every render and
//! every keystroke ([`derive_rows`] via [`rows_for`] — same no-caching
//! contract as `keybindings_view::derive_rows`): Theme, Dark mode, and
//! Font size under **Appearance**, Find style under **Keyboard**. Each row
//! is one enumerated setting — an ordered list of value labels plus the
//! index of the currently-active one — and editing is *stepping*:
//! `h`/`left` and `l`/`right` step the selected row's value (wrapping at
//! both ends, [`step`]), `enter`/`space` cycle forward, and a click on the
//! already-selected row cycles forward too (the mirror of keybindings'
//! click-to-listen). A step applies IMMEDIATELY through the same
//! apply-then-persist seams the old dialog's controls used
//! ([`set_theme`]/[`set_dark_mode`]/[`set_font_size`]/[`set_find_style`]'s
//! shared `*_on` cores) — theme stepping is a live preview, and
//! persistence stays on the existing background paths
//! (`ShellView::persist_theme` and friends; no I/O lands on the render
//! thread here).
//!
//! The old dialog's read-only content — the "Mod key: …" line and the
//! "saved to your app.toml" caption — survives as muted inert footer
//! lines, not rows: there is nothing to step on either.
//!
//! ## What deliberately did NOT change
//!
//! The four setter helpers ([`set_theme`], [`set_dark_mode`],
//! [`set_font_size`], [`set_find_style`]) keep their exact
//! `Entity<ShellView>`-taking signatures and semantics — they are the
//! seam `shell::mod`'s tests drive directly, and nothing about *applying*
//! a setting changed, only the control surface in front of it. Each now
//! delegates to a `*_on(&mut ShellView, ...)` core so this module's own
//! [`handle_key`] (which already holds `&mut ShellView` mid-key-dispatch
//! and must not reenter the entity via `Entity::update`) can apply the
//! identical path.
//!
//! ## Find (`/`), both styles
//!
//! Driven by the shared session drivers in [`crate::vimfind`] (promoted
//! out of `keybindings_view` by this same rewrite, and now this dialog's
//! only caller — the keybinding dialog has since moved to an
//! always-focused fuzzy filter, which this dialog has yet to follow):
//! vim style jumps the selection incrementally from
//! the anchor with `n`/`shift+n` repeats; fzf style narrows the rendered
//! list, `up`/`down` step the matches, and `enter` picks — with the one
//! settings-specific meaning of *picking*: the session just ends, full
//! list back, selection on the picked row (there is no "listening" to
//! start here — [`vimfind::FzfOutcome::Picked`] needs no extra work at
//! all). Searchable text per row is title + category
//! ([`searchable_text`]) — exactly what the row displays, matching
//! keybindings' philosophy of never matching invisible text.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, HighlightStyle, Hsla, MouseButton, StyledText,
    Window, div, px,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::fontsize::FontSize;
use crate::keymap::Keystroke;
use crate::keymap::Modifiers;
use crate::shell::ShellView;
use crate::shell::dialog;
use crate::theme::Mode;
use crate::vimfind::{self, FindDirection, FindStyle, VimFind, filter_matches};
use crate::vimnav::{self, NavResult, VimListNav};

use super::keybindings_view::key_chip;

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
    DarkMode,
    FontSize,
    FindStyle,
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
/// dialog's fixed display order: the Appearance rows first (Theme, Dark
/// mode, Font size), then Keyboard (Find style). A boolean setting is a
/// two-value enum (`off`/`on`) so stepping and cycling need no special
/// case — wrapping a two-element list IS a toggle.
///
/// An `active_theme` not present in `theme_names` (impossible via the UI —
/// `ThemeService::apply` only ever activates a bundled name — but cheap to
/// be deterministic about) marks the first theme as current rather than
/// panicking or carrying an out-of-range index into [`step`].
pub fn derive_rows(
    theme_names: &[String],
    active_theme: &str,
    dark: bool,
    font_size: FontSize,
    find_style: FindStyle,
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
            id: SettingId::DarkMode,
            title: "Dark mode",
            category: "Appearance",
            values: vec!["off".to_string(), "on".to_string()],
            current: usize::from(dark),
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
    ]
}

/// Which way a value step goes — `h`/`left` vs. `l`/`right` (and
/// `enter`/`space`/same-row click, which cycle forward = `Right`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepDirection {
    Left,
    Right,
}

/// The value index one step from `current` in a `len`-value list,
/// WRAPPING at both ends — deliberately unlike `vimnav::apply`'s clamped
/// list navigation: a settings value is a cycle (fzf → vim → fzf), not a
/// list with ends, and wrap is what makes `enter`'s cycle-forward reach
/// every value. `len == 0` yields 0 (unreachable for real rows — every
/// setting has at least two values — but deterministic).
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
    pub selected: usize,
    pub nav: VimListNav,
    /// Vim-style `/` find over the rows ([`crate::vimfind`]): the session
    /// state machine plus the committed query for vim-style `n`/`N`.
    pub find: VimFind,
    /// The selection when `/` was pressed — the incremental jump's origin,
    /// and what `escape` restores. `None` outside a find session.
    pub find_anchor: Option<usize>,
}

impl SettingsState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The text one row exposes to `/` find: title and category — exactly
/// what the row displays as its identity, and nothing more. The value
/// labels deliberately do NOT participate (same philosophy as
/// keybindings' `searchable_text` excluding the invisible action id, one
/// step further: values ARE visible, but `/gruvbox` jumping to the Theme
/// row only when Gruvbox happens to be active — and `/large` matching
/// Font size only sometimes — would make matching depend on current
/// state rather than on what the row *is*).
pub fn searchable_text(row: &SettingRow) -> String {
    format!("{} {}", row.title, row.category)
}

/// What a click on row `clicked_ix` does to already-open dialog state:
/// clicking any other row selects it (cancelling any in-progress find
/// session or pending nav gesture — the click wins, same as keybindings'
/// `click_selects_or_listens`); clicking the already-selected row means
/// "cycle this row's value forward" — returns `true` so the gpui caller
/// ([`on_row_clicked`]) applies the step, keeping this function pure.
pub fn click_selects_or_steps(state: &mut SettingsState, clicked_ix: usize) -> bool {
    state.find.cancel();
    state.find_anchor = None;
    if state.selected == clicked_ix {
        true
    } else {
        state.selected = clicked_ix;
        state.nav.cancel();
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
        SettingId::DarkMode => set_dark_mode_on(shell, value_ix == 1, cx),
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

/// [`set_theme_on`]'s sibling for the dark-mode row/switch value.
fn set_dark_mode_on(shell: &mut ShellView, dark: bool, cx: &mut Context<ShellView>) {
    let mode = if dark { Mode::Dark } else { Mode::Light };
    shell.services.theme.set_mode(mode, cx);
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
/// state itself: both list dialogs read `ShellView::find_style` fresh on
/// every keystroke and render — including THIS one, so stepping the row
/// re-labels its own value and swaps the dialog's own find behavior (and
/// its footer's find hints) on the very next render.
fn set_find_style_on(shell: &mut ShellView, style: FindStyle, cx: &mut Context<ShellView>) {
    shell.find_style = style;
    shell.persist_find_style(cx);
    cx.notify();
}

/// Apply a theme by name — the `Entity<ShellView>`-taking seam kept from
/// the pre-rewrite dialog (its doc'd purpose — a directly drivable
/// handler for tests, since the old dropdown's popup overlay couldn't be
/// clicked from a `#[gpui::test]` — still holds, and `shell::mod`'s tests
/// still call it). Now a thin `Entity::update` wrapper over
/// [`set_theme_on`], the same core this dialog's `h`/`l` stepping applies
/// through. Only test code calls these four wrappers since the rewrite
/// (the dialog itself already holds `&mut ShellView` mid-key-dispatch and
/// must use the cores directly), hence the not-test `dead_code`
/// allowance rather than `#[cfg(test)]`: they stay compiled, documented,
/// and reachable for any future non-modal caller.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_theme(view: &Entity<ShellView>, name: &str, cx: &mut App) {
    view.update(cx, |shell, cx| set_theme_on(shell, name, cx));
}

/// Apply the mode a dark-mode value implies — same survival story as
/// [`set_theme`].
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_dark_mode(view: &Entity<ShellView>, checked: bool, cx: &mut App) {
    view.update(cx, |shell, cx| set_dark_mode_on(shell, checked, cx));
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
/// Rows visible before the list scrolls. Four rows today, so nothing
/// scrolls — kept anyway so the list's sizing arithmetic stays identical
/// to keybindings' and a fifth setting never needs layout thought.
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
        shell.services.theme.active_mode().is_dark(),
        shell.font_size,
        shell.find_style,
    )
}

/// Open the settings modal (`settings::open`: `ctrl+,`, the palette
/// entry, and the sidebar profile icon all reach this). A no-op if a
/// modal is already open — re-triggering the action must not clobber
/// whatever's up. Fresh [`SettingsState`] every open, nothing survives a
/// close/reopen — the same contract as the palette and the keybinding
/// dialog. Goes through [`dialog::open_shell_dialog_with_key`] (the one
/// standard door, with the Part B key seam): this dialog needs first
/// refusal on every keystroke for vim nav, `/` find, and value stepping.
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
        // No filter field yet: this dialog still drives `/` find and vim
        // motions, and `build` does not render `dialog::filter_row`.
        // Focusing an input that is not in the rendered element tree
        // would route every key to the window root, past `ShellView`'s
        // own listener — so it stays `false` until this dialog moves to
        // the shared filter too.
        false,
    );
}

/// The [`dialog::ModalKeyHandler`] for this dialog. Priority order (the
/// shape `keybindings_view::handle_key` had before that dialog moved to
/// the shared filter, with stepping in place of rebind capture):
///
/// 1. an active find session owns every keystroke (vim or fzf per
///    `ShellView::find_style`, via the shared `vimfind` drivers — an fzf
///    pick needs nothing extra here, the session ending IS the pick);
/// 2. `/` starts a session; `n`/`shift+n` repeat-find in vim style only;
/// 3. `vimnav` motions (j/k with counts, gg/G, ctrl+d/u/f/b);
/// 4. bare `h`/`left` and `l`/`right` step the selected row's value
///    (wrapping), bare `enter`/`space` cycle it forward — applied
///    immediately through [`apply_setting`]. Reached only when `vimnav`
///    reported `NotNav`, which also means a pending count/`g` gesture
///    aborts on these keys rather than combining with them (`5l` steps
///    once — counts belong to j/k alone, same rule as everywhere else);
/// 5. anything else — bare `escape` included — returns `false`, falling
///    through to `handle_key_down`'s own "escape closes the modal".
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    _window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let rows = rows_for(shell);
    let find_style = shell.find_style;
    let Some(state) = shell.settings.as_mut() else {
        return false;
    };

    // Find mode — priority 1: an active session owns every keystroke
    // (even ones it ignores, so stray chords can't leak into list nav or
    // value stepping mid-search).
    if state.find.is_active() {
        let texts: Vec<String> = rows.iter().map(searchable_text).collect();
        match find_style {
            FindStyle::Vim => vimfind::press_while_finding(
                &mut state.find,
                &mut state.selected,
                &mut state.find_anchor,
                &texts,
                ks,
            ),
            FindStyle::Fzf => {
                // `Picked` needs no interpretation here (module doc: the
                // session ending, anchor dropped, selection on the picked
                // row — all done inside the driver — IS the pick).
                let _ = vimfind::press_while_finding_fzf(
                    &mut state.find,
                    &mut state.selected,
                    &mut state.find_anchor,
                    &texts,
                    ks,
                );
            }
        }
        // Scroll within whatever list is actually rendered — the narrowed
        // match list during a live fzf session, the full list otherwise
        // (same position mapping as keybindings').
        let scroll_ix = if find_style == FindStyle::Fzf && state.find.is_active() {
            filter_matches(&texts, state.find.query().unwrap_or(""))
                .iter()
                .position(|&ix| ix == state.selected)
                .unwrap_or(0)
        } else {
            state.selected
        };
        shell.settings_scroll.scroll_to_item(scroll_ix);
        cx.notify();
        return true;
    }
    if ks.mods == Modifiers::NONE && ks.key == "/" {
        state.find_anchor = Some(state.selected);
        state.find.start();
        state.nav.cancel();
        cx.notify();
        return true;
    }
    // `n`/`shift+n` repeat-find is vim-mode-only (fzf sessions end by
    // picking or cancelling; there is no committed query to repeat).
    if find_style == FindStyle::Vim
        && ks.key == "n"
        && (ks.mods == Modifiers::NONE
            || ks.mods
                == Modifiers {
                    shift: true,
                    ..Modifiers::NONE
                })
    {
        let dir = if ks.mods.shift {
            FindDirection::Backward
        } else {
            FindDirection::Forward
        };
        let texts: Vec<String> = rows.iter().map(searchable_text).collect();
        if vimfind::repeat_find(&state.find, &mut state.selected, &texts, dir) {
            let selected = state.selected;
            shell.settings_scroll.scroll_to_item(selected);
            cx.notify();
            return true;
        }
    }

    match state.nav.press(ks) {
        NavResult::Command(cmd) => {
            state.selected = vimnav::apply(state.selected, rows.len(), cmd);
            let selected = state.selected;
            shell.settings_scroll.scroll_to_item(selected);
            cx.notify();
            true
        }
        NavResult::Pending => {
            cx.notify();
            true
        }
        NavResult::NotNav => {
            let dir = match (ks.mods == Modifiers::NONE, ks.key.as_str()) {
                (true, "h" | "left") => Some(StepDirection::Left),
                (true, "l" | "right" | "enter" | "space") => Some(StepDirection::Right),
                _ => None,
            };
            let Some(dir) = dir else {
                return false;
            };
            let Some(row) = rows.get(state.selected) else {
                return false;
            };
            let (id, new_ix) = (row.id, step(row.values.len(), row.current, dir));
            apply_setting(shell, id, new_ix, cx);
            cx.notify();
            true
        }
    }
}

/// Selection/step logic for a real mouse click on the row for `clicked`
/// (`SettingId`, resolved back to an index against freshly derived rows —
/// same identity-not-position keying as keybindings' `on_row_clicked`).
/// The gpui-facing wrapper around the pure [`click_selects_or_steps`]: a
/// `true` (already-selected row) cycles that row's value forward.
fn on_row_clicked(shell: &mut ShellView, clicked: SettingId, cx: &mut Context<ShellView>) {
    let rows = rows_for(shell);
    let Some(ix) = rows.iter().position(|r| r.id == clicked) else {
        return;
    };
    let Some(state) = shell.settings.as_mut() else {
        return;
    };
    let cycle = click_selects_or_steps(state, ix);
    let selected = state.selected;
    shell.settings_scroll.scroll_to_item(selected);
    if cycle {
        let row = &rows[ix];
        let new_ix = step(row.values.len(), row.current, StepDirection::Right);
        apply_setting(shell, row.id, new_ix, cx);
    }
    cx.notify();
}

/// One line of a row's label with the find query's matched span lit —
/// contiguous substring, so a background tint (`primary` at 20%, the same
/// tint idiom the sidebar's active workspace disc uses — no raw colors)
/// plus bold, via `StyledText::with_highlights` exactly as the palette's
/// `highlighted_title` does (byte range from [`vimfind::match_range`]).
/// Plain text when there's no query or this line doesn't contain it.
///
/// Lives here, private, rather than in `keybindings_view`: that module's
/// shared [`highlighted_text`](super::keybindings_view::highlighted_text)
/// moved to fuzzy-match *indices* when the keybinding dialog adopted the
/// filter-first UX, and a subsequence match has no single contiguous
/// range to hand `match_range`. This dialog is still on `/` find, so it
/// keeps the substring rendering until it moves too — at which point this
/// goes away in favour of the shared helper again.
fn highlighted_find_text(text: &str, query: Option<&str>, primary: Hsla) -> AnyElement {
    match query.and_then(|q| vimfind::match_range(text, q)) {
        Some(range) => {
            let style = HighlightStyle {
                background_color: Some(primary.opacity(0.2)),
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            };
            StyledText::new(text.to_string())
                .with_highlights([(range, style)])
                .into_any_element()
        }
        None => div().child(text.to_string()).into_any_element(),
    }
}

/// The [`dialog::ShellModal::build`] closure body: a scrollable row list
/// (title + muted category on the left, with find-match highlighting; the
/// current value label on the right in the mono data face) plus the
/// footer — the live `/query` line or the three-row chip hints, then the
/// two muted inert lines the old dialog's read-only content became.
/// `entity` is the `Entity<ShellView>` every row's click handler captures
/// to reach [`on_row_clicked`] at click time; `shell` is this call's own
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
    let hl_query = state.find.highlight_query();

    // While an fzf find session is active the list renders ONLY the
    // matching rows — same filter idiom, same full-list `debug_selector`
    // indices, as the keybinding dialog (rows keep their identity across
    // filtering because click handlers are keyed by `SettingId`).
    let fzf_session = shell.find_style == FindStyle::Fzf && state.find.is_active();
    let visible: Vec<usize> = if fzf_session {
        let texts: Vec<String> = rows.iter().map(searchable_text).collect();
        filter_matches(&texts, state.find.query().unwrap_or(""))
    } else {
        (0..rows.len()).collect()
    };

    let mut list = v_flex()
        .id("settings-list")
        .w(px(WIDTH))
        .h(px(
            (visible.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT)
        ))
        .overflow_y_scroll()
        .track_scroll(&shell.settings_scroll)
        .debug_selector(|| "settings-list".to_string());

    for &i in &visible {
        let row = &rows[i];
        let is_selected = i == state.selected;

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
            .child(highlighted_find_text(row.title, hl_query, theme.primary))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(highlighted_find_text(row.category, hl_query, theme.primary)),
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
            .debug_selector(move || format!("settings-row-{i}"))
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                entity_for_row.update(cx, |shell, cx| {
                    on_row_clicked(shell, id, cx);
                });
            });

        list = list.child(row_el);
    }

    if fzf_session && visible.is_empty() {
        // Zero matches: one muted line where the rows would be, same as
        // keybindings — an empty filter reads as a state, not a glitch.
        // Enter is inert here (the shared driver); the session stays live.
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

    let hint_line: AnyElement = if let Some(find_display) = state.find.pending_display() {
        // The live `/query` line, vim command-line style, both find
        // styles — rendered in the data face so the query reads as typed
        // input, not prose.
        div()
            .font_family(crate::fonts::MONO)
            .child(find_display)
            .into_any_element()
    } else {
        // Three rows, one idiom family each — motion, find, edit — same
        // table shape as the keybinding dialog's hints (and the same
        // "pending nav gestures don't take over this line" choice).
        v_flex()
            .gap_0p5()
            .child(h_flex().gap_1().items_center().flex_wrap().children(vec![
                chip("j"),
                chip("k"),
                sep("move (counts: 5j) ·"),
                chip("g"),
                chip("g"),
                sep("/"),
                chip("shift+g"),
                sep("top/bottom ·"),
                chip("ctrl+d"),
                chip("ctrl+u"),
                chip("ctrl+f"),
                chip("ctrl+b"),
                sep("page"),
            ]))
            .child(match shell.find_style {
                FindStyle::Vim => h_flex().gap_1().items_center().flex_wrap().children(vec![
                    chip("/"),
                    sep("find,"),
                    chip("n"),
                    sep("/"),
                    chip("shift+n"),
                    sep("next"),
                ]),
                FindStyle::Fzf => h_flex().gap_1().items_center().flex_wrap().children(vec![
                    chip("/"),
                    sep("filter,"),
                    chip("up"),
                    sep("/"),
                    chip("down"),
                    sep("move,"),
                    chip("enter"),
                    sep("pick"),
                ]),
            })
            .child(h_flex().gap_1().items_center().flex_wrap().children(vec![
                chip("h"),
                sep("/"),
                chip("l"),
                sep("change value ·"),
                chip("enter"),
                sep("cycle"),
            ]))
            .into_any_element()
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
        .child(list)
        .child(footer)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::NONE,
            key: k.to_string(),
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn rows() -> Vec<SettingRow> {
        derive_rows(
            &names(&["Default Light", "Gruvbox Dark"]),
            "Gruvbox Dark",
            true,
            FontSize::Medium,
            FindStyle::Vim,
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
                (SettingId::DarkMode, "Dark mode", "Appearance"),
                (SettingId::FontSize, "Font size", "Appearance"),
                (SettingId::FindStyle, "Find style", "Keyboard"),
            ]
        );
    }

    #[test]
    fn each_row_carries_its_value_labels_and_current_index() {
        let rows = rows();
        assert_eq!(rows[0].values, names(&["Default Light", "Gruvbox Dark"]));
        assert_eq!(rows[0].current, 1, "the active theme is current");
        assert_eq!(rows[1].values, names(&["off", "on"]));
        assert_eq!(rows[1].current, 1, "dark = true reads as 'on'");
        assert_eq!(rows[2].values, names(&["Small", "Medium", "Large"]));
        assert_eq!(rows[2].current, 1, "FontSize::Medium is ALL[1]");
        assert_eq!(rows[3].values, names(&["Vim", "Fzf"]));
        assert_eq!(rows[3].current, 0, "FindStyle::Vim is ALL[0]");
    }

    #[test]
    fn a_light_mode_shell_reads_dark_mode_off() {
        let rows = derive_rows(&names(&["A"]), "A", false, FontSize::Small, FindStyle::Fzf);
        assert_eq!(rows[1].current, 0, "dark = false reads as 'off'");
        assert_eq!(rows[2].current, 0);
        assert_eq!(rows[3].current, 1);
    }

    #[test]
    fn an_unknown_active_theme_falls_back_to_the_first_name() {
        let rows = derive_rows(
            &names(&["A", "B"]),
            "no-such-theme",
            false,
            FontSize::Medium,
            FindStyle::Vim,
        );
        assert_eq!(
            rows[0].current, 0,
            "deterministic fallback, never an out-of-range index"
        );
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
        // Bool rows are two-value enums — wrap makes h and l both toggle.
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
        assert_eq!(searchable_text(&rows[3]), "Find style Keyboard");
        assert!(
            !searchable_text(&rows[0]).contains("Gruvbox"),
            "value labels must not participate — matching would depend on \
             the current value rather than what the row is"
        );
    }

    // -- find sessions over settings rows (the shared drivers) ----------

    fn texts() -> Vec<String> {
        rows().iter().map(searchable_text).collect()
    }

    fn finding_state(selected: usize) -> SettingsState {
        let mut state = SettingsState {
            selected,
            find_anchor: Some(selected),
            ..Default::default()
        };
        state.find.start();
        state
    }

    #[test]
    fn vim_find_jumps_the_selection_to_a_matching_row() {
        let texts = texts();
        let mut state = finding_state(0);
        for k in ["k", "e", "y"] {
            vimfind::press_while_finding(
                &mut state.find,
                &mut state.selected,
                &mut state.find_anchor,
                &texts,
                &key(k),
            );
        }
        assert_eq!(
            state.selected, 3,
            "'key' matches the Keyboard category — Find style's row"
        );
        vimfind::press_while_finding(
            &mut state.find,
            &mut state.selected,
            &mut state.find_anchor,
            &texts,
            &key("escape"),
        );
        assert_eq!(state.selected, 0, "escape restores the anchor");
    }

    #[test]
    fn fzf_pick_just_ends_the_session_with_selection_on_the_picked_row() {
        // The settings-specific pick semantics (module doc): enter ends
        // the session — full list back, selection kept — and nothing
        // else. No listening state exists to start.
        let texts = texts();
        let mut state = finding_state(0);
        vimfind::press_while_finding_fzf(
            &mut state.find,
            &mut state.selected,
            &mut state.find_anchor,
            &texts,
            &key("f"),
        );
        assert_eq!(state.selected, 2, "first 'f' match is Font size");
        let outcome = vimfind::press_while_finding_fzf(
            &mut state.find,
            &mut state.selected,
            &mut state.find_anchor,
            &texts,
            &key("enter"),
        );
        assert_eq!(outcome, vimfind::FzfOutcome::Picked);
        assert!(!state.find.is_active(), "the session is over");
        assert_eq!(state.selected, 2, "selection stays on the picked row");
        assert_eq!(state.find_anchor, None, "no anchor residue");
    }

    #[test]
    fn fzf_enter_on_zero_matches_stays_inert() {
        let texts = texts();
        let mut state = finding_state(1);
        for k in ["z", "z"] {
            vimfind::press_while_finding_fzf(
                &mut state.find,
                &mut state.selected,
                &mut state.find_anchor,
                &texts,
                &key(k),
            );
        }
        let outcome = vimfind::press_while_finding_fzf(
            &mut state.find,
            &mut state.selected,
            &mut state.find_anchor,
            &texts,
            &key("enter"),
        );
        assert_eq!(outcome, vimfind::FzfOutcome::Continue);
        assert!(state.find.is_active(), "no row to pick — session survives");
    }

    // -- click_selects_or_steps ------------------------------------------

    #[test]
    fn clicking_a_different_row_selects_it_without_stepping() {
        let mut state = SettingsState {
            selected: 0,
            ..Default::default()
        };
        assert!(!click_selects_or_steps(&mut state, 2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn clicking_the_selected_row_asks_for_a_forward_cycle() {
        let mut state = SettingsState {
            selected: 2,
            ..Default::default()
        };
        assert!(click_selects_or_steps(&mut state, 2));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn a_click_cancels_an_active_find_session() {
        let mut state = finding_state(0);
        assert!(state.find.is_active());
        assert!(!click_selects_or_steps(&mut state, 1));
        assert!(!state.find.is_active());
        assert_eq!(state.find_anchor, None);
        assert_eq!(state.selected, 1);
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
