//! The rollup tree: the sheet's shown lines arranged under a grouping
//! chain, as the blotter arranges positions.
//!
//! Value levels partition the shown legs (and bare lines) by the value the
//! line reads as a `pricer` row ([`SheetRow`]), so a grouping value is
//! exactly the scope value: `expiry` groups by ISO date or tenor, a leg's
//! `template` is its package's. Values compare with NULL distinct from the
//! empty string; groups order numbers by number, text by byte order (the
//! blotter's sort) and NULL last. Beneath the last value level, lines are
//! gathered by parent in sheet order: a package's legs under one `Package`
//! node, bare lines as `Leaf` nodes. A package whose legs fall under
//! different groups appears under each, with only that group's legs — a
//! calendar under `expiry` sits beneath both dates — and is `split`.
//!
//! `position_ref` and `instrument_ref` are structural levels, never
//! compared against values (they are the sheet's own ids): `position_ref`
//! is the `Package` node itself, `instrument_ref` makes every leg a leaf
//! with no package row. Either ends the tree, since a line is the finest
//! node it has; a level after one is dropped.
//!
//! Pure: no I/O, no entities. Hidden lines are never placed.

use crate::core::dataset::pricer_dataset;
use crate::core::sheet::Sheet;
use crate::core::visibility::{SheetRow, Visibility};
use geode_core::clock::Clock;
use geode_core::dimensions::{DerivedDimension, DerivedDimensions};
use geode_core::document::Value;
use geode_core::expansion::Path;
use geode_core::scope::RowValues;
use gpui::SharedString;
use std::cmp::Ordering;

const POSITION_REF: &str = "position_ref";
const INSTRUMENT_REF: &str = "instrument_ref";

/// A group node's label for NULL (the path segment is `None`).
pub const NULL_LABEL: &str = "—";

/// The chain after dropping levels `pricer` cannot group by (spec ruling
/// 8): `kept` in order, `dropped` in order, for the header.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectiveChain {
    pub kept: Vec<String>,
    pub dropped: Vec<String>,
}

impl EffectiveChain {
    /// No level to group by: the flat sheet.
    pub fn is_flat(&self) -> bool {
        self.kept.is_empty()
    }
}

/// One resolved level of the chain.
enum Level<'a> {
    /// A groupable `pricer` column, read through [`SheetRow`].
    Column(&'a str),
    /// A derived dimension over a groupable `pricer` column: the mapped
    /// label, NULL for an unmapped or NULL source (the SQL `CASE`'s rule).
    Derived(&'a DerivedDimension),
    /// `position_ref`: packages and bare lines; ends the tree.
    Positions,
    /// `instrument_ref`: every leg a leaf; ends the tree.
    Instruments,
}

fn resolve<'a>(
    name: &'a str,
    groupable: &[&str],
    dims: &'a DerivedDimensions,
) -> Option<Level<'a>> {
    match name {
        POSITION_REF => return Some(Level::Positions),
        INSTRUMENT_REF => return Some(Level::Instruments),
        _ => {}
    }
    if groupable.contains(&name) {
        return Some(Level::Column(name));
    }
    // Mirrors the shell's groupable vocabulary (derived over a groupable
    // base), minus the synthetic keys: a desk map over `p<id>`/`i<id>`
    // could only ever be NULL, a plausible grouping of nothing.
    let d = dims.get(name)?;
    let base = d.from.as_str();
    (groupable.contains(&base) && base != POSITION_REF && base != INSTRUMENT_REF)
        .then_some(Level::Derived(d))
}

/// Resolve `chain` against the `pricer` dataset: a level it cannot group
/// by (a column it lacks, a measure, a derived dimension over either or
/// over a synthetic key) is dropped, as is every level after a structural
/// one (`position_ref` / `instrument_ref`) and a level already kept (a
/// repeat would only nest each node under an identical parent; the
/// Groupings dialog refuses one, but `:group` and a hand-edited slot
/// can still name it).
pub fn effective_chain(chain: &[String], dims: &DerivedDimensions) -> EffectiveChain {
    let groupable = pricer_dataset().groupable_columns();
    let mut out = EffectiveChain::default();
    let mut ended = false;
    for name in chain {
        let repeat = out.kept.contains(name);
        match (ended || repeat, resolve(name, &groupable, dims)) {
            (false, Some(level)) => {
                ended = matches!(level, Level::Positions | Level::Instruments);
                out.kept.push(name.clone());
            }
            _ => out.dropped.push(name.clone()),
        }
    }
    out
}

/// What `:group` can pin, for its completion: every groupable `pricer`
/// column (the structural `position_ref`/`instrument_ref` included) and
/// every derived dimension over one that is not synthetic — the names
/// [`effective_chain`] keeps. Sorted, each once.
pub fn groupable_vocabulary(dims: &DerivedDimensions) -> Vec<String> {
    let groupable = pricer_dataset().groupable_columns();
    let mut out: Vec<String> = groupable.iter().map(|c| c.to_string()).collect();
    out.extend(
        dims.all()
            .filter(|d| resolve(&d.name, &groupable, dims).is_some())
            .map(|d| d.name.clone()),
    );
    out.sort();
    out.dedup();
    out
}

/// How many of `chain`'s kept levels are value levels (group rows):
/// every kept level but a closing structural one. The longest group path
/// the chain can produce, and so the regroup's `Expansion::prune_to`.
pub fn value_levels(chain: &EffectiveChain) -> usize {
    chain
        .kept
        .iter()
        .filter(|l| *l != POSITION_REF && *l != INSTRUMENT_REF)
        .count()
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// A value level's node. `level` indexes the effective chain's `kept`;
    /// `label` is the value's text (`—` for NULL).
    Group {
        level: usize,
        column: String,
        value: Option<Value>,
        label: SharedString,
    },
    /// `legs` = this node's legs of the package (sheet rows, sheet order);
    /// `split` = fewer than the package's SHOWN legs; `partial` = fewer
    /// than all its legs. Split implies partial; a scope hiding legs makes
    /// a package partial without splitting it. An empty package has no
    /// legs and is neither.
    Package {
        row: usize,
        legs: Vec<usize>,
        split: bool,
        partial: bool,
    },
    /// A shown line: a bare line, or a leg (beneath its `Package` node, or
    /// directly under `instrument_ref`).
    Leaf { row: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub kind: NodeKind,
    /// 0 at the roots; a package's leg leaves sit one deeper than it.
    pub depth: usize,
    /// A group's grouping values from the root to it (its `Expansion`
    /// key); a package or leaf carries its enclosing group's path (empty
    /// at the root) — packages expand by `LineId`, not by path.
    pub path: Path,
    /// Node ids, in display order.
    pub children: Vec<usize>,
}

/// The tree in preorder: every node's id is greater than its parent's.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rollup {
    pub nodes: Vec<Node>,
    pub roots: Vec<usize>,
}

impl Rollup {
    /// `node`'s parent, `None` at a root. Preorder puts a parent before
    /// its children, so the scan runs backwards from `node`.
    pub fn parent(&self, node: usize) -> Option<usize> {
        (0..node.min(self.nodes.len()))
            .rev()
            .find(|&p| self.nodes[p].children.contains(&node))
    }
}

/// A placed row with the value it reads at each value level: a shown leg,
/// a shown bare line, or an empty package (which reads its own row).
struct Item {
    row: usize,
    keys: Vec<Option<Value>>,
}

/// Arrange `sheet`'s shown lines under `chain` (from [`effective_chain`]
/// with the same `dims`; a level that does not resolve is skipped). An
/// empty chain is the flat sheet: packages and bare lines in sheet order,
/// each package's shown legs as its children.
pub fn build(
    sheet: &Sheet,
    vis: &Visibility,
    chain: &EffectiveChain,
    dims: &DerivedDimensions,
    clock: Clock,
) -> Rollup {
    let groupable = pricer_dataset().groupable_columns();
    let mut values: Vec<(&str, Level<'_>)> = Vec::new();
    let mut tail = Level::Positions;
    for name in &chain.kept {
        match resolve(name, &groupable, dims) {
            Some(end @ (Level::Positions | Level::Instruments)) => {
                tail = end;
                break;
            }
            Some(level) => values.push((name.as_str(), level)),
            None => {}
        }
    }
    let items: Vec<Item> = (0..sheet.len())
        .filter(|&row| {
            vis.is_shown(row)
                && (sheet.is_line(row) || (sheet.is_package(row) && sheet.children(row).is_empty()))
        })
        .map(|row| {
            let line = SheetRow { sheet, row, clock };
            let keys = values
                .iter()
                .map(|(_, level)| match level {
                    Level::Column(c) => line.value(c).map(zero),
                    Level::Derived(d) => derived(d, line.value(&d.from)),
                    Level::Positions | Level::Instruments => unreachable!("value levels only"),
                })
                .collect();
            Item { row, keys }
        })
        .collect();
    let mut b = Builder {
        sheet,
        vis,
        values: &values,
        instruments: matches!(tail, Level::Instruments),
        nodes: Vec::new(),
    };
    let order: Vec<&Item> = items.iter().collect();
    let roots = b.place(&order, 0, &Vec::new());
    Rollup {
        nodes: b.nodes,
        roots,
    }
}

/// `-0.0` groups with `0.0` (and would otherwise label `-0`).
fn zero(mut v: Value) -> Value {
    if let Value::F64(f) = &mut v
        && *f == 0.0
    {
        *f = 0.0;
    }
    v
}

/// A derived level's value: the label its source maps to. A number or
/// date source matches a map key that spells the same value, as the SQL
/// `CASE` casts the key literal to the column's type.
fn derived(d: &DerivedDimension, source: Option<Value>) -> Option<Value> {
    let label = match source? {
        Value::Utf8(s) => d.values.get(&s),
        Value::F64(f) => d
            .values
            .iter()
            .find(|(k, _)| k.trim().parse::<f64>().is_ok_and(|k| k == f))
            .map(|(_, v)| v),
        Value::I64(i) => d
            .values
            .iter()
            .find(|(k, _)| k.trim().parse::<i64>().is_ok_and(|k| k == i))
            .map(|(_, v)| v),
        Value::Date(date) => d.values.get(&date.format("%Y-%m-%d").to_string()),
    };
    label.map(|l| Value::Utf8(l.clone()))
}

/// Group order: NULL last; numbers by number (NaN equal to itself, above
/// every number), text by byte order, dates by date.
fn cmp_key(a: &Option<Value>, b: &Option<Value>) -> Ordering {
    let rank = |v: &Value| match v {
        Value::I64(_) => 0,
        Value::F64(_) => 1,
        Value::Date(_) => 2,
        Value::Utf8(_) => 3,
    };
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => match (a, b) {
            (Value::I64(a), Value::I64(b)) => a.cmp(b),
            (Value::F64(a), Value::F64(b)) => match (a.is_nan(), b.is_nan()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => a.partial_cmp(b).expect("neither is NaN"),
            },
            (Value::Date(a), Value::Date(b)) => a.cmp(b),
            (Value::Utf8(a), Value::Utf8(b)) => a.as_bytes().cmp(b.as_bytes()),
            (a, b) => rank(a).cmp(&rank(b)),
        },
    }
}

/// A value's text: its label and its path segment.
fn text(v: &Value) -> String {
    match v {
        Value::Utf8(s) => s.clone(),
        Value::F64(f) => f.to_string(),
        Value::I64(i) => i.to_string(),
        Value::Date(d) => d.format("%Y-%m-%d").to_string(),
    }
}

struct Builder<'a> {
    sheet: &'a Sheet,
    vis: &'a Visibility,
    values: &'a [(&'a str, Level<'a>)],
    instruments: bool,
    nodes: Vec<Node>,
}

impl Builder<'_> {
    fn push(&mut self, kind: NodeKind, depth: usize, path: &Path) -> usize {
        self.nodes.push(Node {
            kind,
            depth,
            path: path.clone(),
            children: Vec::new(),
        });
        self.nodes.len() - 1
    }

    /// Place `items` (sheet order) from value level `level` down; returns
    /// the ids of the nodes made at this depth. Depth equals `level` here:
    /// every value level adds one.
    fn place(&mut self, items: &[&Item], level: usize, path: &Path) -> Vec<usize> {
        let depth = level;
        if level < self.values.len() {
            let mut sorted = items.to_vec();
            // Stable: within a group, lines keep sheet order.
            sorted.sort_by(|a, b| cmp_key(&a.keys[level], &b.keys[level]));
            let mut out = Vec::new();
            for chunk in sorted.chunk_by(|a, b| cmp_key(&a.keys[level], &b.keys[level]).is_eq()) {
                let value = chunk[0].keys[level].clone();
                let segment = value.as_ref().map(text);
                let label =
                    SharedString::from(segment.clone().unwrap_or_else(|| NULL_LABEL.into()));
                let mut child_path = path.clone();
                child_path.push(segment);
                let id = self.push(
                    NodeKind::Group {
                        level,
                        column: self.values[level].0.to_string(),
                        value,
                        label,
                    },
                    depth,
                    &child_path,
                );
                let children = self.place(chunk, level + 1, &child_path);
                self.nodes[id].children = children;
                out.push(id);
            }
            return out;
        }
        self.gather(items, depth, path)
    }

    /// Beneath the last value level: packages with this node's legs and
    /// bare lines in sheet order, or (under `instrument_ref`) every leg a
    /// leaf. A package's legs are contiguous sheet rows, so its legs among
    /// `items` (sheet order) are adjacent.
    fn gather(&mut self, items: &[&Item], depth: usize, path: &Path) -> Vec<usize> {
        let sheet = self.sheet;
        let mut out = Vec::new();
        let mut i = 0;
        while i < items.len() {
            let row = items[i].row;
            match sheet.parent(row) {
                Some(_) if self.instruments => {
                    out.push(self.push(NodeKind::Leaf { row }, depth, path));
                    i += 1;
                }
                Some(package) => {
                    let mut legs = Vec::new();
                    while i < items.len() && sheet.parent(items[i].row) == Some(package) {
                        legs.push(items[i].row);
                        i += 1;
                    }
                    let all = sheet.children(package).len();
                    let shown = sheet
                        .children(package)
                        .filter(|&l| self.vis.is_shown(l))
                        .count();
                    let kind = NodeKind::Package {
                        row: package,
                        split: legs.len() < shown,
                        partial: legs.len() < all,
                        legs: legs.clone(),
                    };
                    let id = self.push(kind, depth, path);
                    let children = legs
                        .into_iter()
                        .map(|row| self.push(NodeKind::Leaf { row }, depth + 1, path))
                        .collect();
                    self.nodes[id].children = children;
                    out.push(id);
                }
                // An empty package keeps its place, with no legs.
                None if sheet.is_package(row) => {
                    let kind = NodeKind::Package {
                        row,
                        legs: Vec::new(),
                        split: false,
                        partial: false,
                    };
                    out.push(self.push(kind, depth, path));
                    i += 1;
                }
                None => {
                    out.push(self.push(NodeKind::Leaf { row }, depth, path));
                    i += 1;
                }
            }
        }
        out
    }
}

/// Every shown leg (and bare line) beneath `node`, in display order: a
/// group's for its sums, unanimity and totals; a package node's legs; a
/// leaf's own row. An empty package contributes nothing.
pub fn legs_under(rollup: &Rollup, node: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(id) = stack.pop() {
        let n = &rollup.nodes[id];
        if let NodeKind::Leaf { row } = n.kind {
            out.push(row);
        }
        stack.extend(n.children.iter().rev());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{at, push, result};
    use crate::core::shorthand::parse_builtin;
    use crate::core::visibility::apply_scope;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::pricing::Measure;
    use geode_core::scope::{Scope, parse_expr};

    /// Rows: 0 SPX Z26 4000 P, 1 CS, 2 and 3 its legs (4800 C, 5200 C),
    /// 4 NDX Z26 5000 C, 5 CAL, 6 and 7 its legs (H27 C, Z26 C), 8 SPX
    /// H27 4000 P.
    fn sheet() -> Sheet {
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
            .map(|l| parse_builtin(l).unwrap())
            .collect(),
        );
        assert_eq!(s.len(), 9);
        s
    }

    fn chain(levels: &[&str]) -> EffectiveChain {
        let levels: Vec<String> = levels.iter().map(|l| l.to_string()).collect();
        effective_chain(&levels, &DerivedDimensions::default())
    }

    fn roll(s: &Sheet, levels: &[&str]) -> Rollup {
        let vis = Visibility::all(s);
        build(
            s,
            &vis,
            &chain(levels),
            &DerivedDimensions::default(),
            Clock::utc(),
        )
    }

    /// One line per node, depth-first, indented by depth: a group by its
    /// label, a package `P<row> [legs]` with `split`/`partial` flags, a
    /// leaf `L<row>`.
    fn shape(r: &Rollup) -> Vec<String> {
        fn walk(r: &Rollup, id: usize, out: &mut Vec<String>) {
            let n = &r.nodes[id];
            let pad = "  ".repeat(n.depth);
            out.push(match &n.kind {
                NodeKind::Group { label, .. } => format!("{pad}{label}"),
                NodeKind::Package {
                    row,
                    legs,
                    split,
                    partial,
                } => format!(
                    "{pad}P{row} {legs:?}{}{}",
                    if *split { " split" } else { "" },
                    if *partial { " partial" } else { "" }
                ),
                NodeKind::Leaf { row } => format!("{pad}L{row}"),
            });
            for &c in &n.children {
                walk(r, c, out);
            }
        }
        let mut out = Vec::new();
        for &root in &r.roots {
            walk(r, root, &mut out);
        }
        out
    }

    /// The expected shape, one node per non-blank line, with the common
    /// indentation of the literal stripped.
    fn lines(text: &str) -> Vec<String> {
        let rows: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let common = rows
            .iter()
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        rows.iter().map(|l| l[common..].to_string()).collect()
    }

    fn dims(toml: &str) -> DerivedDimensions {
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", toml).unwrap()],
        );
        let (dims, diags) = DerivedDimensions::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        dims
    }

    fn group_node(r: &Rollup, label: &str) -> usize {
        r.nodes
            .iter()
            .position(|n| matches!(&n.kind, NodeKind::Group { label: l, .. } if l == label))
            .unwrap_or_else(|| panic!("no group {label}"))
    }

    fn root_labels(r: &Rollup) -> Vec<String> {
        r.roots
            .iter()
            .map(|&g| match &r.nodes[g].kind {
                NodeKind::Group { label, .. } => label.to_string(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn an_empty_chain_is_the_flat_sheet() {
        let s = sheet();
        let r = roll(&s, &[]);
        assert_eq!(
            shape(&r),
            lines(
                "
                L0
                P1 [2, 3]
                  L2
                  L3
                L4
                P5 [6, 7]
                  L6
                  L7
                L8
                "
            )
        );
        assert!(r.nodes.iter().all(|n| n.path.is_empty()));
    }

    /// Text orders by byte order (`NDX` before `SPX`), as the blotter
    /// sorts; within a group, lines keep sheet order.
    #[test]
    fn levels_group_shown_legs_by_value_nulls_last() {
        let s = sheet();
        let r = roll(&s, &["underlying_ref"]);
        assert_eq!(
            shape(&r),
            lines(
                "
                NDX
                  L4
                SPX
                  L0
                  P1 [2, 3]
                    L2
                    L3
                  P5 [6, 7]
                    L6
                    L7
                  L8
                "
            )
        );
        // A barrier line groups by its level; the vanillas' NULL is last.
        let mut s = sheet();
        push(
            &mut s,
            vec![parse_builtin("SPX Z26 5000 C UO 5500").unwrap()],
        );
        let r = roll(&s, &["barrier"]);
        let values: Vec<_> = r
            .roots
            .iter()
            .map(|&g| match &r.nodes[g].kind {
                NodeKind::Group { value, .. } => value.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(values, vec![Some(Value::F64(5500.0)), None]);
        assert_eq!(r.nodes[r.roots[0]].path, vec![Some("5500".to_string())]);
        assert_eq!(r.nodes[r.roots[1]].path, vec![None]);
        assert_eq!(legs_under(&r, r.roots[0]), vec![9]);
    }

    #[test]
    fn negative_zero_is_zero_and_nan_sorts_above_numbers() {
        assert_eq!(text(&zero(Value::F64(-0.0))), "0");
        assert_eq!(
            cmp_key(&Some(Value::F64(f64::NAN)), &Some(Value::F64(1e300))),
            Ordering::Greater
        );
        assert_eq!(
            cmp_key(&Some(Value::F64(f64::NAN)), &None),
            Ordering::Less,
            "NULL is still last"
        );
    }

    #[test]
    fn numbers_order_by_number_not_by_text() {
        let mut s = sheet();
        push(&mut s, vec![parse_builtin("SPX Z26 10000 C").unwrap()]);
        let r = roll(&s, &["strike"]);
        assert_eq!(
            root_labels(&r),
            vec!["4000", "4800", "5000", "5200", "10000"]
        );
    }

    /// Review Focus 2: the calendar's legs sit under different expiries,
    /// so the package appears under both, one leg each, `split`; the two
    /// nodes' sums add up to the package's.
    #[test]
    fn a_calendar_splits_under_expiry() {
        let mut s = sheet();
        let answers: Vec<_> = (0..s.len())
            .filter(|&r| s.is_line(r))
            .map(|r| (s.id(r), s.revision(r), Ok(result(r as f64 + 1.0))))
            .collect();
        s.deliver_all(answers, at(0));
        let r = roll(&s, &["expiry"]);
        assert_eq!(
            shape(&r),
            lines(
                "
                2026-12-18
                  L0
                  P1 [2, 3]
                    L2
                    L3
                  L4
                  P5 [7] split partial
                    L7
                2027-03-19
                  P5 [6] split partial
                    L6
                  L8
                "
            )
        );
        let npv = |legs: &[usize]| {
            s.fold_legs(legs.iter().copied())
                .result
                .unwrap()
                .get(Measure::Npv, false)
        };
        let parts: Vec<f64> = r
            .nodes
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::Package { row: 5, legs, .. } => Some(npv(legs)),
                _ => None,
            })
            .collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0] + parts[1], npv(&[6, 7]));
        assert_ne!(parts[0], parts[1], "each node sums its own leg");
    }

    #[test]
    fn position_ref_as_a_level_is_the_package_node() {
        let s = sheet();
        assert_eq!(roll(&s, &["position_ref"]), roll(&s, &[]));
        assert_eq!(
            roll(&s, &["underlying_ref", "position_ref"]),
            roll(&s, &["underlying_ref"])
        );
    }

    #[test]
    fn instrument_ref_as_a_level_makes_every_leg_a_leaf() {
        let s = sheet();
        assert_eq!(
            shape(&roll(&s, &["instrument_ref"])),
            lines("L0\nL2\nL3\nL4\nL6\nL7\nL8")
        );
        assert_eq!(
            shape(&roll(&s, &["underlying_ref", "instrument_ref"])),
            lines("NDX\n  L4\nSPX\n  L0\n  L2\n  L3\n  L6\n  L7\n  L8")
        );
    }

    #[test]
    fn a_dropped_level_is_reported_and_skipped() {
        let c = chain(&["lhu", "underlying_ref", "position_ref"]);
        assert_eq!(c.kept, vec!["underlying_ref", "position_ref"]);
        assert_eq!(c.dropped, vec!["lhu"]);
        let s = sheet();
        assert_eq!(
            roll(&s, &["lhu", "underlying_ref", "position_ref"]),
            roll(&s, &["underlying_ref"])
        );
        // A measure is not groupable; neither is a level past a
        // structural one (a line is the finest node the tree has).
        let c = chain(&["npv", "position_ref", "expiry", "instrument_ref"]);
        assert_eq!(c.kept, vec!["position_ref"]);
        assert_eq!(c.dropped, vec!["npv", "expiry", "instrument_ref"]);
        // A derived dimension over a pricer column is kept; over a
        // synthetic key or a column pricer lacks, dropped.
        let d = dims(
            "[index]\nfrom = \"underlying_ref\"\n[index.values]\nbroad = [\"SPX\"]\n\
             [desk_pos]\nfrom = \"position_ref\"\n[desk_pos.values]\nmine = [\"p1\"]\n\
             [region]\nfrom = \"lhu\"\n[region.values]\nemea = [\"L1\"]\n",
        );
        let levels: Vec<String> = ["index", "desk_pos", "region"]
            .iter()
            .map(|l| l.to_string())
            .collect();
        let c = effective_chain(&levels, &d);
        assert_eq!(c.kept, vec!["index"]);
        assert_eq!(c.dropped, vec!["desk_pos", "region"]);
    }

    /// A level named twice groups once: the repeat is reported dropped
    /// (the header strikes it through), not a second level whose every
    /// node has one child reading the same value as its parent.
    #[test]
    fn a_repeated_level_is_dropped() {
        let c = chain(&["underlying_ref", "expiry", "underlying_ref"]);
        assert_eq!(c.kept, vec!["underlying_ref", "expiry"]);
        assert_eq!(c.dropped, vec!["underlying_ref"]);
        let s = sheet();
        assert_eq!(
            roll(&s, &["underlying_ref", "underlying_ref"]),
            roll(&s, &["underlying_ref"])
        );
    }

    /// A scope hiding one CS leg: the CS sits under its node with its one
    /// shown leg — `partial` (fewer than all its legs), not `split` (every
    /// shown leg is here).
    #[test]
    fn hidden_legs_are_not_placed() {
        let s = sheet();
        let scope = Scope {
            expression: Some(parse_expr("strike != 4800").unwrap()),
            ..Scope::default()
        };
        let d = DerivedDimensions::default();
        let vis = apply_scope(&s, &scope, &d, Clock::utc()).unwrap();
        assert!(!vis.is_shown(2));
        let r = build(&s, &vis, &chain(&["underlying_ref"]), &d, Clock::utc());
        assert_eq!(
            shape(&r),
            lines(
                "
                NDX
                  L4
                SPX
                  L0
                  P1 [3] partial
                    L3
                  P5 [6, 7]
                    L6
                    L7
                  L8
                "
            )
        );
        assert_eq!(legs_under(&r, group_node(&r, "SPX")), vec![0, 3, 6, 7, 8]);
        // Flat, the same package is partial too.
        let r = build(&s, &vis, &chain(&[]), &d, Clock::utc());
        assert_eq!(shape(&r)[1], "P1 [3] partial");
    }

    #[test]
    fn expiry_groups_by_its_scope_value() {
        let s = sheet();
        let r = roll(&s, &["expiry"]);
        assert_eq!(
            r.nodes[r.roots[0]].kind,
            NodeKind::Group {
                level: 0,
                column: "expiry".into(),
                value: Some(Value::Utf8("2026-12-18".into())),
                label: "2026-12-18".into(),
            }
        );
    }

    #[test]
    fn a_derived_dimension_groups_by_its_label() {
        let s = sheet();
        let d = dims(
            "[index]\nfrom = \"underlying_ref\"\n[index.values]\nbroad = [\"SPX\"]\ntech = [\"NDX\"]\n",
        );
        let c = effective_chain(&["index".to_string()], &d);
        let r = build(&s, &Visibility::all(&s), &c, &d, Clock::utc());
        assert_eq!(root_labels(&r), vec!["broad", "tech"]);
        assert_eq!(legs_under(&r, group_node(&r, "tech")), vec![4]);
        // An empty label is a value, distinct from an unmapped source's
        // NULL, which sorts last.
        let d = dims("[index]\nfrom = \"underlying_ref\"\n[index.values]\n\"\" = [\"SPX\"]\n");
        let c = effective_chain(&["index".to_string()], &d);
        let r = build(&s, &Visibility::all(&s), &c, &d, Clock::utc());
        let roots: Vec<_> = r
            .roots
            .iter()
            .map(|&g| (r.nodes[g].path.clone(), legs_under(&r, g)))
            .collect();
        assert_eq!(
            roots,
            vec![
                (vec![Some(String::new())], vec![0, 2, 3, 6, 7, 8]),
                (vec![None], vec![4]),
            ]
        );
    }

    #[test]
    fn paths_are_values_root_to_node() {
        let s = sheet();
        let r = roll(&s, &["underlying_ref", "expiry"]);
        let spx = group_node(&r, "SPX");
        assert_eq!(r.nodes[spx].path, vec![Some("SPX".to_string())]);
        assert_eq!(r.nodes[spx].depth, 0);
        let h27 = r.nodes[spx].children[1];
        assert_eq!(
            r.nodes[h27].path,
            vec![Some("SPX".to_string()), Some("2027-03-19".to_string())]
        );
        assert_eq!(r.nodes[h27].depth, 1);
        assert!(matches!(
            r.nodes[h27].kind,
            NodeKind::Group { level: 1, .. }
        ));
        // A package or leaf carries its group's path (packages expand by
        // LineId, not by path).
        let under = r.nodes[h27].children[0];
        assert!(matches!(
            r.nodes[under].kind,
            NodeKind::Package { row: 5, .. }
        ));
        assert_eq!(r.nodes[under].path, r.nodes[h27].path);
        assert_eq!(r.nodes[under].depth, 2);
        assert_eq!(legs_under(&r, h27), vec![6, 8]);
    }

    /// An empty package (its legs all removed) is shown and keeps its
    /// place: in sheet order when flat, and under the values its own row
    /// reads when grouped (NULL for an instrument column).
    #[test]
    fn an_empty_package_keeps_its_place() {
        let mut s = sheet();
        s.apply(Edit::Remove { at: 3 }).unwrap();
        s.apply(Edit::Remove { at: 2 }).unwrap();
        assert_eq!(s.children(1), 2..2);
        assert_eq!(
            shape(&roll(&s, &[])),
            lines("L0\nP1 []\nL2\nP3 [4, 5]\n  L4\n  L5\nL6")
        );
        let r = roll(&s, &["underlying_ref"]);
        let null = r.roots[2];
        assert_eq!(r.nodes[null].path, vec![None]);
        assert_eq!(shape(&r).last().unwrap(), "  P1 []");
        assert!(legs_under(&r, null).is_empty());
        let r = roll(&s, &["template"]);
        assert!(legs_under(&r, group_node(&r, "CS")).is_empty());
    }
}
