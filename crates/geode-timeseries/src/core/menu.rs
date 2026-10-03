//! Prepared menu rows derived from the model: the action list (`.`/`⋯`), the
//! range menu (`r`) and the frequency menu (`f`), as `geode_tile::menu` rows
//! over [`Pick`]. Each row names what a pick does, its hint (an action's live
//! chord, resolved by the door, or a non-key label), its enablement reason,
//! and toggle or choice state. The tile routes every pick through its
//! `MenuHost::menu_pick`:
//! action rows dispatch their action id, range and frequency rows write through
//! the model's own setters (the ones `:range` and `:freq` use). The action list
//! includes selected slot operations and common tile controls; it is not the
//! full action registry.

use geode_core::series::{Frequency, SlotKind};
use geode_shell::actions::ActionId;
use geode_tile::menu::{ActionRow, Hint, MenuPick, Row, first_enabled};
use gpui::SharedString;

use super::model::Model;
use super::range::{Preset, Range};

/// Which menu is open — what the header anchors it under and what the
/// key context's `menu` pair names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    /// The action list, anchored at the header's right edge under `⋯`.
    Actions,
    /// The range presets and `Custom dates…`, under the range trigger.
    Range,
    /// The six frequencies, under the frequency trigger.
    Frequency,
}

impl MenuKind {
    /// The key context's `menu` value: the range menu's own `c` binds
    /// against it.
    pub fn word(self) -> &'static str {
        match self {
            MenuKind::Actions => "actions",
            MenuKind::Range => "range",
            MenuKind::Frequency => "frequency",
        }
    }
}

/// What picking a row does.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// Re-enter the tile's `dispatch` on this action id.
    Action(ActionId),
    /// Write this preset as the range.
    Range(Preset),
    /// Open the custom dates editor.
    CustomRange,
    /// Write this frequency.
    Frequency(Frequency),
}

impl MenuPick for Pick {
    fn element_name(&self) -> SharedString {
        match self {
            Pick::Action(id) => SharedString::from(id.0.clone()),
            Pick::Range(p) => format!("range-{}", p.as_str()).into(),
            Pick::CustomRange => SharedString::new_static("range-custom"),
            Pick::Frequency(f) => format!("freq-{}", f.as_str()).into(),
        }
    }
}

/// Inputs used to prepare menu rows without accessing a retained tile entity.
pub struct MenuInputs<'a> {
    pub model: &'a Model,
}

fn action(
    id: &'static str,
    title: impl Into<SharedString>,
    enabled: Result<(), &'static str>,
) -> Row<Pick> {
    Row::Action(
        ActionRow::new(Pick::Action(ActionId(id.to_string())), title)
            .hint(Hint::chord(id))
            .enabled(enabled.map_err(SharedString::new_static)),
    )
}

fn toggle(id: &'static str, title: &'static str, on: bool) -> Row<Pick> {
    Row::Action(
        ActionRow::new(Pick::Action(ActionId(id.to_string())), title)
            .hint(Hint::chord(id))
            .checked(on),
    )
}

/// Build openers, cursor-slot operations, `Frequency…` (the frequency menu's
/// opener), display toggles, and view reset in fixed order. Empty tiles keep
/// their slot section disabled. Bucket rules require a source and expression
/// editing requires an expression; Color opens a picker while Cycle color
/// advances through the palette.
pub fn rows(i: &MenuInputs, default_source: Option<&str>) -> Vec<Row<Pick>> {
    let m = i.model;
    let mut out = vec![
        action("timeseries::add", "Add series…", Ok(())),
        action("timeseries::expr", "Compose expression…", Ok(())),
        action("timeseries::list", "Series…", Ok(())),
        action("timeseries::range", "Range…", Ok(())),
        Row::Separator,
    ];
    let cursor = m.cursor();
    let slot = m.cursor_slot();
    let heading: SharedString = match cursor {
        Some(index) => m.label(index, default_source).into(),
        None => SharedString::new_static("no series"),
    };
    out.push(Row::Section(heading));
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
    out.push(action("timeseries::color", "Cycle color", none));
    out.push(action("timeseries::pick_color", "Color…", none));
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
    out.push(Row::Separator);
    out.push(action("timeseries::freq", "Frequency…", Ok(())));
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

/// The action id `Custom dates…` shares with its `c` key. The row's hint is
/// that action's live chord, empty when the keymap binds it nowhere: `c` is
/// a keymap binding, not a key the tile reads itself, so a user who unbinds
/// it must not see a stale `c` there.
pub const CUSTOM_RANGE_ACTION: &str = "timeseries::range_custom";

/// The range menu: the seven presets written out, each with its short
/// label (the `:range` spelling) trailing, then a separator and
/// `Custom dates…`. Exactly one row is ticked — the preset in force, or
/// `Custom dates…` while the range is two dates.
pub fn range_rows(current: &Range) -> Vec<Row<Pick>> {
    let mut out: Vec<Row<Pick>> = Preset::ALL
        .into_iter()
        .map(|p| {
            Row::Action(
                ActionRow::new(Pick::Range(p), SharedString::new_static(p.title()))
                    .hint(Hint::label(p.as_str()))
                    .checked(*current == Range::Relative(p)),
            )
        })
        .collect();
    out.push(Row::Separator);
    out.push(Row::Action(
        ActionRow::new(Pick::CustomRange, SharedString::new_static("Custom dates…"))
            .hint(Hint::chord(CUSTOM_RANGE_ACTION))
            .checked(matches!(current, Range::Absolute { .. })),
    ));
    out
}

/// The frequency menu: the six frequencies, finest first, each with its
/// short label trailing and the one in force ticked. `refusal` is the
/// model's own point-cap check over the range in force, asked once per
/// row when the menu is built. Frequencies rejected by that check are
/// disabled and carry its refusal as their reason; the lane shows
/// `OVER_CAP` in its place.
pub fn frequency_rows(
    current: Frequency,
    refusal: impl Fn(Frequency) -> Result<(), String>,
) -> Vec<Row<Pick>> {
    Frequency::ALL
        .into_iter()
        .map(|f| {
            Row::Action(
                ActionRow::new(
                    Pick::Frequency(f),
                    SharedString::new_static(frequency_title(f)),
                )
                .hint(Hint::label(f.as_str()))
                .enabled(refusal(f).map_err(SharedString::from))
                .short_reason(OVER_CAP)
                .checked(f == current),
            )
        })
        .collect()
}

/// A capped frequency row's trailing text; picking the row gives the
/// model's whole refusal as the notice.
const OVER_CAP: &str = "over cap";

/// A frequency written out, for its menu row.
fn frequency_title(f: Frequency) -> &'static str {
    match f {
        Frequency::M1 => "1 minute",
        Frequency::M5 => "5 minutes",
        Frequency::M15 => "15 minutes",
        Frequency::H1 => "1 hour",
        Frequency::D1 => "1 day",
        Frequency::W1 => "1 week",
    }
}

/// Where a menu's highlight starts: on the ticked CHOICE when it can be
/// picked (the value in force — `Custom dates…` while the range is two
/// dates), else on the first enabled row. The action list ticks only
/// toggles, which are not choices, so it starts on its first enabled row.
pub fn start(rows: &[Row<Pick>]) -> Option<usize> {
    rows.iter()
        .position(|r| {
            r.action().is_some_and(|a| {
                a.tick() == Some(true)
                    && a.is_enabled()
                    && matches!(
                        a.pick(),
                        Pick::Range(_) | Pick::CustomRange | Pick::Frequency(_)
                    )
            })
        })
        .or_else(|| first_enabled(rows))
}

/// The `Custom dates…` row's index — where `escape` out of the date editor
/// puts the highlight back.
pub fn custom_row(rows: &[Row<Pick>]) -> Option<usize> {
    rows.iter().position(|r| {
        r.action()
            .is_some_and(|a| matches!(a.pick(), Pick::CustomRange))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::series::expr::Expr;
    use geode_tile::menu::{Menu, Trailing, step};

    fn titles(rows: &[Row<Pick>]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                Row::Action(a) => a.title().to_string(),
                Row::Separator => "---".into(),
                Row::Section(s) => format!("[{s}]"),
            })
            .collect()
    }

    fn row<'a>(rows: &'a [Row<Pick>], id: &str) -> &'a ActionRow<Pick> {
        rows.iter()
            .filter_map(Row::action)
            .find(|a| matches!(a.pick(), Pick::Action(i) if i.0 == id))
            .unwrap_or_else(|| panic!("{id} is a row"))
    }

    fn enabled(rows: &[Row<Pick>], id: &str) -> Result<(), String> {
        match row(rows, id).reason() {
            Some(reason) => Err(reason.to_string()),
            None => Ok(()),
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
                "Cycle color",
                "Color…",
                "Cycle bucket rule",
                "Edit expression…",
                "Remove",
                "---",
                "Frequency…",
                "Density",
                "Percentiles",
                "Reset view",
            ]
        );
        for id in [
            "timeseries::toggle_visible",
            "timeseries::axis_next",
            "timeseries::color",
            "timeseries::pick_color",
            "timeseries::rule",
            "timeseries::edit",
            "timeseries::remove",
        ] {
            assert_eq!(enabled(&rows, id), Err("add a series first".into()), "{id}");
        }
        assert_eq!(enabled(&rows, "timeseries::add"), Ok(()));
        assert_eq!(first_enabled(&rows), Some(0));
    }

    #[test]
    fn the_slot_section_names_the_cursor_and_reads_its_kind_and_visibility() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_expr("SPX.close * 2", Expr::Ref(1)).unwrap();
        // The cursor is on the expression.
        let rows = rows(&MenuInputs { model: &m }, Some("demo_kdb"));
        assert!(titles(&rows).contains(&"[SPX.close * 2]".to_string()));
        assert_eq!(
            enabled(&rows, "timeseries::rule"),
            Err("an expression has no bucket rule".into())
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
            Err("a source is not an expression".into())
        );
    }

    #[test]
    fn the_toggles_carry_their_state_as_a_tick() {
        let mut m = Model::new();
        let checked = |rows: &[Row<Pick>], id: &str| row(rows, id).tick();
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
        assert_eq!(row(&rows, "timeseries::add").tick(), None);
    }

    #[test]
    fn stepping_skips_separators_and_sections_and_clamps() {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        let rows = rows(&MenuInputs { model: &m }, None);
        // Row 4 is the separator, 5 the section: from `Range…` (3) one
        // step down lands on `Hide` (6).
        assert_eq!(step(&rows, Some(3), 1), Some(6));
        assert_eq!(step(&rows, Some(6), -1), Some(3));
        // On a source `Edit expression…` (11) is greyed: stepped over.
        assert_eq!(step(&rows, Some(10), 1), Some(12));
        assert_eq!(step(&rows, Some(12), -1), Some(10));
        assert_eq!(step(&rows, Some(0), -1), Some(0), "clamped at the top");
        let last = rows.len() - 1;
        assert_eq!(
            step(&rows, Some(last), 1),
            Some(last),
            "clamped at the bottom"
        );
        assert_eq!(
            step(&rows, Some(4), 1),
            Some(0),
            "from a non-row: the first enabled"
        );
    }

    /// Disabled slot actions are skipped, so an empty tile moves directly from
    /// Range to Frequency….
    #[test]
    fn stepping_skips_disabled_rows() {
        let m = Model::new();
        let rows = rows(&MenuInputs { model: &m }, None);
        assert_eq!(
            step(&rows, Some(3), 1),
            Some(14),
            "over the greyed slot section"
        );
        assert_eq!(step(&rows, Some(14), -1), Some(3));
        assert_eq!(
            step(&rows, Some(8), 1),
            Some(14),
            "a highlight the pointer left on a greyed row steps from it"
        );
        assert_eq!(
            step(&rows, Some(8), 0),
            Some(8),
            "a zero step keeps it there"
        );
    }

    fn pick_of(rows: &[Row<Pick>], i: usize) -> &Pick {
        match &rows[i] {
            Row::Action(a) => a.pick(),
            other => panic!("row {i} is not a row: {other:?}"),
        }
    }

    fn checked_of(rows: &[Row<Pick>], i: usize) -> Option<bool> {
        rows[i].action().and_then(|a| a.tick())
    }

    /// Rows as an open menu holds them: hints resolved against the keymap
    /// this module ships.
    fn resolved(rows: Vec<Row<Pick>>) -> Vec<Row<Pick>> {
        Menu::new(rows, &crate::content::test_bindings(None))
            .rows()
            .to_vec()
    }

    /// The trailing lane as a test reads it: text as itself, keys by their
    /// keymap spelling (`[c]`).
    fn trail(row: &Row<Pick>) -> String {
        match row.action().map(|a| a.trailing()) {
            Some(Trailing::Text(t)) => t.to_string(),
            Some(Trailing::Keys(keys)) => keys.iter().map(|k| format!("[{}]", k.key)).collect(),
            Some(Trailing::None) | None => String::new(),
        }
    }

    fn hint_of(rows: &[Row<Pick>], i: usize) -> String {
        trail(&rows[i])
    }

    #[test]
    fn the_range_menu_writes_the_presets_out_ticks_the_current_and_ends_on_custom() {
        let rows = resolved(range_rows(&Range::Relative(Preset::M3)));
        assert_eq!(
            titles(&rows),
            vec![
                "1 week",
                "1 month",
                "3 months",
                "6 months",
                "1 year",
                "2 years",
                "5 years",
                "---",
                "Custom dates…",
            ]
        );
        let hints: Vec<String> = (0..rows.len()).map(|i| hint_of(&rows, i)).collect();
        assert_eq!(
            hints,
            vec!["1w", "1m", "3m", "6m", "1y", "2y", "5y", "", "[c]"]
        );
        assert_eq!(pick_of(&rows, 2), &Pick::Range(Preset::M3));
        assert_eq!(pick_of(&rows, 8), &Pick::CustomRange);
        // Every row carries the tick column, so the titles line up.
        let ticks: Vec<Option<bool>> = (0..rows.len()).map(|i| checked_of(&rows, i)).collect();
        assert_eq!(
            ticks,
            vec![
                Some(false),
                Some(false),
                Some(true),
                Some(false),
                Some(false),
                Some(false),
                Some(false),
                None,
                Some(false),
            ]
        );
        assert_eq!(
            start(&rows),
            Some(2),
            "the highlight starts on the current range"
        );
    }

    #[test]
    fn an_absolute_range_ticks_custom_dates_and_starts_there() {
        let absolute = Range::Absolute {
            from: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            to: chrono::NaiveDate::from_ymd_opt(2026, 2, 5).unwrap(),
        };
        let rows = range_rows(&absolute);
        let custom = rows.len() - 1;
        assert_eq!(checked_of(&rows, custom), Some(true));
        assert!((0..7).all(|i| checked_of(&rows, i) == Some(false)));
        assert_eq!(start(&rows), Some(custom));
        assert_eq!(custom_row(&rows), Some(custom));
    }

    #[test]
    fn the_frequency_menu_ticks_the_current_and_disables_what_the_cap_refuses() {
        let rows = resolved(frequency_rows(Frequency::H1, |f| match f {
            Frequency::M1 => Err("1m over 1y is 525,600 points; the cap is 500,000".into()),
            _ => Ok(()),
        }));
        assert_eq!(
            titles(&rows),
            vec![
                "1 minute",
                "5 minutes",
                "15 minutes",
                "1 hour",
                "1 day",
                "1 week"
            ]
        );
        let hints: Vec<String> = (0..rows.len()).map(|i| hint_of(&rows, i)).collect();
        assert_eq!(hints, vec!["over cap", "5m", "15m", "1h", "1d", "1w"]);
        assert_eq!(pick_of(&rows, 0), &Pick::Frequency(Frequency::M1));
        assert_eq!(
            rows[0].action().unwrap().reason().cloned(),
            Some(SharedString::from(
                "1m over 1y is 525,600 points; the cap is 500,000"
            )),
            "a capped frequency is disabled with the cap's own reason"
        );
        assert_eq!(
            trail(&rows[0]),
            "over cap",
            "the trailing column carries a short reason; a pick's notice the full one"
        );
        assert_eq!(trail(&rows[3]), "1h");
        assert_eq!(checked_of(&rows, 3), Some(true));
        assert_eq!(checked_of(&rows, 4), Some(false));
        assert_eq!(
            start(&rows),
            Some(3),
            "the highlight starts on the current frequency"
        );
        assert_eq!(
            step(&rows, Some(1), -1),
            Some(1),
            "the capped row is stepped over"
        );
    }
}
