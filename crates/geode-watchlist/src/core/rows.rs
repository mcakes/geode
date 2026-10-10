//! The grid's rows: the snapshot's members, re-derived over the tile's
//! pending definition while an edit awaits its reload, each with its
//! reference name. Pure: the tile hands in the snapshot entry, the pending
//! object and the reference tables, and paints what comes back.

use std::collections::{BTreeMap, HashSet};

use geode_core::reference::ReferenceData;
use geode_core::watchlist::Watchlist;
use geode_core::watchlist::edit::Manual;
use geode_core::watchlist::members::{Member, Origin, resolve_members};
use geode_core::watchlist::state::WatchlistState;

/// The reference dataset the names are looked up in, and the column that
/// carries each name's long form.
pub const REFERENCE_DATASET: &str = "underlyings";
pub const REFERENCE_NAME: &str = "name";

/// What the ` · pending` suffix reads.
pub const PENDING: &str = "pending";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchRow {
    pub name: String,
    /// The reference table's `name` cell; `None` when the row is absent or
    /// the cell is NULL.
    pub reference: Option<String>,
    /// Whether the reference table holds the name at all. A row with a
    /// NULL `name` cell is in the reference; an unknown name is not.
    pub in_reference: bool,
    pub origin: Origin,
    /// The name's manual state in the pending definition differs from the
    /// snapshot's: the edit awaits its reload.
    pub pending: bool,
}

impl WatchRow {
    pub fn is_excluded(&self) -> bool {
        matches!(self.origin, Origin::Excluded { .. })
    }
}

/// The grid's rows: the snapshot's members, re-derived over the pending
/// definition when the tile has one, with the reference name beside each.
/// The rule-supplied names are the snapshot's (each member's `rules()`),
/// so a pending manual change shows at once without a resolution:
/// `resolve_members` is re-run over those names with the pending
/// `include` and `exclude`. A row is `pending` when its manual state in
/// the pending definition differs from the snapshot's definition.
pub fn rows(
    state: &WatchlistState,
    pending: Option<&Watchlist>,
    reference: &ReferenceData,
) -> Vec<WatchRow> {
    let members: Vec<Member> = match pending {
        None => state.members.clone(),
        Some(p) => {
            // Rebuild each rule's name list from the snapshot's members.
            let mut by_rule: BTreeMap<usize, Vec<String>> = BTreeMap::new();
            for m in &state.members {
                for &i in m.rules() {
                    by_rule.entry(i).or_default().push(m.name.clone());
                }
            }
            let rule_names: Vec<(usize, Vec<String>)> = by_rule.into_iter().collect();
            resolve_members(&rule_names, &p.include, &p.exclude)
        }
    };
    // The keys once, so a name the table does not hold costs one probe
    // rather than a walk of every key; likewise each definition's manual
    // names, read once rather than scanned per row (`manual_of`'s answer,
    // probed).
    let keys: HashSet<&str> = reference.keys(REFERENCE_DATASET).collect();
    let saved = ManualNames::of_list(&state.definition);
    let pending_manual = pending.map(ManualNames::of_list);
    members
        .into_iter()
        .map(|m| {
            let pending_row = pending_manual
                .as_ref()
                .is_some_and(|p| p.of(&m.name) != saved.of(&m.name));
            let reference_name = reference
                .lookup(REFERENCE_DATASET, &m.name, REFERENCE_NAME)
                .map(str::to_string);
            // A row with a NULL `name` cell is in the reference all the same.
            let in_reference = reference_name.is_some() || keys.contains(m.name.as_str());
            WatchRow {
                reference: reference_name,
                in_reference,
                name: m.name,
                origin: m.origin,
                pending: pending_row,
            }
        })
        .collect()
}

/// One definition's `include` and `exclude` as sets: `of` answers what
/// `edit::manual_of` does, in one probe each.
struct ManualNames<'a> {
    include: HashSet<&'a str>,
    exclude: HashSet<&'a str>,
}

impl<'a> ManualNames<'a> {
    fn of_list(list: &'a Watchlist) -> ManualNames<'a> {
        ManualNames {
            include: list.include.iter().map(String::as_str).collect(),
            exclude: list.exclude.iter().map(String::as_str).collect(),
        }
    }

    fn of(&self, name: &str) -> Manual {
        Manual {
            included: self.include.contains(name),
            excluded: self.exclude.contains(name),
        }
    }
}

/// `rule 1`, `rules 1, 3`: rules are numbered from 1, as the rules popup
/// lists them.
fn rules_text(rules: &[usize]) -> String {
    let numbers: Vec<String> = rules.iter().map(|i| (i + 1).to_string()).collect();
    let noun = if rules.len() == 1 { "rule" } else { "rules" };
    format!("{noun} {}", numbers.join(", "))
}

/// The origin column: `manual`, `rule 1`, `rules 1, 3`, `manual + rule 2`,
/// `excluded (rule 1)`, `excluded (manual)`, or `excluded` for an exclusion
/// nothing supplies; with ` · pending` while the row awaits its reload.
pub fn origin_text(o: &Origin, pending: bool) -> String {
    let mut text = match o {
        Origin::Manual => "manual".to_string(),
        Origin::Rules(rules) => rules_text(rules),
        Origin::Both(rules) => format!("manual + {}", rules_text(rules)),
        Origin::Excluded { rules, manual } => {
            let mut by = Vec::new();
            if *manual {
                by.push("manual".to_string());
            }
            if !rules.is_empty() {
                by.push(rules_text(rules));
            }
            if by.is_empty() {
                "excluded".to_string()
            } else {
                format!("excluded ({})", by.join(" + "))
            }
        }
    };
    if pending {
        text.push_str(" \u{00b7} ");
        text.push_str(PENDING);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::query::ReferenceTable;
    use geode_core::watchlist::Rule;
    use geode_core::watchlist::state::Status;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn member(name: &str, origin: Origin) -> Member {
        Member {
            name: name.into(),
            origin,
        }
    }

    /// Two rules: rule 1 supplies DAX and SPX, rule 2 SPX and UKX; NDX is
    /// included by hand, UKX excluded by hand.
    fn state() -> WatchlistState {
        WatchlistState {
            definition: Watchlist {
                include: s(&["NDX"]),
                exclude: s(&["UKX"]),
                rules: vec![Rule::default(), Rule::default()],
            },
            layer: None,
            shadowed: None,
            rule_errors: vec![],
            members: vec![
                member("DAX", Origin::Rules(vec![0])),
                member("NDX", Origin::Manual),
                member("SPX", Origin::Rules(vec![0, 1])),
                member(
                    "UKX",
                    Origin::Excluded {
                        rules: vec![1],
                        manual: false,
                    },
                ),
            ],
            resolved_at: None,
            status: Status::Current,
        }
    }

    /// `underlyings` with a `name` column: SPX and DAX named, NDX with a
    /// NULL name.
    fn reference() -> ReferenceData {
        let table = ReferenceTable {
            columns: vec!["underlying_ref".into(), "name".into()],
            rows: vec![
                vec![Some("SPX".into()), Some("S&P 500".into())],
                vec![Some("DAX".into()), Some("DAX 40".into())],
                vec![Some("NDX".into()), None],
            ],
            gen_id: 1,
            source_time: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        };
        ReferenceData::default()
            .with_table(REFERENCE_DATASET, &table, 1)
            .unwrap()
    }

    fn find<'a>(rows: &'a [WatchRow], name: &str) -> &'a WatchRow {
        rows.iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name} is a row"))
    }

    #[test]
    fn without_a_pending_definition_the_rows_are_the_members() {
        let state = state();
        let rows = super::rows(&state, None, &reference());
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["DAX", "NDX", "SPX", "UKX"]
        );
        for (row, member) in rows.iter().zip(&state.members) {
            assert_eq!(row.origin, member.origin, "{}", row.name);
            assert!(!row.pending, "{}", row.name);
        }
    }

    #[test]
    fn reference_names_are_filled_and_an_unknown_name_is_not_in_reference() {
        let rows = super::rows(&state(), None, &reference());
        let spx = find(&rows, "SPX");
        assert_eq!(spx.reference.as_deref(), Some("S&P 500"));
        assert!(spx.in_reference);
        // A NULL name cell: in the reference, no name to show.
        let ndx = find(&rows, "NDX");
        assert_eq!(ndx.reference, None);
        assert!(ndx.in_reference);
        let ukx = find(&rows, "UKX");
        assert_eq!(ukx.reference, None);
        assert!(!ukx.in_reference);
        // No reference table at all: nothing is in it.
        let rows = super::rows(&state(), None, &ReferenceData::default());
        assert!(
            rows.iter()
                .all(|r| r.reference.is_none() && !r.in_reference)
        );
    }

    #[test]
    fn a_pending_include_adds_a_manual_pending_row() {
        let state = state();
        let mut pending = state.definition.clone();
        pending.include.push("HSI".into());
        let rows = super::rows(&state, Some(&pending), &reference());
        let hsi = find(&rows, "HSI");
        assert_eq!(hsi.origin, Origin::Manual);
        assert!(hsi.pending);
        assert!(!find(&rows, "DAX").pending, "an untouched row is not");
        assert!(
            !find(&rows, "NDX").pending,
            "nor a manual one already saved"
        );
        // The rule rows are re-derived from the snapshot's members, so a
        // pending include of a rule name reads as both, pending.
        pending.include.push("DAX".into());
        let rows = super::rows(&state, Some(&pending), &reference());
        let dax = find(&rows, "DAX");
        assert_eq!(dax.origin, Origin::Both(vec![0]));
        assert!(dax.pending);
    }

    #[test]
    fn a_pending_exclusion_of_a_rule_row_is_excluded_pending() {
        let state = state();
        let mut pending = state.definition.clone();
        pending.exclude.push("SPX".into());
        let rows = super::rows(&state, Some(&pending), &reference());
        let spx = find(&rows, "SPX");
        assert_eq!(
            spx.origin,
            Origin::Excluded {
                rules: vec![0, 1],
                manual: false
            }
        );
        assert!(spx.pending);
        // A pending restore of the saved exclusion: UKX is a rule row
        // again, pending.
        pending.exclude.retain(|n| n != "UKX");
        let rows = super::rows(&state, Some(&pending), &reference());
        let ukx = find(&rows, "UKX");
        assert_eq!(ukx.origin, Origin::Rules(vec![1]));
        assert!(ukx.pending);
    }

    #[test]
    fn removing_a_pending_include_drops_the_row() {
        let state = state();
        let mut pending = state.definition.clone();
        pending.include.clear();
        let rows = super::rows(&state, Some(&pending), &reference());
        assert!(rows.iter().all(|r| r.name != "NDX"));
        assert_eq!(rows.len(), 3);
    }

    /// A snapshot that moved on (new rule members) under the same
    /// definition still shows the pending include: the pending object is
    /// re-applied over whatever rule names the snapshot now holds.
    #[test]
    fn a_pending_include_survives_a_snapshot_with_new_rule_members() {
        let mut state = state();
        let mut pending = state.definition.clone();
        pending.include.push("HSI".into());
        state
            .members
            .insert(0, member("CAC", Origin::Rules(vec![0])));
        let rows = super::rows(&state, Some(&pending), &reference());
        assert!(find(&rows, "HSI").pending);
        assert_eq!(find(&rows, "CAC").origin, Origin::Rules(vec![0]));
        assert!(!find(&rows, "CAC").pending);
    }

    #[test]
    fn origin_text_names_every_origin_and_the_pending_suffix() {
        assert_eq!(origin_text(&Origin::Manual, false), "manual");
        assert_eq!(origin_text(&Origin::Rules(vec![0]), false), "rule 1");
        assert_eq!(origin_text(&Origin::Rules(vec![0, 2]), false), "rules 1, 3");
        assert_eq!(
            origin_text(&Origin::Both(vec![1]), false),
            "manual + rule 2"
        );
        assert_eq!(
            origin_text(&Origin::Both(vec![0, 2]), false),
            "manual + rules 1, 3"
        );
        let excluded = |rules: &[usize], manual: bool| Origin::Excluded {
            rules: rules.to_vec(),
            manual,
        };
        assert_eq!(
            origin_text(&excluded(&[0], false), false),
            "excluded (rule 1)"
        );
        assert_eq!(
            origin_text(&excluded(&[0, 1], false), false),
            "excluded (rules 1, 2)"
        );
        assert_eq!(
            origin_text(&excluded(&[], true), false),
            "excluded (manual)"
        );
        assert_eq!(
            origin_text(&excluded(&[1], true), false),
            "excluded (manual + rule 2)"
        );
        assert_eq!(
            origin_text(&excluded(&[], false), false),
            "excluded",
            "an orphan exclusion"
        );
        assert_eq!(
            origin_text(&Origin::Manual, true),
            "manual \u{00b7} pending"
        );
        assert_eq!(
            origin_text(&excluded(&[0], false), true),
            "excluded (rule 1) \u{00b7} pending"
        );
    }
}
