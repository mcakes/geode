//! The "add tile" direction setting (`[tiles] add`, spec
//! `2026-09-08-geode-add-tile-design.md` §5) and the pure resolution
//! rule every add goes through (§4.1). Mirrors `vimfind::FindStyle`'s
//! shape exactly — `ALL`/`label`/`config_value`/`from_value`/
//! `from_config` plus a `persist_to_user_config` sibling — so the
//! settings row, startup resolution and hot reload ride the paths font
//! size and find style already do. `persist_to_user_config` is the
//! seventh copy of that read-modify-write and a Phase 4c `config_write`
//! migration target; it is not to be consolidated by hand (CLAUDE.md).

use std::path::Path;

use geode_core::config::Config;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::tiling::{Orientation, Rect};

/// Where an add lands when the palette row or chord did not say (spec
/// §2, §5): to the right, below, or along the focused tile's longer
/// side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddDirection {
    Horizontal,
    Vertical,
    #[default]
    Auto,
}

impl AddDirection {
    /// Display and stepping order for the settings row.
    pub const ALL: [AddDirection; 3] = [
        AddDirection::Horizontal,
        AddDirection::Vertical,
        AddDirection::Auto,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AddDirection::Horizontal => "Horizontal",
            AddDirection::Vertical => "Vertical",
            AddDirection::Auto => "Auto",
        }
    }

    /// The value written to / read from `[tiles] add`.
    pub fn config_value(self) -> &'static str {
        match self {
            AddDirection::Horizontal => "horizontal",
            AddDirection::Vertical => "vertical",
            AddDirection::Auto => "auto",
        }
    }

    pub fn from_value(s: &str) -> Option<AddDirection> {
        AddDirection::ALL
            .into_iter()
            .find(|d| d.config_value() == s)
    }

    /// Doc `app`, key `tiles.add`. Missing or unknown → `Auto`, no
    /// diagnostic — `FindStyle::from_config`'s lenient shape.
    pub fn from_config(config: &Config) -> AddDirection {
        config
            .get("app", "tiles.add")
            .and_then(|v| v.as_str())
            .and_then(AddDirection::from_value)
            .unwrap_or_default()
    }

    /// The one placement rule (spec §4.1): an explicit direction wins;
    /// else `Horizontal`/`Vertical` as set; else `Auto` reads the focused
    /// tile's rect and splits along its longer side (`w >= h` → side by
    /// side). No rect — nothing focused, nothing painted yet — means
    /// side by side.
    pub fn resolve(self, explicit: Option<Orientation>, rect: Option<Rect>) -> Orientation {
        if let Some(o) = explicit {
            return o;
        }
        match self {
            AddDirection::Horizontal => Orientation::Horizontal,
            AddDirection::Vertical => Orientation::Vertical,
            AddDirection::Auto => match rect {
                Some(r) if r.h > r.w => Orientation::Vertical,
                _ => Orientation::Horizontal,
            },
        }
    }
}

/// Write `[tiles] add` into `<user_dir>/app.toml`, preserving every
/// other table, key and comment — the same toml_edit + atomic-write
/// contract as `vimfind::persist_to_user_config` (see
/// `theme::persist_to_user_config`'s doc comment for the failure-mode
/// reasoning: a missing file is created with `config_version = 1`, an
/// unparseable file is left untouched and reported as `Err`).
pub fn persist_to_user_config(user_dir: &Path, direction: AddDirection) -> Result<(), String> {
    let path = user_dir.join("app.toml");
    let existed = path.exists();

    let mut doc = if existed {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        text.parse::<DocumentMut>().map_err(|e| {
            format!(
                "failed to parse {}: {e} (file left untouched)",
                path.display()
            )
        })?
    } else {
        DocumentMut::new()
    };

    if !existed {
        doc["config_version"] = value(1_i64);
    }

    if !doc.get("tiles").is_some_and(Item::is_table_like) {
        doc["tiles"] = Item::Table(Table::new());
    }
    let tiles = doc["tiles"]
        .as_table_mut()
        .expect("just ensured [tiles] is a table");
    tiles["add"] = value(direction.config_value());

    crate::theme::write_atomic(user_dir, &path, &doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};

    fn rect(w: f32, h: f32) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }

    #[test]
    fn config_values_round_trip_and_unknown_is_none() {
        for d in AddDirection::ALL {
            assert_eq!(AddDirection::from_value(d.config_value()), Some(d));
        }
        assert_eq!(AddDirection::from_value("sideways"), None);
        assert_eq!(AddDirection::default(), AddDirection::Auto);
    }

    #[test]
    fn from_config_reads_tiles_add_and_falls_back_to_auto() {
        let empty = Config::load(&ConfigSources::default());
        assert_eq!(AddDirection::from_config(&empty), AddDirection::Auto);
        let set = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[tiles]\nadd = \"vertical\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(AddDirection::from_config(&set), AddDirection::Vertical);
        let bogus = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[tiles]\nadd = \"diagonal\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(AddDirection::from_config(&bogus), AddDirection::Auto);
    }

    #[test]
    fn an_explicit_direction_beats_the_setting() {
        assert_eq!(
            AddDirection::Vertical.resolve(Some(Orientation::Horizontal), Some(rect(1.0, 10.0))),
            Orientation::Horizontal
        );
        assert_eq!(
            AddDirection::Auto.resolve(Some(Orientation::Vertical), Some(rect(10.0, 1.0))),
            Orientation::Vertical
        );
    }

    #[test]
    fn fixed_settings_ignore_the_rect() {
        assert_eq!(
            AddDirection::Horizontal.resolve(None, Some(rect(1.0, 10.0))),
            Orientation::Horizontal
        );
        assert_eq!(
            AddDirection::Vertical.resolve(None, Some(rect(10.0, 1.0))),
            Orientation::Vertical
        );
    }

    #[test]
    fn auto_splits_along_the_longer_side_and_a_square_or_missing_rect_goes_right() {
        assert_eq!(
            AddDirection::Auto.resolve(None, Some(rect(10.0, 4.0))),
            Orientation::Horizontal,
            "wider than tall: side by side"
        );
        assert_eq!(
            AddDirection::Auto.resolve(None, Some(rect(4.0, 10.0))),
            Orientation::Vertical,
            "taller than wide: stacked"
        );
        assert_eq!(
            AddDirection::Auto.resolve(None, Some(rect(5.0, 5.0))),
            Orientation::Horizontal,
            "square: w >= h → side by side"
        );
        assert_eq!(
            AddDirection::Auto.resolve(None, None),
            Orientation::Horizontal
        );
    }

    #[test]
    fn persist_creates_a_fresh_file_with_config_version() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), AddDirection::Vertical).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("config_version = 1"));
        assert!(text.contains("[tiles]"));
        assert!(text.contains("add = \"vertical\""));
    }

    #[test]
    fn persist_preserves_comments_and_sibling_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(
            &path,
            "# my config\nconfig_version = 1\n\n[ui]\nfont_size = \"large\" # keep me\n",
        )
        .unwrap();
        persist_to_user_config(dir.path(), AddDirection::Horizontal).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("font_size = \"large\" # keep me"));
        assert!(text.contains("add = \"horizontal\""));
    }

    #[test]
    fn persist_overwrites_a_previous_value_in_place() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), AddDirection::Vertical).unwrap();
        persist_to_user_config(dir.path(), AddDirection::Auto).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("add = \"auto\""));
        assert!(!text.contains("add = \"vertical\""));
    }

    #[test]
    fn persist_refuses_to_touch_an_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(&path, "not [valid toml").unwrap();
        assert!(persist_to_user_config(dir.path(), AddDirection::Auto).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not [valid toml");
    }
}
