//! The prompt field: a bar under the header asking one thing (a name to
//! add, for now), its text field, the ranked choices hung under it, and why
//! the last answer was refused. The typed text may be a name nothing
//! lists: a highlight is only a guess until the trader moves it (`up`,
//! `down`, a row click) or types it out in full.
//!
//! The pure parts here (the commit rule, the paint) are the field's; the
//! tile owns the field's lifetime, focus and writes.

use std::rc::Rc;

use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::shell::chip::{self, chip_paint};
use geode_shell::shell::scale;
use geode_shell::vimnav::NavCommand;
use geode_tile::popover::{self, ROW_HEIGHT, ROW_INSET};
use gpui::prelude::*;
use gpui::{Anchor, App, Entity, MouseButton, SharedString, div};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::core::prompt::Prompt;
use crate::tile::WatchlistTile;

/// What the list says while it has nothing to rank.
pub(crate) const NO_NAMES: &str = "no names known: type one";
/// What it says while the typed text matches no name.
pub(crate) const NEW_NAME: &str = "no name matches: enter adds it as typed";

/// What hangs under the field: a ranked choice over known names. (A rule
/// expression's completion is a later shape.)
pub(crate) enum Rows {
    Choice(ChoiceList),
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
    /// choice, not a guess, and enter takes it whatever is typed.
    pub moved: bool,
    /// The rows as painted, prepared when they change.
    pub choice: Rc<ChoicePaint>,
}

impl PromptField {
    /// The field asking `prompt`, with `options` to rank.
    pub(crate) fn new(input: Entity<InputState>, prompt: Prompt, options: Vec<String>) -> Self {
        let list = ChoiceList::new(options, DEFAULT_CAP);
        let choice = Rc::new(choice_paint(&list));
        PromptField {
            input,
            label: label(&prompt).into(),
            prompt,
            rows: Rows::Choice(list),
            error: None,
            moved: false,
            choice,
        }
    }

    fn list_mut(&mut self) -> &mut ChoiceList {
        match &mut self.rows {
            Rows::Choice(list) => list,
        }
    }

    pub(crate) fn repaint(&mut self) {
        self.choice = Rc::new(match &self.rows {
            Rows::Choice(list) => choice_paint(list),
        });
    }

    /// The typed text changed: re-rank. Typing after a moved highlight
    /// makes it a guess again. Whether the rows changed.
    pub(crate) fn typed(&mut self, text: &str) -> bool {
        let changed = self.list_mut().set_query(text);
        if changed {
            self.moved = false;
            self.repaint();
        }
        changed
    }

    /// `up`/`down`: the highlight moves, and is a choice now.
    pub(crate) fn step(&mut self, delta: i64) {
        self.list_mut().nav(NavCommand::Move(delta));
        self.moved = true;
        self.repaint();
    }

    /// Hover lights a row without making it a choice: a pointer passing
    /// over the list must not change what `enter` writes. Whether it
    /// changed.
    pub(crate) fn hover(&mut self, row: usize) -> bool {
        let list = self.list_mut();
        let changed = list.highlighted() != row && list.set_highlighted(row);
        if changed {
            self.repaint();
        }
        changed
    }

    /// A press on row `row`: its text, chosen.
    pub(crate) fn pick(&mut self, row: usize) -> Option<String> {
        let list = self.list_mut();
        let text = list
            .set_highlighted(row)
            .then(|| list.highlighted_text().unwrap_or_default().to_string())?;
        self.moved = true;
        Some(text)
    }

    /// The answer `enter` gives for `text` (see [`answer_value`]).
    pub(crate) fn answer(&mut self, text: &str) -> String {
        let moved = self.moved;
        answer_value(self.list_mut(), moved, text)
    }
}

/// The answer `enter` gives for `text`: the highlighted option when the
/// trader moved the highlight or typed it out in full (any case, by
/// Unicode lowercasing: the listed spelling wins, so `spx` never makes a
/// second `SPX`); else the typed text trimmed, as typed, never re-cased.
pub(crate) fn answer_value(list: &mut ChoiceList, moved: bool, text: &str) -> String {
    list.set_query(text);
    let typed = text.trim();
    let highlighted = list.highlighted_text().map(str::to_string);
    let take = moved
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
    }
}

/// The field's placeholder for `prompt`.
pub(crate) fn placeholder(prompt: &Prompt) -> &'static str {
    match prompt {
        Prompt::AddName => "underlying",
    }
}

/// What the choice paints, prepared when the list changes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChoicePaint {
    pub rows: Vec<SharedString>,
    /// Window-relative, as `ChoiceList::highlighted` answers it.
    pub highlighted: usize,
    /// Shown in place of rows when there are none.
    pub empty: &'static str,
}

fn choice_paint(list: &ChoiceList) -> ChoicePaint {
    ChoicePaint {
        rows: list
            .painted()
            .iter()
            .map(|r| list.options()[r.row].clone().into())
            .collect(),
        highlighted: list.highlighted(),
        empty: if list.options().is_empty() {
            NO_NAMES
        } else {
            NEW_NAME
        },
    }
}

/// The ranked options hung from the bar's bottom-left, over the grid.
/// `deferred` (via `anchor_popup`) escapes the table's clip. A row press
/// picks it and stops there, so the grid under the list does not also take
/// the press and close the field. No outside-press closer here: a press on
/// the field itself is outside the list.
fn render_choice(
    p: &ChoicePaint,
    tile: &Entity<WatchlistTile>,
    tile_id: u64,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover::surface(cx)
        .debug_selector(move || format!("watchlist-prompt-list-{tile_id}"))
        .occlude();
    if p.rows.is_empty() {
        list = list.child(popover::empty_row(theme, p.empty));
    }
    for (i, text) in p.rows.iter().enumerate() {
        list = list.child(
            h_flex()
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
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
                .child(text.clone()),
        );
    }
    popover::anchor_popup(list, Anchor::TopLeft)
}

/// The prompt bar: under the header, full width, its label then the field;
/// a refusal on its own line beneath, in the danger color, so the trader
/// reads why next to what they typed. The choice hangs from the bar's
/// bottom-left over the grid.
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
        .child(div().absolute().left_0().bottom_0().child(list))
        .into_any_element()
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
}
