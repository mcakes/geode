//! Presentation shared by tile-owned search cells: displayed line numbers and
//! disclosure controls. Tree indentation and value formatting stay with tiles.
use super::FuzzyFind;
use gpui::{prelude::*, *};
use gpui_component::ActiveTheme as _;

pub struct FindRow<'a> {
    pub(super) source: usize,
    pub(super) position: usize,
    pub(super) indices: &'a [usize],
    pub(super) number: Option<usize>,
    pub(super) gutter_width: f32,
    pub(super) visible_gutter_width: f32,
    pub(super) selected: bool,
    pub(super) expanded: Option<bool>,
    pub(super) context: bool,
    pub(super) owner: WeakEntity<FuzzyFind>,
}

impl FindRow<'_> {
    pub fn source_row(&self) -> usize {
        self.source
    }
    pub fn indices(&self) -> &[usize] {
        self.indices
    }
    pub fn is_context(&self) -> bool {
        self.context
    }
    pub fn gutter(&self, cx: &App) -> Div {
        self.render_gutter(self.gutter_width, cx)
    }

    /// Size to the painted numbers rather than all expanded search results.
    pub fn compact_gutter(&self, cx: &App) -> Div {
        self.render_gutter(self.visible_gutter_width, cx)
    }

    fn render_gutter(&self, width: f32, cx: &App) -> Div {
        let position = self.position;
        div()
            .flex()
            .flex_shrink_0()
            .justify_end()
            .w(px(width))
            .pr(px(if self.number.is_some() {
                crate::linenumbers::GUTTER_GAP_PX
            } else {
                0.
            }))
            .font_family(crate::fonts::MONO)
            .text_color(if self.selected {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .debug_selector(move || format!("find-gutter-{position}"))
            .children(self.number.map(|n| n.to_string()))
    }

    /// A disclosure slot. Its click never confirms a result or changes the
    /// original tree's expansion; it folds this search session's branch only.
    pub fn disclosure(
        &self,
        width: impl Into<Length>,
        leaf: &'static str,
        cx: &App,
    ) -> Stateful<Div> {
        let width: Length = width.into();
        let position = self.position;
        let slot = div()
            .id(("find-disclosure", self.source))
            .debug_selector(move || format!("find-chevron-{position}"))
            .w(width)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center();
        let Some(expanded) = self.expanded else {
            return slot.text_color(cx.theme().muted_foreground).child(leaf);
        };
        let owner = self.owner.clone();
        let row = self.source;
        slot.h_full()
            .rounded(cx.theme().radius_tokens().sm)
            .text_color(cx.theme().muted_foreground)
            .hover(|el| el.bg(cx.theme().list_hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |event, _, cx| {
                cx.stop_propagation();
                if event.click_count() == 1 {
                    let _ = owner.update(cx, |state, cx| state.toggle_branch(row, cx));
                }
            })
            .child(if expanded { "▾" } else { "▸" })
    }
}
