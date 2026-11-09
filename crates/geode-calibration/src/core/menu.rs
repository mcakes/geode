//! The `.` action menu's rows, prepared from the tile's state as
//! `geode_tile::menu` rows over action ids. Each row names the action its
//! key dispatches, hints that key (resolved against the live keymap by the
//! menu door), and carries the reason it cannot be picked now. A pick
//! re-enters the tile's dispatch on the row's id, so a row, its key and the
//! palette take one path.

use geode_shell::actions::ActionId;
use geode_tile::menu::{ActionRow, Hint, Row};
use gpui::SharedString;

/// The shell action that opens the cursor's stage in the object dialog's
/// Edit screen: the `Open stage` row's pick, once that route reaches the
/// tile.
pub const OPEN_STAGE: &str = "config::calibration";

/// The reason on a row whose verb the tile does not carry yet: the row is
/// in its place so the menu reads whole, and says honestly that a pick
/// does nothing until the verb lands.
pub const NEXT_CHANGE: &str = "arrives with the next change";

/// `Run from here` with no cursor: there is no stage to run from.
pub const NO_CURSOR: &str = "no stage under the cursor";

/// Any run or publish with no pipeline shown.
pub const NO_PIPELINE: &str = "no pipeline";

/// What the rows are built from, read from the tile when the menu opens
/// and whenever the tile changes under an open menu.
pub struct MenuInputs<'a> {
    pub cursor: Option<&'a str>,
    /// Whether a pipeline is shown.
    pub pipeline: bool,
    /// Whether `p` would ask now, else the refusal it would give: the row
    /// and the key say the same words.
    pub publish: Result<(), String>,
}

fn row(id: &'static str, title: &'static str) -> ActionRow<ActionId> {
    ActionRow::new(ActionId(id.to_string()), SharedString::new_static(title)).hint(Hint::chord(id))
}

/// The rows, in fixed order: the runs, the publish, then the editors.
pub fn rows(i: &MenuInputs) -> Vec<Row<ActionId>> {
    let pipeline = if i.pipeline {
        Ok(())
    } else {
        Err(SharedString::new_static(NO_PIPELINE))
    };
    let from_here = match (i.pipeline, i.cursor) {
        (false, _) => Err(SharedString::new_static(NO_PIPELINE)),
        (true, None) => Err(SharedString::new_static(NO_CURSOR)),
        (true, Some(_)) => Ok(()),
    };
    let later = || Err(SharedString::new_static(NEXT_CHANGE));
    vec![
        Row::Action(row("calibration::run_from", "Run from here").enabled(from_here)),
        Row::Action(row("calibration::run_all", "Run all").enabled(pipeline)),
        Row::Separator,
        Row::Action(
            row("calibration::publish", "Publish")
                .enabled(i.publish.clone().map_err(SharedString::from)),
        ),
        Row::Separator,
        Row::Action(row(OPEN_STAGE, "Open stage").enabled(later())),
        Row::Action(row("calibration::connect", "Connect\u{2026}").enabled(later())),
        Row::Action(row("calibration::disconnect", "Disconnect\u{2026}").enabled(later())),
        Row::Action(row("calibration::add", "Add stage\u{2026}").enabled(later())),
        Row::Action(row("calibration::delete", "Delete stage").enabled(later())),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(rows: &[Row<ActionId>]) -> Vec<&str> {
        rows.iter()
            .filter_map(Row::action)
            .map(|a| a.pick().0.as_str())
            .collect()
    }

    fn find<'a>(rows: &'a [Row<ActionId>], id: &str) -> &'a ActionRow<ActionId> {
        rows.iter()
            .filter_map(Row::action)
            .find(|a| a.pick().0 == id)
            .unwrap_or_else(|| panic!("{id} has a row"))
    }

    #[test]
    fn the_rows_name_every_verb_in_order_with_its_state() {
        let rows = rows(&MenuInputs {
            cursor: Some("parity"),
            pipeline: true,
            publish: Ok(()),
        });
        assert_eq!(
            ids(&rows),
            [
                "calibration::run_from",
                "calibration::run_all",
                "calibration::publish",
                OPEN_STAGE,
                "calibration::connect",
                "calibration::disconnect",
                "calibration::add",
                "calibration::delete",
            ]
        );
        // Two separators part the runs, the publish and the editors.
        assert_eq!(
            rows.iter().filter(|r| !r.is_action()).count(),
            2,
            "{rows:?}"
        );
        assert_eq!(rows.len(), 10);
        for id in [
            "calibration::run_from",
            "calibration::run_all",
            "calibration::publish",
        ] {
            assert!(find(&rows, id).is_enabled(), "{id}");
        }
        for id in [
            OPEN_STAGE,
            "calibration::connect",
            "calibration::disconnect",
            "calibration::add",
            "calibration::delete",
        ] {
            assert_eq!(
                find(&rows, id).reason().map(|r| r.as_ref()),
                Some(NEXT_CHANGE),
                "{id}"
            );
        }
        assert_eq!(
            find(&rows, "calibration::connect").title().as_ref(),
            "Connect\u{2026}"
        );
        assert_eq!(
            find(&rows, "calibration::delete").title().as_ref(),
            "Delete stage"
        );
    }

    #[test]
    fn rows_that_cannot_act_now_say_why() {
        let rows = rows(&MenuInputs {
            cursor: None,
            pipeline: true,
            publish: Err("publish is held: df_monotone: 1 violation".into()),
        });
        assert_eq!(
            find(&rows, "calibration::run_from")
                .reason()
                .map(|r| r.as_ref()),
            Some(NO_CURSOR)
        );
        assert!(find(&rows, "calibration::run_all").is_enabled());
        assert_eq!(
            find(&rows, "calibration::publish")
                .reason()
                .map(|r| r.as_ref()),
            Some("publish is held: df_monotone: 1 violation")
        );
        let none = super::rows(&MenuInputs {
            cursor: Some("parity"),
            pipeline: false,
            publish: Err(NO_PIPELINE.into()),
        });
        for id in [
            "calibration::run_from",
            "calibration::run_all",
            "calibration::publish",
        ] {
            assert_eq!(
                find(&none, id).reason().map(|r| r.as_ref()),
                Some(NO_PIPELINE),
                "{id}"
            );
        }
    }
}
