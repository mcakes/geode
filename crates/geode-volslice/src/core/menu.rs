//! The `.` action menu's rows, prepared from the tile's state as
//! `geode_tile::menu` rows over action ids. Each row names the action its
//! key dispatches, hints that key (resolved against the live keymap by the
//! menu door), and carries the reason it cannot be picked now. A pick
//! re-enters the tile's dispatch on the row's id, so a row, its key and the
//! palette take one path.

use geode_core::link::Group;
use geode_core::vol::Coordinate;
use geode_shell::actions::ActionId;
use geode_tile::menu::{ActionRow, Hint, MenuPick, Row};
use gpui::SharedString;

use super::model::{Kind, State};

/// What a menu row does when picked: the action menu's rows re-enter the
/// tile's dispatch on an action id; the coordinate menu's rows set one
/// coordinate outright, which no single action names (`x` cycles).
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Action(ActionId),
    Coordinate(Coordinate),
}

impl MenuPick for Pick {
    fn element_name(&self) -> SharedString {
        match self {
            Pick::Action(id) => SharedString::from(id.0.clone()),
            Pick::Coordinate(c) => format!("coordinate-{}", c.name()).into(),
        }
    }
}

/// The action that fixes the differences axis's y domain, or frees it.
pub const FIX_DIFF_Y: &str = "volslice::fix_diff_y";

/// A fix asked while the differences axis shows nothing: nothing to freeze.
pub const NO_DIFF_DOMAIN: &str = "no differences shown";

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

fn row(id: &'static str, title: impl Into<SharedString>) -> ActionRow<Pick> {
    ActionRow::new(Pick::Action(ActionId(id.to_string())), title).hint(Hint::chord(id))
}

fn action(id: &'static str, title: impl Into<SharedString>) -> Row<Pick> {
    Row::Action(row(id, title))
}

/// The coordinate menu's rows, one per coordinate in `x`'s cycling
/// order, the shown one ticked. Opened from the header's coordinate
/// trigger; a pick sets that coordinate as `:x <name>` does. No key lane:
/// the keyboard's route is `x`, which cycles through every row.
pub fn coordinate_rows(current: Coordinate) -> Vec<Row<Pick>> {
    Coordinate::ALL
        .into_iter()
        .map(|c| Row::Action(ActionRow::new(Pick::Coordinate(c), c.name()).checked(c == current)))
        .collect()
}

/// The rows, in fixed order: the underlying and the coordinate, the kinds
/// and densities as ticked toggles, the difference chooser and the fixed
/// y-axis toggle, and the view reset.
pub fn rows(i: &MenuInputs) -> Vec<Row<Pick>> {
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
    // Always enabled, as the digit is: a kind with nothing loaded keeps
    // its choice, so one hidden before it arrives arrives hidden.
    for (kind, id) in Kind::ALL.into_iter().zip(KIND_ACTIONS) {
        let title: SharedString = if i.loaded.contains(&kind) {
            SharedString::new_static(kind.label())
        } else {
            format!("{} (not loaded)", kind.label()).into()
        };
        out.push(Row::Action(
            row(id, title).checked(!st.hidden.contains(&kind)),
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
        row(FIX_DIFF_Y, "Fix difference y-axis")
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

    fn id_of(pick: &Pick) -> &str {
        match pick {
            Pick::Action(id) => id.0.as_str(),
            Pick::Coordinate(c) => c.name(),
        }
    }

    fn ids(rows: &[Row<Pick>]) -> Vec<&str> {
        rows.iter()
            .filter_map(Row::action)
            .map(|a| id_of(a.pick()))
            .collect()
    }

    fn find<'a>(rows: &'a [Row<Pick>], id: &str) -> &'a ActionRow<Pick> {
        rows.iter()
            .filter_map(Row::action)
            .find(|a| id_of(a.pick()) == id)
            .unwrap_or_else(|| panic!("{id} has a row"))
    }

    /// One row per coordinate in `x`'s order, the current one ticked and
    /// none with a key lane.
    #[test]
    fn the_coordinate_rows_tick_the_shown_coordinate() {
        let rows = coordinate_rows(Coordinate::Delta);
        assert_eq!(
            ids(&rows),
            ["strike", "moneyness", "log-moneyness", "delta"],
            "`Coordinate::ALL`, the order `x` cycles in"
        );
        assert_eq!(find(&rows, "delta").tick(), Some(true));
        assert_eq!(find(&rows, "strike").tick(), Some(false));
        assert!(rows.iter().filter_map(Row::action).all(|a| a.is_enabled()));
        assert_eq!(
            find(&rows, "delta").pick(),
            &Pick::Coordinate(Coordinate::Delta)
        );
        // With `x` bound to the cycling action, a row that carried its
        // chord would show it; none does.
        let x = geode_shell::keymap::Binding {
            keystrokes: geode_shell::keymap::parse_binding(
                "x",
                geode_shell::keymap::Modifiers::NONE,
            )
            .unwrap(),
            predicate: None,
            action: ActionId("volslice::coordinate".into()),
            layer: geode_core::config::Layer::Builtin,
            index: 0,
            context_source: None,
            key_source: "x".into(),
        };
        let menu = geode_tile::menu::Menu::new(rows, &[x]);
        assert!(
            menu.rows()
                .iter()
                .filter_map(Row::action)
                .all(|a| *a.lane() == geode_tile::menu::Lane::Empty),
            "no key lane: `x` cycles"
        );
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
        assert_eq!(draft.title().as_ref(), "cvi draft (not loaded)");
        assert!(
            draft.is_enabled(),
            "the digit acts on a kind not loaded, so the row does"
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
            .find(|a| id_of(a.pick()) == FIX_DIFF_Y)
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
