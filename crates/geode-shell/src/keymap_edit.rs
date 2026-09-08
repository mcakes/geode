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
//! Both of those guarantees are now [`crate::config_write`]'s, not this
//! module's: the read-or-create-and-parse half comes from
//! `config_write::open` (which is also where the `config_version = 1`
//! stamp and the untouched-on-parse-failure refusal live), and the write
//! from `config_write::write`. This module keeps only the one refusal
//! that is genuinely keymap-shaped — a `bindings` key of the wrong TOML
//! shape, see [`open_doc_with_bindings`] — which is exactly why it uses
//! `open`/`write` rather than `config_write::edit`: `edit`'s closure
//! cannot fail, and this one must.
//!
//! Phase 4c collapsed the three `write_atomic` copies (this module's,
//! `theme`'s and `session`'s) into that one door. The earlier note here
//! recorded not sharing them as a deliberate scoping choice; the reason
//! it gave — that a fourth copy was cheaper than editing reviewed
//! modules — stopped holding once the config dialogs would have made it
//! ten. The write is still atomic (unique temp file in `user_dir`,
//! `fsync`, rename) and the temp filename still ends in `.tmp`, not
//! `.toml`, so `reload::scan`'s `*.toml` glob never observes a partial
//! write mid-flight.

use std::path::Path;

use geode_core::config::Layer;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, TableLike, value};

/// The layered config document this module writes: `<user_dir>/keymap.toml`
/// — never `app.toml`, since key bindings are the keymap doc's own concern
/// (spec §3.1/§3.4). Named once here so the write, the parse and the error
/// messages can never drift onto different files.
const KEYMAP_DOC: &str = "keymap";

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
    let mut doc = open_doc_with_bindings(user_dir)?;
    let bindings = doc["bindings"]
        .as_array_of_tables_mut()
        .expect("open_doc_with_bindings just ensured this");

    let keys = keys_table_for(bindings, rebind.context.as_deref());

    set_key(keys, rebind.new_key.as_str(), value(rebind.action.as_str()));

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
                set_key(keys, old_key.as_str(), value("none"));
                Displacement::Displaced
            }
        }
        _ => Displacement::NotRequested,
    };

    crate::config_write::write(user_dir, Layer::User, KEYMAP_DOC, &doc.to_string())?;
    Ok(RebindOutcome { displacement })
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
    /// Whether the binding being silenced was itself set by a user-layer
    /// entry. `true` removes the key outright; `false` shadows a
    /// builtin/desk binding by writing [`crate::keymap::UNBOUND_ACTION`].
    ///
    /// Getting this backwards is the dangerous case, not a cosmetic one: a
    /// wrong `true` deletes whatever the user *did* have on that key, and a
    /// wrong `false` leaves a redundant `"none"` shadowing the user's own
    /// entry so the key stays dead. See [`Rebind::old_key_is_user_layer`]
    /// for the identical rule stated the other way round.
    pub is_user_layer: bool,
}

/// What [`apply_unbind`] did. `removed` is false for a shadow write, and
/// also for a removal that found nothing to remove — the caller's belief
/// about where the binding lives can be stale, which is a warning rather
/// than a failure (the same contract [`Displacement::OldKeyNotFound`]
/// keeps for a rebind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnbindOutcome {
    pub removed: bool,
}

/// Silence one binding in `<user_dir>/keymap.toml`. `Err` only ever means
/// the file read/parse/write itself failed; the file is left untouched on
/// a parse error, exactly as [`apply_rebind`] leaves it.
///
/// This is precisely [`apply_rebind`]'s step 2 (displacement) performed on
/// its own, with no step 1 new-binding write first: locate (or create) the
/// `[[bindings]]` entry matching `unbind.context` via the same
/// [`keys_table_for`] helper, then either remove `unbind.key` from its
/// `keys` table (`is_user_layer: true`) or shadow it with
/// [`crate::keymap::UNBOUND_ACTION`] (`is_user_layer: false`) — see the
/// module doc's "Semantics" section and [`Unbind::is_user_layer`]'s own
/// doc for why getting that branch backwards is the dangerous case.
pub fn apply_unbind(user_dir: &Path, unbind: &Unbind) -> Result<UnbindOutcome, String> {
    let mut doc = open_doc_with_bindings(user_dir)?;
    let bindings = doc["bindings"]
        .as_array_of_tables_mut()
        .expect("open_doc_with_bindings just ensured this");

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

    crate::config_write::write(user_dir, Layer::User, KEYMAP_DOC, &doc.to_string())?;
    Ok(UnbindOutcome { removed })
}

/// Read `<user_dir>/keymap.toml` through [`crate::config_write::open`]
/// (which reads it if it exists, or starts a fresh document stamped with
/// `config_version = 1` when it doesn't, and refuses an unparseable one
/// without touching it), then ensure `bindings` is ready to index into as
/// an [`ArrayOfTables`]. Shared by [`apply_rebind`] and [`apply_unbind`],
/// so this one check protects both.
///
/// `bindings = [ { ... } ]` is a *legal* keymap document —
/// `keymap::build_keymap`/`build.rs` read `bindings` as a plain TOML array
/// and don't care whether it round-trips through `toml_edit` as
/// `ArrayOfTables` or as a bare `Value::Array` of inline tables — but only
/// the former is exposed by `as_array_of_tables`/`as_array_of_tables_mut`.
/// Treating "present but the wrong shape" the same as "missing entirely"
/// would silently replace it with an empty `ArrayOfTables`, and the write
/// that follows would then destroy every binding the file had — exactly
/// the corruption the module doc's "Corrupt file / atomicity" guarantee
/// promises never happens. So this case is `Err`, same as a parse failure,
/// and the file is left byte-for-byte untouched.
fn open_doc_with_bindings(user_dir: &Path) -> Result<DocumentMut, String> {
    let path = crate::config_write::doc_path(user_dir, Layer::User, KEYMAP_DOC)?;
    let mut doc = crate::config_write::open(user_dir, Layer::User, KEYMAP_DOC)?;

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

    Ok(doc)
}

/// Find the `[[bindings]]` entry whose `context` exactly matches `context`
/// (`None` matching the no-`context` entry — see the module doc's
/// "Semantics" section), creating one if none exists, and return that
/// entry's `keys` table as a [`TableLike`], creating it too if necessary.
/// Shared by [`apply_rebind`] and [`apply_unbind`] — both only ever need to
/// reach the same `keys` table before writing or removing one entry in it.
///
/// `TableLike`, not the concrete `Table`: `keys = { "ctrl+k" = "..." }` (an
/// inline table) is just as legal a keymap document as `[bindings.keys]`
/// — `Item::is_table_like` is true for both — but `Item::as_table_mut`
/// returns `None` for the inline case, so returning `&mut Table` here
/// forced every caller through an `.expect()` that could panic on a file
/// this crate itself never writes but happily reads back. Only
/// `as_table_like_mut` covers both shapes, so this is the one place that
/// must.
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

/// Set `keys[key] = item`, the way every write in this module needs to:
/// preserving an already-present key's own comment and quoting, exactly
/// as the module doc's comment-preserving promise requires.
///
/// `TableLike::insert`'s occupied-entry branch calls
/// `entry.key_mut().fmt()`, which resets that key's own representation —
/// stripping a leading comment and reverting custom quoting (e.g.
/// `'mod+h'`) to a plain double-quoted key — even though only the *value*
/// was meant to change. `get_mut` touches only the value slot when the
/// key already exists, leaving its decor untouched; `insert` is used only
/// on the vacant path, where there is no existing decor to lose.
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

    /// `bindings = [ { ... } ]` is a *legal* keymap document — `build.rs`
    /// reads `bindings` as a plain TOML array and does not care whether it
    /// round-trips through `toml_edit` as `ArrayOfTables` or as a bare
    /// `Value::Array` of inline tables — but `toml_edit`'s own
    /// `as_array_of_tables` only recognises the former. Treating "present
    /// but the wrong shape" the same as "absent" would silently replace it
    /// with an empty `ArrayOfTables` and then write that back out,
    /// destroying every binding the file had. This must be `Err`, and the
    /// file must come back byte-for-byte unchanged — "returned Err" and
    /// "did not destroy the file" are different claims, so both are
    /// checked.
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

    /// `keys = { "ctrl+k" = "..." }` (an inline table) is just as legal a
    /// keymap document as `[bindings.keys]` — `build.rs` reads through the
    /// same generic TOML value either way — but `Item::is_table_like` is
    /// true for an inline table while `Item::as_table_mut` returns `None`
    /// for one, so the old `.expect("just ensured 'keys' is a table")`
    /// panicked on exactly the input its own guard claimed to have
    /// handled. A panic here is reachable from a keystroke once Task 4
    /// wires `d` to `apply_unbind`, which PHILOSOPHY forbids outright.
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

    /// Overwriting an already-present key must preserve that key's own
    /// comment and quoting. Round 1's index-to-insert conversion
    /// regressed this: `TableLike::insert`'s occupied-entry branch calls
    /// `entry.key_mut().fmt()`, which resets the key's own formatting —
    /// stripping a leading comment and reverting custom quoting (e.g.
    /// `'mod+h'`) to a plain double-quoted key — while indexing
    /// assignment (what round 1 replaced) touched only the value slot.
    /// This fires on real paths: overwriting a binding the user already
    /// has, and the same-key rebind edge case.
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
}
