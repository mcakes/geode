//! The table's prepared rows (line-pricer spec §8.2): every visible row's
//! cells as `SharedString`s with the core's `CellState`, built on every
//! edit, delivery, expansion, view and clock change — never per
//! frame (`cell_text` allocates a `String` per cell). The paint is NOT
//! here: `Paints` resolves a state to a colour at render from a per-theme
//! memo (planning decision 14), so a theme switch rebuilds nothing.

use crate::core::columns::{CellState, ColumnKind, cell_text};
use crate::core::sheet::{LineId, RowKind, Sheet};
use crate::core::shorthand::render_expiry;
use crate::core::tree::{Expansion, visible_rows};
use crate::core::views::ColumnPlan;
use geode_core::clock::Clock;
use gpui::SharedString;

#[derive(Debug, Clone)]
pub struct GridColumn {
    /// The vocabulary's name: the table's column key, stable whatever a
    /// view labels it.
    pub name: &'static str,
    pub label: SharedString,
    /// Pixels (the vocabulary's widths; see `ColumnDef::default_width`).
    pub width: f32,
    /// Numbers read down the right edge.
    pub right: bool,
    pub kind: ColumnKind,
    pub editable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridRowKind {
    Line,
    Leg,
    Package { open: bool },
}

#[derive(Debug, Clone)]
pub struct GridCell {
    pub text: SharedString,
    pub state: CellState,
}

#[derive(Debug, Clone)]
pub struct GridRow {
    pub kind: GridRowKind,
    /// The sheet's flat row.
    pub row: Option<usize>,
    pub id: Option<LineId>,
    pub depth: usize,
    /// Column 0's label: the row's shorthand (spec §8.2).
    pub tree: SharedString,
    pub cells: Vec<GridCell>,
}

#[derive(Debug, Clone, Default)]
pub struct GridModel {
    pub columns: Vec<GridColumn>,
    pub rows: Vec<GridRow>,
}

fn right_aligned(kind: ColumnKind) -> bool {
    !matches!(
        kind,
        ColumnKind::Underlying
            | ColumnKind::Expiry
            | ColumnKind::Type
            | ColumnKind::BarrierType
            | ColumnKind::PricedAt
            | ColumnKind::Status
    )
}

/// A package's column-0 label: its template form while the legs still
/// match the table (the grammar round-trips it), else its template token
/// with its legs' distinct underlyings and expiries (spec §8.2).
fn package_label(sheet: &Sheet, row: usize) -> String {
    let text = sheet.shorthand(row);
    if !text.is_empty() && !text.contains('\n') {
        return text;
    }
    let RowKind::Package { template } = sheet.kind(row) else {
        return text;
    };
    let mut unds: Vec<String> = Vec::new();
    let mut exps: Vec<String> = Vec::new();
    for leg in sheet.children(row) {
        if let Some(i) = sheet.instrument(leg) {
            let u = i.underlying().to_string();
            if !unds.contains(&u) {
                unds.push(u);
            }
            let e = render_expiry(i.expiry());
            if !exps.contains(&e) {
                exps.push(e);
            }
        }
    }
    let mut parts = vec![template.token().to_string()];
    if !unds.is_empty() {
        parts.push(unds.join("/"));
    }
    if !exps.is_empty() {
        parts.push(exps.join("/"));
    }
    parts.join(" ")
}

impl GridModel {
    pub fn build(
        sheet: &Sheet,
        expansion: &Expansion,
        plan: &ColumnPlan,
        clock: Clock,
    ) -> GridModel {
        let columns: Vec<GridColumn> = plan
            .columns
            .iter()
            .map(|c| GridColumn {
                name: c.def.name,
                label: c.label.clone().into(),
                width: c.width,
                right: right_aligned(c.def.kind),
                kind: c.def.kind,
                editable: c.def.editable,
            })
            .collect();
        let visible = visible_rows(sheet, expansion);
        let mut rows = Vec::with_capacity(visible.len());
        for r in visible {
            let kind = match sheet.kind(r) {
                RowKind::Package { .. } => GridRowKind::Package {
                    open: expansion.is_open(sheet.id(r)),
                },
                RowKind::Line | RowKind::Underlying if sheet.parent(r).is_some() => {
                    GridRowKind::Leg
                }
                RowKind::Line | RowKind::Underlying => GridRowKind::Line,
            };
            let tree = if sheet.is_package(r) {
                package_label(sheet, r)
            } else {
                sheet.shorthand(r)
            };
            rows.push(GridRow {
                kind,
                row: Some(r),
                id: Some(sheet.id(r)),
                depth: sheet.depth(r),
                tree: tree.into(),
                cells: plan
                    .columns
                    .iter()
                    .map(|c| {
                        let t = cell_text(sheet, r, c.def, &c.format, clock);
                        GridCell {
                            text: t.text.into(),
                            state: t.state,
                        }
                    })
                    .collect(),
            });
        }
        GridModel { columns, rows }
    }

    pub fn grid_row_of(&self, id: LineId) -> Option<usize> {
        self.rows.iter().position(|r| r.id == Some(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::{Expansion, Sheet, Views};
    use geode_core::clock::Clock;
    use geode_core::pricing::OptionKind;

    /// [A, P(L1, L2), B].
    fn sheet() -> Sheet {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![callspread(1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s
    }

    fn plan() -> ColumnPlan {
        ColumnPlan::build(Views::builtin().get("vanilla").unwrap())
    }

    fn build(s: &Sheet, e: &Expansion) -> GridModel {
        GridModel::build(s, e, &plan(), Clock::utc())
    }

    #[test]
    fn rows_follow_the_expansion_and_carry_depth_ids_and_the_tree_label() {
        let s = sheet();
        let closed = build(&s, &Expansion::default());
        assert_eq!(closed.rows.len(), 3);
        assert_eq!(closed.rows[1].kind, GridRowKind::Package { open: false });
        assert_eq!(closed.rows[1].tree.as_ref(), "SPX Z26 4800/5200 CS");
        let mut e = Expansion::default();
        e.set(s.id(1), true);
        let open = build(&s, &e);
        assert_eq!(open.rows.len(), 5);
        assert_eq!(open.rows[1].kind, GridRowKind::Package { open: true });
        assert_eq!(open.rows[2].kind, GridRowKind::Leg);
        assert_eq!(open.rows[2].depth, 1);
        assert_eq!(open.rows[4].tree.as_ref(), "SPX Z26 4000 P");
        assert_eq!(open.grid_row_of(s.id(4)), Some(4));
        assert_eq!(
            open.columns.len(),
            plan().columns.len(),
            "the tree column is the delegate's own"
        );
    }

    #[test]
    fn cells_carry_the_core_text_and_state() {
        let mut s = sheet();
        let answers: Vec<_> = (0..s.len())
            .filter(|r| s.is_line(*r))
            .map(|r| (s.id(r), s.revision(r), Ok(result(12.5))))
            .collect();
        s.deliver_all(answers, at(0));
        let m = build(&s, &Expansion::default());
        let price = plan()
            .columns
            .iter()
            .position(|c| c.def.name == "price")
            .unwrap();
        assert_eq!(m.rows[0].cells[price].text.as_ref(), "12.50");
        assert_eq!(m.rows[0].cells[price].state, CellState::Own);
        let strike = plan()
            .columns
            .iter()
            .position(|c| c.def.name == "strike")
            .unwrap();
        assert_eq!(
            m.rows[1].cells[strike].state,
            CellState::Blank,
            "a package has no strike"
        );
        assert!(
            m.columns[price].right && !m.columns[1].right,
            "numbers read down the right edge"
        );
    }

    #[test]
    fn a_custom_package_labels_by_template_underlyings_and_expiries() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(crate::core::Edit::Group {
            first: 0,
            count: 2,
            template: crate::core::Template::Custom,
            id: None,
        })
        .unwrap();
        let m = build(&s, &Expansion::default());
        assert_eq!(m.rows[0].tree.as_ref(), "CUSTOM SPX Z26");
    }
}
