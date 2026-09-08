//! Keymap rebind persistence: comment-preserving edits to the *user* layer
//! keymap document (`<user_dir>/keymap.toml`) — never `app.toml`, since key
//! bindings are the keymap doc's own concern (spec §3.1/§3.4), and never the
//! builtin or desk layers (this crate's contract: only the user layer is
//! ever written from inside the running app).
//!
//! This is the write half of the keybinding dialog (Part B fills in the UI
//! that calls it); reads/resolution still go entirely through the ordinary
//! `keymap::build_keymap` layered-merge path — this module only ever adds
//! or edits `[[bindings]]` entries in the user document, then leaves the
//! next config load (or the hot-reload watcher's next ~500ms tick — the
//! write lands inside a directory `reload::scan` polls, and the target
//! filename is `keymap.toml`, which is *not* `session.toml` and so is not
//! excluded from that poll; that pickup is intended, not a bug to guard
//! against) to pick the change up like any other on-disk edit.
//!
//! `toml_edit`, not the plain `toml` crate, for the same reason
//! `theme::persist_to_user_config` gives: a hand-written `keymap.toml` can
//! carry comments and unrelated `[[bindings]]` entries/tables this write
//! knows nothing about, and only a format-preserving editor can touch just
//! the one entry/key involved without clobbering the rest of the document.
//!
//! ## Semantics
//!
//! [`apply_rebind`] locates (or creates) the `[[bindings]]` entry whose
//! `context` exactly matches [`Rebind::context`] (`None` means the
//! no-`context` entry — the one every non-workspace binding in
//! `defaults::BUILTIN_KEYMAP` uses), then:
//!
//! 1. Sets `keys.<new_key> = action` in that entry — the new binding. This
//!    step always succeeds (it's an unconditional table write), so a
//!    successful `apply_rebind` always means `new_key` is bound.
//! 2. Displaces the old binding, *unless* `old_key == Some(new_key)` (a
//!    same-key "rebind" — see the note below): if [`Rebind::
//!    old_key_is_user_layer`], removes `old_key` from that same entry's
//!    `keys` table outright (it was the user's own prior override for this
//!    action; deleting it reverts to whatever the lower layers say, rather
//!    than leaving a redundant `"none"` in a table this write already
//!    owns). Otherwise, `old_key`'s current effective binding comes from a
//!    layer this module can never touch (builtin or desk) — so it's
//!    *shadowed* instead, by setting `keys.<old_key> = "none"` in the user
//!    entry (`keymap::UNBOUND_ACTION`), the documented way a higher layer
//!    silences a lower one; this branch is an unconditional write too (a
//!    fresh `"none"` shadow is correct whether or not `old_key` already had
//!    a value in this entry), so it always succeeds.
//!
//! Unlike step 1's write and the shadow branch of step 2, the *removal*
//! branch of step 2 (`old_key_is_user_layer: true`) can fail to find
//! anything to remove — the caller's belief that `old_key` lives in this
//! exact matched/created entry can be wrong (a stale read, a mismatched
//! `context`). Silently treating that as success (`keys.remove` is a no-op
//! on a missing key) would leave the caller believing the old binding was
//! displaced when it wasn't — the action could still be reachable from
//! wherever `old_key` actually lives. [`apply_rebind`] therefore returns a
//! [`RebindOutcome`] whose [`Displacement`] distinguishes this
//! (`OldKeyNotFound`) from an actual removal (`Displaced`) and from there
//! being nothing to displace at all (`NotRequested`) — see [`Displacement`]
//! for the exact rule per case. The write still completes in every case
//! (an `Err` from this function only ever means the *file* operation
//! failed — see "Corrupt file / atomicity" below — never a displacement
//! miss); the caller (Part B) is expected to warn the user on
//! `OldKeyNotFound` rather than silently trusting the rebind was clean.
//!
//! **Same-key edge case**: when `old_key_is_user_layer` is true and the
//! caller passes `old_key == Some(new_key.clone())` (rebinding an action to
//! the very key it's already bound to in the user layer, or any other
//! caller-side reason the two happen to coincide), step 2 must not run at
//! all — the `old_key_is_user_layer` remove-branch would delete the exact
//! `keys` entry step 1 just wrote (both live at the same TOML key), and the
//! shadow-branch would equally clobber it by immediately overwriting that
//! same key with `"none"`. Guarded by one explicit `old_key != new_key`
//! check that skips *all* of step 2, not just the risky branch, reporting
//! [`Displacement::NotRequested`] — see
//! `same_key_rebind_does_not_clobber_the_new_binding` below.
//!
//! ## Corrupt file / atomicity
//!
//! An existing file that fails to parse returns `Err` and is left byte-for-
//! byte untouched — a user's hand-edited `keymap.toml`, however broken,
//! must never be destroyed by a UI-driven rebind. A missing file is created
//! fresh with `config_version = 1` at the top, matching every other config
//! document in this codebase.
//!
//! The write itself is atomic (unique temp file in `user_dir`, `fsync`,
//! rename) with its own small copy of the pid+counter `.tmp`-suffixed
//! scheme `session::write_atomic`/`theme::write_atomic` already use —
//! deliberately *not* factored into a shared helper here: `theme.rs`'s own
//! copy already declined to share with `session.rs`'s (different error
//! type: `Result<(), String>` vs `std::io::Result`), and this module's copy
//! matches `theme.rs`'s shape exactly, so extracting a shared helper now
//! would mean editing an unrelated, already-reviewed module just to save a
//! dozen lines in a new one — out of scope for this pass. Recorded here as
//! a deliberate choice, not an oversight, per the same reasoning `theme.rs`
//! already documents for itself. The temp filename ends in `.tmp`, not
//! `.toml`, so `reload::scan`'s `*.toml` glob never observes a partial
//! write mid-flight.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

/// One rebind to apply to the user keymap document. See the module doc for
/// the full write semantics.
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

/// What happened to [`Rebind::old_key`] during an [`apply_rebind`] call. See
/// the module doc's "Semantics" section for the full reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Displacement {
    /// Nothing needed displacing: `old_key` was `None` (the action had no
    /// prior binding), or `old_key == new_key` (the same-key edge case —
    /// displacement is deliberately skipped so it can't clobber the
    /// binding `apply_rebind`'s step 1 just wrote).
    NotRequested,
    /// `old_key` was found and displaced: removed outright
    /// (`old_key_is_user_layer: true`) or shadowed with `"none"`
    /// (`old_key_is_user_layer: false`).
    Displaced,
    /// `old_key_is_user_layer` was `true`, but `old_key` was not actually
    /// present in the matched/created entry's `keys` table, so there was
    /// nothing to remove — the caller's belief about where the old binding
    /// lives was wrong. The rebind still completed (`new_key` is bound),
    /// but the caller should treat this as a warning: the old key may still
    /// be effectively bound to the same action from wherever it actually
    /// lives.
    OldKeyNotFound,
}

/// The outcome of a successful [`apply_rebind`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebindOutcome {
    pub displacement: Displacement,
}

/// Apply one [`Rebind`] to `<user_dir>/keymap.toml`. See the module doc for
/// the full semantics (entry lookup/creation, displacement, the same-key
/// edge case, and the corrupt-file/atomicity guarantees). An `Err` here
/// only ever means the file read/parse/write itself failed — a displacement
/// that couldn't find its target is reported through `Ok`'s
/// [`RebindOutcome`] instead (see [`Displacement::OldKeyNotFound`]), since
/// `new_key` still gets bound either way.
pub fn apply_rebind(user_dir: &Path, rebind: &Rebind) -> Result<RebindOutcome, String> {
    let path = user_dir.join("keymap.toml");
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

    if doc
        .get("bindings")
        .and_then(Item::as_array_of_tables)
        .is_none()
    {
        doc["bindings"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let bindings = doc["bindings"]
        .as_array_of_tables_mut()
        .expect("just ensured 'bindings' is an array of tables");

    let keys = keys_table_for(bindings, rebind.context.as_deref());

    keys[rebind.new_key.as_str()] = value(rebind.action.as_str());

    // Same-key edge case (module doc): skip displacement entirely when it
    // would touch the key `new_key` just wrote.
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
                keys[old_key.as_str()] = value("none");
                Displacement::Displaced
            }
        }
        _ => Displacement::NotRequested,
    };

    write_atomic(user_dir, &path, &doc.to_string())?;
    Ok(RebindOutcome { displacement })
}

/// Find the `[[bindings]]` entry whose `context` exactly matches `context`
/// (`None` matching the no-`context` entry — see the module doc's
/// "Semantics" section), creating one if none exists, and return that
/// entry's `keys` table, creating it too if necessary. Shared by
/// [`apply_rebind`] and `apply_unbind` — both only ever need to reach the
/// same `keys` table before writing or removing one entry in it.
fn keys_table_for<'a>(bindings: &'a mut ArrayOfTables, context: Option<&str>) -> &'a mut Table {
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
        .as_table_mut()
        .expect("just ensured 'keys' is a table")
}

/// Process-global counter for [`apply_rebind`]'s temp filenames — same
/// reasoning as `theme.rs`'s/`session.rs`'s own `TMP_COUNTER`: a distinct
/// name per call, on top of the pid, so concurrent writers in this process
/// never interleave on one shared temp file.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomic write for [`apply_rebind`]: unique temp file in `dir`, `fsync`,
/// rename over `path`. See the module doc's "Corrupt file / atomicity"
/// section for why this is its own small copy rather than a shared helper.
fn write_atomic(dir: &Path, path: &Path, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;

    let pid = std::process::id();
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = dir.join(format!(".keymap.toml.{pid}-{counter}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("failed to create {}: {e}", tmp_path.display()))?;
        file.write_all(text.as_bytes())
            .map_err(|e| format!("failed to write {}: {e}", tmp_path.display()))?;
        file.sync_all()
            .map_err(|e| format!("failed to sync {}: {e}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, path).map_err(|e| {
        format!(
            "failed to rename {} to {}: {e}",
            tmp_path.display(),
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::DocumentMut as Doc;

    use crate::actions::{ActionDef, ActionId, ActionRegistry};
    use crate::keymap::{Modifiers as KeymapModifiers, build_keymap};
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
        // Regression for the reload-watcher interplay documented at the top
        // of this file: the write targets keymap.toml (which IS watched,
        // intentionally), but the intermediate temp file must never look
        // like a *.toml file to reload::scan's glob, or a reader could
        // observe a partial write mid-rename.
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

    // --- round-trip through the real production path -----------------------
    //
    // The tests above only prove `apply_rebind`'s output is self-consistent
    // TOML (parseable via toml_edit). These prove the file it writes is
    // consumable by the actual reader this app uses in production —
    // `geode_core::config::LayerDoc` + `keymap::build_keymap` — with zero
    // diagnostics, both for a freshly created file and for an edit to an
    // existing one.

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
}
