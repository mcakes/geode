//! Prepared action-menu rows derived from the model: titles, enablement
//! reasons, and toggle state. The tile adds binding hints and dispatches enabled
//! picks through its ordinary action handler. The menu includes selected slot
//! operations and common tile controls; it is not the full action registry.

use geode_core::series::SlotKind;
use geode_shell::actions::ActionId;
use gpui::SharedString;

use super::model::Model;

/// One row of the action list.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuRow {
    Action {
        id: ActionId,
        title: SharedString,
        /// Binding hint filled by the tile when rows are prepared; empty if absent.
        hint: SharedString,
        enabled: Result<(), &'static str>,
        /// `None` for a verb; `Some(on)` for a toggle, which paints a
        /// tick (or a same-width blank) ahead of its title.
        checked: Option<bool>,
    },
    Separator,
    /// A muted heading over the rows that follow.
    Section(SharedString),
}

/// Inputs used to prepare menu rows without accessing a retained tile entity.
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
        hint: SharedString::default(),
        enabled,
        checked: None,
    }
}

fn toggle(id: &'static str, title: &'static str, on: bool) -> MenuRow {
    MenuRow::Action {
        id: ActionId(id.to_string()),
        title: SharedString::new_static(title),
        hint: SharedString::default(),
        enabled: Ok(()),
        checked: Some(on),
    }
}

/// Build openers, cursor-slot operations, frequency steps, display toggles,
/// and view reset in fixed order. Empty tiles keep the slot section but disable
/// its actions. Bucket-rule changes require a source; expression editing requires
/// an expression. These reasons explain why a row cannot currently act.
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

/// Move over Action rows, including disabled actions, clamping at the ends.
/// Skip separators/headings. If `from` is not an action, return the first enabled
/// row (or zero when none is enabled).
pub fn step(rows: &[MenuRow], from: usize, delta: isize) -> usize {
    let actionable: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, MenuRow::Action { .. }).then_some(i))
        .collect();
    let Some(pos) = actionable.iter().position(|&i| i == from) else {
        return first_enabled(rows);
    };
    let last = actionable.len().saturating_sub(1) as isize;
    let next = (pos as isize + delta).clamp(0, last);
    actionable[next as usize]
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
        let m = Model::new();
        let rows = rows(&MenuInputs { model: &m }, None);
        // Row 4 is the separator, 5 the section: from `Range…` (3) one
        // step down lands on `Hide` (6).
        assert_eq!(step(&rows, 3, 1), 6);
        assert_eq!(step(&rows, 6, -1), 3);
        assert_eq!(step(&rows, 0, -1), 0, "clamped at the top");
        let last = rows.len() - 1;
        assert_eq!(step(&rows, last, 1), last, "clamped at the bottom");
        assert_eq!(step(&rows, 4, 1), 0, "from a non-row: the first enabled");
    }
}
