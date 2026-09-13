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
//! `toml_edit::Item` — [`object_value`] parses exactly the text
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
//! It will, and nothing here suppresses it. `apply_reload` decides what a
//! reload changes by comparing the freshly loaded layered documents
//! against the ones already in `services.config` (`docs_equal`), so what
//! that reload costs depends on whether the file read back says exactly
//! what memory already says.
//!
//! For a **value-setting** edit it does, by construction: the file on
//! disk is `config_write::edit`'s read-modify-write of the same object
//! value memory already holds, and a fresh user-layer document
//! [`docs_with_object`] creates carries the same `config_version` stamp
//! `edit` puts at the top of a file it creates — so every `changed(..)`
//! predicate answers false, no `ConfigReloaded` is emitted, no tile
//! requeries, no palette closes, and the frame is never touched. The
//! reload assigns an identical `Config` and repaints.
//!
//! A **removal** against a doc the user layer has no file for is the one
//! case where it does not, and it is stated rather than hidden:
//! [`docs_with_object`] deliberately creates nothing in memory for a
//! `None` value when there is no user doc to remove from, while
//! `config_write::edit` read-or-**creates** and stamps it, so disk gains
//! a `config_version`-only document memory does not have. `docs_equal`
//! differs, and the watcher's next poll reports `changed(..)` for a write
//! the app itself made. The cost is one spurious fan-out — the same
//! merge over the same values, one extra time — never a wrong value.
//!
//! That is why the reload is left alone in both cases: suppressing it
//! would mean keeping a "self-write" ledger that has to be right about
//! every path a write can take (including the ones that fail after the
//! ledger entry is made) to avoid missing a real external edit, and the
//! reload cannot change a value either way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use geode_core::config::{CONFIG_VERSION, Config, Layer, LayerDoc, Severity};
use gpui::Context;

use super::{Destination, Domain, Stage};
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

/// The status line for a flush whose in-memory merge was refused (§19.6):
/// the file half of a flush (`run_writes`) and the memory half
/// (`apply_in_memory` → `apply_reload`) are independent outcomes, and
/// `reload::decide` rejecting the merge says nothing about whether the
/// write itself succeeded — disk stays the arbiter (this module's own
/// header), so the write still happens and the status line has to say
/// both halves rather than picking one.
pub(crate) const REJECTED_STATUS: &str = "saved to disk · rejected by the merge";

/// One config document's user-layer copy of one object, as an edit leaves
/// it: `Some(value)` to set it, `None` to remove it entirely.
///
/// `None` is not an optimisation, and it does **not** mean "empty" —
/// [`object_value`] decides which renderings become one, and only a
/// [`Destination::Presentation`] one ever can. A presentation table that
/// matches the view's own doc in every respect renders **empty**
/// (`views::presentation_table` omits an `order` equal to the doc's, an
/// empty `hidden`, and every width the doc already declares), and writing
/// an empty table would leave `[tree]` alone in `view_presentation.toml`
/// — a table that says nothing, which is exactly the artefact seen in a
/// user's file before this design. Under the old staged-save model that
/// needed a save whose draft excluded nothing; under this one it is one
/// keystroke away, every time a trader unhides the last hidden column.
/// So an empty *overlay* rendering means "I have no personalisation of
/// this object" and is written as an absence, in memory and on disk
/// alike.
pub type ObjectEdit = Option<toml::Value>;

/// What one rendered object does to its user-layer document — the return
/// of [`object_value`], and the reason "empty" cannot mean the wrong
/// thing at the wrong destination.
///
/// Three answers, not two, because removal and no-write are genuinely
/// different acts in a layered config: removing the user's key means
/// *inherit the layer beneath*, which is the opposite of what an emptied
/// object asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjectWrite {
    /// Set the user-layer key to this value.
    Set(toml::Value),
    /// Remove the user-layer key. Only an overlay destination
    /// ([`Destination::Presentation`]) ever renders this, where absence
    /// IS the state being recorded.
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
}

/// What `object`'s user-layer entry in `dest`'s document should become.
///
/// Goes through [`super::object_text`] — the exact text the file write
/// produces — and parses it back, rather than converting
/// `toml_edit::Item` to `toml::Value` field by field. One rendering,
/// two destinations: memory cannot end up holding something the file
/// would not have said. It is a parse of a few hundred bytes, and it is
/// part of the keystroke rather than of the flush — measured inside that
/// keystroke's 37 µs, against an 8 ms budget.
///
/// **`dest` is a parameter, not a convenience.** What an *empty*
/// rendering means is not a property of the item — it is a property of
/// the file it would be written to, and the two files these dialogs write
/// mean opposite things by an absent key:
///
/// * `view_presentation.toml` is an **overlay** read over the object it
///   names, so an absent key is "I have no personalisation of this
///   object" (spec §16) — exactly what an empty rendering says, hence
///   [`ObjectWrite::Remove`];
/// * a domain's **own** doc is merged by layer, atomically at depth 1,
///   so an absent user key means *inherit the layer beneath*. Removing it
///   for an emptied object would silently restore the desk's copy of it —
///   the trader asks for "this slot groups by nothing" and gets "revert
///   to the desk's slot 3", with the edit stage still painting the empty
///   chain they just cleared. So a Doc rendering never collapses to an
///   absence, and the question cannot be reached from the wrong
///   destination because no caller gets to answer it.
///
/// The other half of that rule lives in `Draft::step_selected`, which
/// refuses the keystroke that would empty a `Destination::Doc` list at
/// all: the states the config model cannot represent are not offered,
/// so [`ObjectWrite::Nothing`] below is the inert answer to something
/// that should never arrive rather than a behaviour anything relies on.
pub fn object_value(object: &str, item: toml_edit::Item, dest: Destination) -> ObjectWrite {
    if item_is_empty(&item) {
        return match dest {
            Destination::Presentation => ObjectWrite::Remove,
            Destination::Doc => ObjectWrite::Nothing,
        };
    }
    let text = super::object_text(object, item);
    let parsed: Option<toml::Value> = text
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut parsed| parsed.remove(object));
    // A rendering that will not parse back is a bug in the renderer, not
    // an instruction to delete the object: nothing, rather than the
    // removal an `Option` return used to collapse this into.
    match parsed {
        Some(value) => ObjectWrite::Set(value),
        None => ObjectWrite::Nothing,
    }
}

/// Whether `item` carries nothing at all: an empty table (no keys, which
/// `views::presentation_table` can render) or an empty array (every
/// `dimensions` tick undone, which `groupings::to_table` could render if
/// `Draft::step_selected` let a trader get there). What that *means* is
/// [`object_value`]'s question, not this one's — this says only that the
/// container is empty.
///
/// Any other value — a populated table, a populated array, a bare
/// string or number — is never empty: no domain built so far renders a
/// Doc write as a bare scalar, so there is nothing else this could mean.
fn item_is_empty(item: &toml_edit::Item) -> bool {
    match item {
        toml_edit::Item::Table(t) => t.is_empty(),
        toml_edit::Item::ArrayOfTables(a) => a.is_empty(),
        toml_edit::Item::Value(v) => v.as_array().is_some_and(|a| a.is_empty()),
        toml_edit::Item::None => true,
    }
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
///
/// `row.layer: None` (§18.4 — an unconfigured Groupings slot) forks
/// nothing: there is no copy in any layer for a user-layer write to
/// freeze anyone out of, so filling an empty slot is a plain write, not
/// a fork.
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
/// to prevent. The draft keeps the value and stays **dirty** — its
/// baseline only moves once the batch is queued — and the row still
/// paints it, which is the honest picture of a shell with nowhere to
/// write.
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
    let mut edits = edits_for(shell);
    if edits.is_empty() {
        return None;
    }
    // §19.6: a Doc write onto an object the user layer does not own is a
    // fork; record what it shadows, in THIS batch, so the entry cannot
    // land without the fork nor before it. Stale entries ride along as
    // removals — the one moment the sidecar is being written anyway.
    //
    // The stale removals are inserted BEFORE the fork's own entry below,
    // and that order is load-bearing: at fork time the user layer does
    // not yet hold the object, so `stale_override_keys` can list the
    // very key being written here as stale. Both inserts share one
    // `BTreeMap` key, so whichever runs second wins — the fresh `Some`
    // entry below must be that one, not the stale `None` clearing it
    // right back out.
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

    // **Before the baseline moves.** A shell with nowhere to write queues
    // nothing, so nothing has been accounted for and the draft must stay
    // dirty: a `mark_saved()` here would make the unqueued value the
    // baseline, and a later declined `Confirm::Fork` would then
    // `revert_to_baseline` onto a forked value that was never applied and
    // never persisted — exactly what `cancel_confirm` exists to prevent.
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

    queue_batch(shell, edits, user_dir, WRITE_DEBOUNCE, cx);
    None
}

/// Record a confirmed `d`/`r`'s removal and put it on the exact batch a
/// field edit would join — the same [`PendingConfigWrite`], the same
/// [`promote`] → [`apply_in_memory`] → [`run_writes`] → [`finish_flush`],
/// so a delete or revert is no longer invisible until the 500 ms watcher
/// notices the write `spawn_removals` used to make on its own.
///
/// Takes bare `(doc, object)` keys, not `edits` — **on purpose, and this
/// is the property the rest of this doc leans on.** An earlier version
/// took the same `BTreeMap<(&'static str, String), ObjectEdit>` shape
/// [`commit_edit`] does and trusted `render::removal_edits` to fill
/// every value with `None`; nothing in that signature stopped a future
/// caller passing `Some(value)` instead, which would join the batch,
/// flush at zero delay and skip [`blocking_diagnostic`] entirely — the
/// exact hole Task 2 closed, reopened through the one door built not to
/// need the gate. Building the `None`s here, from keys that carry no
/// value at all, makes that unrepresentable rather than conventional: a
/// caller cannot pass a value through this door because there is no
/// parameter to put one in.
///
/// **Deliberately does not call [`blocking_diagnostic`].** That gate
/// exists so an edit `Domain::validate` rated `Severity::Error` can never
/// join the batch, because `reload::decide` would refuse the merge a
/// flush later while the file write had already fired. A removal cannot
/// hit that: it does not write a value the reader could reject, it
/// erases the object carrying one. Gating a delete or revert on the very
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
    queue_batch(shell, edits, user_dir, Duration::ZERO, cx);
    None
}

/// Record a freshly named object (§18.2) — the third door onto the batch
/// beside [`commit_edit`] and [`commit_removal`].
///
/// One `Destination::Doc` write, built here rather than by `edits_for`: a
/// new draft's baselines are empty (`Draft::new_object`), so
/// `writes_by_destination` would also name `Presentation` for an untouched
/// column list and queue an empty overlay write that means "remove what is
/// not there". Gated by [`blocking_diagnostic`] like an edit (a new object
/// the reader rejects must not reach disk); flushed at `Duration::ZERO`
/// like a removal (one decided act, nothing to coalesce) so the browse
/// list the config derives shows the object on the next executor tick
/// rather than 250 ms later.
///
/// `pub(crate)`, not `pub(super)`: `render`'s `create_from_name` — the `n`
/// keybinding's `enter`, over [`Stage::Naming`](super::Stage::Naming) —
/// is `render`'s production caller; `crate::shell::tests::objectdialog`
/// also calls it directly, to pin this door's own contract apart from the
/// keybinding that reaches it.
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
    queue_batch(shell, edits, user_dir, Duration::ZERO, cx);
    None
}

/// The tail [`commit_edit`] and [`commit_removal`] share once each has
/// decided what belongs in `edits` **and** found somewhere to write it:
/// capture the batch's revert baseline and schedule the flush that applies
/// and writes it.
///
/// `user_dir` is a parameter rather than looked up here, and that is the
/// whole point of the split: each caller has to resolve it *before* it
/// commits to anything else (`commit_edit` moves the draft's baseline),
/// and their two no-directory notices differ in wording anyway ("nothing
/// was changed" vs. "nothing was removed"). Taking the directory as an
/// argument means a caller cannot reach the queue without having answered
/// that question first.
fn queue_batch(
    shell: &mut ShellView,
    edits: BTreeMap<(&'static str, String), ObjectEdit>,
    user_dir: PathBuf,
    delay: Duration,
    cx: &mut Context<ShellView>,
) {
    // Captured before the first edit of a batch, so a failed write
    // restores the state the batch started from.
    let revert = match shell.pending_config_write.as_ref() {
        Some(pending) => pending.revert.clone(),
        None => shell.services.config.all_docs(),
    };

    schedule_flush(shell, user_dir, edits, revert, delay, cx);
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

/// The live config with the pending batch folded in — what memory WILL
/// hold once the debounce closes — or `None` when nothing is pending and
/// `services.config` is already that.
///
/// This exists for one reader, [`super::render::enter_edit_stage`], and
/// closes a hole the digit jump (§18.8) made two keystrokes wide: a
/// draft derived from `services.config` inside the [`WRITE_DEBOUNCE`]
/// window paints the object as it stood BEFORE the tick just made,
/// because the flush has not reached memory yet — and that stale draft
/// then outlives the flush (nothing rebuilds an open draft when
/// [`promote`] applies), so its next tick renders the whole object
/// without the earlier one and writes that. Deriving from the folded
/// documents instead makes the draft agree with the batch by
/// construction; the flush, when it comes, changes nothing the draft does
/// not already show.
///
/// The same fold [`apply_in_memory`] does, minus the apply: no
/// `apply_reload`, no fan-out, no write — the batch keeps its own timer
/// and its own debounce semantics. `Config::from_docs` over every layered
/// document is the whole cost, the one `promote` pays on every flush.
pub(crate) fn config_with_pending(shell: &ShellView) -> Option<Config> {
    let pending = shell.pending_config_write.as_ref()?;
    let mut docs = shell.services.config.all_docs();
    for ((doc, object), value) in &pending.edits {
        docs = docs_with_object(docs, &pending.user_dir, doc, object, value.clone());
    }
    Some(Config::from_docs(docs))
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
        let Ok(Some((user_dir, edits, rejected))) =
            this.update(cx, |shell, cx| promote(shell, seq, cx))
        else {
            return;
        };
        let outcome = cx
            .background_executor()
            .spawn(async move { run_writes(&user_dir, edits) })
            .await;
        this.update(cx, |shell, cx| {
            finish_flush(shell, seq, outcome, rejected, cx)
        })
        .ok();
    })
    .detach();
}

/// The debounce window has closed: apply the accumulated batch to memory
/// through the one applier, and hand back the writes the file half owes
/// plus, when the merge itself was refused, how many errors it carried
/// (§19.6).
///
/// `None` when a later keystroke has taken the batch over — that
/// keystroke's own timer carries everything, including this one's edits,
/// so doing the work twice would be one extra whole-app fan-out for
/// nothing.
///
/// The batch stays on `ShellView` rather than being taken here, for two
/// reasons: a failure still needs its `revert` documents to restore from,
/// and an edit arriving during the write has to have somewhere to land.
///
/// The third element of the returned tuple is read right after
/// `apply_in_memory` returns: that call runs `apply_reload`, which just
/// set `self.last_reload` to whatever `reload::decide` answered for the
/// merged config, so this is the first and only moment `promote` can
/// learn whether the write about to happen is going to a config that
/// memory actually took.
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
/// `rejected` (§19.6) is `promote`'s own reading of `shell.last_reload`
/// right after it applied the merge — `Some(n)` when `reload::decide`
/// kept last-good over `n` errors, `None` when the merge was applied (or
/// when a superseded `promote` never ran at all, in which case this
/// whole function is never reached for that flush). It says nothing
/// about whether the FILE write below succeeded: the two are independent
/// outcomes of the same flush, and a `Some` here only ever changes the
/// success arm's own status line, never which branch of the `match` on
/// `outcome` runs.
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
    rejected: Option<usize>,
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
            // §19.6: the file is on disk either way; what differs is
            // whether memory took it. A rejected merge is said in the
            // same status slot a failed write uses, and cleared by the
            // next flush memory accepts.
            match rejected {
                Some(n) => {
                    shell.config_write_error = Some(format!(
                        "{REJECTED_STATUS}: {n} error(s) — keeping last good"
                    ));
                    cx.notify();
                }
                None => {
                    // A write that succeeds AND lands in memory clears
                    // whatever the last failure (or rejection) left on
                    // the status bar — the config on disk is current
                    // again, and memory agrees with it.
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
        // Part 2c §5.2: a rebuilt draft is the OBJECT's, with no column
        // projection on it, so the stage has to come back with it — and
        // so does the cursor. Left at `Column` the crumb would keep
        // naming a column whose seven fields are no longer installed, the
        // one thing on screen still claiming the projection this revert
        // just dropped; and `selected` is an index into those seven,
        // which against the view's own rows points wherever that number
        // happens to land. Both are resolved the way `Draft::leave_column`
        // resolves them: the stage steps back to the view, the cursor onto
        // the column's own row, by NAME.
        match &state.stage {
            Stage::Column { object, column } => {
                let (object, column) = (object.clone(), column.clone());
                rebuilt.select_item_named(&column);
                state.stage = Stage::Edit { object };
            }
            _ => rebuilt.selected = selected,
        }
        state.draft = Some(rebuilt);
        state.notice = Some(format!("could not save — change reverted ({message})"));
    }
    cx.notify();
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
        // An empty array reaches the same answer for the same reason —
        // `item_is_empty` widened beyond `Table::is_empty` for Groupings'
        // bare-array object shape, and the widening is about the
        // container, not about which file it is going to.
        let array = toml_edit::Item::Value(toml_edit::Array::new().into());
        assert_eq!(
            object_value("tree", array, Destination::Presentation),
            ObjectWrite::Remove
        );
    }

    /// A populated array round-trips as a `toml::Value::Array` — exactly
    /// the shape `GroupingSlots::from_doc` reads back — rather than the
    /// `toml_edit::Table` fallback an unconditional `.as_table()` would
    /// have produced (an empty table, silently erasing the slot).
    /// Groupings is the first (and so far only) domain whose object is
    /// ever a bare array rather than a table, so this is the one path
    /// Views' own tests never exercise.
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
