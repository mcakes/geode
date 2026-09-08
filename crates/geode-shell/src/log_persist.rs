//! Persist a `:level` change into the user layer's `app.toml` `[log]`
//! table (Phase 4b Task 4, spec §4.3) — `Diagnostics::request_level`'s
//! `take_pending_level` drain calls this on the background executor.
//!
//! Reuses `theme::write_atomic` and the same `toml_edit` format-
//! preserving read-modify-write `frame::persist_slot_to_user_config`
//! already uses (a plain `toml::Table` re-serialize would silently
//! destroy comments and reorder keys — see `theme::persist_to_user_config`'s
//! own doc comment for the fuller version of that argument). This adds
//! no new atomic-write implementation of its own (plan ruling, "config
//! write paths stay separate in 4b" — 4c's `config_write` door migrates
//! every persist, this one included, onto itself later).

use std::path::Path;

use geode_core::log::Level;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::theme::write_atomic;

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

    if !doc.get("log").is_some_and(Item::is_table_like) {
        doc["log"] = Item::Table(Table::new());
    }
    let log_table = doc["log"]
        .as_table_mut()
        .expect("just ensured [log] is a table");
    log_table[target] = value(level.to_string().to_ascii_lowercase());

    write_atomic(user_dir, &path, &doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
