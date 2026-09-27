//! Edit the frame's expression layer in the shared dialog Input. The dialog
//! has three modes ([`Mode`]), chosen by the door that opens it:
//!
//! - **Whole** (`frame::scope_expression`): seeded with the whole
//!   expression; Enter replaces it and an empty field clears it.
//! - **Term** (a click on one of the toolbar's term chips): seeded with
//!   that top-level `and` term (`Expr::conjuncts`); Enter replaces that
//!   term alone and an empty field removes it. The other terms keep their
//!   order. The mode carries the seeded term: if the scope changed
//!   underneath so that index no longer holds it, an edit or a removal
//!   refuses inline rather than touch a different term.
//! - **Add** (`frame::add_expression`, the toolbar's add-a-filter menu):
//!   empty; Enter joins the typed expression to the current one with
//!   `and` (or sets it when there is none), and an empty field closes
//!   without a change.
//!
//! Every commit goes through `Frame`'s undoable `set_scope` path. A parse
//! error stays inline in every mode, and editing clears the error. Escape
//! closes without applying. Each open seeds a fresh draft from the current
//! frame.
//!
//! While typing, `expr_suggest` lists what fits at the caret and warns
//! about schema problems. Enter refuses a syntax error or an unknown
//! column (`geode_core::scope::complete::check`). An operator that means
//! nothing on its column fails at query time, as before.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use geode_core::scope::complete::ExprVocab;
use geode_core::scope::{Expr, Scope, parse_expr};

use crate::exprcomplete::ExprCompletion;

use crate::frame::Frame;
use crate::keymap::{Keystroke, Modifiers};

use super::ShellView;
use super::chip;
use super::dialog;
use super::picker::{Hint, hint_row};
use super::scale;

// ---------------------------------------------------------------------
// Pure core — no gpui.
// ---------------------------------------------------------------------

/// Which part of the expression the dialog edits.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// The whole expression.
    Whole,
    /// Top-level `and` term `index`, in `Expr::conjuncts` order, and the
    /// term the dialog was seeded with. A commit edits the term only while
    /// it is still `seeded` at `index`: the index alone could name a
    /// different term after the scope changed underneath.
    Term { index: usize, seeded: Expr },
    /// A new expression joined to the current one with `and`.
    Add,
}

impl Mode {
    /// Term mode on term `index` of `expression`, or `None` when the
    /// expression has no such term.
    pub fn term(index: usize, expression: Option<&Expr>) -> Option<Mode> {
        let seeded = expression?.conjuncts().get(index).map(|t| (*t).clone())?;
        Some(Mode::Term { index, seeded })
    }

    pub fn title(&self) -> &'static str {
        match self {
            Mode::Whole => "Scope expression",
            Mode::Term { .. } => "Edit scope term",
            Mode::Add => "Add scope expression",
        }
    }

    /// The one-line note under the field, if the mode needs one.
    pub fn note(&self) -> Option<&'static str> {
        match self {
            Mode::Whole => None,
            Mode::Term { .. } => Some("Edits this term only; the other terms stay."),
            Mode::Add => Some("Joined to the current expression with and."),
        }
    }

    fn hints(&self) -> &'static [Hint] {
        match self {
            Mode::Whole => WHOLE_HINTS,
            Mode::Term { .. } => TERM_HINTS,
            Mode::Add => ADD_HINTS,
        }
    }

    /// The text the field opens with.
    pub fn seed(&self, expression: Option<&Expr>) -> String {
        match self {
            Mode::Whole => expression.map(ToString::to_string).unwrap_or_default(),
            Mode::Term { seeded, .. } => seeded.to_string(),
            Mode::Add => String::new(),
        }
    }
}

/// The inline refusal when a term commit finds its term gone or changed.
pub const TERM_GONE: &str = "This term is no longer in the scope expression";

/// The dialog's state: its mode, the last failed commit's message, and the
/// field's suggestions.
#[derive(Debug)]
pub struct ScopeExprState {
    pub mode: Mode,
    pub error: Option<String>,
    pub completion: ExprCompletion,
}

impl ScopeExprState {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            error: None,
            completion: ExprCompletion::default(),
        }
    }
}

/// The scope a new expression will be ANDed with, which narrows its value
/// suggestions. Whole replaces the expression, so it is dropped. Add joins
/// it, so it is kept. Term keeps the other terms.
pub fn request_scope(mode: &Mode, current: &Scope) -> Scope {
    let mut scope = current.clone();
    scope.expression = match mode {
        Mode::Whole => None,
        Mode::Add => current.expression.clone(),
        Mode::Term { index, .. } => current.expression.as_ref().and_then(|e| {
            Expr::from_conjuncts(
                e.conjuncts()
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| i != index)
                    .map(|(_, t)| t.clone()),
            )
        }),
    };
    scope
}

/// Trim and parse the draft, treating an empty value as no expression.
/// Errors label the parser's one-based byte offset as a column. A parsed
/// draft that names a column the schema does not know is refused with the
/// first schema warning; an empty `vocab` (no schema loaded) checks nothing.
pub fn commit_text(text: &str, vocab: &ExprVocab) -> Result<Option<Expr>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let expr = parse_expr(text).map_err(|e| format!("{} at column {}", e.message, e.caret + 1))?;
    if let Some(w) = geode_core::scope::complete::check(text, vocab, None)
        .into_iter()
        .next()
    {
        return Err(w.message);
    }
    Ok(Some(expr))
}

/// Apply `text` to `frame` as `mode` says. `Ok(changed)` means the dialog
/// closes; `Err(message)` stays inline (a parse error, an unknown column,
/// or [`TERM_GONE`]). Every change goes through `Frame::set_scope`, so undo
/// sees it.
pub fn apply(
    frame: &mut Frame,
    mode: &Mode,
    text: &str,
    vocab: &ExprVocab,
) -> Result<bool, String> {
    let parsed = commit_text(text, vocab)?;
    match mode {
        Mode::Whole => {
            let mut scope = frame.scope().clone();
            scope.expression = parsed;
            Ok(frame.set_scope(scope))
        }
        Mode::Term { index, seeded } => frame
            .replace_expression_term(*index, seeded, parsed)
            .map_err(|_| TERM_GONE.to_string()),
        Mode::Add => Ok(parsed.is_some_and(|e| frame.append_expression(e))),
    }
}

// ---------------------------------------------------------------------
// gpui: the modal.
// ---------------------------------------------------------------------

const WIDTH: f32 = 640.0;

const WHOLE_HINTS: &[Hint] = &[
    Hint::Key("tab"),
    Hint::Text("insert ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("set · empty clears ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

const TERM_HINTS: &[Hint] = &[
    Hint::Key("tab"),
    Hint::Text("insert ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("set · empty removes the term ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

const ADD_HINTS: &[Hint] = &[
    Hint::Key("tab"),
    Hint::Text("insert ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("add ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

/// Open the dialog on term `index` of the frame's current expression (a
/// term chip's click); a no-op when the expression has no such term.
pub fn open_term(
    view: &mut ShellView,
    index: usize,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let Some(mode) = Mode::term(index, view.frame.read(cx).scope().expression.as_ref()) else {
        return;
    };
    open(view, mode, window, cx);
}

/// Open the dialog in `mode`, seeded from the frame's current expression.
/// A no-op if a modal is already open, like every other `open` here. The
/// seed is written AFTER the door (`open_shell_dialog_with_key` resets the
/// field to empty), and `set_value` emits no `Change`, so the state starts
/// with no error regardless.
pub fn open(view: &mut ShellView, mode: Mode, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    let seed = mode.seed(view.frame.read(cx).scope().expression.as_ref());
    let title = mode.title();
    view.scope_expr_dialog = Some(ScopeExprState::new(mode));
    // A row click needs the shell entity to accept through; `build` is
    // handed only `&ShellView`.
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        SharedString::new_static(title),
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
    view.dialog_input
        .update(cx, |input, cx| input.set_value(seed, window, cx));
    super::expr_suggest::refresh(view, cx);
}

/// Typing clears the last error (the `Change` subscription arm in
/// `shell/mod.rs` calls this).
pub(crate) fn on_query_changed(state: &mut ScopeExprState) {
    state.error = None;
}

fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if super::expr_suggest::handle_key(shell, ks, window, cx) {
        return true;
    }
    if ks.mods != Modifiers::NONE || ks.key != "enter" {
        return false;
    }
    let Some(mode) = shell.scope_expr_dialog.as_ref().map(|s| s.mode.clone()) else {
        return false;
    };
    let text = shell.dialog_input.read(cx).value().to_string();
    let vocab = shell.expr_vocab.clone();
    let outcome = shell.frame.update(cx, |f, cx| {
        let outcome = apply(f, &mode, &text, &vocab);
        if outcome == Ok(true) {
            cx.notify();
        }
        outcome
    });
    match outcome {
        Ok(_) => shell.close_modal(window, cx),
        Err(message) => {
            if let Some(state) = shell.scope_expr_dialog.as_mut() {
                state.error = Some(message);
            }
            cx.notify();
        }
    }
    true
}

fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.scope_expr_dialog.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let mut column = v_flex()
        .gap_2()
        .w(scale::design(WIDTH))
        .child(dialog::filter_row(&shell.dialog_input, None, cx));
    let entity = entity.clone();
    column = column.child(super::expr_suggest::render(
        &state.completion,
        &shell.expr_scroll,
        theme,
        move |named, label, window, cx| {
            entity.update(cx, |shell, cx| {
                super::expr_suggest::accept_row(shell, named, label, window, cx)
            });
        },
    ));
    if let Some(note) = state.mode.note() {
        column = column.child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "scope-expr-note".to_string())
                .child(SharedString::new_static(note)),
        );
    }
    if let Some(err) = &state.error {
        // Use the chip paint helper so error text meets its readability floor.
        column = column.child(
            div()
                .text_sm()
                .text_color(chip::chip_paint(theme, chip::Tone::DangerText).text)
                .debug_selector(|| "scope-expr-error".to_string())
                .child(err.clone()),
        );
    }
    column
        .child(hint_row(
            state.mode.hints(),
            "scope-expr-hints",
            WIDTH,
            theme.muted_foreground,
            theme.border,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::groupings::GroupingSlots;
    use geode_core::scope::Scope;
    use geode_core::scopes::SavedScopes;

    fn frame_with(expr: Option<&str>) -> Frame {
        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        f.set_scope(Scope {
            expression: expr.map(|t| parse_expr(t).unwrap()),
            ..Scope::default()
        });
        f
    }

    fn terms(f: &Frame) -> Vec<String> {
        f.scope()
            .expression
            .as_ref()
            .map(|e| e.conjuncts().iter().map(|t| t.to_string()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn empty_clears_and_a_broken_expression_names_the_column() {
        assert_eq!(commit_text("   ", &ExprVocab::default()).unwrap(), None);
        assert!(
            commit_text("book = 'BK000'", &ExprVocab::default())
                .unwrap()
                .is_some()
        );
        let err = commit_text("book =", &ExprVocab::default()).unwrap_err();
        assert!(err.contains("at column"), "{err}");
    }

    fn term_mode(f: &Frame, index: usize) -> Mode {
        Mode::term(index, f.scope().expression.as_ref()).expect("the term exists")
    }

    #[test]
    fn each_mode_seeds_its_own_text() {
        let e = parse_expr("a = 1 and (b = 2 or c = 3)").unwrap();
        assert_eq!(
            Mode::Whole.seed(Some(&e)),
            "(a = 1) and ((b = 2) or (c = 3))"
        );
        assert_eq!(Mode::Whole.seed(None), "");
        let term = Mode::term(1, Some(&e)).unwrap();
        assert_eq!(term.seed(Some(&e)), "(b = 2) or (c = 3)");
        assert_eq!(Mode::term(2, Some(&e)), None, "no such term");
        assert_eq!(Mode::term(0, None), None);
        assert_eq!(Mode::Add.seed(Some(&e)), "");
    }

    #[test]
    fn whole_mode_replaces_and_empty_clears() {
        let mut f = frame_with(Some("a = 1 and b = 2"));
        assert_eq!(
            apply(&mut f, &Mode::Whole, "c = 3", &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(terms(&f), vec!["c = 3"]);
        assert_eq!(
            apply(&mut f, &Mode::Whole, "  ", &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(f.scope().expression, None);
    }

    #[test]
    fn term_mode_replaces_only_its_term_and_empty_removes_it() {
        let mut f = frame_with(Some("a = 1 and b = 2 and c = 3"));
        let mode = term_mode(&f, 1);
        assert_eq!(
            apply(&mut f, &mode, "x = 9", &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(terms(&f), vec!["a = 1", "x = 9", "c = 3"]);
        let mode = term_mode(&f, 0);
        assert_eq!(apply(&mut f, &mode, "", &ExprVocab::default()), Ok(true));
        assert_eq!(terms(&f), vec!["x = 9", "c = 3"]);
    }

    /// The scope changed underneath with the same term count: index 1
    /// holds `y = 2`, not the seeded `b = 2`, so an edit and a removal
    /// both refuse, and so does an index that no longer exists.
    #[test]
    fn a_term_changed_underneath_refuses_edit_and_removal() {
        let mut f = frame_with(Some("a = 1 and b = 2"));
        let mode = term_mode(&f, 1);
        f.set_scope(Scope {
            expression: Some(parse_expr("x = 1 and y = 2").unwrap()),
            ..Scope::default()
        });
        assert_eq!(
            apply(&mut f, &mode, "b = 3", &ExprVocab::default()),
            Err(TERM_GONE.to_string())
        );
        assert_eq!(
            apply(&mut f, &mode, "", &ExprVocab::default()),
            Err(TERM_GONE.to_string())
        );
        assert_eq!(terms(&f), vec!["x = 1", "y = 2"]);
        f.set_scope(Scope {
            expression: Some(parse_expr("x = 1").unwrap()),
            ..Scope::default()
        });
        assert_eq!(
            apply(&mut f, &mode, "b = 3", &ExprVocab::default()),
            Err(TERM_GONE.to_string())
        );
        assert_eq!(terms(&f), vec!["x = 1"]);
    }

    #[test]
    fn add_mode_joins_with_and_sets_when_none_and_empty_changes_nothing() {
        let mut f = frame_with(None);
        assert_eq!(
            apply(&mut f, &Mode::Add, "   ", &ExprVocab::default()),
            Ok(false),
            "empty: no change"
        );
        assert_eq!(f.scope().expression, None);
        assert_eq!(
            apply(&mut f, &Mode::Add, "a = 1", &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(terms(&f), vec!["a = 1"]);
        assert_eq!(
            apply(&mut f, &Mode::Add, "b = 2 or c = 3", &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(
            f.scope()
                .expression
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("(a = 1) and ((b = 2) or (c = 3))"),
            "existing and (new)"
        );
    }

    #[test]
    fn a_parse_error_changes_nothing_in_every_mode() {
        let f0 = frame_with(Some("a = 1"));
        for mode in [Mode::Whole, term_mode(&f0, 0), Mode::Add] {
            let mut f = frame_with(Some("a = 1"));
            let err = apply(&mut f, &mode, "book =", &ExprVocab::default()).unwrap_err();
            assert!(err.contains("at column"), "{mode:?}: {err}");
            assert_eq!(terms(&f), vec!["a = 1"], "{mode:?}");
        }
    }

    #[test]
    fn request_scope_narrows_by_what_the_new_text_is_anded_with() {
        use geode_core::scope::{DimensionSelection, Scope};
        let current = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["A".into()],
            }],
            expression: Some(parse_expr("x = 'a' and y = 'b'").unwrap()),
            ..Scope::default()
        };
        assert_eq!(request_scope(&Mode::Whole, &current).expression, None);
        assert_eq!(
            request_scope(&Mode::Whole, &current).dimensions,
            current.dimensions
        );
        assert_eq!(
            request_scope(&Mode::Add, &current).expression,
            current.expression
        );
        let term = Mode::term(0, current.expression.as_ref()).unwrap();
        assert_eq!(
            request_scope(&term, &current)
                .expression
                .map(|e| e.to_string()),
            Some("y = 'b'".to_string())
        );
    }

    #[test]
    fn commit_refuses_an_unknown_column_when_a_schema_exists() {
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::dimensions::DerivedDimensions;
        use geode_core::schema::SchemaSpec;
        let datasets = LayerDoc::builtin(
            "datasets",
            "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\n",
        )
        .unwrap();
        let (schema, _) = SchemaSpec::from_doc(&merge_docs("datasets", &[datasets]));
        let vocab = ExprVocab::new(&schema, &DerivedDimensions::default());
        assert_eq!(
            commit_text("bokk = 'A'", &vocab),
            Err("unknown column 'bokk'; did you mean 'book'?".to_string())
        );
        assert!(commit_text("book = 'A'", &vocab).is_ok());
        assert!(
            commit_text("bokk = 'A'", &ExprVocab::default()).is_ok(),
            "no schema, no check"
        );
    }
}
