//! The Scope dialog's saved group: saved scopes, then saved expressions,
//! each in name order, painted under the current sections on the one
//! screen. `enter` on a scope replaces the lane's scope with it; on an
//! expression it adds or removes the reference. Either commit closes the
//! dialog when the saved door opened it, and otherwise stays on the screen
//! with the cursor on the row (`Opener::closes_on_saved_commit`).
//!
//! `c` names a copy of the row's definition; `d` and `r` ask before
//! deleting the user's own copy or reverting it to the lower layer's, and
//! refuse when the user holds nothing to remove (`prompt::ask_delete`,
//! `prompt::ask_revert`).
//!
//! The rows derive with the current rows under one key (`view::rows_key`): saved
//! scopes and named expressions both bump the frame's config version, and
//! the `applied` tags read the lane's scope. `/` filters them by name and
//! summary; membership only, so the two sections and their name order hold.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, MouseButton, SharedString, Window, div, px};
use gpui_component::{ActiveTheme as _, h_flex};

use geode_core::named::NamedExpressions;
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;

use crate::dialogmode::{self, DialogMode};
use crate::keymap::{Keystroke, Modifiers};

use super::saved::{self, SavedId, SavedKind, SavedRow};
use super::screen::{SEPARATOR_AND_FILTER, Zone};
use super::state::{After, Step};
use super::view::{ScopeDialogState, edit_lane};
use crate::shell::control::{self, PointerStates as _};
use crate::shell::{ShellView, dialog, scale};

const ROW_HEIGHT: f32 = 28.0;
/// The glyph column, as the current rows': scopes leave it empty so names line up.
const GLYPH_WIDTH: f32 = 16.0;
const NAME_WIDTH: f32 = 120.0;
const EXPRESSION_GLYPH: &str = "≡";
const APPLIED_TAG: &str = "applied";

/// Section headers as painted: the copy `Saved scopes`, `Saved expressions`
/// in capitals, as Current paints its own.
const SCOPES_TITLE: &str = "SAVED SCOPES";
const EXPRESSIONS_TITLE: &str = "SAVED EXPRESSIONS";

/// Refusal when the saved scope under the cursor was removed since the rows
/// derived (a reload, another window's write).
pub(crate) const SCOPE_GONE: &str = "that saved scope no longer exists";
/// Refusal when the expression under the cursor was removed since the rows
/// derived.
pub(crate) const EXPRESSION_GONE: &str = "that expression no longer exists";
/// `e` on a scope row: a saved scope is edited by loading and saving over it.
pub(crate) const EDIT_SCOPE: &str = "load it, change it, then save over it (s)";
/// `n` on a scope row: a new saved scope is the current one, saved.
pub(crate) const NEW_SCOPE: &str = "narrow the current scope, then save it (s)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Scopes,
    Expressions,
}

/// Where the cursor can rest: a visible row (its index into `rows`), or a
/// section's empty row. The empty rows are stops so that `n` and `enter`
/// reach a first expression from the keyboard, as a press on the row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Row(usize),
    Empty(Section),
}

/// A stop's identity across a re-derive: a row by its definition, an empty
/// row by its section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StopId {
    Row(SavedId),
    Empty(Section),
}

fn section_of(id: &SavedId) -> Section {
    match id {
        SavedId::Scope(_) => Section::Scopes,
        SavedId::Expression(_) => Section::Expressions,
    }
}

/// One row's painted strings, prepared when the rows derive.
pub(crate) struct SavedDisplay {
    /// The row's element id, from its domain identity (`SavedId`), so a
    /// row keeps its element state when the filter or a re-derive moves it.
    pub key: SharedString,
    pub glyph: Option<&'static str>,
    pub name: SharedString,
    /// A scope's summary, an expression's text, or a broken definition's
    /// reason.
    pub detail: SharedString,
    /// The detail is expression text: painted monospace.
    pub mono: bool,
    pub applied: bool,
    pub broken: bool,
}

impl SavedDisplay {
    fn of(row: &SavedRow) -> Self {
        let key: SharedString = match &row.id {
            SavedId::Scope(name) => format!("scope:{name}"),
            SavedId::Expression(name) => format!("expr:{name}"),
        }
        .into();
        match &row.kind {
            SavedKind::Scope { summary } => SavedDisplay {
                key,
                glyph: None,
                name: row.name.clone().into(),
                detail: summary.clone().into(),
                mono: false,
                applied: false,
                broken: false,
            },
            SavedKind::Expression {
                text,
                applied,
                broken,
            } => SavedDisplay {
                key,
                glyph: Some(EXPRESSION_GLYPH),
                name: row.name.clone().into(),
                detail: broken.clone().unwrap_or_else(|| text.clone()).into(),
                mono: broken.is_none(),
                applied: *applied,
                broken: broken.is_some(),
            },
        }
    }
}

/// The saved group's state inside the Scope dialog. `visible` indexes
/// `rows` in row order; `stops` is what paints, section by section (the
/// visible rows, or an empty section's row); `cursor` indexes `stops`.
pub(crate) struct SavedScreen {
    pub rows: Vec<SavedRow>,
    pub visible: Vec<usize>,
    pub stops: Vec<Stop>,
    pub cursor: usize,
    /// The cursor's stop, so a re-derive or a re-filter keeps it there.
    pub cursor_id: Option<StopId>,
    pub mode: DialogMode,
    /// The filter: the source of truth `sync_dialog_text` mirrors.
    pub query: String,
    /// The query when filter mode was entered, restored by `escape`.
    pub entry_query: String,
    /// `rows`' painted strings, index for index.
    pub display: Vec<SavedDisplay>,
    /// The key the rows were derived at (`view::rows_key`).
    pub key: (u64, u64),
    /// Each row's searchable text: name, then summary or expression text.
    texts: Vec<String>,
}

impl SavedScreen {
    pub(crate) fn new() -> Self {
        SavedScreen {
            rows: Vec::new(),
            visible: Vec::new(),
            stops: Vec::new(),
            cursor: 0,
            cursor_id: None,
            mode: DialogMode::Normal,
            query: String::new(),
            entry_query: String::new(),
            display: Vec::new(),
            // Never a real key: the first refresh always derives.
            key: (u64::MAX, u64::MAX),
            texts: Vec::new(),
        }
    }

    pub(crate) fn refresh(
        &mut self,
        saved: &SavedScopes,
        named: &NamedExpressions,
        scope: &Scope,
        key: (u64, u64),
    ) {
        self.rows = saved::saved_rows(saved, named, scope);
        self.display = self.rows.iter().map(SavedDisplay::of).collect();
        self.texts = self
            .rows
            .iter()
            .map(|row| match &row.kind {
                SavedKind::Scope { summary } => format!("{} {summary}", row.name),
                SavedKind::Expression { text, .. } => format!("{} {text}", row.name),
            })
            .collect();
        self.key = key;
        self.refilter();
    }

    pub(crate) fn set_query(&mut self, query: &str) {
        if self.query != query {
            self.query = query.to_string();
            self.refilter();
        }
    }

    /// Narrow `visible` to the rows matching `query`. Ranking decides
    /// membership only: the rows go back into row order, so the scopes stay
    /// above the expressions and each section stays in name order.
    fn refilter(&mut self) {
        let mut matched = crate::listfilter::rank(&self.texts, &self.query);
        matched.sort_by_key(|m| m.row);
        self.visible = matched.into_iter().map(|m| m.row).collect();
        // A section emptied by the filter has no stop; one with no
        // definitions at all has its empty row, a stop only while no filter
        // narrows the list: a filter that matches nothing leaves `enter`
        // nothing to act on.
        let mut stops = Vec::with_capacity(self.visible.len() + 2);
        for section in [Section::Scopes, Section::Expressions] {
            if self.section_is_empty(section) {
                if self.query.is_empty() {
                    stops.push(Stop::Empty(section));
                }
            } else {
                stops.extend(
                    self.visible
                        .iter()
                        .filter(|&&i| section_of(&self.rows[i].id) == section)
                        .map(|&i| Stop::Row(i)),
                );
            }
        }
        self.stops = stops;
        let kept = self.cursor_id.as_ref().and_then(|id| {
            self.stops
                .iter()
                .position(|&stop| &self.stop_id(stop) == id)
        });
        self.cursor = kept.unwrap_or_else(|| self.cursor.min(self.stops.len().saturating_sub(1)));
        self.cursor_id = self.cursor_stop().map(|stop| self.stop_id(stop));
    }

    fn stop_id(&self, stop: Stop) -> StopId {
        match stop {
            Stop::Row(i) => StopId::Row(self.rows[i].id.clone()),
            Stop::Empty(section) => StopId::Empty(section),
        }
    }

    pub(crate) fn cursor_stop(&self) -> Option<Stop> {
        self.stops.get(self.cursor).copied()
    }

    pub(crate) fn cursor_row(&self) -> Option<&SavedRow> {
        match self.cursor_stop()? {
            Stop::Row(i) => Some(&self.rows[i]),
            Stop::Empty(_) => None,
        }
    }

    fn move_cursor(&mut self, cmd: crate::vimnav::NavCommand) {
        self.cursor = crate::vimnav::apply(self.cursor, self.stops.len(), cmd);
        self.cursor_id = self.cursor_stop().map(|stop| self.stop_id(stop));
    }

    pub(crate) fn stop_count(&self) -> usize {
        self.stops.len()
    }

    /// Rest the cursor on stop `at` (clamped) and take its identity.
    pub(crate) fn set_cursor(&mut self, at: usize) {
        self.cursor = at.min(self.stops.len().saturating_sub(1));
        self.cursor_id = self.cursor_stop().map(|stop| self.stop_id(stop));
    }

    fn stop_section(&self, stop: Stop) -> Section {
        match stop {
            Stop::Row(i) => section_of(&self.rows[i].id),
            Stop::Empty(section) => section,
        }
    }

    pub(crate) fn cursor_section(&self) -> Option<Section> {
        self.cursor_stop().map(|stop| self.stop_section(stop))
    }

    /// Put the cursor on `section`'s row named `prefer` when it is a stop,
    /// else on the section's first stop. False, with the cursor unmoved, when
    /// the section has no stop (a filter hid every row).
    pub(crate) fn jump_to(&mut self, section: Section, prefer: Option<&str>) -> bool {
        let named = prefer.and_then(|name| {
            self.stops.iter().position(|&stop| {
                matches!(stop, Stop::Row(i)
                    if section_of(&self.rows[i].id) == section && self.rows[i].name == name)
            })
        });
        let first = || {
            self.stops
                .iter()
                .position(|&stop| self.stop_section(stop) == section)
        };
        match named.or_else(first) {
            Some(at) => {
                self.set_cursor(at);
                true
            }
            None => false,
        }
    }

    /// Whether `section` has a stop for the cursor (a filter can hide every
    /// row, and its empty row is no stop under a filter).
    pub(crate) fn has_stop(&self, section: Section) -> bool {
        self.stops
            .iter()
            .any(|&stop| self.stop_section(stop) == section)
    }

    /// Children each section paints under its header: its stops, or its
    /// empty row (painted under a filter too, though not a stop there).
    pub(crate) fn painted_counts(&self) -> [usize; 2] {
        [Section::Scopes, Section::Expressions].map(|section| {
            if self.section_is_empty(section) {
                1
            } else {
                self.stops
                    .iter()
                    .filter(|&&stop| self.stop_section(stop) == section)
                    .count()
            }
        })
    }

    /// The cursor's section index (Scopes 0, Expressions 1) and its index
    /// among that section's painted children.
    pub(crate) fn cursor_within(&self) -> Option<(usize, usize)> {
        let stop = self.cursor_stop()?;
        let section = self.stop_section(stop);
        let within = self.stops[..self.cursor]
            .iter()
            .filter(|&&s| self.stop_section(s) == section)
            .count();
        Some((usize::from(section == Section::Expressions), within))
    }

    fn section_is_empty(&self, section: Section) -> bool {
        !self.rows.iter().any(|r| section_of(&r.id) == section)
    }
}

/// Whether the shared input is the saved group's filter right now.
pub(crate) fn filtering(state: &ScopeDialogState) -> bool {
    state.on_screen() && state.saved.mode == DialogMode::Filter
}

/// Enter the filter (`/`, or the frozen filter row's press): the cursor
/// moves into the saved group, which the filter narrows. A no-op under a
/// step or while already filtering.
pub(crate) fn enter_filter(state: &mut ScopeDialogState) {
    if !state.on_screen() || state.saved.mode == DialogMode::Filter {
        return;
    }
    let saved = &mut state.saved;
    dialogmode::enter_filter(&mut saved.mode, &mut saved.entry_query, &saved.query);
    state.rest_in(Zone::Saved);
    if state.saved.stop_count() > 0 && state.saved.cursor >= state.saved.stop_count() {
        state.saved.set_cursor(0);
    }
}

/// Keys while the filter owns the input: `escape` restores the entry query,
/// bare `enter` keeps the typed one, and neither commits a row. Arrows move;
/// every other key is the field's.
pub(super) fn filter_key(saved: &mut SavedScreen, ks: &Keystroke) -> bool {
    if let Some(exit) = dialogmode::filter_exit(ks) {
        if dialogmode::exit_filter(&mut saved.mode, &saved.entry_query, &mut saved.query, exit) {
            saved.refilter();
        }
        return true;
    }
    if let Some(cmd) = crate::listfilter::nav_command(ks) {
        saved.move_cursor(cmd);
        return true;
    }
    // Tab would type a literal tab into the filter.
    ks.key == "tab"
        && (ks.mods == Modifiers::NONE
            || ks.mods
                == (Modifiers {
                    shift: true,
                    ..Modifiers::NONE
                }))
}

/// A row verb on the saved group's cursor stop: `enter` loads or toggles
/// (or, on an empty row, starts its section's first definition), `e` edits
/// an expression's definition, `n` names a new one, `c` copies, `d` and `r`
/// ask. Off a definition (an empty row) `c`/`d`/`r` are declined.
pub(super) fn row_key(
    shell: &mut ShellView,
    key: &str,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return false;
    };
    // A verb on a saved row is the user's use of the saved group: a cursor
    // only forced here by an empty Current stays, so a load from `mod+o`
    // keeps it on the row it loaded.
    state.rest_in(Zone::Saved);
    // As on the current rows: every claimed key drops the last refusal; an
    // unclaimed key puts it back.
    let prior_error = state.error.take();
    let stop = state.saved.cursor_stop();
    let cursor = state.saved.cursor_row().map(|r| r.id.clone());
    // The empty Scopes row stands for the scopes section: its verbs give
    // the scope rows' guidance.
    let on_scope =
        matches!(cursor, Some(SavedId::Scope(_))) || stop == Some(Stop::Empty(Section::Scopes));
    match key {
        "e" if on_scope => state.error = Some(EDIT_SCOPE.into()),
        "n" if on_scope => state.error = Some(NEW_SCOPE.into()),
        // On an expression row `e` edits its definition; `n` anywhere off a
        // scope row (an expression, or no row at all) starts a new one.
        "e" => {
            let Some(SavedId::Expression(name)) = cursor else {
                state.error = prior_error;
                return false;
            };
            super::definition::push(shell, Some(name), window, cx);
            return true;
        }
        "n" => {
            super::definition::push(shell, None, window, cx);
            return true;
        }
        // Copy, delete and revert act on a definition: an empty row has none.
        "c" | "d" | "r" => {
            let Some(row) = state.saved.cursor_row().cloned() else {
                state.error = prior_error;
                return false;
            };
            row_verb(shell, key, row, window, cx);
            return true;
        }
        // An empty row has nothing to load or toggle: `enter` there is the
        // section's way to make a first one.
        "enter" if stop == Some(Stop::Empty(Section::Scopes)) => {
            state.error = Some(NEW_SCOPE.into());
        }
        "enter" if stop == Some(Stop::Empty(Section::Expressions)) => {
            super::definition::push(shell, None, window, cx);
            return true;
        }
        "enter" => {
            commit_cursor(shell, window, cx);
            return true;
        }
        _ => {
            state.error = prior_error;
            return false;
        }
    }
    shell.refresh_dialog_rows(cx);
    cx.notify();
    true
}

/// `c` pushes the copy prompt for `row`; `d` and `r` ask before deleting or
/// reverting it, or refuse into the error line when there is nothing of the
/// user's to remove. The input is synced either way: a question takes its
/// focus, the prompt gives it back.
fn row_verb(
    shell: &mut ShellView,
    key: &str,
    row: SavedRow,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let refused = match key {
        "c" => {
            if let Some(state) = shell.scope_dialog.as_mut() {
                state.prompt = Some(super::prompt::copy_prompt(&row));
                state.push_step(Step::CopyName { from: row.id });
            }
            Ok(())
        }
        "d" => super::prompt::ask_delete(shell, row.id, cx),
        _ => super::prompt::ask_revert(shell, row.id, cx),
    };
    if let (Err(refusal), Some(state)) = (refused, shell.scope_dialog.as_mut()) {
        state.error = Some(refusal);
    }
    shell.refresh_dialog_rows(cx);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Load the cursor's scope or toggle the cursor's expression. The saved
/// door's dialog closes with it; any other door's stays on the screen. A
/// definition removed since the rows derived refuses here and the screen
/// re-derives, so the next key sees the rows as they now are.
fn commit_cursor(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(id) = shell
        .scope_dialog
        .as_ref()
        .and_then(|s| s.saved.cursor_row().map(|r| r.id.clone()))
    else {
        shell.refresh_dialog_rows(cx);
        cx.notify();
        return;
    };
    let refused = match &id {
        SavedId::Scope(name) => shell.load_saved_scope(name, cx).err().map(|_| SCOPE_GONE),
        SavedId::Expression(name) => {
            let defined = shell
                .target_frame(cx)
                .read(cx)
                .named_expressions()
                .get(name)
                .is_some();
            if defined {
                edit_lane(shell, cx, |f| f.toggle_named(name));
                None
            } else {
                Some(EXPRESSION_GONE)
            }
        }
    };
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    if let Some(refusal) = refused {
        state.error = Some(refusal.into());
        shell.refresh_dialog_rows(cx);
        cx.notify();
        return;
    }
    if state.opener.closes_on_saved_commit() {
        shell.close_modal(window, cx);
        cx.notify();
        return;
    }
    // From `mod+o` the screen stays: the current sections show the change,
    // and the cursor stays on the saved row by identity.
    shell.refresh_dialog_rows(cx);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Show the screen a step left for, or close the dialog when the step was
/// the bottom layer.
pub(super) fn finish(
    shell: &mut ShellView,
    after: After,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    match after {
        After::Close => shell.close_modal(window, cx),
        After::Show => {
            shell.refresh_dialog_rows(cx);
            dialog::sync_dialog_text(shell, window, cx);
        }
    }
    cx.notify();
}

/// A row press moves the cursor onto the row; the second press of a
/// double-click is `enter`. Ignored unless the screen is the top layer: a
/// step pushed over it owns the pointer.
fn press_row(
    shell: &mut ShellView,
    at: usize,
    click_count: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    // A question up over the screen owns the pointer too.
    if !state.on_screen() || state.pending.is_some() {
        return;
    }
    state.rest_in(Zone::Saved);
    state.saved.set_cursor(at);
    state.error = None;
    // Exactly 2, so a triple-click does not commit twice.
    if click_count == 2 {
        commit_cursor(shell, window, cx);
        return;
    }
    cx.notify();
}

/// The empty Expressions row's press: a new expression, as `n` opens.
/// Ignored unless the screen is on top with no question up and no filter
/// live (`view::takes_pointer`): while filtering the field owns the
/// keyboard, and a step opened here would cover the typing.
fn press_new_expression(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if !shell
        .scope_dialog
        .as_ref()
        .is_some_and(super::view::takes_pointer)
    {
        return;
    }
    super::definition::push(shell, None, window, cx);
}

struct SectionNames {
    title: &'static str,
    note: &'static str,
    empty: &'static str,
    slug: &'static str,
}

fn section_names(section: Section) -> SectionNames {
    match section {
        Section::Scopes => SectionNames {
            title: SCOPES_TITLE,
            note: saved::SCOPES_NOTE,
            empty: saved::NO_SCOPES,
            slug: "scopes",
        },
        Section::Expressions => SectionNames {
            title: EXPRESSIONS_TITLE,
            note: saved::EXPRESSIONS_NOTE,
            empty: saved::NO_EXPRESSIONS,
            slug: "expressions",
        },
    }
}

/// The saved group, painted under the current sections: a hairline
/// separator, the filter row (frozen in Normal mode, live in Filter), then
/// the saved scopes and the saved expressions. The order is
/// `screen::child_index`'s: it must change with it. Each element is a
/// direct child of the screen's scroll list, so the list can scroll to any
/// of them; a section the filter emptied paints its header alone, an empty
/// section its header and empty row (`SavedScreen::painted_counts`).
pub(super) fn group(
    shell: &ShellView,
    state: &ScopeDialogState,
    entity: &Entity<ShellView>,
    cx: &App,
) -> Vec<AnyElement> {
    let screen = &state.saved;
    let theme = cx.theme();
    let paint = crate::shell::listrow::row_paint(theme);
    let muted = theme.muted_foreground;
    let danger = theme.danger;
    let radius = theme.radius;
    let tag_radius = theme.radius_tokens().sm;
    let border = theme.border;
    let mono = crate::fonts::MONO;
    // The clickable empty row is a muted label on the modal panel, as
    // the current sections' are.
    let empty_states = control::paint(theme, control::Rest::Bare, theme.popover, muted);

    // The filter row: live while filtering, frozen (a press enters filter
    // mode) otherwise.
    let frozen = (screen.mode == DialogMode::Normal).then(|| dialog::FrozenFilter {
        query: &screen.query,
        slash_filters: true,
        entity: entity.clone(),
    });
    // The highlight follows the cursor's group: a saved stop shows it only
    // while the cursor rests in this group.
    let cursor = (state.zone == Zone::Saved).then_some(screen.cursor);
    // The separator and filter, two headers, and at most one child per stop
    // or empty row.
    let mut list: Vec<AnyElement> =
        Vec::with_capacity(SEPARATOR_AND_FILTER + 4 + screen.stops.len());
    // A hairline: one pixel, not a size the rem scale owns. `scope-saved`
    // marks the group as painted: the separator paints exactly when it does.
    list.push(
        div()
            .flex_shrink_0()
            .h(px(1.))
            .w_full()
            .bg(border)
            .debug_selector(|| "scope-saved".to_string())
            .into_any_element(),
    );
    list.push(
        div()
            .flex_shrink_0()
            .debug_selector(|| "scope-saved-filter".to_string())
            .child(dialog::filter_row(&shell.dialog_input, frozen, cx))
            .into_any_element(),
    );
    let mut row_i = 0usize;
    for section in [Section::Scopes, Section::Expressions] {
        let names = section_names(section);
        let slug = names.slug;
        list.push(
            h_flex()
                .flex_shrink_0()
                .px_2()
                .pt_2()
                .pb_0p5()
                .justify_between()
                .items_center()
                .text_xs()
                .text_color(muted)
                .debug_selector(move || format!("scope-saved-section-{slug}"))
                .child(names.title)
                .child(
                    div()
                        .debug_selector(move || format!("scope-saved-note-{slug}"))
                        .child(names.note),
                )
                .into_any_element(),
        );
        if screen.section_is_empty(section) {
            let highlighted =
                cursor.and_then(|c| screen.stops.get(c).copied()) == Some(Stop::Empty(section));
            let empty = div()
                .flex_shrink_0()
                .px_2()
                .h(scale::design(ROW_HEIGHT))
                .flex()
                .items_center()
                .text_sm()
                .rounded(radius)
                .text_color(muted)
                .debug_selector(move || format!("scope-saved-empty-{slug}"))
                .child(names.empty);
            // A press on the Expressions one starts a new expression,
            // as `n` and `enter` there do; the Scopes one is a cursor
            // stop only.
            list.push(if section == Section::Expressions {
                let click = entity.clone();
                let el = empty.id("scope-saved-empty-expressions").on_mouse_down(
                    MouseButton::Left,
                    move |_event, window, cx| {
                        click.update(cx, |shell, cx| press_new_expression(shell, window, cx));
                    },
                );
                if highlighted {
                    crate::shell::listrow::paint_row(el, paint, true).into_any_element()
                } else {
                    el.pointer_states(empty_states).into_any_element()
                }
            } else {
                let el = empty.id("scope-saved-empty-scopes");
                if highlighted {
                    crate::shell::listrow::paint_row(el, paint, true).into_any_element()
                } else {
                    el.into_any_element()
                }
            });
            continue;
        }
        for (s, &stop) in screen.stops.iter().enumerate() {
            let row_ix = match stop {
                Stop::Row(row_ix) if section_of(&screen.rows[row_ix].id) == section => row_ix,
                _ => continue,
            };
            // Selectors index `visible`: rows are stopped in visible order.
            let i = row_i;
            row_i += 1;
            let shown = &screen.display[row_ix];
            let el = h_flex()
                .id(gpui::ElementId::Name(screen.display[row_ix].key.clone()))
                .w_full()
                .h(scale::design(ROW_HEIGHT))
                .flex_shrink_0()
                .px_2()
                .items_center()
                .gap_2()
                .text_sm()
                .rounded(radius)
                .debug_selector(move || format!("scope-saved-row-{i}"));
            let mut el = crate::shell::listrow::paint_row(el, paint, cursor == Some(s));
            let detail_color = if shown.broken { danger } else { muted };
            el = el
                .child(
                    div()
                        .w(scale::design(GLYPH_WIDTH))
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .text_color(detail_color)
                        .when_some(shown.glyph, |el, glyph| el.child(glyph)),
                )
                .child(
                    div()
                        .w(scale::design(NAME_WIDTH))
                        .flex_shrink_0()
                        .truncate()
                        .when(shown.broken, |el| el.text_color(danger))
                        .child(shown.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .text_xs()
                        .text_color(detail_color)
                        .when(shown.mono, |el| el.font_family(mono))
                        .child(shown.detail.clone()),
                )
                .when(shown.applied, |el| {
                    el.child(
                        div()
                            .flex_shrink_0()
                            .px_1()
                            .text_xs()
                            .text_color(muted)
                            .border_1()
                            .border_color(border)
                            .rounded(tag_radius)
                            .debug_selector(move || format!("scope-saved-tag-applied-{i}"))
                            .child(APPLIED_TAG),
                    )
                });
            let click = entity.clone();
            el = el.on_mouse_down(MouseButton::Left, move |event, window, cx| {
                click.update(cx, |shell, cx| {
                    press_row(shell, s, event.click_count, window, cx)
                });
            });
            list.push(el.into_any_element());
        }
    }
    list
}

/// The saved scopes' config doc, as `Config::layered_docs` keys it.
pub(super) const SCOPES_DOC: &str = "scopes";

/// Who ticks the named expression `name`, as the sentence the delete question
/// carries: saved scopes in name order, then the frame. `None` when nothing
/// does. Scopes are read off the raw pending-aware doc, not the reader, so a
/// scope the reader drops for some other fault still counts as a user.
/// Deleting never edits these users; the sentence is what says so.
pub(crate) fn named_expression_users(shell: &ShellView, name: &str, cx: &App) -> Option<String> {
    let pending = crate::shell::objectdialog::apply::config_with_pending(shell, cx);
    let config = pending.as_ref().unwrap_or(&shell.services(cx).config);
    let users: Vec<String> = config
        .doc(SCOPES_DOC)
        .map(|doc| {
            doc.value
                .iter()
                .filter(|(_, scope)| {
                    scope
                        .get("named")
                        .and_then(|v| v.as_array())
                        .is_some_and(|names| names.iter().any(|n| n.as_str() == Some(name)))
                })
                .map(|(scope, _)| scope.clone())
                .collect()
        })
        .unwrap_or_default();
    let frame = shell
        .target_frame(cx)
        .read(cx)
        .scope()
        .named
        .iter()
        .any(|n| n == name);
    super::saved::used_by_sentence(users, frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::scope::Scope;

    fn named(text: &str) -> NamedExpressions {
        use geode_core::config::{EXPRESSIONS_DOC, LayerDoc, merge_docs};
        let doc = LayerDoc::builtin(EXPRESSIONS_DOC, text).unwrap();
        NamedExpressions::from_doc(
            &merge_docs(EXPRESSIONS_DOC, &[doc]),
            &geode_core::scope::complete::ExprVocab::default(),
        )
        .0
    }

    /// Scopes `asia`, `eu`, `us`; expressions `big`, `liq`.
    fn screen() -> SavedScreen {
        let mut saved = SavedScopes::new();
        for (name, book) in [("asia", "BK2"), ("eu", "BK1"), ("us", "BK3")] {
            saved.insert(name.into(), Scope::one("book", book));
        }
        let defs = named("[liq]\nexpression = \"npv > 0\"\n[big]\nexpression = \"npv > 100\"\n");
        let mut s = SavedScreen::new();
        s.refresh(&saved, &defs, &Scope::default(), (1, 1));
        s
    }

    fn cursor_name(s: &SavedScreen) -> Option<String> {
        s.cursor_row().map(|r| r.name.clone())
    }

    #[test]
    fn jump_prefers_the_named_scope_else_the_section_start() {
        let mut s = screen();
        assert!(s.jump_to(Section::Scopes, Some("eu")));
        assert_eq!(cursor_name(&s).as_deref(), Some("eu"));
        assert!(s.jump_to(Section::Scopes, Some("gone")));
        assert_eq!(cursor_name(&s).as_deref(), Some("asia"));
        assert!(s.jump_to(Section::Expressions, None));
        assert_eq!(cursor_name(&s).as_deref(), Some("big"));
        assert_eq!(s.cursor_section(), Some(Section::Expressions));
    }

    #[test]
    fn jump_into_a_section_the_filter_emptied_refuses_and_keeps_the_cursor() {
        let mut s = screen();
        s.set_query("liq");
        assert!(s.jump_to(Section::Expressions, None));
        assert!(!s.jump_to(Section::Scopes, None));
        assert_eq!(cursor_name(&s).as_deref(), Some("liq"));
    }

    #[test]
    fn painted_counts_and_cursor_within_follow_the_filter() {
        let mut s = screen();
        assert_eq!(s.painted_counts(), [3, 2]);
        assert_eq!(s.stop_count(), 5);
        s.set_cursor(4);
        assert_eq!(s.cursor_within(), Some((1, 1)));
        // Only `us` holds a `u` followed by an `s` in its name and summary.
        s.set_query("us");
        assert_eq!(s.painted_counts(), [1, 0]);
        assert_eq!(s.cursor_within(), Some((0, 0)));
    }

    #[test]
    fn an_empty_section_counts_its_empty_row_even_under_a_filter() {
        let defs = named("");
        let mut s = SavedScreen::new();
        s.refresh(&SavedScopes::new(), &defs, &Scope::default(), (1, 1));
        assert_eq!(s.painted_counts(), [1, 1]);
        assert_eq!(s.stop_count(), 2, "both empty rows are stops unfiltered");
        s.set_query("x");
        assert_eq!(s.painted_counts(), [1, 1], "still painted");
        assert_eq!(s.stop_count(), 0, "but no longer stops");
        assert_eq!(s.cursor_within(), None);
    }
}
