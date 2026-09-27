//! The footer strip a grid tile shows while a selection is live (grid
//! selection spec §3.3): the selection's extent, then one group per
//! selected numeric column — its label and its total.
//!
//! A group reads as belonging to its column: the label takes the
//! column's own color the way its header does, and the total is painted
//! the way the column's cells paint the same number. The tile formats
//! every string when the selection or the data changes and resolves the
//! colors once per theme ([`CellPaint`]); this only lays prepared values
//! out, so painting it per frame formats and resolves nothing.

use gpui::prelude::*;
use gpui::{Div, Hsla, SharedString, div};
use gpui_component::{Theme, h_flex};

use crate::fonts;
use geode_core::format::Sign;

/// One selected numeric column's group: its label and its total, as the
/// tile formatted them.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateCell {
    pub label: SharedString,
    pub text: SharedString,
    /// The total's sign; `None` paints plain.
    pub sign: Option<Sign>,
    /// The text is a refusal mark (`—†`, `—‡`, `—`), not a number.
    pub refused: bool,
}

/// A group's colors, resolved by the tile from its column's format:
/// `label` is the header's color (`None` keeps the muted label), and
/// `positive`/`negative`/`zero` are what a signed cell of that column
/// paints.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellPaint {
    pub label: Option<Hsla>,
    pub positive: Hsla,
    pub negative: Hsla,
    pub zero: Hsla,
}

impl CellPaint {
    /// An uncolored column: every value in the foreground.
    pub fn plain(theme: &Theme) -> CellPaint {
        CellPaint {
            label: None,
            positive: theme.foreground,
            negative: theme.foreground,
            zero: theme.foreground,
        }
    }

    /// A total's color: by sign, the foreground when unsigned, muted for
    /// a refusal mark.
    pub fn value(&self, cell: &AggregateCell, theme: &Theme) -> Hsla {
        if cell.refused {
            return theme.muted_foreground;
        }
        match cell.sign {
            Some(Sign::Positive) => self.positive,
            Some(Sign::Negative) => self.negative,
            Some(Sign::Zero) => self.zero,
            None => theme.foreground,
        }
    }
}

/// `paints` pairs with `cells` by index; a missing entry paints plain.
pub fn strip(
    extent: Option<&SharedString>,
    cells: &[AggregateCell],
    paints: &[CellPaint],
    theme: &Theme,
) -> Div {
    let plain = CellPaint::plain(theme);
    h_flex()
        .gap_3()
        .items_center()
        .when_some(extent, |el, extent| {
            el.child(
                div()
                    .text_color(theme.muted_foreground)
                    .debug_selector(|| "aggregate-extent".to_string())
                    .child(extent.clone()),
            )
        })
        .children(cells.iter().enumerate().map(|(i, c)| {
            let paint = paints.get(i).copied().unwrap_or(plain);
            let label = c.label.clone();
            h_flex()
                .gap_2()
                .items_center()
                // A hairline parts each column's group from what precedes
                // it (the extent, or the previous group).
                .pl_3()
                .border_l_1()
                .border_color(theme.border)
                .debug_selector(move || format!("aggregate-{label}"))
                .child(
                    div()
                        .text_color(paint.label.unwrap_or(theme.muted_foreground))
                        .child(c.label.clone()),
                )
                .child(
                    div()
                        .font_family(fonts::MONO)
                        .text_color(paint.value(c, theme))
                        .child(c.text.clone()),
                )
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use gpui_component::ActiveTheme as _;

    fn cell(label: &str, text: &str, sign: Option<Sign>) -> AggregateCell {
        AggregateCell {
            label: label.to_string().into(),
            text: text.to_string().into(),
            sign,
            refused: false,
        }
    }

    struct Host(Option<SharedString>, Vec<AggregateCell>);
    impl gpui::Render for Host {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            strip(self.0.as_ref(), &self.1, &[], cx.theme())
        }
    }

    #[gpui::test]
    fn the_extent_and_each_column_group_are_painted(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let cells = vec![
            cell("delta", "3.00", Some(Sign::Positive)),
            cell("gamma", "—", None),
        ];
        let extent = Some(SharedString::from("2 rows × 3 cols"));
        let (_view, cx) = cx.add_window_view(|_, _| Host(extent, cells));
        cx.run_until_parked();
        assert!(cx.debug_bounds("aggregate-extent").is_some());
        assert!(cx.debug_bounds("aggregate-delta").is_some());
        assert!(cx.debug_bounds("aggregate-gamma").is_some());
    }

    #[gpui::test]
    fn a_total_takes_its_sign_color_and_a_refusal_is_muted(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            let paint = CellPaint {
                label: Some(theme.info),
                positive: theme.chart_bullish,
                negative: theme.chart_bearish,
                zero: theme.foreground,
            };
            let neg = cell("d", "-3.00", Some(Sign::Negative));
            let pos = cell("d", "1.00", Some(Sign::Positive));
            let unsigned = cell("d", "1.00", None);
            let refusal = AggregateCell {
                refused: true,
                ..cell("d", "—‡", None)
            };
            assert_eq!(paint.value(&neg, theme), theme.chart_bearish);
            assert_eq!(paint.value(&pos, theme), theme.chart_bullish);
            assert_eq!(paint.value(&unsigned, theme), theme.foreground);
            assert_eq!(paint.value(&refusal, theme), theme.muted_foreground);
        });
    }
}
