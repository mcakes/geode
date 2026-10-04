//! The Scope dialog's Current screen: the lane's scope as rows by
//! ingredient, refreshed when the frame changes under it. Steps that exist
//! as their own modals (the dimension picker, the expression dialog, the
//! save prompt) are pushed over this one; their commits pop back here. The
//! text step is drawn here; the Saved screen (`saved_view`) is a layer of
//! this dialog, and keys and paint route to it while it is on top.

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
use super::state::{After, Layer, Layers, Step};
use crate::frame::FrameViewMut;
use crate::shell::control::{self, PointerStates as _};
use crate::shell::{ShellView, dialog, scale};

pub(super) const WIDTH: f32 = 560.0;
const ROW_HEIGHT: f32 = 28.0;
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

/// Each row's leading glyph, by kind, in its own fixed-width column.
const DIMENSION_GLYPH: &str = "▦";
const TERM_GLYPH: &str = "ƒ";
const NAMED_GLYPH: &str = "≡";
const TEXT_GLYPH: &str = "⌕";

/// One row's painted strings, prepared when the rows derive so `build`
/// only clones them instead of formatting on every frame.
pub(crate) struct RowDisplay {
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
        match &row.kind {
            RowKind::Dimension { column, values } => RowDisplay {
                glyph: DIMENSION_GLYPH,
                label: column.clone().into(),
                detail: values.join(", ").into(),
                count: Some(values.len().to_string().into()),
                broken: false,
            },
            RowKind::Term { text, .. } => RowDisplay {
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
                    glyph: NAMED_GLYPH,
                    label: name.clone().into(),
                    detail: detail.into(),
                    count: None,
                    broken,
                }
            }
            RowKind::Text { text } => RowDisplay {
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
    /// The Saved screen's rows, filter and cursor; derived with `rows`
    /// under the same key.
    pub saved: super::saved_view::SavedScreen,
    /// The name being typed while a name prompt is the top layer.
    pub prompt: Option<super::prompt::NamePrompt>,
    /// The question awaiting `y`/`n`; while `Some` it owns every key.
    pub pending: Option<super::prompt::Pending>,
}

/// What the rows read: the frame generation (every lane value change) and
/// the config version (a named expression redefined).
pub(crate) fn rows_key(frame: &Frame) -> (u64, u64) {
    (frame.generation(), frame.config_version())
}

impl ScopeDialogState {
    fn new(first: Layer) -> Self {
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
        }
    }

    pub(crate) fn cursor_row(&self) -> Option<&Row> {
        self.rows.rows.get(self.cursor)
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

/// The Change arm's pure half: the text step's draft, a name prompt's draft,
/// or the Saved screen's filter while it is filtering. Current ignores the
/// field.
pub(crate) fn on_query_changed(state: &mut ScopeDialogState, text: &str) {
    // A question owns the keyboard; the field is not its to edit.
    if state.pending.is_some() {
        return;
    }
    if in_text_step(state) {
        state.text_draft = text.to_string();
        state.error = None;
    } else if super::prompt::in_name_prompt(state) {
        super::prompt::on_draft_changed(state, text);
    } else if super::saved_view::filtering(state) {
        state.saved.set_query(text);
    }
}

/// Open on Current (`frame::scope`, `mod+o`, the scope bar's `+`). A no-op
/// when the dialog is already on top; refused with a notice when it is lower
/// in the stack (`dialog::can_open`).
pub(crate) fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    open_on(view, Layer::Current, window, cx);
}

/// Open on the Saved screen (`frame::scope_saved`, the load glyph,
/// `config::scopes`, `config::expressions`). Saved is the bottom layer, so
/// a commit or `escape` closes the dialog. With the dialog already on top
/// on Current, Saved is pushed over it instead, as `o` does; on any other
/// layer this is a no-op.
pub(crate) fn open_saved(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.top_kind() == Some(dialog::DialogKind::Scope) {
        if let Some(state) = view.scope_dialog.as_mut()
            && matches!(state.layers.top(), Layer::Current)
        {
            super::saved_view::push(state);
            view.refresh_dialog_rows(cx);
            dialog::sync_dialog_text(view, window, cx);
            cx.notify();
        }
        return;
    }
    open_on(view, Layer::Saved, window, cx);
}

/// The one-shot save door (the scope bar's save chip,
/// `scope::save_current`): the save prompt alone, so its commit or `escape`
/// closes the dialog. An empty scope refuses on the status bar and opens
/// nothing. With the dialog already on top on Current the prompt is pushed
/// over it instead, as `s` does; on any other layer this is a no-op.
pub(crate) fn open_save(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.top_kind() == Some(dialog::DialogKind::Scope) {
        if view
            .scope_dialog
            .as_ref()
            .is_some_and(|s| matches!(s.layers.top(), Layer::Current) && s.pending.is_none())
        {
            super::prompt::push_save(view, cx);
            view.refresh_dialog_rows(cx);
            dialog::sync_dialog_text(view, window, cx);
            cx.notify();
        }
        return;
    }
    let (empty, seed) = {
        let frame = view.target_frame().read(cx);
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
    if !open_on(view, Layer::Step(Step::SaveScope), window, cx) {
        return;
    }
    if let Some(state) = view.scope_dialog.as_mut() {
        state.prompt = Some(super::prompt::save_prompt(seed));
    }
    view.refresh_dialog_rows(cx);
    dialog::sync_dialog_text(view, window, cx);
    cx.notify();
}

/// Open the dialog with `first` as its bottom layer. Returns whether it
/// opened (`dialog::can_open` refuses one lower in the stack).
fn open_on(
    view: &mut ShellView,
    first: Layer,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if !dialog::can_open(view, dialog::DialogKind::Scope) {
        return false;
    }
    view.scope_dialog = Some(ScopeDialogState::new(first));
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
    // Reads the layers, so one registration serves both doors: Back shows
    // only while Saved sits over Current.
    dialog::set_back(
        view,
        super::saved_view::back_available,
        super::saved_view::back,
    );
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
    shell.target_frame().update(cx, |f, cx| {
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
    if super::prompt::in_name_prompt(state) {
        return super::prompt::prompt_key(shell, ks, window, cx);
    }
    if super::saved_view::in_saved(state) {
        return super::saved_view::handle_key(shell, ks, window, cx);
    }
    // The error describes the last action only: every claimed key drops it,
    // and the refusing action sets it again. An unclaimed key puts it back.
    let prior_error = state.error.take();
    let plain = !ks.mods.is_chord();
    match (ks.key.as_str(), plain, ks.mods.shift) {
        ("j" | "down", true, false) => move_cursor(shell, 1),
        ("k" | "up", true, false) => move_cursor(shell, -1),
        ("d", true, false) => remove_cursor_row(shell, cx),
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
        ("o", true, false) => {
            if let Some(state) = shell.scope_dialog.as_mut() {
                super::saved_view::push(state);
            }
        }
        ("s", true, false) => {
            super::prompt::push_save(shell, cx);
            dialog::sync_dialog_text(shell, window, cx);
        }
        ("enter" | "e", true, false) => {
            open_cursor_row(shell, window, cx);
            return true;
        }
        ("t", true, false) => {
            enter_text_step(shell, window, cx);
            return true;
        }
        ("i", true, false) => inline_cursor_row(shell, cx),
        ("s", false, false) if ks.mods == shell.services.mod_alias => {
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
/// clears the filter) and returns to Current; Escape returns with nothing
/// changed, and is claimed here so the shell's unclaimed-escape fallback
/// does not close the whole dialog. Every other key is the field's, so
/// Current's verbs type as text.
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

fn move_cursor(shell: &mut ShellView, delta: i64) {
    let Some(state) = shell.scope_dialog.as_mut() else {
        return;
    };
    let len = state.rows.rows.len();
    state.cursor = crate::vimnav::apply(state.cursor, len, crate::vimnav::NavCommand::Move(delta));
    // Re-take the identity, or a re-derive would put the cursor back on the
    // row it left.
    state.cursor_id = state.rows.rows.get(state.cursor).map(|r| r.id.clone());
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
/// reference's definition in the Expressions object dialog, the text in
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
        RowKind::Named { name, .. } => crate::shell::objectdialog::render::open_object(
            shell,
            crate::shell::objectdialog::Domain::Expressions,
            &name,
            window,
            cx,
        ),
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
        .target_frame()
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

/// Open the text step over Current, seeded with the lane's text so `enter`
/// on the text row edits it rather than retyping it.
///
/// A no-op when the step is already open: re-seeding would drop what was
/// typed, and a second step layer would need two `escape`s to leave.
fn enter_text_step(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if shell.scope_dialog.as_ref().is_none_or(in_text_step) {
        return;
    }
    let seed = shell
        .target_frame()
        .read(cx)
        .scope()
        .text
        .clone()
        .unwrap_or_default();
    if let Some(state) = shell.scope_dialog.as_mut() {
        state.text_draft = seed;
        state.error = None;
        state.layers.push(Layer::Step(Step::Text));
    }
    dialog::sync_dialog_text(shell, window, cx);
    cx.notify();
}

/// Whether Current's rows and controls take the pointer: only while Current
/// itself is on top with no question up. Under a step drawn on this body
/// (the text step, a name prompt) they are its preview.
fn takes_pointer(state: &ScopeDialogState) -> bool {
    matches!(state.layers.top(), Layer::Current) && state.pending.is_none()
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
    if super::saved_view::in_saved(state) {
        return super::saved_view::build(shell, state, entity, cx);
    }
    let theme = cx.theme();
    let paint = crate::shell::listrow::row_paint(theme);
    let muted = theme.muted_foreground;
    let danger = theme.danger;
    let radius = theme.radius;
    let mono = crate::fonts::MONO;

    let mut list = v_flex()
        .id("scope-dialog-rows")
        .w_full()
        .gap_0p5()
        .debug_selector(|| "scope-dialog".to_string());
    if state.rows.contradiction {
        list = list.child(
            div()
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
                .child(names.title)
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
            let highlighted = i == state.cursor;
            let el = h_flex()
                .id(("scope-dialog-row", i))
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
                    // step, a name prompt) are its preview, not controls: a
                    // press there would move Current's cursor under the
                    // field, and a double-click would open a row's step
                    // over the typing.
                    if !shell.scope_dialog.as_ref().is_some_and(takes_pointer) {
                        return;
                    }
                    if let Some(state) = shell.scope_dialog.as_mut() {
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
        None => dialog::hint_rows(&hints(shell, state)),
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

fn hints(shell: &ShellView, state: &ScopeDialogState) -> Vec<Hint> {
    // Inside the text step only its own two keys act; Current's verbs type.
    if in_text_step(state) {
        return vec![
            Hint::new(HintRow::Go, &["enter"], "set text").selector("scope-dialog-hint-set-text"),
            Hint::new(HintRow::Go, &["escape"], "back"),
        ];
    }
    if super::prompt::in_name_prompt(state) {
        return super::prompt::hints(state);
    }
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
                mods: shell.services.mod_alias,
            },
            "name",
        )),
        _ => {}
    }
    hints.extend([
        Hint::new(HintRow::Go, &["enter"], "edit row").selector("scope-dialog-hint-edit-row"),
        Hint::new(HintRow::Go, &["o"], "saved…"),
        Hint::new(HintRow::Go, &["s"], "save as…"),
        Hint::new(HintRow::Go, &["escape"], "close"),
    ]);
    hints
}
