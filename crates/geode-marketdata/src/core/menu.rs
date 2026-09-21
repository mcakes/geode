//! The action list's own pure core (market-data spec 2026-09-14 §6.2):
//! turning what the draft and the spec say into an ordered list of rows,
//! with which ones are pickable and why the rest are not. No gpui, no
//! entity — `popup.rs` is the paint, this is the decision.

use crate::core::draft::{DraftBadge, UpdatePolicy, local_hhmm};
use crate::core::spec::KindAction;
use geode_core::clock::Clock;
use geode_shell::actions::ActionId;
use gpui::SharedString;

/// Everything [`rows`] needs: every row's enablement reads the draft's
/// badge alone (a `has_key` input once sat here for the picker row and
/// was never read — removed by the final review); `policy` decides which
/// of the "On new document" rows carries the tick.
pub struct MenuInputs<'a> {
    pub badge: DraftBadge,
    pub upload_built: bool,
    pub policy: UpdatePolicy,
    pub kind_title: &'a str,
    pub kind_actions: &'a [KindAction],
}

/// One row of the action list.
pub enum MenuRow {
    Action {
        id: ActionId,
        title: SharedString,
        hint: SharedString,
        enabled: Result<(), &'static str>,
        /// `None` for a verb; `Some(_)` for a CHOICE row — one of a group
        /// of which exactly one is in force — and the paint puts a tick
        /// (or a same-width blank) ahead of the title so the group reads
        /// as a group.
        checked: Option<bool>,
    },
    Separator,
    Section(SharedString),
}

fn action(
    id: &str,
    title: impl Into<SharedString>,
    hint: &str,
    enabled: Result<(), &'static str>,
) -> MenuRow {
    MenuRow::Action {
        id: ActionId(id.to_string()),
        title: title.into(),
        hint: hint.into(),
        enabled,
        checked: None,
    }
}

/// The three "On new document" rows, in [`UpdatePolicy::ALL`]'s order,
/// ticked where `policy` matches.
fn policy_rows(policy: UpdatePolicy) -> impl Iterator<Item = MenuRow> {
    UpdatePolicy::ALL.into_iter().map(move |p| {
        let (id, title) = match p {
            UpdatePolicy::Hold => ("marketdata::auto_hold", "hold edits"),
            UpdatePolicy::Rebase => ("marketdata::auto_rebase", "rebase edits"),
            UpdatePolicy::Replace => ("marketdata::auto_replace", "replace edits"),
        };
        MenuRow::Action {
            id: ActionId(id.to_string()),
            title: title.into(),
            hint: SharedString::default(),
            enabled: Ok(()),
            checked: Some(p == policy),
        }
    })
}

/// The action list, in order (spec §6.2's table): `Load underlying…`
/// (always enabled since 2026-09-19 — a switch PARKS the current draft
/// under its underlying rather than being refused by it, spec §7's
/// amendment), `Upload`, `Rebase` only while `Behind`, `Revert edits`,
/// then a separator and the `On new document` section (three policy
/// rows, one ticked — always enabled, since a policy is a setting and not
/// a verb on the draft), then — only while the spec names any — a
/// separator, the kind's own section header, and one row per
/// [`KindAction`].
pub fn rows(i: &MenuInputs, clock: Clock) -> Vec<MenuRow> {
    let dirty = !matches!(i.badge, DraftBadge::Clean);
    let behind = matches!(i.badge, DraftBadge::Behind { .. });
    let mut out = vec![
        action(
            "marketdata::load_underlying",
            "Load underlying…",
            "u",
            Ok(()),
        ),
        action(
            "marketdata::upload",
            "Upload",
            ":upload",
            if !i.upload_built {
                Err("not built yet")
            } else if behind {
                Err("rebase or revert first")
            } else if !dirty {
                Err("nothing to upload")
            } else {
                Ok(())
            },
        ),
    ];
    if let DraftBadge::Behind { newer } = &i.badge {
        out.push(action(
            "marketdata::rebase",
            format!("Rebase onto {}", local_hhmm(newer, clock)),
            ":rebase",
            Ok(()),
        ));
    }
    out.push(action(
        "marketdata::revert",
        "Revert edits",
        ":revert",
        if dirty {
            Ok(())
        } else {
            Err("nothing to revert")
        },
    ));
    out.push(MenuRow::Separator);
    out.push(MenuRow::Section("On new document".into()));
    out.extend(policy_rows(i.policy));
    if !i.kind_actions.is_empty() {
        out.push(MenuRow::Separator);
        out.push(MenuRow::Section(i.kind_title.into()));
        for k in i.kind_actions {
            out.push(action(
                k.id,
                k.title,
                "",
                if k.built {
                    Ok(())
                } else {
                    Err("not built yet")
                },
            ));
        }
    }
    out
}

/// The first row worth landing the highlight on — the first enabled
/// `Action`, or `0` if none is (an all-disabled menu still needs a
/// highlighted row for the border to paint on).
pub fn first_enabled(rows: &[MenuRow]) -> usize {
    rows.iter()
        .position(|r| {
            matches!(
                r,
                MenuRow::Action {
                    enabled: Ok(()),
                    ..
                }
            )
        })
        .unwrap_or(0)
}

/// Move `delta` steps from `from` over `Action` rows only — a
/// `Separator`/`Section` is never landed on — clamped at either end
/// rather than wrapping.
pub fn step(rows: &[MenuRow], from: usize, delta: isize) -> usize {
    let actionable: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, MenuRow::Action { .. }).then_some(i))
        .collect();
    let Some(pos) = actionable.iter().position(|&i| i == from) else {
        return from;
    };
    let moved = (pos as isize + delta).clamp(0, actionable.len() as isize - 1);
    actionable[moved as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CVI;

    fn inputs(badge: DraftBadge) -> MenuInputs<'static> {
        MenuInputs {
            badge,
            upload_built: false,
            policy: UpdatePolicy::Hold,
            kind_title: "CVI",
            kind_actions: CVI.actions,
        }
    }
    fn checked(rows: &[MenuRow]) -> Vec<(String, Option<bool>)> {
        rows.iter()
            .filter_map(|r| match r {
                MenuRow::Action { title, checked, .. } => Some((title.to_string(), *checked)),
                _ => None,
            })
            .collect()
    }
    fn titles(rows: &[MenuRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                MenuRow::Action { title, .. } => title.to_string(),
                MenuRow::Separator => "—".into(),
                MenuRow::Section(s) => format!("[{s}]"),
            })
            .collect()
    }
    fn enabled(rows: &[MenuRow], title: &str) -> Result<(), &'static str> {
        rows.iter()
            .find_map(|r| match r {
                MenuRow::Action {
                    title: t, enabled, ..
                } if t == title => Some(*enabled),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn a_clean_draft_offers_load_and_greys_upload_and_revert() {
        let rows = rows(&inputs(DraftBadge::Clean), Clock::utc());
        assert_eq!(
            titles(&rows),
            vec![
                "Load underlying…",
                "Upload",
                "Revert edits",
                "—",
                "[On new document]",
                "hold edits",
                "rebase edits",
                "replace edits",
                "—",
                "[CVI]",
                "Reanchor",
                "Recalc forward"
            ]
        );
        assert_eq!(enabled(&rows, "Load underlying…"), Ok(()));
        assert_eq!(enabled(&rows, "Upload"), Err("not built yet"));
        assert_eq!(enabled(&rows, "Revert edits"), Err("nothing to revert"));
        assert_eq!(enabled(&rows, "Reanchor"), Err("not built yet"));
    }

    /// Per-underlying drafts (2026-09-19): a dirty draft no longer greys
    /// `Load underlying…` — a switch parks it under its own underlying.
    #[test]
    fn a_dirty_draft_leaves_load_live_and_a_built_upload_is_live() {
        let mut i = inputs(DraftBadge::Dirty);
        i.upload_built = true;
        let dirty_rows = rows(&i, Clock::utc());
        assert_eq!(enabled(&dirty_rows, "Load underlying…"), Ok(()));
        assert_eq!(enabled(&dirty_rows, "Upload"), Ok(()));
        assert_eq!(enabled(&dirty_rows, "Revert edits"), Ok(()));
        let clean = rows(
            &MenuInputs {
                upload_built: true,
                ..inputs(DraftBadge::Clean)
            },
            Clock::utc(),
        );
        assert_eq!(enabled(&clean, "Upload"), Err("nothing to upload"));
    }

    #[test]
    fn behind_shows_rebase_and_greys_upload() {
        let mut i = inputs(DraftBadge::Behind {
            newer: "2026-09-14T14:09:00Z".into(),
        });
        i.upload_built = true;
        let rows = rows(&i, Clock::utc());
        assert!(titles(&rows).iter().any(|t| t.starts_with("Rebase onto ")));
        assert_eq!(enabled(&rows, "Upload"), Err("rebase or revert first"));
    }

    #[test]
    fn a_spec_with_no_kind_actions_has_no_kind_section() {
        let mut i = inputs(DraftBadge::Clean);
        i.kind_actions = &[];
        let rows = rows(&i, Clock::utc());
        let t = titles(&rows);
        assert!(!t.iter().any(|t| t == "[CVI]"), "{t:?}");
        assert_eq!(
            t.last().map(String::as_str),
            Some("replace edits"),
            "the policy section is the last thing on the list"
        );
        assert_eq!(t.iter().filter(|t| *t == "—").count(), 1);
    }

    /// The `On new document` section sits after the verbs and before the
    /// kind's own section; exactly one of its three rows is checked, the
    /// one matching the policy, and no verb row carries a check at all.
    #[test]
    fn exactly_one_policy_row_is_checked_and_it_follows_the_policy() {
        for policy in UpdatePolicy::ALL {
            let rows = rows(
                &MenuInputs {
                    policy,
                    ..inputs(DraftBadge::Dirty)
                },
                Clock::utc(),
            );
            let t = titles(&rows);
            let section = t.iter().position(|t| t == "[On new document]").unwrap();
            let kind = t.iter().position(|t| t == "[CVI]").unwrap();
            assert_eq!(t[section - 1], "—");
            assert_eq!(t[section - 2], "Revert edits");
            assert!(section < kind, "{t:?}");
            let c = checked(&rows);
            let ticked: Vec<&str> = c
                .iter()
                .filter(|(_, ch)| *ch == Some(true))
                .map(|(t, _)| t.as_str())
                .collect();
            assert_eq!(ticked, vec![format!("{} edits", policy.as_str())]);
            assert_eq!(
                c.iter().filter(|(_, ch)| ch.is_some()).count(),
                3,
                "the three policy rows are the only choice rows"
            );
            for title in ["hold edits", "rebase edits", "replace edits"] {
                assert_eq!(enabled(&rows, title), Ok(()), "{title} is always live");
            }
        }
    }

    #[test]
    fn navigation_skips_separators_and_starts_on_the_first_enabled_row() {
        let rows = rows(&inputs(DraftBadge::Clean), Clock::utc());
        assert_eq!(first_enabled(&rows), 0);
        let last_action = rows.len() - 1;
        assert_eq!(
            step(&rows, 2, 1),
            5,
            "over the separator and the `On new document` section header"
        );
        assert_eq!(step(&rows, 5, -1), 2);
        assert_eq!(
            step(&rows, 7, 1),
            10,
            "over the separator and the kind section header"
        );
        assert_eq!(step(&rows, last_action, 3), last_action);
        assert_eq!(step(&rows, 0, -1), 0);
    }
}
