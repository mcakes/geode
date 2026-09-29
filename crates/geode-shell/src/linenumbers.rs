//! The `[ui] line_numbers` setting and the numbering rule tiles paint.
//!
//! The shell owns the setting and publishes [`UiSettings`] at startup and
//! on changes from settings, `ui::line_numbers_cycle`, or config reload.
//! Modules read it with `cx.try_global::<UiSettings>()` and subscribe with
//! `cx.observe_global::<UiSettings>()`; they do not access `ShellView`.
//! This global carries UI changes independently of document reload events.
//!
//! [`gutter_number`] counts visible rows after grouping, expansion, and
//! narrowing. `on` shows the one-based row index used by `NG`; `rel` shows
//! distance from the cursor except on the cursor row, which retains its
//! absolute number. This lets both absolute and relative motion be read
//! from the gutter.

use std::path::Path;

use geode_core::config::{Config, Layer};
use toml_edit::{Item, Table, value};

/// Whether and how a tile numbers its rows (`[ui] line_numbers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineNumbers {
    #[default]
    Off,
    On,
    Relative,
}

impl LineNumbers {
    /// Display and stepping order for the settings control and
    /// `ui::line_numbers_cycle`.
    pub const ALL: [LineNumbers; 3] = [LineNumbers::Off, LineNumbers::On, LineNumbers::Relative];

    /// Label for the settings control's button.
    pub fn label(self) -> &'static str {
        match self {
            LineNumbers::Off => "Off",
            LineNumbers::On => "On",
            LineNumbers::Relative => "Relative",
        }
    }

    /// The value written to / read from `[ui] line_numbers`.
    pub fn config_value(self) -> &'static str {
        match self {
            LineNumbers::Off => "off",
            LineNumbers::On => "on",
            LineNumbers::Relative => "rel",
        }
    }

    /// Parse a config value. `None` for anything that isn't exactly one
    /// of the three known values — the caller decides the fallback
    /// ([`LineNumbers::from_config`] falls back to `Off`).
    pub fn from_value(s: &str) -> Option<LineNumbers> {
        LineNumbers::ALL.into_iter().find(|m| m.config_value() == s)
    }

    /// Resolve the effective setting from the layered config: doc `app`,
    /// key `ui.line_numbers`. A missing key, or any unknown value, is
    /// `Off` — same lenient shape as `FindStyle::from_config`.
    pub fn from_config(config: &Config) -> LineNumbers {
        config
            .get("app", "ui.line_numbers")
            .and_then(|v| v.as_str())
            .and_then(LineNumbers::from_value)
            .unwrap_or_default()
    }

    /// The next value in `ALL`, wrapping — what `ui::line_numbers_cycle`
    /// steps by.
    pub fn next(self) -> LineNumbers {
        let ix = LineNumbers::ALL
            .iter()
            .position(|&m| m == self)
            .expect("every LineNumbers is in ALL");
        LineNumbers::ALL[(ix + 1) % LineNumbers::ALL.len()]
    }
}

/// The app-wide UI settings a module may read without a path to
/// `ShellView` — see the module doc. Set by the shell only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiSettings {
    pub line_numbers: LineNumbers,
}

impl gpui::Global for UiSettings {}

/// The number a gutter shows for visible row `row` (0-based) when the
/// cursor is on visible row `cursor` (0-based), or `None` when the
/// gutter is off. See the module doc for the rule.
pub fn gutter_number(mode: LineNumbers, row: usize, cursor: usize) -> Option<usize> {
    match mode {
        LineNumbers::Off => None,
        LineNumbers::On => Some(row + 1),
        LineNumbers::Relative if row == cursor => Some(row + 1),
        LineNumbers::Relative => Some(row.abs_diff(cursor)),
    }
}

/// How many digit cells a gutter over `len` visible rows needs: the
/// digit count of the largest number it can show (`len` itself, in
/// either mode), never fewer than two so a short list's gutter does not
/// jump in width the moment it grows past nine rows.
pub fn gutter_digits(len: usize) -> usize {
    let mut n = len.max(1);
    let mut digits = 0;
    while n > 0 {
        digits += 1;
        n /= 10;
    }
    digits.max(2)
}

/// Fixed width of a gutter digit cell in pixels, shared by grids alongside their
/// fixed-pixel column widths. This estimate is based on the mono face at the default UI
/// size.
pub const GUTTER_DIGIT_PX: f32 = 8.0;
/// The gap between a gutter's last digit and the cell text after it.
pub const GUTTER_GAP_PX: f32 = 6.0;

/// A gutter's width in px over `len` visible rows — `0` when off — so
/// every grid that paints one sizes it by the same rule.
pub fn gutter_px(mode: LineNumbers, len: usize) -> f32 {
    match mode {
        LineNumbers::Off => 0.0,
        _ => gutter_digits(len) as f32 * GUTTER_DIGIT_PX + GUTTER_GAP_PX,
    }
}

/// Space for numbers in the painted window, independent of the total row count.
/// Relative mode also includes the selected row's absolute number when visible.
pub fn window_gutter_px(mode: LineNumbers, visible: std::ops::Range<usize>, cursor: usize) -> f32 {
    let largest = [visible.start, visible.end.saturating_sub(1), cursor]
        .into_iter()
        .filter(|row| visible.contains(row))
        .filter_map(|row| gutter_number(mode, row, cursor))
        .max()
        .unwrap_or(0);
    gutter_px(mode, largest)
}

/// Write `[ui] line_numbers` into `<user_dir>/app.toml`, preserving every
/// other table, key and comment — `config_write::edit`'s contract, the
/// same door `vimfind`/`fontsize`/`tileadd` persist through.
pub fn persist_to_user_config(user_dir: &Path, mode: LineNumbers) -> Result<(), String> {
    crate::config_write::edit(user_dir, Layer::User, "app", |doc| {
        if !doc.get("ui").is_some_and(Item::is_table_like) {
            doc["ui"] = Item::Table(Table::new());
        }
        let ui_table = doc["ui"]
            .as_table_mut()
            .expect("just ensured [ui] is a table");
        ui_table["line_numbers"] = value(mode.config_value());
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};

    #[test]
    fn config_values_round_trip_and_unknown_is_none() {
        for m in LineNumbers::ALL {
            assert_eq!(LineNumbers::from_value(m.config_value()), Some(m));
        }
        assert_eq!(LineNumbers::from_value("relative"), None);
        assert_eq!(LineNumbers::from_value("On"), None);
        assert_eq!(LineNumbers::from_value(""), None);
    }

    #[test]
    fn from_config_reads_ui_line_numbers_and_falls_back_to_off() {
        let read = |doc: &str| {
            let sources = ConfigSources {
                builtin: vec![LayerDoc::builtin("app", doc).unwrap()],
                ..Default::default()
            };
            LineNumbers::from_config(&Config::load(&sources))
        };
        assert_eq!(
            read("[ui]\nline_numbers = \"rel\"\n"),
            LineNumbers::Relative
        );
        assert_eq!(read("[ui]\nline_numbers = \"on\"\n"), LineNumbers::On);
        assert_eq!(read("[ui]\nline_numbers = \"bogus\"\n"), LineNumbers::Off);
        assert_eq!(read("[ui]\nfind_style = \"vim\"\n"), LineNumbers::Off);
    }

    #[test]
    fn next_cycles_off_on_rel_and_wraps() {
        assert_eq!(LineNumbers::Off.next(), LineNumbers::On);
        assert_eq!(LineNumbers::On.next(), LineNumbers::Relative);
        assert_eq!(LineNumbers::Relative.next(), LineNumbers::Off);
    }

    #[test]
    fn compact_gutter_fits_visible_numbers_and_relative_cursor() {
        assert_eq!(window_gutter_px(LineNumbers::Off, 999..1_020, 1_000), 0.);
        assert_eq!(
            window_gutter_px(LineNumbers::On, 0..20, 0),
            gutter_px(LineNumbers::On, 20)
        );
        assert_eq!(
            window_gutter_px(LineNumbers::On, 990..1_010, 1_000),
            gutter_px(LineNumbers::On, 1_010)
        );
        assert_eq!(
            window_gutter_px(LineNumbers::Relative, 990..1_010, 1_000),
            gutter_px(LineNumbers::On, 1_001)
        );
        assert_eq!(
            window_gutter_px(LineNumbers::Relative, 990..1_000, 1_010),
            gutter_px(LineNumbers::On, 20)
        );
        assert_eq!(
            window_gutter_px(LineNumbers::On, 0..0, 0),
            gutter_px(LineNumbers::On, 0)
        );
    }

    #[test]
    fn off_numbers_nothing() {
        assert_eq!(gutter_number(LineNumbers::Off, 0, 0), None);
        assert_eq!(gutter_number(LineNumbers::Off, 7, 3), None);
    }

    #[test]
    fn on_is_the_one_based_visible_index_regardless_of_cursor() {
        assert_eq!(gutter_number(LineNumbers::On, 0, 5), Some(1));
        assert_eq!(gutter_number(LineNumbers::On, 11, 0), Some(12));
        assert_eq!(gutter_number(LineNumbers::On, 5, 5), Some(6));
    }

    #[test]
    fn relative_is_the_distance_from_the_cursor_in_both_directions() {
        assert_eq!(gutter_number(LineNumbers::Relative, 2, 5), Some(3));
        assert_eq!(gutter_number(LineNumbers::Relative, 8, 5), Some(3));
        assert_eq!(gutter_number(LineNumbers::Relative, 4, 5), Some(1));
        assert_eq!(gutter_number(LineNumbers::Relative, 6, 5), Some(1));
    }

    #[test]
    fn relative_shows_the_absolute_number_on_the_cursor_row() {
        // The cursor row keeps its absolute number, never `0`.
        assert_eq!(gutter_number(LineNumbers::Relative, 5, 5), Some(6));
        assert_eq!(gutter_number(LineNumbers::Relative, 0, 0), Some(1));
    }

    #[test]
    fn gutter_width_is_the_digit_count_of_the_row_total_with_a_floor_of_two() {
        assert_eq!(gutter_digits(0), 2);
        assert_eq!(gutter_digits(1), 2);
        assert_eq!(gutter_digits(9), 2);
        assert_eq!(gutter_digits(10), 2);
        assert_eq!(gutter_digits(99), 2);
        assert_eq!(gutter_digits(100), 3);
        assert_eq!(gutter_digits(1_000_000), 7);
    }

    #[test]
    fn persist_creates_a_fresh_file_with_config_version() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), LineNumbers::Relative).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("config_version = 1"));
        assert!(text.contains("[ui]"));
        assert!(text.contains("line_numbers = \"rel\""));
    }

    #[test]
    fn persist_preserves_comments_and_sibling_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(
            &path,
            "# my config\nconfig_version = 1\n\n[ui]\nfont_size = \"large\" # keep me\n",
        )
        .unwrap();
        persist_to_user_config(dir.path(), LineNumbers::On).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("font_size = \"large\" # keep me"));
        assert!(text.contains("line_numbers = \"on\""));
    }

    #[test]
    fn persist_overwrites_a_previous_value_in_place() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), LineNumbers::On).unwrap();
        persist_to_user_config(dir.path(), LineNumbers::Off).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("line_numbers = \"off\""));
        assert!(!text.contains("line_numbers = \"on\""));
    }

    #[test]
    fn persist_refuses_to_touch_an_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(&path, "not [valid toml").unwrap();
        assert!(persist_to_user_config(dir.path(), LineNumbers::On).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not [valid toml");
    }
}
