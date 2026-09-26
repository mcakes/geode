//! Persist log levels to the user layer's `app.toml` `[log]` table.
//!
//! The palette's `Set log level…` action queues a request through
//! `Diagnostics::request_level`. The shell drains it with
//! `take_pending_level` and persists it on the background executor.
//!
//! [`crate::config_write::edit`] owns the user-layer guard, preservation of
//! unrelated keys and comments, atomic replacement, and refusal to overwrite
//! an unparseable file.

use geode_core::config::Layer;
use geode_core::log::Level;
use std::path::Path;
use toml_edit::{Item, Table, value};

/// `target`'s bare suffix (`"ingest"`, not `"geode::ingest"` — the same
/// key shape `[log]` already reads via `LogLevels::from_doc`) mapped to
/// `level`, written into `<user_dir>/app.toml`'s `[log]` table.
///
/// - **Missing file**: created fresh, `config_version = 1` then `[log]`.
/// - **Existing, valid file**: `[log]` created if absent; `target` set
///   (or overwritten) inside it. Every other table, key and comment is
///   byte-preserved.
/// - **Existing, unparseable file**: `Err`, file untouched — a user's
///   hand-edited config, however broken, is never destroyed by this.
pub fn persist_log_level_to_user_config(
    user_dir: &Path,
    target: &str,
    level: Level,
) -> Result<(), String> {
    crate::config_write::edit(user_dir, Layer::User, "app", |doc| {
        if !doc.get("log").is_some_and(Item::is_table_like) {
            doc["log"] = Item::Table(Table::new());
        }
        let log_table = doc["log"]
            .as_table_mut()
            .expect("just ensured [log] is a table");
        log_table[target] = value(level.to_string().to_ascii_lowercase());
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::DocumentMut;

    #[test]
    fn creates_a_fresh_file_with_config_version_and_the_target() {
        let dir = tempfile::tempdir().unwrap();
        persist_log_level_to_user_config(dir.path(), "ingest", Level::DEBUG).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(doc["config_version"].as_integer(), Some(1));
        assert_eq!(doc["log"]["ingest"].as_str(), Some("debug"));
    }

    #[test]
    fn preserves_comments_and_sibling_keys_and_overwrites_the_same_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(
            &path,
            "config_version = 1\n# a hand-written comment\n[theme]\nname = \"dracula\"\n[log]\ndefault = \"info\"\ningest = \"info\"\n",
        )
        .unwrap();

        persist_log_level_to_user_config(dir.path(), "ingest", Level::TRACE).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# a hand-written comment"));
        let doc: DocumentMut = text.parse().unwrap();
        assert_eq!(doc["theme"]["name"].as_str(), Some("dracula"));
        assert_eq!(doc["log"]["default"].as_str(), Some("info"));
        assert_eq!(doc["log"]["ingest"].as_str(), Some("trace"));
    }

    #[test]
    fn refuses_to_touch_an_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(&path, "not valid toml [[[").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let err = persist_log_level_to_user_config(dir.path(), "ingest", Level::WARN).unwrap_err();
        assert!(err.contains("failed to parse"));
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "the file is left untouched");
    }
}
