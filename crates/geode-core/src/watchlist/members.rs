//! The set algebra: rule results ∪ include − exclude, every member with its
//! origin so a tile can say where a name came from and a consumer can skip
//! the excluded ones.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Manual,
    Rules(Vec<usize>),
    Both(Vec<usize>),
    Excluded { rules: Vec<usize>, manual: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub origin: Origin,
}

impl Member {
    pub fn is_excluded(&self) -> bool {
        matches!(self.origin, Origin::Excluded { .. })
    }

    pub fn rules(&self) -> &[usize] {
        match &self.origin {
            Origin::Manual => &[],
            Origin::Rules(r) | Origin::Both(r) | Origin::Excluded { rules: r, .. } => r,
        }
    }
}

/// `rule_names` is each good rule's index and the names it produced.
/// Members sort by name; excluded names are kept (origin `Excluded`) so a
/// tile can show and restore them. An excluded name no rule produces and
/// `include` does not hold is still listed, as
/// `Excluded { rules: [], manual: false }`: the exclusion exists and the
/// trader may want to drop it.
pub fn resolve_members(
    rule_names: &[(usize, Vec<String>)],
    include: &[String],
    exclude: &[String],
) -> Vec<Member> {
    let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, names) in rule_names {
        for n in names {
            let rules = by_name.entry(n.as_str()).or_default();
            if !rules.contains(index) {
                rules.push(*index);
            }
        }
    }
    for n in include {
        by_name.entry(n.as_str()).or_default();
    }
    for n in exclude {
        by_name.entry(n.as_str()).or_default();
    }
    by_name
        .into_iter()
        .map(|(name, mut rules)| {
            rules.sort_unstable();
            let manual = include.iter().any(|i| i == name);
            let origin = if exclude.iter().any(|e| e == name) {
                Origin::Excluded { rules, manual }
            } else if manual && rules.is_empty() {
                Origin::Manual
            } else if manual {
                Origin::Both(rules)
            } else {
                Origin::Rules(rules)
            };
            Member {
                name: name.to_string(),
                origin,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn members_union_rules_add_include_and_mark_exclusions() {
        let rules = vec![(0, s(&["SPX", "DAX"])), (2, s(&["DAX", "SMI", "UKX"]))];
        let members = resolve_members(&rules, &s(&["NDX", "DAX"]), &s(&["UKX", "HSI"]));
        let find = |n: &str| {
            members
                .iter()
                .find(|m| m.name == n)
                .map(|m| m.origin.clone())
        };
        assert_eq!(find("SPX"), Some(Origin::Rules(vec![0])));
        assert_eq!(find("DAX"), Some(Origin::Both(vec![0, 2])));
        assert_eq!(find("SMI"), Some(Origin::Rules(vec![2])));
        assert_eq!(find("NDX"), Some(Origin::Manual));
        assert_eq!(
            find("UKX"),
            Some(Origin::Excluded {
                rules: vec![2],
                manual: false
            })
        );
        assert_eq!(
            find("HSI"),
            Some(Origin::Excluded {
                rules: vec![],
                manual: false
            })
        );
        assert_eq!(
            members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["DAX", "HSI", "NDX", "SMI", "SPX", "UKX"]
        );
        assert_eq!(members.iter().filter(|m| !m.is_excluded()).count(), 4);
    }

    #[test]
    fn an_excluded_name_is_never_a_live_member_whatever_produced_it() {
        let members = resolve_members(&[(0, s(&["SPX"]))], &s(&["SPX"]), &s(&["SPX"]));
        assert_eq!(
            members[0].origin,
            Origin::Excluded {
                rules: vec![0],
                manual: true
            }
        );
    }
}
