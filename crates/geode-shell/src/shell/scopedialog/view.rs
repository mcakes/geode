//! The Scope dialog's Current screen: the lane's scope as rows by
//! ingredient, refreshed when the frame changes under it. Steps that exist
//! as their own modals (the dimension picker, the expression dialog, the
//! saved-scope chooser, the save prompt) are pushed over this one; their
//! commits pop back here. The text step is drawn here.

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
use super::state::{Layer, Layers, Step};
use crate::frame::FrameViewMut;
use crate::shell::{ShellView, dialog, scale};

const WIDTH: f32 = 560.0;
const ROW_HEIGHT: f32 = 28.0;

/// Section headers as painted: the copy `Dimensions`, `Expressions`, `Text`
/// in capitals.
const DIMENSIONS_TITLE: &str = "DIMENSIONS";
const EXPRESSIONS_TITLE: &str = "EXPRESSIONS";
const TEXT_TITLE: &str = "TEXT";
const TERM_DETAIL: &str = "unnamed";
const TEXT_DETAIL: &str = "textual columns";

/// One row's painted strings, prepared when the rows derive so `build`
/// only clones them instead of formatting on every frame.
pub(crate) struct RowDisplay {
    pub label: SharedString,
    pub detail: SharedString,
    /// A named reference that does not resolve: painted in the danger color.
    pub broken: bool,
}

impl RowDisplay {
    fn of(row: &Row) -> Self {
        match &row.kind {
            RowKind::Dimension { column, values } => RowDisplay {
                label: column.clone().into(),
                detail: values.join(", ").into(),
                broken: false,
            },
            RowKind::Term { text, .. } => RowDisplay {
                label: text.clone().into(),
                detail: TERM_DETAIL.into(),
                broken: false,
            },
            RowKind::Named { name, state } => {
                let (detail, broken) = match state {
                    NamedState::Valid { text } => (text.clone(), false),
                    NamedState::Invalid { reason, .. } => (reason.clone(), true),
                    NamedState::Missing => (format!("'{name}' is not defined"), true),
                };
                RowDisplay {
                    label: format!("≡ {name}").into(),
                    detail: detail.into(),
                    broken,
                }
            }
            RowKind::Text { text } => RowDisplay {
                label: format!("\"{text}\"").into(),
                detail: TEXT_DETAIL.into(),
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
}

/// What the rows read: the frame generation (every lane value change) and
/// the config version (a named expression redefined).
pub(crate) fn rows_key(frame: &Frame) -> (u64, u64) {
    (frame.generation(), frame.config_version())
}

impl ScopeDialogState {
    fn new() -> Self {
        ScopeDialogState {
            layers: Layers::open(Layer::Current),
            rows: CurrentRows::default(),
            display: Vec::new(),
            title: None,
            // Never a real key: the first refresh always derives.
            key: (u64::MAX, u64::MAX),
            cursor: 0,
            cursor_id: None,
            text_draft: String::new(),
            error: None,
        }
    }

    pub(crate) fn cursor_row(&self) -> Option<&Row> {
        self.rows.rows.get(self.cursor)
    }

    pub(crate) fn is_current(&self, key: (u64, u64)) -> bool {
        self.key == key
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
    }
}

pub(crate) fn in_text_step(state: &ScopeDialogState) -> bool {
    matches!(state.layers.top(), Layer::Step(Step::Text))
}

/// The Change arm's pure half: only the text step types.
pub(crate) fn on_query_changed(state: &mut ScopeDialogState, text: &str) {
    if in_text_step(state) {
        state.text_draft = text.to_string();
        state.error = None;
    }
}

/// Open on Current (`frame::scope`, `mod+o`, the scope bar's `+`). A no-op
/// when the dialog is already on top; refused with a notice when it is lower
/// in the stack (`dialog::can_open`).
pub(crate) fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if !dialog::can_open(view, dialog::DialogKind::Scope) {
        return;
    }
    view.scope_dialog = Some(ScopeDialogState::new());
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
}

/// Run one edit on the dialog's lane (`target_frame`, so a pinned workspace
/// is edited, not the shared lane) and notify when the frame generation
/// moved: a provenance-only change (a clear of an empty scope) moves it too,
/// and a notify keyed on the method's boolean would leave observers stale.
fn edit_lane<R>(
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
    let Some(state) = shell.scope_dialog.as_ref() else {
        return false;
    };
    if in_text_step(state) {
        return text_step_key(shell, ks, window, cx);
    }
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
        _ => return false,
    }
    // Every edit above may have changed the lane: re-derive now so the next
    // key and the next paint read the same rows.
    shell.refresh_dialog_rows(cx);
    cx.notify();
    true
}

/// The text step's keys; the step has no route onto the screen yet.
fn text_step_key(
    _shell: &mut ShellView,
    _ks: &Keystroke,
    _window: &mut Window,
    _cx: &mut Context<ShellView>,
) -> bool {
    false
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

fn section_name(section: Section) -> (&'static str, &'static str) {
    match section {
        Section::Dimensions => (DIMENSIONS_TITLE, "dimensions"),
        Section::Expressions => (EXPRESSIONS_TITLE, "expressions"),
        Section::Text => (TEXT_TITLE, "text"),
    }
}

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
    for section in [Section::Dimensions, Section::Expressions, Section::Text] {
        let (title, slug) = section_name(section);
        list = list.child(
            div()
                .flex_shrink_0()
                .px_2()
                .pt_2()
                .pb_0p5()
                .text_xs()
                .text_color(muted)
                .debug_selector(move || format!("scope-dialog-section-{slug}"))
                .child(title),
        );
        if state.rows.section_is_empty(section) {
            list = list.child(
                div()
                    .px_2()
                    .h(scale::design(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .text_sm()
                    .text_color(muted)
                    .debug_selector(move || format!("scope-dialog-empty-{slug}"))
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
            el = match &row.kind {
                RowKind::Dimension { .. } => el
                    .child(div().w(scale::design(120.0)).text_color(muted).child(label))
                    .child(div().flex_1().truncate().child(detail)),
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
            el = el.on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                click.update(cx, |shell, cx| {
                    if let Some(state) = shell.scope_dialog.as_mut() {
                        state.cursor = i;
                        state.cursor_id = state.rows.rows.get(i).map(|r| r.id.clone());
                    }
                    dialog::sync_dialog_text(shell, window, cx);
                    cx.notify();
                });
            });
            list = list.child(el);
        }
    }

    let mut body = v_flex().gap_2().w(scale::design(WIDTH)).child(list);
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
    body.child(
        v_flex()
            .w_full()
            .gap_1()
            .pt_2()
            .border_t_1()
            .border_color(theme.border)
            .child(dialog::hint_rows(&hints(shell, state))),
    )
    .into_any_element()
}

fn hints(_shell: &ShellView, _state: &ScopeDialogState) -> Vec<Hint> {
    vec![
        Hint::new(HintRow::Move, &["j", "k"], "row"),
        Hint::new(HintRow::Edit, &["d"], "remove"),
        Hint::new(HintRow::Edit, &["shift+d"], "clear all"),
        Hint::new(HintRow::Edit, &["u"], "undo"),
        Hint::new(HintRow::Edit, &["ctrl+r"], "redo"),
        Hint::new(HintRow::Go, &["escape"], "close"),
    ]
}
