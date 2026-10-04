//! The Scope dialog's Saved screen: saved scopes, then saved expressions,
//! each in name order. `enter` on a scope replaces the lane's scope with it;
//! on an expression it adds or removes the reference. Either commit leaves
//! the screen — back to Current when Saved was entered from there, else the
//! dialog closes (`Layers::commit_saved_row`).
//!
//! The rows derive with Current's under one key (`view::rows_key`): saved
//! scopes and named expressions both bump the frame's config version, and
//! the `applied` tags read the lane's scope. `/` filters them by name and
//! summary; membership only, so the two sections and their name order hold.

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, MouseButton, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use geode_core::named::NamedExpressions;
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;

use crate::dialogmode::{self, DialogMode};
use crate::footer::{Hint, HintRow};
use crate::keymap::{Keystroke, Modifiers};

use super::saved::{self, SavedId, SavedKind, SavedRow};
use super::state::{After, Layer};
use super::view::{ScopeDialogState, edit_lane};
use crate::shell::{ShellView, dialog, scale};

const ROW_HEIGHT: f32 = 28.0;
/// The glyph column, as Current's: scopes leave it empty so names line up.
const GLYPH_WIDTH: f32 = 16.0;
const NAME_WIDTH: f32 = 120.0;
const EXPRESSION_GLYPH: &str = "≡";
const APPLIED_TAG: &str = "applied";

/// Section headers as painted: the copy `Scopes`, `Expressions` in capitals,
/// as Current paints its own.
const SCOPES_TITLE: &str = "SCOPES";
const EXPRESSIONS_TITLE: &str = "EXPRESSIONS";

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
enum Section {
    Scopes,
    Expressions,
}

fn section_of(id: &SavedId) -> Section {
    match id {
        SavedId::Scope(_) => Section::Scopes,
        SavedId::Expression(_) => Section::Expressions,
    }
}

/// One row's painted strings, prepared when the rows derive.
pub(crate) struct SavedDisplay {
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
        match &row.kind {
            SavedKind::Scope { summary } => SavedDisplay {
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

/// The Saved screen's state inside the Scope dialog. `visible` indexes
/// `rows` in row order; `cursor` indexes `visible`.
pub(crate) struct SavedScreen {
    pub rows: Vec<SavedRow>,
    pub visible: Vec<usize>,
    pub cursor: usize,
    /// The cursor's row, so a re-derive or a re-filter keeps it there.
    pub cursor_id: Option<SavedId>,
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

    /// A fresh visit: no filter, the cursor on the first row.
    pub(crate) fn reset(&mut self) {
        self.mode = DialogMode::Normal;
        self.query.clear();
        self.entry_query.clear();
        self.cursor = 0;
        self.cursor_id = None;
        self.refilter();
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
        let kept = self
            .cursor_id
            .as_ref()
            .and_then(|id| self.visible.iter().position(|&i| &self.rows[i].id == id));
        self.cursor = kept.unwrap_or_else(|| self.cursor.min(self.visible.len().saturating_sub(1)));
        self.cursor_id = self.cursor_row().map(|r| r.id.clone());
    }

    pub(crate) fn cursor_row(&self) -> Option<&SavedRow> {
        self.visible.get(self.cursor).map(|&i| &self.rows[i])
    }

    fn move_cursor(&mut self, cmd: crate::vimnav::NavCommand) {
        self.cursor = crate::vimnav::apply(self.cursor, self.visible.len(), cmd);
        self.cursor_id = self.cursor_row().map(|r| r.id.clone());
    }

    fn section_is_empty(&self, section: Section) -> bool {
        !self.rows.iter().any(|r| section_of(&r.id) == section)
    }
}

pub(crate) fn in_saved(state: &ScopeDialogState) -> bool {
    matches!(state.layers.top(), Layer::Saved)
}

/// Whether the shared input is the Saved screen's filter right now.
pub(crate) fn filtering(state: &ScopeDialogState) -> bool {
    in_saved(state) && state.saved.mode == DialogMode::Filter
}

/// Push the Saved screen over Current for a fresh visit.
pub(crate) fn push(state: &mut ScopeDialogState) {
    debug_assert_eq!(state.layers.top(), &Layer::Current);
    state.error = None;
    state.saved.reset();
    state.layers.push(Layer::Saved);
}

/// Whether the title row's Back button leads anywhere: only while Saved is
/// on top with Current beneath it. Saved opened alone has no screen to go
/// back to; its close button is the way out.
pub(crate) fn back_available(shell: &ShellView) -> bool {
    shell
        .scope_dialog
        .as_ref()
        .is_some_and(|s| in_saved(s) && s.layers.depth() > 1 && s.pending.is_none())
}

/// The Back button's step: what `escape` does from here, all its rungs at
/// once — a filter is left (reverted) and the screen leaves for Current.
/// Does nothing when the button is no longer available, since a click can
/// land after the state changed under it.
pub(crate) fn back(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if !back_available(shell) {
        return;
    }
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    state.error = None;
    let saved = &mut state.saved;
    if saved.mode == DialogMode::Filter {
        dialogmode::exit_filter(
            &mut saved.mode,
            &saved.entry_query,
            &mut saved.query,
            dialogmode::FilterExit::Revert,
        );
        saved.refilter();
    }
    let after = state.layers.escape();
    finish(shell, after, window, cx);
}

/// The pointer's route into filter mode: the frozen filter row's press.
pub(crate) fn enter_filter(state: &mut ScopeDialogState) {
    if !in_saved(state) || state.saved.mode == DialogMode::Filter {
        return;
    }
    let saved = &mut state.saved;
    dialogmode::enter_filter(&mut saved.mode, &mut saved.entry_query, &saved.query);
}

pub(crate) fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return false;
    };
    // As on Current: every claimed key drops the last refusal; an unclaimed
    // key puts it back.
    let prior_error = state.error.take();
    let claimed = if state.saved.mode == DialogMode::Filter {
        filter_key(&mut state.saved, ks)
    } else {
        match normal_key(shell, ks, window, cx) {
            Normal::Claimed => true,
            // The commit or escape already refreshed, synced and notified.
            Normal::Done => return true,
            Normal::Declined => false,
        }
    };
    if !claimed {
        if let Some(state) = shell.scope_dialog.as_mut() {
            state.error = prior_error;
        }
        return false;
    }
    shell.refresh_dialog_rows(cx);
    cx.notify();
    true
}

/// Keys while the filter owns the input: `escape` restores the entry query,
/// bare `enter` keeps the typed one, and neither commits a row. Arrows move;
/// every other key is the field's.
fn filter_key(saved: &mut SavedScreen, ks: &Keystroke) -> bool {
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

enum Normal {
    Claimed,
    Done,
    Declined,
}

fn normal_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> Normal {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return Normal::Declined;
    };
    if ks.mods.is_chord() || ks.mods.shift {
        return Normal::Declined;
    }
    let on_scope = matches!(
        state.saved.cursor_row().map(|r| &r.id),
        Some(SavedId::Scope(_))
    );
    match ks.key.as_str() {
        "j" | "down" => state.saved.move_cursor(crate::vimnav::NavCommand::Move(1)),
        "k" | "up" => state.saved.move_cursor(crate::vimnav::NavCommand::Move(-1)),
        "/" => {
            let saved = &mut state.saved;
            dialogmode::enter_filter(&mut saved.mode, &mut saved.entry_query, &saved.query);
        }
        // `e` and `n` on an expression row belong to the definition step.
        "e" if on_scope => state.error = Some(EDIT_SCOPE.into()),
        "n" if on_scope => state.error = Some(NEW_SCOPE.into()),
        "enter" => {
            commit_cursor(shell, window, cx);
            return Normal::Done;
        }
        "escape" => {
            let after = state.layers.escape();
            finish(shell, after, window, cx);
            return Normal::Done;
        }
        _ => return Normal::Declined,
    }
    Normal::Claimed
}

/// Load the cursor's scope or toggle the cursor's expression, then leave
/// the screen. A definition removed since the rows derived refuses here and
/// the screen re-derives, so the next key sees the rows as they now are.
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
                .target_frame()
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
    let after = state.layers.commit_saved_row();
    finish(shell, after, window, cx);
}

/// Show whatever the Saved screen left for, or close the dialog when it was
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

/// A row press moves the cursor; the second press of a double-click is
/// `enter`. Ignored unless Saved is the top layer: a step pushed over this
/// screen owns the pointer.
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
    if !in_saved(state) || state.pending.is_some() {
        return;
    }
    state.saved.cursor = at;
    state.saved.cursor_id = state.saved.cursor_row().map(|r| r.id.clone());
    state.error = None;
    // Exactly 2, so a triple-click does not commit twice.
    if click_count == 2 {
        commit_cursor(shell, window, cx);
        return;
    }
    cx.notify();
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

pub(crate) fn build(
    shell: &ShellView,
    state: &ScopeDialogState,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    let screen = &state.saved;
    let theme = cx.theme();
    let paint = crate::shell::listrow::row_paint(theme);
    let muted = theme.muted_foreground;
    let danger = theme.danger;
    let radius = theme.radius;
    let tag_radius = theme.radius_tokens().sm;
    let border = theme.border;
    let mono = crate::fonts::MONO;

    let mut list = v_flex()
        .id("scope-saved-rows")
        .w_full()
        .gap_0p5()
        .debug_selector(|| "scope-saved".to_string());
    for section in [Section::Scopes, Section::Expressions] {
        let names = section_names(section);
        let slug = names.slug;
        list = list.child(
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
                ),
        );
        if screen.section_is_empty(section) {
            list = list.child(
                div()
                    .px_2()
                    .h(scale::design(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .debug_selector(move || format!("scope-saved-empty-{slug}"))
                    .child(names.empty),
            );
            continue;
        }
        for (i, &row_ix) in screen.visible.iter().enumerate() {
            if section_of(&screen.rows[row_ix].id) != section {
                continue;
            }
            let shown = &screen.display[row_ix];
            let el = h_flex()
                .id(("scope-saved-row", i))
                .w_full()
                .h(scale::design(ROW_HEIGHT))
                .flex_shrink_0()
                .px_2()
                .items_center()
                .gap_2()
                .text_sm()
                .rounded(radius)
                .debug_selector(move || format!("scope-saved-row-{i}"));
            let mut el = crate::shell::listrow::paint_row(el, paint, i == screen.cursor);
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
                    press_row(shell, i, event.click_count, window, cx)
                });
            });
            list = list.child(el);
        }
    }

    // The filter row: live while filtering, frozen (a press enters filter
    // mode) otherwise.
    let frozen = (screen.mode == DialogMode::Normal).then(|| dialog::FrozenFilter {
        query: &screen.query,
        slash_filters: true,
        entity: entity.clone(),
    });
    let mut body = v_flex()
        .gap_2()
        .w(scale::design(super::view::WIDTH))
        .child(
            div()
                .debug_selector(|| "scope-saved-filter".to_string())
                .child(dialog::filter_row(&shell.dialog_input, frozen, cx)),
        )
        .child(list);
    if let Some(error) = state.error.as_ref() {
        body = body.child(
            div()
                .px_2()
                .text_xs()
                .text_color(danger)
                .debug_selector(|| "scope-dialog-error".to_string())
                .child(error.clone()),
        );
    }
    let border = cx.theme().border;
    let footer = match super::prompt::pending_footer(state, entity, cx) {
        Some(question) => question,
        None => dialog::hint_rows(&hints(state)),
    };
    body.child(
        v_flex()
            .w_full()
            .gap_1()
            .pt_2()
            .border_t_1()
            .border_color(border)
            .child(footer),
    )
    .into_any_element()
}

fn hints(state: &ScopeDialogState) -> Vec<Hint> {
    if state.saved.mode == DialogMode::Filter {
        return vec![
            Hint::new(HintRow::Go, &["escape"], "restore"),
            Hint::new(HintRow::Go, &["enter"], "keep"),
        ];
    }
    let leave = if state.layers.depth() > 1 {
        "back"
    } else {
        "close"
    };
    vec![
        Hint::new(HintRow::Move, &["j", "k"], "row"),
        Hint::new(HintRow::Go, &["enter"], "load / add-remove"),
        Hint::new(HintRow::Go, &["/"], "filter"),
        Hint::new(HintRow::Go, &["escape"], leave),
    ]
}
