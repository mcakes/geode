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
//! Whole and Add also stage named expressions: a named row's accept
//! stages its name as a `≡ name` chip above the field, and Enter applies
//! the staged names with the text in one `set_scope`. Whole opens with the
//! frame's names staged and Enter replaces them (an empty field with
//! nothing staged clears both); Add opens with none and Enter appends the
//! ones the frame lacks. Backspace at the field's start with no selection
//! removes the last chip; a chip's `×` removes that chip. Term mode offers
//! and stages no names.
//!
//! `mod+s` in Whole and Add turns the field into a name entry for the
//! typed text (suggestions off, staged chips kept). Enter refuses an
//! invalid or taken name, an empty text or one Enter itself would refuse,
//! each inline with the entry still open; otherwise it writes
//! `[name] expression = "<text>"` to the user layer of `expressions.toml`,
//! refreshes the frame's definitions from the pending config (so the name
//! resolves before the write lands), empties the field and stages the
//! name. Escape puts the text back. In Term mode `mod+s` only says where
//! saving lives.
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
use gpui::{AnyElement, App, Context, Entity, MouseButton, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};

use geode_core::named::{NamedExpr, NamedExpressions};
use geode_core::scope::complete::ExprVocab;
use geode_core::scope::{Expr, Scope, parse_expr};

use crate::exprcomplete::{ExprCompletion, NamedOffer};

use crate::frame::Frame;
use crate::keymap::{Keystroke, Modifiers};

use super::ShellView;
use super::chip;
use super::control::{self, PointerStates as _};
use super::dialog;
use super::picker::{Hint, hint_row_with};
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

    /// The footer's keys; with chips staged it also names `backspace`,
    /// which removes the last chip from the field's start.
    fn hints(&self, staged: bool) -> &'static [Hint] {
        match (self, staged) {
            (Mode::Whole, false) => WHOLE_HINTS,
            (Mode::Whole, true) => WHOLE_STAGED_HINTS,
            (Mode::Term { .. }, _) => TERM_HINTS,
            (Mode::Add, false) => ADD_HINTS,
            (Mode::Add, true) => ADD_STAGED_HINTS,
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

/// `mod+s` in Term mode, which edits one term and has nothing whole to name.
pub const SAVE_FROM_TERM: &str = "save a named expression from the whole or add dialog";

/// The name entry's label.
const NAMING_LABEL: &str = "Save this expression as a named expression · name";

/// The dialog's state: its mode, the named expressions staged beside the
/// text, the last failed commit's message, and the field's suggestions.
#[derive(Debug)]
pub struct ScopeExprState {
    pub mode: Mode,
    /// Names Enter applies with the text, in order and without repeats.
    /// Always empty in Term mode, which edits one term of the text.
    pub staged: Vec<String>,
    /// `Some(text)` while the field takes a name for `text` (`mod+s`):
    /// the expression waits here so Escape can put it back.
    pub naming: Option<String>,
    pub error: Option<String>,
    pub completion: ExprCompletion,
}

impl ScopeExprState {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            staged: Vec::new(),
            naming: None,
            error: None,
            completion: ExprCompletion::default(),
        }
    }
}

/// The names `mode` opens with staged: the frame's own names in Whole
/// (Enter replaces them), none in Add and Term.
pub fn seed_staged(mode: &Mode, current: &Scope) -> Vec<String> {
    match mode {
        Mode::Whole => current.named.clone(),
        Mode::Term { .. } | Mode::Add => Vec::new(),
    }
}

/// The named rows the field offers: every defined name not already
/// staged, previewed by its text (or, when invalid, by its reason). Term
/// mode edits one term of the text, where a name has no place, so it
/// offers none.
pub fn named_offers(mode: &Mode, defined: &NamedExpressions, staged: &[String]) -> Vec<NamedOffer> {
    if matches!(mode, Mode::Term { .. }) {
        return Vec::new();
    }
    defined
        .names()
        .filter(|name| !staged.iter().any(|s| s == name))
        .filter_map(|name| {
            let (preview, broken) = match defined.get(name)? {
                NamedExpr::Valid { text, .. } => (text.clone(), false),
                NamedExpr::Invalid { reason, .. } => (reason.clone(), true),
            };
            Some(NamedOffer {
                name: name.to_string(),
                preview,
                broken,
            })
        })
        .collect()
}

/// The scope a new expression will be ANDed with, which narrows its value
/// suggestions. Whole replaces the expression and the names, so the
/// expression is dropped and the names are the staged ones. Add joins
/// both, so the expression is kept and the staged names join the frame's.
/// Term keeps the other terms. The in-progress text never narrows.
pub fn request_scope(mode: &Mode, current: &Scope, staged: &[String]) -> Scope {
    let mut scope = current.clone();
    match mode {
        Mode::Whole => scope.named = staged.to_vec(),
        Mode::Add => append_missing(&mut scope.named, staged),
        Mode::Term { .. } => {}
    }
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

/// Push each of `staged` that `named` lacks, keeping `named`'s order.
fn append_missing(named: &mut Vec<String>, staged: &[String]) {
    for name in staged {
        if !named.contains(name) {
            named.push(name.clone());
        }
    }
}

/// Apply `text` and the `staged` names to `frame` as `mode` says.
/// `Ok(changed)` means the dialog closes; `Err(message)` stays inline (a
/// parse error, an unknown column, or [`TERM_GONE`]). Every change goes
/// through `Frame::set_scope`, so undo sees it. Whole and Add change the
/// names and the expression in ONE `set_scope`: two calls would leave two
/// undo entries, and one undo would restore half the edit.
///
/// Whole sets the names to `staged` and the expression to the text, so
/// an empty field with nothing staged clears both. Add appends the staged
/// names the frame lacks and joins the text with `and`; with neither, it
/// changes nothing. Term ignores `staged` (it never has any).
pub fn apply(
    frame: &mut Frame,
    mode: &Mode,
    text: &str,
    staged: &[String],
    vocab: &ExprVocab,
) -> Result<bool, String> {
    let parsed = commit_text(text, vocab)?;
    match mode {
        Mode::Whole => {
            let mut scope = frame.scope().clone();
            scope.named = staged.to_vec();
            scope.expression = parsed;
            Ok(frame.set_scope(scope))
        }
        Mode::Term { index, seeded } => frame
            .replace_expression_term(*index, seeded, parsed)
            .map_err(|_| TERM_GONE.to_string()),
        Mode::Add => {
            let mut scope = frame.scope().clone();
            append_missing(&mut scope.named, staged);
            if let Some(term) = parsed {
                scope.expression = Some(crate::frame::and_join(scope.expression.take(), term));
            }
            Ok(frame.set_scope(scope))
        }
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

const WHOLE_STAGED_HINTS: &[Hint] = &[
    Hint::Key("tab"),
    Hint::Text("insert ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("backspace"),
    Hint::Text("remove chip ·"),
    Hint::Key("enter"),
    Hint::Text("set ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

const NAMING_HINTS: &[Hint] = &[
    Hint::Key("enter"),
    Hint::Text("save ·"),
    Hint::Key("escape"),
    Hint::Text("back"),
];

const ADD_STAGED_HINTS: &[Hint] = &[
    Hint::Key("tab"),
    Hint::Text("insert ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("backspace"),
    Hint::Text("remove chip ·"),
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
    let current = view.frame.read(cx).scope();
    let seed = mode.seed(current.expression.as_ref());
    let staged = seed_staged(&mode, current);
    let title = mode.title();
    view.scope_expr_dialog = Some(ScopeExprState {
        staged,
        ..ScopeExprState::new(mode)
    });
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
    sync_named_offers(view, cx);
    super::expr_suggest::refresh(view, cx);
}

/// Re-offer every defined name the open dialog has not staged. Runs
/// whenever the staged list or the frame's definitions change, so a
/// staged name is never offered twice and an unstaged one comes back.
pub(crate) fn sync_named_offers(view: &mut ShellView, cx: &mut Context<ShellView>) {
    let vocab = view.expr_vocab.clone();
    let Some(state) = view.scope_expr_dialog.as_mut() else {
        return;
    };
    // The name entry paints no suggestions; leaving it re-syncs.
    if state.naming.is_some() {
        return;
    }
    let offers = named_offers(
        &state.mode,
        view.frame.read(cx).named_expressions(),
        &state.staged,
    );
    state.completion.set_named_offers(offers, &vocab);
    cx.notify();
}

/// Stage `name` beside the text unless it is staged already. Term mode
/// never stages: it edits one term of the text.
pub(crate) fn stage(view: &mut ShellView, name: &str, cx: &mut Context<ShellView>) {
    let Some(state) = view.scope_expr_dialog.as_mut() else {
        return;
    };
    if matches!(state.mode, Mode::Term { .. }) || state.staged.iter().any(|s| s == name) {
        return;
    }
    state.staged.push(name.to_string());
    sync_named_offers(view, cx);
}

/// Remove staged `name` (a chip's `×`, or backspace on the last chip).
fn unstage(view: &mut ShellView, name: &str, cx: &mut Context<ShellView>) {
    let Some(state) = view.scope_expr_dialog.as_mut() else {
        return;
    };
    let before = state.staged.len();
    state.staged.retain(|s| s != name);
    if state.staged.len() != before {
        sync_named_offers(view, cx);
    }
}

/// Backspace with the caret at the field's start and no selection removes
/// the last staged chip. Anywhere else, or with a selection to delete,
/// the key stays the field's, so editing text never drops a name.
fn backspace_unstages(view: &mut ShellView, cx: &mut Context<ShellView>) -> bool {
    let Some(last) = view
        .scope_expr_dialog
        .as_ref()
        .and_then(|s| s.staged.last().cloned())
    else {
        return false;
    };
    let input = view.dialog_input.read(cx);
    if input.cursor() != 0 || !input.selected_range().is_empty() {
        return false;
    }
    unstage(view, &last, cx);
    true
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
    if shell
        .scope_expr_dialog
        .as_ref()
        .is_some_and(|s| s.naming.is_some())
    {
        return naming_key(shell, ks, window, cx);
    }
    if is_mod_s(shell, ks) {
        begin_naming(shell, window, cx);
        return true;
    }
    if super::expr_suggest::handle_key(shell, ks, window, cx) {
        return true;
    }
    if ks.mods == Modifiers::NONE && ks.key == "backspace" {
        // False hands the key back to the field, which deletes text.
        return backspace_unstages(shell, cx);
    }
    if ks.mods != Modifiers::NONE || ks.key != "enter" {
        return false;
    }
    let Some((mode, staged)) = shell
        .scope_expr_dialog
        .as_ref()
        .map(|s| (s.mode.clone(), s.staged.clone()))
    else {
        return false;
    };
    let text = shell.dialog_input.read(cx).value().to_string();
    let vocab = shell.expr_vocab.clone();
    let outcome = shell.frame.update(cx, |f, cx| {
        let outcome = apply(f, &mode, &text, &staged, &vocab);
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

/// `mod+s` as the user's alias spells it.
fn is_mod_s(shell: &ShellView, ks: &Keystroke) -> bool {
    ks.key == "s" && ks.mods == shell.services.mod_alias
}

/// Keys while the field takes a name. Enter saves and Escape goes back,
/// both claimed so the modal stays open; tab is claimed so focus never
/// leaves the entry, and `mod+s` again does nothing. Every other key is
/// the field's.
fn naming_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    if is_mod_s(shell, ks) {
        return true;
    }
    if ks.key == "tab" && !ks.mods.is_chord() {
        return true;
    }
    if ks.mods != Modifiers::NONE {
        return false;
    }
    match ks.key.as_str() {
        "enter" => {
            if let Err(message) = save_named(shell, window, cx)
                && let Some(state) = shell.scope_expr_dialog.as_mut()
            {
                state.error = Some(message);
            }
            cx.notify();
            true
        }
        "escape" => {
            let text = shell
                .scope_expr_dialog
                .as_mut()
                .and_then(|s| {
                    s.error = None;
                    s.naming.take()
                })
                .unwrap_or_default();
            leave_naming(shell, text, window, cx);
            true
        }
        _ => false,
    }
}

/// Stash the text and turn the field into a name entry. Term mode edits
/// one term, so it refuses with [`SAVE_FROM_TERM`] and changes nothing.
fn begin_naming(shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let text = shell.dialog_input.read(cx).value().to_string();
    let Some(state) = shell.scope_expr_dialog.as_mut() else {
        return;
    };
    if matches!(state.mode, Mode::Term { .. }) {
        state.error = Some(SAVE_FROM_TERM.to_string());
        cx.notify();
        return;
    }
    state.naming = Some(text);
    state.error = None;
    // A fresh completion paints nothing: the text's rows would otherwise
    // stay on screen under the name, and `completion_mut` feeds it
    // nothing until the entry closes.
    state.completion = ExprCompletion::default();
    shell
        .dialog_input
        .update(cx, |input, cx| input.set_value("", window, cx));
    cx.notify();
}

/// Close the name entry with `text` in the field, and rebuild the
/// suggestions the entry dropped.
fn leave_naming(
    shell: &mut ShellView,
    text: String,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(state) = shell.scope_expr_dialog.as_mut() {
        state.naming = None;
    }
    shell
        .dialog_input
        .update(cx, |input, cx| input.set_value(text, window, cx));
    sync_named_offers(shell, cx);
    super::expr_suggest::refresh(shell, cx);
    cx.notify();
}

/// Save the stashed text under the typed name, then stage the name. The
/// name is checked before the text, and every refusal leaves the entry
/// open with nothing written.
///
/// The frame's definitions are refreshed from the pending config at once:
/// the write promotes on a timer, and until then the staged name would
/// resolve as missing, painting its chip broken and refusing the tile's
/// query if Enter applied it.
fn save_named(
    shell: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> Result<(), String> {
    use super::objectdialog::{Domain, apply};
    let typed = shell.dialog_input.read(cx).value().to_string();
    let name = geode_core::config::check_object_name(&typed)
        .map_err(|reason| format!("name: {reason}"))?
        .to_string();
    {
        let pending = apply::config_with_pending(shell);
        let config = pending.as_ref().unwrap_or(&shell.services.config);
        if Domain::Expressions.is_reserved(&name) {
            return Err(format!("'{name}' is reserved"));
        }
        if Domain::Expressions.name_taken(config, &name) {
            return Err(format!("'{name}' already exists"));
        }
    }
    let text = shell
        .scope_expr_dialog
        .as_ref()
        .and_then(|s| s.naming.as_deref())
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("nothing to save — the expression is empty".to_string());
    }
    commit_text(&text, &shell.expr_vocab)?;
    let mut object = toml::Table::new();
    object.insert("expression".to_string(), toml::Value::String(text));
    apply::queue_object(
        shell,
        geode_core::config::EXPRESSIONS_DOC,
        &name,
        toml::Value::Table(object),
        cx,
    )?;
    if let Some(config) = apply::config_with_pending(shell) {
        let named = super::hot_reload::rebuild_named_expressions(&config);
        shell.frame.update(cx, |f, cx| {
            if f.replace_named_expressions(named) {
                cx.notify();
            }
        });
    }
    if let Some(state) = shell.scope_expr_dialog.as_mut()
        && !state.staged.contains(&name)
    {
        state.staged.push(name);
    }
    leave_naming(shell, String::new(), window, cx);
    Ok(())
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
    let mut column = v_flex().gap_2().w(scale::design(WIDTH));
    if !state.staged.is_empty() {
        column = column.child(staged_chips(shell, &state.staged, entity, cx));
    }
    column = column.child(if state.naming.is_some() {
        dialog::name_row(&shell.dialog_input, NAMING_LABEL, cx)
    } else {
        dialog::filter_row(&shell.dialog_input, None, cx)
    });
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
    let (hints, extra) = if state.naming.is_some() {
        (NAMING_HINTS, Vec::new())
    } else {
        let hints = state.mode.hints(!state.staged.is_empty());
        let extra = match state.mode {
            Mode::Term { .. } => Vec::new(),
            // The chip shows the user's own alias, so it is built here
            // rather than as a `Hint::Key`, which parses with none.
            Mode::Whole | Mode::Add => {
                let ks = Keystroke {
                    key: "s".to_string(),
                    mods: shell.services.mod_alias,
                };
                vec![
                    div().child("·").into_any_element(),
                    super::kbd::hint(&[ks], "save as named").into_any_element(),
                ]
            }
        };
        (hints, extra)
    };
    column
        .child(hint_row_with(
            hints,
            extra,
            "scope-expr-hints",
            WIDTH,
            theme.muted_foreground,
            theme.border,
        ))
        .into_any_element()
}

/// One `≡ name` chip per staged name, in staged order, each with a `×`
/// that unstages it. A name the frame cannot resolve (missing or invalid)
/// takes the danger tone, as its scope-bar chip does, since Enter would
/// leave the tile refusing to query. Ids derive from the name, so a chip
/// keeps its pointer state when a neighbour is removed.
fn staged_chips(
    shell: &ShellView,
    staged: &[String],
    entity: &Entity<ShellView>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let defined = shell.frame.read(cx).named_expressions();
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    // The `×` occludes the chip body, so its hover is measured against
    // the chip's rest fill on the modal panel (`popover`).
    let chip_states = control::paint(
        theme,
        control::Rest::Filled(chip_bg),
        theme.popover,
        chip_fg,
    );
    let broken = chip::chip_paint(theme, chip::Tone::Danger);
    let broken_fill = broken.fill.unwrap_or(theme.danger);
    let broken_states = control::for_chip(theme, &broken, theme.popover);
    let mut row = h_flex().gap_1().flex_wrap().items_center();
    for name in staged {
        let is_broken = !matches!(defined.get(name), Some(NamedExpr::Valid { .. }));
        let (fg, bg, close_states) = if is_broken {
            (broken.text, broken_fill, broken_states)
        } else {
            (chip_fg, chip_bg, chip_states)
        };
        let body = format!("scope-expr-staged-{name}");
        let close = format!("scope-expr-staged-close-{name}");
        let entity = entity.clone();
        let target = name.clone();
        row = row.child(
            h_flex()
                .id(SharedString::from(body.clone()))
                .items_center()
                .gap_1()
                .pl_2()
                .pr_0p5()
                .py_0p5()
                .rounded(theme.radius)
                .bg(bg)
                .text_color(fg)
                .text_sm()
                .debug_selector(move || body.clone())
                .child(SharedString::from(format!("≡ {name}")))
                .child(
                    div()
                        .id(SharedString::from(close.clone()))
                        .flex()
                        .items_center()
                        .justify_center()
                        // The toolbar chip's glyph box, so a staged chip
                        // and its scope-bar chip read as one object.
                        .size(scale::design(super::toolbar::GLYPH_BOX))
                        .rounded(theme.radius_tokens().sm)
                        .text_color(fg)
                        .child(Icon::new(IconName::Close).small())
                        .debug_selector(move || close.clone())
                        .occlude()
                        .pointer_states(close_states)
                        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            // The field keeps the keyboard, so typing and
                            // backspace go on working after the click.
                            window.prevent_default();
                            entity.update(cx, |shell, cx| {
                                unstage(shell, &target, cx);
                                shell
                                    .dialog_input
                                    .update(cx, |input, cx| input.focus(window, cx));
                            });
                        }),
                ),
        );
    }
    row.into_any_element()
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
            apply(&mut f, &Mode::Whole, "c = 3", &[], &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(terms(&f), vec!["c = 3"]);
        assert_eq!(
            apply(&mut f, &Mode::Whole, "  ", &[], &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(f.scope().expression, None);
    }

    #[test]
    fn term_mode_replaces_only_its_term_and_empty_removes_it() {
        let mut f = frame_with(Some("a = 1 and b = 2 and c = 3"));
        let mode = term_mode(&f, 1);
        assert_eq!(
            apply(&mut f, &mode, "x = 9", &[], &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(terms(&f), vec!["a = 1", "x = 9", "c = 3"]);
        let mode = term_mode(&f, 0);
        assert_eq!(
            apply(&mut f, &mode, "", &[], &ExprVocab::default()),
            Ok(true)
        );
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
            apply(&mut f, &mode, "b = 3", &[], &ExprVocab::default()),
            Err(TERM_GONE.to_string())
        );
        assert_eq!(
            apply(&mut f, &mode, "", &[], &ExprVocab::default()),
            Err(TERM_GONE.to_string())
        );
        assert_eq!(terms(&f), vec!["x = 1", "y = 2"]);
        f.set_scope(Scope {
            expression: Some(parse_expr("x = 1").unwrap()),
            ..Scope::default()
        });
        assert_eq!(
            apply(&mut f, &mode, "b = 3", &[], &ExprVocab::default()),
            Err(TERM_GONE.to_string())
        );
        assert_eq!(terms(&f), vec!["x = 1"]);
    }

    #[test]
    fn add_mode_joins_with_and_sets_when_none_and_empty_changes_nothing() {
        let mut f = frame_with(None);
        assert_eq!(
            apply(&mut f, &Mode::Add, "   ", &[], &ExprVocab::default()),
            Ok(false),
            "empty: no change"
        );
        assert_eq!(f.scope().expression, None);
        assert_eq!(
            apply(&mut f, &Mode::Add, "a = 1", &[], &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(terms(&f), vec!["a = 1"]);
        assert_eq!(
            apply(
                &mut f,
                &Mode::Add,
                "b = 2 or c = 3",
                &[],
                &ExprVocab::default()
            ),
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
            let err = apply(&mut f, &mode, "book =", &[], &ExprVocab::default()).unwrap_err();
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
        assert_eq!(request_scope(&Mode::Whole, &current, &[]).expression, None);
        assert_eq!(
            request_scope(&Mode::Whole, &current, &[]).dimensions,
            current.dimensions
        );
        assert_eq!(
            request_scope(&Mode::Add, &current, &[]).expression,
            current.expression
        );
        let term = Mode::term(0, current.expression.as_ref()).unwrap();
        assert_eq!(
            request_scope(&term, &current, &[])
                .expression
                .map(|e| e.to_string()),
            Some("y = 'b'".to_string())
        );
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    /// Whole replaces the names, so its values are narrowed by the staged
    /// names alone; Add joins them, so by the frame's plus the staged.
    #[test]
    fn request_scope_narrows_by_the_staged_names() {
        let current = Scope {
            named: names(&["liq", "hedges"]),
            expression: Some(parse_expr("x = 'a'").unwrap()),
            ..Scope::default()
        };
        assert_eq!(
            request_scope(&Mode::Whole, &current, &names(&["liq"])).named,
            names(&["liq"])
        );
        assert_eq!(
            request_scope(&Mode::Add, &current, &names(&["hedges", "big"])).named,
            names(&["liq", "hedges", "big"])
        );
        let term = Mode::term(0, current.expression.as_ref()).unwrap();
        assert_eq!(request_scope(&term, &current, &[]).named, current.named);
    }

    /// Whole sets the names and the expression in one `set_scope`, so one
    /// undo restores the whole prior scope.
    #[test]
    fn whole_apply_sets_names_and_text_in_one_undo_step() {
        let mut f = frame_with(Some("a = 1"));
        let before = f.scope().clone();
        assert_eq!(
            apply(
                &mut f,
                &Mode::Whole,
                "b = 2",
                &names(&["liq"]),
                &ExprVocab::default()
            ),
            Ok(true)
        );
        assert_eq!(f.scope().named, names(&["liq"]));
        assert_eq!(terms(&f), vec!["b = 2"]);
        assert!(f.undo_scope());
        assert_eq!(f.scope(), &before, "one undo restores names and text");
    }

    /// Add appends only the names the frame lacks, and joins the text,
    /// in one undo step.
    #[test]
    fn add_apply_appends_only_the_missing_names() {
        let mut f = frame_with(Some("a = 1"));
        let mut scope = f.scope().clone();
        scope.named = names(&["liq"]);
        f.set_scope(scope);
        let before = f.scope().clone();
        assert_eq!(
            apply(
                &mut f,
                &Mode::Add,
                "b = 2",
                &names(&["hedges", "liq"]),
                &ExprVocab::default()
            ),
            Ok(true)
        );
        assert_eq!(f.scope().named, names(&["liq", "hedges"]));
        assert_eq!(terms(&f), vec!["a = 1", "b = 2"]);
        assert!(f.undo_scope());
        assert_eq!(f.scope(), &before);
    }

    /// An empty field with names staged applies the names alone, in both
    /// modes; Whole also clears the expression, as an empty field always did.
    #[test]
    fn empty_text_with_staged_names_applies_the_names() {
        let mut f = frame_with(None);
        assert_eq!(
            apply(
                &mut f,
                &Mode::Add,
                "  ",
                &names(&["liq"]),
                &ExprVocab::default()
            ),
            Ok(true)
        );
        assert_eq!(f.scope().named, names(&["liq"]));
        assert_eq!(f.scope().expression, None);
        let mut f = frame_with(Some("a = 1"));
        assert_eq!(
            apply(
                &mut f,
                &Mode::Whole,
                "",
                &names(&["hedges"]),
                &ExprVocab::default()
            ),
            Ok(true)
        );
        assert_eq!(f.scope().named, names(&["hedges"]));
        assert_eq!(f.scope().expression, None);
    }

    /// An empty field with nothing staged: Whole clears the expression
    /// and the names, Add changes nothing.
    #[test]
    fn empty_text_with_nothing_staged_keeps_todays_results() {
        let mut f = frame_with(Some("a = 1"));
        let mut scope = f.scope().clone();
        scope.named = names(&["liq"]);
        f.set_scope(scope);
        let before = f.scope().clone();
        assert_eq!(
            apply(&mut f, &Mode::Add, "", &[], &ExprVocab::default()),
            Ok(false)
        );
        assert_eq!(f.scope(), &before);
        assert_eq!(
            apply(&mut f, &Mode::Whole, "", &[], &ExprVocab::default()),
            Ok(true)
        );
        assert_eq!(f.scope().expression, None);
        assert!(f.scope().named.is_empty());
    }

    /// Offers leave out staged names; Term mode offers none; an invalid
    /// definition is offered broken with its reason.
    #[test]
    fn named_offers_exclude_staged_and_term_mode() {
        use geode_core::config::{LayerDoc, merge_docs};
        let doc = LayerDoc::builtin(
            "expressions",
            "[liq]\nexpression = \"npv > 0\"\n[bad]\nexpression = \"npv >\"\n",
        )
        .unwrap();
        let (defined, _) =
            NamedExpressions::from_doc(&merge_docs("expressions", &[doc]), &ExprVocab::default());
        let offers = named_offers(&Mode::Add, &defined, &[]);
        let offered: Vec<&str> = offers.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(offered, vec!["bad", "liq"]);
        assert!(offers[0].broken);
        // A broken offer previews WHY it is broken, not its text.
        let Some(NamedExpr::Invalid { reason, .. }) = defined.get("bad") else {
            panic!("bad is invalid");
        };
        assert_eq!(&offers[0].preview, reason);
        assert_ne!(offers[0].preview, "npv >");
        assert_eq!(offers[1].preview, "npv > 0");
        assert!(!offers[1].broken);
        let offers = named_offers(&Mode::Whole, &defined, &names(&["liq"]));
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].name, "bad");
        let e = parse_expr("a = 1").unwrap();
        let term = Mode::term(0, Some(&e)).unwrap();
        assert!(named_offers(&term, &defined, &[]).is_empty());
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
