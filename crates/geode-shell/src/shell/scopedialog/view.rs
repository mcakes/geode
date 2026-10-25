//! The Scope dialog's one screen: the lane's scope as rows by ingredient,
//! then the saved scopes and expressions (`saved_view`), under one cursor
//! (`screen`). Steps that exist as their own modals are pushed over it; the
//! text step, name prompts and the definition step are layers of this
//! dialog.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, MouseButton, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use geode_core::named::NamedExpressions;
use geode_core::scope::Scope;
use geode_core::scopes::SavedScopes;

use crate::footer::{Hint, HintRow};
use crate::frame::Frame;
use crate::keymap::Keystroke;

use super::rows::{
    CONTRADICTION, CurrentRows, NamedState, Row, RowId, RowKind, Section, empty_hint,
};
use super::screen::{Jump, Zone};
use super::state::{After, Layer, Layers, Opener, Step};
use crate::frame::FrameViewMut;
use crate::shell::control::{self, PointerStates as _};
use crate::shell::{ShellView, dialog, scale};

pub(super) const WIDTH: f32 = 560.0;
const ROW_HEIGHT: f32 = 28.0;
/// The list's height cap, sixteen rows: the current scope and the head of
/// the saved list fit without scrolling on the reference display.
const LIST_MAX_HEIGHT: f32 = 16.0 * ROW_HEIGHT;
/// The glyph column: wide enough for one glyph, so labels line up by kind.
const GLYPH_WIDTH: f32 = 16.0;

/// Section headers as painted: the copy `Dimensions`, `Expressions`, `Text`
/// in capitals.
const DIMENSIONS_TITLE: &str = "DIMENSIONS";
const EXPRESSIONS_TITLE: &str = "EXPRESSIONS";
const TEXT_TITLE: &str = "TEXT";
const TERM_DETAIL: &str = "unnamed";
const TEXT_DETAIL: &str = "textual columns";

/// Refusal when `mod+s` is pressed off an unnamed term.
pub(crate) const NAME_ONLY_TERMS: &str = "only an unnamed term can be named";
/// Refusal when `i` is pressed off a named reference.
pub(crate) const INLINE_ONLY_NAMED: &str = "only a named reference can be inlined";
/// `c` off a saved row.
pub(crate) const COPY_ONLY_SAVED: &str = "only a saved scope or expression can be copied";
/// `r` off a saved row.
pub(crate) const REVERT_ONLY_SAVED: &str = "only a saved scope or expression can be reverted";

/// Each row's leading glyph, by kind, in its own fixed-width column.
const DIMENSION_GLYPH: &str = "▦";
const TERM_GLYPH: &str = "ƒ";
const NAMED_GLYPH: &str = "≡";
const TEXT_GLYPH: &str = "⌕";

/// One row's painted strings, prepared when the rows derive so `build`
/// only clones them instead of formatting on every frame.
pub(crate) struct RowDisplay {
    /// The row's element id, from its domain identity (`RowId`), so a row
    /// keeps its element state when rows above it come or go.
    pub key: SharedString,
    pub glyph: &'static str,
    pub label: SharedString,
    pub detail: SharedString,
    /// A dimension's selected value count; `None` on every other row.
    pub count: Option<SharedString>,
    /// A named reference that does not resolve: painted in the danger color.
    pub broken: bool,
}

impl RowDisplay {
    fn of(row: &Row) -> Self {
        let key: SharedString = match &row.id {
            RowId::Dimension(column) => format!("dim:{column}"),
            RowId::Term(text, n) => format!("term:{n}:{text}"),
            RowId::Named(name) => format!("named:{name}"),
            RowId::Text => "text".to_string(),
        }
        .into();
        match &row.kind {
            RowKind::Dimension { column, values } => RowDisplay {
                key,
                glyph: DIMENSION_GLYPH,
                label: column.clone().into(),
                detail: values.join(", ").into(),
                count: Some(values.len().to_string().into()),
                broken: false,
            },
            RowKind::Term { text, .. } => RowDisplay {
                key,
                glyph: TERM_GLYPH,
                label: text.clone().into(),
                detail: TERM_DETAIL.into(),
                count: None,
                broken: false,
            },
            RowKind::Named { name, state } => {
                let (detail, broken) = match state {
                    NamedState::Valid { text } => (text.clone(), false),
                    NamedState::Invalid { reason, .. } => (reason.clone(), true),
                    NamedState::Missing => (format!("'{name}' is not defined"), true),
                };
                RowDisplay {
                    key,
                    glyph: NAMED_GLYPH,
                    label: name.clone().into(),
                    detail: detail.into(),
                    count: None,
                    broken,
                }
            }
            RowKind::Text { text } => RowDisplay {
                key,
                glyph: TEXT_GLYPH,
                label: format!("\"{text}\"").into(),
                detail: TEXT_DETAIL.into(),
                count: None,
                broken: false,
            },
        }
    }
}

pub(crate) struct ScopeDialogState {
    pub layers: Layers,
    pub rows: CurrentRows,
    /// `rows`' painted strings, index for index.
    pub display: Vec<RowDisplay>,
    /// The title's provenance text; `None` paints nothing.
    pub title: Option<SharedString>,
    /// The frame generation and config version the rows were derived at.
    pub key: (u64, u64),
    pub cursor: usize,
    /// The cursor's row, so a re-derive keeps it on what it pointed at.
    pub cursor_id: Option<RowId>,
    /// The text step's field: the source of truth `sync_dialog_text` mirrors.
    pub text_draft: String,
    /// The last refusal (an inline that could not resolve, a term that moved).
    pub error: Option<String>,
    /// The saved group's rows, filter and cursor; derived with `rows`
    /// under the same key.
    pub saved: super::saved_view::SavedScreen,
    /// The name being typed while a name prompt is the top layer.
    pub prompt: Option<super::prompt::NamePrompt>,
    /// The question awaiting `y`/`n`; while `Some` it owns every key.
    pub pending: Option<super::prompt::Pending>,
    /// The definition being edited while the definition step is on top.
    pub definition: Option<super::definition::DefinitionStep>,
    /// Which group the cursor rests in; `cursor` (Current's rows) and
    /// `saved.cursor` (Saved's stops) each keep their place while the other
    /// group holds the cursor.
    pub zone: Zone,
    /// The door that opened the dialog: decides whether a saved-row commit
    /// closes it, and which toolbar glyph reads pressed.
    pub opener: Opener,
    /// The cursor is in the saved group only because Current had no rows
    /// (`normalize_zone`), not because the user put it there: the first
    /// current row to appear takes it back, so an `enter` after adding a
    /// first ingredient edits that row instead of loading a saved scope over
    /// it. Any user-chosen move (`rest_in`) clears it.
    forced_saved: bool,
    /// What `follow_cursor` last scrolled to; `None` before the first
    /// follow and from the moment a step is pushed over the screen
    /// (`push_step`), whose paint drops the saved group and may clamp the
    /// offset. A re-derive that leaves the cursor on the same row at the
    /// same child (a data tick, an unchanged key) must not scroll: the user
    /// may have wheeled the list away from the cursor, and a refresh
    /// snapping it back is a defect.
    followed: Option<Followed>,
}

/// The list child `follow_cursor` scrolled to and the row on it. A filter
/// edit can put a different row at the same child; the identity catches it.
#[derive(PartialEq)]
struct Followed {
    ix: usize,
    current: Option<RowId>,
    saved: Option<super::saved::SavedId>,
}

/// What the rows read: the frame generation (every lane value change) and
/// the config version (a named expression redefined).
pub(crate) fn rows_key(frame: &Frame) -> (u64, u64) {
    (frame.generation(), frame.config_version())
}

impl ScopeDialogState {
    fn new(first: Layer, opener: Opener) -> Self {
        ScopeDialogState {
            layers: Layers::open(first),
            rows: CurrentRows::default(),
            display: Vec::new(),
            title: None,
            // Never a real key: the first refresh always derives.
            key: (u64::MAX, u64::MAX),
            cursor: 0,
            cursor_id: None,
            text_draft: String::new(),
            error: None,
            saved: super::saved_view::SavedScreen::new(),
            prompt: None,
            pending: None,
            definition: None,
            zone: Zone::Current,
            opener,
            forced_saved: false,
            followed: None,
        }
    }

    /// Push `step` over the screen. The step's paint drops the saved group
    /// and may clamp the list's offset, so the screen's return follows the
    /// cursor afresh: every push goes through here.
    pub(crate) fn push_step(&mut self, step: Step) {
        self.layers.push(Layer::Step(step));
        self.followed = None;
    }

    /// Put the cursor in `zone` by the user's choice (a move, a jump, a
    /// press, a door's landing, the filter): `normalize_zone` no longer
    /// treats the saved group as a stand-in for an empty Current.
    pub(crate) fn rest_in(&mut self, zone: Zone) {
        self.zone = zone;
        self.forced_saved = false;
    }

    /// The screen is on top: no step drawn over it.
    pub(crate) fn on_screen(&self) -> bool {
        matches!(self.layers.top(), Layer::Current)
    }

    /// Keep the cursor on a group that has somewhere to rest: off Current
    /// when it has no rows, off Saved when it has no stops — except while
    /// the filter is open, which owns the saved group even when it matches
    /// nothing. A cursor forced off an empty Current returns to its first
    /// row once there is one. Runs after every `refresh_dialog_rows`, the
    /// one place that re-derives or re-filters before the next key.
    pub(crate) fn normalize_zone(&mut self) {
        let filtering = self.saved.mode == crate::dialogmode::DialogMode::Filter;
        match self.zone {
            Zone::Current if self.rows.rows.is_empty() && self.saved.stop_count() > 0 => {
                self.zone = Zone::Saved;
                self.forced_saved = true;
            }
            Zone::Saved if self.forced_saved && !self.rows.rows.is_empty() => {
                self.zone = Zone::Current;
                self.forced_saved = false;
                self.cursor = 0;
                self.cursor_id = self.rows.rows.first().map(|r| r.id.clone());
            }
            Zone::Saved
                if !filtering && self.saved.stop_count() == 0 && !self.rows.rows.is_empty() =>
            {
                self.zone = Zone::Current;
            }
            _ => {}
        }
    }

    /// The cursor's Current row; `None` while the cursor is in the saved
    /// group. Every Current verb reads its row here, so this is their
    /// dispatch guard: none of them acts while the cursor is on a saved row.
    pub(crate) fn cursor_row(&self) -> Option<&Row> {
        (self.zone == Zone::Current)
            .then(|| self.rows.rows.get(self.cursor))
            .flatten()
    }

    pub(crate) fn is_current(&self, key: (u64, u64)) -> bool {
        self.key == key && self.saved.key == key
    }

    pub(crate) fn refresh(
        &mut self,
        scope: &Scope,
        named: &NamedExpressions,
        loaded_from: Option<&str>,
        saved: &SavedScopes,
        key: (u64, u64),
    ) {
        self.rows = CurrentRows::derive(scope, named);
        self.display = self.rows.rows.iter().map(RowDisplay::of).collect();
        self.title = super::rows::provenance(scope, loaded_from, saved)
            .label()
            .map(SharedString::from);
        let at = self
            .rows
            .place_cursor(self.cursor_id.as_ref(), self.cursor)
            .unwrap_or(0);
        self.cursor = at;
        self.cursor_id = self.rows.rows.get(at).map(|r| r.id.clone());
        self.key = key;
        self.saved.refresh(saved, named, scope, key);
    }
}

pub(crate) fn in_text_step(state: &ScopeDialogState) -> bool {
    matches!(state.layers.top(), Layer::Step(Step::Text))
}

/// The Change arm's pure half: the text step's draft, the definition
/// step's draft, a name prompt's draft, or the saved group's filter while
/// it is filtering. The screen ignores the field otherwise.
pub(crate) fn on_query_changed(state: &mut ScopeDialogState, text: &str) {
    // A question owns the keyboard; the field is not its to edit.
    if state.pending.is_some() {
        return;
    }
    if in_text_step(state) {
        state.text_draft = text.to_string();
        state.error = None;
    } else if super::definition::in_definition(state) {
        super::definition::on_draft_changed(state, text);
    } else if super::prompt::in_name_prompt(state) {
        super::prompt::on_draft_changed(state, text);
    } else if super::saved_view::filtering(state) {
        state.saved.set_query(text);
    }
}

/// Open on the screen (`frame::scope`, `mod+o`, the scope bar's `+`), the
/// cursor on the first current row. A no-op when the dialog is already on
/// top; refused with a notice when it is lower in the stack
/// (`dialog::can_open`).
pub(crate) fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    // Opening derived the rows, which followed the cursor.
    open_on(view, Layer::Current, Opener::Scope, window, cx);
}

/// The saved door (`frame::scope_saved`, `config::scopes`,
/// `config::expressions`): the screen with the cursor on `section` — on the
/// lane's `loaded_from` scope when it is listed. A load or toggle closes the
/// dialog (`Opener::Saved`). With the dialog already on top on its screen,
/// the cursor jumps there and the opener stays; on a step this is a no-op.
pub(crate) fn open_saved(
    view: &mut ShellView,
    section: super::saved_view::Section,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if view.top_kind() == Some(dialog::DialogKind::Scope) {
        if view
            .scope_dialog
            .as_ref()
            .is_some_and(|s| s.on_screen() && s.pending.is_none())
        {
            jump_saved(view, section, cx);
            cx.notify();
        }
        return;
    }
    // Opening derived the rows (`open_shell_dialog_with_key`).
    if !open_on(view, Layer::Current, Opener::Saved, window, cx) {
        return;
    }
    jump_saved(view, section, cx);
    cx.notify();
}

/// Put the cursor in the saved group on `section`: on the lane's
/// `loaded_from` scope for Scopes when it is listed, else on the section's
/// first stop. A section with no stop (a filter hid every row) leaves the
/// cursor where it was.
fn jump_saved(view: &mut ShellView, section: super::saved_view::Section, cx: &App) {
    let prefer = (section == super::saved_view::Section::Scopes)
        .then(|| {
            view.target_frame(cx)
                .read(cx)
                .loaded_from()
                .map(str::to_string)
        })
        .flatten();
    if let Some(state) = view.scope_dialog.as_mut()
        && state.saved.jump_to(section, prefer.as_deref())
    {
        state.rest_in(Zone::Saved);
        state.error = None;
    }
    // The saved door jumps after opening derived the rows: no re-derive
    // follows this move.
    follow_cursor(view);
}

/// Scroll the list so the cursor's row is in view. Runs at the end of every
/// `refresh_dialog_rows` (keys, loads and toggles, refilters, reloads and
/// `normalize_zone` all pass there) and after the moves that no re-derive
/// follows (the saved door's jump, the frozen filter row's press); never
/// from `build`. The child index comes from `screen::child_index`, which
/// must agree with `build`'s paint order. It scrolls only when that index
/// differs from the one it last scrolled to, so a refresh that moves
/// nothing leaves a wheel-scrolled list alone. Only while the screen is on top:
/// a step drawn over it does not paint the saved group, so an index into
/// it would be left pending against the wrong children.
pub(crate) fn follow_cursor(shell: &mut ShellView) {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    if !state.on_screen() {
        // `push_step` already cleared `followed`; clearing it again is a
        // deliberate safeguard for a step that reached the top without it,
        // so the screen's return still follows.
        state.followed = None;
        return;
    }
    let layout = super::screen::Layout {
        contradiction: state.rows.contradiction,
        current: [Section::Dimensions, Section::Expressions, Section::Text].map(|section| {
            state
                .rows
                .rows
                .iter()
                .filter(|r| r.section == section)
                .count()
                .max(1)
        }),
        saved: state.saved.painted_counts(),
    };
    let at = match state.zone {
        Zone::Current => state.rows.rows.get(state.cursor).map(|row| {
            let section = match row.section {
                Section::Dimensions => 0,
                Section::Expressions => 1,
                Section::Text => 2,
            };
            let within = state.rows.rows[..state.cursor]
                .iter()
                .filter(|r| r.section == row.section)
                .count();
            super::screen::At {
                zone: Zone::Current,
                section,
                within,
            }
        }),
        Zone::Saved => state
            .saved
            .cursor_within()
            .map(|(section, within)| super::screen::At {
                zone: Zone::Saved,
                section,
                within,
            }),
    };
    let Some(at) = at else {
        return;
    };
    let ix = super::screen::child_index(&layout, &at);
    let now = match state.zone {
        Zone::Current => Followed {
            ix,
            current: state.rows.rows.get(state.cursor).map(|r| r.id.clone()),
            saved: None,
        },
        Zone::Saved => Followed {
            ix,
            current: None,
            saved: state.saved.cursor_row().map(|r| r.id.clone()),
        },
    };
    // Only a cursor whose child or row changed scrolls; see `followed`.
    if state.followed.as_ref() != Some(&now) {
        state.followed = Some(now);
        let first_current = state.zone == Zone::Current && state.cursor == 0;
        let target = super::screen::follow_target(
            &at,
            ix,
            first_current,
            header_above(&shell.scope_dialog_scroll, ix),
        );
        shell.scope_dialog_scroll.scroll_to_item(target);
    }
}

/// Whether the header painted just above child `ix` lies above the list's
/// viewport, read from the handle's last layout. The children before a
/// cursor change little between two frames (a row added or removed above
/// moves it by one row), so the last layout answers for the next; a list
/// not laid out yet (no bounds) reads as not above, and the row is
/// scrolled to directly.
fn header_above(scroll: &gpui::ScrollHandle, ix: usize) -> bool {
    let Some(header) = ix.checked_sub(1).and_then(|h| scroll.bounds_for_item(h)) else {
        return false;
    };
    header.top() + scroll.offset().y < scroll.bounds().top()
}

/// The one-shot save door (the scope bar's save chip,
/// `scope::save_current`): the save prompt alone, so its commit or `escape`
/// closes the dialog. An empty scope refuses on the status bar and opens
/// nothing. With the dialog already on top on its screen the prompt is
/// pushed over it instead, as `s` does there; on any other layer this is a
/// no-op.
pub(crate) fn open_save(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.top_kind() == Some(dialog::DialogKind::Scope) {
        if view
            .scope_dialog
            .as_ref()
            .is_some_and(|s| s.on_screen() && s.pending.is_none())
        {
            super::prompt::push_save(view, cx);
            view.refresh_dialog_rows(cx);
            dialog::sync_dialog_text(view, window, cx);
            cx.notify();
        }
        return;
    }
    let (empty, seed) = {
        let frame = view.target_frame(cx).read(cx);
        (
            frame.scope().is_empty(),
            frame.loaded_from().map(str::to_string),
        )
    };
    if empty {
        view.notice = Some(super::prompt::NOTHING_TO_SAVE.into());
        cx.notify();
        return;
    }
    if !open_on(
        view,
        Layer::Step(Step::SaveScope),
        Opener::Scope,
        window,
        cx,
    ) {
        return;
    }
    if let Some(state) = view.scope_dialog.as_mut() {
        state.prompt = Some(super::prompt::save_prompt(seed));
    }
    view.refresh_dialog_rows(cx);
    dialog::sync_dialog_text(view, window, cx);
    cx.notify();
}

/// The one-shot definition door (a `≡` chip's body): the expression's
/// definition alone, so its commit or `escape` closes the dialog. A name
/// nothing defines refuses on the status bar and opens nothing. With the
/// dialog already on top on its screen the step is pushed over it instead;
/// on any other layer this is a no-op.
pub(crate) fn open_definition(
    view: &mut ShellView,
    name: String,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if view.top_kind() == Some(dialog::DialogKind::Scope) {
        if view
            .scope_dialog
            .as_ref()
            .is_some_and(|s| s.on_screen() && s.pending.is_none())
        {
            super::definition::push(view, Some(name), window, cx);
        }
        return;
    }
    let step = match super::definition::step_for(view, Some(&name), cx) {
        Ok(step) => step,
        Err(refusal) => {
            view.notice = Some(refusal.into());
            cx.notify();
            return;
        }
    };
    if !open_on(
        view,
        Layer::Step(Step::Definition { name: Some(name) }),
        Opener::Scope,
        window,
        cx,
    ) {
        return;
    }
    if let Some(state) = view.scope_dialog.as_mut() {
        state.definition = Some(step);
    }
    view.refresh_dialog_rows(cx);
    super::definition::begin(view, window, cx);
}

/// Open the dialog with `first` as its bottom layer, recording the door that
/// opened it. Returns whether it opened (`dialog::can_open` refuses one
/// lower in the stack).
fn open_on(
    view: &mut ShellView,
    first: Layer,
    opener: Opener,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if !dialog::can_open(view, dialog::DialogKind::Scope) {
        return false;
    }
    view.scope_dialog = Some(ScopeDialogState::new(first, opener));
    // The handle outlives a close: a fresh dialog opens at the top, not at
    // the offset the last visit left (an offset the minimal follow scroll
    // would keep, hiding the first header). Set now, before opening derives
    // the rows and follows the cursor from it.
    view.scope_dialog_scroll.set_offset(gpui::Point::default());
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        dialog::DialogKind::Scope,
        "Scope",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        false,
    );
    dialog::set_title_extra(view, title_extra);
    true
}

/// Run one edit on the dialog's lane (`target_frame`, so a pinned workspace
/// is edited, not the shared lane) and notify when the frame generation
/// moved: a provenance-only change (a clear of an empty scope) moves it too,
/// and a notify keyed on the method's boolean would leave observers stale.
pub(super) fn edit_lane<R>(
    shell: &mut ShellView,
    cx: &mut Context<ShellView>,
    edit: impl FnOnce(&mut FrameViewMut) -> R,
) -> R {
    shell.target_frame(cx).update(cx, |f, cx| {
        let before = f.generation();
        let out = edit(f);
        if f.generation() != before {
            cx.notify();
        }
        out
    })
}

fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return false;
    };
    // A question owns every key, whatever layer it was asked over.
    if state.pending.is_some() {
        return super::prompt::pending_key(shell, ks, window, cx);
    }
    if in_text_step(state) {
        return text_step_key(shell, ks, window, cx);
    }
    if super::definition::in_definition(state) {
        return super::definition::handle_key(shell, ks, window, cx);
    }
    if super::prompt::in_name_prompt(state) {
        return super::prompt::prompt_key(shell, ks, window, cx);
    }
    // The filter owns every key it claims, verb letters included: they
    // type into the field rather than act.
    if state.saved.mode == crate::dialogmode::DialogMode::Filter {
        let prior_error = state.error.take();
        if !super::saved_view::filter_key(&mut state.saved, ks) {
            state.error = prior_error;
            return false;
        }
        state.rest_in(Zone::Saved);
        shell.refresh_dialog_rows(cx);
        cx.notify();
        return true;
    }
    // The error describes the last action only: every claimed key drops it,
    // and the refusing action sets it again. An unclaimed key puts it back.
    let prior_error = state.error.take();
    let zone = state.zone;
    let plain = !ks.mods.is_chord();
    match (ks.key.as_str(), plain, ks.mods.shift) {
        ("j" | "down", true, false) => move_cursor(shell, 1),
        ("k" | "up", true, false) => move_cursor(shell, -1),
        ("d", true, true) => {
            edit_lane(shell, cx, |f| f.clear_scope());
        }
        ("u", true, false) => {
            edit_lane(shell, cx, |f| f.undo_scope());
        }
        ("r", false, false) if ks.mods.ctrl && !ks.mods.alt && !ks.mods.cmd => {
            edit_lane(shell, cx, |f| f.redo_scope());
        }
        // Step openers push their modal over this one and return: the pushed
        // modal owns the next frame, and its commit pops back here through
        // `close_modal`, which re-derives these rows.
        ("p", true, false) => {
            crate::shell::picker::open(shell, None, window, cx);
            return true;
        }
        ("x", true, false) => {
            crate::shell::scope_expr_view::open(
                shell,
                crate::shell::scope_expr_view::Mode::Add,
                window,
                cx,
            );
            return true;
        }
        ("t", true, false) => {
            enter_text_step(shell, window, cx);
            return true;
        }
        ("s", true, false) => {
            super::prompt::push_save(shell, cx);
            dialog::sync_dialog_text(shell, window, cx);
        }
        ("o", true, false) => jump(shell, cx),
        ("/", true, false) => {
            if let Some(state) = shell.scope_dialog.as_mut() {
                super::saved_view::enter_filter(state);
            }
            dialog::sync_dialog_text(shell, window, cx);
        }
        // Row verbs: the cursor's group decides.
        ("enter" | "e" | "n" | "c" | "d" | "r", true, false) if zone == Zone::Saved => {
            return super::saved_view::row_key(shell, ks.key.as_str(), window, cx);
        }
        ("i", true, false) if zone == Zone::Saved => set_error(shell, INLINE_ONLY_NAMED),
        ("s", false, false) if zone == Zone::Saved && ks.mods == shell.services(cx).mod_alias => {
            set_error(shell, NAME_ONLY_TERMS)
        }
        ("d", true, false) => remove_cursor_row(shell, cx),
        ("enter" | "e", true, false) => {
            open_cursor_row(shell, window, cx);
            return true;
        }
        ("n", true, false) => {
            super::definition::push(shell, None, window, cx);
            return true;
        }
        ("c", true, false) => set_error(shell, COPY_ONLY_SAVED),
        ("r", true, false) => set_error(shell, REVERT_ONLY_SAVED),
        ("i", true, false) => inline_cursor_row(shell, cx),
        ("s", false, false) if ks.mods == shell.services(cx).mod_alias => {
            name_cursor_term(shell, window, cx);
            return true;
        }
        _ => {
            if let Some(state) = shell.scope_dialog.as_mut() {
                state.error = prior_error;
            }
            return false;
        }
    }
    // Every edit above may have changed the lane: re-derive now so the next
    // key and the next paint read the same rows.
    shell.refresh_dialog_rows(cx);
    cx.notify();
    true
}

/// Keys while the text step is open. Enter commits the trimmed draft (empty
/// clears the filter) and returns to the screen; Escape returns with nothing
/// changed, and is claimed here so the shell's unclaimed-escape fallback
/// does not close the whole dialog. Every other key is the field's, so
/// the screen's verbs type as text.
fn text_step_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if ks.mods.is_chord() {
        return false;
    }
    match ks.key.as_str() {
        "enter" => {
            let draft = shell
                .scope_dialog
                .as_ref()
                .map(|s| s.text_draft.trim().to_string())
                .unwrap_or_default();
            edit_lane(shell, cx, |f| {
                f.set_text((!draft.is_empty()).then_some(draft))
            });
            if let Some(state) = shell.scope_dialog.as_mut() {
                let after = state.layers.commit_step();
                debug_assert_eq!(after, After::Show, "Current is beneath the step");
            }
        }
        "escape" => {
            if let Some(state) = shell.scope_dialog.as_mut() {
                let after = state.layers.escape();
                debug_assert_eq!(after, After::Show, "Current is beneath the step");
            }
        }
        _ => return false,
    }
    shell.refresh_dialog_rows(cx);
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
    true
}

fn set_error(shell: &mut ShellView, refusal: &str) {
    if let Some(state) = shell.scope_dialog.as_mut() {
        state.error = Some(refusal.into());
    }
}

/// `j`/`k`: one row over the current rows and the saved stops as one list.
fn move_cursor(shell: &mut ShellView, delta: i64) {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    let at = match state.zone {
        Zone::Current => state.cursor,
        Zone::Saved => state.saved.cursor,
    };
    let (zone, at) = super::screen::step(
        state.zone,
        at,
        state.rows.rows.len(),
        state.saved.stop_count(),
        delta,
    );
    state.rest_in(zone);
    match zone {
        // Re-take the identity, or a re-derive would put the cursor back on
        // the row it left.
        Zone::Current => {
            state.cursor = at;
            state.cursor_id = state.rows.rows.get(at).map(|r| r.id.clone());
        }
        Zone::Saved => state.saved.set_cursor(at),
    }
}

/// `o`: the saved scopes, the saved expressions, the current rows, in turn.
fn jump(shell: &mut ShellView, cx: &App) {
    let Some(state) = shell.scope_dialog.as_ref() else {
        return;
    };
    use super::saved_view::Section as Saved;
    let on_scopes = state.saved.cursor_section() == Some(Saved::Scopes);
    // A target with nowhere to land (a kept filter emptied its section)
    // passes the jump on to the next, so repeated `o` always reaches what
    // is visible.
    let lands = |jump: Jump| match jump {
        Jump::Scopes => state.saved.has_stop(Saved::Scopes),
        Jump::Expressions => state.saved.has_stop(Saved::Expressions),
        Jump::Current => !state.rows.rows.is_empty(),
    };
    let Some(target) =
        super::screen::jump_target(state.zone, on_scopes, state.rows.rows.len(), lands)
    else {
        return;
    };
    match target {
        Jump::Scopes => jump_saved(shell, super::saved_view::Section::Scopes, cx),
        Jump::Expressions => jump_saved(shell, super::saved_view::Section::Expressions, cx),
        Jump::Current => {
            if let Some(state) = shell.scope_dialog.as_mut() {
                state.rest_in(Zone::Current);
                state.cursor = 0;
                state.cursor_id = state.rows.rows.first().map(|r| r.id.clone());
            }
        }
    }
}

/// Remove the cursor's row in one undoable edit. A term passes the term the
/// row was derived from, so a term that moved since refuses with
/// `TERM_GONE` rather than removing whichever term now has its index.
fn remove_cursor_row(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(row) = shell
        .scope_dialog
        .as_ref()
        .and_then(|s| s.cursor_row().cloned())
    else {
        return;
    };
    let refused = match row.kind {
        RowKind::Dimension { column, .. } => {
            edit_lane(shell, cx, |f| f.drop_dimension(&column));
            None
        }
        RowKind::Term { index, term, .. } => edit_lane(shell, cx, |f| {
            f.replace_expression_term(index, &term, None).err()
        })
        .map(|_| crate::shell::scope_expr_view::TERM_GONE.to_string()),
        RowKind::Named { name, .. } => {
            edit_lane(shell, cx, |f| f.drop_named(&name));
            None
        }
        RowKind::Text { .. } => {
            edit_lane(shell, cx, |f| f.set_text(None));
            None
        }
    };
    if let Some(state) = shell.scope_dialog.as_mut() {
        state.error = refused;
    }
}

/// Open the step that edits the cursor's row, pushed over this dialog: a
/// dimension's values in the picker, a term in the expression dialog, a
/// reference's definition in this dialog's definition step, the text in
/// the text step.
fn open_cursor_row(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(row) = shell
        .scope_dialog
        .as_ref()
        .and_then(|s| s.cursor_row().cloned())
    else {
        return;
    };
    match row.kind {
        RowKind::Dimension { column, .. } => {
            crate::shell::picker::open(shell, Some(column), window, cx)
        }
        RowKind::Term { index, term, .. } => {
            open_term_row(shell, index, &term, window, cx);
        }
        RowKind::Named { name, .. } => super::definition::push(shell, Some(name), window, cx),
        RowKind::Text { .. } => enter_text_step(shell, window, cx),
    }
}

/// Open the expression dialog on the cursor's term, only while term `index`
/// is still the term the row was derived from. A row derived before the
/// expression changed would otherwise seed the editor (or the name entry)
/// with whichever neighbour now holds its index, so it refuses with
/// `TERM_GONE` instead, as removal does. Returns whether it opened.
fn open_term_row(
    shell: &mut ShellView,
    index: usize,
    term: &geode_core::scope::Expr,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if !shell
        .target_frame(cx)
        .read(cx)
        .expression_term_is(index, term)
    {
        if let Some(state) = shell.scope_dialog.as_mut() {
            state.error = Some(crate::shell::scope_expr_view::TERM_GONE.into());
        }
        // The refusal stays on this screen: re-derive so the paint and the
        // next key read the lane as it now is.
        shell.refresh_dialog_rows(cx);
        cx.notify();
        return false;
    }
    crate::shell::scope_expr_view::open_term(shell, index, window, cx);
    true
}

/// Open the text step over the screen, seeded with the lane's text so `enter`
/// on the text row edits it rather than retyping it.
///
/// A no-op when the step is already open: re-seeding would drop what was
/// typed, and a second step layer would need two `escape`s to leave.
fn enter_text_step(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if shell.scope_dialog.as_ref().is_none_or(in_text_step) {
        return;
    }
    let seed = shell
        .target_frame(cx)
        .read(cx)
        .scope()
        .text
        .clone()
        .unwrap_or_default();
    if let Some(state) = shell.scope_dialog.as_mut() {
        state.text_draft = seed;
        state.error = None;
        state.push_step(Step::Text);
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Whether the current rows and add controls take the pointer: only while
/// the screen itself is on top with no question up and no filter live.
/// Under a step drawn on this body (the text step, the save prompt) they are
/// its preview; while filtering, the field owns the keyboard and a press
/// here would act behind it.
pub(super) fn takes_pointer(state: &ScopeDialogState) -> bool {
    state.on_screen()
        && state.pending.is_none()
        && state.saved.mode != crate::dialogmode::DialogMode::Filter
}

/// The pointer route to `p`, `x` and `t`: a section header's `add` control
/// or its empty row opens the step that adds to that section, the same door
/// its key opens. Ignored under a step drawn here, where these are a preview
/// of the rows rather than controls: a step opened there would cover the
/// typing.
fn add_from_pointer(
    shell: &mut ShellView,
    section: Section,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if !shell.scope_dialog.as_ref().is_some_and(takes_pointer) {
        return;
    }
    if let Some(state) = shell.scope_dialog.as_mut() {
        state.error = None;
    }
    match section {
        Section::Dimensions => crate::shell::picker::open(shell, None, window, cx),
        Section::Expressions => crate::shell::scope_expr_view::open(
            shell,
            crate::shell::scope_expr_view::Mode::Add,
            window,
            cx,
        ),
        Section::Text => enter_text_step(shell, window, cx),
    }
    cx.notify();
}

/// `mod+s`: open the cursor's term in the expression dialog straight into
/// its name entry. Off a term it refuses, so a dimension or reference is
/// never handed to a dialog that would name some other term.
fn name_cursor_term(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let row = shell
        .scope_dialog
        .as_ref()
        .and_then(|s| s.cursor_row().cloned());
    let Some(RowKind::Term { index, term, .. }) = row.map(|r| r.kind) else {
        if let Some(state) = shell.scope_dialog.as_mut() {
            state.error = Some(NAME_ONLY_TERMS.into());
        }
        cx.notify();
        return;
    };
    // A refusal or a term `open_term` could not find opens nothing; naming
    // then would land on whatever dialog is on top.
    if open_term_row(shell, index, &term, window, cx)
        && shell.top_kind() == Some(dialog::DialogKind::ScopeExpr)
    {
        crate::shell::scope_expr_view::begin_naming(shell, window, cx);
    }
}

/// `i`: replace the cursor's reference with its definition in one undoable
/// edit. A reference that does not resolve, or a row that is not a
/// reference, refuses into `error` and changes nothing.
fn inline_cursor_row(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let row = shell
        .scope_dialog
        .as_ref()
        .and_then(|s| s.cursor_row().cloned());
    let refused = match row.map(|r| r.kind) {
        Some(RowKind::Named { name, .. }) => edit_lane(shell, cx, |f| f.inline_named(&name)).err(),
        _ => Some(INLINE_ONLY_NAMED.to_string()),
    };
    if let Some(state) = shell.scope_dialog.as_mut() {
        state.error = refused;
    }
}

/// Paints only when there is a provenance to say, so an empty scope's title
/// carries no element at all.
fn title_extra(shell: &ShellView, cx: &mut App) -> AnyElement {
    let Some(label) = shell.scope_dialog.as_ref().and_then(|s| s.title.clone()) else {
        return div().into_any_element();
    };
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .debug_selector(|| "scope-dialog-title-extra".to_string())
        .child(label)
        .into_any_element()
}

/// A section's painted names, all static so `build` formats nothing: its
/// header, the slug its row selectors carry, and its `add` control's id,
/// tooltip title and key.
struct SectionNames {
    title: &'static str,
    slug: &'static str,
    add_id: &'static str,
    add_tip: &'static str,
    add_tip_title: &'static str,
    add_key: &'static str,
    empty_id: &'static str,
}

fn section_names(section: Section) -> SectionNames {
    match section {
        Section::Dimensions => SectionNames {
            title: DIMENSIONS_TITLE,
            slug: "dimensions",
            add_id: "scope-dialog-add-dimensions",
            add_tip: "tip-scope-dialog-add-dimensions",
            add_tip_title: "Add a dimension",
            add_key: "p",
            empty_id: "scope-dialog-empty-dimensions",
        },
        Section::Expressions => SectionNames {
            title: EXPRESSIONS_TITLE,
            slug: "expressions",
            add_id: "scope-dialog-add-expressions",
            add_tip: "tip-scope-dialog-add-expressions",
            add_tip_title: "Add an expression",
            add_key: "x",
            empty_id: "scope-dialog-empty-expressions",
        },
        Section::Text => SectionNames {
            title: TEXT_TITLE,
            slug: "text",
            add_id: "scope-dialog-add-text",
            add_tip: "tip-scope-dialog-add-text",
            add_tip_title: "Set the text filter",
            add_key: "t",
            empty_id: "scope-dialog-empty-text",
        },
    }
}

/// The `add` control's copy, painted at a section header's right.
const ADD_LABEL: &str = "add";

fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    #[cfg(debug_assertions)]
    shell.assert_rows_current(cx);
    let Some(state) = shell.scope_dialog.as_ref() else {
        return div().into_any_element();
    };
    if super::definition::in_definition(state) {
        return super::definition::build(shell, state, entity, cx);
    }
    // Only the save prompt previews the lane's scope, which is what it
    // saves; the other name prompts preview what they name, alone.
    if super::prompt::paints_alone(state) {
        return super::prompt::build_alone(shell, state, entity, cx);
    }
    let theme = cx.theme();
    let paint = crate::shell::listrow::row_paint(theme);
    let muted = theme.muted_foreground;
    let danger = theme.danger;
    let radius = theme.radius;
    let mono = crate::fonts::MONO;

    // One scroll list: every header, row, empty row, the separator and the
    // filter row are its direct children, in `screen::child_index`'s order,
    // so `follow_cursor` can scroll to the cursor's child.
    let mut list = v_flex()
        .id("scope-dialog-scroll")
        .w_full()
        .gap_0p5()
        .max_h(scale::design(LIST_MAX_HEIGHT))
        .overflow_y_scroll()
        .track_scroll(&shell.scope_dialog_scroll)
        .debug_selector(|| "scope-dialog-scroll".to_string());
    if state.rows.contradiction {
        list = list.child(
            div()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(danger)
                .debug_selector(|| "scope-dialog-contradiction".to_string())
                .child(CONTRADICTION),
        );
    }
    // The add controls and empty rows are muted labels on the modal panel.
    let add_states = control::paint(theme, control::Rest::Bare, theme.popover, muted);
    for section in [Section::Dimensions, Section::Expressions, Section::Text] {
        let names = section_names(section);
        let slug = names.slug;
        let add = entity.clone();
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
                .debug_selector(move || format!("scope-dialog-section-{slug}"))
                // `scope-dialog` marks the current sections as painted: the
                // Dimensions header always paints with them. An element has
                // one selector, so it sits on the header's title.
                .child(
                    div()
                        .when(section == Section::Dimensions, |el| {
                            el.debug_selector(|| "scope-dialog".to_string())
                        })
                        .child(names.title),
                )
                .child(
                    div()
                        .id(names.add_id)
                        .px_1()
                        .rounded(radius)
                        .text_color(muted)
                        .debug_selector(move || names.add_id.to_string())
                        .pointer_states(add_states)
                        .tooltip(crate::tips::tip_key(
                            names.add_tip,
                            names.add_tip_title,
                            names.add_key,
                        ))
                        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            add.update(cx, |shell, cx| {
                                add_from_pointer(shell, section, window, cx)
                            });
                        })
                        .child(ADD_LABEL),
                ),
        );
        if state.rows.section_is_empty(section) {
            // No cursor rests here, so a single press opens the step that
            // fills the section.
            let add = entity.clone();
            list = list.child(
                div()
                    .id(names.empty_id)
                    .flex_shrink_0()
                    .px_2()
                    .h(scale::design(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .text_sm()
                    .rounded(radius)
                    .text_color(muted)
                    .debug_selector(move || names.empty_id.to_string())
                    .pointer_states(add_states)
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        add.update(cx, |shell, cx| add_from_pointer(shell, section, window, cx));
                    })
                    .child(empty_hint(section)),
            );
            continue;
        }
        for (i, row) in state.rows.rows.iter().enumerate() {
            if row.section != section {
                continue;
            }
            let highlighted = state.zone == Zone::Current && i == state.cursor;
            let el = h_flex()
                .id(gpui::ElementId::Name(state.display[i].key.clone()))
                .w_full()
                .h(scale::design(ROW_HEIGHT))
                .flex_shrink_0()
                .px_2()
                .items_center()
                .gap_2()
                .text_sm()
                .rounded(radius)
                .debug_selector(move || format!("scope-dialog-row-{i}"));
            let mut el = crate::shell::listrow::paint_row(el, paint, highlighted);
            let shown = &state.display[i];
            let (label, detail) = (shown.label.clone(), shown.detail.clone());
            el = el.child(
                div()
                    .w(scale::design(GLYPH_WIDTH))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .text_color(if shown.broken { danger } else { muted })
                    .debug_selector(move || format!("scope-dialog-glyph-{i}"))
                    .child(shown.glyph),
            );
            el = match &row.kind {
                RowKind::Dimension { .. } => el
                    .child(div().w(scale::design(120.0)).text_color(muted).child(label))
                    .child(div().flex_1().truncate().child(detail))
                    .when_some(shown.count.clone(), |el, count| {
                        el.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted)
                                .debug_selector(move || format!("scope-dialog-count-{i}"))
                                .child(count),
                        )
                    }),
                RowKind::Term { .. } => el
                    .child(div().flex_1().truncate().font_family(mono).child(label))
                    .child(div().text_xs().text_color(muted).child(detail)),
                RowKind::Named { .. } => el
                    .child(
                        div()
                            .text_color(if shown.broken { danger } else { paint.text })
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_xs()
                            .font_family(mono)
                            .text_color(if shown.broken { danger } else { muted })
                            .child(detail),
                    ),
                RowKind::Text { .. } => el
                    .child(div().flex_1().truncate().child(label))
                    .child(div().text_xs().text_color(muted).child(detail)),
            };
            let click = entity.clone();
            el = el.on_mouse_down(MouseButton::Left, move |event, window, cx| {
                click.update(cx, |shell, cx| {
                    // The rows painted under a step drawn here (the text
                    // step, the save prompt) are its preview, not controls: a
                    // press there would move the cursor under the
                    // field, and a double-click would open a row's step
                    // over the typing.
                    if !shell.scope_dialog.as_ref().is_some_and(takes_pointer) {
                        return;
                    }
                    if let Some(state) = shell.scope_dialog.as_mut() {
                        state.rest_in(Zone::Current);
                        state.cursor = i;
                        state.cursor_id = state.rows.rows.get(i).map(|r| r.id.clone());
                        state.error = None;
                    }
                    // The first press moves the cursor; the second press of a
                    // double-click is `enter`. Exactly 2, so a triple-click
                    // does not open a second step.
                    if event.click_count == 2 {
                        open_cursor_row(shell, window, cx);
                    }
                    // A pushed step synced the shared input for itself.
                    if shell.top_kind() == Some(dialog::DialogKind::Scope) {
                        dialog::sync_dialog_text(shell, window, cx);
                    }
                    cx.notify();
                });
            });
            list = list.child(el);
        }
    }
    // The saved group shares the screen; a step drawn here (the text step,
    // the save prompt) previews the current sections only.
    if state.on_screen() {
        list = list.children(super::saved_view::group(shell, state, entity, cx));
    }

    let mut body = v_flex().gap_2().w(scale::design(WIDTH));
    // The text step is drawn here, above the rows it will change.
    if in_text_step(state) {
        body = body.child(
            div()
                .debug_selector(|| "scope-dialog-text-field".to_string())
                .child(dialog::filter_row(&shell.dialog_input, None, cx)),
        );
    }
    if let Some(field) = super::prompt::field(shell, state, cx) {
        body = body.child(field);
    }
    body = body.child(list);
    if let Some(error) = super::prompt::error(state).or(state.error.as_ref()) {
        body = body.child(
            div()
                .px_2()
                .text_xs()
                .text_color(danger)
                .debug_selector(|| "scope-dialog-error".to_string())
                .child(error.clone()),
        );
    }
    let border = theme.border;
    let footer = match super::prompt::pending_footer(state, entity, cx) {
        Some(question) => question,
        None => dialog::hint_rows(&hints(shell, state, cx)),
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

fn hints(shell: &ShellView, state: &ScopeDialogState, cx: &App) -> Vec<Hint> {
    // Inside the text step only its own two keys act; the screen's verbs type.
    if in_text_step(state) {
        return vec![
            Hint::new(HintRow::Go, &["enter"], "set text").selector("scope-dialog-hint-set-text"),
            Hint::new(HintRow::Go, &["escape"], "back"),
        ];
    }
    if super::prompt::in_name_prompt(state) {
        return super::prompt::hints(state);
    }
    if state.saved.mode == crate::dialogmode::DialogMode::Filter {
        return vec![
            Hint::new(HintRow::Go, &["escape"], "restore"),
            Hint::new(HintRow::Go, &["enter"], "keep"),
        ];
    }
    match state.zone {
        Zone::Current => current_hints(shell, state, cx),
        Zone::Saved => saved_hints(state),
    }
}

fn current_hints(shell: &ShellView, state: &ScopeDialogState, cx: &App) -> Vec<Hint> {
    let mut hints = vec![
        Hint::new(HintRow::Move, &["j", "k"], "row"),
        Hint::new(HintRow::Edit, &["p"], "dimension…"),
        Hint::new(HintRow::Edit, &["x"], "expression…"),
        Hint::new(HintRow::Edit, &["t"], "text"),
        Hint::new(HintRow::Edit, &["d"], "remove"),
        Hint::new(HintRow::Edit, &["shift+d"], "clear all"),
        Hint::new(HintRow::Edit, &["u"], "undo"),
        Hint::new(HintRow::Edit, &["ctrl+r"], "redo"),
    ];
    // Row-specific verbs show only on the rows they act on.
    match state.cursor_row().map(|r| &r.kind) {
        Some(RowKind::Named { .. }) => hints.push(Hint::new(HintRow::Edit, &["i"], "inline")),
        Some(RowKind::Term { .. }) => hints.push(Hint::keystroke(
            HintRow::Edit,
            Keystroke {
                key: "s".into(),
                mods: shell.services(cx).mod_alias,
            },
            "name",
        )),
        _ => {}
    }
    hints.extend([
        Hint::new(HintRow::Go, &["enter"], "edit row").selector("scope-dialog-hint-edit-row"),
        Hint::new(HintRow::Go, &["o"], "saved"),
        Hint::new(HintRow::Go, &["s"], "save as…"),
        Hint::new(HintRow::Go, &["/"], "filter"),
        Hint::new(HintRow::Go, &["escape"], "close"),
    ]);
    hints
}

fn saved_hints(state: &ScopeDialogState) -> Vec<Hint> {
    use super::saved_view::{Section as Saved, Stop};
    // `enter` says what it does on the stop under the cursor.
    let enter = match state.saved.cursor_stop() {
        Some(Stop::Empty(Saved::Expressions)) => {
            Hint::new(HintRow::Go, &["enter"], "new expression").selector("scope-saved-hint-new")
        }
        _ => Hint::new(HintRow::Go, &["enter"], "load / add-remove"),
    };
    // `e` and `n` act only on an expression row (`n` on the empty
    // Expressions row too); on a scope row both refuse, so neither shows.
    let on_expression = matches!(
        state.saved.cursor_row().map(|r| &r.id),
        Some(super::saved::SavedId::Expression(_))
    );
    let on_empty_expressions = state.saved.cursor_stop() == Some(Stop::Empty(Saved::Expressions));
    let mut hints = vec![Hint::new(HintRow::Move, &["j", "k"], "row"), enter];
    if on_expression {
        hints.push(
            Hint::new(HintRow::Edit, &["e"], "edit expression").selector("scope-saved-hint-e"),
        );
    }
    if on_expression || on_empty_expressions {
        hints.push(
            Hint::new(HintRow::Edit, &["n"], "new expression").selector("scope-saved-hint-n"),
        );
    }
    hints.extend([
        Hint::new(HintRow::Edit, &["c"], "copy"),
        Hint::new(HintRow::Edit, &["d"], "delete"),
        Hint::new(HintRow::Edit, &["r"], "revert"),
        Hint::new(HintRow::Edit, &["u"], "undo"),
        Hint::new(HintRow::Go, &["o"], "next section"),
        Hint::new(HintRow::Go, &["s"], "save as…"),
        Hint::new(HintRow::Go, &["/"], "filter"),
        Hint::new(HintRow::Go, &["escape"], "close"),
    ]);
    hints
}
