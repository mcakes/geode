//! Configuration edits update the dialog draft immediately and reach the active
//! configuration and user files after a shared debounce.
//!
//! `commit_edit` records whole-object edits by document and object name. The latest
//! timer promotes the accumulated batch through `Config::from_docs` and
//! `hot_reload::apply_reload`, then submits file writes through `config_write`. Memory
//! and disk use the same rendered object values. Creation and confirmed removal join
//! the batch with zero delay; ordinary field edits wait 250 ms after the latest edit.
//! Closing the dialog does not cancel a queued batch.
//!
//! The merge and file writes have separate outcomes: a rejected merge still writes the
//! files and reports that memory kept its previous configuration. A current write
//! failure attempts to restore the batch's initial documents and rebuilds an open
//! draft. Completion sequence checks prevent an older success or failure from clearing
//! a newer batch or replacing its status. Files are written one at a time, so a batch
//! can partially persist; restoration does not undo successful file writes. The watcher
//! can subsequently reload that partial disk state.
//!
//! The watcher also observes successful dialog writes. Equal layered documents avoid
//! the view-specific reload event, palette closure, and changed-document rebuilds, but
//! every accepted reload still advances the frame's config revision and republishes
//! chords. Removing an object from a nonexistent user document creates no in-memory
//! document, while the file writer creates a version-only file; this difference can
//! trigger additional changed-document work.
//!
//! There is no shutdown flush for the debounce batch. Exiting before promotion loses
//! all pending edits, potentially spanning several objects or files. An in-flight write
//! is also best effort at process exit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use geode_core::config::{CONFIG_VERSION, Config, Layer, LayerDoc, Severity};
use gpui::Context;

use super::{Destination, Domain, Stage};
use crate::config_write;
use crate::shell::ShellView;

/// Quiet period after the latest field edit before merging, applying, and writing the
/// pending batch. The draft itself already shows each change. Restarting this timer
/// coalesces repeated steps and reorders into one application and one write per touched
/// document. Creation and confirmed removal use zero delay.
pub(crate) const WRITE_DEBOUNCE: Duration = Duration::from_millis(250);

/// Status for a successful disk write whose in-memory reload was rejected. Merge
/// acceptance and file-write success are independent outcomes.
pub(crate) const REJECTED_STATUS: &str = "saved to disk · rejected by the merge";

/// Set one user-layer object with `Some(value)`, or remove it with `None`. Only empty
/// presentation overlays render as removals: their absence means inherit presentation.
/// An empty definition must not remove the user key, because that would restore an
/// inherited object instead of keeping it empty.
pub type ObjectEdit = Option<toml::Value>;

/// The result of rendering one object. Removal inherits the layer below; `Nothing`
/// leaves the existing user-layer entry unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjectWrite {
    /// Set the user-layer key to this value.
    Set(toml::Value),
    /// Remove the user-layer key. Only empty `Presentation` or `DatasetPresentation`
    /// overlays render this result.
    Remove,
    /// Touch nothing: neither memory nor disk gains or loses a key, and
    /// the object keeps whatever it already had.
    Nothing,
}

/// Edits recorded by keystrokes and not yet merged, applied or written —
/// everything the next [`promote`] owes the rest of the app.
///
/// Each entry is the whole rendered object as of the last keystroke or
/// confirmed removal that touched it, not a delta, so promoting the
/// batch twice is idempotent and a superseded flush costs nothing.
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
    /// The object-dialog domains whose drafts contributed edits to this batch. A
    /// failed write reverts memory for the whole batch, so exactly these drafts are
    /// rebuilt; any other open dialog's draft had nothing in it and is left alone.
    origins: Vec<Domain>,
}

/// Render and parse the exact object text used for persistence, so memory and disk
/// receive the same value.
///
/// An empty presentation overlay means no personalisation and removes the user entry.
/// An empty definition returns `Nothing`: removing it would inherit an object the draft
/// intended to empty. Unparseable renderings also return `Nothing`, never a destructive
/// removal. List editing refuses a final untick where the definition cannot represent
/// an empty list.
pub fn object_value(object: &str, item: toml_edit::Item, dest: Destination) -> ObjectWrite {
    if item_is_empty(&item) {
        return match dest {
            // An absent overlay inherits presentation without removing the definition.
            Destination::Presentation | Destination::DatasetPresentation => ObjectWrite::Remove,
            Destination::Doc => ObjectWrite::Nothing,
        };
    }
    let text = super::object_text(object, item);
    let parsed: Option<toml::Value> = text
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut parsed| parsed.remove(object));
    // An invalid rendering must not become an instruction to delete the object.
    match parsed {
        Some(value) => ObjectWrite::Set(value),
        None => ObjectWrite::Nothing,
    }
}

/// Whether the item is absent or an empty table, array, or array of tables. Scalars are
/// nonempty. `object_value` interprets emptiness by destination.
fn item_is_empty(item: &toml_edit::Item) -> bool {
    match item {
        toml_edit::Item::Table(t) => t.is_empty(),
        toml_edit::Item::ArrayOfTables(a) => a.is_empty(),
        toml_edit::Item::Value(v) => v.as_array().is_some_and(|a| a.is_empty()),
        toml_edit::Item::None => true,
    }
}

/// Set or remove an object in the user-layer document. New documents receive the same
/// version stamp as `config_write::edit`, keeping their layered values equal when the
/// watcher reads the resulting file.
///
/// A removal does not create a missing document. New user documents are appended;
/// `Config::from_docs` preserves each document's layer order while grouping names.
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
        let item = state.domain.to_table(draft, *dest);
        let key = (dest.doc(state.domain), draft.name.clone());
        // The three answers are kept apart here rather than flattened
        // into an `Option`: `Nothing` must not become the `None` the
        // batch spells "remove this object's key", which for a
        // `Destination::Doc` document is a revert (see [`object_value`]).
        match object_value(&draft.name, item, *dest) {
            ObjectWrite::Set(value) => {
                out.insert(key, Some(value));
            }
            ObjectWrite::Remove => {
                out.insert(key, None);
            }
            ObjectWrite::Nothing => {}
        }
    }
    out
}

/// Whether a definition edit would copy an inherited object into the user layer.
/// Whole-object replacement stops inheriting later changes to that definition;
/// presentation-only edits do not fork it. Unconfigured grouping slots have no
/// inherited object to fork.
///
/// Reads the active config, which may trail the pending batch by the debounce interval.
/// Multiple definition edits within that interval can therefore repeat the fork notice
/// and sidecar entry.
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
        .is_some_and(|row| row.layer.is_some_and(|layer| layer != Layer::User))
}

/// Announce the copy, its shadowed layer, and the revert verb. Read before
/// `commit_edit` advances the draft baseline and empties its pending differences.
pub(super) fn fork_notice(shell: &ShellView, domain: Domain) -> String {
    let Some(name) = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .map(|draft| draft.name.clone())
    else {
        return String::new();
    };
    match super::shadow_of(&shell.services.config, domain.doc(), &name) {
        Some((layer, _)) => format!(
            "copied '{name}' to your config — r restores the {} copy",
            layer.name()
        ),
        None => format!("copied '{name}' to your config — r restores it"),
    }
}

/// The first error-severity draft diagnostic, formatted as a refusal notice. Errors
/// block value edits before they can enter the batch; warnings remain editable. This
/// gate does not apply to removals that can clear invalid objects.
pub(super) fn blocking_diagnostic(shell: &ShellView) -> Option<String> {
    let diagnostic = shell
        .object_dialog
        .as_ref()?
        .draft
        .as_ref()?
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)?;
    Some(format!("not saved — {}", diagnostic.message))
}

/// Queue the draft's rendered changes without merging or applying them yet. The dialog
/// already paints the draft; `promote` updates active configuration after the debounce.
/// Returns a refusal notice or `None`.
///
/// Without a writable user directory, nothing is queued and the draft remains dirty.
/// Its baseline advances only after validation and directory checks pass.
pub(super) fn commit_edit(shell: &mut ShellView, cx: &mut Context<ShellView>) -> Option<String> {
    // Gate every value-edit caller, including overwrite, at the queue boundary. The
    // renderer also checks early so it does not announce an unsaveable fork. Removals
    // use `commit_removal`: an invalid object must remain removable.
    if let Some(notice) = blocking_diagnostic(shell) {
        return Some(notice);
    }
    let mut edits = edits_for(shell);
    if edits.is_empty() {
        return None;
    }
    // Record the inherited definition in the same batch as the fork. Clear stale
    // sidecar keys first: the new fork's key can itself appear stale before its
    // definition reaches the user layer, so the fresh entry must win.
    let domain = shell.object_dialog.as_ref().map(|s| s.domain);
    if let Some(domain) = domain
        && would_fork(shell, domain)
        && let Some(draft) = shell.object_dialog.as_ref().and_then(|s| s.draft.as_ref())
        && let Some((layer, value)) =
            super::shadow_of(&shell.services.config, domain.doc(), &draft.name)
    {
        let key = super::override_key(domain.doc(), &draft.name);
        for stale in super::stale_override_keys(&shell.services.config) {
            edits.insert((super::OVERRIDES_DOC, stale), None);
        }
        edits.insert(
            (super::OVERRIDES_DOC, key),
            Some(super::override_entry(layer, &draft.name, &value)),
        );
    }

    // Do not advance the baseline until there is somewhere to persist the edit. An
    // unqueued change must remain dirty for a later attempt.
    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was changed".to_string());
    };

    // The draft's baseline moves here, not when the flush lands: the
    // baseline is "what this keystroke has already accounted for", so the
    // next keystroke's difference is that keystroke's alone. The batch
    // below carries the whole rendered object either way, so a flush is
    // idempotent over it.
    if let Some(draft) = shell
        .object_dialog
        .as_mut()
        .and_then(|state| state.draft.as_mut())
    {
        draft.mark_saved();
    }

    queue_batch(shell, edits, user_dir, WRITE_DEBOUNCE, domain, cx);
    None
}

/// Queue confirmed removals with zero delay, joining any pending field edits. The
/// keys-only input cannot carry a value past the edit validation gate.
///
/// Removals bypass draft errors because deleting or reverting the invalid object may
/// resolve those errors. They still use the shared asynchronous flush so a pending edit
/// on another object is included rather than raced.
pub(super) fn commit_removal(
    shell: &mut ShellView,
    keys: impl IntoIterator<Item = (&'static str, String)>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    let edits: BTreeMap<(&'static str, String), ObjectEdit> =
        keys.into_iter().map(|key| (key, None)).collect();
    if edits.is_empty() {
        return None;
    }
    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was removed".to_string());
    };
    let origin = shell.object_dialog.as_ref().map(|s| s.domain);
    queue_batch(shell, edits, user_dir, Duration::ZERO, origin, cx);
    None
}

/// Queue a newly named definition after validation, with zero delay. Render only
/// `Destination::Doc`; a new draft's empty baselines would otherwise also report
/// untouched presentation fields as changes. Refuse empty definitions and missing user
/// directories before advancing the draft baseline.
pub(crate) fn commit_create(shell: &mut ShellView, cx: &mut Context<ShellView>) -> Option<String> {
    if let Some(notice) = blocking_diagnostic(shell) {
        return Some(notice);
    }
    let state = shell.object_dialog.as_ref()?;
    let draft = state.draft.as_ref()?;
    let domain = state.domain;
    let item = domain.to_table(draft, Destination::Doc);
    let mut edits = BTreeMap::new();
    match object_value(&draft.name, item, Destination::Doc) {
        ObjectWrite::Set(value) => {
            edits.insert(
                (Destination::Doc.doc(domain), draft.name.clone()),
                Some(value),
            );
        }
        ObjectWrite::Remove | ObjectWrite::Nothing => {
            return Some("nothing to create — the object would be empty".to_string());
        }
    }
    let Some(user_dir) = shell.user_dir.clone() else {
        return Some("no writable user config directory — nothing was changed".to_string());
    };
    if let Some(draft) = shell.object_dialog.as_mut().and_then(|s| s.draft.as_mut()) {
        draft.mark_saved();
    }
    queue_batch(shell, edits, user_dir, Duration::ZERO, Some(domain), cx);
    None
}

/// Queue one whole object (`[name]` in `doc`'s user layer) with zero delay, for a surface
/// outside the object dialog that creates a definition. It joins any pending batch, so
/// an object-dialog edit in flight is written with it rather than raced. Refuses, with
/// nothing queued, when there is no writable user directory. No object dialog's draft
/// contributed, so a failed write rebuilds none.
pub(crate) fn queue_object(
    shell: &mut ShellView,
    doc: &'static str,
    name: &str,
    value: toml::Value,
    cx: &mut Context<ShellView>,
) -> Result<(), String> {
    let Some(user_dir) = shell.user_dir.clone() else {
        return Err("no writable user config directory — nothing was changed".to_string());
    };
    let edits = BTreeMap::from([((doc, name.to_string()), Some(value))]);
    queue_batch(shell, edits, user_dir, Duration::ZERO, None, cx);
    Ok(())
}

/// Capture the batch's initial documents and schedule its accumulated edits. Callers
/// resolve the writable directory before advancing any draft baseline. `origin` is the
/// object dialog whose draft produced the edits, if any.
fn queue_batch(
    shell: &mut ShellView,
    edits: BTreeMap<(&'static str, String), ObjectEdit>,
    user_dir: PathBuf,
    delay: Duration,
    origin: Option<Domain>,
    cx: &mut Context<ShellView>,
) {
    // Captured before the first edit of a batch, so a failed write
    // restores the state the batch started from.
    let revert = match shell.pending_config_write.as_ref() {
        Some(pending) => pending.revert.clone(),
        None => shell.services.config.all_docs(),
    };

    schedule_flush(shell, user_dir, edits, revert, delay, origin, cx);
}

/// Fold edits into the active layered documents, re-merge them, and use the same reload
/// applier as the watcher.
///
/// Do not carry prior file-reading diagnostics into `Config::from_docs`: they can
/// describe files that were skipped and contributed no documents. `apply_reload`
/// recomputes its own acceptance diagnostics from the merged documents. A later watcher
/// read can report unresolved disk-file errors again.
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
    let config = Config::from_docs(docs);
    shell.apply_reload(config, cx);
}

/// Active documents with the pending batch folded in, without applying or writing them.
/// `None` means no batch is pending.
///
/// Stage entry uses this snapshot so reopening an object during the debounce sees its
/// latest edits. A stale draft would otherwise outlive the flush and overwrite those
/// edits when it next renders the whole object.
pub(crate) fn config_with_pending(shell: &ShellView) -> Option<Config> {
    let pending = shell.pending_config_write.as_ref()?;
    let mut docs = shell.services.config.all_docs();
    for ((doc, object), value) in &pending.edits {
        docs = docs_with_object(docs, &pending.user_dir, doc, object, value.clone());
    }
    Some(Config::from_docs(docs))
}

/// Merge edits into the pending batch and schedule a sequence-tagged timer. Only the
/// latest sequence promotes the accumulated whole-object values; older timers return
/// without work. Field edits debounce, while creations and confirmed removals use zero
/// delay.
///
/// The batch remains available while writes run. Completion checks the sequence again
/// so a write cannot discard newer edits that arrived while it was in flight.
fn schedule_flush(
    shell: &mut ShellView,
    user_dir: PathBuf,
    edits: BTreeMap<(&'static str, String), ObjectEdit>,
    revert: Vec<LayerDoc>,
    delay: Duration,
    origin: Option<Domain>,
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
            origins: Vec::new(),
        });
    pending.seq = seq;
    pending.edits.extend(edits);
    if let Some(domain) = origin
        && !pending.origins.contains(&domain)
    {
        pending.origins.push(domain);
    }

    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(delay).await;
        let Ok(Some((user_dir, edits, rejected))) =
            this.update(cx, |shell, cx| promote(shell, seq, cx))
        else {
            return;
        };
        let outcome =
            config_write::submit(&user_dir.clone(), cx.background_executor(), move || {
                run_writes(&user_dir, edits)
            })
            .await;
        this.update(cx, |shell, cx| {
            finish_flush(shell, seq, outcome, rejected, cx)
        })
        .ok();
    })
    .detach();
}

/// Apply the current batch to memory and return its file writes plus the reload's
/// rejection count, if any. Superseded sequences return `None`.
///
/// Keep the batch on the shell while writing: failure needs its original documents, and
/// later edits need to extend it. Capture `last_reload` immediately after apply so the
/// completion reports this flush's memory outcome.
#[allow(clippy::type_complexity)]
fn promote(
    shell: &mut ShellView,
    seq: u64,
    cx: &mut Context<ShellView>,
) -> Option<(
    PathBuf,
    BTreeMap<(&'static str, String), ObjectEdit>,
    Option<usize>,
)> {
    let pending = shell.pending_config_write.as_ref()?;
    if pending.seq != seq {
        return None;
    }
    let user_dir = pending.user_dir.clone();
    let edits = pending.edits.clone();
    apply_in_memory(shell, &user_dir, &edits, cx);
    let rejected = match &shell.last_reload {
        crate::reload::ReloadOutcome::KeptLastGood { errors } => Some(errors.len()),
        _ => None,
    };
    Some((user_dir, edits, rejected))
}

/// Finish only the current write sequence. An older success or failure must not clear
/// newer edits, restore old documents, or replace the current status.
///
/// `rejected` records this flush's in-memory reload outcome. Successful writes report
/// that rejection separately; failed writes attempt restoration regardless of whether
/// the proposed merge was accepted.
pub(crate) fn finish_flush(
    shell: &mut ShellView,
    seq: u64,
    outcome: Result<(), String>,
    rejected: Option<usize>,
    cx: &mut Context<ShellView>,
) {
    // Completion callbacks can be scheduled after a newer edit or completion.
    // Neither an old failure nor an old success owns current memory/status.
    if shell.config_write_seq != seq {
        return;
    }
    match outcome {
        Err(message) => revert_failed_write(shell, message, cx),
        Ok(()) => {
            shell.pending_config_write = None;
            // The write succeeded; separately report whether memory accepted its merge.
            match rejected {
                Some(n) => {
                    shell.config_write_error =
                        Some(format!("{REJECTED_STATUS}: {n} error(s) — keeping last good").into());
                    cx.notify();
                }
                None => {
                    // A successful write and accepted merge clear the previous failure
                    // status.
                    if shell.config_write_error.take().is_some() {
                        cx.notify();
                    }
                }
            }
        }
    }
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
                        super::set_object(document, &object, super::toml_value_to_item(&value));
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

/// Attempt to restore the batch's initial documents through the reload applier and
/// rebuild every open draft that contributed to the batch after a current write failure.
///
/// Restoration covers the whole in-memory batch. Successful writes to other files are
/// not rolled back, and the watcher may subsequently load that partial disk state. This
/// is recovery from a write failure, not a transaction across files.
fn revert_failed_write(shell: &mut ShellView, message: String, cx: &mut Context<ShellView>) {
    tracing::warn!(target: "geode::config", "{message}");
    let Some(pending) = shell.pending_config_write.take() else {
        return;
    };
    // Re-derive diagnostics from the restored documents, as in `apply_in_memory`.
    let restored = Config::from_docs(pending.revert);
    shell.apply_reload(restored, cx);
    // Keep the failure visible even when the dialog that queued the write is closed.
    shell.config_write_error = Some(format!("config not saved — reverted: {message}").into());
    // Only the drafts that contributed to the batch show it reverted. A covered
    // dialog of another domain keeps its unsaved draft: nothing of it was in the
    // batch, and rebuilding it would throw away the trader's place.
    let config = &shell.services.config;
    for state in shell
        .object_dialog
        .iter_mut()
        .chain(crate::shell::dialog::parked_objects_mut(&mut shell.modals))
        .filter(|state| pending.origins.contains(&state.domain))
    {
        rebuild_after_revert(state, config, &message);
    }
    cx.notify();
}

/// Rebuild one dialog's draft from the reverted config and say why. The draft is the
/// edit buffer the reverted value has to show through; otherwise the row keeps
/// painting the value the file refused.
fn rebuild_after_revert(state: &mut super::ObjectDialogState, config: &Config, message: &str) {
    let Some(draft) = state.draft.as_ref() else {
        return;
    };
    // The ad hoc chain lives in the frame and never joins a batch, so a
    // failed write reverted nothing of it; rebuilding it from config would
    // turn it into a slot-shaped draft named `*`.
    if state.domain == super::Domain::Groupings && draft.name == super::grouping_list::AD_HOC {
        return;
    }
    let selected = draft.selected;
    let name = draft.name.clone();
    let mut rebuilt = state.domain.draft(config, &name);
    // The rebuilt draft has object fields, not a column projection. Return to the
    // object stage and select the column by name rather than reusing an index into
    // its seven presentation fields.
    match &state.stage {
        Stage::Column { object, column } => {
            let (object, column) = (object.clone(), column.clone());
            rebuilt.select_item_named(&column);
            state.stage = Stage::Edit { object };
        }
        // A rebuilt scope also has no Values projection. Return to its object stage
        // and reset the cursor to the first row.
        Stage::Values { object, .. } => {
            let object = object.clone();
            rebuilt.selected = 0;
            state.stage = Stage::Edit { object };
        }
        _ => rebuilt.selected = selected,
    }
    // Rebuilding can change which rows the restored index or column name
    // identifies. Settle the replacement draft here because failure recovery runs
    // outside the key path.
    rebuilt.settle_selection(state.domain);
    state.draft = Some(rebuilt);
    state.notice = Some(format!("could not save — change reverted ({message})"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An empty rendering means opposite things at the two
    /// destinations, and the Doc one is the dangerous half.** Removing a
    /// user-layer key in a domain's own doc means *inherit the layer
    /// beneath*, so collapsing an emptied object to an absence there
    /// silently restores the desk's copy of it — the trader asks for
    /// "this slot groups by nothing" and gets "revert to the desk's slot
    /// 3" while the edit stage keeps painting the chain they cleared.
    /// [`ObjectWrite::Nothing`] is the only safe answer: no write, no
    /// removal, nothing of theirs destroyed.
    ///
    /// Unreachable by keystroke — `Draft::step_selected` refuses the last
    /// untick — and pinned here anyway, because it is the semantics a
    /// future `Destination::Doc` adapter inherits.
    #[test]
    fn an_empty_doc_rendering_writes_nothing_rather_than_removing_the_key() {
        let item = toml_edit::Item::Value(toml_edit::Array::new().into());
        assert_eq!(
            object_value("3", item, Destination::Doc),
            ObjectWrite::Nothing
        );
    }

    /// The overlay half: `view_presentation.toml` is read *over* the
    /// object it names, so an empty rendering is "I have no
    /// personalisation of this object" ([`ObjectEdit`]'s own doc) and the
    /// key goes away rather than `[tree]` sitting alone in the file.
    #[test]
    fn an_empty_presentation_rendering_is_an_absence() {
        let item = toml_edit::Item::Table(toml_edit::Table::new());
        assert_eq!(
            object_value("tree", item, Destination::Presentation),
            ObjectWrite::Remove
        );
        // Empty arrays have the same overlay-removal semantics as empty tables.
        let array = toml_edit::Item::Value(toml_edit::Array::new().into());
        assert_eq!(
            object_value("tree", array, Destination::Presentation),
            ObjectWrite::Remove
        );
    }

    /// Grouping definitions use bare arrays. Their round trip must preserve that shape
    /// rather than falling back to an empty table.
    #[test]
    fn a_populated_array_item_round_trips_as_a_toml_array() {
        let mut array = toml_edit::Array::new();
        array.push("lhu");
        array.push("book");
        let item = toml_edit::Item::Value(array.into());
        let ObjectWrite::Set(value) = object_value("3", item, Destination::Doc) else {
            panic!("a populated array is a real write");
        };
        assert_eq!(
            value
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()),
            Some(vec!["lhu", "book"])
        );
    }
}
