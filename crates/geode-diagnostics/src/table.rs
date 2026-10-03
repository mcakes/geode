//! One `TableDelegate` for every table section, over a shared prepared
//! table. The page owns the truth (cursor, expansion, filters) and hears
//! row selection through `TableEvent::SelectRow` on the `TableState`; the
//! delegate only paints.

use std::rc::Rc;

use geode_shell::fonts;
use geode_shell::shell::{chip, scale};
use gpui::prelude::*;
use gpui::{App, Context, Div, Entity, FocusHandle, SharedString, TextAlign, Window, div, px};
use gpui_component::table::{Column, DataTable, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Sizable as _, Size, v_flex};

use crate::model::Tone;
use crate::prepared::{PreparedTable, RowKind};

/// Design pixels of left padding per nesting level of a child cell.
const INDENT_STEP: f32 = 12.0;

/// A page table inside the wrapper that keeps the page handle as the
/// focus owner. `DataTable`'s root tracks the table's own handle, so a row
/// click would move window focus into the table, whose `escape` clears its
/// selection and stops there whenever a row is selected, which is always:
/// the page keeps `set_selected_row` in step with its cursor. The first
/// Escape after a click would then clear the highlight instead of closing
/// the page. The capture-phase `prevent_default` runs before the
/// bubble-phase focus transfer and suppresses it; the row's own click still
/// fires, because gpui records the pending mouse-down independently of the
/// default action. Explicit page focus also leaves a focused filter when a
/// row is clicked. The wrapper fills what its panel leaves; without
/// `min_h_0` the virtualised list would take its intrinsic height and paint
/// no rows.
pub(crate) fn table_el(state: &Entity<TableState<SectionDelegate>>, focus: &FocusHandle) -> Div {
    let focus = focus.clone();
    div()
        .debug_selector(|| "diagnostics-table".to_string())
        .flex_1()
        .min_h_0()
        .w_full()
        .capture_any_mouse_down(move |_event, window, cx| {
            window.prevent_default();
            focus.focus(window, cx);
        })
        .child(
            DataTable::new(state)
                .with_size(Size::XSmall)
                .bordered(false)
                .stripe(false),
        )
}

pub struct SectionDelegate {
    table: Rc<PreparedTable>,
    rem_px: f32,
    /// Prefix of the per-row debug selector. The Config section paints two
    /// tables at once; distinct prefixes keep their rows addressable.
    row_selector: &'static str,
    empty_title: SharedString,
    empty_help: SharedString,
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
            empty_title: SharedString::new_static("No rows"),
            empty_help: SharedString::default(),
        }
    }

    pub fn set(&mut self, table: Rc<PreparedTable>) {
        self.table = table;
    }

    /// A static text shares its literal; only a formatted one allocates.
    pub(crate) fn set_empty(
        &mut self,
        title: impl Into<SharedString>,
        help: impl Into<SharedString>,
    ) {
        self.empty_title = title.into();
        self.empty_help = help.into();
    }

    #[cfg(test)]
    pub fn empty_title(&self) -> &SharedString {
        &self.empty_title
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
    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .debug_selector(|| "diagnostics-empty".to_string())
            .child(div().text_sm().child(self.empty_title.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.empty_help.clone()),
            )
    }

    fn columns_count(&self, _cx: &App) -> usize {
        self.table.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.table.rows.len()
    }

    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let spec = &self.table.columns[col_ix];
        Column {
            key: spec.key.clone(),
            name: spec.name.clone(),
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

    /// The table asks for every header on every frame: a reference
    /// column's declared name is shared, not copied, so a repeated
    /// `column()` hands out the same allocation. `SharedString` keeps a
    /// name of up to 23 bytes inline, which copies without allocating, so
    /// the pointer check needs a longer, heap-held name.
    #[gpui::test]
    fn a_reference_header_shares_its_name_across_calls(cx: &mut gpui::TestAppContext) {
        const LONG: &str = "settlement_calendar_for_the_listed_future";
        let mut answer = crate::model::tests::ref_table();
        answer.columns[1] = LONG.to_string();
        let mut d = SectionDelegate::new();
        d.set(Rc::new(crate::prepared::reference_table(Some(&answer), "")));
        cx.update(|cx| {
            let (first, second) = (d.column(1, cx), d.column(1, cx));
            assert_eq!(first.name.as_ref(), LONG);
            assert!(std::ptr::eq(first.name.as_ptr(), second.name.as_ptr()));
            assert!(std::ptr::eq(first.key.as_ptr(), second.key.as_ptr()));
            assert!(std::ptr::eq(
                first.name.as_ptr(),
                d.table().columns[1].name.as_ptr()
            ));
        });
    }
}
