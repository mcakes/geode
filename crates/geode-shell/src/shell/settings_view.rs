//! The settings modal: a keyboard-driven flat list of typed settings.
//!
//! Rows derive from current shell state on every render and key event. Stepping updates
//! the shell immediately and submits background persistence through each setting's
//! setter. In Normal mode, `i` or Enter opens the selected row's choice typeahead.
//!
//! `/` enters list filtering and captures its entry query. Escape restores that query;
//! bare Enter keeps the edited query. Both return to Normal without opening a choice or
//! closing the dialog. A subsequent Normal-mode Enter opens the selected row. Choice
//! entry has its own query and key routing: Enter picks an option and Escape cancels,
//! independently of list-filter exits.
//!
//! Pure `SettingsState` owns mode and queries. `dialog::sync_dialog_text` reconciles
//! them with the shared input and focus after transitions. Normal mode blurs the input
//! so bare letters act as commands.
//!
//! Visible settings are Theme, Font size, Line numbers, Find style, Add tile direction,
//! and timeseries default source. With no configured sources, its only option is
//! `(none)`. Theme names include appearance, so there is no separate mode row.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, MouseButton, Window, div};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::dialogmode::{self, DialogMode, EscapeStep, FilterExit, NormalCommand};
use crate::fontsize::FontSize;
use crate::footer::{Hint, HintRow};
use crate::keymap::Keystroke;
use crate::keymap::Modifiers;
use crate::linenumbers::LineNumbers;
use crate::listfilter::{self, Ranked};
use crate::shell::ShellView;
use crate::shell::dialog;
use crate::shell::scale;
use crate::tileadd::AddDirection;
use crate::vimfind::FindStyle;
use crate::vimnav;
use crate::vimnav::NavCommand;

use super::keybindings_view::{highlighted_text, split_label_indices};

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
    DefaultSource,
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

/// Build rows from plain inputs in fixed order: Theme, Font size, Find style, Line
/// numbers, Add tile direction, and Default series source. The source row always
/// includes `(none)`, even with no configured sources. [`rows_for`] supplies the live
/// shell values.
///
/// An unknown active theme falls back to index zero. An unavailable default source
/// selects `(none)`.
#[allow(clippy::too_many_arguments)]
pub fn derive_rows(
    theme_names: &[String],
    active_theme: &str,
    font_size: FontSize,
    find_style: FindStyle,
    line_numbers: LineNumbers,
    add_direction: AddDirection,
    default_source: Option<&str>,
    fetch_sources: &[String],
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
        SettingRow {
            id: SettingId::DefaultSource,
            title: "Default series source",
            category: "Timeseries",
            values: std::iter::once(NO_DEFAULT_SOURCE.to_string())
                .chain(fetch_sources.iter().cloned())
                .collect(),
            current: default_source
                .and_then(|d| fetch_sources.iter().position(|s| s == d))
                .map_or(0, |i| i + 1),
        },
    ]
}

/// First source option, identified by index zero rather than its label. Choosing it
/// removes `[timeseries] default_source`; a source named `(none)` still has its own
/// nonzero index.
const NO_DEFAULT_SOURCE: &str = "(none)";

/// Direction of a cyclic value step. Tab/Shift-Tab work in either list mode; Normal
/// also accepts the shared forward/backward commands. A value-chip click steps forward
/// and Shift-click steps backward. Row clicks select, and Normal Enter opens a choice
/// instead of stepping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepDirection {
    Left,
    Right,
}

/// Wrap a single step within the value list in either direction. An empty list yields
/// zero; callers normally supply the selected row's available values.
pub fn step(len: usize, current: usize, dir: StepDirection) -> usize {
    if len == 0 {
        return 0;
    }
    match dir {
        StepDirection::Right => (current + 1) % len,
        StepDirection::Left => (current + len - 1) % len,
    }
}

/// The open setting's identity and ranked choice options. Choice query and selection
/// are separate from the dialog's list filter.
#[derive(Debug)]
pub struct ChoiceEntry {
    pub id: SettingId,
    pub list: crate::choice::ChoiceList,
}

/// Pure state for one open settings dialog, recreated on each open. Owns the
/// list-filter query and its entry snapshot separately from an optional choice field.
/// The scroll handle remains on `ShellView`.
#[derive(Debug)]
pub struct SettingsState {
    /// Index into the **filtered** row list ([`visible_rows`]), not the
    /// full one — the same convention `KeybindingsState::selected` uses.
    pub selected: usize,
    /// List-filter query mirrored from input while no choice field is open. Bare Enter
    /// keeps it when leaving Filter; Escape restores `filter_entry_query`.
    pub query: String,
    /// List query captured by `enter_filter` at the start of this filter session.
    /// `exit_filter` restores it on Escape; it does not restore selection or values
    /// stepped while filtering.
    pub filter_entry_query: String,
    /// Keyboard mode reconciled with input focus through `sync_dialog_text`. An open
    /// choice field uses Filter for text entry but has its own key route.
    pub mode: DialogMode,
    /// Open choice field replacing the settings rows with ranked options and the filter
    /// input with value entry. Its query is independent of the list filter.
    pub choice: Option<ChoiceEntry>,
}

/// The dialog explicitly opens in Normal; `DialogMode` has no global default.
impl Default for SettingsState {
    fn default() -> Self {
        Self {
            selected: 0,
            query: String::new(),
            filter_entry_query: String::new(),
            mode: DialogMode::Normal,
            choice: None,
        }
    }
}

impl SettingsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The text the shared `Input` should hold: the choice field's own
    /// query while one is open, the filter query otherwise — what
    /// `dialog::sync_dialog_text` mirrors (the object dialog's
    /// `effective_query`, for the same reason).
    pub fn effective_query(&self) -> &str {
        match self.choice.as_ref() {
            Some(entry) => entry.list.query(),
            None => self.query.as_str(),
        }
    }

    pub fn choosing(&self) -> bool {
        self.choice.is_some()
    }

    /// Replace the query and reset the selection to the top match — the
    /// pure half of the `InputEvent::Change` subscription in
    /// `ShellView::new`. While a choice field is open the keystroke feeds
    /// ITS query instead (`SettingsState::query` is the filter's alone),
    /// exactly as `effective_query` reads back whichever is live.
    pub fn set_query(&mut self, query: String) {
        if let Some(entry) = self.choice.as_mut() {
            entry.list.set_query(&query);
            return;
        }
        self.query = query;
        self.selected = 0;
    }
}

/// Search title and category only. Excluding current values keeps a setting's search
/// identity stable when its value changes.
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

/// What one keystroke does to this dialog — decided by [`route`] from
/// the mode, the query and the keystroke alone, so the whole key
/// vocabulary is a pure table a unit test can walk without a window.
/// [`handle_key`] only applies the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Move the selection within the filtered list.
    Nav(NavCommand),
    /// Step the selected row's value, applied immediately.
    Step(StepDirection),
    /// `/` in normal mode: hand the keys to the filter.
    EnterFilter,
    /// Return to Normal after keeping the typed query with bare Enter or restoring the
    /// entry query with Escape. This does not open a choice or close the dialog.
    LeaveFilter(FilterExit),
    /// The ladder's second rung: clear the applied query.
    ClearQuery,
    /// Open the selected row's choice typeahead from `i` or bare Enter in Normal.
    OpenChoice,
    /// A keystroke while a row's typeahead is open — the one table every
    /// choice field reads (`crate::choice::route`).
    Choice(crate::choice::ChoiceKey),
    /// Claimed and dropped — every key normal mode does not name.
    Drop,
    /// Not this dialog's to claim: a printable key on its way to the
    /// focused filter, or the ladder's last rung, which the shell's own
    /// modal branch turns into a close.
    PassThrough,
}

/// Tab/Shift-Tab step values in both list modes. Reclaimed bindings allow these keys to
/// reach the dialog even while Input owns text. Normal also accepts space/Shift-space
/// and the shared left/right stepping commands.
fn tab_step(ks: &Keystroke) -> Option<StepDirection> {
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    match (ks.mods, ks.key.as_str()) {
        (Modifiers::NONE, "tab") => Some(StepDirection::Right),
        (SHIFT, "tab") => Some(StepDirection::Left),
        _ => None,
    }
}

/// Route an open choice field before all list commands, using `choice::route`. Choice
/// Enter picks and Escape cancels; neither uses the list-filter snapshot.
///
/// Without a choice field, Filter Escape restores its entry query and bare Enter keeps
/// its edited query. Both leave Filter and stop there. Normal Escape clears an applied
/// query before closing; Normal bare Enter or `i` opens a choice. Tab stepping and
/// filter-safe navigation work in either list mode. Other Filter keys pass to text
/// input, while unrecognized Normal commands are consumed.
pub fn route(mode: DialogMode, query_is_empty: bool, choosing: bool, ks: &Keystroke) -> KeyAction {
    // Choice entry owns its keys before the list-filter keep/revert rules.
    if choosing {
        return match crate::choice::route(ks) {
            Some(key) => KeyAction::Choice(key),
            None => KeyAction::PassThrough,
        };
    }
    // Handle list-filter exits before the Normal Escape ladder or row activation. The
    // shared table distinguishes keeping the query from restoring its snapshot.
    if mode == DialogMode::Filter
        && let Some(exit) = dialogmode::filter_exit(ks)
    {
        return KeyAction::LeaveFilter(exit);
    }
    if ks.key == "escape" {
        return match dialogmode::escape_step(mode, query_is_empty, false) {
            // Unreachable: filter mode's `escape` is answered by the exit
            // table above. Folded in rather than special-cased away,
            // because `escape_step` is the one ladder every modal surface
            // walks and forking it per call site is how the rungs drift.
            EscapeStep::LeaveFilter => KeyAction::LeaveFilter(FilterExit::Revert),
            EscapeStep::ClearQuery => KeyAction::ClearQuery,
            // `PreviousStage` is unreachable with no nested stage; folded
            // in with `Close` rather than special-cased away, because the
            // ladder is the one every modal surface walks.
            EscapeStep::PreviousStage | EscapeStep::Close => KeyAction::PassThrough,
        };
    }
    if let Some(dir) = tab_step(ks) {
        return KeyAction::Step(dir);
    }
    if let Some(cmd) = listfilter::nav_command(ks) {
        return KeyAction::Nav(cmd);
    }
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        // Filter Enter has already returned above. Only Normal Enter opens a choice.
        return KeyAction::OpenChoice;
    }
    match mode {
        DialogMode::Filter => KeyAction::PassThrough,
        DialogMode::Normal => match dialogmode::normal_command(ks) {
            Some(NormalCommand::Nav(nav)) => KeyAction::Nav(nav),
            Some(NormalCommand::EnterFilter) => KeyAction::EnterFilter,
            Some(NormalCommand::Toggle) => KeyAction::Step(StepDirection::Right),
            Some(NormalCommand::ToggleBack) => KeyAction::Step(StepDirection::Left),
            Some(NormalCommand::EditText) => KeyAction::OpenChoice,
            _ => KeyAction::Drop,
        },
    }
}

// Applying values to live shell state and submitting persistence.

/// Apply a row's selected value through the same setter used by keyboard steps, choice
/// picks, and value-chip clicks. Mutate live shell state before submitting background
/// persistence; invalid indices are ignored. Persistence failure does not roll the live
/// value back.
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
        SettingId::DefaultSource => {
            // Value 0 is `(none)`; the rest index the fetch sources in
            // the same order `derive_rows` chained them on. An
            // out-of-range index is a no-op like every other arm — not a
            // clear, which is what `get(..).cloned()` alone would make
            // it.
            if value_ix == 0 {
                shell.set_default_source(None, cx);
            } else if let Some(name) = fetch_source_names(cx).get(value_ix - 1).cloned() {
                shell.set_default_source(Some(name), cx);
            }
        }
    }
}

/// Apply the theme, request persistence, and notify. Keyboard and pointer paths use the
/// existing shell borrow; the entity wrapper delegates to this same core.
fn set_theme_on(shell: &mut ShellView, name: &str, cx: &mut Context<ShellView>) {
    shell.services.theme.apply(name, cx);
    shell.persist_theme(cx);
    cx.notify();
}

/// Store and persist font size, then notify. The next shell render applies its rem size
/// to the window.
fn set_font_size_on(shell: &mut ShellView, size: FontSize, cx: &mut Context<ShellView>) {
    shell.font_size = size;
    shell.persist_font_size(cx);
    cx.notify();
}

/// Store and persist find style, then notify. This setter updates the shell's
/// preference; feature-specific consumers receive configuration separately.
fn set_find_style_on(shell: &mut ShellView, style: FindStyle, cx: &mut Context<ShellView>) {
    shell.find_style = style;
    shell.persist_find_style(cx);
    cx.notify();
}

/// Store and persist the add direction, used by the next `ShellView::add_tile`.
fn set_add_direction_on(shell: &mut ShellView, d: AddDirection, cx: &mut Context<ShellView>) {
    shell.add_direction = d;
    shell.persist_add_direction(cx);
    cx.notify();
}

/// Entity wrapper for the theme setter, also used by tests. Modal handlers use
/// [`set_theme_on`] directly because they already hold the shell's mutable borrow.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_theme(view: &Entity<ShellView>, name: &str, cx: &mut App) {
    view.update(cx, |shell, cx| set_theme_on(shell, name, cx));
}

/// Entity wrapper for setting and persisting `[ui] font_size`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_font_size(view: &Entity<ShellView>, size: FontSize, cx: &mut App) {
    view.update(cx, |shell, cx| set_font_size_on(shell, size, cx));
}

/// Entity wrapper for setting and persisting `[ui] find_style`.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_find_style(view: &Entity<ShellView>, style: FindStyle, cx: &mut App) {
    view.update(cx, |shell, cx| set_find_style_on(shell, style, cx));
}

/// Entity wrapper for setting and persisting `[tiles] add`.
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

// GPUI wiring and rendering.

/// Estimated two-line row height for viewport sizing. Scroll-follow uses actual layout
/// through the scroll handle.
const ROW_HEIGHT: f32 = 44.0;
/// Maximum visible settings rows before the list scrolls.
const VISIBLE_ROWS: usize = 10;
/// Target dialog content width in pixels — same as the keybinding
/// dialog's, so the two sibling dialogs read as one family.
const WIDTH: f32 = 640.0;

/// [`derive_rows`] over the live shell state — the one place the pure row
/// model meets `ShellView`. Called fresh on every render ([`build`]) and
/// every keystroke ([`handle_key`]); rows are never cached, so a step's
/// effect (or a config hot reload's) is visible on the very next derive.
///
/// Fetch-source options come from `SeriesSettings`, shared with timeseries
/// tiles. This is the configured source list, not adapter startup status.
fn rows_for(shell: &ShellView, cx: &App) -> Vec<SettingRow> {
    derive_rows(
        &shell.services.theme.names(),
        shell.services.theme.active_name(),
        shell.font_size,
        shell.find_style,
        shell.line_numbers,
        shell.add_direction,
        shell.default_source.as_deref(),
        &fetch_source_names(cx),
    )
}

/// The configured fetch source names in doc order — the default-source
/// row's values after `(none)`. The shell publishes the global before
/// its first render; an absent one reads as no sources rather than a
/// panic in `render`.
fn fetch_source_names(cx: &App) -> Vec<String> {
    cx.try_global::<crate::series::SeriesSettings>()
        .map(crate::series::SeriesSettings::names)
        .unwrap_or_default()
}

/// Open fresh settings state through the shared modal lifecycle, preserving any
/// already-open modal. The custom key handler routes the settings vocabulary.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    // Install state before opening so synchronization parks Normal focus on the shell,
    // allowing bare-letter commands.
    view.settings = Some(SettingsState::new());
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        "Settings",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        // Mode-aware synchronization chooses initial focus; no filter-first override.
        false,
    );
    // The title-extra slot shows the mode pill, or `choose` for an open choice.
    dialog::set_title_extra(view, |shell, cx| {
        shell
            .settings
            .as_ref()
            .map(|s| {
                if s.choice.is_some() {
                    dialog::choose_pill(cx)
                } else {
                    dialog::mode_pill(s.mode, cx)
                }
            })
            .unwrap_or_else(|| div().into_any_element())
    });
}

/// Apply the pure action returned by `route`. Claim list-filter Enter so it cannot
/// reach the focused input and trigger a redundant Change event that resets selection.
/// Claim Tab stepping so it cannot insert a literal tab or cycle focus; scoped
/// reclaimed bindings allow it to reach this handler.
///
/// State changes here do not directly move focus or replace input text.
/// `ShellView::handle_key_down` runs `sync_dialog_text` on return to reconcile Normal,
/// list Filter, or choice-entry state with the actual input.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    _window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let rows = rows_for(shell, cx);
    let Some(state) = shell.settings.as_mut() else {
        return false;
    };
    let visible = visible_rows(state, &rows);

    match route(state.mode, state.query.is_empty(), state.choosing(), ks) {
        KeyAction::Nav(cmd) => {
            state.selected = vimnav::apply(state.selected, visible.len(), cmd);
            let selected = state.selected;
            shell.settings_scroll.scroll_to_item(selected);
        }
        KeyAction::Step(dir) => {
            let selected = state.selected;
            // An empty filtered list has nothing to step: still claimed,
            // so the keystroke does not fall through to the `Input` (see
            // the doc comment above on `tab`).
            if let Some(row) = visible.get(selected).and_then(|m| rows.get(m.row)) {
                let (id, new_ix) = (row.id, step(row.values.len(), row.current, dir));
                apply_setting(shell, id, new_ix, cx);
            }
        }
        KeyAction::EnterFilter => {
            dialogmode::enter_filter(&mut state.mode, &mut state.filter_entry_query, &state.query);
        }
        KeyAction::LeaveFilter(exit) => {
            // Keep the typed query on Enter or restore the entry query on Escape.
            // Shared input synchronization applies the resulting text and Normal-mode
            // focus.
            let changed = dialogmode::exit_filter(
                &mut state.mode,
                &state.filter_entry_query,
                &mut state.query,
                exit,
            );
            if changed {
                // The viewport has to follow a restored query for the
                // same reason the `ClearQuery` rung below does: the list
                // re-expands under a scroll offset still parked where the
                // narrowed one left it.
                state.selected = 0;
                shell.settings_scroll.scroll_to_item(0);
            }
        }
        KeyAction::ClearQuery => {
            state.query.clear();
            state.selected = 0;
            // Clearing the query changes the visible list. Reset the viewport with its
            // selection; shared input synchronization clears the text afterward.
            shell.settings_scroll.scroll_to_item(0);
        }
        KeyAction::OpenChoice => open_choice_on_selected(state, &rows, &visible),
        KeyAction::Choice(key) => match key {
            crate::choice::ChoiceKey::Cancel => {
                state.choice = None;
                state.mode = DialogMode::Normal;
            }
            crate::choice::ChoiceKey::Pick => {
                // The field's live text may never have reached the list
                // through a `Change` event (`set_value` emits none).
                let live = shell.dialog_input.read(cx).value().to_string();
                let Some(state) = shell.settings.as_mut() else {
                    return true;
                };
                let picked = state.choice.as_mut().and_then(|entry| {
                    entry.list.set_query(&live);
                    entry.list.pick().map(|ix| (entry.id, ix))
                });
                // Nothing lit: the field stays open. The settings dialog
                // has no notice slot; the empty list says it.
                if let Some((id, ix)) = picked {
                    state.choice = None;
                    state.mode = DialogMode::Normal;
                    apply_setting(shell, id, ix, cx);
                }
            }
            crate::choice::ChoiceKey::Complete => {
                if let Some(entry) = state.choice.as_mut() {
                    entry.list.complete();
                    shell
                        .settings_scroll
                        .scroll_to_item(entry.list.ranked_highlighted());
                }
            }
            crate::choice::ChoiceKey::Nav(cmd) => {
                if let Some(entry) = state.choice.as_mut() {
                    entry.list.nav(cmd);
                    // The list is a scroll container the wheel moves
                    // freely; the keys point it back at the lit row.
                    shell
                        .settings_scroll
                        .scroll_to_item(entry.list.ranked_highlighted());
                }
            }
        },
        KeyAction::Drop => return true,
        KeyAction::PassThrough => return false,
    }
    cx.notify();
    true
}

/// Open the selected row's choice list with its current value highlighted. Normal
/// `i`/Enter and a row double-click share this route. An empty list is inert. Choice
/// entry uses its own query; it does not start a list-filter session.
fn open_choice_on_selected(state: &mut SettingsState, rows: &[SettingRow], visible: &[Ranked]) {
    let selected = state.selected;
    if let Some(row) = visible.get(selected).and_then(|m| rows.get(m.row)) {
        let mut list =
            crate::choice::ChoiceList::new(row.values.clone(), crate::choice::DEFAULT_CAP);
        list.place(row.values.get(row.current).map(String::as_str));
        state.choice = Some(ChoiceEntry { id: row.id, list });
        state.mode = DialogMode::Filter;
    }
}

/// Select the clicked setting by identity; a double-click opens its choice list.
/// Synchronize text and focus afterward. This pointer route can open a choice while
/// list filtering, whereas filter Enter only accepts the search.
fn on_row_clicked(
    shell: &mut ShellView,
    clicked: SettingId,
    click_count: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let rows = rows_for(shell, cx);
    let Some(state) = shell.settings.as_mut() else {
        return;
    };
    // Ignore stale setting-row clicks while a choice list owns the surface.
    if state.choosing() {
        return;
    }
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {
        return;
    };
    state.selected = ix;
    // The second mouse-down opens the selected row through the shared choice route.
    if click_count == 2 {
        open_choice_on_selected(state, &rows, &visible);
    }
    shell.settings_scroll.scroll_to_item(ix);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Select the value chip's row and step through [`step`] and [`apply_setting`], the
/// same path as keyboard stepping. `forward` is `!shift`.
fn on_value_chip_clicked(
    shell: &mut ShellView,
    clicked: SettingId,
    forward: bool,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let rows = rows_for(shell, cx);
    let Some(state) = shell.settings.as_mut() else {
        return;
    };
    // Ignore stale value-chip callbacks while a choice field owns the surface.
    if state.choosing() {
        return;
    }
    let visible = visible_rows(state, &rows);
    let Some(ix) = filtered_position(&visible, &rows, clicked) else {
        return;
    };
    state.selected = ix;
    shell.settings_scroll.scroll_to_item(ix);
    if let Some(row) = visible.get(ix).and_then(|m| rows.get(m.row)) {
        let dir = if forward {
            StepDirection::Right
        } else {
            StepDirection::Left
        };
        let new_ix = step(row.values.len(), row.current, dir);
        apply_setting(shell, row.id, new_ix, cx);
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Build either the filtered settings list or an open choice list, with its input and
/// active hints. Borrow the shell for current render state; capture `entity` for
/// pointer handlers that run later.
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.settings.as_ref() else {
        return div().into_any_element();
    };
    let rows = rows_for(shell, cx);
    let theme = cx.theme();
    let row_paint = super::listrow::row_paint(theme);
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    // The dialog sits on `popover` (`dialog::render_modal`'s own fill).
    let chip_states = super::control::paint(
        theme,
        super::control::Rest::Filled(chip_bg),
        theme.popover,
        chip_fg,
    );
    let chip_radius = theme.radius;

    // An open choice list replaces the settings rows entirely.
    let list: AnyElement = if let Some(entry) = state.choice.as_ref() {
        let entity_for_click = entity.clone();
        dialog::choice_rows(
            &entry.list,
            "settings",
            &shell.settings_scroll,
            theme,
            move |row, window, cx| {
                entity_for_click.update(cx, |shell, cx| {
                    on_choice_row_clicked(shell, row, window, cx);
                });
            },
        )
    } else {
        // The list renders ONLY the rows that survive the filter. Safe
        // because row click handlers are keyed by `SettingId`, not
        // position (see [`filtered_position`]).
        let visible = visible_rows(state, &rows);

        let mut list = v_flex()
            .id("settings-list")
            .w(scale::design(WIDTH))
            .h(scale::design(
                (visible.len().max(1) as f32 * ROW_HEIGHT).min(VISIBLE_ROWS as f32 * ROW_HEIGHT),
            ))
            .overflow_y_scroll()
            .track_scroll(&shell.settings_scroll)
            .debug_selector(|| "settings-list".to_string());

        for (position, m) in visible.iter().enumerate() {
            let row_ix = m.row;
            let row = &rows[row_ix];
            let is_selected = position == state.selected;

            // Split ranked character offsets between their title and category lines.
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
                .child(highlighted_text(row.title, &title_ix, row_paint.accent))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(highlighted_text(row.category, &cat_ix, row_paint.accent)),
                );

            // A value-chip click steps forward; Shift-click steps backward. Its handler
            // stops propagation so the row's selection handler does not also run.
            let entity_for_chip = entity.clone();
            let chip_id = row.id;
            let on_step: dialog::StepHandler = Rc::new(move |forward, window, cx| {
                entity_for_chip.update(cx, |shell, cx| {
                    on_value_chip_clicked(shell, chip_id, forward, window, cx);
                });
            });
            let value_el = dialog::value_chip(
                row.values[row.current].clone(),
                format!("settings-value-{row_ix}"),
                chip_fg,
                chip_bg,
                chip_radius,
                chip_states,
                Some(on_step),
            );

            let entity_for_row = entity.clone();
            let id = row.id;
            let row_el = row_el
                .child(label)
                .child(value_el)
                .debug_selector(move || format!("settings-row-{row_ix}"))
                .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    entity_for_row.update(cx, |shell, cx| {
                        on_row_clicked(shell, id, event.click_count, window, cx);
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
        list.into_any_element()
    };

    // Teach only keys active in the current mode. Normal includes bare-letter and space
    // stepping; Filter reserves them for query text and teaches Tab stepping. Hints
    // carry their footer category. Both stepping groups expose the
    // `settings-hint-change` selector on their first chip.
    let hints: Vec<Hint> = if state.choosing() {
        // Choice entry owns its own narrow/move/complete/pick/cancel vocabulary.
        vec![
            Hint::prose(HintRow::Move, "type to narrow"),
            Hint::new(HintRow::Move, &["up", "down"], "move"),
            Hint::new(HintRow::Go, &["tab"], "complete"),
            Hint::new(HintRow::Go, &["enter"], "choose"),
            Hint::new(HintRow::Go, &["escape"], "cancel"),
        ]
    } else {
        match state.mode {
            DialogMode::Normal => vec![
                Hint::new(HintRow::Move, &["j", "k"], "move"),
                Hint::new(HintRow::Move, &["ctrl+d", "ctrl+u"], "±5"),
                Hint::new(HintRow::Move, &["ctrl+f", "ctrl+b"], "±10"),
                Hint::new(
                    HintRow::Edit,
                    &["space", "shift+space", "tab", "h", "l"],
                    "change",
                )
                .selector("settings-hint-change"),
                Hint::new(HintRow::Edit, &["i", "enter"], "choose")
                    .selector("settings-hint-choose"),
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
                Hint::new(HintRow::Edit, &["tab", "shift+tab"], "change")
                    .selector("settings-hint-change"),
                // Both keys return to Normal: keep the edited query or restore its
                // entry text.
                Hint::new(HintRow::Go, &["enter"], "keep the filter"),
                Hint::new(HintRow::Go, &["escape"], "discard the filter"),
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
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(hint_line),
        )
        // Read-only information: modifier alias and configuration file location.
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

    // Freeze the query while Normal owns commands; clicking it enters filtering. There
    // is no capture state here, so the empty frozen row can always teach `/`. An open
    // choice instead labels the same shared Input with the setting name and supplies
    // its own query.
    let top_row = if let Some(entry) = state.choice.as_ref() {
        let title = rows
            .iter()
            .find(|r| r.id == entry.id)
            .map(|r| r.title)
            .unwrap_or("");
        dialog::name_row(&shell.dialog_input, &format!("{title} · choose"), cx)
    } else {
        let frozen_query = (state.mode == DialogMode::Normal).then_some(dialog::FrozenFilter {
            query: state.query.as_str(),
            slash_filters: true,
            entity: entity.clone(),
        });
        dialog::filter_row(&shell.dialog_input, frozen_query, cx)
    };

    v_flex()
        .gap_2()
        .child(top_row)
        .child(list)
        .child(footer)
        .into_any_element()
}

/// Complete the clicked ranked choice as Tab would, then synchronize the input.
/// Completion leaves the choice open; Enter picks its value.
fn on_choice_row_clicked(
    shell: &mut ShellView,
    row: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(entry) = shell.settings.as_mut().and_then(|s| s.choice.as_mut())
        && entry.list.set_ranked_highlighted(row)
    {
        entry.list.complete();
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
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
            None,
            &names(&["demo_kdb", "demo_rest"]),
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
                (
                    SettingId::DefaultSource,
                    "Default series source",
                    "Timeseries"
                ),
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
        assert_eq!(rows[5].values, names(&["(none)", "demo_kdb", "demo_rest"]));
        assert_eq!(rows[5].current, 0, "no default source is `(none)`");
    }

    #[test]
    fn the_default_source_row_offers_none_then_every_fetch_source() {
        let row = |default: Option<&str>, sources: &[&str]| {
            derive_rows(
                &names(&["A"]),
                "A",
                FontSize::Medium,
                FindStyle::Vim,
                LineNumbers::Off,
                AddDirection::Auto,
                default,
                &names(sources),
            )[5]
            .clone()
        };
        let r = row(Some("demo_rest"), &["demo_kdb", "demo_rest"]);
        assert_eq!(r.id, SettingId::DefaultSource);
        assert_eq!(r.current, 2, "the configured source, offset past (none)");
        let r = row(Some("gone"), &["demo_kdb"]);
        assert_eq!(
            r.current, 0,
            "a default naming no configured source falls back to (none), never out of range"
        );
        let r = row(None, &[]);
        assert_eq!(r.values, names(&["(none)"]));
        assert_eq!(
            r.current, 0,
            "with no fetch sources the row is (none) alone — stepping it is a no-op cycle"
        );
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
            None,
            &[],
        );
        assert_eq!(rows[1].current, 0, "FontSize::Small is ALL[0]");
        assert_eq!(rows[2].current, 1, "FindStyle::Fzf is ALL[1]");
        assert_eq!(rows[3].current, 2, "LineNumbers::Relative is ALL[2]");
    }

    #[test]
    fn there_is_no_dark_mode_row() {
        // Theme names already include their appearance mode.
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
            None,
            &[],
        );
        assert_eq!(
            rows[0].current, 0,
            "deterministic fallback, never an out-of-range index"
        );
    }

    #[test]
    fn the_add_direction_row_is_the_fifth_in_the_tiling_category() {
        let rows = derive_rows(
            &names(&["A"]),
            "A",
            FontSize::Small,
            FindStyle::Vim,
            LineNumbers::Off,
            AddDirection::Vertical,
            None,
            &[],
        );
        assert_eq!(rows.len(), 6);
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
        // Resolve clicked SettingId against the filtered list, not the full row order.
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

    // Mode and key routing (`crate::dialogmode`).

    fn ks(key: &str, mods: Modifiers) -> Keystroke {
        Keystroke {
            mods,
            key: key.to_string(),
        }
    }
    fn bare(key: &str) -> Keystroke {
        ks(key, Modifiers::NONE)
    }
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };

    /// A fresh session is Normal, so opening synchronization focuses the shell.
    #[test]
    fn a_fresh_session_opens_in_normal_mode() {
        assert_eq!(SettingsState::new().mode, DialogMode::Normal);
    }

    #[test]
    fn setting_a_query_never_moves_the_mode() {
        let mut state = SettingsState::new();
        state.mode = DialogMode::Filter;
        state.set_query("f".to_string());
        assert_eq!(state.mode, DialogMode::Filter);
    }

    /// Normal mode: the shared vocabulary's step keys step, `/` enters
    /// filter mode, `j`/`k` move, and an unclaimed key is dropped rather
    /// than passed to the shell underneath.
    #[test]
    fn normal_mode_routes_the_shared_vocabulary() {
        use KeyAction::*;
        let n = DialogMode::Normal;
        assert_eq!(
            route(n, true, false, &bare("space")),
            Step(StepDirection::Right)
        );
        assert_eq!(
            route(n, true, false, &ks("space", SHIFT)),
            Step(StepDirection::Left)
        );
        assert_eq!(route(n, true, false, &bare("/")), EnterFilter);
        assert_eq!(route(n, true, false, &bare("j")), Nav(NavCommand::Move(1)));
        assert_eq!(route(n, true, false, &bare("k")), Nav(NavCommand::Move(-1)));
        assert_eq!(route(n, true, false, &bare("g")), Nav(NavCommand::Top));
        assert_eq!(
            route(n, true, false, &ks("g", SHIFT)),
            Nav(NavCommand::Bottom)
        );
        assert_eq!(
            route(n, true, false, &bare("s")),
            Drop,
            "a stray letter does nothing"
        );
        assert_eq!(
            route(n, true, false, &bare("i")),
            OpenChoice,
            "spec 2026-09-19 §3.3/§7: i opens the row's typeahead"
        );
        assert_eq!(
            route(n, true, false, &bare("enter")),
            OpenChoice,
            "and so does a bare enter, beside it"
        );
        assert_eq!(
            route(n, true, false, &ks("v", Modifiers::CTRL)),
            Drop,
            "a chord normal mode does not name is dropped like any other \
             unclaimed key (the modal branch would stop it regardless)"
        );
    }

    /// Normal `i`/Enter opens a choice; Filter Enter only keeps the query and returns
    /// to Normal. An open choice delegates all keys to its own routing table.
    #[test]
    fn i_and_enter_open_a_choice_in_normal_mode_and_choice_keys_route_while_open() {
        let bare = |k: &str| Keystroke {
            mods: Modifiers::NONE,
            key: k.to_string(),
        };
        assert_eq!(
            route(DialogMode::Normal, true, false, &bare("i")),
            KeyAction::OpenChoice
        );
        assert_eq!(
            route(DialogMode::Normal, true, false, &bare("enter")),
            KeyAction::OpenChoice
        );
        assert_eq!(
            route(DialogMode::Filter, true, false, &bare("enter")),
            KeyAction::LeaveFilter(FilterExit::Keep),
            "filter mode's enter keeps the query and hands the keys back"
        );
        assert_eq!(
            route(DialogMode::Filter, false, true, &bare("enter")),
            KeyAction::Choice(crate::choice::ChoiceKey::Pick)
        );
        assert_eq!(
            route(DialogMode::Filter, false, true, &bare("escape")),
            KeyAction::Choice(crate::choice::ChoiceKey::Cancel)
        );
        assert_eq!(
            route(DialogMode::Filter, false, true, &bare("tab")),
            KeyAction::Choice(crate::choice::ChoiceKey::Complete)
        );
        assert_eq!(
            route(DialogMode::Filter, false, true, &bare("x")),
            KeyAction::PassThrough,
            "a letter types into the field"
        );
    }

    /// Filter treats printable keys, including space and `/`, as input text. Typing a
    /// query must not step a setting.
    #[test]
    fn filter_mode_passes_printable_keys_to_the_input() {
        use KeyAction::*;
        let f = DialogMode::Filter;
        assert_eq!(route(f, false, false, &bare("space")), PassThrough);
        assert_eq!(route(f, false, false, &ks("space", SHIFT)), PassThrough);
        assert_eq!(route(f, false, false, &bare("/")), PassThrough);
        assert_eq!(
            route(f, false, false, &bare("j")),
            PassThrough,
            "j types, never moves"
        );
        assert_eq!(
            route(f, false, false, &bare("enter")),
            LeaveFilter(FilterExit::Keep),
            "enter is still claimed — see handle_key — and since \
             2026-09-23 it leaves filter mode keeping the query"
        );
    }

    /// Normal `h`/`l` step through the shared command table. In Filter they are input
    /// text and must not change a setting.
    #[test]
    fn h_and_l_step_in_normal_mode_and_type_in_filter_mode() {
        use KeyAction::*;
        let n = DialogMode::Normal;
        assert_eq!(
            route(n, true, false, &bare("l")),
            Step(StepDirection::Right)
        );
        assert_eq!(route(n, true, false, &bare("h")), Step(StepDirection::Left));
        let f = DialogMode::Filter;
        assert_eq!(route(f, false, false, &bare("l")), PassThrough, "l types");
        assert_eq!(route(f, false, false, &bare("h")), PassThrough, "h types");
    }

    /// The keys both modes share: `tab`/`shift+tab` step and the
    /// filter-safe motions move, whichever mode is current.
    #[test]
    fn tab_and_the_arrows_work_in_both_modes() {
        use KeyAction::*;
        for mode in [DialogMode::Normal, DialogMode::Filter] {
            assert_eq!(
                route(mode, true, false, &bare("tab")),
                Step(StepDirection::Right)
            );
            assert_eq!(
                route(mode, true, false, &ks("tab", SHIFT)),
                Step(StepDirection::Left)
            );
            assert_eq!(
                route(mode, true, false, &bare("down")),
                Nav(NavCommand::Move(1))
            );
            assert_eq!(
                route(mode, true, false, &ks("d", Modifiers::CTRL)),
                Nav(NavCommand::Move(5))
            );
        }
    }

    /// Escape leaves Filter by restoring its entry query, then clears any applied
    /// Normal-mode query, then lets the shell close the dialog. Modifiers are ignored.
    #[test]
    fn escape_walks_the_ladder() {
        use KeyAction::*;
        assert_eq!(
            route(DialogMode::Filter, false, false, &bare("escape")),
            LeaveFilter(FilterExit::Revert)
        );
        assert_eq!(
            route(DialogMode::Filter, true, false, &bare("escape")),
            LeaveFilter(FilterExit::Revert)
        );
        assert_eq!(
            route(DialogMode::Normal, false, false, &bare("escape")),
            ClearQuery
        );
        assert_eq!(
            route(DialogMode::Normal, true, false, &bare("escape")),
            PassThrough
        );
        assert_eq!(
            route(DialogMode::Normal, true, false, &ks("escape", SHIFT)),
            PassThrough,
            "a modified escape walks the same ladder as a bare one"
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
