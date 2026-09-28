//! Persist keymap edits in `<user_dir>/keymap.toml` through
//! [`crate::config_write`]. Builtin and desk documents are read-only. Writes become
//! active through the ordinary configuration load/reload path; a successful write
//! does not establish that the resulting binding wins at dispatch.
//!
//! Rebind/unbind operations use the first entry matching the raw context string
//! and the supplied key spelling, creating the entry when needed. Rebinding writes
//! the new key, then removes a displaced user key or shadows a lower-layer key
//! with `"none"`. A same-key rebind skips displacement. A missing old user key is
//! reported in the successful outcome, so callers can warn about stale state.
//! Removing a user key exposes any lower-layer binding on that key.
//!
//! Reset removes the named overrides from every matching context entry. Reset all
//! removes the entire user `bindings` array, including hand-written entries.
//! Other document fields remain. Later duplicate context entries can still shadow
//! a rebind written into the first entry; context matching does not normalize
//! predicate spellings.
//!
//! The editor preserves unrelated text and existing key decorations. Malformed
//! TOML and unsupported top-level `bindings` shapes return errors before writing.
//! The reader accepts an inline array of bindings, but these editors require
//! `[[bindings]]`; inline `keys` tables inside entries are supported. A missing
//! or non-table `keys` value is replaced by an empty table before the edit.
//! No action, predicate, or keystroke validation occurs here.
//!
//! Writes use the shared serialized read/edit/temporary-file/rename transaction.
//! The temporary file has a `.tmp` extension so the reload scanner cannot read it
//! as a configuration document. See [`crate::config_write`] for write-failure
//! and durability limits.

use std::path::Path;

use crate::keymap::UserOverride;
use geode_core::config::Layer;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike, value};

/// The user-layer document name, resolved to `<user_dir>/keymap.toml`.
const KEYMAP_DOC: &str = "keymap";

/// One rebind to persist; the caller supplies validated binding/action spelling.
#[derive(Debug, Clone)]
pub struct Rebind {
    /// The `[[bindings]]` entry's `context` to write into (exact string
    /// match against that entry's own `context` value). `None` means the
    /// no-`context` entry (matches an entry with no `context` key at all).
    pub context: Option<String>,
    /// The rendered keystroke (or sequence) to bind, e.g. `"ctrl+shift+x"`
    /// or `"g g"` — written verbatim as the new `[bindings.keys]` key.
    pub new_key: String,
    /// The action id to bind `new_key` to.
    pub action: String,
    /// The current effective binding being displaced, if any (`None` when
    /// the action previously had no binding at all).
    pub old_key: Option<String>,
    /// Whether `old_key`'s current effective binding was itself set by a
    /// user-layer entry (assumed to be *this same* `context` entry — see
    /// module doc) — `true` removes it outright; `false` shadows a
    /// lower-layer (builtin/desk) binding by writing `"none"` over it.
    pub old_key_is_user_layer: bool,
}

/// What happened to the old key while writing a replacement binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Displacement {
    /// No old key was supplied, or it equals the new key and must remain bound.
    NotRequested,
    /// `old_key` was found and displaced: removed outright
    /// (`old_key_is_user_layer: true`) or shadowed with `"none"`
    /// (`old_key_is_user_layer: false`).
    Displaced,
    /// The requested old user key was absent from the first matching entry.
    /// The new entry was written, but another entry may still bind the old key.
    OldKeyNotFound,
}

/// The outcome of a successful [`apply_rebind`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebindOutcome {
    pub displacement: Displacement,
}

/// Write a replacement binding and displace the old key when distinct.
/// Read, parse, unsupported document shape, and write failures return `Err`.
/// A missing old user key is reported through [`RebindOutcome`] after the new
/// key is written. Effective resolution is determined on the next load.
pub fn apply_rebind(user_dir: &Path, rebind: &Rebind) -> Result<RebindOutcome, String> {
    apply_rebind_clearing(user_dir, &[], rebind)
}

/// [`apply_rebind`] after removing `clear` as [`apply_reset`] does, in one
/// transaction. A shared motion's edit clears the action's old per-module
/// overrides before writing into the shared context, so the edit reaches
/// every tile rather than sitting beside a module override that still wins
/// there.
pub fn apply_rebind_clearing(
    user_dir: &Path,
    clear: &[UserOverride],
    rebind: &Rebind,
) -> Result<RebindOutcome, String> {
    crate::config_write::try_edit(user_dir, Layer::User, KEYMAP_DOC, |doc| {
        ensure_bindings(user_dir, doc)?;
        let bindings = doc["bindings"]
            .as_array_of_tables_mut()
            .expect("ensure_bindings just ensured this");
        remove_overrides(bindings, clear);

        let keys = keys_table_for(bindings, rebind.context.as_deref());

        set_key(keys, rebind.new_key.as_str(), value(rebind.action.as_str()));

        // Displacing the same spelling would remove or overwrite the new binding.
        let displacement = match &rebind.old_key {
            Some(old_key) if old_key != &rebind.new_key => {
                if rebind.old_key_is_user_layer {
                    if keys.contains_key(old_key) {
                        keys.remove(old_key);
                        Displacement::Displaced
                    } else {
                        Displacement::OldKeyNotFound
                    }
                } else {
                    set_key(keys, old_key.as_str(), value("none"));
                    Displacement::Displaced
                }
            }
            _ => Displacement::NotRequested,
        };

        Ok(RebindOutcome { displacement })
    })
}

/// One binding to silence, the displacement half of a [`Rebind`] performed
/// on its own (`keybindings_view`'s `d`).
#[derive(Debug, Clone)]
pub struct Unbind {
    /// The `[[bindings]]` entry's `context`, matched exactly as
    /// [`Rebind::context`] is. `None` means the no-`context` entry.
    pub context: Option<String>,
    /// The rendered keystroke to silence, e.g. `"ctrl+k"`.
    pub key: String,
    /// Whether to remove a user entry (`true`) or shadow a lower-layer
    /// binding with `"none"` (`false`). Removing exposes lower layers; the
    /// caller must retain the binding's original layer and key spelling.
    pub is_user_layer: bool,
}

/// Whether a user key was removed. `false` can mean either a successful
/// shadow write or a removal miss; interpret it with [`Unbind::is_user_layer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnbindOutcome {
    pub removed: bool,
}

/// Remove a user key or write a `"none"` shadow in the first matching context
/// entry. Removal exposes lower layers. A removal miss still succeeds with
/// `removed = false`; read, parse, shape, or write failures return `Err`.
pub fn apply_unbind(user_dir: &Path, unbind: &Unbind) -> Result<UnbindOutcome, String> {
    apply_unbind_clearing(user_dir, &[], unbind)
}

/// [`apply_unbind`] after removing `clear` as [`apply_reset`] does, in one
/// transaction: a shared motion's unbind, for the reason given at
/// [`apply_rebind_clearing`].
pub fn apply_unbind_clearing(
    user_dir: &Path,
    clear: &[UserOverride],
    unbind: &Unbind,
) -> Result<UnbindOutcome, String> {
    crate::config_write::try_edit(user_dir, Layer::User, KEYMAP_DOC, |doc| {
        ensure_bindings(user_dir, doc)?;
        let bindings = doc["bindings"]
            .as_array_of_tables_mut()
            .expect("ensure_bindings just ensured this");
        remove_overrides(bindings, clear);

        let keys = keys_table_for(bindings, unbind.context.as_deref());

        let removed = if unbind.is_user_layer {
            if keys.contains_key(&unbind.key) {
                keys.remove(&unbind.key);
                true
            } else {
                false
            }
        } else {
            set_key(keys, unbind.key.as_str(), value("none"));
            false
        };

        Ok(UnbindOutcome { removed })
    })
}

/// Number of keys actually removed. Callers can compare this with the
/// requested override set to detect stale state; duplicate context entries can
/// produce more than one removal for a single requested key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetOutcome {
    pub removed: usize,
}

/// Remove the specified overrides in one transaction, searching every entry
/// with each raw context string. Resetting an action normally removes both its
/// replacement bindings and its `"none"` shadows so lower layers become visible.
/// Missing entries are skipped and empty entries remain to preserve their comments.
/// Read, parse, unsupported `bindings` shape, or write failures return `Err`.
pub fn apply_reset(user_dir: &Path, overrides: &[UserOverride]) -> Result<ResetOutcome, String> {
    crate::config_write::try_edit(user_dir, Layer::User, KEYMAP_DOC, |doc| {
        ensure_bindings(user_dir, doc)?;
        let bindings = doc["bindings"]
            .as_array_of_tables_mut()
            .expect("ensure_bindings just ensured this");

        let removed = remove_overrides(bindings, overrides);
        Ok(ResetOutcome { removed })
    })
}

/// Remove each override from every entry with its raw context string; return
/// the number of keys removed. Missing keys are skipped; emptied entries stay.
fn remove_overrides(bindings: &mut ArrayOfTables, overrides: &[UserOverride]) -> usize {
    let mut removed = 0;
    for o in overrides {
        for entry in bindings.iter_mut().filter(|entry| {
            entry.get("context").and_then(Item::as_str) == o.context_source.as_deref()
        }) {
            let Some(keys) = entry.get_mut("keys").and_then(Item::as_table_like_mut) else {
                continue;
            };
            if keys.remove(&o.key).is_some() {
                removed += 1;
            }
        }
    }
    removed
}

/// Remove the entire user `bindings` array, including hand-written entries.
/// Other fields and their comments remain; builtin and desk layers are untouched.
/// The outcome counts keys in table-like `keys` values. Read, parse, unsupported
/// `bindings` shape, or write failures return `Err`.
pub fn apply_reset_all(user_dir: &Path) -> Result<ResetOutcome, String> {
    crate::config_write::try_edit(user_dir, Layer::User, KEYMAP_DOC, |doc| {
        ensure_bindings(user_dir, doc)?;
        let removed = doc["bindings"]
            .as_array_of_tables()
            .expect("ensure_bindings just ensured this")
            .iter()
            .map(|entry| {
                entry
                    .get("keys")
                    .and_then(Item::as_table_like)
                    .map_or(0, TableLike::len)
            })
            .sum();
        doc.remove("bindings");

        Ok(ResetOutcome { removed })
    })
}

/// Require an array of tables or create an empty one when `bindings` is absent.
/// A plain array of inline tables is readable by the keymap compiler but is not
/// editable here. Reject it before writing instead of discarding existing bindings.
/// This guard applies to rebind, unbind, reset, and reset-all transactions.
fn ensure_bindings(user_dir: &Path, doc: &mut DocumentMut) -> Result<(), String> {
    let path = crate::config_write::doc_path(user_dir, Layer::User, KEYMAP_DOC)?;

    match doc.get("bindings") {
        None => doc["bindings"] = Item::ArrayOfTables(ArrayOfTables::new()),
        Some(item) if item.as_array_of_tables().is_none() => {
            return Err(format!(
                "{}: 'bindings' exists but is not an array of tables (file left untouched)",
                path.display()
            ));
        }
        Some(_) => {}
    }

    Ok(())
}

/// Return the first context-matching entry's keys table, creating either when
/// absent. Matching uses `as_str`, so a non-string context also matches `None`.
/// Both ordinary and inline keys tables are editable through [`TableLike`].
/// An existing non-table keys value is replaced with an empty table.
fn keys_table_for<'a>(
    bindings: &'a mut ArrayOfTables,
    context: Option<&str>,
) -> &'a mut dyn TableLike {
    let match_ix = bindings
        .iter()
        .position(|entry| entry.get("context").and_then(Item::as_str) == context);

    let entry = match match_ix {
        Some(ix) => bindings.get_mut(ix).expect("index came from position()"),
        None => {
            let mut new_entry = Table::new();
            if let Some(ctx) = context {
                new_entry["context"] = value(ctx);
            }
            new_entry["keys"] = Item::Table(Table::new());
            bindings.push(new_entry);
            bindings.iter_mut().last().expect("just pushed an entry")
        }
    };

    if !entry.get("keys").is_some_and(Item::is_table_like) {
        entry["keys"] = Item::Table(Table::new());
    }
    entry["keys"]
        .as_table_like_mut()
        .expect("just ensured 'keys' is table-like")
}

/// Replace only the value of an existing key, preserving its key spelling and
/// leading decoration. Inserting over an occupied entry would reformat the key;
/// insertion is reserved for new keys. Replaced value decorations are not retained.
fn set_key(keys: &mut dyn TableLike, key: &str, item: Item) {
    if let Some(existing) = keys.get_mut(key) {
        *existing = item;
    } else {
        keys.insert(key, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::DocumentMut as Doc;

    use crate::actions::{ActionDef, ActionId, ActionRegistry};
    use crate::keymap::{Modifiers as KeymapModifiers, UserOverride, build_keymap};
    use geode_core::config::{Layer, LayerDoc};

    fn read(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("keymap.toml")).unwrap()
    }

    fn rebind(context: Option<&str>, new_key: &str, action: &str) -> Rebind {
        Rebind {
            context: context.map(str::to_string),
            new_key: new_key.to_string(),
            action: action.to_string(),
            old_key: None,
            old_key_is_user_layer: false,
        }
    }

    #[test]
    fn creates_a_fresh_file_with_config_version_and_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = apply_rebind(
            dir.path(),
            &rebind(Some("workspace"), "ctrl+shift+x", "workspace::focus_left"),
        )
        .unwrap();
        assert_eq!(
            outcome.displacement,
            Displacement::NotRequested,
            "no old_key was given"
        );

        let text = read(dir.path());
        let doc: Doc = text.parse().unwrap();
        assert_eq!(doc["config_version"].as_integer(), Some(1));
        let bindings = doc["bindings"].as_array_of_tables().unwrap();
        assert_eq!(bindings.len(), 1);
        let entry = bindings.get(0).unwrap();
        assert_eq!(entry["context"].as_str(), Some("workspace"));
        assert_eq!(
            entry["keys"]["ctrl+shift+x"].as_str(),
            Some("workspace::focus_left")
        );
    }

    #[test]
    fn fresh_file_with_no_context_creates_the_no_context_entry() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = apply_rebind(dir.path(), &rebind(None, "ctrl+k", "palette::toggle")).unwrap();
        assert_eq!(outcome.displacement, Displacement::NotRequested);

        let text = read(dir.path());
        let doc: Doc = text.parse().unwrap();
        let bindings = doc["bindings"].as_array_of_tables().unwrap();
        assert_eq!(bindings.len(), 1);
        let entry = bindings.get(0).unwrap();
        assert!(
            entry.get("context").is_none(),
            "no-context rebind must not write a context key"
        );
        assert_eq!(entry["keys"]["ctrl+k"].as_str(), Some("palette::toggle"));
    }

    #[test]
    fn round_trips_an_existing_file_byte_preserving_comments_and_unrelated_entries() {
        let dir = tempfile::tempdir().unwrap();
        let original = "\
# hand-written keymap
config_version = 1

[[bindings]]
context = \"workspace\"
[bindings.keys]
\"mod+h\" = \"workspace::focus_left\" # inline comment

[[bindings]]
# palette entry, no context
[bindings.keys]
\"ctrl+k\" = \"palette::toggle\"
";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let outcome = apply_rebind(
            dir.path(),
            &rebind(Some("workspace"), "mod+shift+h", "workspace::focus_left"),
        )
        .unwrap();
        assert_eq!(outcome.displacement, Displacement::NotRequested);

        let text = read(dir.path());
        assert!(text.contains("# hand-written keymap"));
        assert!(text.contains("# palette entry, no context"));
        assert!(text.contains("\"mod+h\" = \"workspace::focus_left\" # inline comment"));

        let doc: Doc = text.parse().unwrap();
        let bindings = doc["bindings"].as_array_of_tables().unwrap();
        assert_eq!(bindings.len(), 2, "no new entry should have been created");
        let ws_entry = bindings.get(0).unwrap();
        assert_eq!(
            ws_entry["keys"]["mod+shift+h"].as_str(),
            Some("workspace::focus_left")
        );
        let palette_entry = bindings.get(1).unwrap();
        assert_eq!(
            palette_entry["keys"]["ctrl+k"].as_str(),
            Some("palette::toggle"),
            "unrelated entry must be untouched"
        );
    }

    #[test]
    fn matching_context_entry_is_found_among_several() {
        let dir = tempfile::tempdir().unwrap();
        let original = "\
config_version = 1

[[bindings]]
context = \"palette\"
[bindings.keys]
\"ctrl+p\" = \"palette::up\"

[[bindings]]
context = \"workspace\"
[bindings.keys]
\"mod+h\" = \"workspace::focus_left\"

[[bindings]]
context = \"dialog\"
[bindings.keys]
\"escape\" = \"dialog::close\"
";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let outcome = apply_rebind(
            dir.path(),
            &rebind(Some("workspace"), "mod+l", "workspace::focus_right"),
        )
        .unwrap();
        assert_eq!(outcome.displacement, Displacement::NotRequested);

        let text = read(dir.path());
        let doc: Doc = text.parse().unwrap();
        let bindings = doc["bindings"].as_array_of_tables().unwrap();
        assert_eq!(bindings.len(), 3, "must edit in place, not append");
        let ws_entry = bindings
            .iter()
            .find(|e| e.get("context").and_then(Item::as_str) == Some("workspace"))
            .unwrap();
        assert_eq!(
            ws_entry["keys"]["mod+l"].as_str(),
            Some("workspace::focus_right")
        );
        assert_eq!(
            ws_entry["keys"]["mod+h"].as_str(),
            Some("workspace::focus_left"),
            "the pre-existing key in the matched entry must survive"
        );
    }

    #[test]
    fn context_missing_creates_a_new_entry_without_touching_others() {
        let dir = tempfile::tempdir().unwrap();
        let original = "\
config_version = 1

[[bindings]]
context = \"workspace\"
[bindings.keys]
\"mod+h\" = \"workspace::focus_left\"
";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let outcome = apply_rebind(
            dir.path(),
            &rebind(Some("dialog"), "escape", "dialog::close"),
        )
        .unwrap();
        assert_eq!(outcome.displacement, Displacement::NotRequested);

        let text = read(dir.path());
        let doc: Doc = text.parse().unwrap();
        let bindings = doc["bindings"].as_array_of_tables().unwrap();
        assert_eq!(bindings.len(), 2);
        let dialog_entry = bindings
            .iter()
            .find(|e| e.get("context").and_then(Item::as_str) == Some("dialog"))
            .expect("a new entry for the missing context must be created");
        assert_eq!(
            dialog_entry["keys"]["escape"].as_str(),
            Some("dialog::close")
        );
        let ws_entry = bindings
            .iter()
            .find(|e| e.get("context").and_then(Item::as_str) == Some("workspace"))
            .unwrap();
        assert_eq!(
            ws_entry["keys"]["mod+h"].as_str(),
            Some("workspace::focus_left"),
            "the pre-existing entry must be untouched"
        );
    }

    #[test]
    fn old_key_shadowing_writes_none_for_a_lower_layer_binding() {
        let dir = tempfile::tempdir().unwrap();
        let r = Rebind {
            context: Some("workspace".to_string()),
            new_key: "mod+shift+h".to_string(),
            action: "workspace::focus_left".to_string(),
            old_key: Some("mod+h".to_string()),
            old_key_is_user_layer: false,
        };
        let outcome = apply_rebind(dir.path(), &r).unwrap();
        assert_eq!(outcome.displacement, Displacement::Displaced);

        let text = read(dir.path());
        let doc: Doc = text.parse().unwrap();
        let entry = doc["bindings"]
            .as_array_of_tables()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(
            entry["keys"]["mod+shift+h"].as_str(),
            Some("workspace::focus_left")
        );
        assert_eq!(
            entry["keys"]["mod+h"].as_str(),
            Some("none"),
            "a lower-layer old key must be shadowed with none, not removed"
        );
    }

    #[test]
    fn user_layer_old_key_is_removed_not_shadowed() {
        let dir = tempfile::tempdir().unwrap();
        let original = "\
config_version = 1

[[bindings]]
context = \"workspace\"
[bindings.keys]
\"ctrl+shift+x\" = \"workspace::focus_left\"
";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let r = Rebind {
            context: Some("workspace".to_string()),
            new_key: "mod+shift+h".to_string(),
            action: "workspace::focus_left".to_string(),
            old_key: Some("ctrl+shift+x".to_string()),
            old_key_is_user_layer: true,
        };
        let outcome = apply_rebind(dir.path(), &r).unwrap();
        assert_eq!(outcome.displacement, Displacement::Displaced);

        let text = read(dir.path());
        let doc: Doc = text.parse().unwrap();
        let entry = doc["bindings"]
            .as_array_of_tables()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(
            entry["keys"]["mod+shift+h"].as_str(),
            Some("workspace::focus_left")
        );
        assert!(
            entry.get("keys").unwrap().get("ctrl+shift+x").is_none(),
            "a user-layer old key must be removed outright, not left as none"
        );
    }

    #[test]
    fn displacement_reports_old_key_not_found_when_the_user_layer_claim_is_wrong() {
        // old_key_is_user_layer: true claims "ctrl+shift+x" is a user-layer
        // binding already present in this entry, but the file (and thus the
        // matched/created entry's keys table) doesn't actually contain it —
        // a stale caller read, or a mismatched context. This must not
        // silently report success: the caller needs to know the displacement
        // didn't actually happen.
        let dir = tempfile::tempdir().unwrap();
        let r = Rebind {
            context: Some("workspace".to_string()),
            new_key: "mod+shift+h".to_string(),
            action: "workspace::focus_left".to_string(),
            old_key: Some("ctrl+shift+x".to_string()),
            old_key_is_user_layer: true,
        };
        let outcome = apply_rebind(dir.path(), &r).unwrap();
        assert_eq!(
            outcome.displacement,
            Displacement::OldKeyNotFound,
            "a missing old_key under old_key_is_user_layer must not be reported as Displaced"
        );

        // The rebind itself must still have gone through: new_key is bound
        // even though the displacement step found nothing to do.
        let text = read(dir.path());
        let doc: Doc = text.parse().unwrap();
        let entry = doc["bindings"]
            .as_array_of_tables()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(
            entry["keys"]["mod+shift+h"].as_str(),
            Some("workspace::focus_left")
        );
    }

    #[test]
    fn same_key_rebind_does_not_clobber_the_new_binding_user_layer_case() {
        let dir = tempfile::tempdir().unwrap();
        let r = Rebind {
            context: Some("workspace".to_string()),
            new_key: "ctrl+shift+x".to_string(),
            action: "workspace::focus_left".to_string(),
            old_key: Some("ctrl+shift+x".to_string()),
            old_key_is_user_layer: true,
        };
        let outcome = apply_rebind(dir.path(), &r).unwrap();
        assert_eq!(outcome.displacement, Displacement::NotRequested);

        let text = read(dir.path());
        let doc: Doc = text.parse().unwrap();
        let entry = doc["bindings"]
            .as_array_of_tables()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(
            entry["keys"]["ctrl+shift+x"].as_str(),
            Some("workspace::focus_left"),
            "old_key == new_key must never result in the just-written binding being removed"
        );
    }

    #[test]
    fn same_key_rebind_does_not_clobber_the_new_binding_shadow_case() {
        let dir = tempfile::tempdir().unwrap();
        let r = Rebind {
            context: Some("workspace".to_string()),
            new_key: "mod+h".to_string(),
            action: "workspace::focus_left".to_string(),
            old_key: Some("mod+h".to_string()),
            old_key_is_user_layer: false,
        };
        let outcome = apply_rebind(dir.path(), &r).unwrap();
        assert_eq!(outcome.displacement, Displacement::NotRequested);

        let text = read(dir.path());
        let doc: Doc = text.parse().unwrap();
        let entry = doc["bindings"]
            .as_array_of_tables()
            .unwrap()
            .get(0)
            .unwrap();
        assert_eq!(
            entry["keys"]["mod+h"].as_str(),
            Some("workspace::focus_left"),
            "old_key == new_key must never write none over the just-written binding"
        );
    }

    #[test]
    fn a_corrupt_existing_file_is_left_untouched_and_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymap.toml");
        let corrupt = "this is [not valid toml";
        std::fs::write(&path, corrupt).unwrap();

        let result = apply_rebind(dir.path(), &rebind(None, "ctrl+k", "palette::toggle"));
        assert!(result.is_err(), "a parse failure must return Err");

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, corrupt, "a corrupt file must never be written to");

        let leftover_tmp: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(
            leftover_tmp.is_empty(),
            "no temp file should be left behind on a parse failure"
        );
    }

    #[test]
    fn temp_file_used_during_write_does_not_end_in_toml() {
        // After a successful write, only the final keymap document remains;
        // no temporary `.toml` document may be visible to the reload scanner.
        let dir = tempfile::tempdir().unwrap();
        apply_rebind(dir.path(), &rebind(None, "ctrl+k", "palette::toggle")).unwrap();

        let leftover_tmp: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert!(
            leftover_tmp.is_empty(),
            "no *.tmp files should remain after a successful write: {leftover_tmp:?}"
        );

        let toml_files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
            .collect();
        assert_eq!(
            toml_files.len(),
            1,
            "exactly one real keymap.toml must exist, no stray *.toml temp files"
        );
    }

    // Round-trip persisted files through the production compiler as well as TOML parsing.

    fn registry_with(action_id: &str) -> ActionRegistry {
        let mut reg = ActionRegistry::default();
        reg.register(ActionDef {
            id: ActionId(action_id.to_string()),
            title: action_id.to_string(),
            category: "Test".to_string(),
        })
        .unwrap();
        reg
    }

    fn user_layer_doc(dir: &Path) -> LayerDoc {
        let text = read(dir);
        LayerDoc {
            layer: Layer::User,
            name: "keymap".to_string(),
            file: dir.join("keymap.toml"),
            table: text.parse().expect("apply_rebind must write valid TOML"),
        }
    }

    #[test]
    fn a_freshly_created_file_builds_clean_through_the_real_production_path() {
        let dir = tempfile::tempdir().unwrap();
        apply_rebind(
            dir.path(),
            &rebind(Some("workspace"), "ctrl+shift+x", "workspace::focus_left"),
        )
        .unwrap();

        let doc = user_layer_doc(dir.path());
        let (keymap, diags) = build_keymap(
            &[doc],
            KeymapModifiers::ALT,
            &registry_with("workspace::focus_left"),
        );
        assert!(
            diags.is_empty(),
            "a freshly written keymap.toml must build with zero diagnostics: {diags:?}"
        );
        let binding = keymap
            .bindings()
            .iter()
            .find(|b| b.action.0 == "workspace::focus_left")
            .expect("the rebound action must be present in the built keymap");
        assert_eq!(binding.keystrokes.len(), 1);
        assert_eq!(binding.keystrokes[0].key, "x");
        assert!(binding.keystrokes[0].mods.ctrl && binding.keystrokes[0].mods.shift);
        assert_eq!(binding.context_source.as_deref(), Some("workspace"));
    }

    #[test]
    fn an_edited_existing_file_builds_clean_through_the_real_production_path() {
        let dir = tempfile::tempdir().unwrap();
        let original = "\
config_version = 1

[[bindings]]
context = \"workspace\"
[bindings.keys]
\"mod+h\" = \"workspace::focus_left\"

[[bindings]]
[bindings.keys]
\"ctrl+k\" = \"palette::toggle\"
";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let r = Rebind {
            context: Some("workspace".to_string()),
            new_key: "mod+shift+h".to_string(),
            action: "workspace::focus_left".to_string(),
            old_key: Some("mod+h".to_string()),
            old_key_is_user_layer: false,
        };
        apply_rebind(dir.path(), &r).unwrap();

        let doc = user_layer_doc(dir.path());
        let mut reg = registry_with("workspace::focus_left");
        reg.register(ActionDef {
            id: ActionId("palette::toggle".to_string()),
            title: "Toggle palette".to_string(),
            category: "Test".to_string(),
        })
        .unwrap();

        let (keymap, diags) = build_keymap(&[doc], KeymapModifiers::ALT, &reg);
        assert!(
            diags.is_empty(),
            "an edited keymap.toml must build with zero diagnostics: {diags:?}"
        );

        // The old key was shadowed with "none" (UNBOUND_ACTION), so it must
        // build clean (none needs no registered action) but not resolve to
        // workspace::focus_left any more.
        let ws_bindings: Vec<_> = keymap
            .bindings()
            .iter()
            .filter(|b| b.context_source.as_deref() == Some("workspace"))
            .collect();
        assert!(
            ws_bindings
                .iter()
                .any(|b| b.action.0 == "workspace::focus_left"
                    && b.keystrokes[0].key == "h"
                    && b.keystrokes[0].mods.shift),
            "the new binding must be present and resolve to the rebound action"
        );
        assert!(
            ws_bindings
                .iter()
                .any(|b| b.action.0 == crate::keymap::UNBOUND_ACTION
                    && b.keystrokes[0].key == "h"
                    && !b.keystrokes[0].mods.shift),
            "the displaced old key must build as an explicit none/unbound entry"
        );
        // The unrelated entry must still be present and untouched.
        assert!(
            keymap
                .bindings()
                .iter()
                .any(|b| b.action.0 == "palette::toggle")
        );
    }

    /// A binding that comes from builtin or desk cannot be removed — this
    /// module only ever writes the user layer — so it is silenced with the
    /// documented `"none"` shadow instead.
    #[test]
    fn unbinding_a_lower_layer_binding_writes_a_none_shadow() {
        let dir = tempfile::tempdir().unwrap();
        let out = apply_unbind(
            dir.path(),
            &Unbind {
                context: None,
                key: "ctrl+k".into(),
                is_user_layer: false,
            },
        )
        .expect("write");
        assert!(!out.removed, "a shadow is not a removal");
        let text = read(dir.path());
        assert!(text.contains(r#""ctrl+k" = "none""#), "{text}");
    }

    /// The user's own binding is removed outright, leaving no redundant
    /// `"none"` in a table this module owns.
    #[test]
    fn unbinding_a_user_layer_binding_removes_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let original = "config_version = 1\n\n[[bindings]]\n\n[bindings.keys]\n\
             \"ctrl+k\" = \"palette::toggle\"\n\"ctrl+j\" = \"tile::focus_down\"\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let out = apply_unbind(
            dir.path(),
            &Unbind {
                context: None,
                key: "ctrl+k".into(),
                is_user_layer: true,
            },
        )
        .expect("write");
        assert!(out.removed);
        let text = read(dir.path());
        assert!(!text.contains("ctrl+k"), "the key is gone: {text}");
        assert!(text.contains("ctrl+j"), "siblings survive: {text}");
        assert!(!text.contains("none"), "no redundant shadow: {text}");
    }

    /// The caller's belief about where a binding lives can be stale. Removal
    /// that finds nothing reports it rather than failing — the same
    /// `Displacement::OldKeyNotFound` contract `apply_rebind` already keeps.
    #[test]
    fn a_removal_that_finds_nothing_reports_it_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let original = "config_version = 1\n\n[[bindings]]\n\n[bindings.keys]\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let out = apply_unbind(
            dir.path(),
            &Unbind {
                context: None,
                key: "ctrl+k".into(),
                is_user_layer: true,
            },
        )
        .expect("a stale belief is not a write failure");
        assert!(!out.removed);
    }

    /// Comments and unrelated tables survive, as they do for every other
    /// keyed persist in this crate.
    #[test]
    fn unbinding_preserves_comments_and_unrelated_entries() {
        let dir = tempfile::tempdir().unwrap();
        let original = "# my keymap\nconfig_version = 1\n\n[[bindings]]\ncontext = \"tile\"\n\n\
             [bindings.keys]\n\"ctrl+k\" = \"tile::close\"\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        apply_unbind(
            dir.path(),
            &Unbind {
                context: None,
                key: "ctrl+k".into(),
                is_user_layer: false,
            },
        )
        .expect("write");
        let text = read(dir.path());
        assert!(text.contains("# my keymap"), "{text}");
        assert!(
            text.contains(r#"context = "tile""#),
            "the tile entry is untouched: {text}"
        );
        assert!(text.contains(r#""ctrl+k" = "tile::close""#), "{text}");
        assert!(
            text.contains(r#""ctrl+k" = "none""#),
            "the no-context entry got the shadow: {text}"
        );
    }

    /// The compiler accepts inline binding arrays, but editing rejects that
    /// shape without discarding the existing file.
    #[test]
    fn bindings_as_a_plain_array_is_rejected_without_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let original =
            "config_version = 1\nbindings = [ { keys = { \"ctrl+k\" = \"palette::toggle\" } } ]\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let err = apply_unbind(
            dir.path(),
            &Unbind {
                context: None,
                key: "ctrl+k".into(),
                is_user_layer: true,
            },
        )
        .expect_err("bindings as a plain array must be rejected, not silently replaced");
        assert!(
            err.contains("not an array of tables"),
            "error should name the problem: {err}"
        );

        let text = read(dir.path());
        assert_eq!(
            text, original,
            "a rejected write must leave the file byte-for-byte untouched"
        );
    }

    /// An inline keys table remains editable through the same TableLike interface.
    #[test]
    fn keys_as_an_inline_table_does_not_panic_and_stays_editable() {
        let dir = tempfile::tempdir().unwrap();
        let original =
            "config_version = 1\n\n[[bindings]]\nkeys = { \"ctrl+k\" = \"palette::toggle\" }\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let out = apply_unbind(
            dir.path(),
            &Unbind {
                context: None,
                key: "ctrl+k".into(),
                is_user_layer: true,
            },
        )
        .expect("an inline keys table must not panic and must be editable");
        assert!(out.removed);
        let text = read(dir.path());
        assert!(!text.contains("ctrl+k"), "{text}");
    }

    fn user_override(context: Option<&str>, key: &str) -> UserOverride {
        UserOverride {
            context_source: context.map(str::to_string),
            key: key.to_string(),
        }
    }

    #[test]
    fn resetting_removes_every_named_key_in_one_write() {
        // The rebind pair: the new key and the `"none"` shadow over the old
        // one, both in the same entry. One reset removes both.
        let dir = tempfile::tempdir().unwrap();
        let original = "config_version = 1\n\n[[bindings]]\ncontext = \"workspace\"\n\n\
             [bindings.keys]\n\"mod+j\" = \"workspace::focus_left\"\n\"mod+h\" = \"none\"\n\
             \"mod+l\" = \"workspace::focus_right\"\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let outcome = apply_reset(
            dir.path(),
            &[
                user_override(Some("workspace"), "mod+h"),
                user_override(Some("workspace"), "mod+j"),
            ],
        )
        .expect("write");
        assert_eq!(outcome, ResetOutcome { removed: 2 });

        let text = read(dir.path());
        assert!(!text.contains("mod+j"), "{text}");
        assert!(!text.contains("mod+h"), "{text}");
        assert!(
            text.contains(r#""mod+l" = "workspace::focus_right""#),
            "an unrelated key in the same entry survives: {text}"
        );
    }

    #[test]
    fn resetting_a_key_that_is_not_there_counts_nothing_and_creates_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let original = "config_version = 1\n\n[[bindings]]\n\n[bindings.keys]\n\
             \"mod+h\" = \"workspace::focus_left\"\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let outcome =
            apply_reset(dir.path(), &[user_override(Some("blotter"), "mod+h")]).expect("write");
        assert_eq!(outcome, ResetOutcome { removed: 0 });

        let text = read(dir.path());
        assert!(
            !text.contains("blotter"),
            "a reset must never create a [[bindings]] entry: {text}"
        );
        assert!(
            text.contains(r#""mod+h" = "workspace::focus_left""#),
            "{text}"
        );
    }

    #[test]
    fn resetting_preserves_comments_and_the_files_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let original = "# my keymap\nconfig_version = 1\nmod = \"alt\"\n\n[[bindings]]\n\n\
             [bindings.keys]\n# left\n'mod+h' = \"workspace::focus_left\"\n\
             \"mod+l\" = \"workspace::focus_right\"\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        apply_reset(dir.path(), &[user_override(None, "mod+h")]).expect("write");
        let text = read(dir.path());
        assert!(text.contains("# my keymap"), "{text}");
        assert!(text.contains(r#"mod = "alt""#), "{text}");
        assert!(!text.contains("'mod+h'"), "{text}");
        assert!(
            text.contains(r#""mod+l" = "workspace::focus_right""#),
            "{text}"
        );
    }

    #[test]
    fn reset_all_drops_every_bindings_entry_and_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let original = "# my keymap\nconfig_version = 1\nmod = \"alt\"\n\n\
             [[bindings]]\ncontext = \"workspace\"\n\n[bindings.keys]\n\
             \"mod+j\" = \"workspace::focus_left\"\n\"mod+h\" = \"none\"\n\n\
             [[bindings]]\n\n[bindings.keys]\n\"ctrl+k\" = \"tile::close\"\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        let outcome = apply_reset_all(dir.path()).expect("write");
        assert_eq!(outcome, ResetOutcome { removed: 3 });

        let text = read(dir.path());
        assert!(text.contains("# my keymap"), "{text}");
        assert!(text.contains(r#"mod = "alt""#), "{text}");
        assert!(!text.contains("[[bindings]]"), "{text}");
        assert!(!text.contains("tile::close"), "{text}");

        // And the file still builds clean as a keymap with no bindings.
        let (keymap, diags) = crate::keymap::build_keymap(
            &[user_layer_doc(dir.path())],
            KeymapModifiers::ALT,
            &registry_with("tile::close"),
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(keymap.bindings().is_empty());
    }

    #[test]
    fn reset_all_on_a_missing_file_removes_nothing_and_writes_a_fresh_one() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = apply_reset_all(dir.path()).expect("write");
        assert_eq!(outcome, ResetOutcome { removed: 0 });
        let text = read(dir.path());
        assert!(text.contains("config_version = 1"), "{text}");
    }

    #[test]
    fn reset_all_leaves_a_corrupt_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let original = "this is = not [ toml\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();
        assert!(apply_reset_all(dir.path()).is_err());
        assert_eq!(read(dir.path()), original);
    }

    /// Replacing a key's value preserves its leading comment and custom
    /// quoting. Both rebind and lower-layer unbind use the same write helper.
    #[test]
    fn overwriting_an_existing_key_preserves_its_comment_and_quoting() {
        let dir = tempfile::tempdir().unwrap();
        let original = "config_version = 1\n\n[[bindings]]\n\n[bindings.keys]\n\
             # my comment\n'mod+h' = \"workspace::focus_left\"\n";
        std::fs::write(dir.path().join("keymap.toml"), original).unwrap();

        apply_rebind(dir.path(), &rebind(None, "mod+h", "workspace::focus_right")).unwrap();

        let text = read(dir.path());
        assert!(
            text.contains("# my comment"),
            "comment must survive: {text}"
        );
        assert!(
            text.contains("'mod+h'"),
            "custom quoting must survive: {text}"
        );
        assert!(
            text.contains("workspace::focus_right"),
            "the value must still be updated: {text}"
        );
    }

    /// The shipped keymap with the renames the grid modules register, plus
    /// the user file in `dir`, built as the app builds it.
    fn motion_keymap(dir: &Path) -> crate::keymap::Keymap {
        let mut reg = ActionRegistry::default();
        crate::defaults::register_builtin_actions(&mut reg);
        reg.register_rename("blotter::down", "motion::down")
            .unwrap();
        reg.register_rename("pricer::down", "motion::down").unwrap();
        let builtin = LayerDoc::builtin("keymap", crate::defaults::BUILTIN_KEYMAP).unwrap();
        let user = LayerDoc {
            layer: Layer::User,
            name: "keymap".to_string(),
            file: dir.join("keymap.toml"),
            table: read(dir).parse().unwrap(),
        };
        build_keymap(&[builtin, user], KeymapModifiers::ALT, &reg).0
    }

    /// What `j` does in a blotter grid in visual mode under `keymap`.
    fn j_in_blotter_visual(keymap: &crate::keymap::Keymap) -> crate::keymap::MatchResult {
        use crate::keymap::{KeyContext, Matcher, parse_keystroke};
        let stack = vec![
            KeyContext::new("workspace"),
            KeyContext::new("tile"),
            KeyContext::new("blotter").grid().pair("mode", "visual"),
        ];
        let j = parse_keystroke("j", KeymapModifiers::ALT).unwrap();
        Matcher::default().press(keymap, j, &stack)
    }

    /// An old dialog rebind of `blotter::down` left `j = "none"` under the
    /// blotter's visual context. `r` on Motion: down resets through the
    /// override set, and `j` moves the blotter again afterwards: the orphan
    /// shadow went with the renamed key.
    #[test]
    fn resetting_a_motion_lifts_an_old_module_rebinds_orphan_shadow() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "config_version = 1\n\n[[bindings]]\ncontext = \"blotter && mode == visual\"\n\
             [bindings.keys]\n\"n\" = \"blotter::down\"\n\"j\" = \"none\"\n",
        )
        .unwrap();
        let before = motion_keymap(dir.path());
        assert_eq!(
            j_in_blotter_visual(&before),
            crate::keymap::MatchResult::NoMatch,
            "fixture: the old shadow silences j"
        );
        let down = ActionId("motion::down".to_string());
        let overrides = crate::keymap::user_overrides_for(before.bindings(), &down);
        assert_eq!(overrides.len(), 2, "{overrides:?}");

        apply_reset(dir.path(), &overrides).unwrap();

        assert_eq!(
            j_in_blotter_visual(&motion_keymap(dir.path())),
            crate::keymap::MatchResult::Matched {
                action: down,
                count: None
            },
            "{}",
            read(dir.path())
        );
    }
}
