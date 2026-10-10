//! Edit verbs over a list. Each returns the whole next object and an entry
//! holding only what changed, so undo can replay row by row over whatever
//! the object is by then and skip a row another surface changed since —
//! restoring a stored snapshot would silently revert edits made elsewhere.

use super::members::{Member, Origin};
use super::{Rule, Watchlist};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Manual {
    pub included: bool,
    pub excluded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Name {
        name: String,
        before: Manual,
        after: Manual,
    },
    Rules {
        before: Vec<Rule>,
        after: Vec<Rule>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UndoEntry {
    pub changes: Vec<Change>,
}

impl UndoEntry {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Whether `name` is in the list's `include` and `exclude` by hand.
pub fn manual_of(list: &Watchlist, name: &str) -> Manual {
    Manual {
        included: list.include.iter().any(|n| n == name),
        excluded: list.exclude.iter().any(|n| n == name),
    }
}

fn set_manual(list: &mut Watchlist, name: &str, manual: Manual) {
    let set = |v: &mut Vec<String>, on: bool| {
        let has = v.iter().any(|n| n == name);
        if on && !has {
            v.push(name.to_string());
        }
        if !on && has {
            v.retain(|n| n != name);
        }
    };
    set(&mut list.include, manual.included);
    set(&mut list.exclude, manual.excluded);
}

/// Move `name` to `after`, recording the change; a no-op records nothing
/// so undo never replays a row that did not move.
fn change(list: &mut Watchlist, entry: &mut UndoEntry, name: &str, after: Manual) {
    let before = manual_of(list, name);
    if before == after {
        return;
    }
    set_manual(list, name, after);
    entry.changes.push(Change::Name {
        name: name.to_string(),
        before,
        after,
    });
}

/// Add names by hand. A name already a live member is refused naming its
/// origin (the first refusal wins, nothing is changed); an excluded name is
/// restored instead of added. `names` are trimmed; blanks are ignored.
pub fn add(
    list: &Watchlist,
    members: &[Member],
    names: &[String],
) -> Result<(Watchlist, UndoEntry), String> {
    let mut next = list.clone();
    let mut entry = UndoEntry::default();
    for raw in names {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        match members.iter().find(|m| m.name == name).map(|m| &m.origin) {
            Some(Origin::Manual) | Some(Origin::Both(_)) => {
                return Err(format!("{name} is already here"));
            }
            Some(Origin::Rules(rules)) => {
                return Err(format!("{name} is already here from {}", rule_label(rules)));
            }
            // Restore when a rule will supply the name again; an orphan
            // exclusion (no rule, not manual) is included by hand instead,
            // or clearing it would leave the name nowhere.
            Some(Origin::Excluded { rules, manual }) => change(
                &mut next,
                &mut entry,
                name,
                Manual {
                    included: *manual || rules.is_empty(),
                    excluded: false,
                },
            ),
            None => change(
                &mut next,
                &mut entry,
                name,
                Manual {
                    included: true,
                    excluded: false,
                },
            ),
        }
    }
    Ok((next, entry))
}

/// Rules numbered from 1 for people: `rule 2` for index 1.
fn rule_label(rules: &[usize]) -> String {
    let shown: Vec<String> = rules.iter().map(|i| (i + 1).to_string()).collect();
    if shown.len() == 1 {
        format!("rule {}", shown[0])
    } else {
        format!("rules {}", shown.join(", "))
    }
}

/// Remove names: a manual one leaves `include`, a rule-derived one is
/// excluded, one that is both does both, an excluded one is restored. An
/// unknown name changes nothing.
pub fn remove(list: &Watchlist, members: &[Member], names: &[String]) -> (Watchlist, UndoEntry) {
    let mut next = list.clone();
    let mut entry = UndoEntry::default();
    for name in names {
        let after = match members.iter().find(|m| &m.name == name).map(|m| &m.origin) {
            Some(Origin::Manual) => Manual {
                included: false,
                excluded: false,
            },
            Some(Origin::Rules(_)) | Some(Origin::Both(_)) => Manual {
                included: false,
                excluded: true,
            },
            Some(Origin::Excluded { manual, .. }) => Manual {
                included: *manual,
                excluded: false,
            },
            None => continue,
        };
        change(&mut next, &mut entry, name, after);
    }
    (next, entry)
}

/// Replace the rules whole: one change, undone whole.
pub fn set_rules(list: &Watchlist, rules: Vec<Rule>) -> (Watchlist, UndoEntry) {
    let mut next = list.clone();
    let mut entry = UndoEntry::default();
    if next.rules != rules {
        entry.changes.push(Change::Rules {
            before: next.rules.clone(),
            after: rules.clone(),
        });
        next.rules = rules;
    }
    (next, entry)
}

/// Replay `entry` backwards over `current`. A name whose manual state is
/// no longer the entry's `after`, or a rules set no longer `after`, was
/// changed elsewhere and is skipped. Returns the next object, the entry
/// that redoes what was undone (itself undone by `undo`), and how many
/// changes were skipped.
pub fn undo(current: &Watchlist, entry: &UndoEntry) -> (Watchlist, UndoEntry, usize) {
    let mut next = current.clone();
    let mut redo = UndoEntry::default();
    let mut skipped = 0;
    for c in entry.changes.iter().rev() {
        match c {
            Change::Name {
                name,
                before,
                after,
            } => {
                if manual_of(&next, name) != *after {
                    skipped += 1;
                    continue;
                }
                set_manual(&mut next, name, *before);
                redo.changes.push(Change::Name {
                    name: name.clone(),
                    before: *after,
                    after: *before,
                });
            }
            Change::Rules { before, after } => {
                if next.rules != *after {
                    skipped += 1;
                    continue;
                }
                next.rules = before.clone();
                redo.changes.push(Change::Rules {
                    before: after.clone(),
                    after: before.clone(),
                });
            }
        }
    }
    redo.changes.reverse();
    (next, redo, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }
    fn member(name: &str, origin: Origin) -> Member {
        Member {
            name: name.into(),
            origin,
        }
    }

    #[test]
    fn add_includes_a_new_name_and_restores_an_excluded_one() {
        let list = Watchlist {
            exclude: s(&["SMI"]),
            ..Default::default()
        };
        let members = vec![member(
            "SMI",
            Origin::Excluded {
                rules: vec![0],
                manual: false,
            },
        )];
        let (next, entry) = add(&list, &members, &s(&["NDX", "SMI"])).unwrap();
        assert_eq!(next.include, vec!["NDX"]);
        assert!(next.exclude.is_empty());
        assert_eq!(entry.changes.len(), 2);
    }

    #[test]
    fn add_of_an_orphan_exclusion_includes_it_by_hand() {
        // No rule supplies HSI, so clearing the exclusion alone would leave
        // the name nowhere; it is included by hand instead.
        let list = Watchlist {
            exclude: s(&["HSI"]),
            ..Default::default()
        };
        let members = vec![member(
            "HSI",
            Origin::Excluded {
                rules: vec![],
                manual: false,
            },
        )];
        let (next, entry) = add(&list, &members, &s(&["HSI"])).unwrap();
        assert_eq!(next.include, vec!["HSI"]);
        assert!(next.exclude.is_empty());
        assert_eq!(entry.changes.len(), 1);
    }

    #[test]
    fn add_refuses_a_name_a_rule_already_supplies_naming_the_rule() {
        let list = Watchlist::default();
        let members = vec![member("SPX", Origin::Rules(vec![1]))];
        let err = add(&list, &members, &s(&["SPX"])).unwrap_err();
        assert!(err.contains("SPX") && err.contains("rule 2"), "{err}");
        let err = add(
            &Watchlist {
                include: s(&["NDX"]),
                ..Default::default()
            },
            &[member("NDX", Origin::Manual)],
            &s(&["NDX"]),
        )
        .unwrap_err();
        assert!(err.contains("already"), "{err}");
    }

    #[test]
    fn remove_dispatches_on_origin() {
        let list = Watchlist {
            include: s(&["NDX", "DAX"]),
            exclude: s(&["SMI"]),
            ..Default::default()
        };
        let members = vec![
            member("NDX", Origin::Manual),
            member("SPX", Origin::Rules(vec![0])),
            member("DAX", Origin::Both(vec![0])),
            member(
                "SMI",
                Origin::Excluded {
                    rules: vec![0],
                    manual: false,
                },
            ),
        ];
        let (next, entry) = remove(
            &list,
            &members,
            &s(&["NDX", "SPX", "DAX", "SMI", "UNKNOWN"]),
        );
        assert!(next.include.is_empty());
        assert_eq!(next.exclude, vec!["SPX", "DAX"]);
        assert_eq!(entry.changes.len(), 4, "the unknown name changes nothing");
    }

    #[test]
    fn undo_replays_over_the_current_object_and_skips_rows_changed_since() {
        let list = Watchlist::default();
        let (edited, entry) = add(&list, &[], &s(&["NDX", "SPX"])).unwrap();
        // Another surface removed NDX and added HSI meanwhile.
        let mut current = edited.clone();
        current.include.retain(|n| n != "NDX");
        current.include.push("HSI".into());
        let (undone, redo, skipped) = undo(&current, &entry);
        assert_eq!(skipped, 1);
        assert_eq!(
            undone.include,
            vec!["HSI"],
            "SPX undone, NDX skipped, HSI untouched"
        );
        // Redo re-applies only what undo changed.
        let (redone, _, skipped) = undo(&undone, &redo);
        assert_eq!(skipped, 0);
        assert_eq!(redone.include, vec!["HSI", "SPX"]);
    }

    #[test]
    fn set_rules_is_one_change_and_undoes_whole() {
        let list = Watchlist::default();
        let rule = Rule {
            dataset: "risk".into(),
            scope: None,
            expression: None,
        };
        let (next, entry) = set_rules(&list, vec![rule.clone()]);
        assert_eq!(next.rules, vec![rule.clone()]);
        let (back, _, skipped) = undo(&next, &entry);
        assert_eq!(skipped, 0);
        assert!(back.rules.is_empty());
        // Rules changed since: the undo is skipped, not applied over them.
        let mut moved = next.clone();
        moved.rules.push(Rule {
            dataset: "cvi".into(),
            scope: None,
            expression: None,
        });
        let (same, _, skipped) = undo(&moved, &entry);
        assert_eq!(skipped, 1);
        assert_eq!(same, moved);
    }
}
