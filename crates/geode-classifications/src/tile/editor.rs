//! The label editor: a free typeahead over the classification's labels,
//! opened on the cursor row's label cell (`enter`, `c`, a double-click).
//! The text field replaces the cell's label; the ranked labels hang under
//! it. The typed text may be a new label: a highlight is only a guess until
//! the trader moves it (`up`/`down`, a row click) or types it out in full.
//!
//! The pure parts here (the commit rule, the prefill, the paint) are the
//! editor's; the tile owns the editor's lifetime, focus and writes.
//!
//! The prompt field ([`PromptField`]) is the tile's other text entry: a bar
//! under the header asking a new classification's name and source column,
//! or a new name. Its column step hangs a closed choice over the columns a
//! classification may map, painted by the label editor's list painter.

use std::rc::Rc;

use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::shell::chip::{self, chip_paint};
use geode_shell::shell::scale;
use geode_tile::popover::{self, ROW_HEIGHT, ROW_INSET};
use gpui::prelude::*;
use gpui::{Anchor, App, Entity, MouseButton, SharedString, WeakEntity, div};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use crate::core::prompt::Prompt;
use crate::tile::ClassificationsTile;

/// What the list says while it has nothing to rank.
pub(crate) const NO_LABELS: &str = "no labels yet: type one";
/// What it says while the typed text matches no label.
pub(crate) const NEW_LABEL: &str = "no label matches: enter sets it as typed";
/// What the column choice says while no column may be classified.
pub(crate) const NO_COLUMNS: &str = "no column can be classified";
/// What it says while the typed text matches no column.
pub(crate) const NO_COLUMN_MATCHES: &str = "no column matches";

pub(crate) struct LabelEditor {
    pub input: Entity<InputState>,
    pub list: ChoiceList,
    /// The trader moved the highlight (a key or a row click): it is a
    /// choice, not a guess, and enter takes it whatever is typed.
    pub moved: bool,
    /// The source values the commit labels, fixed when the editor opened.
    pub targets: Vec<String>,
}

impl LabelEditor {
    /// The editor over `labels`, highlight on `prefill` when it is one.
    pub(crate) fn new(
        input: Entity<InputState>,
        labels: Vec<String>,
        prefill: Option<&str>,
        targets: Vec<String>,
    ) -> LabelEditor {
        let mut list = ChoiceList::new(labels, DEFAULT_CAP);
        list.place(prefill);
        LabelEditor {
            input,
            list,
            moved: false,
            targets,
        }
    }

    /// The label `enter` writes for `text` (see [`commit_value`]).
    pub(crate) fn commit(&mut self, text: &str) -> Option<String> {
        commit_value(&mut self.list, self.moved, text)
    }
}

/// The label `enter` writes for `text`, `None` to clear. The highlighted
/// label when the trader moved the highlight or typed it out in full (any
/// case, by Unicode lowercasing: the existing spelling wins, so `tech` never makes a second `Tech`);
/// else the typed text trimmed, as typed, never re-cased. A blank entry
/// clears.
pub(crate) fn commit_value(list: &mut ChoiceList, moved: bool, text: &str) -> Option<String> {
    list.set_query(text);
    let typed = text.trim();
    let highlighted = list.highlighted_text().map(str::to_string);
    // Unicode lowercasing on both sides: an ASCII-only fold would let
    // `économie` make a second `Économie`.
    let take = moved
        || highlighted
            .as_deref()
            .is_some_and(|h| h.to_lowercase() == typed.to_lowercase());
    match highlighted {
        Some(h) if take => Some(h),
        _ if typed.is_empty() => None,
        _ => Some(typed.to_string()),
    }
}

/// The label the editor opens with: the targets' one label when they all
/// share it, else nothing. A selection of mixed labels must not prefill
/// one of them, which enter would then write across all.
pub(crate) fn prefill<'a>(mut labels: impl Iterator<Item = Option<&'a str>>) -> Option<&'a str> {
    let first = labels.next()??;
    labels.all(|l| l == Some(first)).then_some(first)
}

/// Which list a [`ChoicePaint`] is: the label editor's free typeahead,
/// where the typed text may be a new label, or the prompt's closed choice
/// of source columns, where only a listed column is an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChoiceKind {
    Labels,
    Columns,
}

/// What a typeahead paints, prepared when the list changes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChoicePaint {
    pub rows: Vec<SharedString>,
    /// Window-relative, as `ChoiceList::highlighted` answers it.
    pub highlighted: usize,
    /// Shown in place of rows when there are none.
    pub empty: &'static str,
    /// The selector stem: `{stem}-list-{tile}` and `{stem}-row-{text}`.
    pub stem: &'static str,
    /// A press outside the list closes its owner. The prompt's list does
    /// not: a press on the prompt's own field is outside it.
    pub close_outside: bool,
}

pub(crate) fn choice_paint(list: &ChoiceList, kind: ChoiceKind) -> ChoicePaint {
    let none = list.options().is_empty();
    let (empty, stem, close_outside) = match kind {
        ChoiceKind::Labels => (
            if none { NO_LABELS } else { NEW_LABEL },
            "classifications-editor",
            true,
        ),
        ChoiceKind::Columns => (
            if none { NO_COLUMNS } else { NO_COLUMN_MATCHES },
            "classifications-prompt",
            false,
        ),
    };
    ChoicePaint {
        rows: list
            .painted()
            .iter()
            .map(|r| list.options()[r.row].clone().into())
            .collect(),
        highlighted: list.highlighted(),
        empty,
        stem,
        close_outside,
    }
}

/// The open editor as the grid's delegate paints it: the shown row it sits
/// on, its field and its list.
#[derive(Clone)]
pub(crate) struct EditorPaint {
    pub row: usize,
    pub input: Entity<InputState>,
    pub choice: Rc<ChoicePaint>,
    pub tile: WeakEntity<ClassificationsTile>,
    pub tile_id: u64,
}

/// The field, in place of the label cell's text: no chrome of its own, so
/// the row keeps its height and the cell its alignment.
pub(crate) fn field(input: &Entity<InputState>) -> Input {
    Input::new(input).appearance(false).px_0().h_full()
}

/// The ranked options hung from the owner's bottom-left: the edited cell's,
/// or the prompt bar's. `deferred` (via `anchor_popup`) escapes the
/// table's clip. A row press picks it and stops there, so the grid under
/// the list does not also take the press and cancel; for the label editor
/// a press anywhere else closes it unchanged.
pub(crate) fn render_choice(
    p: &ChoicePaint,
    tile: &Entity<ClassificationsTile>,
    tile_id: u64,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let stem = p.stem;
    let mut list = popover::surface(cx)
        .debug_selector(move || format!("{stem}-list-{tile_id}"))
        .occlude()
        .when(p.close_outside, |el| {
            el.on_mouse_down_out({
                let tile = tile.clone();
                move |_, window, cx| tile.update(cx, |t, cx| t.close_editor(window, cx))
            })
        });
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
                    move || format!("{stem}-row-{text}")
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

/// The label cell while the editor is on its row: the field, with the list
/// hung under it. A dropped tile paints the field alone.
pub(crate) fn render_cell(e: &EditorPaint, cx: &App) -> gpui::AnyElement {
    let tile_id = e.tile_id;
    let popup = e
        .tile
        .upgrade()
        .map(|tile| render_choice(&e.choice, &tile, tile_id, cx).into_any_element());
    div()
        .relative()
        .size_full()
        .flex()
        .items_center()
        .debug_selector(move || format!("classifications-editor-{tile_id}"))
        .child(field(&e.input))
        .when_some(popup, |el, popup| {
            el.child(div().absolute().left_0().bottom_0().child(popup))
        })
        .into_any_element()
}

/// The open prompt: what it asks, its field, the closed column choice on
/// the column step, and why the last answer was refused.
pub(crate) struct PromptField {
    pub input: Entity<InputState>,
    pub prompt: Prompt,
    /// The words before the field, prepared when the step changes.
    pub label: SharedString,
    /// The columns a classification may map, on the column step only.
    pub list: Option<ChoiceList>,
    /// The list as painted, prepared when it changes.
    pub choice: Option<Rc<ChoicePaint>>,
    /// The last answer's refusal; the field stays open under it.
    pub error: Option<SharedString>,
}

impl PromptField {
    /// The field asking `prompt`; `columns` lists the column step's
    /// choices.
    pub(crate) fn new(input: Entity<InputState>, prompt: Prompt, columns: Vec<String>) -> Self {
        let mut field = PromptField {
            input,
            prompt: Prompt::NewName,
            label: SharedString::default(),
            list: None,
            choice: None,
            error: None,
        };
        field.ask(prompt, columns);
        field
    }

    /// Move to `prompt`: its label, and a fresh column list on the column
    /// step. The last refusal belonged to the previous question.
    pub(crate) fn ask(&mut self, prompt: Prompt, columns: Vec<String>) {
        self.label = label(&prompt).into();
        self.list = matches!(prompt, Prompt::NewColumn { .. })
            .then(|| ChoiceList::new(columns, DEFAULT_CAP));
        self.prompt = prompt;
        self.error = None;
        self.repaint();
    }

    pub(crate) fn repaint(&mut self) {
        self.choice = self
            .list
            .as_ref()
            .map(|l| Rc::new(choice_paint(l, ChoiceKind::Columns)));
    }

    /// The answer `text` gives: on the closed column step, the highlighted
    /// column whenever one survives the typed text (`under` answers
    /// `underlying_ref`), else the text, which validation then refuses.
    pub(crate) fn answer(&mut self, text: &str) -> String {
        match self.list.as_mut() {
            Some(list) => {
                list.set_query(text.trim());
                list.highlighted_text().unwrap_or(text).to_string()
            }
            None => text.to_string(),
        }
    }
}

/// The words before the field.
fn label(prompt: &Prompt) -> String {
    match prompt {
        Prompt::NewName => "New classification".to_string(),
        Prompt::NewColumn { name } => format!("Source column for {name}"),
        Prompt::Rename { from } => format!("Rename {from} to"),
    }
}

/// The field's placeholder for `prompt`.
pub(crate) fn placeholder(prompt: &Prompt) -> &'static str {
    match prompt {
        Prompt::NewName | Prompt::Rename { .. } => "name",
        Prompt::NewColumn { .. } => "source column",
    }
}

/// The prompt bar: under the header, full width, its label then the field;
/// a refusal on its own line beneath, in the danger color, so the trader
/// reads why next to what they typed. The column step's list hangs from
/// the bar's bottom-left over the grid.
pub(crate) fn render_prompt(
    p: &PromptField,
    tile: &Entity<ClassificationsTile>,
    tile_id: u64,
    cx: &App,
) -> gpui::AnyElement {
    let theme = cx.theme();
    let list = p
        .choice
        .as_ref()
        .map(|c| render_choice(c, tile, tile_id, cx).into_any_element());
    v_flex()
        .relative()
        .w_full()
        .flex_none()
        .px_2()
        .py_1()
        .gap_0p5()
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(move || format!("classifications-prompt-{tile_id}"))
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
                    .debug_selector(move || format!("classifications-prompt-error-{tile_id}"))
                    .child(why),
            )
        })
        .when_some(list, |el, list| {
            el.child(div().absolute().left_0().bottom_0().child(list))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(labels: &[&str]) -> ChoiceList {
        ChoiceList::new(labels.iter().map(|l| l.to_string()).collect(), DEFAULT_CAP)
    }

    #[test]
    fn the_commit_takes_a_case_variant_highlight_and_otherwise_the_text() {
        let mut l = list(&["Tech"]);
        assert_eq!(commit_value(&mut l, false, "tech"), Some("Tech".into()));
        let mut l = list(&["Tech"]);
        assert_eq!(commit_value(&mut l, false, " techx "), Some("techx".into()));
        // A subsequence guess is not taken unless moved to.
        let mut l = list(&["Technology"]);
        assert_eq!(commit_value(&mut l, false, "tech"), Some("tech".into()));
        let mut l = list(&["Technology"]);
        assert_eq!(
            commit_value(&mut l, true, "tech"),
            Some("Technology".into())
        );
        // Blank clears; a moved highlight still wins over blank.
        let mut l = list(&[]);
        assert_eq!(commit_value(&mut l, false, "  "), None);
        let mut l = list(&["Tech"]);
        assert_eq!(commit_value(&mut l, true, ""), Some("Tech".into()));
    }

    /// The case-insensitive match is Unicode's, not ASCII's: a label typed
    /// out with a different-cased accented letter is still that label.
    #[test]
    fn a_unicode_case_variant_takes_the_existing_label() {
        let mut l = list(&["\u{c9}conomie"]);
        assert_eq!(
            commit_value(&mut l, false, "\u{e9}conomie"),
            Some("\u{c9}conomie".into())
        );
    }

    #[test]
    fn prefill_is_only_a_unanimous_label() {
        assert_eq!(prefill([Some("X"), Some("X")].into_iter()), Some("X"));
        assert_eq!(prefill([Some("X"), Some("Y")].into_iter()), None);
        assert_eq!(prefill([Some("X"), None].into_iter()), None);
        assert_eq!(prefill([None, None].into_iter()), None);
        assert_eq!(prefill(std::iter::empty()), None);
    }
}
