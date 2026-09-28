//! Prepared action-menu rows, enablement reasons, and policy choices, as
//! `geode_tile::menu` rows over action ids. Key hints name actions; the door
//! resolves them against the live keymap. These functions own no entity or
//! window.

use crate::core::draft::{DraftBadge, UpdatePolicy, local_hhmm};
use crate::core::spec::KindAction;
use geode_core::clock::Clock;
use geode_shell::actions::ActionId;
use geode_tile::menu::{ActionRow, Hint, Row};
use gpui::SharedString;

/// Inputs for [`rows`]: draft state, upload availability, current update
/// policy, and the kind's advertised actions. The tile can impose further
/// execution guards beyond this menu's enablement.
pub struct MenuInputs<'a> {
    pub badge: DraftBadge,
    pub upload_built: bool,
    pub policy: UpdatePolicy,
    pub kind_title: &'a str,
    pub kind_actions: &'a [KindAction],
}

fn action(
    id: &'static str,
    title: impl Into<SharedString>,
    hint: Hint,
    enabled: Result<(), &'static str>,
) -> Row<ActionId> {
    Row::Action(
        ActionRow::new(ActionId(id.to_string()), title)
            .hint(hint)
            .enabled(enabled.map_err(SharedString::new_static)),
    )
}

/// The three "On new document" rows, in [`UpdatePolicy::ALL`]'s order,
/// ticked where `policy` matches: a choice group, exactly one in force.
/// Each hint is its action's live chord, else the `:auto` verb that sets the
/// same policy, so a user binding shows and an unbound row still names a route.
fn policy_rows(policy: UpdatePolicy) -> impl Iterator<Item = Row<ActionId>> {
    UpdatePolicy::ALL.into_iter().map(move |p| {
        let (id, title, verb) = match p {
            UpdatePolicy::Hold => ("marketdata::auto_hold", "hold edits", ":auto hold"),
            UpdatePolicy::Rebase => ("marketdata::auto_rebase", "rebase edits", ":auto rebase"),
            UpdatePolicy::Replace => ("marketdata::auto_replace", "replace edits", ":auto replace"),
        };
        Row::Action(
            ActionRow::new(ActionId(id.to_string()), title)
                .hint(Hint::chord_or_verb(id, verb))
                .checked(p == policy),
        )
    })
}

/// Ordered actions: load, upload, rebase while behind, and revert; then
/// update-policy choices and any kind-specific actions. Load stays enabled
/// because the tile parks drafts by underlying. Policy choices are always
/// enabled because they configure later deliveries rather than edit the draft.
pub fn rows(i: &MenuInputs, clock: Clock) -> Vec<Row<ActionId>> {
    let dirty = !matches!(i.badge, DraftBadge::Clean);
    let behind = matches!(i.badge, DraftBadge::Behind { .. });
    let mut out = vec![
        action(
            "marketdata::load_underlying",
            "Load underlying…",
            Hint::chord("marketdata::load_underlying"),
            Ok(()),
        ),
        action(
            "marketdata::upload",
            "Upload",
            Hint::chord_or_verb("marketdata::upload", ":upload"),
            if !i.upload_built {
                Err("not built yet")
            } else if behind {
                Err("rebase or revert first")
            } else if matches!(i.badge, DraftBadge::Sent { .. }) {
                Err("already sent")
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
            Hint::chord_or_verb("marketdata::rebase", ":rebase"),
            Ok(()),
        ));
    }
    out.push(action(
        "marketdata::revert",
        "Revert edits",
        Hint::chord_or_verb("marketdata::revert", ":revert"),
        if dirty {
            Ok(())
        } else {
            Err("nothing to revert")
        },
    ));
    out.push(Row::Separator);
    out.push(Row::Section("On new document".into()));
    out.extend(policy_rows(i.policy));
    if !i.kind_actions.is_empty() {
        out.push(Row::Separator);
        out.push(Row::Section(i.kind_title.into()));
        for k in i.kind_actions {
            out.push(action(
                k.id,
                k.title,
                // Kind actions ship unbound; a user binding still shows.
                Hint::chord(k.id),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_fixtures::CVI;
    use geode_tile::menu::{first_enabled, step};

    fn inputs(badge: DraftBadge) -> MenuInputs<'static> {
        MenuInputs {
            badge,
            upload_built: false,
            policy: UpdatePolicy::Hold,
            kind_title: "CVI",
            kind_actions: &CVI.actions,
        }
    }
    fn checked(rows: &[Row<ActionId>]) -> Vec<(String, Option<bool>)> {
        rows.iter()
            .filter_map(|r| r.action().map(|a| (a.title().to_string(), a.tick())))
            .collect()
    }
    fn titles(rows: &[Row<ActionId>]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Action(a) => a.title().to_string(),
                Row::Separator => "—".into(),
                Row::Section(s) => format!("[{s}]"),
            })
            .collect()
    }
    fn enabled(rows: &[Row<ActionId>], title: &str) -> Result<(), String> {
        rows.iter()
            .find_map(|r| r.action().filter(|a| a.title().as_ref() == title))
            .map(|a| a.reason().map_or(Ok(()), |r| Err(r.to_string())))
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
        assert_eq!(enabled(&rows, "Upload"), Err("not built yet".into()));
        assert_eq!(
            enabled(&rows, "Revert edits"),
            Err("nothing to revert".into())
        );
        assert_eq!(enabled(&rows, "Reanchor"), Err("not built yet".into()));
    }

    /// Loading another underlying remains available with pending edits;
    /// the tile parks the draft under its current underlying.
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
        assert_eq!(enabled(&clean, "Upload"), Err("nothing to upload".into()));
    }

    #[test]
    fn a_sent_draft_greys_upload_until_the_next_edit() {
        let mut i = inputs(DraftBadge::Sent {
            at: "2026-09-14T14:09:00Z".into(),
        });
        i.upload_built = true;
        assert_eq!(
            enabled(&rows(&i, Clock::utc()), "Upload"),
            Err("already sent".into())
        );
    }

    #[test]
    fn behind_shows_rebase_and_greys_upload() {
        let mut i = inputs(DraftBadge::Behind {
            newer: "2026-09-14T14:09:00Z".into(),
        });
        i.upload_built = true;
        let rows = rows(&i, Clock::utc());
        assert!(titles(&rows).iter().any(|t| t.starts_with("Rebase onto ")));
        assert_eq!(
            enabled(&rows, "Upload"),
            Err("rebase or revert first".into())
        );
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
        let mut i = inputs(DraftBadge::Dirty);
        i.upload_built = true;
        let rows = rows(&i, Clock::utc());
        assert_eq!(first_enabled(&rows), Some(0));
        assert_eq!(
            step(&rows, Some(2), 1),
            Some(5),
            "over the separator and the `On new document` section header"
        );
        assert_eq!(step(&rows, Some(5), -1), Some(2));
        assert_eq!(
            step(&rows, Some(5), -2),
            Some(1),
            "a count steps that many rows"
        );
        assert_eq!(
            step(&rows, Some(7), 1),
            Some(7),
            "the kind section's rows are unbuilt: clamped on the last live row"
        );
        assert_eq!(step(&rows, Some(0), -1), Some(0));
    }

    /// On a clean draft, keyboard motion skips disabled Upload and Revert rows,
    /// landing on the first update-policy choice. Pointer-highlighted disabled
    /// rows can still be the starting position for that motion.
    #[test]
    fn navigation_skips_disabled_rows() {
        let rows = rows(&inputs(DraftBadge::Clean), Clock::utc());
        assert_eq!(
            step(&rows, Some(0), 1),
            Some(5),
            "over Upload and Revert edits"
        );
        assert_eq!(step(&rows, Some(5), -1), Some(0));
        assert_eq!(
            step(&rows, Some(2), 1),
            Some(5),
            "a highlight the pointer left on a greyed row steps from it"
        );
        assert_eq!(step(&rows, Some(2), -1), Some(0));
        let last = rows.len() - 1;
        assert_eq!(
            step(&rows, Some(last), 1),
            Some(last),
            "nothing live below: stays"
        );
    }
}
