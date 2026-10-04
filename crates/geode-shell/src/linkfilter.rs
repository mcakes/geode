//! `[links] include_tile_filter`: whether an emitter's own `:filter` layer
//! is composed into the scope it posts. On by default, so a follower shows
//! the rows under the emitter's cursor; off keeps tile filters local.

use std::path::Path;

use geode_core::config::{Config, Layer};
use toml_edit::{Item, Table, value};

/// Stepping order for the settings row: the default first.
pub const ALL: [bool; 2] = [true, false];

/// Resolve the setting from the layered config: doc `app`, key
/// `links.include_tile_filter`. A missing key or a non-boolean value is
/// the default, on, so a malformed edit never silently narrows nothing.
pub fn from_config(config: &Config) -> bool {
    config
        .get("app", "links.include_tile_filter")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Label for the settings row's value.
pub fn label(on: bool) -> &'static str {
    if on { "On" } else { "Off" }
}

/// Write `[links] include_tile_filter` into `<user_dir>/app.toml`,
/// preserving every other table, key and comment (`config_write::edit`'s
/// contract).
pub fn persist_to_user_config(user_dir: &Path, on: bool) -> Result<(), String> {
    crate::config_write::edit(user_dir, Layer::User, "app", |doc| {
        if !doc.get("links").is_some_and(Item::is_table_like) {
            doc["links"] = Item::Table(Table::new());
        }
        doc["links"]
            .as_table_mut()
            .expect("just ensured [links] is a table")["include_tile_filter"] = value(on);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::{Config, ConfigSources, LayerDoc};

    fn config(app: &str) -> Config {
        let sources = ConfigSources {
            builtin: vec![LayerDoc::builtin("app", app).unwrap()],
            ..Default::default()
        };
        Config::load(&sources)
    }

    #[test]
    fn the_setting_defaults_on_and_reads_off() {
        assert!(from_config(&config("")));
        assert!(from_config(&config(
            "[links]\ninclude_tile_filter = true\n"
        )));
        assert!(!from_config(&config(
            "[links]\ninclude_tile_filter = false\n"
        )));
        assert!(
            from_config(&config("[links]\ninclude_tile_filter = \"no\"\n")),
            "a non-bool reads as the default"
        );
    }

    #[test]
    fn the_values_step_on_then_off_with_their_labels() {
        assert_eq!(ALL, [true, false]);
        assert_eq!(label(true), "On");
        assert_eq!(label(false), "Off");
    }

    #[test]
    fn persist_writes_the_key_and_keeps_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(
            &path,
            "# mine\nconfig_version = 1\n\n[ui]\nfont_size = \"large\" # keep me\n",
        )
        .unwrap();
        persist_to_user_config(dir.path(), false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# mine"));
        assert!(text.contains("font_size = \"large\" # keep me"));
        assert!(text.contains("[links]"));
        assert!(text.contains("include_tile_filter = false"));
        persist_to_user_config(dir.path(), true).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("include_tile_filter = true"));
        assert!(!text.contains("include_tile_filter = false"));
    }

    #[test]
    fn persist_refuses_to_touch_an_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(&path, "not [valid toml").unwrap();
        assert!(persist_to_user_config(dir.path(), false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not [valid toml");
    }
}
