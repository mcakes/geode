//! The prompt field: a bar under the header asking one thing (a name to
//! add, or one step of a rule), its text field, the rows hung under it,
//! and why the last answer was refused. The rows are one of two shapes: a
//! ranked choice over options (open for the add field, where the typed
//! text may be a name nothing lists and a highlight is only a guess until
//! the trader moves it or types it out in full; closed for a rule's
//! dataset and scope, where the highlight is the answer), or an
//! expression completion over the rule's dataset, whose rows are written
//! into the field rather than answering it.
//!
//! The pure parts (the commit rule, the paint) come first; the tile's side
//! (the field's lifetime, focus and keys, and the commit through the add
//! verb or the rule steps) is the `impl WatchlistTile` beneath them.

use std::rc::Rc;

use geode_core::scope::complete::ExprVocab;
use geode_core::watchlist::edit;
use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::exprcomplete::{Accept, ExprCompletion, Refresh, Write};
use geode_shell::shell::chip::{self, chip_paint};
use geode_shell::shell::scale;
use geode_shell::vimnav::NavCommand;
use geode_tile::notice::{self, Notice, Tone};
use geode_tile::popover::{self, ROW_HEIGHT, ROW_INSET};
use gpui::prelude::*;
use gpui::{Anchor, App, Context, Entity, Focusable as _, MouseButton, SharedString, Window, div};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use super::{NOTHING_SHOWN, WatchlistTile, snapshot};
use crate::core::prompt::{self, Prompt, RuleContext, Step};
use crate::core::rows;

/// What the list says while it has nothing to rank.
pub(crate) const NO_NAMES: &str = "no names known: type one";
/// What it says while the typed text matches no name.
pub(crate) const NEW_NAME: &str = "no name matches: enter adds it as typed";
/// What a closed list says while the typed text matches no choice.
pub(crate) const NO_CHOICE: &str = "nothing matches";

/// What hangs under the field.
pub(crate) enum Rows {
    /// A ranked choice over known options.
    Choice(ChoiceList),
    /// A scope expression's completion over one dataset's vocabulary:
    /// columns, operators and keywords (no categorical values: the tile
    /// issues no distinct). Boxed: the completion is the larger shape by
    /// far, and the field is one of the two.
    Expr(Box<ExprRows>),
}

/// The expression shape's parts: the completion, the vocabulary it is
/// ranked against, and the painted window into its rows (up to fifty,
/// more than a popup should stand), which follows the highlight the way
/// `ChoiceList`'s does: it moves only when the highlight leaves it, so a
/// hover on a painted row never shifts the rows under the pointer.
pub(crate) struct ExprRows {
    pub completion: ExprCompletion,
    pub vocab: ExprVocab,
    /// The first painted row's index among the ranked rows. Kept so that
    /// `window <= highlighted < window + DEFAULT_CAP` (or there are no
    /// rows).
    window: usize,
    /// The categorical column whose values the completion asked for on
    /// the last refresh: the tile issues no distinct, so the completion's
    /// own `loading values…` would never come true, and the hint says so
    /// instead.
    unsuggested: Option<String>,
}

impl ExprRows {
    fn new(vocab: ExprVocab) -> Self {
        let mut e = ExprRows {
            completion: ExprCompletion::default(),
            vocab,
            window: 0,
            unsuggested: None,
        };
        e.refresh("", 0);
        e
    }

    /// Re-rank at `text` and `caret`; the window starts over at the top,
    /// where the completion puts the highlight. Whether anything changed.
    fn refresh(&mut self, text: &str, caret: usize) -> bool {
        match self.completion.refresh(text, caret, &self.vocab) {
            Refresh::Unchanged => return false,
            Refresh::Changed => self.unsuggested = None,
            Refresh::Request(column) => self.unsuggested = Some(column),
        }
        self.window = 0;
        self.follow();
        true
    }

    /// Move the highlight `delta` rows (clamped, as the completion does)
    /// and bring the window to it.
    fn step(&mut self, delta: i64) {
        self.completion.step(delta);
        self.follow();
    }

    /// Light painted row `row` (window-relative); the window does not
    /// move, since the row is already in it (a row past the window is no
    /// painted row, and is refused). Whether it changed.
    fn hover(&mut self, row: usize) -> bool {
        let target = self.window + row;
        let highlighted = self.completion.highlighted();
        if row >= DEFAULT_CAP || target == highlighted || target >= self.completion.rows().len() {
            return false;
        }
        self.completion.step(target as i64 - highlighted as i64);
        self.follow();
        true
    }

    /// The window's start: `ChoiceList::follow`'s rule.
    fn follow(&mut self) {
        let highlighted = self.completion.highlighted();
        if highlighted < self.window {
            self.window = highlighted;
        } else if highlighted >= self.window + DEFAULT_CAP {
            self.window = highlighted + 1 - DEFAULT_CAP;
        }
        self.window = self
            .window
            .min(self.completion.rows().len().saturating_sub(DEFAULT_CAP));
    }

    /// The ranked row painted row `row` names.
    fn ranked(&self, row: usize) -> usize {
        self.window + row
    }

    /// The painted window's start, for tests.
    #[cfg(test)]
    fn window(&self) -> usize {
        self.window
    }
}

/// The open prompt: what it asks, its field, the rows under it, and why
/// the last answer was refused.
pub(crate) struct PromptField {
    pub input: Entity<InputState>,
    pub prompt: Prompt,
    /// The words before the field.
    pub label: SharedString,
    pub rows: Rows,
    /// The last answer's refusal; the field stays open under it.
    pub error: Option<SharedString>,
    /// The trader moved the highlight (a key or a row click): it is a
    /// choice, not a guess, and enter takes it whatever is typed. On the
    /// expression shape, enter writes it into the field instead.
    pub moved: bool,
    /// A closed list: the highlight is the answer whatever is typed, and
    /// the typed text alone answers only when nothing is highlighted.
    pub closed: bool,
    /// The rows as painted: derived from `rows`, never read back into it,
    /// and repainted (`repaint`) after every `step`, `typed` and `hover`,
    /// so render ranks nothing. Both shapes keep the same discipline.
    pub choice: Rc<ChoicePaint>,
}

impl PromptField {
    /// The field asking `prompt`, with `options` to rank: an open typeahead
    /// for the add field, a closed choice for a rule step.
    pub(crate) fn new(input: Entity<InputState>, prompt: Prompt, options: Vec<String>) -> Self {
        let closed = prompt.is_rule();
        let list = ChoiceList::new(options, DEFAULT_CAP);
        let choice = Rc::new(choice_paint(&list, closed));
        PromptField {
            input,
            label: label(&prompt).into(),
            prompt,
            rows: Rows::Choice(list),
            error: None,
            moved: false,
            closed,
            choice,
        }
    }

    /// The field asking `prompt` with an expression completion over
    /// `vocab`, empty until the first text arrives (`typed`).
    pub(crate) fn expr(input: Entity<InputState>, prompt: Prompt, vocab: ExprVocab) -> Self {
        let rows = ExprRows::new(vocab);
        let choice = Rc::new(expr_paint(&rows));
        PromptField {
            input,
            label: label(&prompt).into(),
            prompt,
            rows: Rows::Expr(Box::new(rows)),
            error: None,
            moved: false,
            closed: false,
            choice,
        }
    }

    pub(crate) fn repaint(&mut self) {
        self.choice = Rc::new(match &self.rows {
            Rows::Choice(list) => choice_paint(list, self.closed),
            Rows::Expr(e) => expr_paint(e),
        });
    }

    /// The typed text (or the caret) changed: re-rank. Typing after a
    /// moved highlight makes it a guess again. Whether the rows changed.
    pub(crate) fn typed(&mut self, text: &str, caret: usize) -> bool {
        let changed = match &mut self.rows {
            Rows::Choice(list) => list.set_query(text),
            // A categorical column's values are not requested: the tile
            // issues no distinct, and the hint says so.
            Rows::Expr(e) => e.refresh(text, caret),
        };
        if changed {
            self.moved = false;
            self.repaint();
        }
        changed
    }

    /// `up`/`down`: the highlight moves, and is a choice now.
    pub(crate) fn step(&mut self, delta: i64) {
        match &mut self.rows {
            Rows::Choice(list) => list.nav(NavCommand::Move(delta)),
            Rows::Expr(e) => e.step(delta),
        }
        self.moved = true;
        self.repaint();
    }

    /// Hover lights painted row `row` without making it a choice: a
    /// pointer passing over the list must not change what `enter` writes.
    /// Whether it changed.
    pub(crate) fn hover(&mut self, row: usize) -> bool {
        let changed = match &mut self.rows {
            Rows::Choice(list) => list.highlighted() != row && list.set_highlighted(row),
            Rows::Expr(e) => e.hover(row),
        };
        if changed {
            self.repaint();
        }
        changed
    }

    /// A press on painted row `row` of a choice: its text, chosen. `None`
    /// on the expression shape, whose rows are accepted (`accept`), not
    /// answered.
    pub(crate) fn pick(&mut self, row: usize) -> Option<String> {
        let Rows::Choice(list) = &mut self.rows else {
            return None;
        };
        let text = list
            .set_highlighted(row)
            .then(|| list.highlighted_text().unwrap_or_default().to_string())?;
        self.moved = true;
        Some(text)
    }

    /// What accepting painted row `row` of the completion writes into the
    /// field; `None` on a choice, or with no such row.
    pub(crate) fn accept(&self, row: usize) -> Option<Write> {
        let Rows::Expr(e) = &self.rows else {
            return None;
        };
        match e.completion.accept(e.ranked(row))? {
            Accept::Write(w) => Some(w),
            // No named offers are set, so no row stages.
            Accept::Stage { .. } => None,
        }
    }

    /// The answer `enter` gives for `text` (see [`answer_value`]); the
    /// text as typed on the expression shape.
    pub(crate) fn answer(&mut self, text: &str) -> String {
        let take = self.moved || self.closed;
        match &mut self.rows {
            Rows::Choice(list) => answer_value(list, take, text),
            Rows::Expr(_) => text.trim().to_string(),
        }
    }

    /// The highlighted row's text, whichever shape.
    #[cfg(test)]
    pub(crate) fn highlighted_text(&self) -> Option<String> {
        match &self.rows {
            Rows::Choice(list) => list.highlighted_text().map(str::to_string),
            Rows::Expr(e) => e
                .completion
                .rows()
                .get(e.completion.highlighted())
                .map(|r| r.label.clone()),
        }
    }
}

/// The answer `enter` gives for `text`: the highlighted option when the
/// trader moved the highlight (or the list is closed) or typed it out in
/// full (any case, by Unicode lowercasing: the listed spelling wins, so
/// `spx` never makes a second `SPX`); else the typed text trimmed, as
/// typed, never re-cased.
pub(crate) fn answer_value(list: &mut ChoiceList, take: bool, text: &str) -> String {
    list.set_query(text);
    let typed = text.trim();
    let highlighted = list.highlighted_text().map(str::to_string);
    let take = take
        || highlighted
            .as_deref()
            .is_some_and(|h| h.to_lowercase() == typed.to_lowercase());
    match highlighted {
        Some(h) if take => h,
        _ => typed.to_string(),
    }
}

/// The words before the field.
fn label(prompt: &Prompt) -> &'static str {
    match prompt {
        Prompt::AddName => "Add name",
        Prompt::RuleDataset => "Rule dataset",
        Prompt::RuleScope { .. } => "Rule scope",
        Prompt::RuleExpression { .. } => "Rule expression",
    }
}

/// The field's placeholder for `prompt`.
pub(crate) fn placeholder(prompt: &Prompt) -> &'static str {
    match prompt {
        Prompt::AddName => "underlying",
        Prompt::RuleDataset => "dataset",
        Prompt::RuleScope { .. } => "whole dataset, a saved scope or expression\u{2026}",
        Prompt::RuleExpression { .. } => "book = 'BK000' and npv > 0",
    }
}

/// What the rows paint, prepared when they change.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChoicePaint {
    /// Each row's text and its muted detail (empty on a choice).
    pub rows: Vec<(SharedString, SharedString)>,
    /// Window-relative, as `ChoiceList::highlighted` answers it.
    pub highlighted: usize,
    /// The first painted row's index among the ranked rows: a press on a
    /// painted row names the ranked one.
    pub offset: usize,
    /// Shown in place of rows when there are none; `None` paints no list
    /// at all (the hint says what is wanted).
    pub empty: Option<&'static str>,
    /// Under the field: the completion's warning (`true`) or its hint.
    pub hint: Option<(SharedString, bool)>,
}

fn choice_paint(list: &ChoiceList, closed: bool) -> ChoicePaint {
    ChoicePaint {
        rows: list
            .painted()
            .iter()
            .map(|r| {
                (
                    list.options()[r.row].clone().into(),
                    SharedString::default(),
                )
            })
            .collect(),
        highlighted: list.highlighted(),
        offset: 0,
        empty: Some(if list.options().is_empty() {
            NO_NAMES
        } else if closed {
            NO_CHOICE
        } else {
            NEW_NAME
        }),
        hint: None,
    }
}

/// What the completion's hint says at a categorical column's value
/// position, in place of its own `loading values…`.
pub(crate) const VALUES_NOT_SUGGESTED: &str = "values not suggested here";

/// The window of the completion's rows holding the highlight.
fn expr_paint(e: &ExprRows) -> ChoicePaint {
    let c = &e.completion;
    let offset = e.window;
    let hint = match (c.warning(), &e.unsuggested) {
        (Some(w), _) => (w.to_string().into(), true),
        (None, Some(column)) => (
            format!("value for {column} \u{b7} {VALUES_NOT_SUGGESTED}").into(),
            false,
        ),
        (None, None) => (c.hint().to_string().into(), false),
    };
    ChoicePaint {
        rows: c
            .rows()
            .iter()
            .skip(offset)
            .take(DEFAULT_CAP)
            .map(|r| (r.label.clone().into(), r.detail.clone().into()))
            .collect(),
        highlighted: c.highlighted() - offset,
        offset,
        empty: None,
        hint: Some(hint),
    }
}

/// The ranked rows hung from the bar's bottom-left, over the grid.
/// `deferred` (via `anchor_popup`) escapes the table's clip. A row press
/// picks it and stops there, so the grid under the list does not also take
/// the press and close the field. No outside-press closer here: a press on
/// the field itself is outside the list. `None` with nothing to list.
fn render_choice(
    p: &ChoicePaint,
    tile: &Entity<WatchlistTile>,
    tile_id: u64,
    cx: &App,
) -> Option<impl IntoElement> {
    let theme = cx.theme();
    let mut list = popover::surface(cx)
        .debug_selector(move || format!("watchlist-prompt-list-{tile_id}"))
        .occlude();
    if p.rows.is_empty() {
        list = list.child(popover::empty_row(theme, p.empty?));
    }
    for (i, (text, detail)) in p.rows.iter().enumerate() {
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .gap_2()
                .rounded(theme.radius)
                .items_center()
                .when(i == p.highlighted, |d| {
                    d.bg(theme.accent).text_color(theme.accent_foreground)
                })
                .when(i != p.highlighted, |d| {
                    d.text_color(theme.popover_foreground)
                })
                .debug_selector({
                    let text = text.clone();
                    move || format!("watchlist-prompt-row-{text}")
                })
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_, window, cx| {
                        cx.stop_propagation();
                        tile.update(cx, |t, cx| t.choice_pick(i, window, cx))
                    }
                })
                // Pointer and keys light the same row.
                .on_mouse_move({
                    let tile = tile.clone();
                    move |_, _, cx| tile.update(cx, |t, cx| t.choice_hover(i, cx))
                })
                .child(div().flex_1().child(text.clone()))
                .when(!detail.is_empty(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .when(i != p.highlighted, |d| d.text_color(theme.muted_foreground))
                            .child(detail.clone()),
                    )
                }),
        );
    }
    Some(popover::anchor_popup(list, Anchor::TopLeft))
}

/// The prompt bar: under the header, full width, its label then the field;
/// a refusal on its own line beneath, in the danger color, so the trader
/// reads why next to what they typed; the completion's hint or warning
/// under that. The rows hang from the bar's bottom-left over the grid.
pub(crate) fn render_prompt(
    p: &PromptField,
    tile: &Entity<WatchlistTile>,
    tile_id: u64,
    cx: &App,
) -> gpui::AnyElement {
    let theme = cx.theme();
    let list = render_choice(&p.choice, tile, tile_id, cx);
    v_flex()
        .relative()
        .w_full()
        .flex_none()
        .px_2()
        .py_1()
        .gap_0p5()
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(move || format!("watchlist-prompt-{tile_id}"))
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(p.label.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&p.input).appearance(false).w_full()),
                ),
        )
        .when_some(p.error.clone(), |el, why| {
            el.child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(chip_paint(theme, chip::Tone::DangerText).text)
                    .debug_selector(move || format!("watchlist-prompt-error-{tile_id}"))
                    .child(why),
            )
        })
        .when_some(p.choice.hint.clone(), |el, (hint, warning)| {
            let tone = if warning { Tone::Warning } else { Tone::Status };
            el.child(
                div()
                    .text_xs()
                    .whitespace_normal()
                    .text_color(notice::color(tone, theme))
                    .debug_selector(move || format!("watchlist-prompt-hint-{tile_id}"))
                    .child(hint),
            )
        })
        .when_some(list, |el, list| {
            el.child(div().absolute().left_0().bottom_0().child(list))
        })
        .into_any_element()
}

/// The prompt field's lifetime, focus and keys.
impl WatchlistTile {
    /// Open the field asking `prompt` over `options`, focused: an open
    /// typeahead for the add field, a closed choice for a rule step.
    pub(super) fn open_prompt(
        &mut self,
        prompt: Prompt,
        options: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.new_input(&prompt, window, cx);
        self.install_prompt(PromptField::new(input, prompt, options), window, cx);
    }

    /// Open the field asking `prompt` with an expression completion over
    /// `vocab`, focused.
    pub(super) fn open_expr_prompt(
        &mut self,
        prompt: Prompt,
        vocab: ExprVocab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.new_input(&prompt, window, cx);
        self.install_prompt(PromptField::expr(input, prompt, vocab), window, cx);
    }

    /// A fresh input for `prompt`, with every keystroke re-ranking the rows
    /// (at the live caret: the completion reads it). The subscription dies
    /// with the field.
    fn new_input(
        &mut self,
        prompt: &Prompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let placeholder = placeholder(prompt);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                let (query, caret) = {
                    let s = input.read(cx);
                    (s.value().to_string(), s.cursor())
                };
                if let Some(p) = this.prompt.as_mut().filter(|p| &p.input == input)
                    && p.typed(&query, caret)
                {
                    cx.notify();
                }
            }
        })
        .detach();
        input
    }

    /// Replace any open field with `field`, focused.
    fn install_prompt(&mut self, field: PromptField, window: &mut Window, cx: &mut Context<Self>) {
        self.close_prompt(window, cx);
        field.input.read(cx).focus_handle(cx).focus(window, cx);
        self.prompt = Some(field);
        self.field_window = Some(window.window_handle());
        cx.notify();
    }

    /// `enter` in the field. On the expression shape a moved highlight is
    /// a completion to write into the field, not an answer. Otherwise the
    /// answer goes through `prompt::submit` (the add field) or
    /// `prompt::submit_rule` (a rule step, over the factory's
    /// configuration and the shown list as it is now), then the verb. A
    /// refusal stays on the bar with the field open; a next step reopens
    /// the field asking it; a name added (or restored, when it was
    /// excluded) or a rules vector closes it and writes.
    pub(super) fn commit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.prompt.as_mut() else {
            return;
        };
        if p.moved && matches!(p.rows, Rows::Expr(_)) {
            let at = p.choice.highlighted;
            self.accept_completion(at, window, cx);
            return;
        }
        let text = p.input.read(cx).value().to_string();
        let answer = p.answer(&text);
        let asked = p.prompt.clone();
        let snapshot = snapshot(cx);
        let Some((name, state)) = self.shown(&snapshot) else {
            self.close_prompt(window, cx);
            self.refuse(NOTHING_SHOWN, cx);
            return;
        };
        let config = state.definition.clone();
        let members = rows::members(state, self.history.pending());
        let step = if asked.is_rule() {
            let wc = self.rule_config();
            let ctx = RuleContext {
                schema: &wc.schema,
                dims: &wc.dims,
                saved: &wc.saved,
                named: &wc.named,
                current: self.history.current(&config),
            };
            prompt::submit_rule(&asked, &answer, &ctx)
        } else {
            prompt::submit(&asked, &answer, &members)
        };
        match step {
            Step::Refuse(why) => {
                if let Some(p) = self.prompt.as_mut() {
                    p.error = Some(why.into());
                }
            }
            Step::Add(names) => {
                let current = self.history.current(&config).clone();
                match edit::add(&current, &members, &names) {
                    Err(why) => {
                        if let Some(p) = self.prompt.as_mut() {
                            p.error = Some(why.into());
                        }
                    }
                    Ok((next, entry)) => {
                        let restored = names
                            .iter()
                            .any(|n| members.iter().any(|m| &m.name == n && m.is_excluded()));
                        let verb = if restored { "restored" } else { "added" };
                        let said = format!("{verb} {}", names.join(", "));
                        self.close_prompt(window, cx);
                        self.commit(&name, &config, next, entry, Notice::status(said), cx);
                    }
                }
            }
            Step::Next(next) => self.open_rule_prompt(next, window, cx),
            Step::Rules(rules) => {
                self.close_prompt(window, cx);
                self.write_rules(&name, &config, rules, asked.replace(), cx);
            }
        }
        cx.notify();
    }

    /// Close the field with nothing written: `escape`, a press on the
    /// grid, any other verb. Blurred first if it owns focus: otherwise the
    /// shell cannot restore focus once the field is gone.
    pub(super) fn close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.prompt.take() else {
            return;
        };
        if p.input.read(cx).focus_handle(cx).is_focused(window) {
            window.blur(cx);
        }
        cx.notify();
    }

    /// Drop the field where no window is at hand (a reload removing the
    /// list, another list shown, a close), blurring it later through the
    /// window it opened in if it still owns focus (a newer field is never
    /// blurred).
    pub(super) fn release_prompt(&mut self, cx: &mut App) {
        let Some(p) = self.prompt.take() else {
            return;
        };
        let focus = p.input.read(cx).focus_handle(cx);
        drop(p);
        if let Some(handle) = self.field_window {
            App::defer(cx, move |cx| {
                let _ = handle.update(cx, |_, window, cx| {
                    if focus.is_focused(window) {
                        window.blur(cx);
                    }
                });
            });
        }
    }

    /// `up`/`down` in the field move the highlight: a choice now, which
    /// `enter` takes whatever is typed.
    pub(super) fn choice_step(&mut self, delta: i64, cx: &mut Context<Self>) {
        if let Some(p) = self.prompt.as_mut() {
            p.step(delta);
            cx.notify();
        }
    }

    /// Hover lights a list row without making it a choice.
    fn choice_hover(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.prompt.as_mut().is_some_and(|p| p.hover(row)) {
            cx.notify();
        }
    }

    /// A press on a list row: that name, committed at once; on the
    /// expression shape, that completion written into the field.
    fn choice_pick(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .prompt
            .as_ref()
            .is_some_and(|p| matches!(p.rows, Rows::Expr(_)))
        {
            self.accept_completion(row, window, cx);
            return;
        }
        let picked = self
            .prompt
            .as_mut()
            .and_then(|p| p.pick(row).map(|text| (p.input.clone(), text)));
        if let Some((input, text)) = picked {
            input.update(cx, |s, cx| s.set_value(text, window, cx));
            self.commit_prompt(window, cx);
        }
    }

    /// Write painted completion row `row` over its token, as one range
    /// replace (so the input's undo takes it back), the caret after it,
    /// the keyboard kept in the field (a row press must not take it), and
    /// the rows re-ranked at the new caret. The field stays open.
    fn accept_completion(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.prompt.as_mut() else {
            return;
        };
        let Some(write) = p.accept(row) else {
            return;
        };
        let input = p.input.clone();
        let (text, caret) = input.update(cx, |s, cx| {
            s.set_selected_range(write.range.clone(), cx);
            s.replace(write.text.clone(), window, cx);
            s.focus(window, cx);
            (s.value().to_string(), s.cursor())
        });
        if let Some(p) = self.prompt.as_mut() {
            p.error = None;
            p.typed(&text, caret);
        }
        cx.notify();
    }

    /// The open field: its text, its refusal and its ranked rows.
    #[cfg(test)]
    pub(super) fn prompt_state(&self, cx: &App) -> Option<(String, Option<String>, Vec<String>)> {
        let p = self.prompt.as_ref()?;
        let rows = match &p.rows {
            Rows::Choice(list) => list
                .ranked()
                .iter()
                .map(|r| list.options()[r.row].clone())
                .collect(),
            Rows::Expr(e) => e
                .completion
                .rows()
                .iter()
                .map(|r| r.label.clone())
                .collect(),
        };
        Some((
            p.input.read(cx).value().to_string(),
            p.error.as_ref().map(|e| e.to_string()),
            rows,
        ))
    }

    /// The open field's highlighted option.
    #[cfg(test)]
    pub(super) fn prompt_highlight(&self) -> Option<String> {
        self.prompt.as_ref()?.highlighted_text()
    }

    /// The open field's hint line and whether it is a warning.
    #[cfg(test)]
    pub(super) fn prompt_hint(&self) -> Option<(String, bool)> {
        self.prompt
            .as_ref()?
            .choice
            .hint
            .as_ref()
            .map(|(h, w)| (h.to_string(), *w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(options: &[&str]) -> ChoiceList {
        ChoiceList::new(options.iter().map(|o| o.to_string()).collect(), DEFAULT_CAP)
    }

    #[test]
    fn the_answer_takes_a_case_variant_highlight_and_otherwise_the_text() {
        assert_eq!(answer_value(&mut list(&["SPX"]), false, "spx"), "SPX");
        assert_eq!(answer_value(&mut list(&["SPX"]), false, " spxx "), "spxx");
        // A subsequence guess is not taken unless moved to.
        assert_eq!(answer_value(&mut list(&["SX5E"]), false, "sx"), "sx");
        assert_eq!(answer_value(&mut list(&["SX5E"]), true, "sx"), "SX5E");
        // Blank stays blank (the step refuses it); a moved highlight still
        // wins over blank.
        assert_eq!(answer_value(&mut list(&[]), false, "  "), "");
        assert_eq!(answer_value(&mut list(&["SPX"]), true, ""), "SPX");
    }

    /// The case-insensitive match is Unicode's, not ASCII's.
    #[test]
    fn a_unicode_case_variant_takes_the_listed_name() {
        assert_eq!(
            answer_value(&mut list(&["\u{c9}NERGIE"]), false, "\u{e9}nergie"),
            "\u{c9}NERGIE"
        );
    }

    /// Twenty columns over a twelve-row window.
    fn twenty() -> ExprVocab {
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::dimensions::DerivedDimensions;
        use geode_core::schema::SchemaSpec;
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!(
                "[risk.columns.c{i:02}]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"position\"\n"
            ));
        }
        let (schema, _) = SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", &text).unwrap()],
        ));
        ExprVocab::new(&schema, &DerivedDimensions::default())
    }

    /// The completion's paint is a window holding the highlight, and a
    /// press on a painted row names the ranked one.
    #[test]
    fn the_expression_paint_windows_the_rows_around_the_highlight() {
        let mut e = ExprRows::new(twenty());
        let p = expr_paint(&e);
        assert_eq!(p.rows.len(), DEFAULT_CAP);
        assert_eq!((p.highlighted, p.offset), (0, 0));
        assert_eq!(p.rows[0].0.as_ref(), "c00");
        assert_eq!(p.rows[0].1.as_ref(), "dimension \u{b7} text");
        assert_eq!(
            p.hint.as_ref().map(|(h, w)| (h.as_ref(), *w)),
            Some(("column", false))
        );
        for _ in 0..15 {
            e.step(1);
        }
        let p = expr_paint(&e);
        assert_eq!(
            (p.highlighted, p.offset),
            (DEFAULT_CAP - 1, 15 - (DEFAULT_CAP - 1))
        );
        assert_eq!(p.rows[p.highlighted].0.as_ref(), "c15");
        assert_eq!(e.ranked(p.highlighted), 15);
        assert_eq!(p.empty, None, "no rows is no list, not an empty one");
        // Back up: the window follows only once the highlight leaves it.
        e.step(-1);
        assert_eq!(e.window(), 4);
        e.step(-11);
        assert_eq!((e.completion.highlighted(), e.window()), (3, 3));
        // A new text starts the window over.
        assert!(e.refresh("c1", 2));
        assert_eq!(e.window(), 0);
    }

    /// A hover on a painted row of a scrolled window lights that row and
    /// leaves the window where it is: the next pointer move must find the
    /// same rows under it.
    #[test]
    fn a_hover_in_a_scrolled_window_does_not_move_the_window() {
        let mut e = ExprRows::new(twenty());
        for _ in 0..15 {
            e.step(1);
        }
        assert_eq!(e.window(), 4);
        assert!(e.hover(3));
        assert_eq!(e.window(), 4, "the window stays");
        assert_eq!(
            e.completion.highlighted(),
            7,
            "the painted row 3 is ranked row 7"
        );
        assert_eq!(expr_paint(&e).highlighted, 3);
        assert!(!e.hover(3), "already lit");
        assert!(e.hover(0));
        assert_eq!((e.completion.highlighted(), e.window()), (4, 4));
        assert!(!e.hover(DEFAULT_CAP), "past the window: no painted row");
        assert_eq!(e.window(), 4);
        // At the foot, the window's last rows are the ranked list's last.
        e.step(50);
        let len = e.completion.rows().len();
        assert_eq!(e.window(), len - DEFAULT_CAP);
        assert!(!e.hover(DEFAULT_CAP - 1), "already lit");
        assert!(e.hover(0));
        assert_eq!(e.completion.highlighted(), len - DEFAULT_CAP);
    }

    /// The tile issues no distinct, so the hint says values are not
    /// suggested rather than loading.
    #[test]
    fn a_categorical_value_position_says_values_are_not_suggested() {
        use geode_core::config::{LayerDoc, merge_docs};
        use geode_core::dimensions::DerivedDimensions;
        use geode_core::schema::SchemaSpec;
        let text = "[risk.columns.book]\ntype = \"utf8\"\nrole = \"dimension\"\ngrain = \"position\"\ncategorical = true\n";
        let (schema, _) = SchemaSpec::from_doc(&merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", text).unwrap()],
        ));
        let mut e = ExprRows::new(ExprVocab::new(&schema, &DerivedDimensions::default()));
        assert!(e.refresh("book = ", 7));
        assert_eq!(
            expr_paint(&e).hint.map(|(h, w)| (h.to_string(), w)),
            Some((
                format!("value for book \u{b7} {VALUES_NOT_SUGGESTED}"),
                false
            ))
        );
        assert!(e.refresh("book = 'A' and ", 15));
        assert_eq!(
            expr_paint(&e).hint.map(|(h, _)| h.to_string()),
            Some("column".into())
        );
    }
}
