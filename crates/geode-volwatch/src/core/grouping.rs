//! Grouping over the member rows: which of the frame's chain applies to a
//! list of underlyings, how the header names it, the pin a `:group` holds,
//! and the preorder tree of averaged group rows the grid shows.
//!
//! A vol row is one underlying, so the only levels that can group it are
//! `underlying_ref` itself (the leaf) and a classification over it. The
//! rest of a chain (`lhu`, `position_ref`, a classification over `book`)
//! is skipped and named, never silently dropped: a header reading
//! `region` while the frame says `region / lhu` would be a plausible wrong
//! grouping.

use geode_core::dimensions::DerivedDimensions;
use geode_core::link::UNDERLYING;

/// The group a member the classification does not map falls under: the
/// blotter's absent group, last among its siblings.
pub const UNMAPPED: &str = "\u{2014}";

/// The prefix of a group row's name (its `GridRow` identity): no
/// underlying is spelled `group:…`, so a group never collides with a
/// member in the cursor's or the collapsed set's names.
pub const GROUP_PREFIX: &str = "group:";

/// The chain after dropping what cannot group a vol row: `kept` in chain
/// order, `skipped` in chain order, for the header.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Applicable {
    pub kept: Vec<String>,
    pub skipped: Vec<String>,
}

/// Keep `underlying_ref` (the leaf, which closes the chain: nothing after
/// it applies) and every classification whose `from` is `underlying_ref`,
/// each once; skip the rest and name it. A chain of nothing applicable, or
/// of `underlying_ref` alone, is the flat list.
pub fn applicable(chain: &[String], dims: &DerivedDimensions) -> Applicable {
    let mut out = Applicable::default();
    let mut ended = false;
    for name in chain {
        if ended {
            out.skipped.push(name.clone());
            continue;
        }
        if name == UNDERLYING {
            ended = true;
            continue;
        }
        let over_underlying = dims.get(name).is_some_and(|d| d.from == UNDERLYING);
        if over_underlying && !out.kept.contains(name) {
            out.kept.push(name.clone());
        } else {
            out.skipped.push(name.clone());
        }
    }
    out
}

/// What `:group` completes: the leaf first, then every classification over
/// it, sorted.
pub fn vocabulary(dims: &DerivedDimensions) -> Vec<String> {
    let mut classes: Vec<String> = dims
        .all()
        .filter(|d| d.from == UNDERLYING)
        .map(|d| d.name.clone())
        .collect();
    classes.sort();
    classes.dedup();
    let mut out = vec![UNDERLYING.to_string()];
    out.extend(classes);
    out
}

/// The tile's grouping override. `None` follows the frame; `Grouping` and
/// `Flat` stand whatever the frame does; `Slot` re-reads its slot when the
/// slot changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pin {
    #[default]
    None,
    Grouping(Vec<String>),
    Slot(u8),
    Flat,
}

/// The header's grouping chip: the kept levels joined by ` · `, the
/// skipped ones named (`lhu skipped` beside a grouping, `lhu not
/// applicable` under `ungrouped`), `pinned ` ahead while a pin holds.
pub fn chip_text(pin: &Pin, a: &Applicable) -> String {
    let kept = a.kept.join(" \u{b7} ");
    let skipped = a.skipped.join(", ");
    let body = match (a.kept.is_empty(), a.skipped.is_empty()) {
        (false, true) => kept,
        (false, false) => format!("{kept} \u{b7} {skipped} skipped"),
        (true, true) => "ungrouped".to_string(),
        (true, false) => format!("ungrouped \u{b7} {skipped} not applicable"),
    };
    match pin {
        Pin::None => body,
        _ => format!("pinned {body}"),
    }
}

/// A group row's name from its path (one label per kept level, the
/// group's own last).
pub fn group_name(path: &[String]) -> String {
    format!("{GROUP_PREFIX}{}", path.join("/"))
}

use geode_core::classification::labels;
use geode_core::dimensions::DerivedDimension;
use geode_core::volboard::{Cell, Measure};

use super::lens::z_score;
use super::model::Row;
use super::views::Ask;

/// One measure's aggregate over a group's members: the arithmetic mean of
/// the members' levels, of their changes, of their z-scores and of their
/// percentile ranks, each over the members that have one. A z is the mean
/// of the members' z, never the z of the mean level: the group's
/// companion answers "how rich are these names on average".
#[derive(Debug, Clone, PartialEq)]
pub struct GroupCell {
    pub measure: Measure,
    pub level: Option<f64>,
    pub delta: Option<f64>,
    pub z: Option<f64>,
    pub pctl: Option<f64>,
}

/// A group row's own data.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupInfo {
    /// The classification label (`UNMAPPED` for members it does not map).
    pub label: String,
    /// The member names under it (through every nested group), list order.
    pub members: Vec<String>,
    /// How many of them the board holds.
    pub present: usize,
    /// Parallel to the ask's `measures`.
    pub cells: Vec<GroupCell>,
}

/// The arithmetic mean; `None` over nothing.
pub fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (mut n, mut sum) = (0usize, 0.0f64);
    for v in values {
        n += 1;
        sum += v;
    }
    (n > 0).then(|| sum / n as f64)
}

/// A group row's own path (its enclosing path plus its label); a
/// member's enclosing path.
pub fn full_path(row: &Row) -> Vec<String> {
    let mut p = row.path.clone();
    if let Some(g) = &row.group {
        p.push(g.label.clone());
    }
    p
}

/// Arrange `leaves` (list order) under `kept` (from [`applicable`] over
/// the same `dims`) as a preorder tree: at each level a group row per
/// label in the order `classification::labels` lists them, `UNMAPPED`
/// last, then its members (or the next level's groups). Members keep
/// list order under their group. A kept name `dims` lacks is skipped (it
/// cannot happen through `applicable`; a stale name groups nothing rather
/// than panics). An empty `kept` returns `leaves` unchanged.
pub fn group_rows(
    leaves: Vec<Row>,
    kept: &[String],
    dims: &DerivedDimensions,
    asked: &Ask,
) -> Vec<Row> {
    let levels: Vec<&DerivedDimension> = kept.iter().filter_map(|k| dims.get(k)).collect();
    if levels.is_empty() {
        return leaves;
    }
    let keyed: Vec<(Vec<String>, Row)> = leaves
        .into_iter()
        .map(|r| {
            let path = levels
                .iter()
                .map(|d| {
                    d.values
                        .get(&r.name)
                        .cloned()
                        .unwrap_or_else(|| UNMAPPED.to_string())
                })
                .collect();
            (path, r)
        })
        .collect();
    let mut out = Vec::with_capacity(keyed.len() * 2);
    place(&mut out, keyed, 0, &levels, &[], asked);
    out
}

/// Place `items` from `level` down under `path`.
fn place(
    out: &mut Vec<Row>,
    mut items: Vec<(Vec<String>, Row)>,
    level: usize,
    levels: &[&DerivedDimension],
    path: &[String],
    asked: &Ask,
) {
    if level == levels.len() {
        for (_, mut r) in items {
            r.path = path.to_vec();
            out.push(r);
        }
        return;
    }
    let order = labels(levels[level]);
    let rank = |l: &str| {
        if l == UNMAPPED {
            usize::MAX
        } else {
            order.iter().position(|o| o == l).unwrap_or(usize::MAX - 1)
        }
    };
    let mut seen: Vec<String> = items.iter().map(|(p, _)| p[level].clone()).collect();
    seen.sort_by_key(|l| rank(l));
    seen.dedup();
    for label in seen {
        let (mine, rest): (Vec<_>, Vec<_>) =
            items.into_iter().partition(|(p, _)| p[level] == label);
        items = rest;
        let leaves: Vec<&Row> = mine.iter().map(|(_, r)| r).collect();
        out.push(group_row(&label, path, &leaves, asked));
        let mut full = path.to_vec();
        full.push(label);
        place(out, mine, level + 1, levels, &full, asked);
    }
}

/// The group row over `leaves` (every member beneath it, through any
/// nested level), labelled `label` under `path`.
fn group_row(label: &str, path: &[String], leaves: &[&Row], asked: &Ask) -> Row {
    let mut full = path.to_vec();
    full.push(label.to_string());
    let present: Vec<&Row> = leaves
        .iter()
        .copied()
        .filter(|r| r.present && r.board.is_some())
        .collect();
    /// The row's cell at the ask's position, only while it is the asked
    /// measure (a board answered for another ask is not averaged).
    fn cell_of<'r>(r: &'r Row, pos: usize, m: &Measure) -> Option<&'r Cell> {
        r.board.as_ref()?.cells.get(pos).filter(|c| c.measure == *m)
    }
    let finite = |v: f64| v.is_finite().then_some(v);
    let cells = asked
        .measures
        .iter()
        .enumerate()
        .map(|(pos, m)| GroupCell {
            measure: m.clone(),
            level: mean(
                present
                    .iter()
                    .filter_map(|r| cell_of(r, pos, m)?.current)
                    .filter_map(finite),
            ),
            delta: mean(
                present
                    .iter()
                    .filter_map(|r| Some(cell_of(r, pos, m)?.changes.first()?.as_ref()?.value))
                    .filter_map(finite),
            ),
            z: mean(present.iter().filter_map(|r| z_score(cell_of(r, pos, m)?))),
            pctl: mean(
                present
                    .iter()
                    .filter_map(|r| Some(cell_of(r, pos, m)?.pctl?.0))
                    .filter_map(finite),
            ),
        })
        .collect();
    let vegas: Vec<f64> = leaves
        .iter()
        .filter_map(|r| r.vega)
        .filter(|v| v.is_finite())
        .collect();
    Row {
        name: group_name(&full),
        reference: None,
        present: true,
        as_of: None,
        board: None,
        vega: (!vegas.is_empty()).then(|| vegas.iter().sum()),
        path: path.to_vec(),
        group: Some(GroupInfo {
            label: label.to_string(),
            members: leaves.iter().map(|r| r.name.clone()).collect(),
            present: present.len(),
            cells,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{LayerDoc, merge_docs};

    fn dims(toml: &str) -> DerivedDimensions {
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin("dimensions", toml).unwrap()],
        );
        let (dims, diags) = DerivedDimensions::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        dims
    }

    /// `region` and `sector` over the underlying; `desk` over `book`.
    pub(super) fn europe() -> DerivedDimensions {
        dims(
            r#"
[region]
from = "underlying_ref"
[region.values]
Core = ["DAX", "SX5E"]
Isles = ["UKX"]

[sector]
from = "underlying_ref"
[sector.values]
Index = ["DAX", "SX5E", "UKX"]

[desk]
from = "book"
[desk.values]
Flow = ["b1"]
"#,
        )
    }

    fn chain(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn the_chain_keeps_only_underlying_ref_classifications_in_order() {
        let a = applicable(&chain(&["region", "lhu", "sector"]), &europe());
        assert_eq!(a.kept, ["region", "sector"]);
        assert_eq!(a.skipped, ["lhu"]);
        let a = applicable(&chain(&["desk", "position_ref"]), &europe());
        assert!(a.kept.is_empty());
        assert_eq!(a.skipped, ["desk", "position_ref"]);
    }

    #[test]
    fn underlying_ref_closes_the_chain_and_a_repeat_is_skipped() {
        let a = applicable(&chain(&["region", "underlying_ref", "sector"]), &europe());
        assert_eq!(a.kept, ["region"]);
        assert_eq!(a.skipped, ["sector"], "after the leaf nothing applies");
        let a = applicable(&chain(&["region", "region"]), &europe());
        assert_eq!(a.kept, ["region"]);
        assert_eq!(a.skipped, ["region"]);
        let a = applicable(&chain(&["underlying_ref"]), &europe());
        assert!(a.kept.is_empty() && a.skipped.is_empty(), "the flat list");
    }

    #[test]
    fn a_dimension_the_classifications_no_longer_hold_is_skipped() {
        let a = applicable(&chain(&["region", "country"]), &europe());
        assert_eq!(a.kept, ["region"]);
        assert_eq!(a.skipped, ["country"]);
    }

    #[test]
    fn the_vocabulary_is_underlying_ref_then_the_classifications_over_it() {
        assert_eq!(
            vocabulary(&europe()),
            ["underlying_ref", "region", "sector"]
        );
        assert_eq!(
            vocabulary(&DerivedDimensions::default()),
            ["underlying_ref"]
        );
    }

    #[test]
    fn the_chip_names_the_kept_the_skipped_and_the_pin() {
        let a = |k: &[&str], s: &[&str]| Applicable {
            kept: chain(k),
            skipped: chain(s),
        };
        assert_eq!(chip_text(&Pin::None, &a(&["region"], &[])), "region");
        assert_eq!(
            chip_text(&Pin::None, &a(&["region", "sector"], &[])),
            "region \u{b7} sector"
        );
        assert_eq!(
            chip_text(&Pin::None, &a(&["region"], &["lhu"])),
            "region \u{b7} lhu skipped"
        );
        assert_eq!(
            chip_text(&Pin::None, &a(&[], &["lhu", "position_ref"])),
            "ungrouped \u{b7} lhu, position_ref not applicable"
        );
        assert_eq!(chip_text(&Pin::None, &a(&[], &[])), "ungrouped");
        assert_eq!(
            chip_text(&Pin::Grouping(chain(&["region"])), &a(&["region"], &[])),
            "pinned region"
        );
        assert_eq!(
            chip_text(&Pin::Slot(2), &a(&["region"], &[])),
            "pinned region"
        );
        assert_eq!(chip_text(&Pin::Flat, &a(&[], &[])), "pinned ungrouped");
    }

    #[test]
    fn a_group_name_is_its_path_behind_the_prefix() {
        assert_eq!(group_name(&chain(&["Core"])), "group:Core");
        assert_eq!(group_name(&chain(&["Core", "Index"])), "group:Core/Index");
        assert_eq!(group_name(&chain(&[UNMAPPED])), "group:\u{2014}");
    }

    use crate::core::model::Row;
    use crate::core::views::Ask;
    use chrono::Utc;
    use geode_core::volboard::{Cell, Change, Measure, Stats, VolRow};
    use geode_tile::rows::GridRow;

    fn asked() -> Ask {
        Ask {
            measures: vec![Measure::atfm("1M"), Measure::atfm("3M")],
            window: Some(std::time::Duration::from_secs(365 * 86_400)),
            ..Ask::default()
        }
    }

    /// A leaf whose two cells read `level`, a change of `delta`, a rank
    /// of `rank` over 251 observations with mean 14.5 and sd 2.0 (so z =
    /// (level − 14.5) / 2). `None` level: the member is not in the surface.
    fn leaf(name: &str, level: Option<f64>, delta: f64, rank: f64, vega: Option<f64>) -> Row {
        let cells = asked()
            .measures
            .iter()
            .map(|m| Cell {
                measure: m.clone(),
                current: level,
                current_at: level.map(|_| Utc::now()),
                changes: vec![level.map(|_| Change {
                    value: delta,
                    from: 0.0,
                    at: Utc::now(),
                })],
                pctl: level.map(|_| (rank, 251)),
                stats: level.map(|_| Stats {
                    mean: 14.5,
                    sd: 2.0,
                    count: 251,
                }),
            })
            .collect();
        Row {
            name: name.to_string(),
            reference: None,
            present: level.is_some(),
            as_of: level.map(|_| Utc::now()),
            board: Some(VolRow {
                underlying: name.to_string(),
                as_of: level.map(|_| Utc::now()),
                cells,
                sparks: Vec::new(),
            }),
            vega,
            path: Vec::new(),
            group: None,
        }
    }

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| r.name.clone()).collect()
    }

    fn group<'r>(rows: &'r [Row], name: &str) -> &'r GroupInfo {
        rows.iter()
            .find(|r| r.name == name)
            .and_then(|r| r.group.as_ref())
            .unwrap_or_else(|| panic!("no group {name}"))
    }

    #[test]
    fn an_empty_chain_is_the_flat_list() {
        let leaves = vec![leaf("DAX", Some(16.5), 1.0, 0.9, Some(10.0))];
        let rows = group_rows(leaves.clone(), &[], &europe(), &asked());
        assert_eq!(rows, leaves);
    }

    #[test]
    fn groups_follow_the_labels_order_members_keep_list_order_and_unmapped_falls_last() {
        let leaves = vec![
            leaf("UKX", Some(12.5), -0.5, 0.2, Some(1.0)),
            leaf("SX5E", Some(16.5), 1.0, 0.9, Some(2.0)),
            leaf("NEW", None, 0.0, 0.0, None),
            leaf("DAX", Some(14.5), 0.5, 0.5, Some(3.0)),
        ];
        let rows = group_rows(leaves, &["region".into()], &europe(), &asked());
        assert_eq!(
            names(&rows),
            [
                "group:Core",
                "SX5E",
                "DAX",
                "group:Isles",
                "UKX",
                "group:\u{2014}",
                "NEW"
            ]
        );
        assert_eq!(rows[0].depth(), 0);
        assert_eq!(rows[1].depth(), 1);
        assert_eq!(rows[1].path, ["Core"]);
        assert_eq!(full_path(&rows[0]), ["Core"]);
        assert_eq!(full_path(&rows[1]), ["Core"]);
        let core = group(&rows, "group:Core");
        assert_eq!(core.label, "Core");
        assert_eq!(core.members, ["SX5E", "DAX"]);
        assert_eq!(core.present, 2);
        assert!(
            rows[0].narrow_fields().is_empty(),
            "a group never matches `/`"
        );
    }

    #[test]
    fn a_group_averages_level_delta_z_and_rank_separately_and_sums_vega() {
        // DAX gets its own history (mean 12.5, sd 8.0) so the mean of the
        // members' z differs from the z of the mean level under any stats:
        // shared stats would make z affine in level and the two coincide.
        let mut dax = leaf("DAX", Some(14.5), 0.5, 0.5, Some(3.0));
        for cell in &mut dax.board.as_mut().unwrap().cells {
            cell.stats = Some(Stats {
                mean: 12.5,
                sd: 8.0,
                count: 251,
            });
        }
        let leaves = vec![leaf("SX5E", Some(16.5), 1.0, 0.9, Some(2.0)), dax];
        let rows = group_rows(leaves, &["region".into()], &europe(), &asked());
        let core = group(&rows, "group:Core");
        let c = &core.cells[0];
        assert_eq!(c.measure, Measure::atfm("1M"));
        assert_eq!(c.level, Some(15.5));
        assert_eq!(c.delta, Some(0.75));
        // z: SX5E (16.5 − 14.5)/2 = 1.0, DAX (14.5 − 12.5)/8 = 0.25 → 0.625.
        // The z of the mean level 15.5 would be 0.5 over SX5E's stats,
        // 0.375 over DAX's and 0.4 over their average: none is 0.625.
        assert_eq!(c.z, Some(0.625));
        assert_eq!(c.pctl, Some(0.7));
        assert_eq!(rows[0].vega, Some(5.0));
        assert_eq!(core.cells.len(), 2, "one per asked measure");
    }

    #[test]
    fn a_group_counts_every_member_and_averages_only_those_with_a_value() {
        let leaves = vec![
            leaf("DAX", Some(14.5), 0.5, 0.5, None),
            leaf("SX5E", None, 0.0, 0.0, None),
        ];
        let rows = group_rows(leaves, &["region".into()], &europe(), &asked());
        let core = group(&rows, "group:Core");
        assert_eq!(core.members.len(), 2);
        assert_eq!(core.present, 1);
        assert_eq!(
            core.cells[0].level,
            Some(14.5),
            "the mean of one, not of two"
        );
        assert_eq!(rows[0].vega, None, "no leaf has vega");
        let none = group_rows(
            vec![leaf("SX5E", None, 0.0, 0.0, None)],
            &["region".into()],
            &europe(),
            &asked(),
        );
        let g = group(&none, "group:Core");
        assert_eq!(
            g.cells[0].level, None,
            "a group with no child value is blank"
        );
        assert_eq!(g.cells[0].delta, None);
    }

    #[test]
    fn nested_groups_average_over_leaves_not_over_child_means() {
        // region/sector: Core/Index holds DAX and SX5E; Isles/Index holds
        // UKX. The top-level group's mean is over its leaves whichever
        // way it is nested, so the test nests both ways and compares.
        let leaves = || {
            vec![
                leaf("DAX", Some(10.0), 0.0, 0.5, None),
                leaf("SX5E", Some(20.0), 0.0, 0.5, None),
                leaf("UKX", Some(40.0), 0.0, 0.5, None),
            ]
        };
        let by_sector_region = group_rows(
            leaves(),
            &["sector".into(), "region".into()],
            &europe(),
            &asked(),
        );
        assert_eq!(
            names(&by_sector_region),
            [
                "group:Index",
                "group:Index/Core",
                "DAX",
                "SX5E",
                "group:Index/Isles",
                "UKX"
            ]
        );
        // (10 + 20 + 40) / 3, not (15 + 40) / 2.
        assert_eq!(
            group(&by_sector_region, "group:Index").cells[0].level,
            Some(70.0 / 3.0)
        );
        assert_eq!(by_sector_region[1].depth(), 1);
        assert_eq!(by_sector_region[2].depth(), 2);
        assert_eq!(by_sector_region[2].path, ["Index", "Core"]);
    }

    #[test]
    fn mean_is_blank_over_nothing() {
        assert_eq!(mean(std::iter::empty()), None);
        assert_eq!(mean([1.0, 2.0].into_iter()), Some(1.5));
    }
}
