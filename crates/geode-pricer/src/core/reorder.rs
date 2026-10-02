//! Where a moved row may land, for the keys and the pointer alike.
//!
//! A move is among the row's siblings in sheet order (the roots, or a
//! package's legs). Under a value grouping the painted order is sheet
//! order only WITHIN each rollup group node, so a move is further held to
//! the siblings painted in the mover's own group: [`Placements::same_node`]
//! is the predicate both `move_plan` (the keys' one step) and [`DropPlan`]
//! (the pointer's drop) step by. Moving only the movers, past siblings of
//! other groups, leaves every other group's painted order as it was.
//!
//! [`DropPlan`] resolves a pointer gap between painted grid rows to the
//! edits that land the movers there: one `Edit::Move` per mover that is
//! not already in place, applied as one undo entry. Pure: no I/O, no
//! entities.

use crate::core::edit::Edit;
use crate::core::rollup::{NodeKind, Rollup};
use crate::core::sheet::Sheet;
use geode_core::expansion::Path;
use std::collections::HashMap;
use std::ops::Range;

/// A move whose rows paint under different group nodes: there is no one
/// group order to move them in.
pub const SPANS_GROUPS: &str = "can't move: selection spans groups";

/// The enclosing group path of every rollup node that places each sheet
/// row: one entry for a row painted once, one per node for a package the
/// grouping splits, none for a row the scope hides.
pub struct Placements<'a> {
    by_row: HashMap<usize, Vec<&'a Path>>,
}

impl<'a> Placements<'a> {
    pub fn of(rollup: &'a Rollup) -> Self {
        let mut by_row: HashMap<usize, Vec<&'a Path>> = HashMap::new();
        for n in &rollup.nodes {
            match &n.kind {
                NodeKind::Package { row, .. } | NodeKind::Leaf { row } => {
                    by_row.entry(*row).or_default().push(&n.path);
                }
                NodeKind::Group { .. } => {}
            }
        }
        Placements { by_row }
    }

    /// The one group path `row` paints under: `None` when it paints
    /// nowhere (hidden) or under more than one node (a split package).
    pub fn group(&self, row: usize) -> Option<&'a Path> {
        match self.by_row.get(&row).map(Vec::as_slice) {
            Some([one]) => Some(*one),
            _ => None,
        }
    }

    /// The "same node" rule for `movers`: a sibling may be landed beside
    /// when some node placing it sits in the movers' one group. Refused
    /// ([`SPANS_GROUPS`]) when the movers do not share exactly one group,
    /// or one of them paints in none or several.
    pub fn same_node(self, movers: &[usize]) -> Result<impl Fn(usize) -> bool + 'a, &'static str> {
        let mut groups = movers.iter().map(|&r| self.group(r));
        let Some(Some(path)) = groups.next() else {
            return Err(SPANS_GROUPS);
        };
        if groups.any(|g| g != Some(path)) {
            return Err(SPANS_GROUPS);
        }
        Ok(move |row: usize| {
            self.by_row
                .get(&row)
                .is_some_and(|paths| paths.contains(&path))
        })
    }
}

/// A sibling a drag may land beside, or one of the movers, with the grid
/// rows it paints in the movers' group: its own row, and an open
/// package's legs beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub row: usize,
    pub span: Range<usize>,
}

/// Where a drag of `movers` (top-most rows sharing a parent and a group)
/// can drop. `stops` are the movers and every sibling the move's `lands`
/// predicate accepts, in sibling order, each with its painted span. The
/// legal gaps are the edges of those spans: dropping inside a package's
/// legs, or among another group's rows, has no meaning in this sibling
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropPlan {
    pub movers: Vec<usize>,
    pub stops: Vec<Stop>,
}

/// Where the movers go relative to the siblings that stay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    After(usize),
    Before(usize),
}

impl DropPlan {
    /// The grid rows the stops span together: a pointer outside it is
    /// over rows of another sibling set or group, and drops nowhere.
    fn region(&self) -> Option<Range<usize>> {
        let lo = self.stops.iter().map(|s| s.span.start).min()?;
        let hi = self.stops.iter().map(|s| s.span.end).max()?;
        Some(lo..hi)
    }

    fn legal(&self, gap: usize) -> bool {
        self.stops
            .iter()
            .any(|s| s.span.start == gap || s.span.end == gap)
    }

    /// The legal gap nearest the pointer over grid row `row` (its lower
    /// half when `lower`): `None` when the row lies outside the stops'
    /// region. A tie goes the way the pointer's half leans.
    pub fn snap(&self, row: usize, lower: bool) -> Option<usize> {
        let region = self.region()?;
        if !region.contains(&row) {
            return None;
        }
        let raw = row + usize::from(lower);
        (region.start..=region.end)
            .filter(|&g| self.legal(g))
            .min_by_key(|&g| (g.abs_diff(raw), if lower { raw > g } else { g > raw }))
    }

    /// The edits that land the movers at grid gap `gap`, as one batch:
    /// each mover not already in place moves once, straight to its slot
    /// beside the sibling that stays, so the siblings that do not move
    /// (hidden ones and other groups' among them) keep their order.
    /// `None` when `gap` is not a legal gap, or when the movers already
    /// sit there.
    pub fn edits(&self, sheet: &Sheet, gap: usize) -> Option<Vec<Edit>> {
        let first = *self.movers.first()?;
        let mover = |r: usize| self.movers.contains(&r);
        // The insertion point among the stops: before the stop the gap
        // starts, or after the one it ends.
        let at = self
            .stops
            .iter()
            .position(|s| s.span.start == gap)
            .or_else(|| {
                self.stops
                    .iter()
                    .position(|s| s.span.end == gap)
                    .map(|k| k + 1)
            })?;
        let before = self.stops[..at].iter().rev().find(|s| !mover(s.row));
        let after = self.stops[at..].iter().find(|s| !mover(s.row));
        let mut order = sheet.siblings(first);
        let pos = |order: &[usize], r: usize| order.iter().position(|&s| s == r);
        let mut movers: Vec<usize> = self.movers.clone();
        movers.sort_by_key(|&m| pos(&order, m));
        let lo = before.and_then(|s| pos(&order, s.row));
        let hi = after.and_then(|s| pos(&order, s.row));
        let in_place = movers.iter().all(|&m| {
            let p = pos(&order, m);
            lo.is_none_or(|lo| p > Some(lo)) && hi.is_none_or(|hi| p < Some(hi))
        });
        if in_place {
            return None;
        }
        let mut anchor = match (before, after) {
            (Some(s), _) => Anchor::After(s.row),
            (None, Some(s)) => Anchor::Before(s.row),
            (None, None) => return None,
        };
        let size = |r: usize| sheet.block(r).len();
        let base = sheet.parent(first).map_or(0, |p| p + 1);
        let mut edits = Vec::new();
        for m in movers {
            let j = pos(&order, m)?;
            let t = match anchor {
                Anchor::After(a) => {
                    let a = pos(&order, a)?;
                    if j < a { a } else { a + 1 }
                }
                Anchor::Before(b) => {
                    let b = pos(&order, b)?;
                    if j < b { b - 1 } else { b }
                }
            };
            if t != j {
                let row = base + order[..j].iter().map(|&r| size(r)).sum::<usize>();
                edits.push(Edit::Move {
                    row,
                    delta: t as isize - j as isize,
                });
                let m = order.remove(j);
                order.insert(t, m);
            }
            anchor = Anchor::After(m);
        }
        Some(edits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rollup::{build, effective_chain};
    use crate::core::select::{OFF_END, OFF_GROUP_END, move_plan};
    use crate::core::sheet::tests::push;
    use crate::core::shorthand::parse_builtin;
    use crate::core::visibility::Visibility;
    use geode_core::clock::Clock;
    use geode_core::dimensions::DerivedDimensions;

    fn sheet(lines: &[&str]) -> Sheet {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            lines.iter().map(|l| parse_builtin(l).unwrap()).collect(),
        );
        s
    }

    fn roll(s: &Sheet, levels: &[&str]) -> Rollup {
        let levels: Vec<String> = levels.iter().map(|l| l.to_string()).collect();
        let dims = DerivedDimensions::default();
        build(
            s,
            &Visibility::all(s),
            &effective_chain(&levels, &dims),
            &dims,
            Clock::utc(),
        )
    }

    fn texts(s: &Sheet) -> Vec<String> {
        s.roots().map(|r| s.shorthand(r)).collect()
    }

    /// The four interleaved lines the grouped tests share: SPX, NDX,
    /// SPX, NDX, so each underlying's group paints rows 0/2 and 1/3.
    const INTERLEAVED: [&str; 4] = [
        "SPX Z26 4000 P",
        "NDX Z26 5000 C",
        "SPX Z26 4200 P",
        "NDX Z26 5200 C",
    ];

    #[test]
    fn a_grouped_step_hops_other_groups_and_stops_at_the_groups_end() {
        let mut s = sheet(&INTERLEAVED);
        let r = roll(&s, &["underlying_ref"]);
        let p = Placements::of(&r);
        let lands = p.same_node(&[0]).unwrap();
        let edit = move_plan(&s, &[0], true, &lands, OFF_GROUP_END).unwrap();
        assert_eq!(edit, Edit::Move { row: 2, delta: -2 });
        s.apply(edit).unwrap();
        assert_eq!(
            texts(&s),
            [
                "SPX Z26 4200 P",
                "SPX Z26 4000 P",
                "NDX Z26 5000 C",
                "NDX Z26 5200 C"
            ],
            "SPX swapped; NDX keeps 5000 before 5200"
        );
        let r = roll(&s, &["underlying_ref"]);
        let p = Placements::of(&r);
        let lands = p.same_node(&[1]).unwrap();
        assert_eq!(
            move_plan(&s, &[1], true, &lands, OFF_GROUP_END),
            Err(OFF_GROUP_END),
            "the next siblings paint under NDX"
        );
        // Ungrouped, the same row steps onto the NDX line.
        assert_eq!(
            move_plan(&s, &[1], true, |_| true, OFF_END),
            Ok(Edit::Move { row: 2, delta: -1 })
        );
    }

    #[test]
    fn same_node_refuses_movers_in_two_groups_and_a_split_package() {
        let s = sheet(&INTERLEAVED);
        let r = roll(&s, &["underlying_ref"]);
        assert!(matches!(
            Placements::of(&r).same_node(&[0, 1]),
            Err(SPANS_GROUPS)
        ));
        assert!(Placements::of(&r).same_node(&[0, 2]).is_ok());
        // A calendar under expiry paints under both dates: split.
        let s = sheet(&["SPX Z26/H27 5000 CAL"]);
        let r = roll(&s, &["expiry"]);
        let p = Placements::of(&r);
        assert_eq!(p.group(0), None, "split");
        assert!(p.group(1).is_some(), "each leg paints once");
        assert!(matches!(p.same_node(&[0]), Err(SPANS_GROUPS)));
    }

    /// Flat stops: one per root, each one grid row.
    fn flat(s: &Sheet, movers: &[usize]) -> DropPlan {
        let mut g = 0;
        let stops = s
            .roots()
            .map(|r| {
                let span = g..g + 1;
                g += 1;
                Stop { row: r, span }
            })
            .collect();
        DropPlan {
            movers: movers.to_vec(),
            stops,
        }
    }

    fn land(s: &mut Sheet, edits: Vec<Edit>) {
        for e in edits {
            s.apply(e).unwrap();
        }
    }

    #[test]
    fn a_single_row_drop_is_one_move_and_its_own_gaps_are_no_ops() {
        let five = ["SPX Z26 1 P", "SPX Z26 2 P", "SPX Z26 3 P", "SPX Z26 4 P"];
        let mut s = sheet(&five);
        let plan = flat(&s, &[0]);
        assert_eq!(plan.edits(&s, 0), None, "above itself");
        assert_eq!(plan.edits(&s, 1), None, "below itself");
        let edits = plan.edits(&s, 3).unwrap();
        assert_eq!(edits, vec![Edit::Move { row: 0, delta: 2 }]);
        land(&mut s, edits);
        assert_eq!(
            texts(&s),
            ["SPX Z26 2 P", "SPX Z26 3 P", "SPX Z26 1 P", "SPX Z26 4 P"]
        );
        // Up, to the top.
        let plan = flat(&s, &[3]);
        let edits = plan.edits(&s, 0).unwrap();
        assert_eq!(edits, vec![Edit::Move { row: 3, delta: -3 }]);
        land(&mut s, edits);
        assert_eq!(texts(&s)[0], "SPX Z26 4 P");
        assert_eq!(flat(&s, &[0]).edits(&s, 9), None, "not a gap");
    }

    #[test]
    fn a_block_drop_keeps_its_order_and_lands_whole() {
        let mut s = sheet(&["SPX Z26 1 P", "SPX Z26 2 P", "SPX Z26 3 P", "SPX Z26 4 P"]);
        let plan = flat(&s, &[0, 1]);
        assert_eq!(plan.edits(&s, 2), None, "already there");
        let edits = plan.edits(&s, 4).unwrap();
        assert_eq!(edits.len(), 2, "one move per mover");
        land(&mut s, edits);
        assert_eq!(
            texts(&s),
            ["SPX Z26 3 P", "SPX Z26 4 P", "SPX Z26 1 P", "SPX Z26 2 P"]
        );
        let plan = flat(&s, &[2, 3]);
        let edits = plan.edits(&s, 1).unwrap();
        land(&mut s, edits);
        assert_eq!(
            texts(&s),
            ["SPX Z26 3 P", "SPX Z26 1 P", "SPX Z26 2 P", "SPX Z26 4 P"]
        );
    }

    #[test]
    fn a_package_drop_counts_its_legs_and_a_leg_moves_among_legs() {
        // roots: 0 A; 1 CS (legs 2, 3); 4 B. Open package spans two more
        // grid rows; a drop inside its legs is not a gap.
        let mut s = sheet(&["SPX Z26 1 P", "SPX Z26 4800/5200 CS", "SPX Z26 2 P"]);
        let plan = DropPlan {
            movers: vec![4],
            stops: vec![
                Stop { row: 0, span: 0..1 },
                Stop { row: 1, span: 1..4 },
                Stop { row: 4, span: 4..5 },
            ],
        };
        assert_eq!(plan.snap(2, false), Some(1), "inside the legs snaps out");
        assert_eq!(plan.snap(2, true), Some(4));
        assert_eq!(plan.edits(&s, 2), None, "inside the legs");
        let edits = plan.edits(&s, 1).unwrap();
        assert_eq!(edits, vec![Edit::Move { row: 4, delta: -1 }]);
        land(&mut s, edits);
        assert_eq!(s.shorthand(1), "SPX Z26 2 P");
        assert!(s.is_package(2));
        // A leg among its package's legs (rows 3, 4 now).
        let plan = DropPlan {
            movers: vec![3],
            stops: vec![Stop { row: 3, span: 2..3 }, Stop { row: 4, span: 3..4 }],
        };
        assert_eq!(plan.snap(0, false), None, "outside the package");
        let leg = s.shorthand(3);
        let edits = plan.edits(&s, 4).unwrap();
        land(&mut s, edits);
        assert_eq!(s.shorthand(4), leg);
    }

    #[test]
    fn a_grouped_drop_hops_other_groups_and_leaves_their_order() {
        // Grouped by underlying: SPX paints rows 0, 2 at grid 1, 2; NDX
        // rows 1, 3 at grid 4, 5 (group rows at 0 and 3).
        let mut s = sheet(&INTERLEAVED);
        let plan = DropPlan {
            movers: vec![0],
            stops: vec![Stop { row: 0, span: 1..2 }, Stop { row: 2, span: 2..3 }],
        };
        assert_eq!(plan.snap(4, false), None, "over the other group");
        assert_eq!(plan.snap(2, true), Some(3));
        let edits = plan.edits(&s, 3).unwrap();
        land(&mut s, edits);
        assert_eq!(
            texts(&s),
            [
                "NDX Z26 5000 C",
                "SPX Z26 4200 P",
                "SPX Z26 4000 P",
                "NDX Z26 5200 C"
            ]
        );
        let r = roll(&s, &["underlying_ref"]);
        let p = Placements::of(&r);
        let ndx: Vec<usize> = (0..4).filter(|&q| p.group(q) == p.group(0)).collect();
        assert_eq!(ndx, [0, 3], "NDX keeps 5000 before 5200");
    }
}
