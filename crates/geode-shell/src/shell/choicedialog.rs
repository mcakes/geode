//! A filter-only choice modal for grouping slots, saved scopes, tile kinds,
//! columns, and log levels.
//! [`ChoiceList`] owns ranking, highlight, Tab completion, and navigation.
//! Enter or a row click commits the selected option.
//!
//! Grouping lists the view default followed by configured slots 1–9. With
//! an empty query, digits activate a configured slot and 0 restores the view
//! default. Tile choices follow roster order and omit the placeholder; a
//! commit fills the focused placeholder or splits the focused real tile.
//!
//! Scope (`frame::scope`) lists the target frame's saved scopes as they
//! are at open — not the startup action registry — by name, opening on
//! the one equal to the current scope. A pick loads through
//! `ShellView::load_saved_scope`, the `scope::<name>` actions' own
//! undoable path; a name removed under the open picker posts a notice.
//! With no saved scope the list is replaced by a how-to-save hint.
//!
//! Log level uses two steps in the same modal: select a target, then a level.
//! Escape or the title row's Back button returns from the level step to targets.
//! Escape elsewhere closes; no other step paints a Back button.
//! Each open starts fresh; stage transitions clear and refocus the Input.
//!
//! `tile::open_with` uses the same tile rows, filtered to kinds accepting a
//! column of the focused tile's dimension context, titled `Open {subject}
//! in…` (the first context value of an accepted column); a pick always
//! splits.
//!
//! `config::view_column` / `config::schema_column` list the focused tile's
//! presented columns (Schema without derived ones), the cursor's column
//! highlighted; a pick opens that dialog on the column's Column stage.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Focusable as _, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use geode_core::context::DimensionContext;
use geode_core::groupings::GroupingSlots;
use geode_core::log::{Level, LogLevels, TARGETS};
use geode_core::query::DistinctOutcome;
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;
use geode_core::tile_columns::{TileColumn, TileColumns};

use crate::choice::{self, ChoiceKey, ChoiceList};
use crate::defaults::{AddPlacement, capitalize};
use crate::keymap::{Keystroke, Modifiers};
use crate::module::placeholder::PLACEHOLDER_KIND;

use super::ShellView;
use super::dialog;
use super::objectdialog::{self, Domain};
use super::picker::{Hint, hint_row};
use super::scale;

// ---------------------------------------------------------------------
// Pure core — no gpui.
// ---------------------------------------------------------------------

/// The "return to the views' own grouping" row's text — the same words
/// the toolbar readout shows when no slot is active
/// (`scopebar::build_model`'s `slot_label`).
pub const VIEW_DEFAULT: &str = "view default";

/// What the rows stand for and what a pick does.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// The slot each DECLARED option (an index into `list.options()`)
    /// activates: `None` for the view default.
    Grouping { slots: Vec<Option<u8>> },
    /// The module kind each declared option adds (`add_tile`).
    TileKind { kinds: Vec<String> },
    /// `tile::open_with`: the kinds accepting `context`, which was captured
    /// from the focused tile when the dialog opened (moving that tile's
    /// cursor afterwards does not change what a pick opens). `subject` is
    /// the value the dialog is titled by: the first context value of a
    /// column one of `kinds` accepts.
    TileKindWith {
        kinds: Vec<String>,
        context: DimensionContext,
        subject: Option<String>,
    },
    /// The focused tile's columns for `config::view_column` (Views) or
    /// `config::schema_column` (Schema), captured at open: `names[i]` is the
    /// column declared option `i` stands for.
    Column {
        domain: Domain,
        view: String,
        names: Vec<String>,
    },
    /// `frame::scope`: the saved scope names, read from the target frame's
    /// live saved scopes at open. `names[i]` is the scope declared option
    /// `i` loads (the row text is the name itself).
    Scope { names: Vec<String> },
    /// Log-level stage: `None` shows targets; `Some(target)` shows levels.
    LogLevel {
        targets: Vec<String>,
        chosen: Option<String>,
    },
    /// `ActionCx::choose_value`: `column`'s distinct live values, minus
    /// `exclude`, for the roster action at `action`, which ran on
    /// `context`. `values: None` is loading, waiting on the `ACTION_KEY`
    /// request tagged `tag`; `Some` holds the rows, in delivered order.
    /// `title` is the modal's; `empty` is the notice when no row is left;
    /// `window` is where a reply that closes the dialog closes it (a
    /// delivery arrives without one).
    ActionValue {
        action: usize,
        context: DimensionContext,
        column: String,
        exclude: Option<String>,
        tag: u64,
        values: Option<Vec<String>>,
        title: SharedString,
        empty: &'static str,
        window: gpui::AnyWindowHandle,
    },
}

/// The level rows, in severity order, as `[log]` spells them.
pub const LEVEL_WORDS: [(&str, Level); 5] = [
    ("error", Level::ERROR),
    ("warn", Level::WARN),
    ("info", Level::INFO),
    ("debug", Level::DEBUG),
    ("trace", Level::TRACE),
];

fn level_word(level: Level) -> &'static str {
    LEVEL_WORDS
        .iter()
        .find(|(_, l)| *l == level)
        .map(|(w, _)| *w)
        .unwrap_or("info")
}

/// A target's effective level: its own entry, else the default.
fn effective_level(levels: &LogLevels, target: &str) -> Level {
    levels
        .targets
        .iter()
        .find(|(t, _)| t == target)
        .map(|(_, l)| *l)
        .unwrap_or(levels.default)
}

/// Persistent state for one open choice-dialog session.
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceDialogState {
    /// The ranked rows, spelled as the surface a pick lands on spells
    /// them: a grouping row is `"{n} · {label}"`, the toolbar readout's
    /// own text, so the row a trader picks reads exactly as the bar will
    /// afterwards; a tile row is the palette's `<Kind>` title.
    pub list: ChoiceList,
    pub target: Target,
}

impl ChoiceDialogState {
    /// The grouping rows for `slots`, the highlight placed on `active`
    /// (the frame's current slot — `None` lights the view-default row) so
    /// `enter` on an untouched picker changes nothing, like every other
    /// choice surface.
    pub fn grouping(slots: &GroupingSlots, active: Option<u8>) -> Self {
        let (options, targets) = grouping_rows(slots);
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        let current = targets.iter().position(|t| *t == active);
        let text = current.map(|ix| list.options()[ix].clone());
        list.place(text.as_deref());
        Self {
            list,
            target: Target::Grouping { slots: targets },
        }
    }

    /// The tile rows for the roster's `kinds`, in roster order, the
    /// placeholder left out (it is what a pick REPLACES, never something
    /// to add), the highlight on the first.
    pub fn tile_kinds<'a>(kinds: impl IntoIterator<Item = &'a str>) -> Self {
        let kinds: Vec<String> = kinds
            .into_iter()
            .filter(|k| *k != PLACEHOLDER_KIND)
            .map(str::to_string)
            .collect();
        let options = kinds.iter().map(|k| capitalize(k)).collect();
        Self {
            list: ChoiceList::new(options, choice::DEFAULT_CAP),
            target: Target::TileKind { kinds },
        }
    }

    /// The rows for `tile::open_with`: `kinds` (already filtered to those
    /// accepting `context`), in roster order, the highlight on the first,
    /// titled by `subject`.
    pub fn tile_kinds_with<'a>(
        kinds: impl IntoIterator<Item = &'a str>,
        context: DimensionContext,
        subject: Option<String>,
    ) -> Self {
        let Self { list, target } = Self::tile_kinds(kinds);
        let Target::TileKind { kinds } = target else {
            unreachable!("tile_kinds builds a TileKind target")
        };
        Self {
            list,
            target: Target::TileKindWith {
                kinds,
                context,
                subject,
            },
        }
    }

    /// The column rows for `tile` (derived columns left out for Schema: no
    /// dataset declares them), the highlight on the cursor's column when it
    /// is listed, else the first. `None` when no row is left.
    pub fn columns(domain: Domain, tile: &TileColumns) -> Option<Self> {
        let mut options = Vec::new();
        let mut names = Vec::new();
        let mut active = None;
        for (ix, c) in tile.columns.iter().enumerate() {
            if domain == Domain::Schema && c.derived {
                continue;
            }
            let text = column_row_text(c);
            if tile.active == Some(ix) {
                active = Some(text.clone());
            }
            options.push(text);
            names.push(c.name.clone());
        }
        if names.is_empty() {
            return None;
        }
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        list.place(active.as_deref());
        Some(Self {
            list,
            target: Target::Column {
                domain,
                view: tile.view.clone(),
                names,
            },
        })
    }

    /// One row per saved scope, spelled as its name, in the saved set's
    /// order (`SavedScopes` is a name-ordered map). The highlight opens on
    /// the first saved scope EQUAL to `current` (the frame's scope), so
    /// `enter` on an untouched picker changes nothing; else on the first
    /// row. An empty set is an empty list: nothing is lit and `enter`
    /// picks nothing.
    pub fn scopes(saved: &SavedScopes, current: &Scope) -> Self {
        let names: Vec<String> = saved.keys().cloned().collect();
        let active = saved
            .iter()
            .find(|(_, scope)| *scope == current)
            .map(|(name, _)| name.as_str());
        let mut list = ChoiceList::new(names.clone(), choice::DEFAULT_CAP);
        list.place(active);
        Self {
            list,
            target: Target::Scope { names },
        }
    }

    /// The modal's title: the chrome's fixed words, or `Open {subject}…`
    /// for a context launch with a subject.
    pub fn title(&self) -> SharedString {
        match &self.target {
            Target::TileKindWith { subject, .. } => match subject {
                Some(u) => format!("Open {u} in\u{2026}").into(),
                None => chrome(&self.target).0.into(),
            },
            Target::Column { domain, view, .. } => match domain {
                Domain::Schema => format!("Edit column in schema \u{b7} {view}").into(),
                _ => format!("Edit column in view \u{b7} {view}").into(),
            },
            Target::ActionValue { title, .. } => title.clone(),
            Target::Grouping { .. }
            | Target::TileKind { .. }
            | Target::Scope { .. }
            | Target::LogLevel { .. } => chrome(&self.target).0.into(),
        }
    }

    /// Step 1 of `Set log level…`: one row per `geode::` target suffix,
    /// `"{target} · {level}"`, the highlight on the first.
    pub fn log_targets(levels: &LogLevels) -> Self {
        let targets: Vec<String> = TARGETS
            .iter()
            .map(|t| t.strip_prefix("geode::").unwrap_or(t).to_string())
            .collect();
        let options = targets
            .iter()
            .map(|t| format!("{t} · {}", level_word(effective_level(levels, t))))
            .collect();
        Self {
            list: ChoiceList::new(options, choice::DEFAULT_CAP),
            target: Target::LogLevel {
                targets,
                chosen: None,
            },
        }
    }

    /// Step 2: the five levels, with the current effective level highlighted.
    pub fn log_levels(target: String, current: Level) -> Self {
        let options: Vec<String> = LEVEL_WORDS.iter().map(|(w, _)| (*w).to_string()).collect();
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        list.place(Some(level_word(current)));
        Self {
            list,
            target: Target::LogLevel {
                targets: Vec::new(),
                chosen: Some(target),
            },
        }
    }

    /// The pick the highlighted row stands for, or `None` with nothing
    /// highlighted (every row filtered out).
    pub fn highlighted_pick(&self) -> Option<Pick> {
        self.list.pick().map(|ix| self.pick_at(ix))
    }

    /// The pick a RANKED row (a click's index, `dialog::choice_rows`'s
    /// own positions) stands for.
    pub fn pick_at_ranked(&self, ranked: usize) -> Option<Pick> {
        self.list.ranked().get(ranked).map(|r| self.pick_at(r.row))
    }

    fn pick_at(&self, declared: usize) -> Pick {
        match &self.target {
            Target::Grouping { slots } => Pick::Slot(slots[declared]),
            Target::TileKind { kinds } => Pick::Kind(kinds[declared].clone()),
            Target::TileKindWith { kinds, context, .. } => {
                Pick::KindWith(kinds[declared].clone(), context.clone())
            }
            Target::Column {
                domain,
                view,
                names,
            } => Pick::Column {
                domain: *domain,
                view: view.clone(),
                column: names[declared].clone(),
            },
            Target::Scope { names } => Pick::Scope(names[declared].clone()),
            Target::LogLevel { targets, chosen } => match chosen {
                None => Pick::LogTarget(targets[declared].clone()),
                Some(target) => Pick::LogLevel(target.clone(), LEVEL_WORDS[declared].1),
            },
            Target::ActionValue {
                action, context, ..
            } => Pick::ActionValue {
                action: *action,
                context: context.clone(),
                value: self.list.options()[declared].clone(),
            },
        }
    }

    /// Map a grouping digit to its configured slot, or 0 to the view default.
    /// The key handler gates this on an empty query. Return `None` for an empty
    /// slot, a non-digit, or another target.
    pub fn jump(&self, key: &str) -> Option<Option<u8>> {
        let Target::Grouping { slots } = &self.target else {
            return None;
        };
        let digit = key.parse::<u8>().ok().filter(|d| *d <= 9)?;
        if digit == 0 {
            return Some(None);
        }
        slots.iter().find(|s| **s == Some(digit)).copied()
    }

    /// The grouping-target convenience the tests read.
    pub fn highlighted_slot(&self) -> Option<Option<u8>> {
        match self.highlighted_pick()? {
            Pick::Slot(slot) => Some(slot),
            Pick::Kind(_)
            | Pick::KindWith(..)
            | Pick::Column { .. }
            | Pick::Scope(_)
            | Pick::LogTarget(_)
            | Pick::LogLevel(..)
            | Pick::ActionValue { .. } => None,
        }
    }
}

/// What one row commits. Owned (a `String` kind), not borrowed from the
/// dialog state: a pick is a one-off event whose commit drops that state.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// `FrameViewMut::set_active_slot`; `None` is the view default.
    Slot(Option<u8>),
    /// `ShellView::add_tile` of this kind.
    Kind(String),
    /// `ShellView::add_tile` of this kind, with the factory's
    /// `launch_state` of this context.
    KindWith(String, DimensionContext),
    /// `objectdialog::render::open_column` for this column of `view`.
    Column {
        domain: Domain,
        view: String,
        column: String,
    },
    /// `ShellView::load_saved_scope` of this name.
    Scope(String),
    /// Step 1 of `Set log level…`: replace the rows with the levels.
    LogTarget(String),
    /// Step 2: `Diagnostics::request_level`.
    LogLevel(String, Level),
    /// The roster action at `action`'s `DimensionAction::chosen` with
    /// `value`, on `context`.
    ActionValue {
        action: usize,
        context: DimensionContext,
        value: String,
    },
}

/// The grouping option texts and their slots, in row order: the view
/// default, then slots 1–9 that are filled.
pub fn grouping_rows(slots: &GroupingSlots) -> (Vec<String>, Vec<Option<u8>>) {
    let mut options = vec![VIEW_DEFAULT.to_string()];
    let mut targets = vec![None];
    for n in 1..=9u8 {
        if let Some(label) = slots.label(n) {
            options.push(format!("{n} · {label}"));
            targets.push(Some(n));
        }
    }
    (options, targets)
}

/// A column row: the painted label, then the column name when they differ,
/// so typing either filters to it and two equal labels stay distinct.
fn column_row_text(c: &TileColumn) -> String {
    if c.label == c.name {
        c.name.clone()
    } else {
        format!("{} · {}", c.label, c.name)
    }
}

// ---------------------------------------------------------------------
// gpui: the modal.
// ---------------------------------------------------------------------

/// Dialog width on the design scale — the dimension picker's.
const WIDTH: f32 = 480.0;

const GROUPING_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("activate ·"),
    Hint::Key("1"),
    Hint::Text("–"),
    Hint::Key("9"),
    Hint::Text("slot ·"),
    Hint::Key("0"),
    Hint::Text("view default ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

const TILE_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("add ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

const COLUMN_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("edit ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

const SCOPE_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("load ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

/// The scope picker's footer with no saved scope: no row to move over and
/// nothing for Enter to load, so only the way out is offered.
const SCOPE_EMPTY_HINTS: &[Hint] = &[Hint::Key("escape"), Hint::Text("close")];

/// An action's value choice while its values load: no row to move over and
/// nothing for Enter to choose, so only the way out is offered.
const ACTION_LOADING_HINTS: &[Hint] = &[Hint::Key("escape"), Hint::Text("close")];

const ACTION_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("choose ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

const LOG_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("choose ·"),
    Hint::Key("escape"),
    Hint::Text("back / close"),
];

/// The per-target chrome: the modal's title, the selector prefix
/// (`{prefix}-choice-list`, `{prefix}-choice-{text}`, `{prefix}-hints`)
/// and the footer.
fn chrome(target: &Target) -> (&'static str, &'static str, &'static str, &'static [Hint]) {
    match target {
        Target::Grouping { .. } => ("Grouping", "grouping", "grouping-hints", GROUPING_HINTS),
        Target::TileKind { .. } => ("Add a tile", "tile", "tile-hints", TILE_HINTS),
        // This fallback title shows only if the target is built with no
        // subject; `tile::open_with` never builds it that way (a listed kind
        // accepts a present column). `title()` supplies `Open {subject} in…`
        // instead.
        Target::TileKindWith { .. } => ("Open in\u{2026}", "tile", "tile-hints", TILE_HINTS),
        // Fallback only: `title()` names the view and the dialog.
        Target::Column { .. } => ("Edit column", "column", "column-hints", COLUMN_HINTS),
        Target::Scope { .. } => ("Scope", "scope", "scope-hints", SCOPE_HINTS),
        Target::LogLevel { .. } => ("Log level", "loglevel", "loglevel-hints", LOG_HINTS),
        // Fallback only: `title()` is the action's own.
        Target::ActionValue { .. } => ("Choose a value", "action", "action-hints", ACTION_HINTS),
    }
}

/// Open on the frame's grouping slots — `frame::grouping` and the
/// toolbar readout's click.
pub fn open_grouping(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let state = {
        let frame = view.target_frame().read(cx);
        ChoiceDialogState::grouping(frame.slots(), frame.active_slot())
    };
    open(view, state, window, cx);
}

/// Open on the target frame's saved scopes — `frame::scope` and the
/// toolbar's load glyph. The rows are the frame's LIVE saved scopes, read
/// now, so a scope saved or reloaded since startup is listed (the
/// palette's `scope::<name>` rows are fixed at startup).
pub fn open_scopes(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let state = {
        let frame = view.target_frame().read(cx);
        ChoiceDialogState::scopes(frame.saved_scopes(), frame.scope())
    };
    open(view, state, window, cx);
}

/// Open on the roster's kinds — `tile::add` and a placeholder's
/// double-click. The pick lands wherever `add_tile` puts a tile for the
/// focused tile at COMMIT time: a focused placeholder is filled in place,
/// a real tile is split in the `add` setting's direction.
pub fn open_tile_kinds(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let state = ChoiceDialogState::tile_kinds(view.services.roster.kinds());
    open(view, state, window, cx);
}

/// Open on the roster kinds accepting `context` — `tile::open_with` with a
/// non-empty context — titled by `subject`. The caller has checked at least
/// one kind accepts it.
pub fn open_tile_kinds_with(
    view: &mut ShellView,
    kinds: Vec<&'static str>,
    context: DimensionContext,
    subject: Option<String>,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let state = ChoiceDialogState::tile_kinds_with(kinds, context, subject);
    open(view, state, window, cx);
}

/// Status notice: the focused tile presents no configured view's columns.
pub(crate) const NO_TILE_COLUMNS: &str = "this tile has no dataset columns";
/// Status notice: every column of the tile's view is derived, so Schema has
/// nothing to open.
pub(crate) const NO_SCHEMA_COLUMNS: &str = "no schema columns in this tile's view";

/// Open the column list for `domain` (Views or Schema) over the focused
/// tile's columns — `config::view_column` / `config::schema_column`. The
/// target dialog's stack refusal runs first, so a list is never offered for
/// a dialog that could not open on its pick.
pub fn open_columns(
    view: &mut ShellView,
    domain: Domain,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let tile = view
        .services
        .workspaces
        .active()
        .focused_tile()
        .and_then(|t| view.occupants.get(&t))
        .and_then(|o| o.content.tile_columns(cx));
    let Some(mut tile) = tile else {
        view.notice = Some(NO_TILE_COLUMNS.into());
        cx.notify();
        return;
    };
    // Schema offers only what some dataset of the view declares, against the
    // current config; the tile's `derived` flag misses derived dimensions.
    if domain == Domain::Schema
        && let Some(declared) = objectdialog::render::schema_declared(
            view,
            &tile.view,
            tile.columns.iter().map(|c| c.name.as_str()),
        )
    {
        let active = tile.active_column().map(|c| c.name.clone());
        let mut keep = declared.into_iter();
        tile.columns.retain(|_| keep.next().unwrap_or(false));
        tile.active = active.and_then(|name| tile.columns.iter().position(|c| c.name == name));
    }
    if !dialog::can_open_object(view, domain) {
        cx.notify();
        return;
    }
    let Some(state) = ChoiceDialogState::columns(domain, &tile) else {
        view.notice = Some(NO_SCHEMA_COLUMNS.into());
        cx.notify();
        return;
    };
    open(view, state, window, cx);
}

/// Open `Set log level…` on the target step (`log::level`, palette-only).
pub fn open_log_level(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let state = ChoiceDialogState::log_targets(&view.diagnostics.read(cx).levels);
    open(view, state, window, cx);
}

/// Open the loading value choice for `ActionCx::choose_value`: `column`'s
/// values for the roster action at `action`, minus `exclude`, titled
/// `title`, waiting on the `ACTION_KEY` request tagged `tag`. False when the
/// stack refused it (a choice list already open), so the caller asks for
/// nothing.
#[allow(clippy::too_many_arguments)]
// Every argument is a field of the target the dialog waits on.
pub(crate) fn open_action_values(
    view: &mut ShellView,
    action: usize,
    context: DimensionContext,
    column: String,
    title: SharedString,
    exclude: Option<String>,
    empty: &'static str,
    tag: u64,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if !dialog::can_open(view, dialog::DialogKind::Choice) {
        return false;
    }
    let state = ChoiceDialogState {
        list: ChoiceList::new(Vec::new(), choice::DEFAULT_CAP),
        target: Target::ActionValue {
            action,
            context,
            column,
            exclude,
            tag,
            values: None,
            title,
            empty,
            window: window.window_handle(),
        },
    };
    open(view, state, window, cx);
    true
}

/// Status notice: an action's value fetch failed.
pub(crate) fn action_values_failed(column: &str, reason: &str) -> String {
    format!("could not load {column} values: {reason}")
}

/// An `ACTION_KEY` reply. Applied only while the open choice dialog waits
/// on this tag; anything else is dropped. The values minus the excluded
/// one, in delivered order, replace the loading row, filtered by the query
/// typed while loading: the shared field's text on top, the stack entry's
/// saved text when another dialog covers the choice. None left, or a failed
/// fetch, sets the target's notice and removes the dialog: on top, the
/// close is deferred to the target's window (a delivery arrives without
/// one); covered, its entry is dropped from the stack at once, since focus
/// belongs to the cover.
pub(crate) fn deliver_action_values(
    view: &mut ShellView,
    outcome: DistinctOutcome,
    cx: &mut Context<ShellView>,
) {
    // The choice's stack entry when another dialog covers it.
    let covered_at = view
        .modals
        .iter()
        .position(|m| m.kind == dialog::DialogKind::Choice)
        .filter(|&at| at + 1 < view.modals.len());
    let live = match covered_at {
        Some(at) => view.modals[at]
            .saved_input
            .as_ref()
            .map(|saved| saved.text.clone())
            .unwrap_or_default(),
        None => view.dialog_input.read(cx).value().to_string(),
    };
    let Some(state) = view.choice_dialog.as_mut() else {
        return;
    };
    let Target::ActionValue {
        column,
        exclude,
        tag,
        values,
        empty,
        window,
        ..
    } = &mut state.target
    else {
        return;
    };
    if outcome.tag != *tag || outcome.column != *column {
        return;
    }
    let notice = match outcome.values {
        Ok(rows) => {
            let kept: Vec<String> = rows
                .into_iter()
                .map(|(value, _)| value)
                .filter(|value| exclude.as_ref() != Some(value))
                .collect();
            if kept.is_empty() {
                (*empty).to_string()
            } else {
                state.list = ChoiceList::new(kept.clone(), choice::DEFAULT_CAP);
                // A query typed while loading filters the rows it waited for.
                state.list.set_query(&live);
                *values = Some(kept);
                view.choice_dialog_scroll
                    .scroll_to_item(state.list.ranked_highlighted());
                cx.notify();
                return;
            }
        }
        Err(reason) => action_values_failed(column, &reason),
    };
    let (window, tag) = (*window, *tag);
    view.notice = Some(notice.into());
    if let Some(at) = covered_at {
        // Covered: focus belongs to the cover, so no window is needed. The
        // entries beneath keep their saved input.
        view.modals.remove(at);
        view.choice_dialog = None;
        cx.notify();
        return;
    }
    let entity = cx.entity();
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, cx| {
            entity.update(cx, |shell, cx| {
                // Still this dialog, on top: nothing closed or covered it
                // since the reply.
                let waiting = shell.top_kind() == Some(dialog::DialogKind::Choice)
                    && matches!(
                        shell.choice_dialog.as_ref().map(|s| &s.target),
                        Some(Target::ActionValue { tag: t, .. }) if *t == tag
                    );
                if waiting {
                    shell.close_modal(window, cx);
                }
            })
        });
    });
    cx.notify();
}

fn open(
    view: &mut ShellView,
    state: ChoiceDialogState,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if !dialog::can_open(view, dialog::DialogKind::Choice) {
        return;
    }
    let title = state.title();
    view.choice_dialog_scroll
        .scroll_to_item(state.list.ranked_highlighted());
    view.choice_dialog = Some(state);
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        dialog::DialogKind::Choice,
        title,
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
    dialog::set_back(view, on_level_step, |shell, window, cx| {
        back_to_targets(shell, window, cx);
    });
}

/// Whether the log-level dialog is on its level step, the only step any choice dialog
/// can go back from.
fn on_level_step(shell: &ShellView) -> bool {
    matches!(
        shell.choice_dialog.as_ref().map(|s| &s.target),
        Some(Target::LogLevel {
            chosen: Some(_),
            ..
        })
    )
}

/// Return the level step to the target list, shared by Escape and the Back button.
/// Rebuild the targets from current levels, clear the query, and refocus the Input.
/// Returns `false`, changing nothing, off the level step.
fn back_to_targets(
    shell: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if !on_level_step(shell) {
        return false;
    }
    let state = ChoiceDialogState::log_targets(&shell.diagnostics.read(cx).levels);
    shell
        .choice_dialog_scroll
        .scroll_to_item(state.list.ranked_highlighted());
    shell.choice_dialog = Some(state);
    let input = shell.dialog_input.clone();
    input.update(cx, |i, cx| i.set_value("", window, cx));
    input.read(cx).focus_handle(cx).focus(window, cx);
    cx.notify();
    true
}

/// Notice for a slot removed after the dialog captured its rows.
pub(super) const SLOT_GONE: &str = "that grouping slot is no longer configured";

/// Notice for a saved scope removed after the dialog captured its rows.
pub(super) const SCOPE_GONE: &str = "that saved scope no longer exists";

/// Commit through the target's operation. Grouping revalidates the slot
/// against the frame; a scope loads through `load_saved_scope`, the
/// `scope::<name>` actions' own path, and a name gone since the open
/// posts [`SCOPE_GONE`]. Tile kind closes the modal before calling `add_tile`,
/// so modal focus return precedes occupant creation. A log target replaces
/// the rows without closing; a log level requests the change and closes.
fn commit(shell: &mut ShellView, pick: Pick, window: &mut Window, cx: &mut Context<ShellView>) {
    match pick {
        Pick::Slot(slot) => {
            let (changed, still_there) = shell.target_frame().update(cx, |f, cx| {
                let changed = f.set_active_slot(slot);
                if changed {
                    cx.notify();
                }
                (changed, slot.is_none_or(|n| f.slots().get(n).is_some()))
            });
            if !changed && !still_there {
                shell.notice = Some(SLOT_GONE.into());
            }
            shell.close_modal(window, cx);
        }
        Pick::Scope(name) => {
            if shell.load_saved_scope(&name, cx).is_err() {
                shell.notice = Some(SCOPE_GONE.into());
            }
            shell.close_modal(window, cx);
        }
        Pick::Kind(kind) => {
            shell.close_modal(window, cx);
            shell.add_tile(&kind, AddPlacement::Split(None), None, window, cx);
        }
        Pick::KindWith(kind, context) => {
            shell.close_modal(window, cx);
            let state = shell
                .services
                .roster
                .factory(&kind)
                .and_then(|f| f.launch_state(&context));
            shell.add_tile(&kind, AddPlacement::Split(None), state, window, cx);
        }
        Pick::Column {
            domain,
            view,
            column,
        } => {
            // Close first so the object dialog pushes onto the stack the list
            // was opened over, not onto the list.
            shell.close_modal(window, cx);
            objectdialog::render::open_column(shell, domain, &view, &column, window, cx);
        }
        Pick::LogTarget(target) => {
            // Step 2 replaces the rows in place; the modal stays open and
            // the field is reset (`set_value` emits no `Change`, and the
            // new list starts with an empty query).
            let current = effective_level(&shell.diagnostics.read(cx).levels, &target);
            let state = ChoiceDialogState::log_levels(target, current);
            shell
                .choice_dialog_scroll
                .scroll_to_item(state.list.ranked_highlighted());
            shell.choice_dialog = Some(state);
            let input = shell.dialog_input.clone();
            input.update(cx, |i, cx| i.set_value("", window, cx));
            input.read(cx).focus_handle(cx).focus(window, cx);
            cx.notify();
        }
        Pick::LogLevel(target, level) => {
            shell.diagnostics.update(cx, |d, cx| {
                d.request_level(&target, level);
                cx.notify();
            });
            shell.close_modal(window, cx);
        }
        Pick::ActionValue {
            action,
            context,
            value,
        } => {
            // Close first: what `chosen` opens (a confirm) lands on the
            // stack the list was opened over, not on the list.
            shell.close_modal(window, cx);
            shell.run_action_chosen(action, &context, &value, window, cx);
        }
    }
}

/// Route choice keys and grouping digits. Escape from log levels rebuilds
/// the target list and clears its query; other Escape presses reach the
/// shell's modal-close handler.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    match choice::route(ks) {
        Some(ChoiceKey::Cancel) => {
            // The level step goes BACK to the target step; every other
            // dialog (and the target step itself) falls through to
            // `handle_key_down`'s modal-closes-on-escape branch.
            return back_to_targets(shell, window, cx);
        }
        Some(ChoiceKey::Pick) => {
            // The field's live text may never have reached the list
            // through a `Change` event (`set_value` emits none): re-feed
            // it before trusting the highlight.
            let live = shell.dialog_input.read(cx).value().to_string();
            let pick = shell.choice_dialog.as_mut().and_then(|state| {
                state.list.set_query(&live);
                state.highlighted_pick()
            });
            // Nothing lit (every row filtered out): the picker stays open
            // and the empty list says it.
            if let Some(pick) = pick {
                commit(shell, pick, window, cx);
            }
            return true;
        }
        Some(ChoiceKey::Complete) => {
            // Completion updates the list's query directly. Copy it to the shared
            // Input because programmatic `set_value` does not emit Change.
            let text = shell.choice_dialog.as_mut().and_then(|state| {
                state
                    .list
                    .complete()
                    .then(|| state.list.query().to_string())
            });
            if let Some(text) = text {
                let input = shell.dialog_input.clone();
                input.update(cx, |i, cx| i.set_value(text, window, cx));
                input.read(cx).focus_handle(cx).focus(window, cx);
            }
            if let Some(state) = shell.choice_dialog.as_ref() {
                shell
                    .choice_dialog_scroll
                    .scroll_to_item(state.list.ranked_highlighted());
            }
            cx.notify();
            return true;
        }
        Some(ChoiceKey::Nav(cmd)) => {
            if let Some(state) = shell.choice_dialog.as_mut() {
                state.list.nav(cmd);
                shell
                    .choice_dialog_scroll
                    .scroll_to_item(state.list.ranked_highlighted());
            }
            cx.notify();
            return true;
        }
        None => {}
    }
    // Grouping digits jump only on an empty Input. A digit for an unfilled
    // slot is claimed without editing the query; other targets accept digits
    // as filter text.
    if ks.mods == Modifiers::NONE
        && is_digit(&ks.key)
        && shell.dialog_input.read(cx).text().len() == 0
        && matches!(
            shell.choice_dialog.as_ref().map(|s| &s.target),
            Some(Target::Grouping { .. })
        )
    {
        let slot = shell
            .choice_dialog
            .as_ref()
            .and_then(|state| state.jump(&ks.key));
        if let Some(slot) = slot {
            commit(shell, Pick::Slot(slot), window, cx);
        }
        return true;
    }
    false
}

fn is_digit(key: &str) -> bool {
    key.len() == 1 && key.as_bytes()[0].is_ascii_digit()
}

/// Render the filter, ranked clickable choices, and footer. Row clicks commit.
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.choice_dialog.as_ref() else {
        return div().into_any_element();
    };
    let (_, prefix, hints_selector, hints) = chrome(&state.target);
    let no_scopes = matches!(state.target, Target::Scope { .. }) && state.list.options().is_empty();
    let loading = matches!(state.target, Target::ActionValue { values: None, .. });
    let (hints_selector, hints) = if no_scopes {
        ("scope-empty-hints", SCOPE_EMPTY_HINTS)
    } else if loading {
        ("action-loading-hints", ACTION_LOADING_HINTS)
    } else {
        (hints_selector, hints)
    };
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let click_entity = entity.clone();
    let rows = dialog::choice_rows(
        &state.list,
        prefix,
        &shell.choice_dialog_scroll,
        theme,
        move |ranked, window, cx| {
            click_entity.update(cx, |shell, cx| {
                let pick = shell
                    .choice_dialog
                    .as_ref()
                    .and_then(|state| state.pick_at_ranked(ranked));
                if let Some(pick) = pick {
                    commit(shell, pick, window, cx);
                }
            });
        },
    );
    // No saved scope at all: in place of an empty list, say how to make one.
    let body = if no_scopes {
        no_scopes_hint(muted, cx)
    } else if loading {
        div()
            .px_3()
            .text_sm()
            .text_color(muted)
            .debug_selector(|| "action-loading".to_string())
            .child("loading\u{2026}")
            .into_any_element()
    } else {
        rows
    };
    v_flex()
        .gap_2()
        .w(scale::design(WIDTH))
        .child(dialog::filter_row(&shell.dialog_input, None, cx))
        .child(body)
        .child(hint_row(hints, hints_selector, WIDTH, muted, theme.border))
        .into_any_element()
}

/// The scope picker's empty state: how to save the first scope. Names the
/// `scope::save_current` chord from the live keymap when one is bound
/// (none is by default), else the palette row's title. Painted only while
/// the picker is open on an empty set, so the one lookup is not per-frame
/// work on any other surface.
fn no_scopes_hint(muted: gpui::Hsla, cx: &App) -> AnyElement {
    let chord = cx
        .try_global::<crate::tips::Chords>()
        .and_then(|chords| crate::tips::chord_for(&chords.0, "scope::save_current"));
    let lead = "No saved scopes \u{2014} narrow the scope, then save it with the save glyph or";
    gpui_component::h_flex()
        .id("scope-empty-hint")
        .gap_1()
        .items_center()
        .flex_wrap()
        .px_3()
        .text_sm()
        .text_color(muted)
        .debug_selector(|| "scope-empty-hint".to_string())
        .child(lead)
        .map(|el| match chord {
            Some(keys) => el.child(super::kbd::binding(&keys)),
            None => el.child("Scope: Save current as\u{2026}"),
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::parse_keystroke;
    use geode_core::tile_columns::{TileColumn, TileColumns};

    fn tile() -> TileColumns {
        let c = |name: &str, label: &str, derived: bool| TileColumn {
            name: name.into(),
            label: label.into(),
            derived,
        };
        TileColumns {
            view: "tree".into(),
            columns: vec![
                c("model_code", "model_code", false),
                c("npv", "NPV", false),
                c("npv_x2", "npv_x2", true),
            ],
            active: Some(1),
        }
    }

    #[test]
    fn column_rows_name_the_column_when_the_label_differs() {
        let s = ChoiceDialogState::columns(Domain::Views, &tile()).unwrap();
        assert_eq!(s.list.options(), ["model_code", "NPV · npv", "npv_x2"]);
    }

    #[test]
    fn the_cursor_column_is_preselected() {
        let s = ChoiceDialogState::columns(Domain::Views, &tile()).unwrap();
        assert_eq!(
            s.highlighted_pick(),
            Some(Pick::Column {
                domain: Domain::Views,
                view: "tree".into(),
                column: "npv".into(),
            })
        );
    }

    #[test]
    fn schema_omits_derived_columns() {
        let s = ChoiceDialogState::columns(Domain::Schema, &tile()).unwrap();
        assert_eq!(s.list.options(), ["model_code", "NPV · npv"]);
    }

    #[test]
    fn a_derived_cursor_column_preselects_nothing_in_schema() {
        let mut t = tile();
        t.active = Some(2);
        let s = ChoiceDialogState::columns(Domain::Schema, &t).unwrap();
        assert_eq!(
            s.highlighted_pick(),
            Some(Pick::Column {
                domain: Domain::Schema,
                view: "tree".into(),
                column: "model_code".into(),
            }),
            "first row"
        );
    }

    #[test]
    fn no_active_column_places_the_first_row() {
        let mut t = tile();
        t.active = None;
        let s = ChoiceDialogState::columns(Domain::Views, &t).unwrap();
        assert!(matches!(
            s.highlighted_pick(),
            Some(Pick::Column { ref column, .. }) if column == "model_code"
        ));
    }

    #[test]
    fn a_schema_list_with_only_derived_columns_is_none() {
        let mut t = tile();
        t.columns.retain(|c| c.derived);
        t.active = None;
        assert_eq!(ChoiceDialogState::columns(Domain::Schema, &t), None);
    }

    fn slots() -> GroupingSlots {
        let mut s = GroupingSlots::default();
        s.set(1, vec!["book".into(), "lhu".into()]);
        s.set(3, vec!["underlying_ref".into()]);
        s
    }

    /// Only filled slots are rows, the view default first, each spelled
    /// as the toolbar readout spells it.
    #[test]
    fn rows_are_the_view_default_then_every_filled_slot() {
        let (options, targets) = grouping_rows(&slots());
        assert_eq!(
            options,
            vec!["view default", "1 · book / lhu", "3 · underlying_ref"]
        );
        assert_eq!(targets, vec![None, Some(1), Some(3)]);
    }

    /// Opening on the frame's active slot lights that row, so `enter`
    /// on an untouched picker changes nothing.
    #[test]
    fn the_highlight_opens_on_the_active_slot() {
        let state = ChoiceDialogState::grouping(&slots(), Some(3));
        assert_eq!(state.highlighted_slot(), Some(Some(3)));
        let state = ChoiceDialogState::grouping(&slots(), None);
        assert_eq!(state.highlighted_slot(), Some(None));
    }

    /// Typing narrows the rows and `enter` picks the highlighted one;
    /// a query matching nothing leaves nothing to pick.
    #[test]
    fn typing_narrows_and_the_highlight_names_a_slot() {
        let mut state = ChoiceDialogState::grouping(&slots(), None);
        state.list.set_query("under");
        assert_eq!(state.highlighted_slot(), Some(Some(3)));
        state.list.set_query("zzz");
        assert_eq!(state.highlighted_slot(), None);
    }

    /// A digit jumps to that slot when filled, `0` to the view default,
    /// and an unfilled slot's digit does nothing — the chords' own rule.
    #[test]
    fn a_digit_jumps_to_a_filled_slot_or_the_view_default() {
        let state = ChoiceDialogState::grouping(&slots(), None);
        assert_eq!(state.jump("3"), Some(Some(3)));
        assert_eq!(state.jump("0"), Some(None));
        assert_eq!(state.jump("2"), None, "an empty slot is not a target");
        assert_eq!(state.jump("j"), None);
    }

    /// The ranked index a click hands back resolves through the RANKED
    /// list, not the declared one — after a filter the two differ.
    #[test]
    fn a_click_resolves_through_the_ranked_order() {
        let mut state = ChoiceDialogState::grouping(&slots(), None);
        state.list.set_query("under");
        assert_eq!(state.pick_at_ranked(0), Some(Pick::Slot(Some(3))));
        assert_eq!(state.pick_at_ranked(1), None);
    }

    /// `choice::route` is the key table: a bare digit is none of its keys
    /// (it reaches the jump), `enter` is the pick.
    #[test]
    fn a_bare_digit_is_not_a_choice_key() {
        let one = parse_keystroke("1", Modifiers::NONE).unwrap();
        assert_eq!(choice::route(&one), None);
        let enter = parse_keystroke("enter", Modifiers::NONE).unwrap();
        assert_eq!(choice::route(&enter), Some(ChoiceKey::Pick));
    }

    /// Tile rows are the roster's kinds in roster order, titled as the
    /// palette titles them, with the placeholder left out; a digit on
    /// this target is not a jump.
    #[test]
    fn tile_rows_are_the_roster_kinds_titled_minus_the_placeholder() {
        let state =
            ChoiceDialogState::tile_kinds(["blotter", PLACEHOLDER_KIND, "cvi", "diagnostics"]);
        assert_eq!(state.list.options(), ["Blotter", "Cvi", "Diagnostics"]);
        assert_eq!(state.highlighted_pick(), Some(Pick::Kind("blotter".into())));
        assert_eq!(state.jump("1"), None, "digits type on the tile target");
        let mut state = state;
        state.list.set_query("diag");
        assert_eq!(
            state.pick_at_ranked(0),
            Some(Pick::Kind("diagnostics".into()))
        );
    }

    /// A context launch titles the dialog by its subject and lists only
    /// the pre-filtered kinds; a plain `tile_kinds` dialog keeps its fixed
    /// title.
    #[test]
    fn kinds_with_a_context_title_the_dialog_by_it_and_pick_with_it() {
        let ctx = DimensionContext::of(&[("underlying_ref", "SPX")]);
        let state = ChoiceDialogState::tile_kinds_with(
            ["cvi", "dividend"],
            ctx.clone(),
            Some("SPX".into()),
        );
        assert_eq!(state.title().as_ref(), "Open SPX in\u{2026}");
        assert_eq!(
            state.list.options(),
            &["Cvi".to_string(), "Dividend".to_string()]
        );
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::KindWith("cvi".into(), ctx))
        );
        assert_eq!(
            ChoiceDialogState::tile_kinds(["rec"]).title().as_ref(),
            "Add a tile"
        );
    }

    /// Step 1 rows are the seven `geode::` suffixes with each one's
    /// effective level; step 2 rows are the five levels with the current
    /// one lit.
    #[test]
    fn log_level_rows_name_targets_then_levels() {
        let levels = geode_core::log::LogLevels {
            default: geode_core::log::Level::INFO,
            targets: vec![("ingest".into(), geode_core::log::Level::DEBUG)],
        };
        let state = ChoiceDialogState::log_targets(&levels);
        assert_eq!(state.list.options()[0], "ingest · debug");
        assert_eq!(state.list.options()[1], "query · info");
        assert_eq!(state.list.options().len(), geode_core::log::TARGETS.len());
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::LogTarget("ingest".into()))
        );
        assert_eq!(state.jump("1"), None, "digits type on this target");

        let mut state =
            ChoiceDialogState::log_levels("ingest".into(), geode_core::log::Level::DEBUG);
        assert_eq!(
            state.list.options(),
            ["error", "warn", "info", "debug", "trace"]
        );
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::LogLevel(
                "ingest".into(),
                geode_core::log::Level::DEBUG
            )),
            "opens on the current level"
        );
        state.list.set_query("tr");
        assert_eq!(
            state.pick_at_ranked(0),
            Some(Pick::LogLevel(
                "ingest".into(),
                geode_core::log::Level::TRACE
            ))
        );
    }

    fn text_scope(text: &str) -> geode_core::scope::Scope {
        geode_core::scope::Scope {
            text: Some(text.to_string()),
            ..Default::default()
        }
    }

    fn saved(names: &[(&str, &str)]) -> geode_core::scopes::SavedScopes {
        names
            .iter()
            .map(|(name, text)| ((*name).to_string(), text_scope(text)))
            .collect()
    }

    /// One row per saved scope, spelled as its name, in the saved set's
    /// own (name) order; with no scope equal to the frame's the first row
    /// is lit.
    #[test]
    fn scope_rows_are_the_saved_names_in_order_the_first_lit() {
        let state = ChoiceDialogState::scopes(
            &saved(&[("us", "us"), ("eu", "eu"), ("asia", "asia")]),
            &geode_core::scope::Scope::default(),
        );
        assert_eq!(state.list.options(), ["asia", "eu", "us"]);
        assert_eq!(state.highlighted_pick(), Some(Pick::Scope("asia".into())));
        assert_eq!(state.title().as_ref(), "Scope");
        assert_eq!(state.jump("1"), None, "digits type on this target");
    }

    /// The row whose saved scope equals the frame's current scope is lit,
    /// so `enter` on an untouched picker reloads what is already there.
    #[test]
    fn the_scope_equal_to_the_current_one_is_lit() {
        let state = ChoiceDialogState::scopes(
            &saved(&[("asia", "asia"), ("eu", "eu"), ("us", "us")]),
            &text_scope("eu"),
        );
        assert_eq!(state.highlighted_pick(), Some(Pick::Scope("eu".into())));
    }

    /// Two saved scopes equal to the current one: the first in row order.
    #[test]
    fn of_two_equal_saved_scopes_the_first_is_lit() {
        let state = ChoiceDialogState::scopes(
            &saved(&[("asia", "asia"), ("eu", "same"), ("us", "same")]),
            &text_scope("same"),
        );
        assert_eq!(state.highlighted_pick(), Some(Pick::Scope("eu".into())));
    }

    /// No saved scopes: an empty list, nothing to pick, no panic.
    #[test]
    fn no_saved_scopes_is_an_empty_list_with_nothing_to_pick() {
        let state = ChoiceDialogState::scopes(
            &geode_core::scopes::SavedScopes::new(),
            &geode_core::scope::Scope::default(),
        );
        assert!(state.list.options().is_empty());
        assert_eq!(state.highlighted_pick(), None);
        assert_eq!(state.pick_at_ranked(0), None);
    }

    /// A filtered row resolves to its own name through the ranked order:
    /// the target's names index the DECLARED options.
    #[test]
    fn a_filtered_scope_row_names_its_own_scope() {
        let mut state = ChoiceDialogState::scopes(
            &saved(&[("asia", "asia"), ("eu", "eu"), ("us", "us")]),
            &geode_core::scope::Scope::default(),
        );
        state.list.set_query("us");
        assert_eq!(state.pick_at_ranked(0), Some(Pick::Scope("us".into())));
        assert_eq!(state.highlighted_pick(), Some(Pick::Scope("us".into())));
        let Target::Scope { names } = &state.target else {
            panic!("a scope target")
        };
        assert_eq!(names.as_slice(), state.list.options());
    }
}
