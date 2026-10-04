//! The `.` action menu's rows, prepared from the tile's state as
//! `geode_tile::menu` rows over action ids. Each row names the action its
//! key dispatches, hints that key (resolved against the live keymap by the
//! menu door), and carries the reason it cannot be picked now. A pick
//! re-enters the tile's dispatch on the row's id, so a row, its key and the
//! palette take one path.

use geode_core::link::Group;
use geode_shell::actions::ActionId;
use geode_tile::menu::{ActionRow, Hint, Row};
use gpui::SharedString;

use super::model::{Kind, State};

/// The action that fixes the differences axis's y domain, or frees it.
pub const FIX_DIFF_Y: &str = "volslice::fix_diff_y";

/// A fix asked while the differences axis shows nothing: nothing to freeze.
pub const NO_DIFF_DOMAIN: &str = "no differences shown";

/// Why a kind's row cannot be picked: nothing of it is loaded.
const NOT_LOADED: &str = "not loaded";

const KIND_ACTIONS: [&str; 3] = ["volslice::kind_1", "volslice::kind_2", "volslice::kind_3"];

/// What the rows are built from, read from the tile when the menu opens
/// and whenever the tile changes under an open menu.
pub struct MenuInputs<'a> {
    pub state: &'a State,
    /// The kinds with something loaded, in header order.
    pub loaded: &'a [Kind],
    /// The group the tile follows: its underlying is the group's.
    pub following: Option<Group>,
    /// Whether the differences axis shows a domain now (its fixed one, or
    /// the autoscaled extent of what the view shows of the differences).
    pub diff_domain: bool,
}

fn row(id: &'static str, title: impl Into<SharedString>) -> ActionRow<ActionId> {
    ActionRow::new(ActionId(id.to_string()), title).hint(Hint::chord(id))
}

fn action(id: &'static str, title: impl Into<SharedString>) -> Row<ActionId> {
    Row::Action(row(id, title))
}

/// The rows, in fixed order: the underlying and the coordinate, the kinds
/// and densities as ticked toggles, the difference chooser and the fixed
/// y-axis toggle, and the view reset.
pub fn rows(i: &MenuInputs) -> Vec<Row<ActionId>> {
    let st = i.state;
    let mut out = vec![
        Row::Action(
            row("volslice::underlying", "Underlying\u{2026}").enabled(match i.following {
                Some(g) => Err(format!("following {}", g.letter()).into()),
                None => Ok(()),
            }),
        ),
        action(
            "volslice::coordinate",
            format!("Coordinate: {}", st.coordinate.name()),
        ),
        Row::Separator,
    ];
    for (kind, id) in Kind::ALL.into_iter().zip(KIND_ACTIONS) {
        out.push(Row::Action(
            row(id, kind.label())
                .checked(!st.hidden.contains(&kind))
                .enabled(if i.loaded.contains(&kind) {
                    Ok(())
                } else {
                    Err(SharedString::new_static(NOT_LOADED))
                }),
        ));
    }
    out.push(Row::Action(
        row("volslice::density", "Densities").checked(st.density),
    ));
    out.push(Row::Separator);
    out.push(action("volslice::diff", "Difference\u{2026}"));
    let fixed = st.diff_ylim.is_some();
    out.push(Row::Action(
        // No default key: unbound, the lane names the `:` verb that sets
        // the domain by value.
        row(FIX_DIFF_Y, "Fix diff y-axis")
            .hint(Hint::chord_or_verb(FIX_DIFF_Y, ":ylim"))
            .checked(fixed)
            .enabled(if fixed || i.diff_domain {
                Ok(())
            } else {
                Err(SharedString::new_static(NO_DIFF_DOMAIN))
            }),
    ));
    out.push(Row::Separator);
    out.push(action("volslice::reset_view", "Reset view"));
    out
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
        let mut state = State::default();
        state.toggle_kind(Kind::Chain);
        state.density = true;
        let loaded = [Kind::Cvi, Kind::Chain];
        let rows = rows(&MenuInputs {
            state: &state,
            loaded: &loaded,
            following: None,
            diff_domain: true,
        });
        assert_eq!(
            ids(&rows),
            [
                "volslice::underlying",
                "volslice::coordinate",
                "volslice::kind_1",
                "volslice::kind_2",
                "volslice::kind_3",
                "volslice::density",
                "volslice::diff",
                FIX_DIFF_Y,
                "volslice::reset_view",
            ]
        );
        assert_eq!(
            find(&rows, "volslice::coordinate").title().as_ref(),
            "Coordinate: moneyness"
        );
        assert_eq!(find(&rows, "volslice::kind_1").tick(), Some(true));
        assert_eq!(
            find(&rows, "volslice::kind_3").tick(),
            Some(false),
            "hidden"
        );
        let draft = find(&rows, "volslice::kind_2");
        assert_eq!(
            draft.reason().map(|r| r.as_ref()),
            Some("not loaded"),
            "only loaded kinds are enabled"
        );
        assert_eq!(find(&rows, "volslice::density").tick(), Some(true));
        let fix = find(&rows, FIX_DIFF_Y);
        assert_eq!((fix.tick(), fix.is_enabled()), (Some(false), true));
        // Unbound by default, the lane names `:ylim`.
        let menu = geode_tile::menu::Menu::new(rows.clone(), &[]);
        let lane = menu
            .rows()
            .iter()
            .filter_map(Row::action)
            .find(|a| a.pick().0 == FIX_DIFF_Y)
            .map(|a| a.lane().clone());
        assert_eq!(lane, Some(geode_tile::menu::Lane::Text(":ylim".into())));
        assert!(find(&rows, "volslice::underlying").is_enabled());
    }

    /// Following, the underlying is the group's; with no differences shown
    /// there is no domain to fix, but a fixed one can always be freed.
    #[test]
    fn rows_that_cannot_act_now_say_why() {
        let mut state = State::default();
        let inputs = |state: &State, diff_domain| {
            rows(&MenuInputs {
                state,
                loaded: &Kind::ALL,
                following: Some(Group::A),
                diff_domain,
            })
        };
        let r = inputs(&state, false);
        assert_eq!(
            find(&r, "volslice::underlying")
                .reason()
                .map(|r| r.as_ref()),
            Some("following A")
        );
        assert_eq!(
            find(&r, FIX_DIFF_Y).reason().map(|r| r.as_ref()),
            Some(NO_DIFF_DOMAIN)
        );
        state.diff_ylim = Some((-0.02, 0.02));
        let r = inputs(&state, false);
        let fix = find(&r, FIX_DIFF_Y);
        assert_eq!((fix.tick(), fix.is_enabled()), (Some(true), true));
    }
}
