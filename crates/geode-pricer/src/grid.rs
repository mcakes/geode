//! The pricer grid's index — every row under the current expansion with its
//! tree-column facts, and `row_of` — and the pass that formats measure cells
//! for the rows a window shows. A group row folds its legs only when filled.
//! Colours live in `Paints`; a theme change never refills.
//!
//! The tile rebuilds the index when sheet data, the scope, the grouping,
//! either expansion, columns, or the display clock changes. The delegate
//! paints the tree column from each row's depth, kind, tag, text and note,
//! all prepared here. A row's shorthand is also its search key.

use crate::core::columns::{
    CellState, CellText, ColumnKind, cell_text, group_cell_text, leg_reading, subset_cell_text,
};
use crate::core::rollup::{NULL_LABEL, NodeKind, Rollup, legs_under};
use crate::core::sheet::{Folded, LineId, RowKind, Sheet};
use crate::core::shorthand::{render_expiry, render_strike};
use crate::core::tree::Expansion;
use crate::core::views::ColumnPlan;
use geode_core::clock::Clock;
use geode_core::colour::{Sign, ValueColors};
use geode_core::expansion::{Expansion as GroupExpansion, Path};
use geode_core::view::Colour;
use gpui::SharedString;
use std::collections::HashMap;
use std::sync::Arc;

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
    /// `last`: the final leg of its package in sheet order. Its drawn
    /// connector line stops at its stub, closing the package; every other
    /// leg's line runs the full row height and its row drops the table's
    /// separator, so the line joins the next leg's.
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

#[derive(Debug, Clone, PartialEq)]
pub struct GridCell {
    pub text: SharedString,
    pub state: CellState,
    /// A measure's sign as formatted; `None` on a text cell.
    pub sign: Option<Sign>,
    /// The color `value_colors` maps this cell's dimension value to,
    /// looked up when the window is filled.
    pub value_color: Option<Arc<str>>,
}

/// One painted row's tree-column facts, as [`Flatten`] pushes them.
#[derive(Debug, Clone)]
struct TreeRow {
    kind: GridRowKind,
    /// The sheet's flat row; `None` on a group row.
    row: Option<usize>,
    id: Option<LineId>,
    /// The rollup node this row paints (every row has one).
    node: Option<usize>,
    /// A group row's path, its key in the group expansion; `None` on
    /// every other row (packages expand by `LineId`).
    path: Option<Path>,
    depth: usize,
    /// Tree-column tag: a package's template token (`CS`, `CUSTOM`), empty on a
    /// line or leg.
    tag: SharedString,
    /// Tree-column text: a group's value as its column spells it (`—`
    /// for NULL); a package's summary (`package_summary`); a leg's or a
    /// bare line's full one-line shorthand (`-2 SPX Z26 5000 C`).
    text: SharedString,
    /// Tree-column note: a package's leg count (`· 1 leg`, `· 2 legs`;
    /// `· 1 of 2 legs` when fewer sit under its node, split or partly
    /// hidden), empty on a group, line or leg.
    note: SharedString,
    /// Find key derived from shorthand. It can match text that does not appear in
    /// the current view's columns.
    search: SharedString,
    /// The enclosing group row, for a group row's grouped-column cells.
    parent: Option<u32>,
    /// A group row's value color (its label is the group's value); `None`
    /// on every other row.
    value_color: Option<Arc<str>>,
}

/// Every painted row's tree-column facts, in parallel arrays, and where
/// each line paints. No measure cell: [`CellPass`] formats those for the
/// rows a window shows.
#[derive(Debug, Clone, Default)]
pub struct GridIndex {
    pub columns: Vec<GridColumn>,
    kind: Vec<GridRowKind>,
    row: Vec<Option<usize>>,
    id: Vec<Option<LineId>>,
    node: Vec<Option<usize>>,
    path: Vec<Option<Path>>,
    depth: Vec<usize>,
    tag: Vec<SharedString>,
    text: Vec<SharedString>,
    note: Vec<SharedString>,
    search: Vec<SharedString>,
    parent: Vec<Option<u32>>,
    value_color: Vec<Option<Arc<str>>>,
    /// Line → (first painted row, how many rows paint it).
    row_of: HashMap<LineId, (u32, u32)>,
}

/// One row's tree-column facts, borrowed from a [`GridIndex`].
#[derive(Debug, Clone, Copy)]
pub struct TreeRef<'a> {
    pub kind: GridRowKind,
    /// The sheet's flat row; `None` on a group row.
    pub row: Option<usize>,
    pub id: Option<LineId>,
    /// The rollup node this row paints.
    pub node: Option<usize>,
    /// A group row's path; `None` on every other row.
    pub path: Option<&'a Path>,
    pub depth: usize,
    pub tag: &'a SharedString,
    pub text: &'a SharedString,
    pub note: &'a SharedString,
    pub search: &'a SharedString,
    /// A group row's value color; `None` on every other row.
    pub value_color: Option<&'a Arc<str>>,
}

#[cfg(test)]
thread_local! {
    static BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Index builds on this thread, for tests that prove a route does not build.
#[cfg(test)]
pub(crate) fn builds() -> usize {
    BUILDS.with(|b| b.get())
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

impl GridIndex {
    fn push(&mut self, r: TreeRow) {
        let g = self.kind.len() as u32;
        if let Some(id) = r.id {
            let e = self.row_of.entry(id).or_insert((g, 0));
            e.1 += 1;
        }
        self.kind.push(r.kind);
        self.row.push(r.row);
        self.id.push(r.id);
        self.node.push(r.node);
        self.path.push(r.path);
        self.depth.push(r.depth);
        self.tag.push(r.tag);
        self.text.push(r.text);
        self.note.push(r.note);
        self.search.push(r.search);
        self.parent.push(r.parent);
        self.value_color.push(r.value_color);
    }

    pub fn len(&self) -> usize {
        self.kind.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kind.is_empty()
    }

    /// Row `g`'s tree-column facts.
    pub fn tree(&self, g: usize) -> Option<TreeRef<'_>> {
        Some(TreeRef {
            kind: *self.kind.get(g)?,
            row: self.row[g],
            id: self.id[g],
            node: self.node[g],
            path: self.path[g].as_ref(),
            depth: self.depth[g],
            tag: &self.tag[g],
            text: &self.text[g],
            note: &self.note[g],
            search: &self.search[g],
            value_color: self.value_color[g].as_ref(),
        })
    }

    /// Every row's tree-column facts, in painted order.
    pub fn trees(&self) -> impl Iterator<Item = TreeRef<'_>> + '_ {
        (0..self.len()).filter_map(|g| self.tree(g))
    }

    pub fn kind(&self, g: usize) -> Option<GridRowKind> {
        self.kind.get(g).copied()
    }

    pub fn sheet_row(&self, g: usize) -> Option<usize> {
        self.row.get(g).copied().flatten()
    }

    pub fn id(&self, g: usize) -> Option<LineId> {
        self.id.get(g).copied().flatten()
    }

    pub fn node(&self, g: usize) -> Option<usize> {
        self.node.get(g).copied().flatten()
    }

    pub fn path(&self, g: usize) -> Option<&Path> {
        self.path.get(g)?.as_ref()
    }

    pub fn text(&self, g: usize) -> Option<&SharedString> {
        self.text.get(g)
    }

    /// The group row enclosing row `g`; `None` at the roots.
    pub fn parent(&self, g: usize) -> Option<usize> {
        self.parent.get(g).copied().flatten().map(|p| p as usize)
    }

    /// The first row painting line `id`.
    pub fn grid_row_of(&self, id: LineId) -> Option<usize> {
        self.row_of.get(&id).map(|&(first, _)| first as usize)
    }

    /// How many rows paint line `id`: one, or more for a split package.
    pub fn paints(&self, id: LineId) -> usize {
        self.row_of.get(&id).map_or(0, |&(_, n)| n as usize)
    }

    /// The rows painting line `id`, in painted order.
    pub fn rows_of(&self, id: LineId) -> impl Iterator<Item = usize> + '_ {
        let (first, n) = self
            .row_of
            .get(&id)
            .map_or((0, 0), |&(f, n)| (f as usize, n as usize));
        (first..self.len())
            .filter(move |&g| self.id[g] == Some(id))
            .take(n)
    }

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
        values: &ValueColors,
    ) -> GridIndex {
        #[cfg(test)]
        BUILDS.with(|b| b.set(b.get() + 1));
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
            values,
            out: GridIndex::default(),
            enclosing: Vec::new(),
        };
        for &root in &rollup.roots {
            f.node(root);
        }
        f.out.columns = columns;
        f.out
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

/// Find's targets: every row [`GridIndex::build`] would paint with every
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

/// The walk behind [`GridIndex::build`].
struct Flatten<'a> {
    sheet: &'a Sheet,
    rollup: &'a Rollup,
    groups: &'a GroupExpansion,
    packages: &'a Expansion,
    plan: &'a ColumnPlan,
    clock: Clock,
    /// The value → color mapping a group row's label is looked up in.
    values: &'a ValueColors,
    out: GridIndex,
    /// The enclosing group rows, root first: each pushed row's `parent`.
    enclosing: Vec<u32>,
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
                // The rollup's `label` is the value's own text; the painted
                // label may be the column's formatted spelling of it.
                let value_color = value
                    .as_ref()
                    .and_then(|_| self.values.get(column, label))
                    .cloned();
                self.out.push(TreeRow {
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
                    parent: self.enclosing.last().copied(),
                    value_color,
                });
                self.enclosing.push((self.out.len() - 1) as u32);
                if open {
                    for &child in &node.children {
                        self.node(child);
                    }
                }
                self.enclosing.pop();
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

    fn package(&mut self, node: usize, r: usize, legs: &[usize], split: bool, partial: bool) {
        let sheet = self.sheet;
        let RowKind::Package { template } = sheet.kind(r) else {
            unreachable!("a package node names a package row");
        };
        let total = sheet.children(r).len();
        let summary = match partial {
            true => legs_summary(sheet, legs.iter().copied()),
            false => package_summary(sheet, r),
        };
        self.out.push(TreeRow {
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
            parent: self.enclosing.last().copied(),
            value_color: None,
        });
    }

    fn line(&mut self, node: usize, r: usize, kind: GridRowKind) {
        let sheet = self.sheet;
        // One shorthand, shared by the painted text and the find key.
        let s = SharedString::from(sheet.shorthand(r));
        self.out.push(TreeRow {
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
            parent: self.enclosing.last().copied(),
            value_color: None,
        });
    }
}

/// What a window fill reads: the sheet, the rollup the index was built
/// from, the plan, the clock, and the value → color mapping.
#[derive(Clone, Copy)]
pub struct FillSource<'a> {
    pub sheet: &'a Sheet,
    pub rollup: &'a Rollup,
    pub plan: &'a ColumnPlan,
    pub clock: Clock,
    pub values: &'a ValueColors,
}

/// A row's fold, prepared once for every cell of the row.
enum RowFold {
    /// A group row: its legs and their fold.
    Group { legs: Vec<usize>, folded: Folded },
    /// A partial package row: its node's legs' fold.
    Subset { folded: Folded },
    /// Every other row reads its own cells.
    Own,
}

/// One fill pass over some rows. A group row folds its legs only when it is
/// filled; each leg's readings are memoised for this pass alone (nested
/// groups read every leg once per level, so each is formatted once).
pub struct CellPass<'a> {
    src: FillSource<'a>,
    index: &'a GridIndex,
    /// Each leg's `leg_reading` per plan column, by sheet row.
    readings: Vec<Option<Box<[CellText]>>>,
    /// The last prepared row and its fold.
    memo: Option<(usize, RowFold)>,
}

impl<'a> CellPass<'a> {
    pub fn new(src: FillSource<'a>, index: &'a GridIndex) -> Self {
        CellPass {
            src,
            index,
            readings: Vec::new(),
            memo: None,
        }
    }

    /// Fill `leg`'s readings for every plan column, once per pass.
    fn read(&mut self, leg: usize) {
        if self.readings.len() <= leg {
            self.readings
                .resize_with(self.src.sheet.len().max(leg + 1), || None);
        }
        if self.readings[leg].is_none() {
            self.readings[leg] = Some(
                self.src
                    .plan
                    .columns
                    .iter()
                    .map(|c| leg_reading(self.src.sheet, leg, c.def, &c.format, self.src.clock))
                    .collect(),
            );
        }
    }

    fn prepare(&mut self, g: usize) {
        if self.memo.as_ref().is_some_and(|(at, _)| *at == g) {
            return;
        }
        let node = self.index.node(g);
        let fold = match node.map(|n| &self.src.rollup.nodes[n].kind) {
            Some(NodeKind::Group { .. }) => {
                let legs = legs_under(self.src.rollup, node.unwrap_or_default());
                for &leg in &legs {
                    self.read(leg);
                }
                let folded = self.src.sheet.fold_legs(legs.iter().copied());
                RowFold::Group { legs, folded }
            }
            Some(NodeKind::Package {
                row: r,
                legs: shown,
                partial: true,
                ..
            }) => {
                // Unused here; bound so the subset-fold mutation's whole-package replacement compiles.
                let _ = r;
                let subset = self.src.sheet.fold_legs(shown.iter().copied());
                RowFold::Subset { folded: subset }
            }
            _ => RowFold::Own,
        };
        self.memo = Some((g, fold));
    }

    /// The cells a group row shows for its own and enclosing groups' columns:
    /// the group's value, not a unanimity its legs agree on by construction.
    fn grouped_shown(&self, g: usize, name: &str) -> Option<CellText> {
        let mut at = Some(g);
        while let Some(a) = at {
            if let Some(NodeKind::Group { column, value, .. }) =
                self.index.node(a).map(|n| &self.src.rollup.nodes[n].kind)
                && column == name
            {
                return Some(match value {
                    None => CellText {
                        text: String::new(),
                        state: CellState::Blank,
                        sign: None,
                    },
                    Some(_) => CellText {
                        text: self
                            .index
                            .text(a)
                            .map(|t| t.to_string())
                            .unwrap_or_default(),
                        state: CellState::Own,
                        sign: None,
                    },
                });
            }
            at = self.index.parent(a);
        }
        None
    }

    /// Row `g`'s cell in plan column `col`, formatted through the column
    /// formatters (`cell_text`, `subset_cell_text`, `group_cell_text`).
    pub fn cell(&mut self, g: usize, col: usize) -> Option<GridCell> {
        let c = self.src.plan.columns.get(col)?;
        self.prepare(g);
        let (sheet, clock) = (self.src.sheet, self.src.clock);
        let t = match &self.memo {
            Some((_, RowFold::Group { legs, folded })) => match self.grouped_shown(g, c.def.name) {
                Some(shown) => shown,
                None => group_cell_text(
                    c.def,
                    &c.format,
                    legs.len(),
                    folded,
                    legs.iter().filter_map(|&leg| {
                        self.readings
                            .get(leg)
                            .and_then(Option::as_ref)
                            .map(|cells| &cells[col])
                    }),
                    clock,
                ),
            },
            Some((_, RowFold::Subset { folded })) => {
                let r = self.index.sheet_row(g)?;
                let node = self.index.node(g)?;
                let NodeKind::Package { legs: shown, .. } = &self.src.rollup.nodes[node].kind
                else {
                    return None;
                };
                subset_cell_text(sheet, r, shown, folded, c.def, &c.format, clock)
            }
            _ => cell_text(sheet, self.index.sheet_row(g)?, c.def, &c.format, clock),
        };
        // A text dimension's cell shows its value; a mapping holds text
        // dimensions only, so a measure or date column finds nothing.
        // `mixed` and blank are not values.
        let value_color = (t.state == CellState::Own)
            .then(|| self.src.values.get(c.def.name, &t.text))
            .flatten()
            .cloned();
        Some(GridCell {
            text: t.text.into(),
            state: t.state,
            sign: t.sign,
            value_color,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rollup::{self, EffectiveChain, effective_chain};
    use crate::core::sheet::tests::{at, callspread, in_usd, line, push, result, spx};
    use crate::core::visibility::Visibility;
    use crate::core::{Expansion, Sheet, Views};
    use geode_core::clock::Clock;
    use geode_core::dimensions::DerivedDimensions;
    use geode_core::pricing::{Measure, OptionKind};
    use geode_tile::grid::WindowCache;

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

    static NO_VALUES: std::sync::LazyLock<ValueColors> =
        std::sync::LazyLock::new(ValueColors::default);

    fn src<'a>(s: &'a Sheet, r: &'a Rollup, plan: &'a ColumnPlan) -> FillSource<'a> {
        FillSource {
            sheet: s,
            rollup: r,
            plan,
            clock: Clock::utc(),
            values: &NO_VALUES,
        }
    }

    /// Row `g`'s cells, formatted on demand by a fresh pass.
    fn cells(s: &Sheet, r: &Rollup, plan: &ColumnPlan, m: &GridIndex, g: usize) -> Vec<GridCell> {
        let mut pass = CellPass::new(src(s, r, plan), m);
        (0..plan.columns.len())
            .map(|c| pass.cell(g, c).expect("a planned column"))
            .collect()
    }

    /// Row `g`'s cell texts under the vanilla plan.
    fn texts(s: &Sheet, r: &Rollup, m: &GridIndex, g: usize) -> Vec<String> {
        cells(s, r, &plan(), m, g)
            .into_iter()
            .map(|c| c.text.to_string())
            .collect()
    }

    fn tree(m: &GridIndex, g: usize) -> TreeRef<'_> {
        m.tree(g).unwrap_or_else(|| panic!("no row {g}"))
    }

    fn build(s: &Sheet, e: &Expansion) -> (Rollup, GridIndex) {
        build_vis(s, e, &Visibility::all(s))
    }

    /// The flat sheet (an empty chain) under `v`: what the tile paints
    /// with no grouping.
    fn build_vis(s: &Sheet, e: &Expansion, v: &Visibility) -> (Rollup, GridIndex) {
        let dims = DerivedDimensions::default();
        let r = rollup::build(s, v, &EffectiveChain::default(), &dims, Clock::utc());
        let m = GridIndex::build(
            s,
            &r,
            &GroupExpansion::default(),
            e,
            &plan(),
            Clock::utc(),
            &ValueColors::default(),
        );
        (r, m)
    }

    #[test]
    fn rows_follow_the_expansion_and_carry_depth_ids_tags_and_search_keys() {
        let s = sheet();
        let (_, closed) = build(&s, &Expansion::default());
        assert_eq!(closed.len(), 3);
        assert_eq!(
            tree(&closed, 1).kind,
            GridRowKind::Package {
                open: false,
                partial: false,
                split: false,
            }
        );
        assert_eq!(tree(&closed, 1).search.as_ref(), "SPX Z26 4800/5200 CS");
        assert_eq!(
            tree(&closed, 1).tag.as_ref(),
            "CS",
            "a package: its template token"
        );
        assert_eq!(tree(&closed, 0).tag.as_ref(), "", "a line: no tag");
        let mut e = Expansion::default();
        e.set(s.id(1), true);
        let (_, open) = build(&s, &e);
        assert_eq!(open.len(), 5);
        assert_eq!(
            tree(&open, 1).kind,
            GridRowKind::Package {
                open: true,
                partial: false,
                split: false,
            }
        );
        assert_eq!(tree(&open, 2).kind, GridRowKind::Leg { last: false });
        assert_eq!(tree(&open, 2).depth, 1);
        assert_eq!(tree(&open, 2).tag.as_ref(), "", "a leg: no tag");
        assert_eq!(tree(&open, 4).search.as_ref(), "SPX Z26 4000 P");
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
        let (_, m) = build(&s, &e);
        assert_eq!(tree(&m, 0).text.as_ref(), "SPX Z26 5000 C");
        assert_eq!(tree(&m, 0).note.as_ref(), "", "a bare line has no note");
        assert_eq!(tree(&m, 1).text.as_ref(), "Z26 4800/5200");
        assert_eq!(tree(&m, 1).note.as_ref(), "· 2 legs");
        assert_eq!(tree(&m, 2).text.as_ref(), "SPX Z26 4800 C");
        assert_eq!(tree(&m, 3).text.as_ref(), "-1 SPX Z26 5200 C");
        assert_eq!(tree(&m, 2).note.as_ref(), "", "a leg has no note");
        assert_eq!(tree(&m, 4).text.as_ref(), "SPX Z26 4000 P");
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
        let (_, m) = build(&s, &e);
        let kinds: Vec<GridRowKind> = m.trees().map(|r| r.kind).collect();
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
        let (_, m) = build(&s, &e);
        assert_eq!(m.len(), 2);
        assert_eq!(tree(&m, 0).note.as_ref(), "· 1 leg");
        assert_eq!(tree(&m, 0).text.as_ref(), "Z26 4800");
        assert_eq!(tree(&m, 1).kind, GridRowKind::Leg { last: true });
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
        let (_, m) = build(&s, &Expansion::default());
        let text = tree(&m, 0).text;
        assert!(!text.contains('\n'), "{text:?}");
        assert_eq!(text.as_ref(), "Z26 5000/4000");
        assert_eq!(tree(&m, 0).note.as_ref(), "· 2 legs");
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
        let (_, m) = build(&s, &Expansion::default());
        let row = tree(&m, 0);
        assert_eq!(row.text.as_ref(), "Z26 5000/4000");
        assert!(
            row.search.contains(row.text.as_ref()),
            "{:?} does not find {:?}",
            row.search,
            row.text
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
        let (_, m) = build(&s, &Expansion::default());
        assert_eq!(tree(&m, 0).text.as_ref(), "Z26 5000");
    }

    #[test]
    fn cells_carry_the_core_text_and_state() {
        let mut s = sheet();
        let answers: Vec<_> = (0..s.len())
            .filter(|r| s.is_line(*r))
            .map(|r| (s.id(r), s.revision(r), Ok(result(12.5))))
            .collect();
        s.deliver_all(answers, at(0));
        let (r, m) = build(&s, &Expansion::default());
        let price = col("npv");
        let row0 = cells(&s, &r, &plan(), &m, 0);
        assert_eq!(row0[price].text.as_ref(), "12.50");
        assert_eq!(row0[price].state, CellState::Own);
        let strike = col("strike");
        let row1 = cells(&s, &r, &plan(), &m, 1);
        assert_eq!(
            (row1[strike].text.as_ref(), row1[strike].state),
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
        let (r, before) = build(&s, &Expansion::default());
        assert_eq!(
            cells(&s, &r, &plan(), &before, 0)[col("npv")].sign,
            None,
            "unpriced"
        );
        let mut neg = result(-12.5);
        neg.set(Measure::Delta01, false, -0.5);
        neg.set(Measure::Gamma01, false, 0.0);
        s.deliver_all(vec![(s.id(0), s.revision(0), Ok(neg))], at(0));
        let (r, m) = build(&s, &Expansion::default());
        let row0 = cells(&s, &r, &plan(), &m, 0);
        assert_eq!(row0[col("npv")].sign, Some(Sign::Negative));
        assert_eq!(row0[col("delta01")].sign, Some(Sign::Negative));
        assert_eq!(row0[col("gamma01")].sign, Some(Sign::Zero));
        assert_eq!(row0[col("strike")].sign, None, "text");
        assert_eq!(row0[col("qty")].sign, None, "a dimension");
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
        let (r, m) = build_vis(&s, &e, &v);
        let p = tree(&m, 1);
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
        let pc = cells(&s, &r, &plan(), &m, 1);
        assert_eq!(pc[col("strike")].text.as_ref(), "5200");
        assert_eq!(pc[col("qty")].text.as_ref(), "-1", "the leg's own qty");
        assert_eq!(
            pc[col("npv")].text.as_ref(),
            "-2.00",
            "qty × price of the shown leg, not the package's -1.00 fold"
        );
        assert_eq!(pc[col("npv")].sign, Some(Sign::Negative));
        assert_eq!(tree(&m, 2).row, Some(3));
        assert_eq!(
            tree(&m, 2).kind,
            GridRowKind::Leg { last: true },
            "the last shown leg takes the corner"
        );
        // Unscoped, the same package paints its whole fold.
        let (r, all) = build(&s, &e);
        assert_eq!(texts(&s, &r, &all, 1)[col("npv")], "1.00");
        assert_eq!(tree(&all, 1).note.as_ref(), "· 2 legs");
    }

    /// A partly hidden package's find key reads its shown legs, as its
    /// summary does: `/` must never find a row by a hidden leg's strike
    /// that column 0 does not paint.
    #[test]
    fn a_partly_hidden_packages_find_key_reads_only_its_shown_legs() {
        let s = sheet();
        let v = scoped(&s);
        let (_, m) = build_vis(&s, &Expansion::default(), &v);
        let p = tree(&m, 1);
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
        let (_, all) = build(&s, &Expansion::default());
        assert!(tree(&all, 1).search.contains("4800"));
    }

    #[test]
    fn hidden_rows_are_not_in_the_model() {
        let s = sheet();
        let mut e = Expansion::default();
        e.set(s.id(1), true);
        let v = scoped(&s);
        assert_eq!(v.hidden, 2, "the 4800 leg and B");
        let (_, m) = build_vis(&s, &e, &v);
        let rows: Vec<Option<usize>> = m.trees().map(|r| r.row).collect();
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
        let (_, m) = build(&s, &Expansion::default());
        assert_eq!(tree(&m, 0).tag.as_ref(), "CUSTOM");
        assert_eq!(tree(&m, 0).search.as_ref(), "CUSTOM SPX Z26 5000/4000");
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
            .map(|l| in_usd(crate::core::shorthand::parse_builtin(l).unwrap()))
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
    ) -> (rollup::Rollup, GridIndex) {
        let levels: Vec<String> = levels.iter().map(|l| l.to_string()).collect();
        let chain = effective_chain(&levels, dims);
        let r = rollup::build(s, &Visibility::all(s), &chain, dims, Clock::utc());
        let m = GridIndex::build(
            s,
            &r,
            groups,
            packages,
            plan,
            Clock::utc(),
            &ValueColors::default(),
        );
        (r, m)
    }

    fn grouped(
        s: &Sheet,
        levels: &[&str],
        groups: &GroupExpansion,
        packages: &Expansion,
    ) -> (rollup::Rollup, GridIndex) {
        let dims = DerivedDimensions::default();
        grouped_with(s, levels, &dims, groups, packages, &plan())
    }

    fn all_open() -> GroupExpansion {
        let mut g = GroupExpansion::default();
        g.open_all();
        g
    }

    /// The priced fixture grouped by `[underlying_ref, expiry]` under the
    /// vanilla plan (which shows `underlying_ref`), every group and package
    /// open.
    fn grouped_fixture() -> (Sheet, Rollup, ColumnPlan, GroupExpansion, Expansion) {
        let s = priced();
        let mut packages = Expansion::default();
        packages.open_all(&s);
        let groups = all_open();
        let (r, _) = grouped(&s, &["underlying_ref", "expiry"], &groups, &packages);
        (s, r, plan(), groups, packages)
    }

    /// One line per grid row, indented by depth: a group `▾`/`▸` and its
    /// text, a package `P<row>` and its note (`split` when split), a leg
    /// `├` (`└` if last) and `L<row>`, a line `L<row>`.
    fn describe(m: &GridIndex) -> Vec<String> {
        m.trees()
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
                    GridRowKind::Leg { last } => {
                        format!("{pad}{} L{}", if last { "└" } else { "├" }, r.row.unwrap())
                    }
                    GridRowKind::Line => format!("{pad}L{}", r.row.unwrap()),
                }
            })
            .collect()
    }

    /// The group row labelled `label`.
    fn group_row(m: &GridIndex, label: &str) -> usize {
        (0..m.len())
            .find(|&g| {
                matches!(m.kind(g), Some(GridRowKind::Group { .. }))
                    && m.text(g).is_some_and(|t| t.as_ref() == label)
            })
            .unwrap_or_else(|| panic!("no group row {label}: {:?}", describe(m)))
    }

    /// Row `g`'s cell in column `name` of the vanilla plan.
    fn cell(s: &Sheet, r: &Rollup, m: &GridIndex, g: usize, name: &str) -> (String, CellState) {
        let c = CellPass::new(src(s, r, &plan()), m)
            .cell(g, col(name))
            .expect("a planned column");
        (c.text.to_string(), c.state)
    }

    /// `(text, state)` as [`cell`] returns it.
    fn ts(text: &str, state: CellState) -> (String, CellState) {
        (text.to_string(), state)
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

    /// The window holds exactly what the formatter formats, group rows and
    /// partial packages included.
    #[test]
    fn the_window_paints_what_the_formatter_formats() {
        let (s, tree, plan, groups, packages) = grouped_fixture();
        let index = GridIndex::build(
            &s,
            &tree,
            &groups,
            &packages,
            &plan,
            Clock::utc(),
            &ValueColors::default(),
        );
        let mut window = WindowCache::default();
        let mut pass = CellPass::new(src(&s, &tree, &plan), &index);
        window.set_window(0..index.len(), index.columns.len(), |g, c| pass.cell(g, c));
        let mut fresh = CellPass::new(src(&s, &tree, &plan), &index);
        for g in (0..index.len()).rev() {
            for c in 0..index.columns.len() {
                assert_eq!(window.get(g, c).cloned(), fresh.cell(g, c), "({g}, {c})");
            }
        }
    }

    /// A group's label takes its raw value's color, and a leg's dimension
    /// cell its value's; a measure cell, a line's shorthand and a `mixed`
    /// cell (whose text is no value) never do.
    #[test]
    fn a_group_label_and_a_dimension_cell_carry_their_values_color() {
        let (s, rollup, plan, groups, packages) = grouped_fixture();
        let mut values = ValueColors::default();
        values.insert("underlying_ref", "SPX", "blue");
        // `option_type` reads `mixed` on the SPX group row (puts and calls).
        values.insert("option_type", "mixed", "blue");
        let m = GridIndex::build(
            &s,
            &rollup,
            &groups,
            &packages,
            &plan,
            Clock::utc(),
            &values,
        );
        let spx = group_row(&m, "SPX");
        assert_eq!(
            m.tree(spx).unwrap().value_color.map(|c| &**c),
            Some("blue"),
            "the group's label is its value"
        );
        assert_eq!(
            m.tree(group_row(&m, "NDX")).unwrap().value_color,
            None,
            "an unmapped group"
        );
        let src = FillSource {
            sheet: &s,
            rollup: &rollup,
            plan: &plan,
            clock: Clock::utc(),
            values: &values,
        };
        let mut pass = CellPass::new(src, &m);
        let under = col("underlying_ref");
        // The first leg row under the SPX group shows SPX in its
        // underlying cell.
        let leg = (spx + 1..m.len())
            .find(|&g| m.sheet_row(g).is_some())
            .expect("a line under the group");
        let cell = pass.cell(leg, under).expect("an underlying cell");
        assert_eq!(&*cell.text, "SPX");
        assert_eq!(cell.value_color.as_deref(), Some("blue"));
        // A measure cell and a line's shorthand never do.
        assert_eq!(pass.cell(leg, col("qty")).and_then(|c| c.value_color), None);
        assert_eq!(m.tree(leg).unwrap().value_color, None);
        let mixed = pass.cell(spx, col("option_type")).expect("an option cell");
        assert_eq!(
            (mixed.text.as_ref(), mixed.state),
            ("mixed", CellState::Mixed),
            "fixture: the SPX group's option type is mixed"
        );
        assert_eq!(mixed.value_color, None, "`mixed` is not a value");
    }

    /// `row_of` names a line's first painted row; a split package paints twice.
    #[test]
    fn row_of_is_the_first_painted_row_of_a_line() {
        let (s, tree, plan, groups, packages) = grouped_fixture();
        let index = GridIndex::build(
            &s,
            &tree,
            &groups,
            &packages,
            &plan,
            Clock::utc(),
            &ValueColors::default(),
        );
        let cal = s.id(5);
        assert_eq!(index.paints(cal), 2, "the calendar splits by expiry");
        for g in 0..index.len() {
            if let Some(id) = index.id(g) {
                let first = (0..index.len()).find(|&x| index.id(x) == Some(id));
                assert_eq!(index.grid_row_of(id), first, "{id:?}");
                assert_eq!(
                    index.rows_of(id).count(),
                    (0..index.len())
                        .filter(|&x| index.id(x) == Some(id))
                        .count()
                );
            }
        }
    }

    /// A nested group row shows each enclosing group's value in that group's
    /// column, not a unanimity over its legs.
    #[test]
    fn a_group_row_shows_its_ancestors_grouped_values() {
        let (s, tree, plan, groups, packages) = grouped_fixture();
        let index = GridIndex::build(
            &s,
            &tree,
            &groups,
            &packages,
            &plan,
            Clock::utc(),
            &ValueColors::default(),
        );
        let und = index
            .columns
            .iter()
            .position(|c| c.name == "underlying_ref")
            .expect("shown");
        let inner = (0..index.len())
            .find(|&g| matches!(index.kind(g), Some(GridRowKind::Group { depth: 1, .. })))
            .expect("a nested group");
        let outer = index.parent(inner).expect("enclosed");
        let mut pass = CellPass::new(src(&s, &tree, &plan), &index);
        assert_eq!(
            pass.cell(inner, und).map(|c| c.text),
            index.text(outer).cloned(),
            "the enclosing group's value"
        );

        // Under `underlying_ref` the legs' unanimity agrees by construction;
        // under `qty` it does not: a group's qty cell otherwise counts its
        // legs, so the nested group must read its parent's value instead.
        let (r, m) = grouped(&s, &["qty", "expiry"], &all_open(), &Expansion::default());
        let qty = col("qty");
        let nested: Vec<usize> = (0..m.len())
            .filter(|&g| matches!(m.kind(g), Some(GridRowKind::Group { depth: 1, .. })))
            .collect();
        assert!(!nested.is_empty(), "{:?}", describe(&m));
        for g in nested {
            let parent = m.parent(g).expect("enclosed");
            assert_eq!(
                cell(&s, &r, &m, g, "qty").0,
                m.text(parent).expect("a label").to_string(),
                "row {g}: {:?}",
                describe(&m)
            );
            assert_eq!(cells(&s, &r, &plan, &m, g)[qty].state, CellState::Own);
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
        let spx = tree(&m, group_row(&m, "SPX"));
        assert_eq!(
            spx.kind,
            GridRowKind::Group {
                open: true,
                depth: 0
            }
        );
        assert_eq!((spx.row, spx.id), (None, None), "a group is no sheet row");
        assert_eq!(spx.path, Some(&vec![Some("SPX".to_string())]));
        let node = spx.node.expect("a group row names its node");
        assert!(
            matches!(&r.nodes[node].kind, rollup::NodeKind::Group { label, .. } if label == "SPX")
        );
        assert_eq!((spx.tag.as_ref(), spx.note.as_ref()), ("", ""));
        assert_eq!(spx.search.as_ref(), "SPX", "find matches the label");
        let p1 = tree(&m, 4);
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
            m.kind(group_row(&m, "SPX")),
            Some(GridRowKind::Group {
                open: false,
                depth: 0
            })
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
        let c = m.trees().filter(|r| r.text.as_ref() == "C").nth(1).unwrap();
        assert_eq!(
            c.kind,
            GridRowKind::Group {
                open: true,
                depth: 1
            }
        );
        assert_eq!(
            c.path,
            Some(&vec![Some("SPX".to_string()), Some("C".to_string())])
        );
    }

    /// A group row: measures summed over its legs × qty; an ungrouped
    /// dimension its value where unanimous, `mixed` (muted) where not,
    /// blank where none; the grouped column its node's value; qty the leg
    /// count; status the legs' fold.
    #[test]
    fn a_group_row_sums_its_legs_and_reads_unanimity() {
        let s = priced();
        let (r, m) = grouped(&s, &["underlying_ref"], &all_open(), &Expansion::default());
        let spx = group_row(&m, "SPX");
        let npv: f64 = [0usize, 2, 3, 6, 7, 8]
            .iter()
            .map(|&r| s.qty(r) as f64 * (r as f64 + 1.0))
            .sum();
        assert_eq!(
            cell(&s, &r, &m, spx, "npv"),
            ts(&format!("{npv:.2}"), CellState::Own)
        );
        assert!(
            cells(&s, &r, &plan(), &m, spx)[col("npv")].sign.is_some(),
            "a sum carries its sign"
        );
        assert_eq!(
            cell(&s, &r, &m, spx, "qty"),
            ts("6", CellState::Own),
            "the leg count"
        );
        assert_eq!(
            cell(&s, &r, &m, spx, "underlying_ref"),
            ts("SPX", CellState::Own)
        );
        assert_eq!(
            cell(&s, &r, &m, spx, "strike"),
            ts("mixed", CellState::Mixed)
        );
        assert_eq!(
            cell(&s, &r, &m, spx, "expiry"),
            ts("mixed", CellState::Mixed)
        );
        assert_eq!(
            cell(&s, &r, &m, spx, "option_type"),
            ts("mixed", CellState::Mixed)
        );
        assert_eq!(cell(&s, &r, &m, spx, "currency"), ts("USD", CellState::Own));
        assert_eq!(
            cell(&s, &r, &m, spx, "spot_shift"),
            ts("", CellState::Blank),
            "no leg sets one"
        );
        assert_eq!(
            cell(&s, &r, &m, spx, "status"),
            ts("", CellState::Own),
            "every leg fresh"
        );
        let ndx = group_row(&m, "NDX");
        assert_eq!(
            cell(&s, &r, &m, ndx, "strike"),
            ts("5000", CellState::Own),
            "one line: unanimous"
        );
        assert_eq!(
            cell(&s, &r, &m, ndx, "option_type"),
            ts("C", CellState::Own)
        );
        assert_eq!(cell(&s, &r, &m, ndx, "qty"), ts("1", CellState::Own));
        assert_eq!(cell(&s, &r, &m, ndx, "npv"), ts("5.00", CellState::Own));

        // A leg repricing: the group's status and sums read the fold.
        let mut s = priced();
        s.touch(0);
        let (r, m) = grouped(&s, &["underlying_ref"], &all_open(), &Expansion::default());
        let spx = group_row(&m, "SPX");
        assert_eq!(
            cell(&s, &r, &m, spx, "status"),
            ts("pricing…", CellState::Stale)
        );
        assert_eq!(
            cell(&s, &r, &m, spx, "npv").1,
            CellState::Stale,
            "the sum is muted"
        );
        assert_eq!(
            cell(&s, &r, &m, group_row(&m, "NDX"), "npv").1,
            CellState::Own
        );
    }

    /// A NULL beside a value is `mixed`, not the value: showing it would
    /// claim it for the legs that have none (the blotter's rule).
    #[test]
    fn a_blank_beside_a_value_is_mixed() {
        let mut s = fixture();
        push(
            &mut s,
            vec![in_usd(
                crate::core::shorthand::parse_builtin("SPX Z26 4000 P DO 3500").unwrap(),
            )],
        );
        let plan = plan_of(&["barrier"]);
        let dims = DerivedDimensions::default();
        let (r, m) = grouped_with(
            &s,
            &["underlying_ref"],
            &dims,
            &all_open(),
            &Expansion::default(),
            &plan,
        );
        let spx = &cells(&s, &r, &plan, &m, group_row(&m, "SPX"))[0];
        assert_eq!((spx.text.as_ref(), spx.state), ("mixed", CellState::Mixed));
        let ndx = &cells(&s, &r, &plan, &m, group_row(&m, "NDX"))[0];
        assert_eq!(
            (ndx.text.as_ref(), ndx.state),
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
        crate::core::sheet::tests::deliver_in_eur(&mut s, 0, result(1.0), at(1));
        let plan = plan_of(&["npv", "npv_usd", "currency"]);
        let dims = DerivedDimensions::default();
        let (r, m) = grouped_with(
            &s,
            &["underlying_ref"],
            &dims,
            &all_open(),
            &Expansion::default(),
            &plan,
        );
        let spx = cells(&s, &r, &plan, &m, group_row(&m, "SPX"));
        let at = |i: usize| (spx[i].text.as_ref(), spx[i].state);
        assert_eq!(at(0), ("—", CellState::Stale));
        let usd: f64 = [0usize, 2, 3, 6, 7, 8]
            .iter()
            .map(|&r| s.qty(r) as f64 * s.result(r).unwrap().get(Measure::Npv, true))
            .sum();
        assert_eq!(at(1), (format!("{usd:.2}").as_str(), CellState::Own));
        assert_eq!(at(2), ("mixed", CellState::Mixed));
        let ndx = &cells(&s, &r, &plan, &m, group_row(&m, "NDX"))[0];
        assert_eq!(
            (ndx.text.as_ref(), ndx.state),
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
        let (r, m) = grouped(&s, &["expiry"], &all_open(), &packages);
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
        let cal: Vec<usize> = (0..m.len())
            .filter(|&g| m.sheet_row(g) == Some(5))
            .collect();
        assert_eq!(
            tree(&m, cal[0]).kind,
            GridRowKind::Package {
                open: true,
                partial: true,
                split: true
            }
        );
        assert_eq!(
            tree(&m, cal[0]).text.as_ref(),
            "Z26 5000",
            "the node's legs' summary"
        );
        assert_eq!(tree(&m, cal[1]).text.as_ref(), "H27 5000");
        for (g, leg) in [(cal[0], 7usize), (cal[1], 6)] {
            let npv = s.qty(leg) as f64 * (leg as f64 + 1.0);
            assert_eq!(
                cell(&s, &r, &m, g, "npv").0,
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
        let (r, m) = grouped(&s, &["expiry"], &all_open(), &Expansion::default());
        let z26 = tree(&m, 0);
        assert_eq!(z26.text.as_ref(), "Z26");
        assert_eq!(
            z26.path,
            Some(&vec![Some("2026-12-18".to_string())]),
            "the path stays raw"
        );
        assert_eq!(cell(&s, &r, &m, 0, "expiry"), ts("Z26", CellState::Own));

        let barrier = plan_of(&["barrier"]);
        let dims = DerivedDimensions::default();
        let (r, m) = grouped_with(
            &s,
            &["barrier"],
            &dims,
            &all_open(),
            &Expansion::default(),
            &barrier,
        );
        assert_eq!(tree(&m, 0).text.as_ref(), "—", "no line reads a barrier");
        assert_eq!(tree(&m, 0).path, Some(&vec![None]));
        let c = &cells(&s, &r, &barrier, &m, 0)[0];
        assert_eq!((c.text.as_ref(), c.state), ("", CellState::Blank));

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
            .trees()
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
            tree(&m, 4).text.as_ref(),
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
                .trees()
                .map(|row| (row.node.expect("every row is a node"), row.search.clone()))
                .collect();
            assert_eq!(got, want, "{levels:?}");
        }
    }
}
