//! What a consumer reads: every list's definition, provenance, members and
//! resolution state, plus the pure decisions the app's cache makes about
//! which lists a publish or a reload touches.

use super::Watchlist;
use super::fold::RuleError;
use super::members::Member;
use crate::config::Layer;
use crate::query::ResolvedRule;
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Submitted, no answer yet; `members` are the last good ones or empty.
    Resolving,
    Current,
    /// The last resolution failed whole; `members` are the last good ones.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WatchlistState {
    pub definition: Watchlist,
    pub layer: Option<Layer>,
    pub shadowed: Option<Layer>,
    pub rule_errors: Vec<RuleError>,
    pub members: Vec<Member>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WatchlistSnapshot {
    pub lists: BTreeMap<String, WatchlistState>,
}

impl WatchlistSnapshot {
    /// Live members of one list: the excluded ones left out.
    pub fn members_of(&self, name: &str) -> impl Iterator<Item = &Member> {
        self.lists
            .get(name)
            .into_iter()
            .flat_map(|s| s.members.iter())
            .filter(|m| !m.is_excluded())
    }

    /// Every name any list holds, excluded ones included: completion
    /// vocabulary.
    pub fn all_names(&self) -> BTreeSet<&str> {
        self.lists
            .values()
            .flat_map(|s| s.members.iter())
            .map(|m| m.name.as_str())
            .collect()
    }
}

/// One list as the cache holds it between resolutions: the definition, its
/// folded rules and errors, and its provenance. Two `Folded` compare equal
/// when a reload changed nothing that resolution reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Folded {
    pub list: Watchlist,
    pub rules: Vec<ResolvedRule>,
    pub errors: Vec<RuleError>,
    pub layer: Option<Layer>,
    pub shadowed: Option<Layer>,
}

/// The lists a publish of `dataset` must re-resolve: those with a good rule
/// over it. A list whose only rule over the dataset is bad is not resolved
/// again, since the bad rule contributes nothing either way.
pub fn lists_naming<'a>(folded: &'a BTreeMap<String, Folded>, dataset: &str) -> Vec<&'a str> {
    folded
        .iter()
        .filter(|(_, f)| f.rules.iter().any(|r| r.dataset == dataset))
        .map(|(name, _)| name.as_str())
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DefinitionDiff {
    /// Lists no longer defined: leave the snapshot.
    pub removed: Vec<String>,
    /// New lists and lists whose folded definition changed: resolve again.
    pub resolve: Vec<String>,
}

/// What a reload changes. Provenance alone (`layer`, `shadowed`) does not
/// re-resolve: the members cannot differ.
pub fn diff_definitions(
    old: &BTreeMap<String, Folded>,
    new: &BTreeMap<String, Folded>,
) -> DefinitionDiff {
    let mut diff = DefinitionDiff::default();
    for name in old.keys() {
        if !new.contains_key(name) {
            diff.removed.push(name.clone());
        }
    }
    for (name, f) in new {
        let same = old
            .get(name)
            .is_some_and(|o| o.list == f.list && o.rules == f.rules && o.errors == f.errors);
        if !same {
            diff.resolve.push(name.clone());
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::Scope;

    fn folded(datasets: &[&str], errors: &[usize]) -> Folded {
        Folded {
            list: Watchlist::default(),
            rules: datasets
                .iter()
                .enumerate()
                .map(|(i, d)| ResolvedRule {
                    index: i,
                    dataset: d.to_string(),
                    scope: Scope::default(),
                })
                .collect(),
            errors: errors
                .iter()
                .map(|i| RuleError {
                    index: *i,
                    reason: "x".into(),
                })
                .collect(),
            layer: None,
            shadowed: None,
        }
    }

    #[test]
    fn a_publish_names_only_lists_with_a_good_rule_over_the_dataset() {
        let mut map = BTreeMap::new();
        map.insert("a".to_string(), folded(&["risk"], &[]));
        map.insert("b".to_string(), folded(&["cvi"], &[]));
        map.insert("c".to_string(), folded(&[], &[0]));
        map.insert("d".to_string(), folded(&["risk", "cvi"], &[]));
        assert_eq!(lists_naming(&map, "risk"), vec!["a", "d"]);
        assert_eq!(lists_naming(&map, "other"), Vec::<&str>::new());
    }

    #[test]
    fn a_reload_resolves_new_and_changed_lists_and_removes_gone_ones() {
        let mut old = BTreeMap::new();
        old.insert("same".to_string(), folded(&["risk"], &[]));
        old.insert("gone".to_string(), folded(&["risk"], &[]));
        old.insert("changed".to_string(), folded(&["risk"], &[]));
        old.insert("relayered".to_string(), folded(&["risk"], &[]));
        let mut new = old.clone();
        new.remove("gone");
        new.insert("changed".to_string(), folded(&["cvi"], &[]));
        new.get_mut("relayered").unwrap().layer = Some(Layer::User);
        new.insert("fresh".to_string(), folded(&["risk"], &[]));
        let diff = diff_definitions(&old, &new);
        assert_eq!(diff.removed, vec!["gone"]);
        assert_eq!(diff.resolve, vec!["changed", "fresh"]);
    }

    #[test]
    fn members_of_skips_exclusions_and_all_names_keeps_them() {
        use super::super::members::Origin;
        let mut snap = WatchlistSnapshot::default();
        snap.lists.insert(
            "a".into(),
            WatchlistState {
                definition: Watchlist::default(),
                layer: None,
                shadowed: None,
                rule_errors: vec![],
                members: vec![
                    Member {
                        name: "SPX".into(),
                        origin: Origin::Manual,
                    },
                    Member {
                        name: "SMI".into(),
                        origin: Origin::Excluded {
                            rules: vec![],
                            manual: false,
                        },
                    },
                ],
                resolved_at: None,
                status: Status::Current,
            },
        );
        assert_eq!(
            snap.members_of("a")
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            vec!["SPX"]
        );
        assert_eq!(
            snap.all_names().into_iter().collect::<Vec<_>>(),
            vec!["SMI", "SPX"]
        );
    }
}
