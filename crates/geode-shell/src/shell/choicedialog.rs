//! A filter-only choice modal for saved scopes, tile kinds, columns, log
//! levels, a row action's value, and a tile's link groups.
//! [`ChoiceList`] owns ranking, highlight, Tab completion, and navigation.
//! Enter or a row click commits the selected option; digits are filter text.
//!
//! Tile choices follow roster order and omit the placeholder; a
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
//!
//! `tile::link_group` lists, for a tile whose module follows, what the
//! focused tile may follow (the workspace or a group) and, for a tile whose
//! module emits, what it may emit into (none or a group), opening on the
//! row for what it follows now (or emits into, with no follow row). The
//! title names the groups it is in. A pick changes one of the two through
//! the shell's link doors. With no tile focused, a placeholder, or a tile
//! whose module does neither, nothing opens and the status bar says so.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Focusable as _, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use geode_core::context::DimensionContext;
use geode_core::link::{Group, Membership};
use geode_core::log::{Level, LogLevels, TARGETS};
use geode_core::query::DistinctOutcome;
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;
use geode_core::tile_columns::{TileColumn, TileColumns};

use crate::choice::{self, ChoiceKey, ChoiceList};
use crate::defaults::{AddPlacement, capitalize};
use crate::keymap::Keystroke;
use crate::module::placeholder::PLACEHOLDER_KIND;
use crate::tiling::TileId;

use super::ShellView;
use super::dialog;
use super::objectdialog::{self, Domain};
use super::picker::{Hint, hint_row};
use super::scale;

// ---------------------------------------------------------------------
// Pure core — no gpui.
// ---------------------------------------------------------------------

/// What the rows stand for and what a pick does.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
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
    /// `tile::link_group`: the link groups `tile` may follow, when its
    /// module `follows`, and emit into, when it `emits`. `tile` and its
    /// membership (`current`) are captured at open, so a pick lands on the
    /// tile the chooser was opened for even if focus has moved since.
    /// `changes[i]` is what declared option `i` does: a row stands for its
    /// change by position, never by its text.
    LinkGroup {
        tile: TileId,
        follows: bool,
        emits: bool,
        current: Membership,
        changes: Vec<LinkChange>,
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

/// What one link-chooser row changes about its tile: the group it follows
/// (`None` is the workspace) or the group it emits into (`None` is none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkChange {
    Follow(Option<Group>),
    Emit(Option<Group>),
}

/// The follow rows, offered only to a tile whose module follows: the
/// workspace, then each group in `Group::ALL` order.
const FOLLOW_ROWS: [&str; 5] = [
    "follow \u{00b7} workspace",
    "follow \u{00b7} A",
    "follow \u{00b7} B",
    "follow \u{00b7} C",
    "follow \u{00b7} D",
];

/// The emit rows, offered only to a tile whose module emits: none, then
/// each group in `Group::ALL` order.
const EMIT_ROWS: [&str; 5] = [
    "emit \u{00b7} none",
    "emit \u{00b7} A",
    "emit \u{00b7} B",
    "emit \u{00b7} C",
    "emit \u{00b7} D",
];

/// The group a link row at `ix` of its five names: row 0 is "no group",
/// rows 1 to 4 are `Group::ALL`.
fn link_row_group(ix: usize) -> Option<Group> {
    ix.checked_sub(1).map(|g| Group::ALL[g])
}

/// The follow row for what the tile follows now.
fn current_follow_row(current: Membership) -> &'static str {
    FOLLOW_ROWS[current.follow.map_or(0, |g| g.index() + 1)]
}

/// The emit row for what the tile emits into now.
fn current_emit_row(current: Membership) -> &'static str {
    EMIT_ROWS[current.emit.map_or(0, |g| g.index() + 1)]
}

/// The row the link chooser opens on, and returns to when its query is
/// emptied: the row for what the tile follows now, or, for a tile offered
/// no follow row, the row for what it emits into now. Enter on an untouched
/// chooser therefore changes nothing.
fn link_opening_row(follows: bool, current: Membership) -> &'static str {
    if follows {
        current_follow_row(current)
    } else {
        current_emit_row(current)
    }
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
    /// them: a tile row is the palette's `<Kind>` title, a scope row the
    /// saved scope's name.
    pub list: ChoiceList,
    pub target: Target,
}

impl ChoiceDialogState {
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

    /// The link rows for `tile`: the five follow rows when its module
    /// `follows`, then the five emit rows when it `emits`. A tile that does
    /// neither has no rows, and its chooser is never opened
    /// ([`open_link_group`]). The highlight opens on the row for what the
    /// tile does now (`current`), so `enter` on
    /// an untouched chooser changes nothing.
    pub fn link_group(tile: TileId, follows: bool, emits: bool, current: Membership) -> Self {
        let mut options: Vec<String> = Vec::with_capacity(10);
        let mut changes = Vec::with_capacity(10);
        if follows {
            for (ix, text) in FOLLOW_ROWS.iter().enumerate() {
                options.push((*text).to_string());
                changes.push(LinkChange::Follow(link_row_group(ix)));
            }
        }
        if emits {
            for (ix, text) in EMIT_ROWS.iter().enumerate() {
                options.push((*text).to_string());
                changes.push(LinkChange::Emit(link_row_group(ix)));
            }
        }
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        list.place(Some(link_opening_row(follows, current)));
        Self {
            list,
            target: Target::LinkGroup {
                tile,
                follows,
                emits,
                current,
                changes,
            },
        }
    }

    /// The modal's title: the chrome's fixed words, or `Open {subject}…`
    /// for a context launch with a subject. Built once, when the dialog
    /// opens.
    pub fn title(&self) -> SharedString {
        match &self.target {
            Target::LinkGroup { current, .. } => {
                let base = chrome(&self.target).0;
                match (current.follow, current.emit) {
                    (None, None) => base.into(),
                    (Some(f), None) => format!("{base} \u{00b7} following {}", f.letter()).into(),
                    (None, Some(e)) => format!("{base} \u{00b7} emitting {}", e.letter()).into(),
                    (Some(f), Some(e)) => format!(
                        "{base} \u{00b7} following {} \u{00b7} emitting {}",
                        f.letter(),
                        e.letter()
                    )
                    .into(),
                }
            }
            Target::TileKindWith { subject, .. } => match subject {
                Some(u) => format!("Open {u} in\u{2026}").into(),
                None => chrome(&self.target).0.into(),
            },
            Target::Column { domain, view, .. } => match domain {
                Domain::Schema => format!("Edit column in schema \u{b7} {view}").into(),
                _ => format!("Edit column in view \u{b7} {view}").into(),
            },
            Target::ActionValue { title, .. } => title.clone(),
            Target::TileKind { .. } | Target::Scope { .. } | Target::LogLevel { .. } => {
                chrome(&self.target).0.into()
            }
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

    /// Feed the filter's text to the list: the route for every query
    /// change, per keystroke and at commit. `false` when the text is
    /// unchanged, which moves nothing.
    ///
    /// The link chooser lights the row the query ranks first, or the
    /// tile's current row in that row's section (follow or emit) when
    /// nothing outranks it: under a blank query, and under one every row
    /// of a section shares (`f`, `follow`, `e`, `emit`), which ranks them
    /// level. A tile's current follow row is follow-workspace until it is
    /// linked, and that row's text survives `a` and `c`: kept lit by text
    /// it would sit over the follow-A row and Enter would follow nothing.
    /// Lit on the first of the level rows instead, Enter after a shared
    /// prefix would unfollow, or stop the tile emitting. Every other
    /// target keeps the lit row by text.
    pub fn set_query(&mut self, query: &str) -> bool {
        match &self.target {
            Target::LinkGroup { current, .. } => {
                let current = *current;
                self.list.set_query_placing_with(query, |top| {
                    Some(if EMIT_ROWS.contains(&top) {
                        current_emit_row(current)
                    } else {
                        current_follow_row(current)
                    })
                })
            }
            Target::TileKind { .. }
            | Target::TileKindWith { .. }
            | Target::Column { .. }
            | Target::Scope { .. }
            | Target::LogLevel { .. }
            | Target::ActionValue { .. } => self.list.set_query(query),
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
            Target::LinkGroup { tile, changes, .. } => Pick::Link {
                tile: *tile,
                change: changes[declared],
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
}

/// What one row commits. Owned (a `String` kind), not borrowed from the
/// dialog state: a pick is a one-off event whose commit drops that state.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
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
    /// `ShellView::set_follow` or `ShellView::set_emit` on `tile`.
    Link { tile: TileId, change: LinkChange },
    /// The roster action at `action`'s `DimensionAction::chosen` with
    /// `value`, on `context`.
    ActionValue {
        action: usize,
        context: DimensionContext,
        value: String,
    },
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

/// The link chooser's footer. Each key is named for what it does here:
/// Enter chooses the lit row's group for the tile.
const LINK_HINTS: &[Hint] = &[
    Hint::Text("type to filter \u{00b7}"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move \u{00b7}"),
    Hint::Key("enter"),
    Hint::Text("choose \u{00b7}"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

/// The per-target chrome: the modal's title, the selector prefix
/// (`{prefix}-choice-list`, `{prefix}-choice-{text}`, `{prefix}-hints`)
/// and the footer.
fn chrome(target: &Target) -> (&'static str, &'static str, &'static str, &'static [Hint]) {
    match target {
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
        // `title()` appends the groups the tile is in.
        Target::LinkGroup { .. } => ("Link group", "link", "link-hints", LINK_HINTS),
        // Fallback only: `title()` is the action's own.
        Target::ActionValue { .. } => ("Choose a value", "action", "action-hints", ACTION_HINTS),
    }
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

/// Status notice: `tile::link_group` with no focused tile, or with a
/// placeholder focused. A placeholder is in no group, and a membership set
/// on it would be dropped when a module fills it.
pub(crate) const NO_TILE_TO_LINK: &str = "no tile to link";

/// Status notice: `tile::link_group` on a tile whose module neither follows
/// nor emits. No group would change what it shows and it has nothing to
/// post, so the chooser would open with no row.
pub(crate) const NO_GROUP_TO_JOIN: &str = "this tile has no link group to join";

/// Open the link chooser on the focused tile (`tile::link_group`). The
/// tile, whether its module follows and emits, and its membership are read
/// now and kept by the dialog. A tile that does neither gets
/// `this tile has no link group to join` and no dialog.
pub fn open_link_group(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let focused = view
        .services
        .workspaces
        .active()
        .focused_tile()
        .and_then(|tile| view.occupants.get(&tile).map(|o| (tile, o)))
        .filter(|(_, o)| o.kind != PLACEHOLDER_KIND)
        .map(|(tile, o)| (tile, o.content.follows(), o.content.emits()));
    let Some((tile, follows, emits)) = focused else {
        view.notice = Some(NO_TILE_TO_LINK.into());
        cx.notify();
        return;
    };
    if !follows && !emits {
        view.notice = Some(NO_GROUP_TO_JOIN.into());
        cx.notify();
        return;
    }
    let current = view.frame.read(cx).membership(tile);
    let state = ChoiceDialogState::link_group(tile, follows, emits, current);
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

/// Notice for a saved scope removed after the dialog captured its rows.
pub(super) const SCOPE_GONE: &str = "that saved scope no longer exists";

/// Notice for a link pick whose tile closed after the chooser captured it.
pub(super) const TILE_GONE: &str = "that tile is no longer open";

/// Commit through the target's operation. A scope loads through `load_saved_scope`, the
/// `scope::<name>` actions' own path, and a name gone since the open
/// posts [`SCOPE_GONE`]. Tile kind closes the modal before calling `add_tile`,
/// so modal focus return precedes occupant creation. A log target replaces
/// the rows without closing; a log level requests the change and closes. A
/// link row closes the modal and goes through the shell's link doors; a
/// tile closed since the open posts [`TILE_GONE`] and links nothing.
fn commit(shell: &mut ShellView, pick: Pick, window: &mut Window, cx: &mut Context<ShellView>) {
    match pick {
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
        Pick::Link { tile, change } => {
            // Closed first, like a tile-kind pick: the chooser is gone
            // before the doors notify the frame and the tile. The doors
            // are the shell's own, never the frame's: `set_emit` reads the
            // tile and writes the frame, which is sound here because a
            // commit runs in the shell's key or click handler, inside no
            // update of the tile or of the frame.
            shell.close_modal(window, cx);
            // The palette can close the tile under the open chooser. A
            // follow written for it then would stay in the frame for a
            // tile nothing ever unlinks.
            if shell
                .occupant_kind(tile)
                .is_none_or(|kind| kind == PLACEHOLDER_KIND)
            {
                shell.notice = Some(TILE_GONE.into());
                cx.notify();
                return;
            }
            match change {
                LinkChange::Follow(group) => shell.set_follow(tile, group, cx),
                LinkChange::Emit(group) => shell.set_emit(tile, group, cx),
            }
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

/// Route choice keys; digits are filter text. Escape from log levels rebuilds
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
                state.set_query(&live);
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
    false
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
    use crate::keymap::{Modifiers, parse_keystroke};
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

    /// The ranked index a click hands back resolves through the RANKED
    /// list, not the declared one — after a filter the two differ.
    #[test]
    fn a_click_resolves_through_the_ranked_order() {
        let saved = saved(&[("asia", "asia"), ("eu", "eu")]);
        let mut state = ChoiceDialogState::scopes(&saved, &text_scope("none"));
        state.list.set_query("eu");
        assert_eq!(state.pick_at_ranked(0), Some(Pick::Scope("eu".to_string())));
        assert_eq!(state.pick_at_ranked(1), None);
    }

    /// `choice::route` is the key table: a bare digit is none of its keys
    /// (it is filter text), `enter` is the pick.
    #[test]
    fn a_bare_digit_is_not_a_choice_key() {
        let one = parse_keystroke("1", Modifiers::NONE).unwrap();
        assert_eq!(choice::route(&one), None);
        let enter = parse_keystroke("enter", Modifiers::NONE).unwrap();
        assert_eq!(choice::route(&enter), Some(ChoiceKey::Pick));
    }

    /// Tile rows are the roster's kinds in roster order, titled as the
    /// palette titles them, with the placeholder left out.
    #[test]
    fn tile_rows_are_the_roster_kinds_titled_minus_the_placeholder() {
        let state =
            ChoiceDialogState::tile_kinds(["blotter", PLACEHOLDER_KIND, "cvi", "diagnostics"]);
        assert_eq!(state.list.options(), ["Blotter", "Cvi", "Diagnostics"]);
        assert_eq!(state.highlighted_pick(), Some(Pick::Kind("blotter".into())));
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

    // --- Link group ----------------------------------------------------

    const TILE: TileId = TileId(7);

    /// The chooser of a tile that follows; `emits` adds the emit rows.
    fn link(emits: bool, follow: Option<Group>, emit: Option<Group>) -> ChoiceDialogState {
        ChoiceDialogState::link_group(TILE, true, emits, Membership { follow, emit })
    }

    fn link_pick(change: LinkChange) -> Option<Pick> {
        Some(Pick::Link { tile: TILE, change })
    }

    const FOLLOW: [&str; 5] = [
        "follow \u{00b7} workspace",
        "follow \u{00b7} A",
        "follow \u{00b7} B",
        "follow \u{00b7} C",
        "follow \u{00b7} D",
    ];
    const EMIT: [&str; 5] = [
        "emit \u{00b7} none",
        "emit \u{00b7} A",
        "emit \u{00b7} B",
        "emit \u{00b7} C",
        "emit \u{00b7} D",
    ];

    /// A tile that cannot emit is offered no emit row: picking one would
    /// set nothing, and the row would promise what the tile cannot do.
    #[test]
    fn the_link_rows_offer_emitting_only_to_a_tile_that_emits() {
        assert_eq!(link(false, None, None).list.options(), FOLLOW);
        let emitter = link(true, None, None);
        assert_eq!(emitter.list.options().len(), 10);
        assert_eq!(emitter.list.options()[..5], FOLLOW, "follow rows first");
        assert_eq!(emitter.list.options()[5..], EMIT);
    }

    /// A tile whose queries ignore the frame's scope is offered no follow
    /// row: following would show a group's chip over content the group
    /// does not select. Its chooser opens on the row for what it emits
    /// into now, so `enter` on an untouched chooser still changes nothing.
    #[test]
    fn the_link_rows_offer_following_only_to_a_tile_that_follows() {
        let emit_only = |emit| {
            ChoiceDialogState::link_group(TILE, false, true, Membership { follow: None, emit })
        };
        let state = emit_only(None);
        assert_eq!(state.list.options(), EMIT);
        assert_eq!(state.highlighted_pick(), link_pick(LinkChange::Emit(None)));
        assert_eq!(
            state.pick_at_ranked(1),
            link_pick(LinkChange::Emit(Some(Group::A))),
            "a row still stands for its change by position"
        );
        let state = emit_only(Some(Group::C));
        assert_eq!(
            state.highlighted_pick(),
            link_pick(LinkChange::Emit(Some(Group::C)))
        );
        let mut state = emit_only(Some(Group::C));
        assert!(state.set_query("  "));
        assert_eq!(
            state.highlighted_pick(),
            link_pick(LinkChange::Emit(Some(Group::C))),
            "a blank query keeps the opening row"
        );
        let neither = ChoiceDialogState::link_group(TILE, false, false, Membership::default());
        assert!(neither.list.options().is_empty());
        assert_eq!(neither.highlighted_pick(), None);
    }

    /// The chooser opens on the row that says what the tile follows now,
    /// so `enter` on an untouched chooser changes nothing.
    #[test]
    fn the_highlight_opens_on_the_current_follow_row() {
        let state = link(true, Some(Group::C), Some(Group::A));
        assert_eq!(state.list.highlighted_text(), Some("follow \u{00b7} C"));
        assert_eq!(
            state.highlighted_pick(),
            link_pick(LinkChange::Follow(Some(Group::C)))
        );
        let state = link(true, None, Some(Group::A));
        assert_eq!(
            state.list.highlighted_text(),
            Some("follow \u{00b7} workspace")
        );
        assert_eq!(
            state.highlighted_pick(),
            link_pick(LinkChange::Follow(None))
        );
    }

    /// A row stands for its change by position, never by its text, and a
    /// filtered row still resolves to its own.
    #[test]
    fn each_row_picks_its_change() {
        let state = link(true, None, None);
        let picks: Vec<Option<Pick>> = (0..10).map(|row| state.pick_at_ranked(row)).collect();
        let mut expected = vec![link_pick(LinkChange::Follow(None))];
        expected.extend(Group::ALL.map(|g| link_pick(LinkChange::Follow(Some(g)))));
        expected.push(link_pick(LinkChange::Emit(None)));
        expected.extend(Group::ALL.map(|g| link_pick(LinkChange::Emit(Some(g)))));
        assert_eq!(picks, expected);
        assert_eq!(state.pick_at_ranked(10), None);

        let viewer = link(false, None, None);
        assert_eq!(
            viewer.pick_at_ranked(4),
            link_pick(LinkChange::Follow(Some(Group::D)))
        );
        assert_eq!(viewer.pick_at_ranked(5), None, "no emit row to pick");

        for query in ["emit b", "b emit"] {
            let mut state = link(true, None, None);
            state.set_query(query);
            let first = state.list.ranked().first().map(|r| r.row);
            assert_eq!(
                first.map(|row| state.list.options()[row].as_str()),
                Some("emit \u{00b7} B"),
                "{query}"
            );
            assert_eq!(
                state.pick_at_ranked(0),
                link_pick(LinkChange::Emit(Some(Group::B))),
                "{query}"
            );
            assert_eq!(
                state.highlighted_pick(),
                link_pick(LinkChange::Emit(Some(Group::B))),
                "{query}"
            );
        }
    }

    /// The chooser opens on the row for what the tile follows now, and
    /// every tile starts following the workspace. That row's text contains
    /// `a` and `c`, so a highlight kept by text would stay on it through
    /// `a` or `follow c`, and Enter would follow nothing.
    #[test]
    fn a_typed_query_lights_its_top_ranked_row() {
        let follow = |g| link_pick(LinkChange::Follow(Some(g)));
        for (query, expected) in [
            ("a", follow(Group::A)),
            ("follow a", follow(Group::A)),
            ("fa", follow(Group::A)),
            ("c", follow(Group::C)),
            ("follow c", follow(Group::C)),
            ("b", follow(Group::B)),
            ("d", follow(Group::D)),
        ] {
            let mut state = link(false, None, None);
            assert!(state.set_query(query));
            assert_eq!(state.highlighted_pick(), expected, "{query}");
        }
        for (query, expected) in [
            ("emit a", link_pick(LinkChange::Emit(Some(Group::A)))),
            ("emit none", link_pick(LinkChange::Emit(None))),
        ] {
            let mut state = link(true, None, None);
            assert!(state.set_query(query));
            assert_eq!(state.highlighted_pick(), expected, "{query}");
        }
    }

    /// A query every follow row shares ranks them level and so chooses
    /// none of them. The row for what the tile follows now stays lit: on
    /// the first of the tied rows, `follow workspace`, Enter after a shared
    /// prefix would unfollow a tile the trader meant to leave alone.
    #[test]
    fn a_tie_for_the_top_rank_keeps_the_current_follow_row() {
        let follow = |g| link_pick(LinkChange::Follow(g));
        for query in ["f", "follow", "fol", "o"] {
            let mut state = link(true, Some(Group::C), Some(Group::A));
            assert!(state.set_query(query));
            assert_eq!(
                state.list.ranked()[..5]
                    .iter()
                    .map(|r| r.row)
                    .collect::<Vec<_>>(),
                [0, 1, 2, 3, 4],
                "fixture: {query} ranks the follow rows level, in declared order"
            );
            assert_eq!(state.highlighted_pick(), follow(Some(Group::C)), "{query}");

            let mut unlinked = link(true, None, None);
            assert!(unlinked.set_query(query));
            assert_eq!(unlinked.highlighted_pick(), follow(None), "{query}");
        }
        // A query that ranks one row above the current one has chosen.
        let mut state = link(true, Some(Group::C), None);
        assert!(state.set_query("follow a"));
        assert_eq!(state.highlighted_pick(), follow(Some(Group::A)));
        assert!(state.set_query("emit"));
        assert_eq!(
            state.highlighted_pick(),
            link_pick(LinkChange::Emit(None)),
            "the current follow row is not among the rows `emit` ranks"
        );
    }

    /// The emit rows tie as the follow rows do. A query every emit row
    /// shares chooses none of them, so the row for what the tile emits
    /// into now stays lit: on `emit \u{00b7} none`, the first of them, Enter
    /// would stop a tile the trader meant to leave emitting.
    #[test]
    fn a_tie_for_the_top_rank_keeps_the_current_emit_row() {
        let emit = |g| link_pick(LinkChange::Emit(g));
        for query in ["e", "emit", "em", "mi"] {
            let mut state = link(true, Some(Group::C), Some(Group::B));
            assert!(state.set_query(query));
            let top = state.list.ranked()[0].row;
            assert_eq!(
                state.list.options()[top],
                EMIT[0],
                "fixture: {query} ranks the emit rows first, in declared order"
            );
            assert_eq!(state.highlighted_pick(), emit(Some(Group::B)), "{query}");

            let mut not_emitting = link(true, Some(Group::C), None);
            assert!(not_emitting.set_query(query));
            assert_eq!(not_emitting.highlighted_pick(), emit(None), "{query}");
        }
        // A query that ranks one row above the current one has chosen.
        for (query, expected) in [
            ("emit a", emit(Some(Group::A))),
            ("emit none", emit(None)),
            ("none", emit(None)),
        ] {
            let mut state = link(true, Some(Group::C), Some(Group::B));
            assert!(state.set_query(query));
            assert_eq!(state.highlighted_pick(), expected, "{query}");
        }
        // The follow side is untouched by the emit side's current row, and
        // a tile offered only emit rows keeps its own on a blank query.
        let mut state = link(true, Some(Group::C), Some(Group::B));
        assert!(state.set_query("follow"));
        assert_eq!(
            state.highlighted_pick(),
            link_pick(LinkChange::Follow(Some(Group::C)))
        );
        let mut emit_only = ChoiceDialogState::link_group(
            TILE,
            false,
            true,
            Membership {
                follow: None,
                emit: Some(Group::B),
            },
        );
        assert!(emit_only.set_query("emit"));
        assert_eq!(emit_only.highlighted_pick(), emit(Some(Group::B)));
    }

    /// A query of spaces ranks every row, as an empty one does, so it lights
    /// what an empty one does: on a tile following C, a space then Enter
    /// leaves it following C.
    #[test]
    fn a_blank_query_keeps_the_current_follow_row() {
        for query in [" ", "   ", "\t"] {
            let mut state = link(true, Some(Group::C), None);
            assert!(state.set_query(query));
            assert_eq!(state.list.ranked().len(), 10, "{query:?} drops no row");
            assert_eq!(
                state.highlighted_pick(),
                link_pick(LinkChange::Follow(Some(Group::C))),
                "{query:?}"
            );
        }
    }

    /// A highlight the trader moved after typing is theirs: re-reading the
    /// same text at commit keeps it. The next query change places it again,
    /// and an emptied query returns to the opening row.
    #[test]
    fn a_moved_highlight_is_kept_until_the_query_changes() {
        let mut state = link(false, Some(Group::C), None);
        assert!(state.set_query("follow"));
        assert_eq!(
            state.highlighted_pick(),
            link_pick(LinkChange::Follow(Some(Group::C))),
            "a tie keeps the current row"
        );
        state.list.nav(crate::vimnav::NavCommand::Move(1));
        let moved = link_pick(LinkChange::Follow(Some(Group::D)));
        assert_eq!(state.highlighted_pick(), moved);
        assert!(!state.set_query("follow"), "the commit's re-read");
        assert_eq!(state.highlighted_pick(), moved);

        assert!(state.set_query("follow a"));
        assert_eq!(
            state.highlighted_pick(),
            link_pick(LinkChange::Follow(Some(Group::A)))
        );
        assert!(state.set_query(""));
        assert_eq!(
            state.highlighted_pick(),
            link_pick(LinkChange::Follow(Some(Group::C))),
            "the opening row: what the tile follows now"
        );
    }

    /// The other choice dialogs keep the highlight by text across a query
    /// change, as `ChoiceList::set_query` does.
    #[test]
    fn other_targets_keep_the_highlight_by_text() {
        let saved = saved(&[("asia", "asia"), ("australasia", "aus")]);
        // Opens lit on `australasia`, the scope equal to the current one.
        let mut state = ChoiceDialogState::scopes(&saved, &text_scope("aus"));
        assert!(state.set_query("a"));
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::Scope("australasia".to_string())),
            "it still matches and stays lit"
        );
        assert_ne!(
            state.list.ranked_highlighted(),
            0,
            "fixture: it is not the top-ranked row"
        );
    }

    #[test]
    fn the_title_names_the_current_groups() {
        let title = |follow, emit| link(true, follow, emit).title().to_string();
        assert_eq!(title(None, None), "Link group");
        assert_eq!(
            title(Some(Group::A), None),
            "Link group \u{00b7} following A"
        );
        assert_eq!(
            title(None, Some(Group::B)),
            "Link group \u{00b7} emitting B"
        );
        assert_eq!(
            title(Some(Group::A), Some(Group::B)),
            "Link group \u{00b7} following A \u{00b7} emitting B"
        );
    }
}
