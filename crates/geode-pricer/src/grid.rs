//! Prepared rows for the pricer table. The tile rebuilds this model when sheet data,
//! expansion, columns, or the display clock changes. Each visible row stores formatted
//! `SharedString` cells and their `CellState`, so painting does not format cell values.
//!
//! Colours live separately in `Paints`: a theme change replaces the palette without
//! rebuilding these rows. The delegate paints the tree column from each row's depth,
//! kind, tag, text and note, all prepared here. A row's shorthand is also its search
//! key.

use crate::core::columns::{CellState, ColumnKind, cell_text};
use crate::core::sheet::{LineId, RowKind, Sheet};
use crate::core::shorthand::{render_expiry, render_strike};
use crate::core::tree::{Expansion, visible_rows};
use crate::core::views::ColumnPlan;
use geode_core::clock::Clock;
use geode_core::colour::Sign;
use geode_core::view::Colour;
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
    /// The view's `color` for the column, merged from every presentation
    /// layer: what the delegate paints cells and the header by.
    pub colour: Colour,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridRowKind {
    Line,
    /// `last`: the final leg of its package in sheet order — it takes the
    /// corner connector (`└`), every other leg the tee (`├`).
    Leg {
        last: bool,
    },
    Package {
        open: bool,
    },
}

#[derive(Debug, Clone)]
pub struct GridCell {
    pub text: SharedString,
    pub state: CellState,
    /// A measure's sign as formatted; `None` on a text cell.
    pub sign: Option<Sign>,
}

#[derive(Debug, Clone)]
pub struct GridRow {
    pub kind: GridRowKind,
    /// The sheet's flat row.
    pub row: Option<usize>,
    pub id: Option<LineId>,
    pub depth: usize,
    /// Tree-column tag: a package's template token (`CS`, `CUSTOM`), empty on a
    /// line or leg.
    pub tag: SharedString,
    /// Tree-column text: a package's summary (`package_summary`); a leg's
    /// or a bare line's full one-line shorthand (`-2 SPX Z26 5000 C`).
    pub text: SharedString,
    /// Tree-column note: a package's leg count (`· 1 leg`, `· 2 legs`),
    /// empty on a line or leg.
    pub note: SharedString,
    /// Find key derived from shorthand. It can match text that does not appear in
    /// the current view's columns.
    pub search: SharedString,
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
        ColumnKind::SheetName
            | ColumnKind::PositionRef
            | ColumnKind::InstrumentRef
            | ColumnKind::Template
            | ColumnKind::UnderlyingRef
            | ColumnKind::Expiry
            | ColumnKind::OptionType
            | ColumnKind::Currency
            | ColumnKind::BarrierType
            | ColumnKind::PricedAt
            | ColumnKind::Status
    )
}

/// A package's search key: its template form while the legs still match
/// the table (the grammar round-trips it), else its template token with
/// its legs' distinct underlyings and expiries.
fn package_search(sheet: &Sheet, row: usize) -> String {
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

/// A package's tree-column summary: its legs' distinct expiries, then
/// their distinct strikes, each in leg order joined with `/`
/// (`Z26 4800/5200`). One line whatever form the package's own
/// shorthand takes; empty when no leg carries an instrument.
pub(crate) fn package_summary(sheet: &Sheet, row: usize) -> String {
    let mut exps: Vec<String> = Vec::new();
    let mut strikes: Vec<String> = Vec::new();
    for leg in sheet.children(row) {
        if let Some(i) = sheet.instrument(leg) {
            let e = render_expiry(i.expiry());
            if !exps.contains(&e) {
                exps.push(e);
            }
            let k = render_strike(i.strike());
            if !strikes.contains(&k) {
                strikes.push(k);
            }
        }
    }
    [exps.join("/"), strikes.join("/")]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn leg_note(n: usize) -> String {
    if n == 1 {
        "· 1 leg".to_string()
    } else {
        format!("· {n} legs")
    }
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
                colour: c.format.colour.clone(),
            })
            .collect();
        let visible = visible_rows(sheet, expansion);
        let mut rows = Vec::with_capacity(visible.len());
        for r in visible {
            let kind = match sheet.kind(r) {
                RowKind::Package { .. } => GridRowKind::Package {
                    open: expansion.is_open(sheet.id(r)),
                },
                RowKind::Line | RowKind::Underlying => match sheet.parent(r) {
                    Some(p) => GridRowKind::Leg {
                        last: sheet.children(p).end == r + 1,
                    },
                    None => GridRowKind::Line,
                },
            };
            let (tag, text, note, search) = match sheet.kind(r) {
                RowKind::Package { template } => (
                    SharedString::new_static(template.token()),
                    SharedString::from(package_summary(sheet, r)),
                    SharedString::from(leg_note(sheet.children(r).len())),
                    SharedString::from(package_search(sheet, r)),
                ),
                _ => {
                    // One shorthand, shared by the painted text and the find key.
                    let s = SharedString::from(sheet.shorthand(r));
                    (
                        SharedString::default(),
                        s.clone(),
                        SharedString::default(),
                        s,
                    )
                }
            };
            rows.push(GridRow {
                kind,
                row: Some(r),
                id: Some(sheet.id(r)),
                depth: sheet.depth(r),
                tag,
                text,
                note,
                search,
                cells: plan
                    .columns
                    .iter()
                    .map(|c| {
                        let t = cell_text(sheet, r, c.def, &c.format, clock);
                        GridCell {
                            text: t.text.into(),
                            state: t.state,
                            sign: t.sign,
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
    use geode_core::pricing::{Measure, OptionKind};

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
    fn rows_follow_the_expansion_and_carry_depth_ids_tags_and_search_keys() {
        let s = sheet();
        let closed = build(&s, &Expansion::default());
        assert_eq!(closed.rows.len(), 3);
        assert_eq!(closed.rows[1].kind, GridRowKind::Package { open: false });
        assert_eq!(closed.rows[1].search.as_ref(), "SPX Z26 4800/5200 CS");
        assert_eq!(
            closed.rows[1].tag.as_ref(),
            "CS",
            "a package: its template token"
        );
        assert_eq!(closed.rows[0].tag.as_ref(), "", "a line: no tag");
        let mut e = Expansion::default();
        e.set(s.id(1), true);
        let open = build(&s, &e);
        assert_eq!(open.rows.len(), 5);
        assert_eq!(open.rows[1].kind, GridRowKind::Package { open: true });
        assert_eq!(open.rows[2].kind, GridRowKind::Leg { last: false });
        assert_eq!(open.rows[2].depth, 1);
        assert_eq!(open.rows[2].tag.as_ref(), "", "a leg: no tag");
        assert_eq!(open.rows[4].search.as_ref(), "SPX Z26 4000 P");
        assert_eq!(open.grid_row_of(s.id(4)), Some(4));
        assert_eq!(
            open.columns.len(),
            plan().columns.len(),
            "the tree column is the delegate's own"
        );
    }

    /// Column 0's prepared strings: a package's summary (distinct expiries,
    /// then distinct strikes, leg order) and leg count; a leg's and a bare
    /// line's full shorthand; nothing formatted at paint.
    #[test]
    fn the_tree_text_is_prepared_per_row_kind() {
        let s = sheet(); // [A, P(L1, L2), B]
        let mut e = Expansion::default();
        e.set(s.id(1), true);
        let m = build(&s, &e);
        assert_eq!(m.rows[0].text.as_ref(), "SPX Z26 5000 C");
        assert_eq!(m.rows[0].note.as_ref(), "", "a bare line has no note");
        assert_eq!(m.rows[1].text.as_ref(), "Z26 4800/5200");
        assert_eq!(m.rows[1].note.as_ref(), "· 2 legs");
        assert_eq!(m.rows[2].text.as_ref(), "SPX Z26 4800 C");
        assert_eq!(m.rows[3].text.as_ref(), "-1 SPX Z26 5200 C");
        assert_eq!(m.rows[2].note.as_ref(), "", "a leg has no note");
        assert_eq!(m.rows[4].text.as_ref(), "SPX Z26 4000 P");
    }

    #[test]
    fn the_last_leg_of_every_package_takes_the_corner_connector() {
        // Ends in a package, so the last leg is the sheet's last row.
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        push(&mut s, vec![callspread(2)]);
        let mut e = Expansion::default();
        e.open_all(&s);
        let m = build(&s, &e);
        let kinds: Vec<GridRowKind> = m.rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                GridRowKind::Package { open: true },
                GridRowKind::Leg { last: false },
                GridRowKind::Leg { last: true },
                GridRowKind::Line,
                GridRowKind::Package { open: true },
                GridRowKind::Leg { last: false },
                GridRowKind::Leg { last: true },
            ]
        );
    }

    #[test]
    fn a_one_leg_package_counts_one_leg_and_its_leg_is_last() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(1)]);
        s.apply(crate::core::Edit::Remove { at: 2 }).unwrap();
        let mut e = Expansion::default();
        e.open_all(&s);
        let m = build(&s, &e);
        assert_eq!(m.rows.len(), 2);
        assert_eq!(m.rows[0].note.as_ref(), "· 1 leg");
        assert_eq!(m.rows[0].text.as_ref(), "Z26 4800");
        assert_eq!(m.rows[1].kind, GridRowKind::Leg { last: true });
    }

    #[test]
    fn a_list_form_package_summary_is_one_line() {
        // A custom package (legs match no template table) renders its
        // shorthand one leg per line; the summary must not.
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(crate::core::Edit::Group {
            first: 0,
            count: 2,
            template: crate::core::Template::CUSTOM,
            id: None,
        })
        .unwrap();
        assert!(
            s.shorthand(0).contains('\n'),
            "precondition: list form, {:?}",
            s.shorthand(0)
        );
        let m = build(&s, &Expansion::default());
        assert!(!m.rows[0].text.contains('\n'), "{:?}", m.rows[0].text);
        assert_eq!(m.rows[0].text.as_ref(), "Z26 5000/4000");
        assert_eq!(m.rows[0].note.as_ref(), "· 2 legs");
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
            .position(|c| c.def.name == "npv")
            .unwrap();
        assert_eq!(m.rows[0].cells[price].text.as_ref(), "12.50");
        assert_eq!(m.rows[0].cells[price].state, CellState::Own);
        let strike = plan()
            .columns
            .iter()
            .position(|c| c.def.name == "strike")
            .unwrap();
        assert_eq!(
            (
                m.rows[1].cells[strike].text.as_ref(),
                m.rows[1].cells[strike].state
            ),
            ("4800/5200", CellState::Own),
            "a package shows its legs' strikes"
        );
        assert!(
            m.columns[price].right && !m.columns[1].right,
            "numbers read down the right edge"
        );
    }

    /// A measure cell carries the sign of the value it was formatted
    /// from (rounded as the text is); a text cell and an unpriced cell
    /// carry none.
    #[test]
    fn a_measure_cell_carries_its_sign_and_a_text_cell_none() {
        let mut s = sheet();
        let col = |name: &str| {
            plan()
                .columns
                .iter()
                .position(|c| c.def.name == name)
                .unwrap()
        };
        let before = build(&s, &Expansion::default());
        assert_eq!(before.rows[0].cells[col("npv")].sign, None, "unpriced");
        let mut neg = result(-12.5);
        neg.set(Measure::Delta01, false, -0.5);
        neg.set(Measure::Gamma01, false, 0.0);
        s.deliver_all(vec![(s.id(0), s.revision(0), Ok(neg))], at(0));
        let m = build(&s, &Expansion::default());
        assert_eq!(m.rows[0].cells[col("npv")].sign, Some(Sign::Negative));
        assert_eq!(m.rows[0].cells[col("delta01")].sign, Some(Sign::Negative));
        assert_eq!(m.rows[0].cells[col("gamma01")].sign, Some(Sign::Zero));
        assert_eq!(m.rows[0].cells[col("strike")].sign, None, "text");
        assert_eq!(m.rows[0].cells[col("qty")].sign, None, "a dimension");
        assert_eq!(
            m.columns[col("npv")].colour,
            Colour::Sign,
            "a measure's vocabulary default"
        );
        assert_eq!(m.columns[col("qty")].colour, Colour::None);
    }

    #[test]
    fn a_custom_package_is_tagged_by_its_token_and_searched_by_its_legs() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(4000.0, OptionKind::Put), 1)]);
        s.apply(crate::core::Edit::Group {
            first: 0,
            count: 2,
            template: crate::core::Template::CUSTOM,
            id: None,
        })
        .unwrap();
        let m = build(&s, &Expansion::default());
        assert_eq!(m.rows[0].tag.as_ref(), "CUSTOM");
        assert_eq!(m.rows[0].search.as_ref(), "CUSTOM SPX Z26");
    }
}
