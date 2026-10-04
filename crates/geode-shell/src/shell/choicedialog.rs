//! A filter-only choice modal for tile kinds, columns, log
//! levels, a row action's value, and a tile's link groups.
//! [`ChoiceList`] owns ranking, highlight, Tab completion, and navigation.
//! Enter or a row click commits the selected option; digits are filter text.
//!
//! Tile choices follow roster order and omit the placeholder; a
//! commit fills the focused placeholder or splits the focused real tile.
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
//!
//! The row menu's `Color…` lists, for one value of a text dimension, each
//! named color (alphabetical, with its swatch), then the twelve presets
//! (`preset · {name}`), then `Custom…` (the hue stage), then
//! `New named color…`, then `None`, then `Follow desk ({name})` when the
//! user layer overrides a different, colored lower entry. While the query is
//! a whole number 0–360 a typed `Hue {n}` row is pinned on top, lit, with
//! `New named color…` pinned beneath it so the typed hue can seed one. It
//! opens on the color in force (an inline preset on its row, any other
//! inline entry on `Custom…`, `None` without one), and a query cleared back
//! to blank lights that row again, so enter on an untouched list changes
//! nothing; rows stand for their pick by position, so a color named `None`
//! or `Custom…` is still that color. A pick goes to
//! `ShellView::set_value_color`, which writes the user layer's
//! `value_colors` entry off the UI thread. With no named color the list
//! begins with the presets.
//!
//! `New named color…` pushes the Colors dialog over the list at its naming
//! stage, the value's sanitized name prefilled and the draft seeded from
//! [`ChoiceDialogState::new_color_seed`]. Creating the color removes the
//! covered list ([`drop_covered_value_color`]) and, once the color's write
//! lands, colors the value with it; escape or `‹` at naming returns to the
//! list intact.
//!
//! `Custom…` opens the hue stage, a second stage of the same modal: the
//! pure [`HueStage`] on the target, a gpui-component slider whose state
//! entity is `ShellView::hue_slider` (created on entry, dropped on leaving),
//! a hue field, a tone control, and Apply/Cancel. While it is open the list's
//! field is unpainted and blurred and the stage claims every non-chord key
//! through the shell's modal route; `escape` or `‹` returns to the list.
//! Apply on the hue and tone of the color in force writes nothing.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Focusable as _, SharedString, Window, div};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex,
};

use geode_core::colour::{
    Base, Definition, NamedColours, PRESETS, Tone, ValueColorState, ValueEntry, ValuePick,
    preset_of, value_color_state,
};

use geode_core::config::VALUE_COLORS_DOC;
use geode_core::context::DimensionContext;
use geode_core::link::{Group, Membership};
use geode_core::log::{Level, LogLevels, TARGETS};
use geode_core::query::DistinctOutcome;
use geode_core::tile_columns::{TileColumn, TileColumns};

use crate::choice::{self, ChoiceKey, ChoiceList};
use crate::defaults::{AddPlacement, capitalize};
use crate::keymap::{Keystroke, Modifiers};
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
    /// The row menu's `Color…`: what `value` of `dimension` may be colored
    /// with. `picks[i]` is what declared option `i` does (a row stands for
    /// its pick by position: a color may be named like another row), and
    /// `swatches[i]` the definition its row paints a swatch of. Captured at
    /// open; `resolved` holds them resolved under the theme last painted.
    /// `in_force` is the definition of the color in force at open (the
    /// stage's start and `Custom…`'s swatch); `stage` is `Custom…`'s hue
    /// stage while it is open.
    ValueColor {
        dimension: String,
        value: String,
        picks: Vec<ColorRow>,
        swatches: Vec<Option<Definition>>,
        resolved: SwatchCache,
        in_force: Option<Definition>,
        stage: Option<Box<HueStage>>,
        /// The hue the query spells, while it spells one: row 0 is then `Hue {n}`.
        typed: Option<u16>,
        /// The declared row the list opened on, without a typed row: a query
        /// cleared back to blank lights it again, so enter is still no change.
        opening: usize,
    },
}

/// One value-color row's resolved swatch and its stable selector, or `None`
/// for a row without a color.
pub type SwatchRow = Option<(gpui::Hsla, SharedString)>;

/// The value-color list's swatches resolved (OKLCH and contrast seek) under
/// one theme, keyed on that theme's [`super::colours::theme_signature`], so
/// a repaint under the same theme reuses them and a theme change while the
/// list is open resolves them again. A cache, not part of the dialog's
/// identity: any two compare equal.
#[derive(Debug, Clone, Default)]
pub struct SwatchCache(std::cell::RefCell<Option<Box<SwatchMemo>>>);

/// The theme signature a set of swatch rows was resolved under, and the
/// rows. Boxed in [`SwatchCache`] so the signature does not widen `Target`.
type SwatchMemo = ([gpui::Hsla; 28], Rc<[SwatchRow]>);

impl PartialEq for SwatchCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl SwatchCache {
    /// The rows resolved under the theme `key` names: the cached rows when
    /// `key` is the one they were resolved under, else `resolve`'s, kept.
    pub fn rows(
        &self,
        key: [gpui::Hsla; 28],
        resolve: impl FnOnce() -> Rc<[SwatchRow]>,
    ) -> Rc<[SwatchRow]> {
        let mut slot = self.0.borrow_mut();
        match slot.as_deref() {
            Some((k, rows)) if *k == key => rows.clone(),
            _ => {
                let rows = resolve();
                *slot = Some(Box::new((key, rows.clone())));
                rows
            }
        }
    }
}

/// Each row's swatch resolved under `anchors` and `tokens`: a row with a
/// captured definition and a selector gets its color; every other row `None`.
fn resolve_swatches(
    swatches: &[Option<Definition>],
    picks: &[ColorRow],
    anchors: &geode_core::colour::Anchors,
    tokens: &geode_core::colour::Tokens,
) -> Rc<[SwatchRow]> {
    swatches
        .iter()
        .zip(picks)
        .map(|(definition, row)| {
            let definition = definition.as_ref()?;
            let selector = swatch_selector(row)?;
            Some((
                super::colours::to_hsla(geode_core::colour::resolve(definition, anchors, tokens)),
                SharedString::from(selector),
            ))
        })
        .collect()
}

/// A swatch's stable selector, derived from what its row is.
fn swatch_selector(row: &ColorRow) -> Option<String> {
    match row {
        ColorRow::Set {
            pick: ValuePick::Color(name),
            ..
        } => Some(format!("valuecolor-swatch-{name}")),
        ColorRow::Set {
            pick: ValuePick::Inline(_),
            preset: Some(preset),
        } => Some(format!("valuecolor-swatch-preset-{preset}")),
        ColorRow::Set {
            pick: ValuePick::Inline(_),
            preset: None,
        } => Some("valuecolor-swatch-hue".to_string()),
        ColorRow::Custom => Some("valuecolor-swatch-custom".to_string()),
        ColorRow::NewNamed => None,
        ColorRow::Set { .. } => None,
    }
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
    /// them: a tile row is the palette's `<Kind>` title.
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
            Target::TileKind { .. } | Target::LogLevel { .. } => chrome(&self.target).0.into(),
            Target::ValueColor {
                dimension, value, ..
            } => format!("Color \u{b7} {dimension} {value}").into(),
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
    ///
    /// The value-color list pins a `Hue {n}` row on top while the query is
    /// a whole number 0–360 ([`parse_hue`]), lit, and `New named color…`
    /// pinned second (the digits filter it out by text), with the other
    /// rows filtering beneath them; an unchanged query moves nothing, so enter's
    /// re-feed keeps a moved highlight. A query cleared back to blank lights
    /// the row the list opened on, so enter on it still changes nothing.
    pub fn set_query(&mut self, query: &str) -> bool {
        match &mut self.target {
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
            Target::ValueColor {
                typed,
                picks,
                swatches,
                resolved,
                opening,
                ..
            } => {
                if query == self.list.query() {
                    return false;
                }
                let hue = parse_hue(query);
                if hue != *typed {
                    let mut options = self.list.options().to_vec();
                    if typed.is_some() {
                        options.remove(0);
                        picks.remove(0);
                        swatches.remove(0);
                    }
                    if let Some(n) = hue {
                        let def = Definition::hue(f32::from(n), Tone::Normal);
                        options.insert(0, format!("Hue {n}"));
                        picks.insert(0, ColorRow::set(ValuePick::Inline(def.clone())));
                        swatches.insert(0, Some(def));
                    }
                    *typed = hue;
                    *resolved = SwatchCache::default();
                    self.list.replace_options(options);
                }
                self.list.set_query(query);
                if typed.is_some() {
                    // `New named color…` second: a digits query filters it
                    // out by text, and it is how the typed hue seeds a new
                    // named color. Pinned by declared index, so its pick
                    // stays its own; the next rerank drops both pins.
                    if let Some(new) = picks.iter().position(|p| *p == ColorRow::NewNamed) {
                        self.list.pin_top(new);
                    }
                    self.list.pin_top(0);
                } else if query.trim().is_empty()
                    && let Some(at) = self.list.ranked().iter().position(|r| r.row == *opening)
                {
                    // Blank again: the opening row, not whatever row the
                    // dropped typed row's highlight fell back to.
                    self.list.set_ranked_highlighted(at);
                }
                true
            }
            Target::TileKind { .. }
            | Target::TileKindWith { .. }
            | Target::Column { .. }
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
            Target::ValueColor {
                dimension,
                value,
                picks,
                ..
            } => Pick::ValueColor {
                dimension: dimension.clone(),
                value: value.clone(),
                pick: picks[declared].clone(),
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
    /// What one value-color row does, for `value` of `dimension`.
    ValueColor {
        dimension: String,
        value: String,
        pick: ColorRow,
    },
}

/// The value-color list's no-color row.
pub const NO_COLOR_ROW: &str = "None";

/// The value-color list's row that opens the hue stage.
pub const CUSTOM_ROW: &str = "Custom\u{2026}";

/// The value-color list's row that opens the Colors dialog to name a new color.
pub const NEW_NAMED_ROW: &str = "New named color\u{2026}";

/// The hue a stage opens on when the color in force has none (a token
/// color, or no color).
const DEFAULT_HUE: u16 = 240;

/// What one value-color row does.
#[derive(Debug, Clone, PartialEq)]
pub enum ColorRow {
    /// Write `pick`; `preset` names the preset row it is, for the notice.
    Set {
        pick: ValuePick,
        preset: Option<&'static str>,
    },
    /// `Custom…`: open the hue stage.
    Custom,
    /// `New named color…`: open the Colors dialog at its naming stage.
    NewNamed,
}

impl ColorRow {
    pub fn set(pick: ValuePick) -> ColorRow {
        ColorRow::Set { pick, preset: None }
    }

    /// The preset this row is, if it is one.
    pub fn preset(&self) -> Option<&'static str> {
        match self {
            ColorRow::Set { preset, .. } => *preset,
            ColorRow::Custom | ColorRow::NewNamed => None,
        }
    }
}

/// A hue typed as text: one to three ASCII digits, 0–360, with 360 read as
/// 0 as the document reads it. Shared by the typed `Hue {n}` row and the
/// hue stage's field so the two accept the same text.
pub(crate) fn parse_hue(text: &str) -> Option<u16> {
    let text = text.trim();
    if text.is_empty() || text.len() > 3 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u16 = text.parse().ok()?;
    (n <= 360).then_some(n % 360)
}

/// `Custom…`'s hue stage: the hue (always valid, 0–359) and tone the
/// preview and Apply use, and the hue field's text, which may be mid-edit or
/// out of range. Pure; the slider entity mirroring `hue` is
/// `ShellView::hue_slider`.
#[derive(Debug, Clone, PartialEq)]
pub struct HueStage {
    pub hue: u16,
    pub tone: Tone,
    /// The hue field's text, rebuilt by the handlers that change it so a
    /// paint clones a reference count, never the text.
    pub field: SharedString,
    /// The next digit replaces the field rather than appending: true at open
    /// and after every step, so typing `90` after `l` reads 90.
    fresh: bool,
    /// The hue and tone of the color in force at open, as the stage reads
    /// them, when it is a hue (named or inline): Apply on them writes
    /// nothing, so an untouched stage never detaches a value from its name.
    in_force: Option<(u16, Tone)>,
    /// The value's text, painted by the preview.
    pub value: SharedString,
    pub cache: StageCache,
}

impl HueStage {
    pub fn new(hue: u16, tone: Tone, value: SharedString) -> Self {
        Self {
            hue,
            tone,
            field: hue.to_string().into(),
            fresh: true,
            in_force: None,
            value,
            cache: StageCache::default(),
        }
    }

    /// The stage the color in force opens: its hue and tone when it is a
    /// hue (inline or named), else [`DEFAULT_HUE`], normal.
    pub fn start(in_force: Option<&Definition>, value: SharedString) -> Self {
        match in_force.map(|d| &d.base) {
            Some(Base::Hue { degrees, tone }) => {
                let hue = (degrees.round() as i32).rem_euclid(360) as u16;
                Self {
                    in_force: Some((hue, *tone)),
                    ..Self::new(hue, *tone, value)
                }
            }
            _ => Self::new(DEFAULT_HUE, Tone::Normal, value),
        }
    }

    /// Move `delta` degrees round the wheel, wrapping.
    pub fn step(&mut self, delta: i32) {
        self.hue = (self.hue as i32 + delta).rem_euclid(360) as u16;
        self.field = self.hue.to_string().into();
        self.fresh = true;
    }

    /// The slider moved to `hue`.
    pub fn set_hue(&mut self, hue: u16) {
        self.hue = hue % 360;
        self.field = self.hue.to_string().into();
        self.fresh = true;
    }

    /// A digit typed into the field; the hue follows when the text is valid.
    pub fn digit(&mut self, digit: char) {
        let mut text = if self.fresh {
            String::new()
        } else {
            self.field.to_string()
        };
        self.fresh = false;
        if text.len() < 3 {
            text.push(digit);
        }
        self.set_field(text);
    }

    /// Backspace in the field.
    pub fn erase(&mut self) {
        self.fresh = false;
        let mut text = self.field.to_string();
        text.pop();
        self.set_field(text);
    }

    /// The field now reads `text`; the hue follows when it is one.
    fn set_field(&mut self, text: String) {
        if let Some(hue) = parse_hue(&text) {
            self.hue = hue;
        }
        self.field = text.into();
    }

    pub fn toggle_tone(&mut self) {
        self.tone = match self.tone {
            Tone::Normal => Tone::Light,
            Tone::Light => Tone::Normal,
        };
    }

    /// Whether the field holds a hue: Apply is refused otherwise.
    pub fn valid(&self) -> bool {
        parse_hue(&self.field).is_some()
    }

    pub fn definition(&self) -> Definition {
        Definition::hue(f32::from(self.hue), self.tone)
    }

    /// Whether the stage's hue and tone are those of the color in force
    /// (a named color's `colors.toml` definition, or the inline entry).
    pub fn holds_in_force(&self) -> bool {
        self.in_force == Some((self.hue, self.tone))
    }
}

/// The track memo: the theme signature and tone, and one stop per 15°.
type TrackMemo = (([gpui::Hsla; 28], Tone), Rc<[gpui::Hsla]>);
/// The preview memo: the theme signature, hue and tone, and the color.
type PreviewMemo = (([gpui::Hsla; 28], u16, Tone), gpui::Hsla);

/// The hue stage's colors resolved under one theme: the slider track (for a
/// tone) and the preview (for a hue and tone). Each resolves once per key:
/// a step re-resolves only the preview, a theme change both. A cache, not
/// part of the stage's identity: any two compare equal.
#[derive(Debug, Clone, Default)]
pub struct StageCache {
    track: std::cell::RefCell<Option<Box<TrackMemo>>>,
    preview: std::cell::RefCell<Option<Box<PreviewMemo>>>,
    /// Tracks a paint had to resolve itself ([`Self::painted_track`]): a
    /// handler that changed the key without warming it.
    #[cfg(test)]
    paint_misses: std::cell::Cell<usize>,
}

impl PartialEq for StageCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl StageCache {
    pub fn track(
        &self,
        theme: [gpui::Hsla; 28],
        tone: Tone,
        resolve: impl FnOnce() -> Rc<[gpui::Hsla]>,
    ) -> Rc<[gpui::Hsla]> {
        let key = (theme, tone);
        let mut slot = self.track.borrow_mut();
        match slot.as_deref() {
            Some((k, stops)) if *k == key => stops.clone(),
            _ => {
                let stops = resolve();
                *slot = Some(Box::new((key, stops.clone())));
                stops
            }
        }
    }

    /// Whether the track for `theme` and `tone` is cached, so a paint
    /// resolves nothing.
    #[cfg(test)]
    pub fn holds_track(&self, theme: [gpui::Hsla; 28], tone: Tone) -> bool {
        self.track
            .borrow()
            .as_deref()
            .is_some_and(|(k, _)| *k == (theme, tone))
    }

    /// [`Self::track`] as a paint reads it. The handlers that change the
    /// tone warm the track first, so this only resolves after a theme
    /// change while the stage is open, as the swatches do.
    pub fn painted_track(
        &self,
        theme: [gpui::Hsla; 28],
        tone: Tone,
        resolve: impl FnOnce() -> Rc<[gpui::Hsla]>,
    ) -> Rc<[gpui::Hsla]> {
        #[cfg(test)]
        if !self.holds_track(theme, tone) {
            self.paint_misses.set(self.paint_misses.get() + 1);
        }
        self.track(theme, tone, resolve)
    }

    /// How many tracks a paint resolved itself.
    #[cfg(test)]
    pub fn paint_misses(&self) -> usize {
        self.paint_misses.get()
    }

    pub fn preview(
        &self,
        theme: [gpui::Hsla; 28],
        hue: u16,
        tone: Tone,
        resolve: impl FnOnce() -> gpui::Hsla,
    ) -> gpui::Hsla {
        let key = (theme, hue, tone);
        let mut slot = self.preview.borrow_mut();
        match slot.as_deref() {
            Some((k, color)) if *k == key => *color,
            _ => {
                let color = resolve();
                *slot = Some(Box::new((key, color)));
                color
            }
        }
    }
}

/// What one key does on the hue stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageKey {
    Back,
    Apply,
    Step(i32),
    Tone,
    Digit(char),
    Erase,
    Swallow,
}

/// The hue stage's keys: `escape` back, `enter` apply, `h`/`l` and `←`/`→`
/// step 15° (with `shift` 1°), `t` tone, digits and `backspace` edit the
/// field. Any other non-chord key is swallowed so it never types into the
/// list's field beneath. `None` for a chord, which the shell's modal route
/// may still resolve (the palette, a dialog opener).
pub fn stage_key(ks: &Keystroke) -> Option<StageKey> {
    if ks.mods.is_chord() {
        return None;
    }
    let bare = ks.mods == Modifiers::NONE;
    let shift = ks.mods
        == (Modifiers {
            shift: true,
            ..Modifiers::NONE
        });
    Some(match ks.key.as_str() {
        "escape" => StageKey::Back,
        "enter" if bare => StageKey::Apply,
        "h" | "left" if bare => StageKey::Step(-15),
        "l" | "right" if bare => StageKey::Step(15),
        "h" | "left" if shift => StageKey::Step(-1),
        "l" | "right" if shift => StageKey::Step(1),
        "t" if bare => StageKey::Tone,
        "backspace" => StageKey::Erase,
        key if bare && key.len() == 1 && key.as_bytes()[0].is_ascii_digit() => {
            StageKey::Digit(key.as_bytes()[0] as char)
        }
        _ => StageKey::Swallow,
    })
}

/// The hue stage's slider: its state entity and the subscription that
/// feeds a pointer move back into the stage. Dropped with the stage.
pub struct HueSlider {
    pub(crate) state: Entity<SliderState>,
    _change: gpui::Subscription,
}

impl ChoiceDialogState {
    /// One row per named color (alphabetical), then twelve presets
    /// (`preset · {name}`, [`PRESETS`]), then [`CUSTOM_ROW`], then
    /// [`NEW_NAMED_ROW`], then [`NO_COLOR_ROW`], then `Follow desk ({label})` when the user layer
    /// overrides a different lower entry. Opens on the color in force: a
    /// named color on its row, an inline entry equal to a preset on that
    /// preset, any other inline entry on `Custom…`, none (or a name no
    /// longer defined) on the no-color row, so `enter` on an untouched list
    /// changes nothing. Rows are found by position, never by
    /// text: a color may be named `None` or `Custom…`.
    pub fn value_colors(
        dimension: String,
        value: String,
        named: &NamedColours,
        state: &ValueColorState,
    ) -> Self {
        let in_force = state.effective.as_ref().and_then(|entry| match entry {
            ValueEntry::Named(name) => named.get(name).cloned(),
            ValueEntry::Inline(definition) => Some(definition.clone()),
        });
        let mut options: Vec<String> = named.names().map(str::to_string).collect();
        let mut picks: Vec<ColorRow> = options
            .iter()
            .map(|name| ColorRow::set(ValuePick::Color(name.clone())))
            .collect();
        let mut swatches: Vec<Option<Definition>> = options
            .iter()
            .map(|name| named.get(name).cloned())
            .collect();
        for (name, degrees) in PRESETS {
            let definition = Definition::hue(f32::from(degrees), Tone::Normal);
            options.push(format!("preset \u{b7} {name}"));
            picks.push(ColorRow::Set {
                pick: ValuePick::Inline(definition.clone()),
                preset: Some(name),
            });
            swatches.push(Some(definition));
        }
        let custom = options.len();
        options.push(CUSTOM_ROW.to_string());
        picks.push(ColorRow::Custom);
        swatches.push(in_force.clone());
        options.push(NEW_NAMED_ROW.to_string());
        picks.push(ColorRow::NewNamed);
        swatches.push(None);
        let no_color = options.len();
        options.push(NO_COLOR_ROW.to_string());
        picks.push(ColorRow::set(ValuePick::None));
        swatches.push(None);
        if let Some(lower) = state.follow_desk() {
            options.push(format!("Follow desk ({})", lower.label()));
            picks.push(ColorRow::set(ValuePick::FollowDesk));
            swatches.push(None);
        }
        let opening = match state.effective.as_ref() {
            Some(ValueEntry::Named(name)) => picks.iter().position(
                |p| matches!(p, ColorRow::Set { pick: ValuePick::Color(n), .. } if n == name),
            ),
            Some(ValueEntry::Inline(definition)) => preset_of(definition)
                .and_then(|preset| picks.iter().position(|p| p.preset() == Some(preset)))
                .or(Some(custom)),
            None => None,
        }
        .unwrap_or(no_color);
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        // Unfiltered, ranked order is declared order.
        list.set_ranked_highlighted(opening);
        Self {
            list,
            target: Target::ValueColor {
                dimension,
                value,
                picks,
                swatches,
                resolved: SwatchCache::default(),
                in_force,
                stage: None,
                typed: None,
                opening,
            },
        }
    }

    /// What `New named color…` seeds the new color with: the typed `Hue {n}` while
    /// the query shows one, else the color in force (inline or named), else hue 240.
    pub fn new_color_seed(&self) -> Definition {
        match &self.target {
            Target::ValueColor { typed: Some(n), .. } => {
                Definition::hue(f32::from(*n), Tone::Normal)
            }
            Target::ValueColor {
                in_force: Some(definition),
                ..
            } => definition.clone(),
            _ => Definition::hue(f32::from(DEFAULT_HUE), Tone::Normal),
        }
    }
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
        Target::LogLevel { .. } => ("Log level", "loglevel", "loglevel-hints", LOG_HINTS),
        // `title()` appends the groups the tile is in.
        Target::LinkGroup { .. } => ("Link group", "link", "link-hints", LINK_HINTS),
        // Fallback only: `title()` is the action's own.
        Target::ActionValue { .. } => ("Choose a value", "action", "action-hints", ACTION_HINTS),
        // Fallback only: `title()` names the dimension and the value.
        Target::ValueColor { .. } => ("Color", "valuecolor", "valuecolor-hints", ACTION_HINTS),
    }
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

/// Open the color pick list for `value` of `dimension` (the row menu's
/// `Color…`).
pub(crate) fn open_value_color(
    view: &mut ShellView,
    dimension: String,
    value: String,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let (named, _) = NamedColours::from_config(&view.services.config);
    let state = value_color_state(
        view.services.config.layered_docs(VALUE_COLORS_DOC),
        &dimension,
        &value,
    );
    let state = ChoiceDialogState::value_colors(dimension, value, &named, &state);
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

/// Remove the value-color list from beneath the dialog covering it: `New named
/// color…`'s Colors dialog, once its color is created. The entries above keep their
/// saved input. Nothing happens when the list is on top or absent.
pub(crate) fn drop_covered_value_color(shell: &mut ShellView) {
    let covered = shell
        .modals
        .iter()
        .position(|m| m.kind == dialog::DialogKind::Choice)
        .filter(|&at| at + 1 < shell.modals.len());
    let is_value_color = matches!(
        shell.choice_dialog.as_ref().map(|s| &s.target),
        Some(Target::ValueColor { .. })
    );
    if let Some(at) = covered
        && is_value_color
    {
        shell.modals.remove(at);
        shell.choice_dialog = None;
        shell.hue_slider = None;
    }
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
    dialog::set_back(
        view,
        |shell| on_level_step(shell) || on_hue_stage(shell),
        |shell, window, cx| {
            if !back_to_targets(shell, window, cx) {
                leave_hue_stage(shell, window, cx);
            }
        },
    );
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

fn hue_stage(shell: &ShellView) -> Option<&HueStage> {
    match shell.choice_dialog.as_ref().map(|s| &s.target) {
        Some(Target::ValueColor {
            stage: Some(stage), ..
        }) => Some(&**stage),
        _ => None,
    }
}

fn hue_stage_mut(shell: &mut ShellView) -> Option<&mut HueStage> {
    match shell.choice_dialog.as_mut().map(|s| &mut s.target) {
        Some(Target::ValueColor {
            stage: Some(stage), ..
        }) => Some(&mut **stage),
        _ => None,
    }
}

/// Whether the value-color list is on its hue stage.
pub(super) fn on_hue_stage(shell: &ShellView) -> bool {
    hue_stage(shell).is_some()
}

/// `Custom…`: open the hue stage on the color in force, with its slider
/// entity and subscription (created here, never in render). The list's field
/// is blurred: the stage claims its keys through the shell's modal route.
fn enter_hue_stage(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(Target::ValueColor {
        value,
        in_force,
        stage,
        ..
    }) = shell.choice_dialog.as_mut().map(|s| &mut s.target)
    else {
        return;
    };
    let opened = HueStage::start(in_force.as_ref(), SharedString::from(value.clone()));
    let hue = opened.hue;
    *stage = Some(Box::new(opened));
    let slider = cx.new(|_| {
        SliderState::new()
            .min(0.)
            .max(359.)
            .step(1.)
            .default_value(f32::from(hue))
    });
    let change = cx.subscribe(&slider, |shell, _, event: &SliderEvent, cx| {
        on_slider(shell, event, cx);
    });
    shell.hue_slider = Some(HueSlider {
        state: slider,
        _change: change,
    });
    warm_preview(shell, cx);
    warm_track(shell, cx);
    shell.focus_handle.focus(window, cx);
    cx.notify();
}

/// Back from the hue stage to the list, writing nothing. The list keeps its
/// query and highlight, and its field takes the keys again. `false`, changing
/// nothing, off the stage.
fn leave_hue_stage(
    shell: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let Some(Target::ValueColor { stage, .. }) =
        shell.choice_dialog.as_mut().map(|s| &mut s.target)
    else {
        return false;
    };
    if stage.take().is_none() {
        return false;
    }
    shell.hue_slider = None;
    shell
        .dialog_input
        .read(cx)
        .focus_handle(cx)
        .focus(window, cx);
    cx.notify();
    true
}

/// Apply the stage: one inline pick of its hue and tone, closing the list.
/// A hue and tone equal to the color in force, named or inline, close it
/// writing nothing: an inline copy of a name's hue would silently detach
/// the value from the name. Refused while the field is out of range; Apply
/// is disabled then.
fn apply_hue_stage(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(stage) = hue_stage(shell) else {
        return;
    };
    if !stage.valid() {
        return;
    }
    if stage.holds_in_force() {
        shell.close_modal(window, cx);
        return;
    }
    let definition = stage.definition();
    let Some(Target::ValueColor {
        dimension, value, ..
    }) = shell.choice_dialog.as_ref().map(|s| &s.target)
    else {
        return;
    };
    let pick = Pick::ValueColor {
        dimension: dimension.clone(),
        value: value.clone(),
        pick: ColorRow::set(ValuePick::Inline(definition)),
    };
    commit(shell, pick, window, cx);
}

/// The hue stage's key route ([`stage_key`]): every non-chord key is
/// claimed, so nothing types into the list's field beneath.
fn hue_stage_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let Some(key) = stage_key(ks) else {
        return false;
    };
    match key {
        StageKey::Back => {
            let _ = leave_hue_stage(shell, window, cx);
        }
        StageKey::Apply => apply_hue_stage(shell, window, cx),
        StageKey::Swallow => {}
        StageKey::Step(_) | StageKey::Tone | StageKey::Digit(_) | StageKey::Erase => {
            if let Some(stage) = hue_stage_mut(shell) {
                match key {
                    StageKey::Step(delta) => stage.step(delta),
                    StageKey::Tone => stage.toggle_tone(),
                    StageKey::Digit(digit) => stage.digit(digit),
                    _ => stage.erase(),
                }
            }
            let hue = hue_stage(shell).map(|s| s.hue);
            if let (Some(hue), Some(slider)) =
                (hue, shell.hue_slider.as_ref().map(|s| s.state.clone()))
            {
                slider.update(cx, |s, cx| s.set_value(f32::from(hue), window, cx));
            }
            warm_preview(shell, cx);
            if key == StageKey::Tone {
                warm_track(shell, cx);
            }
        }
    }
    cx.notify();
    true
}

/// A pointer press or drag on the slider: the stage follows it.
fn on_slider(shell: &mut ShellView, event: &SliderEvent, cx: &mut Context<ShellView>) {
    let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
    let hue = (value.end().round() as i32).rem_euclid(360) as u16;
    if let Some(stage) = hue_stage_mut(shell) {
        stage.set_hue(hue);
    }
    warm_preview(shell, cx);
    cx.notify();
}

/// A tone option clicked.
fn set_stage_tone(shell: &mut ShellView, tone: Tone, cx: &mut Context<ShellView>) {
    if let Some(stage) = hue_stage_mut(shell) {
        stage.tone = tone;
    }
    warm_preview(shell, cx);
    warm_track(shell, cx);
    cx.notify();
}

/// `definition` resolved under `theme` as a cell paints it, contrast floor
/// included.
fn resolve_hsla(definition: &Definition, theme: &gpui_component::Theme) -> gpui::Hsla {
    super::colours::to_hsla(geode_core::colour::resolve(
        definition,
        &super::colours::anchors_from_theme(theme),
        &super::colours::tokens_from_theme(theme),
    ))
}

/// Slider track stops: every 15° from 0 to 360 inclusive.
const TRACK_STOPS: usize = 25;

/// The track's stops for `tone`, each resolved through the theme like any hue.
fn track_stops(tone: Tone, theme: &gpui_component::Theme) -> Rc<[gpui::Hsla]> {
    let anchors = super::colours::anchors_from_theme(theme);
    let tokens = super::colours::tokens_from_theme(theme);
    (0..TRACK_STOPS)
        .map(|i| {
            super::colours::to_hsla(geode_core::colour::resolve(
                &Definition::hue((i * 15) as f32, tone),
                &anchors,
                &tokens,
            ))
        })
        .collect()
}

/// Resolve the stage's preview in a handler, so the next paint finds it
/// cached: a step or a slider move costs one resolve outside render. A theme
/// change while the stage is open resolves once at the next paint, as the
/// swatches do.
fn warm_preview(shell: &ShellView, cx: &App) {
    if let Some(stage) = hue_stage(shell) {
        let theme = cx.theme();
        stage.cache.preview(
            super::colours::theme_signature(theme),
            stage.hue,
            stage.tone,
            || resolve_hsla(&stage.definition(), theme),
        );
    }
}

/// Resolve the stage's slider track in a handler, so the next paint finds
/// it cached: the stage opening and a tone change each cost one resolve
/// outside render. A theme change while the stage is open resolves once at
/// the next paint, as the swatches do.
fn warm_track(shell: &ShellView, cx: &App) {
    if let Some(stage) = hue_stage(shell) {
        let theme = cx.theme();
        stage
            .cache
            .track(super::colours::theme_signature(theme), stage.tone, || {
                track_stops(stage.tone, theme)
            });
    }
}

/// The hue field's refusal, painted while its text is not a hue.
const HUE_REFUSAL: &str = "a hue is 0\u{2013}360";

const STAGE_HINTS: &[Hint] = &[
    Hint::Key("h"),
    Hint::Key("l"),
    Hint::Text("15\u{b0} \u{b7}"),
    Hint::Key("shift+h"),
    Hint::Key("shift+l"),
    Hint::Text("1\u{b0} \u{b7}"),
    Hint::Key("t"),
    Hint::Text("tone \u{b7}"),
    Hint::Key("enter"),
    Hint::Text("apply \u{b7}"),
    Hint::Key("escape"),
    Hint::Text("back"),
];

/// The hue stage: the preview (the value in its resolved color beside the
/// same text in the foreground, both on the theme background), the slider
/// over its wheel-gradient track, the hue field, the tone options, and Apply
/// and Cancel. Colors come from the stage's per-theme cache.
fn hue_stage_body(
    shell: &ShellView,
    stage: &HueStage,
    entity: &Entity<ShellView>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let signature = super::colours::theme_signature(theme);
    let preview = stage.cache.preview(signature, stage.hue, stage.tone, || {
        resolve_hsla(&stage.definition(), theme)
    });
    let track = stage
        .cache
        .painted_track(signature, stage.tone, || track_stops(stage.tone, theme));
    let valid = stage.valid();
    let sample = |color: gpui::Hsla, selector: &'static str| {
        div()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .text_sm()
            .text_color(color)
            .debug_selector(move || selector.to_string())
            .child(stage.value.clone())
    };
    let segments = track.windows(2).map(|pair| {
        div().flex_1().h_full().bg(gpui::linear_gradient(
            90.,
            gpui::linear_color_stop(pair[0], 0.),
            gpui::linear_color_stop(pair[1], 1.),
        ))
    });
    let slider = div()
        .relative()
        .w_full()
        .h_6()
        .debug_selector(|| "valuecolor-stage-slider".to_string())
        .child(
            h_flex()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .items_center()
                .child(
                    h_flex()
                        .w_full()
                        .h_1p5()
                        .rounded(theme.radius)
                        .overflow_hidden()
                        .children(segments),
                ),
        )
        .children(shell.hue_slider.as_ref().map(|slider| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(Slider::new(&slider.state).bg(theme.transparent))
        }));
    let field = h_flex()
        .gap_2()
        .items_center()
        .child(
            div()
                .font_family(crate::fonts::MONO)
                .text_sm()
                .px_2()
                .py_0p5()
                .rounded(theme.radius)
                .border_1()
                .border_color(if valid { theme.border } else { theme.danger })
                .debug_selector(|| "valuecolor-stage-hue".to_string())
                .child(stage.field.clone()),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("\u{b0}"),
        )
        .when(!valid, |row| {
            row.child(
                div()
                    .text_sm()
                    .text_color(theme.danger)
                    .debug_selector(|| "valuecolor-stage-refusal".to_string())
                    .child(HUE_REFUSAL),
            )
        });
    let tone_entity = entity.clone();
    let tone = div()
        .debug_selector(|| "valuecolor-stage-tone".to_string())
        .child(
            ButtonGroup::new("valuecolor-tone")
                .small()
                .child(
                    Button::new("valuecolor-tone-normal")
                        .label("Normal")
                        .selected(stage.tone == Tone::Normal),
                )
                .child(
                    Button::new("valuecolor-tone-light")
                        .label("Light")
                        .selected(stage.tone == Tone::Light),
                )
                .on_click(move |clicks: &Vec<usize>, _window, cx| {
                    let tone = if clicks.contains(&1) {
                        Tone::Light
                    } else {
                        Tone::Normal
                    };
                    tone_entity.update(cx, |shell, cx| set_stage_tone(shell, tone, cx));
                }),
        );
    let cancel_entity = entity.clone();
    let apply_entity = entity.clone();
    let buttons = h_flex()
        .gap_2()
        .justify_end()
        .child(
            div()
                .debug_selector(|| "valuecolor-cancel".to_string())
                .child(
                    Button::new("valuecolor-cancel")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .on_click(move |_event, window, cx| {
                            cancel_entity.update(cx, |shell, cx| {
                                leave_hue_stage(shell, window, cx);
                            });
                        }),
                ),
        )
        .child(
            div()
                .debug_selector(|| "valuecolor-apply".to_string())
                .child(
                    Button::new("valuecolor-apply")
                        .small()
                        .primary()
                        .label("Apply")
                        .disabled(!valid)
                        .on_click(move |_event, window, cx| {
                            apply_entity.update(cx, |shell, cx| apply_hue_stage(shell, window, cx));
                        }),
                ),
        );
    v_flex()
        .gap_3()
        .w(scale::design(WIDTH))
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .child(sample(preview, "valuecolor-stage-preview"))
                .child(sample(theme.foreground, "valuecolor-stage-plain")),
        )
        .child(slider)
        .child(
            h_flex()
                .gap_4()
                .items_center()
                .justify_between()
                .child(field)
                .child(tone),
        )
        .child(buttons)
        .child(hint_row(
            STAGE_HINTS,
            "valuecolor-stage-hints",
            WIDTH,
            theme.muted_foreground,
            theme.border,
        ))
        .into_any_element()
}

/// Notice for a link pick whose tile closed after the chooser captured it.
pub(super) const TILE_GONE: &str = "that tile is no longer open";

/// Commit through the target's operation. Tile kind closes the modal before calling `add_tile`,
/// so modal focus return precedes occupant creation. A log target replaces
/// the rows without closing; a log level requests the change and closes. A
/// link row closes the modal and goes through the shell's link doors; a
/// tile closed since the open posts [`TILE_GONE`] and links nothing.
fn commit(shell: &mut ShellView, pick: Pick, window: &mut Window, cx: &mut Context<ShellView>) {
    match pick {
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
        Pick::ValueColor {
            dimension,
            value,
            pick,
        } => match pick {
            ColorRow::Set { pick, preset } => {
                shell.close_modal(window, cx);
                shell.set_value_color(dimension, value, pick, preset, cx);
            }
            // The list stays open beneath its own second stage.
            ColorRow::Custom => enter_hue_stage(shell, window, cx),
            // Colors pushes over the list; the list goes once the color is created.
            ColorRow::NewNamed => {
                let seed = shell
                    .choice_dialog
                    .as_ref()
                    .map(ChoiceDialogState::new_color_seed)
                    .unwrap_or_else(|| Definition::hue(f32::from(DEFAULT_HUE), Tone::Normal));
                let name = super::value_color::color_name_for(&value);
                let hook = objectdialog::ColorForValue { dimension, value };
                objectdialog::render::open_new_color(shell, name, seed, hook, window, cx);
            }
        },
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
    if on_hue_stage(shell) {
        return hue_stage_key(shell, ks, window, cx);
    }
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
    if let Target::ValueColor {
        stage: Some(stage), ..
    } = &state.target
    {
        return hue_stage_body(shell, stage, entity, cx);
    }
    let (_, prefix, hints_selector, hints) = chrome(&state.target);
    let loading = matches!(state.target, Target::ActionValue { values: None, .. });
    let (hints_selector, hints) = if loading {
        ("action-loading-hints", ACTION_LOADING_HINTS)
    } else {
        (hints_selector, hints)
    };
    let leads = value_color_leads(&state.target, cx);
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let click_entity = entity.clone();
    let rows = dialog::choice_rows_led(
        &state.list,
        prefix,
        &shell.choice_dialog_scroll,
        theme,
        leads,
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
    let body = if loading {
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

/// The value-color list's leading elements, by declared row: a swatch for
/// each color, resolved from the definition captured at open under the
/// current theme (cached per theme signature, so a theme change while the
/// list is open repaints it and an unchanged theme resolves nothing), and a
/// swatch-wide space for the rows without one so every name starts on the
/// same spine. Empty for every other target, and when no row has a swatch.
/// Bounded by the row count.
fn value_color_leads(target: &Target, cx: &App) -> Vec<Option<AnyElement>> {
    let Target::ValueColor {
        picks,
        swatches,
        resolved,
        ..
    } = target
    else {
        return Vec::new();
    };
    if swatches.iter().all(Option::is_none) {
        return Vec::new();
    }
    let theme = cx.theme();
    let rows = resolved.rows(super::colours::theme_signature(theme), || {
        resolve_swatches(
            swatches,
            picks,
            &super::colours::anchors_from_theme(theme),
            &super::colours::tokens_from_theme(theme),
        )
    });
    rows.iter()
        .map(|row| {
            Some(match row {
                Some((colour, selector)) => dialog::swatch(*colour, selector.clone(), cx),
                None => dialog::swatch_space(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Modifiers, parse_keystroke};
    use geode_core::tile_columns::{TileColumn, TileColumns};

    /// The value-color swatches resolve once per theme: a repaint under the
    /// same theme signature reuses the resolved rows, a changed signature
    /// (a theme change while the list is open) resolves them again.
    #[test]
    fn value_color_swatches_resolve_once_per_theme() {
        let cache = SwatchCache::default();
        let calls = std::cell::Cell::new(0);
        let resolve = |h: f32| {
            calls.set(calls.get() + 1);
            let rows: Rc<[SwatchRow]> =
                Rc::from(vec![Some((gpui::hsla(h, 0.5, 0.5, 1.0), "s".into())), None]);
            rows
        };
        let light = [gpui::Hsla::default(); 28];
        let mut dark = light;
        dark[12] = gpui::hsla(0.0, 0.0, 1.0, 1.0);
        let first = cache.rows(light, || resolve(0.1));
        let again = cache.rows(light, || resolve(0.9));
        assert_eq!(calls.get(), 1, "an unchanged theme resolves nothing");
        assert!(Rc::ptr_eq(&first, &again));
        let changed = cache.rows(dark, || resolve(0.6));
        assert_eq!(calls.get(), 2, "a theme change resolves again");
        assert_eq!(changed[0].as_ref().map(|(c, _)| c.h), Some(0.6));
        assert_eq!(
            SwatchCache::default(),
            cache,
            "a cache is not part of the dialog's identity"
        );
    }

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
        let mut state = ChoiceDialogState::tile_kinds(["blotter", "cvi", "diagnostics"]);
        state.list.set_query("diag");
        assert_eq!(
            state.pick_at_ranked(0),
            Some(Pick::Kind("diagnostics".into()))
        );
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
        let mut state = ChoiceDialogState::tile_kinds(["blotter", "cvi", "diagnostics"]);
        // Lit on `Diagnostics`, the last row.
        assert!(state.list.set_highlighted(2));
        assert!(state.set_query("i"));
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::Kind("diagnostics".into())),
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

    use geode_core::colour::{
        Definition, NamedColours, Tone, ValueColorState, ValueEntry, ValuePick,
    };

    fn named(names: &[&str]) -> NamedColours {
        let mut out = NamedColours::default();
        for n in names {
            out.insert(n.to_string(), Definition::hue(240.0, Tone::Normal));
        }
        out
    }

    fn value_state(desk: Option<&str>, user: Option<&str>) -> ValueColorState {
        let lower = desk.map(ValueEntry::from);
        let user = user.map(ValueEntry::from);
        ValueColorState {
            effective: user
                .clone()
                .or_else(|| lower.clone())
                .filter(|c| !c.is_cleared()),
            user,
            lower,
        }
    }

    #[test]
    fn follow_desk_names_an_inline_desk_entry_by_its_hue() {
        let mut state = value_state(None, Some("blue"));
        state.lower = Some(ValueEntry::Inline(Definition::hue(210.0, Tone::Normal)));
        let list = value_list(&["blue"], &state);
        assert_eq!(
            list.list.options().last().map(String::as_str),
            Some("Follow desk (hue 210)")
        );
    }

    fn value_list(names: &[&str], state: &ValueColorState) -> ChoiceDialogState {
        ChoiceDialogState::value_colors("underlying_ref".into(), "SPX".into(), &named(names), state)
    }

    fn set(pick: ValuePick) -> ColorRow {
        ColorRow::set(pick)
    }

    fn value_pick(state: &ChoiceDialogState, ranked: usize) -> Option<ColorRow> {
        match state.pick_at_ranked(ranked) {
            Some(Pick::ValueColor { pick, .. }) => Some(pick),
            _ => None,
        }
    }

    fn lit(state: &ChoiceDialogState) -> Option<ColorRow> {
        match state.highlighted_pick() {
            Some(Pick::ValueColor { pick, .. }) => Some(pick),
            _ => None,
        }
    }

    fn position_of(state: &ChoiceDialogState, text: &str) -> usize {
        state
            .list
            .options()
            .iter()
            .position(|o| o == text)
            .expect(text)
    }

    #[test]
    fn the_color_list_holds_the_names_first_and_ends_custom_then_none() {
        let state = value_list(&["blue", "amber"], &value_state(None, Some("blue")));
        let options = state.list.options();
        assert_eq!(&options[..2], ["amber", "blue"], "alphabetical first");
        assert_eq!(
            options.last().map(String::as_str),
            Some(NO_COLOR_ROW),
            "None last"
        );
        assert!(
            position_of(&state, CUSTOM_ROW) > 1,
            "Custom… after the named colors, before None"
        );
        assert_eq!(state.title().as_ref(), "Color \u{b7} underlying_ref SPX");
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::ValueColor {
                dimension: "underlying_ref".into(),
                value: "SPX".into(),
                pick: set(ValuePick::Color("blue".into())),
            }),
            "enter on an untouched list changes nothing"
        );
        let none = value_list(&["blue"], &value_state(None, None));
        assert_eq!(lit(&none), Some(set(ValuePick::None)));
        assert_eq!(
            value_pick(&none, position_of(&none, CUSTOM_ROW)),
            Some(ColorRow::Custom)
        );
    }

    #[test]
    fn follow_desk_is_offered_only_over_a_different_desk_entry() {
        let state = value_list(&["blue", "teal"], &value_state(Some("blue"), Some("teal")));
        let options = state.list.options();
        assert_eq!(
            &options[options.len() - 2..],
            [NO_COLOR_ROW, "Follow desk (blue)"]
        );
        assert_eq!(
            value_pick(&state, options.len() - 1),
            Some(set(ValuePick::FollowDesk))
        );
        for (desk, user) in [
            (Some("blue"), None),
            (None, Some("teal")),
            (Some("teal"), Some("teal")),
            // A desk with no color: following it is what None does.
            (Some("none"), Some("teal")),
        ] {
            let state = value_list(&["blue", "teal"], &value_state(desk, user));
            assert_eq!(
                state.list.options().last().map(String::as_str),
                Some(NO_COLOR_ROW),
                "{desk:?} {user:?}"
            );
        }
    }

    #[test]
    fn a_color_named_none_is_still_picked_by_position() {
        let state = value_list(&["None", "blue"], &value_state(None, None));
        assert_eq!(state.list.options()[0], "None");
        assert_eq!(
            value_pick(&state, 0),
            Some(set(ValuePick::Color("None".into())))
        );
        let last = state.list.options().len() - 1;
        assert_eq!(value_pick(&state, last), Some(set(ValuePick::None)));
        // It opens on the no-color row by position, not on the first row
        // spelled like it, so enter on the untouched list still changes
        // nothing.
        assert_eq!(lit(&state), Some(set(ValuePick::None)));
    }

    /// A color may be named `Custom…` (`check_object_name` allows it; it
    /// refuses `preset · red` and `New named color…`, which hold spaces).
    #[test]
    fn a_color_named_custom_is_still_picked_by_position() {
        let state = value_list(
            &["Custom\u{2026}"],
            &value_state(None, Some("Custom\u{2026}")),
        );
        assert_eq!(
            value_pick(&state, 0),
            Some(set(ValuePick::Color("Custom\u{2026}".into())))
        );
        let custom = state
            .list
            .options()
            .iter()
            .rposition(|o| o == CUSTOM_ROW)
            .unwrap();
        assert_ne!(custom, 0);
        assert_eq!(value_pick(&state, custom), Some(ColorRow::Custom));
        assert_eq!(
            lit(&state),
            Some(set(ValuePick::Color("Custom\u{2026}".into()))),
            "the color in force, not the stage row"
        );
    }

    #[test]
    fn a_color_no_longer_defined_opens_the_list_on_none() {
        let state = value_list(&["blue"], &value_state(None, Some("gone")));
        assert_eq!(lit(&state), Some(set(ValuePick::None)));
    }

    #[test]
    fn an_inline_entry_in_force_opens_on_custom() {
        let mut state = value_state(None, None);
        let inline = ValueEntry::Inline(Definition::hue(200.0, Tone::Light));
        state.user = Some(inline.clone());
        state.effective = Some(inline);
        let list = value_list(&["blue"], &state);
        assert_eq!(lit(&list), Some(ColorRow::Custom));
    }

    #[test]
    fn the_presets_follow_the_named_colors_each_picking_its_hue() {
        let state = value_list(&["blue"], &value_state(None, None));
        let options = state.list.options();
        assert_eq!(
            &options[1..13],
            [
                "preset \u{b7} red",
                "preset \u{b7} orange",
                "preset \u{b7} yellow",
                "preset \u{b7} lime",
                "preset \u{b7} green",
                "preset \u{b7} teal",
                "preset \u{b7} cyan",
                "preset \u{b7} azure",
                "preset \u{b7} blue",
                "preset \u{b7} violet",
                "preset \u{b7} magenta",
                "preset \u{b7} rose",
            ]
        );
        assert_eq!(options[13], CUSTOM_ROW);
        assert_eq!(
            value_pick(&state, 8),
            Some(ColorRow::Set {
                pick: ValuePick::Inline(Definition::hue(210.0, Tone::Normal)),
                preset: Some("azure"),
            })
        );
    }

    #[test]
    fn an_inline_preset_in_force_opens_on_its_preset_row() {
        let opened = |definition: Definition| {
            let mut state = value_state(None, None);
            state.user = Some(ValueEntry::Inline(definition.clone()));
            state.effective = Some(ValueEntry::Inline(definition));
            let list = value_list(&["blue"], &state);
            list.list.highlighted_text().map(str::to_string)
        };
        assert_eq!(
            opened(Definition::hue(240.0, Tone::Normal)).as_deref(),
            Some("preset \u{b7} blue")
        );
        assert_eq!(
            opened(Definition::hue(240.0, Tone::Light)).as_deref(),
            Some(CUSTOM_ROW),
            "a light hue is no preset"
        );
        assert_eq!(
            opened(Definition::hue(200.0, Tone::Normal)).as_deref(),
            Some(CUSTOM_ROW)
        );
    }

    #[test]
    fn a_typed_hue_pins_its_row_and_picks_it() {
        let mut state = value_list(&["blue"], &value_state(None, None));
        assert!(state.set_query("210"));
        assert_eq!(state.list.options()[0], "Hue 210");
        assert_eq!(state.list.ranked()[0].row, 0, "pinned on top");
        assert_eq!(
            lit(&state),
            Some(ColorRow::set(ValuePick::Inline(Definition::hue(
                210.0,
                Tone::Normal
            ))))
        );
        state.set_query("21");
        assert_eq!(state.list.options()[0], "Hue 21");
        assert_eq!(
            state
                .list
                .options()
                .iter()
                .filter(|o| o.starts_with("Hue "))
                .count(),
            1,
            "one typed row at a time"
        );
        state.set_query("360");
        assert_eq!(state.list.options()[0], "Hue 0", "360 reads as 0");
        state.set_query("blue");
        assert_eq!(state.list.options()[0], "blue", "no number, no row");
        state.set_query("1e2");
        assert_eq!(state.list.options()[0], "blue");
    }

    /// A lit row the query keeps by text (a color whose name holds the
    /// digits) does not keep the highlight from the typed row.
    #[test]
    fn a_typed_hue_is_lit_over_a_row_kept_by_text() {
        let mut state = value_list(&["b210"], &value_state(None, Some("b210")));
        assert_eq!(state.list.highlighted_text(), Some("b210"));
        assert!(state.set_query("210"));
        assert_eq!(state.list.highlighted_text(), Some("Hue 210"));
        assert_eq!(state.list.ranked()[0].row, 0, "pinned on top");
    }

    /// Enter re-feeds the live text; an unchanged query must not re-pin and
    /// take the highlight back from the row the trader moved to.
    #[test]
    fn a_moved_highlight_under_a_typed_hue_is_kept_at_enter() {
        let mut state = value_list(&["c1"], &value_state(None, None));
        state.set_query("1");
        assert_eq!(state.list.highlighted_text(), Some("Hue 1"));
        state.list.nav(crate::vimnav::NavCommand::Move(1));
        let moved = state.list.highlighted_text().map(str::to_string);
        assert_ne!(moved.as_deref(), Some("Hue 1"));
        assert!(!state.set_query("1"));
        assert_eq!(state.list.highlighted_text().map(str::to_string), moved);
    }

    /// Apply on a stage whose hue and tone equal the color in force (named
    /// or inline) changes nothing: writing them inline would detach the
    /// value from its name.
    #[test]
    fn the_stage_knows_when_it_holds_the_color_in_force() {
        let spx = Definition::hue(210.0, Tone::Normal);
        let mut s = HueStage::start(Some(&spx), "SPX".into());
        assert!(s.holds_in_force(), "untouched");
        s.step(15);
        assert!(!s.holds_in_force(), "stepped away");
        s.step(-15);
        assert!(s.holds_in_force(), "stepped back");
        s.toggle_tone();
        assert!(!s.holds_in_force(), "the tone changed");
        let rounded = HueStage::start(Some(&Definition::hue(12.6, Tone::Normal)), "SPX".into());
        assert!(rounded.holds_in_force(), "as the stage reads it");
        let token = Definition::token(geode_core::colour::Token::Warning);
        assert!(!HueStage::start(Some(&token), "SPX".into()).holds_in_force());
        assert!(!HueStage::start(None, "SPX".into()).holds_in_force());
    }

    #[test]
    fn with_no_named_colors_the_list_begins_with_the_presets() {
        let state = value_list(&[], &value_state(None, None));
        let Target::ValueColor { picks, .. } = &state.target else {
            panic!("a value-color list");
        };
        assert!(!picks.iter().any(|p| matches!(
            p,
            ColorRow::Set {
                pick: ValuePick::Color(_),
                ..
            }
        )));
        let options = state.list.options();
        assert_eq!(options[0], "preset \u{b7} red");
        assert_eq!(&options[12..], [CUSTOM_ROW, NEW_NAMED_ROW, NO_COLOR_ROW]);
    }

    #[test]
    fn new_named_color_sits_between_custom_and_none() {
        let state = value_list(&["blue"], &value_state(None, None));
        let at = position_of(&state, NEW_NAMED_ROW);
        assert_eq!(state.list.options()[at - 1], CUSTOM_ROW);
        assert_eq!(state.list.options()[at + 1], NO_COLOR_ROW);
        assert_eq!(value_pick(&state, at), Some(ColorRow::NewNamed));
    }

    #[test]
    fn the_new_color_seed_is_the_typed_hue_then_the_color_in_force() {
        let none = value_list(&["blue"], &value_state(None, None));
        assert_eq!(none.new_color_seed(), Definition::hue(240.0, Tone::Normal));
        let mut state = value_state(None, None);
        state.user = Some(ValueEntry::Inline(Definition::hue(210.0, Tone::Light)));
        state.effective = state.user.clone();
        let mut inline = value_list(&["blue"], &state);
        assert_eq!(inline.new_color_seed(), Definition::hue(210.0, Tone::Light));
        inline.set_query("90");
        // A digits query filters `New named color…` out by text; it is pinned
        // beneath the typed row so the typed hue can seed it.
        inline.list.nav(crate::vimnav::NavCommand::Move(1));
        assert_eq!(lit(&inline), Some(ColorRow::NewNamed), "reachable");
        assert_eq!(
            inline.new_color_seed(),
            Definition::hue(90.0, Tone::Normal),
            "the typed hue wins"
        );
        let named = value_list(&["blue"], &value_state(None, Some("blue")));
        assert_eq!(
            named.new_color_seed(),
            Definition::hue(240.0, Tone::Normal),
            "a named color seeds its definition (the fixture's hue 240)"
        );
    }

    /// A query cleared after a typed hue lights the row the list opened on
    /// again, so enter on the cleared list is still no change.
    #[test]
    fn a_cleared_query_lights_the_opening_row_again() {
        let mut state = value_state(None, None);
        state.user = Some(ValueEntry::Inline(Definition::hue(0.0, Tone::Normal)));
        state.effective = state.user.clone();
        let mut preset = value_list(&["blue"], &state);
        let opening = preset.list.highlighted_text().map(str::to_string);
        assert_eq!(opening.as_deref(), Some("preset \u{b7} red"));
        assert!(preset.set_query("2"));
        assert_eq!(preset.list.highlighted_text(), Some("Hue 2"));
        assert!(preset.set_query(""));
        assert_eq!(preset.list.highlighted_text().map(str::to_string), opening);
        assert!(preset.set_query("2"));
        assert!(preset.set_query(" "), "a blank query is cleared too");
        assert_eq!(preset.list.highlighted_text().map(str::to_string), opening);

        let mut none = value_list(&["blue"], &value_state(None, None));
        assert!(none.set_query("bl"));
        assert_eq!(none.list.highlighted_text(), Some("blue"));
        assert!(none.set_query(""));
        assert_eq!(none.list.highlighted_text(), Some(NO_COLOR_ROW));
    }

    /// Dropping the typed row drops its pick and swatch with it: the rows
    /// still stand for their picks by position.
    #[test]
    fn a_dropped_typed_row_keeps_the_picks_aligned() {
        let mut state = value_list(&["blue"], &value_state(None, None));
        assert!(state.set_query("210"));
        let second = state.list.ranked()[1].row;
        assert_eq!(
            state.list.options()[second],
            NEW_NAMED_ROW,
            "pinned beneath the typed row"
        );
        assert_eq!(value_pick(&state, 1), Some(ColorRow::NewNamed));
        assert!(state.set_query("blue"));
        assert!(
            !state
                .list
                .ranked()
                .iter()
                .any(|r| state.list.options()[r.row] == NEW_NAMED_ROW),
            "the second pin goes with the typed row"
        );
        let Target::ValueColor {
            picks, swatches, ..
        } = &state.target
        else {
            panic!("a value-color list");
        };
        assert_eq!(picks[0], ColorRow::set(ValuePick::Color("blue".into())));
        assert_eq!(picks.len(), state.list.options().len());
        assert_eq!(swatches.len(), state.list.options().len());
        assert_eq!(swatches[0], Some(Definition::hue(240.0, Tone::Normal)));
    }

    #[test]
    fn a_hue_is_a_whole_number_from_0_to_360() {
        assert_eq!(parse_hue("0"), Some(0));
        assert_eq!(parse_hue("210"), Some(210));
        assert_eq!(
            parse_hue("07"),
            Some(7),
            "a leading zero is still a whole number"
        );
        assert_eq!(parse_hue(" 45 "), Some(45));
        assert_eq!(parse_hue("359"), Some(359));
        assert_eq!(
            parse_hue("360"),
            Some(0),
            "360 reads as 0, as the document does"
        );
        for refused in [
            "", " ", "-5", "1e2", "3600", "361", "400", "21a", "2.5", "+5",
        ] {
            assert_eq!(parse_hue(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn the_stage_starts_from_the_color_in_force() {
        let start = |d: Option<Definition>| {
            let s = HueStage::start(d.as_ref(), "SPX".into());
            (s.hue, s.tone, s.field)
        };
        assert_eq!(
            start(Some(Definition::hue(210.0, Tone::Light))),
            (210, Tone::Light, "210".into())
        );
        assert_eq!(
            start(Some(Definition::hue(12.6, Tone::Normal))),
            (13, Tone::Normal, "13".into())
        );
        assert_eq!(
            start(Some(Definition::token(geode_core::colour::Token::Warning))),
            (240, Tone::Normal, "240".into())
        );
        assert_eq!(start(None), (240, Tone::Normal, "240".into()));
    }

    #[test]
    fn stage_steps_wrap_and_shift_steps_one_degree() {
        let mut s = HueStage::new(0, Tone::Normal, "SPX".into());
        s.step(-15);
        assert_eq!((s.hue, s.field.as_str()), (345, "345"));
        s.step(15);
        s.step(15);
        assert_eq!(s.hue, 15);
        s.step(-1);
        assert_eq!(s.hue, 14);
        let mut s = HueStage::new(350, Tone::Normal, "SPX".into());
        s.step(15);
        assert_eq!(s.hue, 5, "wraps past 359");
    }

    #[test]
    fn stage_digits_replace_then_append_and_refuse_out_of_range() {
        let mut s = HueStage::new(240, Tone::Normal, "SPX".into());
        s.digit('4');
        assert_eq!(
            (s.field.as_str(), s.hue, s.valid()),
            ("4", 4, true),
            "the first digit replaces"
        );
        s.digit('0');
        s.digit('0');
        assert_eq!(s.field, "400");
        assert!(!s.valid(), "out of range is refused");
        assert_eq!(s.hue, 40, "the last valid hue stays in force");
        s.digit('9');
        assert_eq!(s.field, "400", "three digits at most");
        s.erase();
        assert_eq!((s.field.as_str(), s.hue, s.valid()), ("40", 40, true));
        s.erase();
        s.erase();
        // Erasing through `4` set the hue to 4, as typing it did; the empty
        // field leaves that hue in force.
        assert!(!s.valid(), "an empty field is not a hue");
        assert_eq!(s.hue, 4);
        s.step(15);
        assert_eq!(
            (s.field.as_str(), s.valid()),
            ("19", true),
            "a step rewrites the field"
        );
        s.digit('9');
        assert_eq!(s.field, "9", "after a step the next digit replaces");
    }

    #[test]
    fn stage_tone_toggles() {
        let mut s = HueStage::new(240, Tone::Normal, "SPX".into());
        s.toggle_tone();
        assert_eq!(s.tone, Tone::Light);
        assert_eq!(s.definition(), Definition::hue(240.0, Tone::Light));
        s.toggle_tone();
        assert_eq!(s.tone, Tone::Normal);
    }

    #[test]
    fn stage_keys_map_and_leave_chords_to_the_shell() {
        let k = |s: &str| parse_keystroke(s, Modifiers::CTRL).unwrap();
        assert_eq!(stage_key(&k("escape")), Some(StageKey::Back));
        assert_eq!(stage_key(&k("enter")), Some(StageKey::Apply));
        assert_eq!(stage_key(&k("h")), Some(StageKey::Step(-15)));
        assert_eq!(stage_key(&k("left")), Some(StageKey::Step(-15)));
        assert_eq!(stage_key(&k("l")), Some(StageKey::Step(15)));
        assert_eq!(stage_key(&k("right")), Some(StageKey::Step(15)));
        assert_eq!(stage_key(&k("shift+h")), Some(StageKey::Step(-1)));
        assert_eq!(stage_key(&k("shift+right")), Some(StageKey::Step(1)));
        assert_eq!(stage_key(&k("t")), Some(StageKey::Tone));
        assert_eq!(stage_key(&k("7")), Some(StageKey::Digit('7')));
        assert_eq!(stage_key(&k("backspace")), Some(StageKey::Erase));
        assert_eq!(
            stage_key(&k("x")),
            Some(StageKey::Swallow),
            "never typed into the list beneath"
        );
        assert_eq!(stage_key(&k("ctrl+k")), None, "a chord is the shell's");
    }

    /// The track and the preview resolve once per key: a repaint resolves
    /// nothing, a step only the preview, a theme change while the stage is
    /// open both.
    #[test]
    fn hue_stage_track_and_preview_resolve_once_per_theme() {
        let cache = StageCache::default();
        let calls = std::cell::Cell::new(0);
        let count = |color: gpui::Hsla| {
            calls.set(calls.get() + 1);
            color
        };
        let red = gpui::hsla(0.0, 0.5, 0.5, 1.0);
        let light = [gpui::Hsla::default(); 28];
        let mut dark = light;
        dark[12] = gpui::hsla(0.0, 0.0, 1.0, 1.0);
        assert_eq!(cache.preview(light, 210, Tone::Normal, || count(red)), red);
        cache.preview(light, 210, Tone::Normal, || count(red));
        assert_eq!(calls.get(), 1, "an unchanged key resolves nothing");
        cache.preview(light, 225, Tone::Normal, || count(red));
        assert_eq!(calls.get(), 2, "a step resolves the preview");
        cache.preview(dark, 225, Tone::Normal, || count(red));
        assert_eq!(calls.get(), 3, "a theme change resolves it again");

        let tracks = std::cell::Cell::new(0);
        let stops = || {
            tracks.set(tracks.get() + 1);
            Rc::<[gpui::Hsla]>::from(vec![red; TRACK_STOPS])
        };
        let first = cache.track(light, Tone::Normal, stops);
        let again = cache.track(light, Tone::Normal, || {
            tracks.set(tracks.get() + 1);
            Rc::<[gpui::Hsla]>::from(vec![red; TRACK_STOPS])
        });
        assert_eq!(tracks.get(), 1);
        assert!(Rc::ptr_eq(&first, &again));
        cache.track(light, Tone::Light, || {
            tracks.set(tracks.get() + 1);
            Rc::<[gpui::Hsla]>::from(vec![red; TRACK_STOPS])
        });
        assert_eq!(tracks.get(), 2, "a tone change resolves the track");
        assert!(cache.holds_track(light, Tone::Light), "a hit");
        assert!(
            !cache.holds_track(light, Tone::Normal),
            "the other tone misses"
        );
        assert!(
            !cache.holds_track(dark, Tone::Light),
            "another theme misses"
        );
        assert_eq!(
            StageCache::default(),
            cache,
            "a cache is not part of the stage's identity"
        );
    }
}
