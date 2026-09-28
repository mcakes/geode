//! One `TableDelegate` for every table section, over a shared prepared
//! table. The page owns the truth (cursor, expansion, filters) and hears
//! row selection through `TableEvent::SelectRow` on the `TableState`; the
//! delegate only paints.

use std::rc::Rc;

use geode_shell::fonts;
use geode_shell::shell::{chip, scale};
use gpui::prelude::*;
use gpui::{App, Context, SharedString, TextAlign, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::table::{Column, TableDelegate, TableState};

use crate::model::Tone;
use crate::prepared::{PreparedTable, RowKind};

/// Design pixels of left padding per nesting level of a child cell.
const INDENT_STEP: f32 = 12.0;

pub struct SectionDelegate {
    table: Rc<PreparedTable>,
    rem_px: f32,
    /// Prefix of the per-row debug selector. The Config section paints two
    /// tables at once; distinct prefixes keep their rows addressable.
    row_selector: &'static str,
}

impl SectionDelegate {
    /// The cursor table's delegate; its rows are `diagnostics-row-{ix}`.
    pub fn new() -> SectionDelegate {
        Self::with_row_selector("diagnostics-row")
    }

    pub fn with_row_selector(row_selector: &'static str) -> SectionDelegate {
        SectionDelegate {
            table: Rc::new(PreparedTable::empty()),
            rem_px: scale::DESIGN_REM,
            row_selector,
        }
    }

    pub fn set(&mut self, table: Rc<PreparedTable>) {
        self.table = table;
    }

    /// The page keeps its own `Rc` of the table; tests read the delegate's
    /// copy to check what a table paints.
    #[cfg(test)]
    pub fn table(&self) -> &Rc<PreparedTable> {
        &self.table
    }

    /// The page passes the window's rem so column widths follow the font
    /// size, like the sidebar width does. `column()` has no `Window` to
    /// read it from itself.
    pub fn set_rem(&mut self, rem_px: f32) {
        self.rem_px = rem_px;
    }
}

impl Default for SectionDelegate {
    fn default() -> Self {
        Self::new()
    }
}

impl TableDelegate for SectionDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        self.table.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.table.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let spec = self.table.columns[col_ix];
        Column {
            key: SharedString::new_static(spec.key),
            name: SharedString::new_static(spec.name),
            align: if spec.right {
                TextAlign::Right
            } else {
                TextAlign::Left
            },
            sort: None,
            width: px(scale::design_px(spec.width, px(self.rem_px))),
            fixed: None,
            movable: false,
            resizable: true,
            ..Column::default()
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        // `cell_at` places a notice row's one cell in the widest column;
        // the table clips every td to its column, so the first column
        // would cut the message short.
        let Some(cell) = self.table.cell_at(row_ix, col_ix) else {
            return div();
        };
        let row = &self.table.rows[row_ix];
        let color = match cell.tone {
            Tone::Normal => theme.foreground,
            Tone::Muted => theme.muted_foreground,
            Tone::Warn => chip::chip_paint(theme, chip::Tone::WarningText).text,
            Tone::Error => chip::chip_paint(theme, chip::Tone::DangerText).text,
            Tone::Marked => theme.primary,
        };
        let expander = match (col_ix, row.kind) {
            (0, RowKind::Parent { expanded: true }) => "▾ ",
            (0, RowKind::Parent { expanded: false }) => "▸ ",
            _ => "",
        };
        // Parents are few, so their per-paint `SharedString` is cheap; a
        // plain cell hands over the prepared text without allocating.
        let text = if expander.is_empty() {
            cell.text.clone()
        } else {
            SharedString::from(format!("{expander}{}", cell.text))
        };
        // The first cell names its row for pointer tests; a no-op outside
        // test builds.
        let row_selector = self.row_selector;
        div()
            .when(col_ix == 0, |el| {
                el.debug_selector(move || format!("{row_selector}-{row_ix}"))
            })
            .w_full()
            .pl(scale::design(f32::from(cell.indent) * INDENT_STEP))
            .font_family(fonts::MONO)
            .text_color(color)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .child(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prepared::log_table;

    /// Column widths are design pixels: identity at the design rem and
    /// proportional to the rem the page hands over, so a larger font size
    /// widens the columns with the text they hold.
    #[gpui::test]
    fn the_delegate_reports_the_set_table_and_scales_widths_with_the_rem(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut d = SectionDelegate::new();
        cx.update(|cx| {
            assert_eq!(d.columns_count(cx), 0);
            assert_eq!(d.rows_count(cx), 0);
        });
        d.set(Rc::new(log_table(&[], 3)));
        cx.update(|cx| {
            assert_eq!(d.columns_count(cx), 4);
            assert_eq!(d.rows_count(cx), 1, "the loss notice is a row");
            let at_design = d.column(0, cx);
            assert_eq!(at_design.key.as_ref(), "time");
            assert_eq!(at_design.width, px(d.table().columns[0].width));
            d.set_rem(scale::DESIGN_REM * 2.0);
            let doubled = d.column(0, cx);
            assert_eq!(doubled.width, at_design.width * 2.0);
            assert!(!doubled.movable);
        });
    }
}
