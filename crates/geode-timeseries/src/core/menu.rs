//! The action list's pure core (mouse pass, 2026-09-24): the tile's
//! verbs as an ordered list of rows, each naming the action it
//! dispatches, whether it is pickable and why not, and — for a
//! toggle — whether it is in force. No gpui, no entity: `popup.rs`
//! paints it, the tile decides when it opens, and every row ends in
//! `TimeseriesTile::dispatch` on its own action id, so a menu row, a
//! key and the palette take one path (the coding guide's "model one
//! logical command once").
//!
//! The market-data panel's `core::menu` is the shape; this one adds the
//! cursor-slot section, whose rows read the slot under the cursor.

use geode_core::series::SlotKind;
use geode_shell::actions::ActionId;
use geode_shell::keymap::Keystroke;
use gpui::SharedString;

use super::model::Model;

/// One row of the action list.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuRow {
    Action {
        id: ActionId,
        title: SharedString,
        /// The live chord, resolved by the tile at open; empty when the
        /// keymap has none.
        hint: Vec<Keystroke>,
        enabled: Result<(), &'static str>,
        /// `None` for a verb; `Some(on)` for a toggle, which paints a
        /// tick (or a same-width blank) ahead of its title.
        checked: Option<bool>,
    },
    Separator,
    /// A muted heading over the rows that follow.
    Section(SharedString),
}

/// What a row reads off the tile beyond the model: nothing yet, but
/// spelled as a struct so the next input is a field, not a signature
/// change at every call site.
pub struct MenuInputs<'a> {
    pub model: &'a Model,
}

fn action(
    id: &'static str,
    title: impl Into<SharedString>,
    enabled: Result<(), &'static str>,
) -> MenuRow {
    MenuRow::Action {
        id: ActionId(id.to_string()),
        title: title.into(),
        hint: Vec::new(),
        enabled,
        checked: None,
    }
}

fn toggle(id: &'static str, title: &'static str, on: bool) -> MenuRow {
    MenuRow::Action {
        id: ActionId(id.to_string()),
        title: SharedString::new_static(title),
        hint: Vec::new(),
        enabled: Ok(()),
        checked: Some(on),
    }
}

/// The action list, in order: the four openers; then, under a heading
/// naming the cursor's slot, the seven slot verbs (all disabled with
/// `no series` while the tile holds none — the section stays so the
/// list keeps one shape); then the frequency steps, the two toggles
/// (ticked when on) and the view reset.
///
/// A row's enablement is the same refusal its key would give: `Edit
/// expression…` on a source slot says what `e` says, and `Cycle bucket
/// rule` on an expression is disabled rather than silently inert as
/// `b` is.
pub fn rows(i: &MenuInputs, default_source: Option<&str>) -> Vec<MenuRow> {
    let m = i.model;
    let mut out = vec![
        action("timeseries::add", "Add series…", Ok(())),
        action("timeseries::expr", "Compose expression…", Ok(())),
        action("timeseries::list", "Series…", Ok(())),
        action("timeseries::range", "Range…", Ok(())),
        MenuRow::Separator,
    ];
    let cursor = m.cursor();
    let slot = m.cursor_slot();
    let heading: SharedString = match cursor {
        Some(index) => m.label(index, default_source).into(),
        None => SharedString::new_static("no series"),
    };
    out.push(MenuRow::Section(heading));
    let none: Result<(), &'static str> = match slot {
        Some(_) => Ok(()),
        None => Err("add a series first"),
    };
    let is_source = slot.is_some_and(|s| matches!(s.kind, SlotKind::Source { .. }));
    let visible = slot.is_none_or(|s| s.visible);
    out.push(action(
        "timeseries::toggle_visible",
        if visible { "Hide" } else { "Show" },
        none,
    ));
    out.push(action("timeseries::axis_next", "Cycle axis", none));
    out.push(action("timeseries::colour", "Cycle colour", none));
    out.push(action("timeseries::pick_colour", "Colour…", none));
    out.push(action(
        "timeseries::rule",
        "Cycle bucket rule",
        match (slot, is_source) {
            (None, _) => none,
            (Some(_), true) => Ok(()),
            (Some(_), false) => Err("an expression has no bucket rule"),
        },
    ));
    out.push(action(
        "timeseries::edit",
        "Edit expression…",
        match (slot, is_source) {
            (None, _) => none,
            (Some(_), false) => Ok(()),
            (Some(_), true) => Err("a source is not an expression"),
        },
    ));
    out.push(action("timeseries::remove", "Remove", none));
    out.push(MenuRow::Separator);
    out.push(action("timeseries::freq_finer", "Finer frequency", Ok(())));
    out.push(action(
        "timeseries::freq_coarser",
        "Coarser frequency",
        Ok(()),
    ));
    out.push(toggle(
        "timeseries::density",
        "Density",
        m.density().is_some(),
    ));
    out.push(toggle(
        "timeseries::percentiles",
        "Percentiles",
        !m.percentiles().is_empty(),
    ));
    out.push(action("timeseries::reset_view", "Reset view", Ok(())));
    out
}

/// The first row worth landing the highlight on — the first enabled
/// `Action`, or `0` if none is.
pub fn first_enabled(rows: &[MenuRow]) -> usize {
    rows.iter().position(lands).unwrap_or(0)
}

fn lands(r: &MenuRow) -> bool {
    matches!(
        r,
        MenuRow::Action {
            enabled: Ok(()),
            ..
        }
    )
}

/// Move `delta` ENABLED `Action` rows from `from` — a disabled row, a
/// `Separator` and a `Section` are all stepped over — clamped at either
/// end rather than wrapping (the market-data menu's rule: a list read
/// top-down does not jump to its far end). `delta == 0` is the refresh
/// case: the highlight stays on its row while that row is still an
/// `Action` (the pointer may have left it on a disabled one), else lands
/// on the first enabled row. A highlight on a non-enabled row steps from
/// where it stands; with no enabled row further that way it stays put.
pub fn step(rows: &[MenuRow], from: usize, delta: isize) -> usize {
    if !matches!(rows.get(from), Some(MenuRow::Action { .. })) {
        return first_enabled(rows);
    }
    let mut at = from;
    for _ in 0..delta.unsigned_abs() {
        let next = if delta > 0 {
            (at + 1..rows.len()).find(|&i| lands(&rows[i]))
        } else {
            (0..at).rev().find(|&i| lands(&rows[i]))
        };
        match next {
            Some(i) => at = i,
            None => break,
        }
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::series::expr::Expr;

    fn titles(rows: &[MenuRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                MenuRow::Action { title, .. } => title.to_string(),
                MenuRow::Separator => "---".into(),
                MenuRow::Section(s) => format!("[{s}]"),
            })
            .collect()
    }

    fn row<'a>(rows: &'a [MenuRow], id: &str) -> &'a MenuRow {
        rows.iter()
            .find(|r| matches!(r, MenuRow::Action { id: i, .. } if i.0 == id))
            .unwrap_or_else(|| panic!("{id} is a row"))
    }

    fn enabled(rows: &[MenuRow], id: &str) -> Result<(), &'static str> {
        match row(rows, id) {
            MenuRow::Action { enabled, .. } => *enabled,
            _ => unreachable!(),
        }
    }

    #[test]
    fn an_empty_tile_lists_every_verb_and_disables_the_slot_section() {
        let m = Model::new();
        let rows = rows(&MenuInputs { model: &m }, None);
        assert_eq!(
            titles(&rows),
            vec![
                "Add series…",
                "Compose expression…",
                "Series…",
                "Range…",
                "---",
                "[no series]",
                "Hide",
                "Cycle axis",
                "Cycle colour",
                "Colour…",
                "Cycle bucket rule",
                "Edit expression…",
                "Remove",
                "---",
                "Finer frequency",
                "Coarser frequency",
                "Density",
                "Percentiles",
                "Reset view",
            ]
        );
        for id in [
            "timeseries::toggle_visible",
            "timeseries::axis_next",
            "timeseries::colour",
            "timeseries::pick_colour",
            "timeseries::rule",
            "timeseries::edit",
            "timeseries::remove",
        ] {
            assert_eq!(enabled(&rows, id), Err("add a series first"), "{id}");
        }
        assert_eq!(enabled(&rows, "timeseries::add"), Ok(()));
        assert_eq!(first_enabled(&rows), 0);
    }

    #[test]
    fn the_slot_section_names_the_cursor_and_reads_its_kind_and_visibility() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_expr("s1 * 2", Expr::Ref(1)).unwrap();
        // The cursor is on the expression.
        let rows = rows(&MenuInputs { model: &m }, Some("demo_kdb"));
        assert!(titles(&rows).contains(&"[s1 * 2]".to_string()));
        assert_eq!(
            enabled(&rows, "timeseries::rule"),
            Err("an expression has no bucket rule")
        );
        assert_eq!(enabled(&rows, "timeseries::edit"), Ok(()));
        assert_eq!(enabled(&rows, "timeseries::remove"), Ok(()));
        m.set_cursor(0);
        m.set_visible(1, false).unwrap();
        let rows = super::rows(&MenuInputs { model: &m }, Some("demo_kdb"));
        assert!(titles(&rows).contains(&"[SPX.close]".to_string()));
        assert!(titles(&rows).contains(&"Show".to_string()), "hidden → Show");
        assert_eq!(enabled(&rows, "timeseries::rule"), Ok(()));
        assert_eq!(
            enabled(&rows, "timeseries::edit"),
            Err("a source is not an expression")
        );
    }

    #[test]
    fn the_toggles_carry_their_state_as_a_tick() {
        let mut m = Model::new();
        let checked = |rows: &[MenuRow], id: &str| match row(rows, id) {
            MenuRow::Action { checked, .. } => *checked,
            _ => unreachable!(),
        };
        let rows = rows(&MenuInputs { model: &m }, None);
        let density = m.density().is_some();
        let percentiles = !m.percentiles().is_empty();
        assert_eq!(checked(&rows, "timeseries::density"), Some(density));
        assert_eq!(checked(&rows, "timeseries::percentiles"), Some(percentiles));
        m.toggle_density();
        m.toggle_percentiles();
        let rows = super::rows(&MenuInputs { model: &m }, None);
        assert_eq!(checked(&rows, "timeseries::density"), Some(!density));
        assert_eq!(
            checked(&rows, "timeseries::percentiles"),
            Some(!percentiles)
        );
        assert!(matches!(
            row(&rows, "timeseries::add"),
            MenuRow::Action { checked: None, .. }
        ));
    }

    #[test]
    fn stepping_skips_separators_and_sections_and_clamps() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        let rows = rows(&MenuInputs { model: &m }, None);
        // Row 4 is the separator, 5 the section: from `Range…` (3) one
        // step down lands on `Hide` (6).
        assert_eq!(step(&rows, 3, 1), 6);
        assert_eq!(step(&rows, 6, -1), 3);
        // On a source `Edit expression…` (11) is greyed: stepped over.
        assert_eq!(step(&rows, 10, 1), 12);
        assert_eq!(step(&rows, 12, -1), 10);
        assert_eq!(step(&rows, 0, -1), 0, "clamped at the top");
        let last = rows.len() - 1;
        assert_eq!(step(&rows, last, 1), last, "clamped at the bottom");
        assert_eq!(step(&rows, 4, 1), 0, "from a non-row: the first enabled");
    }

    /// A disabled row is stepped over like a separator (user report
    /// 2026-09-25): on an empty tile the whole slot section is greyed, so
    /// `j` from `Range…` lands on `Finer frequency`.
    #[test]
    fn stepping_skips_disabled_rows() {
        let m = Model::new();
        let rows = rows(&MenuInputs { model: &m }, None);
        assert_eq!(step(&rows, 3, 1), 14, "over the greyed slot section");
        assert_eq!(step(&rows, 14, -1), 3);
        assert_eq!(
            step(&rows, 8, 1),
            14,
            "a highlight the pointer left on a greyed row steps from it"
        );
        assert_eq!(step(&rows, 8, 0), 8, "a refresh keeps it there");
    }
}
