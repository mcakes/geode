//! The action list's own pure core (market-data spec 2026-09-14 §6.2):
//! turning what the draft and the spec say into an ordered list of rows,
//! with which ones are pickable and why the rest are not. No gpui, no
//! entity — `popup.rs` is the paint, this is the decision.

use crate::core::draft::{DraftBadge, local_hhmm};
use crate::core::spec::KindAction;
use geode_shell::actions::ActionId;
use gpui::SharedString;

/// Everything [`rows`] needs. `has_key` is unused by this task's own
/// rules (every row here reads the draft's badge alone) — it is Task 7's
/// own input, kept on this struct now so the door does not move under it.
pub struct MenuInputs<'a> {
    pub badge: DraftBadge,
    pub has_key: bool,
    pub upload_built: bool,
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
    }
}

/// The action list, in order (spec §6.2's table): `Load underlying…`,
/// `Upload`, `Rebase`/`Discard` only while `Behind`, `Revert edits`, then
/// — only while the spec names any — a separator, the kind's own section
/// header, and one row per [`KindAction`].
pub fn rows(i: &MenuInputs) -> Vec<MenuRow> {
    let dirty = !matches!(i.badge, DraftBadge::Clean);
    let behind = matches!(i.badge, DraftBadge::Behind { .. });
    let mut out = vec![
        action(
            "marketdata::load_underlying",
            "Load underlying…",
            "u",
            if dirty {
                Err("revert or upload first")
            } else {
                Ok(())
            },
        ),
        action(
            "marketdata::upload",
            "Upload",
            ":upload",
            if !i.upload_built {
                Err("not built yet")
            } else if behind {
                Err("rebase or discard first")
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
            format!("Rebase onto {}", local_hhmm(newer)),
            ":rebase",
            Ok(()),
        ));
        out.push(action(
            "marketdata::discard",
            "Discard edits",
            ":discard",
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
            has_key: true,
            upload_built: false,
            kind_title: "CVI",
            kind_actions: CVI.actions,
        }
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
        let rows = rows(&inputs(DraftBadge::Clean));
        assert_eq!(
            titles(&rows),
            vec![
                "Load underlying…",
                "Upload",
                "Revert edits",
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

    #[test]
    fn a_dirty_draft_greys_load_and_a_built_upload_is_live() {
        let mut i = inputs(DraftBadge::Dirty);
        i.upload_built = true;
        let dirty_rows = rows(&i);
        assert_eq!(
            enabled(&dirty_rows, "Load underlying…"),
            Err("revert or upload first")
        );
        assert_eq!(enabled(&dirty_rows, "Upload"), Ok(()));
        assert_eq!(enabled(&dirty_rows, "Revert edits"), Ok(()));
        let clean = rows(&MenuInputs {
            upload_built: true,
            ..inputs(DraftBadge::Clean)
        });
        assert_eq!(enabled(&clean, "Upload"), Err("nothing to upload"));
    }

    #[test]
    fn behind_shows_rebase_and_discard_and_greys_upload() {
        let mut i = inputs(DraftBadge::Behind {
            newer: "2026-09-14T14:09:00Z".into(),
        });
        i.upload_built = true;
        let rows = rows(&i);
        assert!(titles(&rows).iter().any(|t| t.starts_with("Rebase onto ")));
        assert!(titles(&rows).contains(&"Discard edits".to_string()));
        assert_eq!(enabled(&rows, "Upload"), Err("rebase or discard first"));
    }

    #[test]
    fn a_spec_with_no_kind_actions_has_no_section() {
        let mut i = inputs(DraftBadge::Clean);
        i.kind_actions = &[];
        let rows = rows(&i);
        assert!(!titles(&rows).iter().any(|t| t == "—" || t.starts_with('[')));
    }

    #[test]
    fn navigation_skips_separators_and_starts_on_the_first_enabled_row() {
        let rows = rows(&inputs(DraftBadge::Clean));
        assert_eq!(first_enabled(&rows), 0);
        let last_action = rows.len() - 1;
        assert_eq!(
            step(&rows, 2, 1),
            5,
            "over the separator and the section header"
        );
        assert_eq!(step(&rows, 5, -1), 2);
        assert_eq!(step(&rows, last_action, 3), last_action);
        assert_eq!(step(&rows, 0, -1), 0);
    }
}
