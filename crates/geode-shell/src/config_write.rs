//! The one door every config write in this crate goes through
//! (`docs/superpowers/specs/2026-09-08-geode-phase-4c-config-dialogs-design.md` §6).
//!
//! Before this module there were three `write_atomic` implementations
//! and six persist paths spread across `theme`, `fontsize`, `vimfind`,
//! `frame` (twice) and `keymap_edit`. Phase 4c's dialogs would have made
//! it ten. One door means one set of guarantees to get right: a write is
//! atomic, only the user layer is writable, and a file this process
//! cannot parse is refused untouched rather than replaced.
//!
//! No `ShellServices` parameter: `user_dir` is what a caller actually
//! has, and threading a whole services struct through the background
//! executor for a path would be worse.
//!
//! ## What the three copies each guaranteed, and what survived here
//!
//! The three implementations this replaces (`theme::write_atomic`,
//! `keymap_edit::write_atomic`, `session::write_atomic`) agreed on the
//! core — `create_dir_all`, a pid+counter-suffixed temp file in the
//! *same* directory as the target, `write_all`, `sync_all`, then a
//! `rename` over the target — and differed in three ways, each of which
//! is kept here rather than dropped:
//!
//! * **Temp filename derived from the target, not hardcoded.**
//!   `theme.rs` derived `.{file_name}.{pid}-{counter}.tmp` from the
//!   target path (finding M9: it had been hardcoded to `.app.toml.*`
//!   back when it only ever wrote `app.toml`, and lied once `frame.rs`
//!   started writing `groupings.toml` through it). The other two
//!   hardcoded `.keymap.toml.*` and `.session.toml.*` — correct only
//!   because each had exactly one target. The derived form is the union:
//!   it reproduces both hardcoded names byte-for-byte for their own
//!   targets and stays honest for every future doc. See [`tmp_file_name`].
//! * **A parent directory that must exist.** `session::write_atomic`
//!   takes a full file path and derives the directory from
//!   `Path::parent`, erroring when there is none; the other two were
//!   handed the directory. [`write_file`] keeps the `parent` derivation
//!   and its error, since it is the primitive both shapes now use.
//! * **The error type.** `theme`/`keymap_edit` returned
//!   `Result<(), String>` with messages naming the path and the failing
//!   step; `session` returned `std::io::Result`. The `String` form is
//!   kept (it is what every persist signature at the seam already
//!   returns) and `session::write_atomic` maps back to `io::Error` at
//!   its own boundary, so its callers are unchanged.
//!
//! None of the three fsync'd the *directory* after the rename, so
//! neither does this: the rename is atomic within a filesystem, and a
//! post-crash-missing-but-not-torn config file is the failure mode this
//! codebase already accepts (see `session::write_atomic`'s own
//! "best-effort, never load-bearing" note). Nothing here is a regression
//! of a guarantee that existed; adding a directory fsync would be a new
//! one, and belongs to whoever wants it, deliberately.
//!
//! The temp name deliberately does not end in `.toml`: `reload::scan`'s
//! `*.toml` glob must never observe a partial write mid-flight.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use geode_core::config::Layer;
use toml_edit::{DocumentMut, value};

/// The path a layered config document lives at, refusing every layer but
/// [`Layer::User`] *before* touching the filesystem.
///
/// The layer guard lives here, on the one function both [`write`] and
/// [`edit`] must call to learn where to write, so there is no way to
/// reach a write without passing it. Only the user layer is ever written
/// from inside the running app (spec §3.1): the builtin layer is
/// compiled in, and the desk layer is shared state a single trader's
/// running app has no business rewriting.
pub(crate) fn doc_path(user_dir: &Path, layer: Layer, doc: &str) -> Result<PathBuf, String> {
    if layer != Layer::User {
        return Err(format!(
            "refusing to write the {} layer's {doc}.toml: only the user layer is writable \
             from inside the app",
            layer.name()
        ));
    }
    Ok(user_dir.join(format!("{doc}.toml")))
}

/// Atomically replace one layered config document with `text`.
///
/// **This is a whole-file replacement and deliberately bypasses
/// [`edit`]'s untouched-on-parse-failure refusal**: nothing here reads,
/// parses, or preserves whatever is on disk, so a user's comments, key
/// order and unrelated tables are gone the moment it succeeds. That is
/// right only when the caller already holds the complete document it
/// means to write — [`open`] plus a mutation, which is how `keymap_edit`
/// uses it. A caller setting one key wants [`edit`] instead; picking
/// this one there silently discards hand-edits, with no failure to
/// notice.
pub fn write(user_dir: &Path, layer: Layer, doc: &str, text: &str) -> Result<(), String> {
    let path = doc_path(user_dir, layer, doc)?;
    write_file(&path, text)
}

/// Read (or create) one layered config document, apply `f` to it, and
/// write it back atomically — the shape every keyed persist in this
/// crate needs.
///
/// `toml_edit`, not the plain `toml` crate: a hand-written config can
/// carry comments and keys this write knows nothing about, and only a
/// format-preserving editor can touch just the one key involved.
///
/// An existing file that fails to parse is an `Err` and is left
/// byte-for-byte untouched — a user's hand-edited file, however broken,
/// is theirs and must never be destroyed by a UI-driven toggle. A
/// missing file is created fresh with `config_version = 1` at the top,
/// matching every other config document this codebase writes.
pub fn edit(
    user_dir: &Path,
    layer: Layer,
    doc: &str,
    f: impl FnOnce(&mut DocumentMut),
) -> Result<(), String> {
    let path = doc_path(user_dir, layer, doc)?;
    let mut document = open_at(&path)?;
    f(&mut document);
    write_file(&path, &document.to_string())
}

/// [`edit`] without the write: read-or-create and parse, handing back the
/// document for the caller to mutate and then [`write`] itself.
///
/// Exists for `keymap_edit`, whose two writers need a *fallible* check
/// between the parse and the mutation (a `bindings` key of the wrong
/// shape is a refusal, not something to overwrite) and need a value out
/// of the mutation (which binding was displaced) — neither of which fits
/// `edit`'s infallible `FnOnce(&mut DocumentMut)`. Keeping it on this
/// side of the door means the parse-refusal contract still has exactly
/// one implementation.
pub(crate) fn open(user_dir: &Path, layer: Layer, doc: &str) -> Result<DocumentMut, String> {
    let path = doc_path(user_dir, layer, doc)?;
    open_at(&path)
}

/// The read-or-create-and-parse half of [`edit`], by path.
fn open_at(path: &Path) -> Result<DocumentMut, String> {
    let existed = path.exists();

    let mut doc = if existed {
        let text = std::fs::read_to_string(path)
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

    Ok(doc)
}

/// Process-global counter giving every [`write_file`] call in this
/// process a temp filename distinct from every other *concurrent* call,
/// on top of the pid already distinguishing this process from any other
/// racing on the same file. `Ordering::Relaxed` is enough — this needs
/// distinct values, not a synchronization point with any other memory
/// access.
///
/// The fixed temp name this replaced was a real race: two writers
/// sharing one temp filename could interleave — one call's
/// `File::create` truncating the other's in-progress write, or one
/// call's `rename` consuming the other's temp file out from under it
/// (an `ENOENT` surfaced as a spurious warning even though the first
/// writer's data was fine). A unique name per call removes that
/// interleaving entirely: each writer only ever touches its own file
/// until its own `rename`.
///
/// The residual case stays honest rather than pretended away: two
/// writers can still race the final `rename` itself (both succeed —
/// `rename` is atomic per call — but whichever finishes second wins,
/// since both target the same path). Every candidate `text` reaching
/// here is a valid, self-consistent document, so the worst outcome is a
/// slightly stale-but-valid file, never a torn one.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically write `text` to `path`: a unique temp file in the *same*
/// directory (rename is only atomic within a filesystem), `fsync`, then
/// rename over `path` — a crash or a concurrent reader never observes a
/// partial write. Creates the parent directory if it does not exist yet.
///
/// `pub(crate)` rather than private because `session::write_atomic`
/// writes `session.toml`, which is not a *layered* config document — it
/// is per-machine session state, deliberately excluded from
/// `reload::scan` — so it cannot come through [`write`]'s
/// layer-and-doc-name door, but must still be the same one atomic write.
///
/// This is real, potentially-blocking file I/O. Callers driven by UI
/// events must run it on a background executor, never inline on the
/// render thread (spec §7: nothing may stall the render thread).
pub(crate) fn write_file(path: &Path, text: &str) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;

    let pid = std::process::id();
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = dir.join(tmp_file_name(path, pid, counter));
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

/// The temp file's name for one atomic write to `path` (M9, 3b final
/// review): derived from `path`'s own file name rather than hardcoded,
/// because this one writer now stages `app.toml`, `groupings.toml`,
/// `scopes.toml`, `keymap.toml` and `session.toml`, and a name naming
/// the wrong file lies to anyone who finds one after a crash. Kept as a
/// separate pure function so the naming can be tested without touching a
/// filesystem.
fn tmp_file_name(path: &Path, pid: u32, counter: u64) -> String {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("geode-write");
    format!(".{file_name}.{pid}-{counter}.tmp")
}

#[cfg(test)]
mod tmp_file_name_tests {
    use super::*;

    #[test]
    fn the_temp_name_derives_from_the_target_file_not_a_hardcoded_app_toml() {
        // M9: this writer stages more than app.toml
        // (groupings.toml via frame's slot save, keymap.toml, session.toml),
        // so a name hardcoded to `.app.toml.*` lied about what it staged.
        assert_eq!(
            tmp_file_name(Path::new("/x/groupings.toml"), 7, 3),
            ".groupings.toml.7-3.tmp"
        );
        assert_eq!(
            tmp_file_name(Path::new("/x/app.toml"), 7, 4),
            ".app.toml.7-4.tmp"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::config::Layer;

    #[test]
    fn write_is_atomic_and_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("nested");
        write(&sub, Layer::User, "app", "config_version = 1\n").expect("write");
        assert_eq!(
            std::fs::read_to_string(sub.join("app.toml")).unwrap(),
            "config_version = 1\n"
        );
        // No temp file survives a successful write.
        let strays: Vec<_> = std::fs::read_dir(&sub)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy() != "app.toml")
            .collect();
        assert!(strays.is_empty(), "temp files must not survive: {strays:?}");
    }

    /// The whole point of `edit`: a user's comments and unrelated keys are
    /// theirs, and a keyed persist must not eat them.
    #[test]
    fn edit_preserves_comments_and_unrelated_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.toml"),
            "# mine\nconfig_version = 1\n\n[ui]\nfont_size = \"small\"\n\n[theme]\nname = \"Ayu Dark\"\n",
        ).unwrap();
        edit(dir.path(), Layer::User, "app", |doc| {
            doc["theme"]["name"] = toml_edit::value("Bloomberg");
        })
        .expect("edit");
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("# mine"), "{text}");
        assert!(text.contains(r#"font_size = "small""#), "{text}");
        assert!(text.contains(r#"name = "Bloomberg""#), "{text}");
    }

    /// A file the user hand-edited into a broken state is theirs. Refuse,
    /// leave it byte-for-byte, and say so — the contract
    /// `fontsize::persist_to_user_config` already keeps.
    #[test]
    fn edit_refuses_an_unparseable_file_without_touching_it() {
        let dir = tempfile::tempdir().unwrap();
        let broken = "config_version = = 1\n";
        std::fs::write(dir.path().join("app.toml"), broken).unwrap();
        let err = edit(dir.path(), Layer::User, "app", |doc| {
            doc["theme"]["name"] = toml_edit::value("x");
        })
        .expect_err("must refuse");
        assert!(err.contains("app.toml"), "the error names the file: {err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("app.toml")).unwrap(),
            broken,
            "the file must be untouched"
        );
    }

    /// Only the user layer is writable. Desk and builtin are refused rather
    /// than attempted, so a bug cannot write a shared desk file.
    #[test]
    fn only_the_user_layer_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        for layer in [Layer::Builtin, Layer::Desk] {
            assert!(
                write(dir.path(), layer, "app", "x = 1\n").is_err(),
                "{layer:?}"
            );
            assert!(edit(dir.path(), layer, "app", |_| {}).is_err(), "{layer:?}");
        }
        assert!(!dir.path().join("app.toml").exists(), "nothing was written");
    }

    /// `edit` on a doc that does not exist yet creates it — every keyed
    /// persist starts from no file on a fresh install.
    #[test]
    fn edit_creates_a_missing_doc() {
        let dir = tempfile::tempdir().unwrap();
        edit(dir.path(), Layer::User, "app", |doc| {
            doc["ui"]["font_size"] = toml_edit::value("large");
        })
        .expect("edit");
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("large"), "{text}");
    }
}
