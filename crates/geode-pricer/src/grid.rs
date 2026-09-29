//! Prepared rows for the pricer table, flattened from the rollup tree. The tile rebuilds
//! this model when sheet data, the scope, the grouping, either expansion, columns, or
//! the display clock changes. Each painted row stores formatted `SharedString` cells and
//! their `CellState`, so painting does not format cell values. A grouping row's cells
//! sum its legs' measures and read its dimensions' unanimity (`group_cell_text`).
//!
//! Colours live separately in `Paints`: a theme change replaces the palette without
//! rebuilding these rows. The delegate paints the tree column from each row's depth,
//! kind, tag, text and note, all prepared here. A row's shorthand is also its search
//! key.

use crate::core::columns::{
    CellState, CellText, ColumnKind, cell_text, group_cell_text, leg_reading, subset_cell_text,
};
use crate::core::rollup::{NULL_LABEL, NodeKind, Rollup, legs_under};
use crate::core::sheet::{LineId, RowKind, Sheet};
use crate::core::shorthand::{render_expiry, render_strike};
use crate::core::tree::Expansion;
use crate::core::views::ColumnPlan;
use geode_core::clock::Clock;
use geode_core::colour::Sign;
use geode_core::expansion::{Expansion as GroupExpansion, Path};
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
    /// A grouping node: open while its path is open in the group
    /// expansion; `depth` its level (0 at the roots). Read-only, and the
    /// only row that paints a ground of its own.
    Group { open: bool, depth: usize },
    /// A bare line, or a leg standing alone under `instrument_ref`.
    Line,
    /// `last`: the final leg of its package in sheet order — it takes the
    /// corner connector (`└`), every other leg the tee (`├`).
    Leg { last: bool },
    /// `partial`: fewer of its legs sit under this node than it has (the
    /// scope hides some, or the grouping splits it); the row aggregates
    /// only this node's and is read-only. `split`: the grouping put its
    /// shown legs under more than one node (implies `partial`). Either
    /// reads `· n of M legs`.
    Package {
        open: bool,
        partial: bool,
        split: bool,
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
    /// The sheet's flat row; `None` on a group row.
    pub row: Option<usize>,
    pub id: Option<LineId>,
    /// The rollup node this row paints (every row has one).
    pub node: Option<usize>,
    /// A group row's path, its key in the group expansion; `None` on
    /// every other row (packages expand by `LineId`).
    pub path: Option<Path>,
    pub depth: usize,
    /// Tree-column tag: a package's template token (`CS`, `CUSTOM`), empty on a
    /// line or leg.
    pub tag: SharedString,
    /// Tree-column text: a group's value as its column spells it (`—`
    /// for NULL); a package's summary (`package_summary`); a leg's or a
    /// bare line's full one-line shorthand (`-2 SPX Z26 5000 C`).
    pub text: SharedString,
    /// Tree-column note: a package's leg count (`· 1 leg`, `· 2 legs`;
    /// `· 1 of 2 legs` when fewer sit under its node, split or partly
    /// hidden), empty on a group, line or leg.
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

/// A package's legs' distinct underlyings, rendered expiries and rendered
/// strikes, each in leg order. The one collection both the painted
/// summary and the fallback find key read, so `/` finds what column 0
/// paints.
struct LegParts {
    unds: Vec<String>,
    exps: Vec<String>,
    strikes: Vec<String>,
}

fn leg_parts(sheet: &Sheet, legs: impl IntoIterator<Item = usize>) -> LegParts {
    let mut unds: Vec<String> = Vec::new();
    let mut exps: Vec<String> = Vec::new();
    let mut strikes: Vec<String> = Vec::new();
    for leg in legs {
        if let Some(i) = sheet.instrument(leg) {
            let u = i.underlying().to_string();
            if !unds.contains(&u) {
                unds.push(u);
            }
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
    LegParts {
        unds,
        exps,
        strikes,
    }
}

/// `parts` joined with spaces, empty parts dropped.
fn join_parts(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A package's search key: its template form while the legs still match
/// the table (the grammar round-trips it), else its template token with
/// its legs' distinct underlyings, then the painted summary (expiries,
/// then strikes), so a custom package's painted text is findable.
///
/// `shown`: a partly hidden package's shown legs. Its key is then always
/// the token-and-parts form over those legs alone, as its painted summary
/// is: the template form reads every leg, so `/` would find the row by a
/// hidden leg column 0 does not paint.
fn package_search(sheet: &Sheet, row: usize, shown: Option<&[usize]>) -> String {
    let text = sheet.shorthand(row);
    if shown.is_none() && !text.is_empty() && !text.contains('\n') {
        return text;
    }
    let RowKind::Package { template } = sheet.kind(row) else {
        return text;
    };
    let p = match shown {
        Some(legs) => leg_parts(sheet, legs.iter().copied()),
        None => leg_parts(sheet, sheet.children(row)),
    };
    join_parts(&[
        template.token().to_string(),
        p.unds.join("/"),
        p.exps.join("/"),
        p.strikes.join("/"),
    ])
}

/// A package's tree-column summary: its legs' distinct expiries, then
/// their distinct strikes, each in leg order joined with `/`
/// (`Z26 4800/5200`). One line whatever form the package's own
/// shorthand takes; empty when no leg carries an instrument.
pub(crate) fn package_summary(sheet: &Sheet, row: usize) -> String {
    legs_summary(sheet, sheet.children(row))
}

/// [`package_summary`] over some of a package's legs.
fn legs_summary(sheet: &Sheet, legs: impl IntoIterator<Item = usize>) -> String {
    let p = leg_parts(sheet, legs);
    join_parts(&[p.exps.join("/"), p.strikes.join("/")])
}

/// `shown` of `total` legs: `· 2 legs`, or `· 1 of 2 legs` when the scope
/// hides some.
fn leg_note(shown: usize, total: usize) -> String {
    match (shown, total) {
        (1, 1) => "· 1 leg".to_string(),
        (n, t) if n == t => format!("· {n} legs"),
        (n, t) => format!("· {n} of {t} legs"),
    }
}

impl GridModel {
    /// Flatten `rollup` into painted rows: a group row, then (while
    /// `groups` has its path open) its children; a package row, then
    /// (while `packages` has it open) its legs under this node; a bare
    /// line, or a leg standing alone under `instrument_ref`, as a line.
    /// Hidden lines never reached the rollup. A package holding fewer
    /// legs under its node than it has (split across nodes, or partly
    /// hidden by the scope) paints those legs' aggregate
    /// (`subset_cell_text`), summary and `· n of M legs` note.
    ///
    /// An empty chain's rollup is the flat sheet, so this is the one
    /// build the tile makes whether or not a grouping is in force.
    pub fn build(
        sheet: &Sheet,
        rollup: &Rollup,
        groups: &GroupExpansion,
        packages: &Expansion,
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
        let mut f = Flatten {
            sheet,
            rollup,
            groups,
            packages,
            plan,
            clock,
            readings: Vec::new(),
            grouped: Vec::new(),
            rows: Vec::with_capacity(rollup.nodes.len()),
        };
        for &root in &rollup.roots {
            f.node(root);
        }
        GridModel {
            columns,
            rows: f.rows,
        }
    }

    pub fn grid_row_of(&self, id: LineId) -> Option<usize> {
        self.rows.iter().position(|r| r.id == Some(id))
    }
}

/// A group row's tree label ([`Flatten::label`]), which is also its
/// find key.
fn group_label(
    sheet: &Sheet,
    plan: &ColumnPlan,
    clock: Clock,
    column: &str,
    first: Option<usize>,
    raw: &SharedString,
) -> SharedString {
    let (Some(def), Some(leg)) = (crate::core::columns::column(column), first) else {
        return raw.clone();
    };
    let format = plan
        .columns
        .iter()
        .find(|c| c.def.name == def.name)
        .map_or(&def.default_format, |c| &c.format);
    let t = leg_reading(sheet, leg, def, format, clock);
    if t.text.is_empty() {
        raw.clone()
    } else {
        t.text.into()
    }
}

/// Find's targets: every row [`GridModel::build`] would paint with every
/// group open and the packages as `packages` has them, in that order
/// (the rollup's preorder), as `(node, find key)`. The keys are the
/// painted rows' `search`: a group's label, a package's
/// `package_search` (a closed package is found by its legs), a line's
/// shorthand. No cells are formatted.
pub fn find_targets(
    sheet: &Sheet,
    rollup: &Rollup,
    packages: &Expansion,
    plan: &ColumnPlan,
    clock: Clock,
) -> Vec<(usize, SharedString)> {
    let mut out = Vec::with_capacity(rollup.nodes.len());
    let mut stack: Vec<usize> = rollup.roots.iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        let node = &rollup.nodes[id];
        let (key, descend) = match &node.kind {
            NodeKind::Group {
                column,
                value,
                label,
                ..
            } => {
                let key = match value {
                    None => SharedString::new_static(NULL_LABEL),
                    Some(_) => {
                        let first = legs_under(rollup, id).first().copied();
                        group_label(sheet, plan, clock, column, first, label)
                    }
                };
                (key, true)
            }
            NodeKind::Package {
                row, legs, partial, ..
            } => (
                package_search(sheet, *row, partial.then_some(legs.as_slice())).into(),
                packages.is_open(sheet.id(*row)),
            ),
            NodeKind::Leaf { row } => (sheet.shorthand(*row).into(), false),
        };
        out.push((id, key));
        if descend {
            stack.extend(node.children.iter().rev());
        }
    }
    out
}

/// The walk behind [`GridModel::build`].
struct Flatten<'a> {
    sheet: &'a Sheet,
    rollup: &'a Rollup,
    groups: &'a GroupExpansion,
    packages: &'a Expansion,
    plan: &'a ColumnPlan,
    clock: Clock,
    /// Each line's `leg_reading` per plan column, by sheet row, filled the
    /// first time a painted group row reads the line: nested groups read
    /// every leg once per level, so each is formatted once.
    readings: Vec<Option<Box<[CellText]>>>,
    /// The enclosing groups' columns and the cell each shows, root first:
    /// a group row's grouped columns (its own and its ancestors') show
    /// the node's value rather than a unanimity every leg already agrees
    /// on by construction.
    grouped: Vec<(&'a str, CellText)>,
    rows: Vec<GridRow>,
}

impl<'a> Flatten<'a> {
    fn node(&mut self, id: usize) {
        let rollup = self.rollup;
        let node = &rollup.nodes[id];
        match &node.kind {
            NodeKind::Group {
                column,
                value,
                label,
                ..
            } => {
                let open = self.groups.is_open(&node.path);
                let legs = legs_under(rollup, id);
                let text = match value {
                    None => SharedString::new_static(NULL_LABEL),
                    Some(_) => self.label(column, legs.first().copied(), label),
                };
                let shown = match value {
                    None => CellText {
                        text: String::new(),
                        state: CellState::Blank,
                        sign: None,
                    },
                    Some(_) => CellText {
                        text: text.to_string(),
                        state: CellState::Own,
                        sign: None,
                    },
                };
                self.grouped.push((column.as_str(), shown));
                let cells = self.group_cells(&legs);
                self.rows.push(GridRow {
                    kind: GridRowKind::Group {
                        open,
                        depth: node.depth,
                    },
                    row: None,
                    id: None,
                    node: Some(id),
                    path: Some(node.path.clone()),
                    depth: node.depth,
                    tag: SharedString::default(),
                    search: text.clone(),
                    text,
                    note: SharedString::default(),
                    cells,
                });
                if open {
                    for &child in &node.children {
                        self.node(child);
                    }
                }
                self.grouped.pop();
            }
            NodeKind::Package {
                row,
                legs,
                split,
                partial,
            } => {
                self.package(id, *row, legs, *split, *partial);
                if self.packages.is_open(self.sheet.id(*row)) {
                    let last = node.children.len().saturating_sub(1);
                    for (i, &child) in node.children.iter().enumerate() {
                        if let NodeKind::Leaf { row } = rollup.nodes[child].kind {
                            self.line(child, row, GridRowKind::Leg { last: i == last });
                        }
                    }
                }
            }
            // A bare line, or a leg standing alone under
            // `instrument_ref`: no package row above it, so no connector.
            NodeKind::Leaf { row } => self.line(id, *row, GridRowKind::Line),
        }
    }

    /// A group's tree label: its value as the grouped column's cells spell
    /// it (a strike group reads as the strike column does; an expiry
    /// `Z26`, not the ISO date it groups by), read from its first leg with
    /// the column's planned format (its default when the view does not
    /// show it). A column the cell leaves blank (`status` on a fresh line)
    /// and a derived dimension, which has no cells, read the rollup's
    /// label.
    fn label(&self, column: &str, first: Option<usize>, raw: &SharedString) -> SharedString {
        group_label(self.sheet, self.plan, self.clock, column, first, raw)
    }

    fn group_cells(&mut self, legs: &[usize]) -> Vec<GridCell> {
        for &leg in legs {
            self.read(leg);
        }
        let folded = self.sheet.fold_legs(legs.iter().copied());
        let readings = &self.readings;
        self.plan
            .columns
            .iter()
            .enumerate()
            .map(|(col, c)| {
                let t = match self
                    .grouped
                    .iter()
                    .rev()
                    .find(|(name, _)| *name == c.def.name)
                {
                    Some((_, shown)) => shown.clone(),
                    None => group_cell_text(
                        c.def,
                        &c.format,
                        legs.len(),
                        &folded,
                        legs.iter().filter_map(|&leg| {
                            readings
                                .get(leg)
                                .and_then(Option::as_ref)
                                .map(|cells| &cells[col])
                        }),
                        self.clock,
                    ),
                };
                GridCell {
                    text: t.text.into(),
                    state: t.state,
                    sign: t.sign,
                }
            })
            .collect()
    }

    /// Fill `leg`'s readings for every plan column, once.
    fn read(&mut self, leg: usize) {
        if self.readings.len() <= leg {
            self.readings
                .resize_with(self.sheet.len().max(leg + 1), || None);
        }
        if self.readings[leg].is_none() {
            self.readings[leg] = Some(
                self.plan
                    .columns
                    .iter()
                    .map(|c| leg_reading(self.sheet, leg, c.def, &c.format, self.clock))
                    .collect(),
            );
        }
    }

    fn package(&mut self, node: usize, r: usize, legs: &[usize], split: bool, partial: bool) {
        let sheet = self.sheet;
        let RowKind::Package { template } = sheet.kind(r) else {
            unreachable!("a package node names a package row");
        };
        // Fewer legs here than the package has: this node's legs and
        // their fold, computed once for every cell of the row.
        let subset = partial.then(|| (legs, sheet.fold_legs(legs.iter().copied())));
        let total = sheet.children(r).len();
        let summary = match partial {
            true => legs_summary(sheet, legs.iter().copied()),
            false => package_summary(sheet, r),
        };
        let cells = self
            .plan
            .columns
            .iter()
            .map(|c| {
                let t = match &subset {
                    Some((legs, folded)) => {
                        subset_cell_text(sheet, r, legs, folded, c.def, &c.format, self.clock)
                    }
                    None => cell_text(sheet, r, c.def, &c.format, self.clock),
                };
                GridCell {
                    text: t.text.into(),
                    state: t.state,
                    sign: t.sign,
                }
            })
            .collect();
        self.rows.push(GridRow {
            kind: GridRowKind::Package {
                open: self.packages.is_open(sheet.id(r)),
                partial,
                split,
            },
            row: Some(r),
            id: Some(sheet.id(r)),
            node: Some(node),
            path: None,
            depth: self.rollup.nodes[node].depth,
            tag: SharedString::new_static(template.token()),
            text: summary.into(),
            note: leg_note(if partial { legs.len() } else { total }, total).into(),
            search: package_search(sheet, r, partial.then_some(legs)).into(),
            cells,
        });
    }

    fn line(&mut self, node: usize, r: usize, kind: GridRowKind) {
        let sheet = self.sheet;
        // One shorthand, shared by the painted text and the find key.
        let s = SharedString::from(sheet.shorthand(r));
        let cells = self
            .plan
            .columns
            .iter()
            .map(|c| {
                let t = cell_text(sheet, r, c.def, &c.format, self.clock);
                GridCell {
                    text: t.text.into(),
                    state: t.state,
                    sign: t.sign,
                }
            })
            .collect();
        self.rows.push(GridRow {
            kind,
            row: Some(r),
            id: Some(sheet.id(r)),
            node: Some(node),
            path: None,
            depth: self.rollup.nodes[node].depth,
            tag: SharedString::default(),
            text: s.clone(),
            note: SharedString::default(),
            search: s,
            cells,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rollup::{self, EffectiveChain, effective_chain};
    use crate::core::sheet::tests::{at, callspread, line, push, result, spx};
    use crate::core::visibility::Visibility;
    use crate::core::{Expansion, Sheet, Views};
    use geode_core::clock::Clock;
    use geode_core::dimensions::DerivedDimensions;
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
        build_vis(s, e, &Visibility::all(s))
    }

    /// The flat sheet (an empty chain) under `v`: what the tile paints
    /// with no grouping.
    fn build_vis(s: &Sheet, e: &Expansion, v: &Visibility) -> GridModel {
        let dims = DerivedDimensions::default();
        let r = rollup::build(s, v, &EffectiveChain::default(), &dims, Clock::utc());
        GridModel::build(s, &r, &GroupExpansion::default(), e, &plan(), Clock::utc())
    }

    #[test]
    fn rows_follow_the_expansion_and_carry_depth_ids_tags_and_search_keys() {
        let s = sheet();
        let closed = build(&s, &Expansion::default());
        assert_eq!(closed.rows.len(), 3);
        assert_eq!(
            closed.rows[1].kind,
            GridRowKind::Package {
                open: false,
                partial: false,
                split: false,
            }
        );
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
        assert_eq!(
            open.rows[1].kind,
            GridRowKind::Package {
                open: true,
                partial: false,
                split: false,
            }
        );
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
                GridRowKind::Package {
                    open: true,
                    partial: false,
                    split: false,
                },
                GridRowKind::Leg { last: false },
                GridRowKind::Leg { last: true },
                GridRowKind::Line,
                GridRowKind::Package {
                    open: true,
                    partial: false,
                    split: false,
                },
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
    fn a_list_form_package_finds_its_painted_summary() {
        // A custom package's find key falls back from its list-form
        // shorthand; `/` must still match the summary column 0 paints.
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
        assert_eq!(m.rows[0].text.as_ref(), "Z26 5000/4000");
        assert!(
            m.rows[0].search.contains(m.rows[0].text.as_ref()),
            "{:?} does not find {:?}",
            m.rows[0].search,
            m.rows[0].text
        );
    }

    #[test]
    fn a_package_summary_names_a_repeated_strike_once() {
        // A straddle shape: a call and a put at one strike and expiry.
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![line(spx(5000.0, OptionKind::Put), 1)]);
        s.apply(crate::core::Edit::Group {
            first: 0,
            count: 2,
            template: crate::core::Template::CUSTOM,
            id: None,
        })
        .unwrap();
        let m = build(&s, &Expansion::default());
        assert_eq!(m.rows[0].text.as_ref(), "Z26 5000");
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

    /// `strike >= 5000` over [A 5000 C, P(4800 C, -1 × 5200 C), B 4000 P]:
    /// A and the 5200 leg show.
    fn scoped(s: &Sheet) -> Visibility {
        let scope = geode_core::scope::Scope {
            expression: Some(geode_core::scope::parse_expr("strike >= 5000").unwrap()),
            ..Default::default()
        };
        crate::core::visibility::apply_scope(s, &scope, &Default::default(), Clock::utc()).unwrap()
    }

    fn col(name: &str) -> usize {
        plan()
            .columns
            .iter()
            .position(|c| c.def.name == name)
            .unwrap()
    }

    #[test]
    fn a_partly_hidden_package_paints_its_shown_legs_aggregate_and_note() {
        let mut s = sheet();
        let answers = vec![
            (s.id(0), s.revision(0), Ok(result(7.0))),
            (s.id(2), s.revision(2), Ok(result(3.0))),
            (s.id(3), s.revision(3), Ok(result(2.0))),
            (s.id(4), s.revision(4), Ok(result(1.0))),
        ];
        s.deliver_all(answers, at(0));
        let mut e = Expansion::default();
        e.set(s.id(1), true);
        let v = scoped(&s);
        let m = build_vis(&s, &e, &v);
        let p = &m.rows[1];
        assert_eq!(p.row, Some(1));
        assert_eq!(
            p.kind,
            GridRowKind::Package {
                open: true,
                partial: true,
                split: false,
            }
        );
        assert_eq!(p.note.as_ref(), "· 1 of 2 legs");
        assert_eq!(p.text.as_ref(), "Z26 5200", "the shown legs' summary");
        assert_eq!(p.cells[col("strike")].text.as_ref(), "5200");
        assert_eq!(p.cells[col("qty")].text.as_ref(), "-1", "the leg's own qty");
        assert_eq!(
            p.cells[col("npv")].text.as_ref(),
            "-2.00",
            "qty × price of the shown leg, not the package's -1.00 fold"
        );
        assert_eq!(p.cells[col("npv")].sign, Some(Sign::Negative));
        assert_eq!(m.rows[2].row, Some(3));
        assert_eq!(
            m.rows[2].kind,
            GridRowKind::Leg { last: true },
            "the last shown leg takes the corner"
        );
        // Unscoped, the same package paints its whole fold.
        let all = build(&s, &e);
        assert_eq!(all.rows[1].cells[col("npv")].text.as_ref(), "1.00");
        assert_eq!(all.rows[1].note.as_ref(), "· 2 legs");
    }

    /// A partly hidden package's find key reads its shown legs, as its
    /// summary does: `/` must never find a row by a hidden leg's strike
    /// that column 0 does not paint.
    #[test]
    fn a_partly_hidden_packages_find_key_reads_only_its_shown_legs() {
        let s = sheet();
        let v = scoped(&s);
        let m = build_vis(&s, &Expansion::default(), &v);
        let p = &m.rows[1];
        assert_eq!(p.row, Some(1));
        assert!(
            !p.search.contains("4800"),
            "the hidden leg's strike is not findable: {}",
            p.search
        );
        assert!(
            p.search.contains("5200"),
            "the shown leg's is: {}",
            p.search
        );
        assert!(
            p.search.contains(p.text.as_ref()),
            "the painted summary is findable"
        );
        // Unscoped, the key is the template form again, over every leg.
        let all = build(&s, &Expansion::default());
        assert!(all.rows[1].search.contains("4800"));
    }

    #[test]
    fn hidden_rows_are_not_in_the_model() {
        let s = sheet();
        let mut e = Expansion::default();
        e.set(s.id(1), true);
        let v = scoped(&s);
        assert_eq!(v.hidden, 2, "the 4800 leg and B");
        let m = build_vis(&s, &e, &v);
        let rows: Vec<Option<usize>> = m.rows.iter().map(|r| r.row).collect();
        assert_eq!(rows, vec![Some(0), Some(1), Some(3)]);
        assert_eq!(m.grid_row_of(s.id(4)), None);
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
        assert_eq!(m.rows[0].search.as_ref(), "CUSTOM SPX Z26 5000/4000");
    }

    // ---- grouping -------------------------------------------------------

    /// Rows: 0 SPX Z26 4000 P, 1 CS, 2 and 3 its legs (4800 C, -1 × 5200
    /// C), 4 NDX Z26 5000 C, 5 CAL, 6 and 7 its legs (H27 C, Z26 C), 8 SPX
    /// H27 4000 P — the rollup's own fixture.
    fn fixture() -> Sheet {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            [
                "SPX Z26 4000 P",
                "SPX Z26 4800/5200 CS",
                "NDX Z26 5000 C",
                "SPX Z26/H27 5000 CAL",
                "SPX H27 4000 P",
            ]
            .iter()
            .map(|l| crate::core::shorthand::parse_builtin(l).unwrap())
            .collect(),
        );
        assert_eq!(s.len(), 9);
        s
    }

    /// Every line priced at `row + 1` (USD).
    fn priced() -> Sheet {
        let mut s = fixture();
        let answers: Vec<_> = (0..s.len())
            .filter(|&r| s.is_line(r))
            .map(|r| (s.id(r), s.revision(r), Ok(result(r as f64 + 1.0))))
            .collect();
        s.deliver_all(answers, at(0));
        s
    }

    fn grouped_with(
        s: &Sheet,
        levels: &[&str],
        dims: &DerivedDimensions,
        groups: &GroupExpansion,
        packages: &Expansion,
        plan: &ColumnPlan,
    ) -> (rollup::Rollup, GridModel) {
        let levels: Vec<String> = levels.iter().map(|l| l.to_string()).collect();
        let chain = effective_chain(&levels, dims);
        let r = rollup::build(s, &Visibility::all(s), &chain, dims, Clock::utc());
        let m = GridModel::build(s, &r, groups, packages, plan, Clock::utc());
        (r, m)
    }

    fn grouped(
        s: &Sheet,
        levels: &[&str],
        groups: &GroupExpansion,
        packages: &Expansion,
    ) -> (rollup::Rollup, GridModel) {
        let dims = DerivedDimensions::default();
        grouped_with(s, levels, &dims, groups, packages, &plan())
    }

    fn all_open() -> GroupExpansion {
        let mut g = GroupExpansion::default();
        g.open_all();
        g
    }

    /// One line per grid row, indented by depth: a group `▾`/`▸` and its
    /// text, a package `P<row>` and its note (`split` when split), a leg
    /// its connector and `L<row>`, a line `L<row>`.
    fn describe(m: &GridModel) -> Vec<String> {
        m.rows
            .iter()
            .map(|r| {
                let pad = "  ".repeat(r.depth);
                match r.kind {
                    GridRowKind::Group { open, .. } => {
                        format!("{pad}{} {}", if open { "▾" } else { "▸" }, r.text)
                    }
                    GridRowKind::Package { split, .. } => format!(
                        "{pad}P{} {}{}",
                        r.row.unwrap(),
                        r.note,
                        if split { " split" } else { "" }
                    ),
                    GridRowKind::Leg { last } => format!(
                        "{pad}{} L{}",
                        crate::delegate::connector(last),
                        r.row.unwrap()
                    ),
                    GridRowKind::Line => format!("{pad}L{}", r.row.unwrap()),
                }
            })
            .collect()
    }

    fn group_row<'m>(m: &'m GridModel, label: &str) -> &'m GridRow {
        m.rows
            .iter()
            .find(|r| matches!(r.kind, GridRowKind::Group { .. }) && r.text.as_ref() == label)
            .unwrap_or_else(|| panic!("no group row {label}: {:?}", describe(m)))
    }

    fn cell<'m>(r: &'m GridRow, name: &str) -> (&'m str, CellState) {
        let c = &r.cells[col(name)];
        (c.text.as_ref(), c.state)
    }

    /// `names`' columns at their vocabulary defaults.
    fn plan_of(names: &[&str]) -> ColumnPlan {
        ColumnPlan {
            columns: names
                .iter()
                .map(|n| {
                    let def = crate::core::columns::column(n).unwrap();
                    crate::core::views::PlannedColumn {
                        def,
                        label: def.label.to_string(),
                        width: def.default_width,
                        format: def.default_format.clone(),
                    }
                })
                .collect(),
        }
    }

    /// `[underlying_ref]`: group rows in value order, each group's lines
    /// and packages one deeper in sheet order; a package's legs one deeper
    /// again, only while it is open; a closed group hides everything under
    /// it. Group rows carry their rollup node and path, and no sheet row.
    #[test]
    fn grouping_rows_flatten_the_rollup_under_both_expansions() {
        let s = fixture();
        let (r, m) = grouped(&s, &["underlying_ref"], &all_open(), &Expansion::default());
        assert_eq!(
            describe(&m),
            [
                "▾ NDX",
                "  L4",
                "▾ SPX",
                "  L0",
                "  P1 · 2 legs",
                "  P5 · 2 legs",
                "  L8"
            ]
        );
        let spx = group_row(&m, "SPX");
        assert_eq!(
            spx.kind,
            GridRowKind::Group {
                open: true,
                depth: 0
            }
        );
        assert_eq!((spx.row, spx.id), (None, None), "a group is no sheet row");
        assert_eq!(spx.path, Some(vec![Some("SPX".to_string())]));
        let node = spx.node.expect("a group row names its node");
        assert!(
            matches!(&r.nodes[node].kind, rollup::NodeKind::Group { label, .. } if label == "SPX")
        );
        assert_eq!((spx.tag.as_ref(), spx.note.as_ref()), ("", ""));
        assert_eq!(spx.search.as_ref(), "SPX", "find matches the label");
        let p1 = &m.rows[4];
        assert_eq!(p1.path, None, "only group rows carry a path");
        assert!(
            matches!(
                r.nodes[p1.node.unwrap()].kind,
                rollup::NodeKind::Package { row: 1, .. }
            ),
            "every row names its node"
        );
        assert_eq!(m.grid_row_of(s.id(1)), Some(4));

        let mut packages = Expansion::default();
        packages.set(s.id(1), true);
        let (_, m) = grouped(&s, &["underlying_ref"], &all_open(), &packages);
        assert_eq!(
            describe(&m),
            [
                "▾ NDX",
                "  L4",
                "▾ SPX",
                "  L0",
                "  P1 · 2 legs",
                "    ├ L2",
                "    └ L3",
                "  P5 · 2 legs",
                "  L8"
            ]
        );

        let mut groups = GroupExpansion::default();
        groups.open(vec![Some("NDX".to_string())]);
        let (_, m) = grouped(&s, &["underlying_ref"], &groups, &packages);
        assert_eq!(
            describe(&m),
            ["▾ NDX", "  L4", "▸ SPX"],
            "a closed group hides its rows"
        );
        assert_eq!(
            group_row(&m, "SPX").kind,
            GridRowKind::Group {
                open: false,
                depth: 0
            }
        );

        // Two levels: the inner group one deeper, its lines deeper again.
        let (_, m) = grouped(
            &s,
            &["underlying_ref", "option_type"],
            &all_open(),
            &Expansion::default(),
        );
        assert_eq!(
            describe(&m),
            [
                "▾ NDX",
                "  ▾ C",
                "    L4",
                "▾ SPX",
                "  ▾ C",
                "    P1 · 2 legs",
                "    P5 · 2 legs",
                "  ▾ P",
                "    L0",
                "    L8"
            ]
        );
        let c = m
            .rows
            .iter()
            .filter(|r| r.text.as_ref() == "C")
            .nth(1)
            .unwrap();
        assert_eq!(
            c.kind,
            GridRowKind::Group {
                open: true,
                depth: 1
            }
        );
        assert_eq!(
            c.path,
            Some(vec![Some("SPX".to_string()), Some("C".to_string())])
        );
    }

    /// A group row: measures summed over its legs × qty; an ungrouped
    /// dimension its value where unanimous, `mixed` (muted) where not,
    /// blank where none; the grouped column its node's value; qty the leg
    /// count; status the legs' fold.
    #[test]
    fn a_group_row_sums_its_legs_and_reads_unanimity() {
        let s = priced();
        let (_, m) = grouped(&s, &["underlying_ref"], &all_open(), &Expansion::default());
        let spx = group_row(&m, "SPX");
        let npv: f64 = [0usize, 2, 3, 6, 7, 8]
            .iter()
            .map(|&r| s.qty(r) as f64 * (r as f64 + 1.0))
            .sum();
        assert_eq!(
            cell(spx, "npv"),
            (format!("{npv:.2}").as_str(), CellState::Own)
        );
        assert!(
            spx.cells[col("npv")].sign.is_some(),
            "a sum carries its sign"
        );
        assert_eq!(cell(spx, "qty"), ("6", CellState::Own), "the leg count");
        assert_eq!(cell(spx, "underlying_ref"), ("SPX", CellState::Own));
        assert_eq!(cell(spx, "strike"), ("mixed", CellState::Mixed));
        assert_eq!(cell(spx, "expiry"), ("mixed", CellState::Mixed));
        assert_eq!(cell(spx, "option_type"), ("mixed", CellState::Mixed));
        assert_eq!(cell(spx, "currency"), ("USD", CellState::Own));
        assert_eq!(
            cell(spx, "spot_shift"),
            ("", CellState::Blank),
            "no leg sets one"
        );
        assert_eq!(cell(spx, "status"), ("", CellState::Own), "every leg fresh");
        let ndx = group_row(&m, "NDX");
        assert_eq!(
            cell(ndx, "strike"),
            ("5000", CellState::Own),
            "one line: unanimous"
        );
        assert_eq!(cell(ndx, "option_type"), ("C", CellState::Own));
        assert_eq!(cell(ndx, "qty"), ("1", CellState::Own));
        assert_eq!(cell(ndx, "npv"), ("5.00", CellState::Own));

        // A leg repricing: the group's status and sums read the fold.
        let mut s = priced();
        s.touch(0);
        let (_, m) = grouped(&s, &["underlying_ref"], &all_open(), &Expansion::default());
        let spx = group_row(&m, "SPX");
        assert_eq!(cell(spx, "status"), ("pricing…", CellState::Stale));
        assert_eq!(cell(spx, "npv").1, CellState::Stale, "the sum is muted");
        assert_eq!(cell(group_row(&m, "NDX"), "npv").1, CellState::Own);
    }

    /// A NULL beside a value is `mixed`, not the value: showing it would
    /// claim it for the legs that have none (the blotter's rule).
    #[test]
    fn a_blank_beside_a_value_is_mixed() {
        let mut s = fixture();
        push(
            &mut s,
            vec![crate::core::shorthand::parse_builtin("SPX Z26 4000 P DO 3500").unwrap()],
        );
        let plan = plan_of(&["barrier"]);
        let dims = DerivedDimensions::default();
        let (_, m) = grouped_with(
            &s,
            &["underlying_ref"],
            &dims,
            &all_open(),
            &Expansion::default(),
            &plan,
        );
        let spx = group_row(&m, "SPX");
        assert_eq!(
            (spx.cells[0].text.as_ref(), spx.cells[0].state),
            ("mixed", CellState::Mixed)
        );
        let ndx = group_row(&m, "NDX");
        assert_eq!(
            (ndx.cells[0].text.as_ref(), ndx.cells[0].state),
            ("", CellState::Blank),
            "no leg reads one: blank"
        );
    }

    /// Unlike currencies under one group: the local sum would add unlike
    /// units, so it paints `—` (muted); the `_usd` twin still sums; the
    /// currency reads `mixed`.
    #[test]
    fn a_mixed_currency_group_paints_no_local_sum() {
        let mut s = priced();
        let mut eur = result(1.0);
        eur.currency = geode_core::pricing::Currency::parse("EUR").unwrap();
        s.deliver_all(vec![(s.id(0), s.revision(0), Ok(eur))], at(1));
        let plan = plan_of(&["npv", "npv_usd", "currency"]);
        let dims = DerivedDimensions::default();
        let (_, m) = grouped_with(
            &s,
            &["underlying_ref"],
            &dims,
            &all_open(),
            &Expansion::default(),
            &plan,
        );
        let spx = group_row(&m, "SPX");
        let at = |i: usize| (spx.cells[i].text.as_ref(), spx.cells[i].state);
        assert_eq!(at(0), ("—", CellState::Stale));
        let usd: f64 = [0usize, 2, 3, 6, 7, 8]
            .iter()
            .map(|&r| s.qty(r) as f64 * s.result(r).unwrap().get(Measure::Npv, true))
            .sum();
        assert_eq!(at(1), (format!("{usd:.2}").as_str(), CellState::Own));
        assert_eq!(at(2), ("mixed", CellState::Mixed));
        let ndx = group_row(&m, "NDX");
        assert_eq!(
            (ndx.cells[0].text.as_ref(), ndx.cells[0].state),
            ("5.00", CellState::Own),
            "a one-currency group sums locally"
        );
    }

    /// `[expiry]` splits the calendar: it sits under both dates with that
    /// date's leg, `· 1 of 2 legs`, `split`, its cells over that leg only.
    #[test]
    fn a_split_package_reads_n_of_m_legs_under_each_node() {
        let s = priced();
        let mut packages = Expansion::default();
        packages.set(s.id(5), true);
        let (_, m) = grouped(&s, &["expiry"], &all_open(), &packages);
        assert_eq!(
            describe(&m),
            [
                "▾ Z26",
                "  L0",
                "  P1 · 2 legs",
                "  L4",
                "  P5 · 1 of 2 legs split",
                "    └ L7",
                "▾ H27",
                "  P5 · 1 of 2 legs split",
                "    └ L6",
                "  L8"
            ]
        );
        let cal: Vec<&GridRow> = m.rows.iter().filter(|r| r.row == Some(5)).collect();
        assert_eq!(
            cal[0].kind,
            GridRowKind::Package {
                open: true,
                partial: true,
                split: true
            }
        );
        assert_eq!(cal[0].text.as_ref(), "Z26 5000", "the node's legs' summary");
        assert_eq!(cal[1].text.as_ref(), "H27 5000");
        for (r, leg) in [(cal[0], 7usize), (cal[1], 6)] {
            let npv = s.qty(leg) as f64 * (leg as f64 + 1.0);
            assert_eq!(
                cell(r, "npv").0,
                format!("{npv:.2}"),
                "the node's leg alone"
            );
        }
        assert_eq!(m.grid_row_of(s.id(5)), Some(4), "the first occurrence");
    }

    /// The tree label reads as the grouped column's cells do (an expiry
    /// group `Z26`, not the ISO date it groups by) and NULL as `—`; the
    /// grouped column's cell shows the same value (blank for NULL). A
    /// derived dimension, which has no column, reads its label.
    #[test]
    fn a_group_label_reads_as_its_column_and_null_as_a_dash() {
        let s = fixture();
        let (_, m) = grouped(&s, &["expiry"], &all_open(), &Expansion::default());
        let z26 = &m.rows[0];
        assert_eq!(z26.text.as_ref(), "Z26");
        assert_eq!(
            z26.path,
            Some(vec![Some("2026-12-18".to_string())]),
            "the path stays raw"
        );
        assert_eq!(cell(z26, "expiry"), ("Z26", CellState::Own));

        let barrier = plan_of(&["barrier"]);
        let dims = DerivedDimensions::default();
        let (_, m) = grouped_with(
            &s,
            &["barrier"],
            &dims,
            &all_open(),
            &Expansion::default(),
            &barrier,
        );
        assert_eq!(m.rows[0].text.as_ref(), "—", "no line reads a barrier");
        assert_eq!(m.rows[0].path, Some(vec![None]));
        assert_eq!(
            (m.rows[0].cells[0].text.as_ref(), m.rows[0].cells[0].state),
            ("", CellState::Blank)
        );

        let dims = {
            let doc = geode_core::config::merge_docs(
                "dimensions",
                &[geode_core::config::LayerDoc::builtin(
                    "dimensions",
                    "[index]\nfrom = \"underlying_ref\"\n[index.values]\nbroad = [\"SPX\"]\n",
                )
                .unwrap()],
            );
            DerivedDimensions::from_doc(&doc).0
        };
        let (_, m) = grouped_with(
            &s,
            &["index"],
            &dims,
            &all_open(),
            &Expansion::default(),
            &plan(),
        );
        let labels: Vec<&str> = m
            .rows
            .iter()
            .filter(|r| matches!(r.kind, GridRowKind::Group { .. }))
            .map(|r| r.text.as_ref())
            .collect();
        assert_eq!(labels, ["broad", "—"]);
    }

    /// `instrument_ref` as a level: every leg a line of its own under its
    /// group, with no package row above it.
    #[test]
    fn under_instrument_ref_a_leg_paints_as_a_line() {
        let s = fixture();
        let (_, m) = grouped(
            &s,
            &["underlying_ref", "instrument_ref"],
            &all_open(),
            &Expansion::default(),
        );
        assert_eq!(
            describe(&m),
            [
                "▾ NDX", "  L4", "▾ SPX", "  L0", "  L2", "  L3", "  L6", "  L7", "  L8"
            ]
        );
        assert_eq!(
            m.rows[4].text.as_ref(),
            "SPX Z26 4800 C",
            "the leg's own shorthand"
        );
    }

    /// Find's targets are the rows the grid paints with every group open,
    /// in order, keyed as those rows' `search` — whatever the groups'
    /// own state; a closed package contributes its row alone, an open
    /// one its legs too.
    #[test]
    fn find_targets_are_the_all_open_rows_and_their_search_keys() {
        let s = fixture();
        let mut packages = Expansion::default();
        packages.set(s.id(1), true);
        for levels in [
            &["underlying_ref"][..],
            &["underlying_ref", "expiry"],
            &["expiry"],
        ] {
            let (r, open) = grouped(&s, levels, &all_open(), &packages);
            let got = find_targets(&s, &r, &packages, &plan(), Clock::utc());
            let want: Vec<(usize, SharedString)> = open
                .rows
                .iter()
                .map(|row| (row.node.expect("every row is a node"), row.search.clone()))
                .collect();
            assert_eq!(got, want, "{levels:?}");
        }
    }
}
