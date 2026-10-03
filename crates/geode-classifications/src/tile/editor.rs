//! The label editor: a free typeahead over the classification's labels,
//! opened on the cursor row's label cell (`enter`, `c`, a double-click).
//! The text field replaces the cell's label; the ranked labels hang under
//! it. The typed text may be a new label: a highlight is only a guess until
//! the trader moves it (`up`/`down`, a row click) or types it out in full.
//!
//! The pure parts here (the commit rule, the prefill, the paint) are the
//! editor's; the tile owns the editor's lifetime, focus and writes.

use std::rc::Rc;

use geode_shell::choice::{ChoiceList, DEFAULT_CAP};
use geode_shell::shell::scale;
use geode_tile::popover::{self, ROW_HEIGHT, ROW_INSET};
use gpui::prelude::*;
use gpui::{Anchor, App, Entity, MouseButton, SharedString, WeakEntity, div};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, h_flex};

use crate::tile::ClassificationsTile;

/// What the list says while it has nothing to rank.
pub(crate) const NO_LABELS: &str = "no labels yet: type one";
/// What it says while the typed text matches no label.
pub(crate) const NEW_LABEL: &str = "no label matches: enter sets it as typed";

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
/// case: the existing spelling wins, so `tech` never makes a second `Tech`);
/// else the typed text trimmed, as typed, never re-cased. A blank entry
/// clears.
pub(crate) fn commit_value(list: &mut ChoiceList, moved: bool, text: &str) -> Option<String> {
    list.set_query(text);
    let typed = text.trim();
    let highlighted = list.highlighted_text().map(str::to_string);
    let take = moved
        || highlighted
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(typed));
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

/// What the typeahead paints, prepared when the list changes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChoicePaint {
    pub rows: Vec<SharedString>,
    /// Window-relative, as `ChoiceList::highlighted` answers it.
    pub highlighted: usize,
    /// Shown in place of rows when there are none.
    pub empty: &'static str,
}

pub(crate) fn choice_paint(list: &ChoiceList) -> ChoicePaint {
    ChoicePaint {
        rows: list
            .painted()
            .iter()
            .map(|r| list.options()[r.row].clone().into())
            .collect(),
        highlighted: list.highlighted(),
        empty: if list.options().is_empty() {
            NO_LABELS
        } else {
            NEW_LABEL
        },
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

/// The ranked labels hung from the edited cell's bottom-left. `deferred`
/// (via `anchor_popup`) escapes the table's clip. A row press picks it and
/// stops there, so the grid under the list does not also take the press
/// and cancel; a press anywhere else closes the editor unchanged.
pub(crate) fn render_choice(
    p: &ChoicePaint,
    tile: &Entity<ClassificationsTile>,
    tile_id: u64,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut list = popover::surface(cx)
        .debug_selector(move || format!("classifications-editor-list-{tile_id}"))
        .occlude()
        .on_mouse_down_out({
            let tile = tile.clone();
            move |_, window, cx| tile.update(cx, |t, cx| t.close_editor(window, cx))
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
                    move || format!("classifications-editor-row-{text}")
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

    #[test]
    fn prefill_is_only_a_unanimous_label() {
        assert_eq!(prefill([Some("X"), Some("X")].into_iter()), Some("X"));
        assert_eq!(prefill([Some("X"), Some("Y")].into_iter()), None);
        assert_eq!(prefill([Some("X"), None].into_iter()), None);
        assert_eq!(prefill([None, None].into_iter()), None);
        assert_eq!(prefill(std::iter::empty()), None);
    }
}
