# Configuration dialogs

The object dialogs edit Views, Groupings, Scopes, Sources, Colors, and
Expressions, and inspect Schema. They share a pure draft model and a GPUI
adapter. See
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
list resets toward the first match; edit stages then settle selection onto an
eligible row as described below. An unchanged query preserves selection.
After keeping a filter, a second Enter performs the stage's normal action: Browse opens the selected object, and eligible Edit rows open
Column or Values. Other rows retain their ordinary edit instructions.

In Normal mode, Escape clears a remaining query, then returns to the parent
stage, then closes. The title row's Back button, painted in every stage but
Browse, returns to the parent stage in one click: it first cancels an open
value field, reverts filtering, and clears the query, and it does nothing
while a confirmation is pending. Open value fields and Naming handle their keys separately:
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

A member row of an ordered list drags from its `⋮` grip, not its body: a press
on a Views member row opens that column's stage, so a gesture started there
would leave the list before it could move anything. The grip's press arms the
drag and does nothing else. Available rows have no grip and open nothing on a
press, so the whole row stays their drag handle. Any row of the list is a drop
target.

In Edit, Column, and Values, `Draft::is_cursor_stop` determines which rows can
hold selection. A row qualifies when it supports a row-specific command or
opens a Column or Values stage. Display-only text, list headers, multi-choice
fields, and one-option choices remain visible for context and diagnostics but
are skipped when eligible rows exist. Schema's declared-column rows qualify
because they open presentation editors; derived-dimension rows do not.
Object-wide commands such as delete, revert, or Groupings' chain editor do
not make an otherwise inert row eligible.

`Draft::move_selection` first moves through the full filtered list, then finds
an eligible row in the direction of travel. Counts measure visible rows, not
eligible stops. A one-row move wraps; larger moves and top/bottom commands
clamp, searching back from the boundary if necessary. Selection resets use
`Draft::settle_selection`: the nearest eligible row at or after the current
position, then the nearest before it. Stage entry, filter changes, distinct
value delivery, and failed-write reconstruction use this same rule. Open value
fields bypass this settling: plain fields retain the edited-row selection,
while chain and choice fields route completion selection separately.

If the filtered list has no eligible row, settling leaves selection unchanged;
keyboard movement can still traverse the inert rows and the footer reports
that no row command is available. Pointer clicks on ineligible rows are always
ignored, including in this fallback state.

Column and Values stages stash the parent fields and fold changes back into
the same draft before validation and persistence. This avoids independent
copies of an object's nested state. Returning from a Schema column refreshes
its read-only summaries from configuration with pending edits included.

Rows carry no destination badge: every field of one stage writes the same
place (the object's document, or a Column stage's one overlay), so the badge
would read the same on every row. Rows name a layer instead where one
applies. A Column stage names the layer a value comes from (`desk`,
`dataset`, or `view`; none for the kind default), and Schema names the layer
that defined each column or derived dimension. Each badge sits in a
right-aligned slot as wide as the widest name it can hold, so values stay in
one column whatever layer each row names and when a badge appears mid-edit.

## Definitions and presentation

All writes target the user layer. Each field carries a destination; the writer
groups changed fields and renders whole named objects for those destinations.

| Domain or edit | Destination and behavior |
|---|---|
| Views: dataset or column membership | `views.toml`; replaces the named definition |
| Views: order, hidden state, label, width, format | `view_presentation.toml`; overlays the definition |
| Schema: declared-column presentation | `dataset_presentation.toml`; applies to columns owned by that dataset |
| Groupings | `groupings.toml`; numbered slot containing an array of dimensions |
| Scopes | `scopes.toml`; saved dimension selections, text, expression, and ticked named-expression references |
| Sources | `sources.toml`; source definition, requiring restart for ingestion changes |
| Colors | `colors.toml`; hue/tone or semantic token, with optional sign tinting |
| Expressions | `expressions.toml`; one named scope expression, referenced by name from a saved scope or the frame |

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
entry. Scopes selections have no meaningful order and offer no reorder route,
and neither does a scope's named-expression list.

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
remove that entry before creating over it. Scopes and Expressions can also
copy an existing object under a new name (`c`); Scopes can additionally save
the current frame scope under a new name. A named expression has no default
text an empty definition could hold: naming a fresh one opens its `expression`
field at once, with no validation error yet painted for the still-empty text,
and the object is written to the user layer only on the first Enter that
parses and is not empty. Escape before that Enter writes nothing. A copy
starts from its source's text as usual, since it is never empty.

Delete removes a user-defined object. Revert requires an inherited object to
restore and removes the user's definition and associated view presentation
where present. A presentation-only view override can therefore be reverted
without ever having copied its definition. These object-wide operations are
unavailable inside Column and Values stages. Overwriting a user-owned scope
requires confirmation; overwriting an inherited one makes an announced fork.
Removals bypass draft validation errors so an invalid object can still be
removed or reverted.

Deleting a named expression names its users under the question: every saved
scope that ticks it, in name order, then "the current scope" when the frame's
own scope does, joined "A", "A and B" or "A, B and C" — for example "Used by
EQ liquid, RATES liquid and the current scope." Nothing is listed when no
scope or the frame uses it. Confirming deletes only the definition; it never
rewrites those scopes, which then show the name as missing.

`overrides.toml` records the shadowed layer and canonical inherited object text
when a definition is forked. Browse drift compares that baseline against the
current inherited definition. It does not compare the user's deliberate edits
with the inherited object. Missing or stale records cannot establish drift;
sidecar cleanup accompanies relevant fork/removal batches. Entries are keyed
`<doc>.<object>`; one recorded under a renamed document's old name
(`colours.<name>`) still counts for the current document, is removed with its
object, and is pruned as stale once a current-spelled entry exists.

Drift is informational, not a conflict lock. There is no optimistic version
check against another editor: read-modify-write preserves other objects in the
file, but the dialog replaces the same object's value with its draft. Whole
batch recovery likewise uses a captured configuration snapshot rather than a
merge of intervening external changes.

## Saved-scope values

Opening Values requests distinct values using the draft's scope with the open
column's constraint removed, plus the frame's as-of state. Each request has a
monotonic tag; delivery must match the current Values stage and tag. Loading
and failure rows remain read-only until a usable result arrives. When that
scope carries a named-expression reference the current `expressions` document
cannot resolve, the stage shows the resolution error as its values-unavailable
text instead of requesting anything.

The list retains selected values absent from the returned data and marks them
as such. Ticking values updates the parent scope's source; unticking the last
one removes that dimension constraint. Selection summaries are presentation
only: persistence dirtiness also compares source values so two different
selections with identical truncated summaries still produce a write.

## Named expressions field

A saved scope has a **Named expressions** field, an ordered-list field shaped
like Dimensions: the scope's own references, ticked, above the rest of the
`expressions.toml` document, unticked and available. `space` and a click tick
or untick a name exactly as they do a dimension; unlike Dimensions, unticking
the last one is allowed, since an empty `named` list is an ordinary scope with
no references at all rather than a state the reader must distinguish. The
list has no reorder route, the way Dimensions has none.

A ticked name the `expressions` document cannot supply is not dropped or
hidden: its row carries a `missing` or `invalid` note (the same two reasons
[`Scope::resolve`](shell.md#the-shared-frame) reports) and can still be
unticked. Saving a scope with such a name warns at that row rather than
blocking the commit — the reference may be legitimate and the definition
written afterward — and the query reports it once the scope is used.

## Scope expression field

The Scopes domain's `expression` field carries the same suggestion list as
the frame's expression dialogs — see [input-and-dialogs.md's Frame
expression](input-and-dialogs.md#frame-expression) for the rows, operators,
value rules, and keys, all of which apply here while the field is open. It
narrows its values request by the edited scope's own dimension selections
and text filter, with no expression (the field is replacing it), and the
frame's as-of.

Enter commits the field, as any other text field does, and refuses an
unknown column or a bad operator on a derived dimension with an `expression:`
notice, keeping the field open; escape reverts the field's text instead of
committing it. Both keep the object dialog's own text-entry contract; only
tab, the arrows, and ctrl+p/ctrl+n additionally serve the suggestion list
while this field is open.

The Expressions domain's own `expression` field — the one field a named
expression has — carries the same suggestion list and the same Enter/Escape
contract, with two differences. Its values request has no enclosing scope: a
named expression is ANDed into whichever scope ticks it, so its suggested
values are the whole dataset's rather than narrowed by anything. And an empty
field is refused on Enter with `expression: a named expression cannot be
empty`, keeping the field open, rather than accepted as clearing an optional
filter — an empty named expression would be a definition every scope ticking
it fails on.

A resolution failure — the edited scope's own `named` list carrying a missing
or invalid reference — is reported wherever this dialog would otherwise
request values under that scope: the Values stage (above) and this field's
own suggestion list both show it as their ordinary values-unavailable text,
and no request is sent. See [the shared frame](shell.md#the-shared-frame) for
resolution itself and the other surfaces it reaches.
