//! The small, medium, and large UI scale setting.
//!
//! `ShellView::render` applies [`FontSize`] with `window.set_rem_size` only
//! when the value differs. Rem-based text and geometry follow that scale;
//! `Theme.font_size`, which feeds separate pixel-based typography tokens,
//! is left unchanged.
//!
//! Persisted as `[ui] font_size` through [`crate::config_write::edit`] and
//! read through [`FontSize::from_config`]. Keeping it outside `[theme]`
//! lets a theme change preserve the chosen scale.

use std::path::Path;

use toml_edit::{Item, Table, value};

use geode_core::config::{Config, Layer};

/// UI scales with 10, 12, or 14 pixel rem sizes. `Medium` (12px) is the
/// default when configuration omits `[ui] font_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl FontSize {
    /// Display order for the settings control.
    pub const ALL: [FontSize; 3] = [FontSize::Small, FontSize::Medium, FontSize::Large];

    /// The window rem size this scale means, in pixels.
    pub fn rem_px(self) -> f32 {
        match self {
            FontSize::Small => 10.0,
            FontSize::Medium => 12.0,
            FontSize::Large => 14.0,
        }
    }

    /// Label for the settings control's button.
    pub fn label(self) -> &'static str {
        match self {
            FontSize::Small => "Small",
            FontSize::Medium => "Medium",
            FontSize::Large => "Large",
        }
    }

    /// The value written to / read from `[ui] font_size`.
    pub fn config_value(self) -> &'static str {
        match self {
            FontSize::Small => "small",
            FontSize::Medium => "medium",
            FontSize::Large => "large",
        }
    }

    /// One step larger, clamped at `Large` (`fontsize::increase`,
    /// `ctrl+=` — the browser-zoom idiom; repeated presses at the top are
    /// a no-op, not a wrap).
    pub fn larger(self) -> FontSize {
        match self {
            FontSize::Small => FontSize::Medium,
            FontSize::Medium | FontSize::Large => FontSize::Large,
        }
    }

    /// One step smaller, clamped at `Small` (`fontsize::decrease`,
    /// `ctrl+-`).
    pub fn smaller(self) -> FontSize {
        match self {
            FontSize::Large => FontSize::Medium,
            FontSize::Medium | FontSize::Small => FontSize::Small,
        }
    }

    /// Parse a config value. `None` for anything that isn't exactly one of
    /// the three known values — the caller decides the fallback
    /// ([`FontSize::from_config`] falls back to `Medium`).
    pub fn from_value(s: &str) -> Option<FontSize> {
        FontSize::ALL.into_iter().find(|f| f.config_value() == s)
    }

    /// Resolve the effective font size from the layered config: doc `app`,
    /// key `ui.font_size`. A missing key, or any unknown value, is
    /// `Medium` — same lenient shape as `defaults::mod_alias_from_config`.
    pub fn from_config(config: &Config) -> FontSize {
        config
            .get("app", "ui.font_size")
            .and_then(|v| v.as_str())
            .and_then(FontSize::from_value)
            .unwrap_or_default()
    }
}

/// Write `[ui] font_size` into `<user_dir>/app.toml`, preserving every
/// other table, key, and comment — the same toml_edit + atomic-write
/// contract as [`crate::theme::persist_to_user_config`] (see its doc
/// comment for the full failure-mode reasoning: missing file created with
/// `config_version = 1`, unparseable file left untouched and reported as
/// `Err`, reload-watcher interplay identical).
pub fn persist_to_user_config(user_dir: &Path, size: FontSize) -> Result<(), String> {
    crate::config_write::edit(user_dir, Layer::User, "app", |doc| {
        if !doc.get("ui").is_some_and(Item::is_table_like) {
            doc["ui"] = Item::Table(Table::new());
        }
        let ui_table = doc["ui"]
            .as_table_mut()
            .expect("just ensured [ui] is a table");
        ui_table["font_size"] = value(size.config_value());
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{ConfigSources, LayerDoc};

    #[test]
    fn rem_px_mapping_and_medium_default() {
        assert_eq!(FontSize::Small.rem_px(), 10.0);
        assert_eq!(FontSize::Medium.rem_px(), 12.0);
        assert_eq!(FontSize::Large.rem_px(), 14.0);
        assert_eq!(FontSize::default(), FontSize::Medium);
    }

    #[test]
    fn larger_and_smaller_step_through_the_scale_and_clamp_at_the_ends() {
        assert_eq!(FontSize::Small.larger(), FontSize::Medium);
        assert_eq!(FontSize::Medium.larger(), FontSize::Large);
        assert_eq!(FontSize::Large.larger(), FontSize::Large, "clamp, no wrap");
        assert_eq!(FontSize::Large.smaller(), FontSize::Medium);
        assert_eq!(FontSize::Medium.smaller(), FontSize::Small);
        assert_eq!(FontSize::Small.smaller(), FontSize::Small, "clamp");
    }

    #[test]
    fn from_value_roundtrips_and_rejects_unknowns() {
        for size in FontSize::ALL {
            assert_eq!(FontSize::from_value(size.config_value()), Some(size));
        }
        assert_eq!(FontSize::from_value("huge"), None);
        assert_eq!(FontSize::from_value(""), None);
    }

    #[test]
    fn from_config_reads_ui_font_size_with_medium_fallback() {
        let with = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[ui]\nfont_size = \"large\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(FontSize::from_config(&with), FontSize::Large);

        let empty = Config::load(&ConfigSources::default());
        assert_eq!(FontSize::from_config(&empty), FontSize::Medium);

        let bogus = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[ui]\nfont_size = \"huge\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(FontSize::from_config(&bogus), FontSize::Medium);
    }

    #[test]
    fn persist_creates_a_fresh_file_with_config_version() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), FontSize::Large).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("config_version = 1"));
        assert!(text.contains("[ui]"));
        assert!(text.contains("font_size = \"large\""));
    }

    #[test]
    fn persist_preserves_comments_and_unrelated_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(
            &path,
            "# my config\nconfig_version = 1\n\n[theme]\nname = \"Gruvbox Dark\" # keep me\n",
        )
        .unwrap();
        persist_to_user_config(dir.path(), FontSize::Small).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("name = \"Gruvbox Dark\" # keep me"));
        assert!(text.contains("font_size = \"small\""));
    }

    #[test]
    fn persist_overwrites_a_previous_value_in_place() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), FontSize::Small).unwrap();
        persist_to_user_config(dir.path(), FontSize::Large).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("font_size = \"large\""));
        assert!(!text.contains("font_size = \"small\""));
    }

    #[test]
    fn persist_refuses_to_touch_an_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(&path, "not [valid toml").unwrap();
        assert!(persist_to_user_config(dir.path(), FontSize::Small).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not [valid toml");
    }
}
