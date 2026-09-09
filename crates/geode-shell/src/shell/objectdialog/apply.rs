//! Applying a field edit **instantly**, in memory, and persisting it in
//! the background (spec §7.1).
//!
//! ## Why there is no save key
//!
//! The first build of these dialogs staged every field edit into a
//! [`Draft`](super::Draft) and wrote nothing until `s`. That made the
//! round trip *disk-shaped*: to see a value change, the app wrote a file,
//! the 500 ms mtime watcher noticed it, and the loader read every layer
//! back off disk to rebuild the same `Config` the app could have built
//! from the documents it was already holding. A trader changing a config
//! field should see it change; instead they pressed a key, waited half a
//! second, and hoped.
//!
//! So an edit now moves memory first. [`commit_edit`] writes the changed
//! object into the in-memory user-layer `LayerDoc`, re-merges through
//! `Config::from_docs` — **the loader's own merge, the only one that
//! exists** — and hands the result to the same
//! `hot_reload::apply_reload` the watcher hands its own reloads to. The
//! only thing that differs between a watcher reload and a dialog edit is
//! where the documents came from; the merge and the application are
//! byte-identical code paths, so a dialog cannot apply a change the
//! watcher would have applied differently.
//!
//! Merging is not free and is not assumed to be: `cargo bench -p
//! geode-core --bench config_merge` measures `all_docs` + `from_docs` on
//! the largest config this repo ships (the demo desk: a 7.8 KB
//! `datasets.toml`, a 6.8 KB `views.toml`, seven documents across two
//! layers) at **63 µs**, which is 0.8% of PHILOSOPHY's 8 ms pure-UI
//! budget. A merge per keystroke is affordable; that is a measurement,
//! not a hope.
//!
//! ## The file write is background, debounced, and feeds nothing back
//!
//! Memory and disk both derive from the same rendered
//! `toml_edit::Table` — [`object_value`] parses exactly the text
//! [`super::object_text`] would write — so the write is a *copy* of the
//! decision, never its source. Nothing about the write's completion
//! updates memory. Its **failure** does: see [`flush`].
//!
//! Writes coalesce on a [`WRITE_DEBOUNCE`] timer rather than firing per
//! keystroke, because a held `shift+j` repeats at the OS key-repeat rate
//! (~100 ms on macOS defaults) and each repeat is a real edit. The
//! debounce is on the *write only* — memory is always current.
//!
//! ## The watcher will see our own write
//!
//! It will, and the resulting reload is a **no-op by construction**,
//! which is why nothing here suppresses it. `apply_reload` decides what a
//! reload changes by comparing the freshly loaded layered documents
//! against the ones already in `services.config` (`docs_equal`). The file
//! on disk is `config_write::edit`'s read-modify-write of the same object
//! value memory already holds, and a fresh user-layer document memory
//! creates carries the same `config_version` stamp `edit` puts at the top
//! of a file it creates — so every `changed(..)` predicate answers false,
//! no `ConfigReloaded` is emitted, no tile requeries, no palette closes,
//! and the frame is never touched. The reload assigns an identical
//! `Config` and repaints. Suppressing it would mean keeping a
//! "self-write" ledger that has to be right about every path a write can
//! take (including the ones that fail after the ledger entry is made) to
//! avoid missing a real external edit; proving the reload inert costs
//! nothing and cannot go stale.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use geode_core::config::{CONFIG_VERSION, Config, Layer, LayerDoc};
use gpui::Context;

use super::{Destination, Domain};
use crate::config_write;
use crate::shell::ShellView;

/// How long a config write waits for the next keystroke before going to
/// disk.
///
/// 250 ms, chosen against two clocks and one budget:
///
/// * it is more than twice macOS's default key-repeat period (~100 ms),
///   so a held `shift+j` reordering a column produces **one** write
///   rather than one per repeat — the thrash this exists to prevent;
/// * it is half the watcher's own 500 ms poll
///   (`hot_reload::RELOAD_POLL_INTERVAL`), so a flush and the poll that
///   observes it never interleave with a second flush;
/// * it delays only the *file*. Memory applied on the keystroke, so
///   nothing a trader can see waits for it.
///
/// Write-on-field-commit was the alternative and was rejected: the Views
/// stage has no commit moment — `space` and `shift+j` act on a row and
/// the cursor may never leave it — so "commit" would mean "when you
/// close the dialog", which is a quarter of an hour of applied-looking
/// edits that a crash loses. A quarter of a second is the whole exposure
/// here.
pub(crate) const WRITE_DEBOUNCE: Duration = Duration::from_millis(250);

/// One config document's user-layer copy of one object, as an edit leaves
/// it: `Some(value)` to set it, `None` to remove it entirely.
///
/// `None` is not an optimisation. A presentation table that matches the
/// view's own doc in every respect renders **empty** (`views::
/// presentation_table` omits an `order` equal to the doc's, an empty
/// `hidden`, and every width the doc already declares), and writing an
/// empty table would leave `[tree]` alone in `view_presentation.toml` —
/// a table that says nothing, which is exactly the artefact seen in a
/// user's file before this design. Under the old staging model that
/// needed a save whose draft excluded nothing; under this one it is one
/// keystroke away, every time a trader unhides the last hidden column.
/// So the empty rendering means "I have no personalisation of this
/// object" and is written as an absence, in memory and on disk alike.
type ObjectEdit = Option<toml::Value>;

/// Config writes applied to memory and not yet on disk.
///
/// Keyed by `(doc, object)` rather than by doc alone: the debounce can
/// span a trader leaving one object and editing another, and two objects
/// in the same file are two independent edits that must both land.
pub(crate) struct PendingConfigWrite {
    /// Which scheduled flush owns this batch. A flush task that finds a
    /// different sequence has been superseded by a later keystroke and
    /// does nothing — the later task carries the whole batch.
    seq: u64,
    user_dir: PathBuf,
    edits: BTreeMap<(&'static str, String), ObjectEdit>,
    /// Every layered document as it stood **before** this batch began —
    /// what memory is restored from if the write fails. Captured once per
    /// batch, so a failure reverts the batch, not the last keystroke of
    /// it.
    revert: Vec<LayerDoc>,
}

/// The user-layer table `object` should carry in its document, or `None`
/// when the rendered table is empty (see [`ObjectEdit`]).
///
/// Goes through [`super::object_text`] — the exact text the file write
/// produces — and parses it back, rather than converting
/// `toml_edit::Table` to `toml::Value` field by field. One rendering,
/// two destinations: memory cannot end up holding something the file
/// would not have said. It is a parse of a few hundred bytes, inside a
/// 63 µs merge, inside an 8 ms budget.
pub(super) fn object_value(object: &str, table: toml_edit::Table) -> ObjectEdit {
    if table.is_empty() {
        return None;
    }
    let text = super::object_text(object, table);
    let mut parsed: toml::Table = text.parse().ok()?;
    parsed.remove(object)
}

/// `docs` with `object`'s entry in the **user layer's** copy of `doc` set
/// to `value` (or removed when it is `None`).
///
/// A user-layer document that does not exist yet is created, carrying the
/// same `config_version` stamp `config_write::edit` writes at the top of
/// a file it creates — without which the document memory holds and the
/// document the watcher reads back a moment later would differ, and the
/// self-write reload this module's header proves inert would stop being
/// inert.
///
/// Appended at the end rather than inserted beside its siblings:
/// `Config::from_docs` groups by name preserving order, and the user
/// layer is last in every group's merge order anyway.
pub(super) fn docs_with_object(
    mut docs: Vec<LayerDoc>,
    user_dir: &Path,
    doc: &str,
    object: &str,
    value: ObjectEdit,
) -> Vec<LayerDoc> {
    if let Some(existing) = docs
        .iter_mut()
        .find(|d| d.layer == Layer::User && d.name == doc)
    {
        match value {
            Some(value) => {
                existing.table.insert(object.to_string(), value);
            }
            None => {
                existing.table.remove(object);
            }
        }
        return docs;
    }
    let Some(value) = value else {
        // Nothing of this object's is in the user layer and nothing is
        // being put there: creating a file to say so would be the empty
        // table one directory up.
        return docs;
    };
    let mut table = toml::Table::new();
    table.insert(
        "config_version".to_string(),
        toml::Value::Integer(CONFIG_VERSION),
    );
    table.insert(object.to_string(), value);
    docs.push(LayerDoc {
        layer: Layer::User,
        name: doc.to_string(),
        file: user_dir.join(format!("{doc}.toml")),
        table,
    });
    docs
}

/// Everything one keystroke changed, as the per-document object edits it
/// implies — the one place the draft's grouping-by-destination is turned
/// into writes, shared by the in-memory apply and the file write so they
/// cannot describe different changes.
fn edits_for(shell: &ShellView) -> BTreeMap<(&'static str, String), ObjectEdit> {
    let mut out = BTreeMap::new();
    let Some(state) = shell.object_dialog.as_ref() else {
        return out;
    };
    let Some(draft) = state.draft.as_ref() else {
        return out;
    };
    for dest in draft.writes_by_destination().keys() {
        let table = state.domain.to_table(draft, *dest);
        out.insert(
            (dest.doc(state.domain), draft.name.clone()),
            object_value(&draft.name, table),
        );
    }
    out
}

/// Would applying the draft as it stands fork the object — write a
/// [`Destination::Doc`] change into the user layer for an object the user
/// layer does not already own?
///
/// The one question this design still asks before acting, because a fork
/// *freezes*: a user-layer `views.toml` copy of a desk view stops
/// receiving the column the desk adds next week (spec §4.1). Everything
/// else — order, inclusion, width — is presentation, forks nothing, and
/// applies on the keystroke.
pub(super) fn would_fork(shell: &ShellView, domain: Domain) -> bool {
    let Some(draft) = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
    else {
        return false;
    };
    if !draft
        .writes_by_destination()
        .contains_key(&Destination::Doc)
    {
        return false;
    }
    domain
        .objects(&shell.services.config)
        .into_iter()
        .find(|row| row.name == draft.name)
        .is_some_and(|row| row.layer != Layer::User)
}

/// Apply the draft's changes **now**: into memory through the loader's
/// own merge and the shell's one applier, and onto the debounced write
/// queue for the disk.
///
/// Returns the notice the caller should show, or `None` when nothing
/// changed. A missing user directory is a notice and no write at all —
/// the contract every persist path in this crate shares — and no
/// in-memory apply either, because memory ahead of a disk that can never
/// catch up is precisely the state hazard 1 exists to prevent.
pub(super) fn commit_edit(shell: &mut ShellView, cx: &mut Context<ShellView>) -> Option<String> {
    let edits = edits_for(shell);
    if edits.is_empty() {
        return None;
    }
    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was changed".to_string());
    };

    // The draft's baseline moves here, not when the file lands: the
    // baseline is "what has been applied", and memory has applied it.
    if let Some(draft) = shell
        .object_dialog
        .as_mut()
        .and_then(|state| state.draft.as_mut())
    {
        draft.mark_saved();
    }

    // Captured before the first edit of a batch, so a failed write
    // restores the state the batch started from.
    let revert = match shell.pending_config_write.as_ref() {
        Some(pending) => pending.revert.clone(),
        None => shell.services.config.all_docs(),
    };

    apply_in_memory(shell, &user_dir, &edits, cx);
    schedule_flush(shell, user_dir, edits, revert, cx);
    None
}

/// Re-merge the documents with `edits` folded in, and hand the result to
/// the same applier the watcher uses.
///
/// The previous config's diagnostics travel with it: nothing was re-read,
/// so nothing new was learned about the files, and dropping them would
/// clear a "config: 2 errors — keeping last good" status the very next
/// watcher tick puts back.
fn apply_in_memory(
    shell: &mut ShellView,
    user_dir: &Path,
    edits: &BTreeMap<(&'static str, String), ObjectEdit>,
    cx: &mut Context<ShellView>,
) {
    let mut docs = shell.services.config.all_docs();
    for ((doc, object), value) in edits {
        docs = docs_with_object(docs, user_dir, doc, object, value.clone());
    }
    let carried = shell.services.config.diagnostics.clone();
    let mut config = Config::from_docs(docs);
    config.diagnostics = carried;
    shell.apply_reload(config, cx);
}

/// Fold `edits` into the pending batch and schedule the flush that will
/// write it.
///
/// Every keystroke bumps the sequence and spawns a fresh timer; whichever
/// task wakes holding the current sequence owns the whole accumulated
/// batch, and every superseded task finds a newer sequence and returns.
/// That is the coalescing: N keystrokes inside the debounce window
/// produce N timers and one write per touched document.
fn schedule_flush(
    shell: &mut ShellView,
    user_dir: PathBuf,
    edits: BTreeMap<(&'static str, String), ObjectEdit>,
    revert: Vec<LayerDoc>,
    cx: &mut Context<ShellView>,
) {
    let seq = shell.config_write_seq.wrapping_add(1);
    shell.config_write_seq = seq;
    let pending = shell
        .pending_config_write
        .get_or_insert_with(|| PendingConfigWrite {
            seq,
            user_dir,
            edits: BTreeMap::new(),
            revert,
        });
    pending.seq = seq;
    pending.edits.extend(edits);

    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(WRITE_DEBOUNCE).await;
        let Ok(Some((user_dir, edits))) = this.update(cx, |shell, _| take_flush(shell, seq)) else {
            return;
        };
        let outcome = cx
            .background_executor()
            .spawn(async move { run_writes(&user_dir, edits) })
            .await;
        if let Err(message) = outcome {
            this.update(cx, |shell, cx| revert_failed_write(shell, message, cx))
                .ok();
        } else {
            this.update(cx, |shell, _| shell.pending_config_write = None)
                .ok();
        }
    })
    .detach();
}

/// The batch a flush of `seq` owns, or `None` when a later keystroke has
/// taken it over. The batch stays on `ShellView` (rather than being taken
/// here) so a failure still has its `revert` documents to restore from.
#[allow(clippy::type_complexity)]
fn take_flush(
    shell: &mut ShellView,
    seq: u64,
) -> Option<(PathBuf, BTreeMap<(&'static str, String), ObjectEdit>)> {
    let pending = shell.pending_config_write.as_ref()?;
    if pending.seq != seq {
        return None;
    }
    Some((pending.user_dir.clone(), pending.edits.clone()))
}

/// The file half, off the render thread: one `config_write::edit` per
/// document, setting or removing each object it holds an edit for.
///
/// `edit`, never `write`: the read-modify-write through `toml_edit` keeps
/// the user's comments and every other object in the file, and refuses a
/// file it cannot parse instead of replacing it.
fn run_writes(
    user_dir: &Path,
    edits: BTreeMap<(&'static str, String), ObjectEdit>,
) -> Result<(), String> {
    let mut by_doc: BTreeMap<&'static str, Vec<(String, ObjectEdit)>> = BTreeMap::new();
    for ((doc, object), value) in edits {
        by_doc.entry(doc).or_default().push((object, value));
    }
    let mut failures = Vec::new();
    for (doc, objects) in by_doc {
        let result = config_write::edit(user_dir, Layer::User, doc, |document| {
            for (object, value) in objects {
                match value {
                    Some(value) => {
                        super::set_object(document, &object, to_edit_table(&value));
                    }
                    None => {
                        document.remove(&object);
                    }
                }
            }
        });
        if let Err(e) = result {
            failures.push(e);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// One object's value as the table a document write inserts. A non-table
/// value cannot occur — [`object_value`] produced it by parsing back a
/// rendered `toml_edit::Table` — and an empty table would have been
/// `None`, so the fallback is unreachable rather than lossy.
fn to_edit_table(value: &toml::Value) -> toml_edit::Table {
    match value.as_table() {
        Some(table) => super::toml_table_to_edit(table),
        None => toml_edit::Table::new(),
    }
}

/// **Hazard 1.** The write failed, so memory is ahead of disk — a trader
/// is looking at a value that is not persisted. Put memory back where the
/// batch started, through the same applier, and say so.
///
/// The whole batch reverts, not the failing document alone: `revert` is
/// one coherent set of documents, and reverting half of a two-file edit
/// would leave a view's presentation describing columns the view no
/// longer lists. If one document of a batch did land before another
/// failed, the 500 ms watcher reconciles memory back toward whatever is
/// actually on disk — disk stays the arbiter, which is the property that
/// makes the revert safe to be approximate.
fn revert_failed_write(shell: &mut ShellView, message: String, cx: &mut Context<ShellView>) {
    eprintln!("[config] warning: {message}");
    let Some(pending) = shell.pending_config_write.take() else {
        return;
    };
    let carried = shell.services.config.diagnostics.clone();
    // Named apart from the apply path's own `config` on purpose: these
    // two `apply_reload` calls are the only ones in this module, they
    // differ only in which documents they carry, and a mutation entry
    // that anchors on one must not silently land on the other.
    let mut restored = Config::from_docs(pending.revert);
    restored.diagnostics = carried;
    shell.apply_reload(restored, cx);
    // The draft is the edit buffer the reverted value has to show through,
    // so it is rebuilt from the config that just went back — otherwise the
    // row keeps painting the value the file refused.
    if let Some(state) = shell.object_dialog.as_mut()
        && let Some(draft) = state.draft.as_ref()
    {
        let selected = draft.selected;
        let name = draft.name.clone();
        let mut rebuilt = state.domain.draft(&shell.services.config, &name);
        rebuilt.selected = selected;
        state.draft = Some(rebuilt);
        state.notice = Some(format!("could not save — change reverted ({message})"));
    }
    cx.notify();
}
