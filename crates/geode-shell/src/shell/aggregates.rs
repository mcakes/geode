//! The footer strip a grid tile shows while a selection is live (grid
//! selection spec §3.3): one `label text` pair per selected numeric
//! column. The tile formats every string when the selection or the data
//! changes; this only lays prepared strings out, so painting it per
//! frame formats nothing.

use gpui::prelude::*;
use gpui::{Div, SharedString, div};
use gpui_component::{Theme, h_flex};

#[derive(Debug, Clone, PartialEq)]
pub struct AggregateCell {
    pub label: SharedString,
    pub text: SharedString,
}

pub fn strip(cells: &[AggregateCell], theme: &Theme) -> Div {
    h_flex()
        .gap_3()
        .items_center()
        .children(cells.iter().map(|c| {
            let label = c.label.clone();
            h_flex()
                .gap_1()
                .debug_selector(move || format!("aggregate-{label}"))
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(c.label.clone()),
                )
                .child(div().text_color(theme.foreground).child(c.text.clone()))
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use gpui_component::ActiveTheme as _;

    struct Host(Vec<AggregateCell>);
    impl gpui::Render for Host {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            strip(&self.0, cx.theme())
        }
    }

    #[gpui::test]
    fn each_cell_paints_its_label_and_text(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let cells = vec![
            AggregateCell {
                label: "delta".into(),
                text: "Σ 3.00 · n 2".into(),
            },
            AggregateCell {
                label: "gamma".into(),
                text: "n 0".into(),
            },
        ];
        let (_view, cx) = cx.add_window_view(|_, _| Host(cells));
        cx.run_until_parked();
        assert!(cx.debug_bounds("aggregate-delta").is_some());
        assert!(cx.debug_bounds("aggregate-gamma").is_some());
    }
}
