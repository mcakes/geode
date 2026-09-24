//! The tile's `TableDelegate` (line-pricer spec §8.2): a prepared
//! `Rc<GridModel>` swapped wholesale by `PricerTile::install_model`, a
//! mirror of the tile's cursor, and (Tasks 9–10) mirrors of the open entry
//! field and cell editor. The tile's own state is the truth; nothing here
//! decides anything. Column 0 is the tree column (indent, chevron,
//! shorthand), pinned left; the cursor never enters it.

use crate::grid::{GridModel, GridRowKind};
use crate::paint::Paints;
use geode_shell::fonts;
use geode_shell::shell::control::{self, PointerStates as _};
use gpui::prelude::*;
use gpui::{App, ClickEvent, Context, EventEmitter, SharedString, TextAlign, Window, div, px};
use gpui_component::table::{Column, ColumnFixed, TableDelegate, TableState};
use gpui_component::{ActiveTheme as _, Theme};
use std::rc::Rc;

/// The tree column: pixels, like every width here (the vocabulary's own
/// known gap); not resizable, since a dragged width has nowhere to live.
const TREE_WIDTH: f32 = 260.0;
const INDENT: f32 = 14.0;
pub(crate) const TREE_COL: usize = 0;

/// A chevron click, re-implemented from the blotter (spec §8.2: "the
/// blotter's idiom re-implemented, nothing lifted"); the tile toggles
/// the package at this grid row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChevronClicked(pub usize);

impl EventEmitter<ChevronClicked> for TableState<SheetDelegate> {}

pub struct SheetDelegate {
    pub(crate) model: Rc<GridModel>,
    /// `(grid row, plan column)`; `None` with no cursor row.
    pub(crate) cursor: Option<(usize, usize)>,
    pub(crate) paints: Paints,
    chevron: Option<(control::ControlInputs, control::ControlPaint)>,
}

impl SheetDelegate {
    pub(crate) fn new(theme: &Theme) -> Self {
        SheetDelegate {
            model: Rc::new(GridModel::default()),
            cursor: None,
            paints: Paints::derive(theme),
            chevron: None,
        }
    }

    /// The plan column behind table column `col_ix`; `None` is the tree.
    pub(crate) fn plan_col(col_ix: usize) -> Option<usize> {
        (col_ix != TREE_COL).then(|| col_ix - 1)
    }

    fn chevron_states(&mut self, theme: &Theme) -> control::ControlPaint {
        let inputs = control::ControlInputs::new(
            theme,
            control::Rest::Bare,
            theme.table,
            theme.muted_foreground,
        );
        match &self.chevron {
            Some((have, paint)) if *have == inputs => *paint,
            _ => {
                let paint = control::control_paint(&inputs);
                self.chevron = Some((inputs, paint));
                paint
            }
        }
    }
}

impl TableDelegate for SheetDelegate {
    fn columns_count(&self, _cx: &App) -> usize {
        1 + self.model.columns.len()
    }

    fn rows_count(&self, _cx: &App) -> usize {
        self.model.rows.len()
    }

    /// Read only on prepare and `TableState::refresh`, which is why every
    /// model swap goes through `install_model`.
    fn column(&self, col_ix: usize, _cx: &App) -> Column {
        let Some(c) = Self::plan_col(col_ix).and_then(|i| self.model.columns.get(i)) else {
            return Column {
                key: SharedString::from("__tree"),
                name: SharedString::from("line"),
                align: TextAlign::Left,
                sort: None,
                width: px(TREE_WIDTH),
                fixed: Some(ColumnFixed::Left),
                movable: false,
                resizable: false,
                ..Column::default()
            };
        };
        Column {
            key: c.label.clone(),
            name: c.label.clone(),
            align: if c.right {
                TextAlign::Right
            } else {
                TextAlign::Left
            },
            sort: None,
            width: px(c.width),
            movable: false,
            resizable: false,
            ..Column::default()
        }
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = self.column(col_ix, cx);
        div()
            .size_full()
            .flex()
            .items_center()
            .when(matches!(column.align, TextAlign::Right), |el| {
                el.justify_end()
            })
            .font_family(fonts::MONO)
            .debug_selector(|| format!("pricer-th-{col_ix}"))
            .child(column.name)
    }

    /// One prepared cell. Nothing is formatted or allocated here beyond
    /// the `debug_selector` closure (dropped unevaluated outside tests):
    /// the text is a `SharedString` refcount out of the model, the colours
    /// `Copy` reads of the `Paints` memo, which the tile re-derives on a
    /// theme change rather than per cell.
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let paints = self.paints;
        // One `Rc` clone, so `chevron_states(&mut self)` can run while a
        // row is borrowed.
        let model = Rc::clone(&self.model);
        let Some(row) = model.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let package = matches!(row.kind, GridRowKind::Package { .. });
        let (active_border, chevron_text, radius) = {
            let t = cx.theme();
            (
                t.table_active_border,
                t.muted_foreground,
                t.radius_tokens().sm,
            )
        };
        let base = div()
            .size_full()
            .flex()
            .items_center()
            .font_family(fonts::MONO)
            .whitespace_nowrap()
            .overflow_hidden()
            .when(package, |el| el.bg(paints.package_ground))
            .debug_selector(|| format!("pricer-cell-{row_ix}-{col_ix}"));
        let Some(plan_col) = Self::plan_col(col_ix) else {
            // The tree column: indent by depth, a chevron on a package,
            // then the row's shorthand.
            let mut el = base
                .pl(px(row.depth as f32 * INDENT))
                .text_color(paints.text(crate::core::CellState::Own, package));
            if let GridRowKind::Package { open } = row.kind {
                let states = self.chevron_states(cx.theme());
                el = el.child(
                    div()
                        .id(("pricer-chevron", row_ix))
                        .w(px(14.))
                        .rounded(radius)
                        .text_color(chevron_text)
                        .pointer_states(states)
                        .debug_selector(|| format!("pricer-chevron-{row_ix}"))
                        .on_click(cx.listener(move |this, e: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            // A double-click toggles once (the blotter's rule).
                            if e.click_count() > 1 {
                                return;
                            }
                            this.set_selected_row(row_ix, cx);
                            cx.emit(ChevronClicked(row_ix));
                        }))
                        .child(if open { "▾" } else { "▸" }),
                );
            }
            return el.child(row.tree.clone()).into_any_element();
        };
        let at_cursor = self.cursor == Some((row_ix, plan_col));
        let right = model.columns.get(plan_col).is_some_and(|c| c.right);
        base.when(right, |el| el.justify_end())
            .when(at_cursor, |el| el.border_1().border_color(active_border))
            .when_some(row.cells.get(plan_col), |el, cell| {
                el.text_color(paints.text(cell.state, package))
                    .child(cell.text.clone())
            })
            .into_any_element()
    }
}
