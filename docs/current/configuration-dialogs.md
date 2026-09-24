# Configuration dialogs

The object dialogs edit Views, Groupings, Scopes, Sources, and Colours, and
inspect Schema. They share a pure draft model and a GPUI adapter. See
[configuration](configuration.md) for layer merging, file writes, and reload
acceptance; this guide describes what editing adds to those contracts.

## Stages and ownership

`ObjectDialogState` owns the domain, stage, mode, browse selection, notices,
and pending confirmation. `Draft` owns one object's fields, source table,
validation diagnostics, edit selection, and queued-change baselines. The
shared input is a text and focus bridge; `sync_dialog_text` reconciles it to
the current state after keyboard and pointer actions.

| Stage | Content and return path |
|---|---|
| Browse | Effective objects and provenance, filtered by visible name and summary |
| Naming | New name; Enter validates and creates, Escape returns without creating |
| Edit | One object's fields; Escape returns to browsing after clearing mode/query |
| Column | Seven presentation fields projected over the same draft; returns to its object |
| Values | Distinct values for one scope dimension; returns to its scope |

Normal mode leaves the input blurred so bare keys act as commands. In Browse,
Edit, Column, and Values, `/` or a click on the frozen filter row enters Filter
mode and records that stage's current query. Each entry takes a fresh snapshot.

While filtering, Escape restores the entry query and bare Enter keeps the
query as typed. Both return to Normal mode without opening the selected row,
committing a value, or closing the dialog. If Escape changes the query, the
selection and viewport return to the first match; an unchanged query preserves
the selection. After keeping a filter, a second Enter performs the stage's
normal action: Browse opens the selected object, and eligible Edit rows open
Column or Values. Other rows retain their ordinary edit instructions.

In Normal mode, Escape clears a remaining query, then returns to the parent
stage, then closes. Open value fields and Naming handle their keys separately:
Enter applies or chooses a field value, or validates and creates a name;
Escape cancels that entry. They do not use the filter snapshot even though
they focus the same input. Cancelling typed text or reverting a filter does
not cancel earlier queued edits. The [shared filter contract](shell.md#dialog-filtering)
also applies to Settings and Keybindings.

Field shapes determine available operations: steps for booleans, choices and
numbers; typed entry for permitted text/numeric fields; typeahead for choices;
and membership or order operations for lists. Keyboard and pointer routes
share the mutation helpers. A pending destructive confirmation blocks other
row mutations, including clicks and drops.

The edit selection only rests on a row that answers to something: a row whose
vocabulary is not inert, or one whose Enter opens a Column or Values stage. A
display-only text row, a list's own header row, a multi-choice row, and a
one-option choice row are therefore painted but never selected — Groupings'
slot number and dimensions header, Scopes' and Views' list headers, and
Schema's derived dimensions. Schema's column rows stay selectable because
Enter opens their column stage. Motion settles onto the next such row in the
direction of travel, following the movement command's own wrap or clamp
behavior, and a pointer click on a row that answers to nothing is dropped
rather than moving the selection. Opening a stage, delivering distinct values,
changing a filter, and rebuilding a draft after a failed write settle the
selection the same way. In a list with no such row — a filter that leaves only
headers, or a dataset of only derived dimensions — settling moves nothing, so
the selection stays where the movement command put it; the footer already
reports that row as inert.

Column and Values stages stash the parent fields and fold changes back into
the same draft before validation and persistence. This avoids independent
copies of an object's nested state. Returning from a Schema column refreshes
its read-only summaries from configuration with pending edits included.

## Definitions and presentation

All writes target the user layer. Each field carries a destination; the writer
groups changed fields and renders whole named objects for those destinations.

| Domain or edit | Destination and behavior |
|---|---|
| Views: dataset or column membership | `views.toml`; replaces the named definition |
| Views: order, hidden state, label, width, format | `view_presentation.toml`; overlays the definition |
| Schema: declared-column presentation | `dataset_presentation.toml`; applies to columns owned by that dataset |
| Groupings | `groupings.toml`; numbered slot containing an array of dimensions |
| Scopes | `scopes.toml`; saved dimension selections, text, and expression |
| Sources | `sources.toml`; source definition, requiring restart for ingestion changes |
| Colours | `colours.toml`; hue/tone or semantic token, with optional sign tinting |

Editing an inherited definition copies the entire object to the user layer.
The dialog announces this fork and the revert operation. Future lower-layer
changes to that object no longer flow through the copied definition.
Presentation-only edits preserve definition inheritance.

View presentation is compared property by property with the view definition
plus dataset presentation, resolved through the column-kind defaults. Equal
values are omitted, so editing a width does not also pin inherited formatting.
Clearing a label or entering an auto width restores the value below and shows
that value immediately. An empty overlay removes its user-layer object.

Presentation writers own their modelled overlay shape. The view writer
rebuilds the overlay and drops unmodelled keys. The dataset writer preserves
other columns of the same dataset but drops unknown dataset-level siblings of
`columns`. Most definition adapters start from the source table and preserve
unmodelled keys; Groupings has a bare-array definition instead.

Schema definitions and derived-dimension rows are read-only. Only declared
columns open its presentation editor. Groupings always lists slots 1–9,
including empty slots, and does not create or rename slots. Its final selected
dimension cannot be unticked; deletion or reversion handles removing the user
entry. Scopes selections have no meaningful order and offer no reorder route.

## Validation and persistence

A field edit changes the draft immediately. Adapters normalize typed values
and validate the current rendered object; error-severity diagnostics block
value edits from entering the pending batch. Warnings remain editable. The
header keeps every diagnostic, and resolvable diagnostic paths also mark the
relevant field. Source-array indices resolve through member names so a view's
presentation order does not redirect a diagnostic to the wrong column.

There is no Save action. Ordinary edits wait for a 250 ms quiet period after
the latest queued edit. Creation and confirmed removal use zero delay through
the same asynchronous path. The batch is keyed by document and object, so it
can span several objects and files. Each key retains its latest whole-object
value. Closing the dialog leaves queued work on the shell.

When the current timer fires, the shell folds those values into the active
layered documents, merges them with `Config::from_docs`, and calls the same
reload applier used by the watcher. It then submits background writes through
the directory's configuration writer. Each touched document is parsed and
edited once, preserving other objects and their comments. Memory and disk
receive values derived from the same rendered object text.

The draft's baseline advances when a change is queued, not when disk confirms
it. Without a writable user directory, nothing is queued and the draft remains
dirty. Reopening an existing object or nested stage reads configuration with
the pending batch folded in, preventing a stale draft from overwriting edits
still waiting for promotion. An already-open draft is not automatically
rebased by unrelated reloads.

Merge acceptance and disk success are separate outcomes. A reload rejection
keeps active configuration while the file write still proceeds; a successful
write then reports that it was saved but rejected by the merge. In-memory
merging does not carry prior file-reading diagnostics for skipped files;
the reload applier derives its acceptance diagnostics from the documents, and
a subsequent watcher read can report unresolved disk errors again.

A current write failure attempts to restore all documents captured before the
batch and rebuilds the open draft. Restoration leaves nested stages and
returns to the object stage. Sequence checks ignore old completion callbacks,
so an older success or failure cannot clear a newer batch, restore its old
snapshot, or replace current status.

This is not a transaction across files. Some documents can persist before
another fails; recovery does not undo those disk writes. A later watcher read
may apply that partial disk state, subject to normal reload acceptance. There
is also no shutdown flush for pending dialog edits: exiting before promotion
loses them, and writes still in flight are best effort at process exit.

The watcher does not suppress dialog writes. Equal layered documents avoid
view-specific events and changed-document rebuilds, but every accepted reload
still advances the frame's configuration revision and republishes chords.
These echoes are therefore not completely inert.

## Creation, removal, and drift

Creation rejects malformed or reserved names and names already held by any
definition layer, a fixed roster, or the user's presentation overlay. An
orphaned view overlay still reserves its name; the notice directs the user to
remove that entry before creating over it. Scopes can also copy a named saved
scope or save the current frame scope under a new name.

Delete removes a user-defined object. Revert requires an inherited object to
restore and removes the user's definition and associated view presentation
where present. A presentation-only view override can therefore be reverted
without ever having copied its definition. These object-wide operations are
unavailable inside Column and Values stages. Overwriting a user-owned scope
requires confirmation; overwriting an inherited one makes an announced fork.
Removals bypass draft validation errors so an invalid object can still be
removed or reverted.

`overrides.toml` records the shadowed layer and canonical inherited object text
when a definition is forked. Browse drift compares that baseline against the
current inherited definition. It does not compare the user's deliberate edits
with the inherited object. Missing or stale records cannot establish drift;
sidecar cleanup accompanies relevant fork/removal batches.

Drift is informational, not a conflict lock. There is no optimistic version
check against another editor: read-modify-write preserves other objects in the
file, but the dialog replaces the same object's value with its draft. Whole
batch recovery likewise uses a captured configuration snapshot rather than a
merge of intervening external changes.

## Saved-scope values

Opening Values requests distinct values using the draft's scope with the open
column's constraint removed, plus the frame's as-of state. Each request has a
monotonic tag; delivery must match the current Values stage and tag. Loading
and failure rows remain read-only until a usable result arrives.

The list retains selected values absent from the returned data and marks them
as such. Ticking values updates the parent scope's source; unticking the last
one removes that dimension constraint. Selection summaries are presentation
only: persistence dirtiness also compares source values so two different
selections with identical truncated summaries still produce a write.
