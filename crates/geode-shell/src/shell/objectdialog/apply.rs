//! Applying a field edit without a save key: instant in the dialog,
//! merged and persisted on one short timer behind it (spec §7.1).
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
//! So an edit never travels through the disk to reach the screen.
//! [`commit_edit`] records the keystroke in one pending batch, and when
//! the [`WRITE_DEBOUNCE`] window closes [`promote`] writes the changed
//! object into the in-memory user-layer `LayerDoc`, re-merges through
//! `Config::from_docs` — **the loader's own merge, the only one that
//! exists** — and hands the result to the same
//! `hot_reload::apply_reload` the watcher hands its own reloads to. What
//! differs between a watcher reload and a dialog edit is where the
//! documents came from and when they are applied; the merge and the
//! application are byte-identical code paths, so a dialog cannot apply a
//! change the watcher would have applied differently.
//!
//! ## What is instant, and what rides the timer
//!
//! **The dialog is instant. The rest of the world catches up on one
//! timer.** A keystroke changes the [`Draft`](super::Draft), which is
//! what the edit stage paints, so the trader sees their change with
//! nothing in between. What it does *not* do is apply the merged config:
//! `apply_reload` emits `ShellEvent::ConfigReloaded`, the app bridge
//! turns that into fresh `ViewSpec`s, and every blotter tile requeries —
//! a §7.1 <50 ms operation at 1M rows. Doing that per keystroke means
//! doing it at the OS key-repeat rate under a held key.
//!
//! So the merge, the application and the file write all happen together
//! when the [`WRITE_DEBOUNCE`] window closes ([`promote`]). They are one
//! event — "the rest of the world catches up" — and they belong on one
//! timer. The blotter updating a beat after the dialog is correct, not a
//! compromise.
//!
//! Both halves are measured, not assumed (`docs/perf.md`, "Phase 4c";
//! `cargo bench -p geode-shell -- config_edit`). A **keystroke** costs
//! **37 µs** — toggle the row, revalidate, render the object, turn it
//! into the value memory and the file both take — which is 0.46% of
//! PHILOSOPHY's 8 ms pure-UI budget, and no merge appears in it. A
//! **flush** pays **70 µs** to merge a builtin layer including the real
//! keymap, plus **20 µs** for the `build_keymap` `apply_reload` runs
//! unconditionally, once per 250 ms. An earlier build merged and applied
//! per keystroke; the arithmetic was affordable and the behaviour was
//! not, which is why the numbers are split the way the work is.
//!
//! Memory and disk both derive from the same rendered
//! `toml_edit::Table` — [`object_value`] parses exactly the text
//! [`super::object_text`] would write — so the write is a *copy* of the
//! decision, never its source. Nothing about the write's completion
//! updates memory. Its **failure** does: see [`revert_failed_write`].
//!
//! One consequence, stated rather than hidden: a quit inside the
//! debounce window loses the pending batch **entirely** — not merely its
//! write. Nothing has been merged or applied yet either, so the edit is
//! gone rather than half-landed, which is the better of the two failures
//! but is still a loss. The exposure is 250 ms of one object's fields,
//! and closing it would mean either writing per keystroke (the thrash
//! this exists to prevent) or a shutdown hook this shell does not have.
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

use geode_core::config::{CONFIG_VERSION, Config, Layer, LayerDoc, Severity};
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
/// * it delays the merge, the application and the file **together** —
///   everything except the dialog itself, which paints from the
///   [`Draft`](super::Draft) and so has already moved. Nothing a trader
///   is looking at waits for this timer; the blotter and the rest of the
///   app do, deliberately (see the module header).
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
/// user's file before this design. Under the old staged-save model that
/// needed a save whose draft excluded nothing; under this one it is one
/// keystroke away, every time a trader unhides the last hidden column.
/// So the empty rendering means "I have no personalisation of this
/// object" and is written as an absence, in memory and on disk alike.
pub type ObjectEdit = Option<toml::Value>;

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
}

/// The user-layer table `object` should carry in its document, or `None`
/// when the rendered table is empty (see [`ObjectEdit`]).
///
/// Goes through [`super::object_text`] — the exact text the file write
/// produces — and parses it back, rather than converting
/// `toml_edit::Table` to `toml::Value` field by field. One rendering,
/// two destinations: memory cannot end up holding something the file
/// would not have said. It is a parse of a few hundred bytes, and it is
/// part of the keystroke rather than of the flush — measured inside that
/// keystroke's 37 µs, against an 8 ms budget.
pub fn object_value(object: &str, table: toml_edit::Table) -> ObjectEdit {
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
/// goes onto the batch unasked.
///
/// It reads `services.config`, which the debounce leaves up to 250 ms
/// behind, so two definitional edits inside one window each ask rather
/// than the second seeing the first already applied. That is the safe
/// direction — asking twice loses nothing — and it needs a `Choice`
/// stepped twice inside a quarter second to happen at all.
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

/// The open draft's first error-severity diagnostic, formatted as the
/// notice a refused commit shows — or `None` when nothing blocks it.
///
/// Spec §7.1's no-carry-forward rule: `reload::decide` rejects any
/// config holding an error diagnostic, so an edit `Domain::validate`
/// rated `Severity::Error` must never reach the batch — the merge would
/// be refused a flush later while the file write still fired, leaving
/// memory and disk disagreeing. `Severity::Warning` never matches: a
/// desk renaming a column produces a warning by design (a stale name is
/// meant to be ignorable, not fatal), and blocking on it would make a
/// personal file unsaveable through the dialog built to manage it.
///
/// `Severity`, not "has any diagnostics", is the sole discriminator —
/// see `an_edit_with_only_warnings_still_joins_the_batch` for the test
/// that pins the difference.
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

/// Record the draft's change and put it on the debounced queue.
///
/// The keystroke's whole job. It does **not** merge and does **not**
/// apply — [`promote`] does both when the window closes — because the
/// dialog already shows the change (it is painted from the draft) and
/// everything else the application would touch is expensive per
/// keystroke. See this module's header.
///
/// Returns the notice the caller should show, or `None` when nothing
/// changed. A missing user directory is a notice and **nothing queued** —
/// the contract every persist path in this crate shares — so no flush is
/// ever scheduled and the change is never applied: memory ahead of a
/// disk that can never catch up is precisely the state hazard 1 exists
/// to prevent. The draft keeps the value, and the row still paints it,
/// which is the honest picture of a shell with nowhere to write.
pub(super) fn commit_edit(shell: &mut ShellView, cx: &mut Context<ShellView>) -> Option<String> {
    // The actual gate: checked here, inside the one function every EDIT
    // reaches before it can queue a batch, rather than trusted to each
    // caller. `render::commit_or_confirm` also checks this early (see
    // `blocking_diagnostic`'s own doc) so a `Confirm::Fork` question is
    // never asked over an edit that can never be saved — but that early
    // check is a UX nicety, not the safety property. This one is: it
    // covers `run_confirmed`'s `Confirm::Fork` arm, which calls this
    // function directly, and any future caller, without depending on
    // anything about how keys are dispatched while a confirm is armed.
    //
    // A removal never reaches this function — see [`commit_removal`],
    // which joins the same batch through a deliberately ungated path.
    // An error-severity diagnostic on the object `d`/`r` is about to
    // remove describes exactly the state those verbs exist to escape;
    // a gate built to keep an unsaveable *value* off the batch must not
    // also refuse the one action that clears it.
    if let Some(notice) = blocking_diagnostic(shell) {
        return Some(notice);
    }
    let edits = edits_for(shell);
    if edits.is_empty() {
        return None;
    }

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

    queue_batch(
        shell,
        edits,
        WRITE_DEBOUNCE,
        "no writable user config directory — nothing was changed",
        cx,
    )
}

/// Record a confirmed `d`/`r`'s removal and put it on the exact batch a
/// field edit would join — the same [`PendingConfigWrite`], the same
/// [`promote`] → [`apply_in_memory`] → [`run_writes`] → [`finish_flush`],
/// so a delete or revert is no longer invisible until the 500 ms watcher
/// notices the write `spawn_removals` used to make on its own. `render::
/// removal_edits` builds `edits`; every value in it is `None` (see
/// [`ObjectEdit`]) — a removal never carries a value for anything to
/// reject.
///
/// **Deliberately does not call [`blocking_diagnostic`].** That gate
/// exists so an edit `Domain::validate` rated `Severity::Error` can never
/// join the batch, because `reload::decide` would refuse the merge a
/// flush later while the file write had already fired. A removal cannot
/// hit that: it does not write the value the reader would reject, it
/// erases the object carrying it. Gating a delete or revert on the very
/// diagnostic it would resolve would trap a trader in the one dialog
/// built to fix that state, refusing the only action that helps — see
/// this crate's Task 3 report for the fuller reasoning.
///
/// **No debounce, unlike [`commit_edit`].** [`WRITE_DEBOUNCE`] exists to
/// coalesce a keystroke *stream* — a held `shift+j`, a typed filter char
/// — into one write; a removal is a single already-confirmed act (armed
/// by `d`/`r`, answered by a second keystroke), so there is nothing to
/// coalesce and nothing gained by waiting on the clock. It still goes
/// through [`schedule_flush`] rather than applying inline, so an edit
/// already mid-debounce on another object is folded into the same flush
/// instead of racing it.
pub(super) fn commit_removal(
    shell: &mut ShellView,
    edits: BTreeMap<(&'static str, String), ObjectEdit>,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    queue_batch(
        shell,
        edits,
        Duration::ZERO,
        "no writable user config directory — nothing was removed",
        cx,
    )
}

/// The tail [`commit_edit`] and [`commit_removal`] share once each has
/// decided what belongs in `edits`: find somewhere to write, capture the
/// batch's revert baseline, and schedule the flush that applies and
/// writes it. `no_dir_notice` differs only in wording between the two
/// callers ("changed" vs. "removed"), so it is the one thing left to
/// parameterise.
fn queue_batch(
    shell: &mut ShellView,
    edits: BTreeMap<(&'static str, String), ObjectEdit>,
    delay: Duration,
    no_dir_notice: &str,
    cx: &mut Context<ShellView>,
) -> Option<String> {
    if edits.is_empty() {
        return None;
    }
    let Some(user_dir) = shell.user_dir.clone() else {
        return Some(no_dir_notice.to_string());
    };

    // Captured before the first edit of a batch, so a failed write
    // restores the state the batch started from.
    let revert = match shell.pending_config_write.as_ref() {
        Some(pending) => pending.revert.clone(),
        None => shell.services.config.all_docs(),
    };

    schedule_flush(shell, user_dir, edits, revert, delay, cx);
    None
}

/// Re-merge the documents with `edits` folded in, and hand the result to
/// the same applier the watcher uses.
///
/// **The previous config's diagnostics deliberately do NOT travel with
/// it.** An earlier build carried them forward, reasoning that nothing
/// had been re-read so nothing new had been learned. That was true and
/// the conclusion was still wrong: `reload::decide` rejects any config
/// holding an error-severity diagnostic, so a single unparseable or
/// unsupported `*.toml` sitting in the user's directory at startup made
/// every dialog edit a silent no-op in memory — while the file write
/// still fired, so memory and disk diverged, and nothing on screen said
/// why. The trader most likely to open a config dialog is precisely the
/// one whose config is broken.
///
/// The diagnostics are not lost so much as not applicable: they describe
/// files that were **skipped**, which contributed no documents, so they
/// are not diagnostics of the documents being re-merged here. Last-good
/// still guards what it is for — `apply_reload` derives `mod_diags` and
/// `keymap_diags` from the documents themselves, so an edit that really
/// does produce a broken config is still rejected. And the watcher, woken
/// by this flush's own write, re-reads the broken file and restores the
/// `config: N error(s) — keeping last good` status within its next poll.
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

/// Fold `edits` into the pending batch and schedule the flush that will
/// apply and write it, after `delay`.
///
/// Every keystroke or removal bumps the sequence and spawns a fresh
/// timer; whichever task wakes holding the current sequence owns the
/// whole accumulated batch, and every superseded task finds a newer
/// sequence and returns. That is the coalescing: N keystrokes inside the
/// debounce window produce N timers, **one** application, and one write
/// per touched document.
///
/// `delay` is [`WRITE_DEBOUNCE`] for [`commit_edit`]'s keystrokes and
/// `Duration::ZERO` for [`commit_removal`]'s single confirmed act. A
/// zero-duration `timer` resolves as soon as it is polled rather than
/// waiting on the clock (`Executor::timer`'s own short-circuit), so a
/// removal needs no clock advance in a test and no wait in the running
/// app, while still folding into — and being folded into by — whatever
/// the pending batch already holds, exactly like a longer delay would.
///
/// The sequence guards both ends of the flush. [`promote`] checks it
/// before doing the work, and [`finish_flush`] checks it before clearing
/// the batch — the second check is not symmetry for its own sake: without
/// it, an edit made while a write is in flight is folded into the batch
/// that the completing write then erases, and it reaches neither memory
/// nor disk.
fn schedule_flush(
    shell: &mut ShellView,
    user_dir: PathBuf,
    edits: BTreeMap<(&'static str, String), ObjectEdit>,
    revert: Vec<LayerDoc>,
    delay: Duration,
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
        cx.background_executor().timer(delay).await;
        let Ok(Some((user_dir, edits))) = this.update(cx, |shell, cx| promote(shell, seq, cx))
        else {
            return;
        };
        let outcome = cx
            .background_executor()
            .spawn(async move { run_writes(&user_dir, edits) })
            .await;
        this.update(cx, |shell, cx| finish_flush(shell, seq, outcome, cx))
            .ok();
    })
    .detach();
}

/// The debounce window has closed: apply the accumulated batch to memory
/// through the one applier, and hand back the writes the file half owes.
///
/// `None` when a later keystroke has taken the batch over — that
/// keystroke's own timer carries everything, including this one's edits,
/// so doing the work twice would be one extra whole-app fan-out for
/// nothing.
///
/// The batch stays on `ShellView` rather than being taken here, for two
/// reasons: a failure still needs its `revert` documents to restore from,
/// and an edit arriving during the write has to have somewhere to land.
#[allow(clippy::type_complexity)]
fn promote(
    shell: &mut ShellView,
    seq: u64,
    cx: &mut Context<ShellView>,
) -> Option<(PathBuf, BTreeMap<(&'static str, String), ObjectEdit>)> {
    let pending = shell.pending_config_write.as_ref()?;
    if pending.seq != seq {
        return None;
    }
    let user_dir = pending.user_dir.clone();
    let edits = pending.edits.clone();
    apply_in_memory(shell, &user_dir, &edits, cx);
    Some((user_dir, edits))
}

/// The write is done, one way or the other.
///
/// **The sequence check on the success arm is load-bearing.** Clearing
/// the batch unconditionally erases any edit that arrived while this
/// write was in flight: that edit was folded into the pending batch, its
/// own flush then finds nothing, and it reaches neither memory nor disk —
/// after which the watcher, woken by the write that *did* land, reverts
/// memory to the older on-disk state and the change disappears with
/// nothing on screen having said so. Only the flush that still owns the
/// batch may clear it; a superseded one leaves it for its successor.
///
/// `pub(crate)` for one reason, stated so it is not mistaken for a leak:
/// the race the sequence check guards cannot be **scheduled** in a gpui
/// test. The test executor polls a `background_executor().spawn` inline,
/// so `run_writes` and this function run inside one `tick()` and no
/// keystroke can be dispatched between them, however finely the ticks are
/// driven (measured). The covering test therefore calls this directly
/// with a stale sequence — real `ShellView`, real pending batch, real
/// function, synthesized scheduling — and then asserts the batch still
/// reaches disk.
pub(crate) fn finish_flush(
    shell: &mut ShellView,
    seq: u64,
    outcome: Result<(), String>,
    cx: &mut Context<ShellView>,
) {
    match outcome {
        Err(message) => revert_failed_write(shell, message, cx),
        Ok(()) => {
            if shell
                .pending_config_write
                .as_ref()
                .is_some_and(|pending| pending.seq == seq)
            {
                shell.pending_config_write = None;
            }
            // A write that succeeds clears whatever the last failure left
            // on the status bar — the config on disk is current again.
            if shell.config_write_error.take().is_some() {
                cx.notify();
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
    tracing::warn!(target: "geode::config", "{message}");
    let Some(pending) = shell.pending_config_write.take() else {
        return;
    };
    // Named apart from the apply path's own `config` on purpose: these
    // two `apply_reload` calls are the only ones in this module, they
    // differ only in which documents they carry, and a mutation entry
    // that anchors on one must not silently land on the other. No
    // carried diagnostics here either, for `apply_in_memory`'s reasons.
    let restored = Config::from_docs(pending.revert);
    shell.apply_reload(restored, cx);
    // The status bar, not just the dialog: `PendingConfigWrite` lives on
    // `ShellView` exactly so a write survives the dialog that started it,
    // so the commonest way to hit this path is with nothing of the
    // dialog's left on screen to carry a notice.
    shell.config_write_error = Some(format!("config not saved — reverted: {message}"));
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
