//! Prepared menu rows derived from the model: the action list (`.`/`⋯`), the
//! range menu (`r`) and the frequency menu (`f`). Each row names what a pick
//! does, its enablement reason, and toggle or choice state. The tile adds
//! binding hints and routes every pick through `TimeseriesTile::menu_pick`:
//! action rows dispatch their action id, range and frequency rows write through
//! the model's own setters (the ones `:range` and `:freq` use). The action list
//! includes selected slot operations and common tile controls; it is not the
//! full action registry.

use geode_core::series::{Frequency, SlotKind};
use geode_shell::actions::ActionId;
use geode_shell::keymap::{Keystroke, Modifiers};
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

/// One row of a menu.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuRow {
    Action {
        pick: Pick,
        title: SharedString,
        /// Keys for the trailing lane, painted as `Kbd`: an action's binding
        /// filled by the tile when rows are prepared (empty if absent), or
        /// `Custom dates…`'s `c`.
        hint: Vec<Keystroke>,
        /// Trailing text that is NOT a key — a preset's or a frequency's
        /// short label (`1w`, `1d`). Painted as text in place of `hint`.
        label: Option<SharedString>,
        /// `Err` names why the row cannot be picked; a pick makes it the
        /// notice, and the trailing lane shows it unless `short_reason`
        /// stands in for it there.
        enabled: Result<(), SharedString>,
        /// A disabled row's trailing text when its full reason is a sentence
        /// too long for the lane (the point cap's): the row stays narrow and
        /// a pick still gives the whole reason.
        short_reason: Option<SharedString>,
        /// `None` for a verb; `Some(on)` for a toggle or a choice, which
        /// paints a tick (or a same-width blank) ahead of its title.
        checked: Option<bool>,
    },
    Separator,
    /// A muted heading over the rows that follow.
    Section(SharedString),
}

/// What a row's trailing lane paints.
#[derive(Clone, Debug, PartialEq)]
pub enum Trailing<'a> {
    /// Keys, painted as `Kbd` (empty when the action has no binding).
    Keys(&'a [Keystroke]),
    /// Text that is not a key: a short label or a disabled row's reason.
    Text(SharedString),
}

impl MenuRow {
    /// A disabled row's short reason (or its reason, when that is short
    /// already); else its non-key label; else its keys. `None` for a
    /// separator or a heading.
    pub fn trailing(&self) -> Option<Trailing<'_>> {
        match self {
            MenuRow::Action {
                enabled,
                hint,
                label,
                short_reason,
                ..
            } => Some(match (enabled, label) {
                (Err(reason), _) => {
                    Trailing::Text(short_reason.clone().unwrap_or_else(|| reason.clone()))
                }
                (Ok(()), Some(label)) => Trailing::Text(label.clone()),
                (Ok(()), None) => Trailing::Keys(hint),
            }),
            _ => None,
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
) -> MenuRow {
    MenuRow::Action {
        pick: Pick::Action(ActionId(id.to_string())),
        title: title.into(),
        hint: Vec::new(),
        label: None,
        enabled: enabled.map_err(SharedString::new_static),
        short_reason: None,
        checked: None,
    }
}

fn toggle(id: &'static str, title: &'static str, on: bool) -> MenuRow {
    MenuRow::Action {
        pick: Pick::Action(ActionId(id.to_string())),
        title: SharedString::new_static(title),
        hint: Vec::new(),
        label: None,
        enabled: Ok(()),
        short_reason: None,
        checked: Some(on),
    }
}

/// Build openers, cursor-slot operations, `Frequency…` (the frequency menu's
/// opener), display toggles, and view reset in fixed order. Empty tiles keep
/// their slot section disabled. Bucket rules require a source and expression
/// editing requires an expression; Color opens a picker while Cycle color
/// advances through the palette.
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
    out.push(MenuRow::Separator);
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

/// The action id `Custom dates…` shares with its `c` key. The pure row
/// carries the shipped key; the tile swaps in the live chord.
pub const CUSTOM_RANGE_ACTION: &str = "timeseries::range_custom";

/// The range menu: the seven presets written out, each with its short
/// label (the `:range` spelling) trailing, then a separator and
/// `Custom dates…`. Exactly one row is ticked — the preset in force, or
/// `Custom dates…` while the range is two dates.
pub fn range_rows(current: &Range) -> Vec<MenuRow> {
    let mut out: Vec<MenuRow> = Preset::ALL
        .into_iter()
        .map(|p| MenuRow::Action {
            pick: Pick::Range(p),
            title: SharedString::new_static(p.title()),
            hint: Vec::new(),
            label: Some(SharedString::new_static(p.as_str())),
            enabled: Ok(()),
            short_reason: None,
            checked: Some(*current == Range::Relative(p)),
        })
        .collect();
    out.push(MenuRow::Separator);
    out.push(MenuRow::Action {
        pick: Pick::CustomRange,
        title: SharedString::new_static("Custom dates…"),
        hint: vec![Keystroke {
            mods: Modifiers::NONE,
            key: "c".to_string(),
        }],
        label: None,
        enabled: Ok(()),
        short_reason: None,
        checked: Some(matches!(current, Range::Absolute { .. })),
    });
    out
}

/// The frequency menu: the six frequencies, finest first, each with its
/// short label trailing and the one in force ticked. `refusal` is the
/// model's own point-cap check over the range in force, asked once per
/// row when the menu is built: a frequency it refuses is a disabled row
/// carrying that refusal as its reason, never a row that looks pickable
/// and then fails.
pub fn frequency_rows(
    current: Frequency,
    refusal: impl Fn(Frequency) -> Result<(), String>,
) -> Vec<MenuRow> {
    Frequency::ALL
        .into_iter()
        .map(|f| MenuRow::Action {
            pick: Pick::Frequency(f),
            title: SharedString::new_static(frequency_title(f)),
            hint: Vec::new(),
            label: Some(SharedString::new_static(f.as_str())),
            enabled: refusal(f).map_err(SharedString::from),
            short_reason: Some(SharedString::new_static(OVER_CAP)),
            checked: Some(f == current),
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
/// toggles, which are not choices, so it starts on its first enabled
/// row.
pub fn start(rows: &[MenuRow]) -> usize {
    rows.iter()
        .position(|r| {
            matches!(
                r,
                MenuRow::Action {
                    checked: Some(true),
                    pick: Pick::Range(_) | Pick::CustomRange | Pick::Frequency(_),
                    enabled: Ok(()),
                    ..
                }
            )
        })
        .unwrap_or_else(|| first_enabled(rows))
}

/// The `Custom dates…` row's index — where `escape` out of the date
/// editor puts the highlight back.
pub fn custom_row(rows: &[MenuRow]) -> Option<usize> {
    rows.iter().position(|r| {
        matches!(
            r,
            MenuRow::Action {
                pick: Pick::CustomRange,
                ..
            }
        )
    })
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

/// Move delta enabled actions, skipping disabled rows, headings, and separators.
/// Clamp at either end. Zero retains any Action row, including a disabled row
/// selected by the pointer. From a non-action, return first_enabled. From a
/// disabled action with no enabled row in the requested direction, stay put.
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
            .find(|r| matches!(r, MenuRow::Action { pick: Pick::Action(i), .. } if i.0 == id))
            .unwrap_or_else(|| panic!("{id} is a row"))
    }

    fn enabled(rows: &[MenuRow], id: &str) -> Result<(), String> {
        match row(rows, id) {
            MenuRow::Action { enabled, .. } => enabled.clone().map_err(|e| e.to_string()),
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

    /// Disabled slot actions are skipped, so an empty tile moves directly from
    /// Range to Frequency….
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

    fn pick_of(rows: &[MenuRow], i: usize) -> &Pick {
        match &rows[i] {
            MenuRow::Action { pick, .. } => pick,
            other => panic!("row {i} is not a row: {other:?}"),
        }
    }

    fn checked_of(rows: &[MenuRow], i: usize) -> Option<bool> {
        match &rows[i] {
            MenuRow::Action { checked, .. } => *checked,
            _ => None,
        }
    }

    /// The trailing lane as a test reads it: text as itself, keys by their
    /// keymap spelling (`[c]`).
    fn trail(row: &MenuRow) -> String {
        match row.trailing() {
            Some(Trailing::Text(t)) => t.to_string(),
            Some(Trailing::Keys(keys)) => keys.iter().map(|k| format!("[{}]", k.key)).collect(),
            None => String::new(),
        }
    }

    fn hint_of(rows: &[MenuRow], i: usize) -> String {
        trail(&rows[i])
    }

    #[test]
    fn the_range_menu_writes_the_presets_out_ticks_the_current_and_ends_on_custom() {
        let rows = range_rows(&Range::Relative(Preset::M3));
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
        assert_eq!(start(&rows), 2, "the highlight starts on the current range");
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
        assert_eq!(start(&rows), custom);
        assert_eq!(custom_row(&rows), Some(custom));
    }

    #[test]
    fn the_frequency_menu_ticks_the_current_and_disables_what_the_cap_refuses() {
        let rows = frequency_rows(Frequency::H1, |f| match f {
            Frequency::M1 => Err("1m over 1y is 525,600 points; the cap is 500,000".into()),
            _ => Ok(()),
        });
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
            match &rows[0] {
                MenuRow::Action { enabled, .. } => enabled.clone(),
                _ => unreachable!(),
            },
            Err(SharedString::from(
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
            3,
            "the highlight starts on the current frequency"
        );
        assert_eq!(step(&rows, 1, -1), 1, "the capped row is stepped over");
    }
}
