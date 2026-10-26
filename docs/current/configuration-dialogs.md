# Configuration dialogs

The object dialogs edit Views, Groupings, Sources, Colors and VolMonitor views (the
VolMonitor's tabs), and inspect Schema. They share a pure draft model and
a GPUI adapter. See
[configuration](configuration.md) for layer merging, file writes, and reload
acceptance; this guide describes what editing adds to those contracts.

Saved scopes and named expressions are managed from the Scope dialog's
[saved sections](input-and-dialogs.md#saved) (`config::scopes`,
`config::expressions` open it on them), not from an object dialog.

## Stages and ownership

`ObjectDialogState` owns the domain, stage, mode, browse selection, notices,
and pending confirmation. `Draft` owns one object's fields, source table,
validation diagnostics, edit selection, and queued-change baselines. The
shared input is a text and focus bridge; `sync_dialog_text` reconciles it to
the current state after keyboard and pointer actions.

`ObjectDialogState::rows` holds the browse list prepared from the active
configuration at its config revision and ranked for the query (in Naming, the
typed name); render and every browse key and click read it. A covered object
dialog is re-keyed when it is revealed, so a reload while it was parked shows
on the first paint. Landing the cursor after a removal still ranks the
pending-aware configuration, because the painted list catches up only when the
batch applies. The Edit and Column rows are the draft's and are not
prepared: they derive at each render and handler call, because one derivation
and rank of the largest demo draft costs 2 to 13 microseconds, too little to
repay a cache key and its refresh seams (see
[performance](performance.md#cache-and-allocation-contracts)).

| Stage | Content and return path |
|---|---|
| Browse | Effective objects and provenance, filtered by visible name and summary |
| Naming | New name; Enter validates and creates, Escape returns without creating |
| Edit | One object's fields; Escape returns to browsing after clearing mode/query |
| Column | Seven presentation fields (Views, Schema) or a vol column's own parts (VolMonitor views) projected over the same draft; returns to its object |

Normal mode leaves the input blurred so bare keys act as commands. In Browse,
Edit, and Column, `/` or a click on the frozen filter row enters Filter
mode and records that stage's current query. Each entry takes a fresh snapshot.

While filtering, Escape restores the entry query and bare Enter keeps the
query as typed. Both return to Normal mode without opening the selected row,
committing a value, or closing the dialog. If Escape changes the query, the
list resets toward the first match; edit stages then settle selection onto an
eligible row as described below. An unchanged query preserves selection.
After keeping a filter, a second Enter performs the stage's normal action:
Browse opens the selected object (on Groupings it applies the row; see
[the Grouping dialog](#the-grouping-dialog)), and eligible Edit rows open
their Column stage. Other rows retain their ordinary edit instructions.

In Normal mode, Escape clears a remaining query, then returns to the parent
stage, then closes. The title row's Back button, painted in every stage but
Browse, returns to the parent stage in one click: it first cancels an open
value field, reverts filtering, and clears the query, and it does nothing
while a confirmation is pending. Open value fields and Naming handle their
keys separately:
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

In Edit and Column, `Draft::is_cursor_stop` determines which rows can
hold selection. A row qualifies when it supports a row-specific command or
opens a Column stage. Display-only text, list headers, multi-choice
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

A Column stage stashes the parent fields and folds changes back into
the same draft before validation and persistence. This avoids independent
copies of an object's nested state. Returning from a Schema column refreshes
its read-only summaries from configuration with pending edits included.

A Column stage can also be entered directly from a tile. "Edit column in
view…" (`config::view_column`) and "Edit column in schema…"
(`config::schema_column`) list the focused tile's presented columns; see
[input and dialogs](input-and-dialogs.md#tile-log-and-column-choices).
A pick opens Views on the tile's view, or Schema on the dataset that owns the
column (the view's primary dataset, then its joins in declaration order), and
enters that column's stage. Both are resolved against configuration with
pending edits folded in, not the tile's copy. An undefined view, or a column
no dataset of the view declares, stays in Browse with a footer notice; a
column the object no longer has stops on the object's Edit stage with a
notice. Escape then walks the usual ladder: Edit, Browse, close.

Rows carry no destination badge: every field of one stage writes the same
place (the object's document — a VolMonitor views tab's `Default` writes that
document's top-level key — or a Column stage's one overlay), so the badge
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
| Views: dataset or column membership | `views.toml`; replaces the named definition. The dataset choice offers every stored and computed dataset; Sources offers stored ones only. |
| Views: order, hidden state, label, width, format | `view_presentation.toml`; overlays the definition |
| Schema: declared-column presentation | `dataset_presentation.toml`; applies to columns owned by that dataset |
| Groupings | `groupings.toml`; numbered slot containing an array of dimensions |
| Sources | `sources.toml`; source definition, requiring restart for ingestion changes. The rows follow the source's shape when the edit opens: a directory source shows dataset, paths, readiness, stable polls, priority, poll interval, pending timeout, batch pattern and adapter; another adapter adds read-only document, topics, coalesce and source time (a subscription's `recover_timeout` and `recover_max_age` are not shown; set by hand, they survive a save from the dialog); a snapshot source (another adapter over a reference dataset) shows only dataset, poll interval (default `5m`), an editable table and the adapter; it has no priority, since snapshots are taken ahead of every file, and its browse line names only its table. The shape is not rederived when the dataset choice changes; a mismatch is reported by validation and the rows follow on the next open. |
| Colors | `colors.toml`; hue/tone or semantic token, with optional sign tinting; also opened at naming by the row menu's `Color…` › `New named color…`, where creating the color sets the value's color |
| Row menu: Color… | `value_colors.toml`; one value's entry in the user layer (a name, an inline `{ hue }` table, `none`, or removed) |

Editing an inherited definition copies the entire object to the user layer.
The dialog announces this fork and the revert operation. Future lower-layer
changes to that object no longer flow through the copied definition.
Presentation-only edits preserve definition inheritance.

Each column-stage field is either set at the stage's layer (the view's
overlay on Views, the dataset's on Schema) or inherited from the layers
below. Which keys that overlay holds is read when the view is opened (Views)
or when the column is (Schema); changing a
value sets it, even to a value equal to the one it inherits, which pins it.
`r` makes the selected field inherit again, `shift+r` every field, and a
set field's ↺ does what `r` does; an empty Label or `auto` Width inherits
too. An inherited field shows the value in force below it, badged with the
layer that sets it (`dataset` or `desk` on Views; none for the kind default
or on Schema); a set field carries the `view` or `dataset` badge. The
writers write exactly the set keys: a column with none is dropped, and an
empty object removes its user-layer entry. Keys already on disk read as
set, so an overlay key equal to its parent shows as pinned until `r`
releases it. Inherited values are resolved from the layers read when the
column opens; a parent changed elsewhere shows the next time it is opened.

Presentation writers own their modelled overlay shape. The view writer
rebuilds the overlay and drops unmodelled keys. The dataset writer preserves
other columns of the same dataset but drops unknown dataset-level siblings of
`columns`. Most definition adapters start from the source table and preserve
unmodelled keys; Groupings has a bare-array definition instead.

Schema definitions and derived-dimension rows are read-only. Only declared
columns open its presentation editor. Groupings always lists slots 1–9,
including empty slots, after its two leading rows, and does not create or
rename slots. Its final selected
dimension cannot be unticked; deletion or reversion handles removing the user
entry.

## The Grouping dialog

Groupings is the one object dialog whose list applies its rows. It is titled
Grouping and opens from `frame::grouping` (`mod+g`), the toolbar's grouping
readout, and `config::groupings`, on the lane of the workspace it was opened
from, with the cursor on that lane's current choice.

The list is the view default (`0`), the lane's ad hoc chain (`*`), and slots
1–9, empty ones included. The first two are not configuration objects: they
are read from the frame, so the list re-derives when the frame changes as
well as when the configuration does, and a `*` row never offers a chain the
lane no longer holds. They exist only in this dialog; an object named `0` or
`*` in another domain is an ordinary object. Each row is one line, and the
row for the lane's choice carries an `active` badge.

| Key | Effect |
|---|---|
| `enter`, a row click | Apply the row and close. On an empty slot or an empty ad hoc row: open its chain field instead. |
| `1`–`9`, `0`, `a` | As `enter` on that slot, the view default, or the ad hoc row, wherever the cursor is. |
| `e`, the row's `edit` control | Open the row's tick-list editor, including for an empty slot. Refused on the view default. |
| `i` | Type an ad hoc chain, seeded with the cursor row's chain (the stored ad hoc chain on the view default or an empty slot). `enter` applies it as the ad hoc chain and closes, even when the seed is untouched. |
| `s`, the row's `save` control | Ask for a slot digit and save the row's chain there. Refused on a row with no chain. |
| `d` | A slot: delete its user entry, after the usual question. The ad hoc row: forget the chain, with no question. Refused on the view default. |
| `r` | A slot: revert to the lower layer. Refused elsewhere. |
| `/` | Filter. `escape` restores the entry query, bare `enter` keeps it, and neither applies a row. |

"Empty" means the frame holds no chain for the slot. A slot the
configuration defines but the reader dropped (an unknown column) therefore
also opens its chain field, where it can be fixed.

The `edit` and `save` controls paint at the right of the cursor row, and of
any slot or ad hoc row under the pointer; the view default has neither.
`save` paints only where the frame holds a chain, the same rows `s` accepts.
A press on a control acts on that row and does not also apply it. While a
question is up (the save prompt or a confirmation), clicks on rows, row
controls, the filter row, and the Back button do nothing.

A chain field opened from the list is the whole visit: `enter` carries the
chain out and closes the dialog, `escape` returns to the list, and `mod+s`
asks for a slot to save the typed chain to. A chain the field refuses (an
unknown name, or an untouched seed naming a column that no longer exists)
keeps the field open with the refusal. A chain field opened with `i` inside
an edit stage returns to that stage's tick list.

Editing a slot writes `groupings.toml` through the pending batch, like every
definitional edit. Editing the ad hoc chain writes no file: each completed
edit sets the lane's ad hoc chain and makes it the choice, so an ad hoc edit
always regroups. Its stage has no `Slot` row, `d` forgets the chain and
returns to the list, and `r` is refused. The chain is checked by the slot
reader, but its refusals name the ad hoc chain (`the ad hoc chain names
'x', which is not a groupable column`) and carry no "not saved" prefix:
nothing was going to be saved.

### Saving to a slot

`s` (list or edit stage) and `mod+s` (a list-opened chain field) put a prompt
in the action bar's place that owns keys and pointer until answered. A digit
or a click on one of its nine buttons chooses the slot; `escape` cancels. A
prompt armed by `mod+s` returns to the chain field on `escape`, with the
typed chain intact. A draft with a blocking error is refused before the
prompt opens: the slot's reader would drop it.

The target is classified against the configuration with the pending batch
folded in, so a slot forked or saved moments ago already counts as the
user's.

| Target slot | Result |
|---|---|
| Empty | Saved. |
| Holds an equal chain | No write. |
| Defined only by a lower layer | Saved as a user-layer fork, announced on the status bar; the inherited value is recorded in `overrides.toml` in the same batch. No question: `r` restores it. |
| Has a user-layer entry with another chain | Asks `y`/`n` first, naming the chain that would be lost as the user layer spells it, even when the reader dropped it for an unknown column. `n` changes nothing. |

After a save the slot is the lane's choice and the dialog closes. A write
the batch refuses leaves the dialog open with the refusal and the frame
unchanged.

A slot write reaches the frame only when its batch is promoted. To activate
a slot on the keystroke that defines or saves it, the dialog first stages the
chain in the frame's slots in memory (`Frame::stage_slot`). The promotion's
reload then rebuilds equal slots and changes nothing. If the write fails, the
reload of the batch's original documents restores the slot as those
documents define it, and the write-failure status is shown. A save over an
inherited slot therefore leaves the lane on that slot with the lower layer's
chain; a slot that was empty disappears, and the lane returns to each view's
own grouping.

## The VolMonitor views dialog

`Domain::VolViews` edits `vol_views.toml` beside the five domains above
(`config::vol_views`, `Edit VolMonitor views…`; `e` on a VolMonitor tile). Its objects are
the watchlist's tabs, which the tile paints and the digits pick in document
order, so three things differ from the other domains.

The browse list keeps the merged document's order
(`Domain::keeps_document_order`) rather than sorting by name, and `shift+j`/`shift+k` on it reorder
(`Domain::reorderable`, VolMonitor views alone): `ObjectDialogState::move_object`
swaps the selected row with its neighbour in derived order — not the filtered
list's, so a filter hiding the tab in between does not make a two-place move
look like one — and `apply::commit_reorder` writes the order. The refusals
(a read-only domain, no user directory, `that is as far as this tab goes`)
are said before the swap, so the list never shows a move nothing can save.
Only the user layer is written, so every shipped tab the move crosses (the
inclusive range where the new and the current order disagree) is forked
first through the ordinary fork path, sidecar entry included, with one notice
(`copied N tabs to your config — r restores each`); a move among the user's
own tabs forks nothing. The forks and the order are one batch
(`apply::queue_document_order`): `geode_core::volviews::reorder` rewrites
the user file in the new order after the forks have landed, and memory
takes the same order (`PendingConfigWrite::order`, one per document, the
latest), so the browse rows show the new order when the batch is applied
and the watcher's later read finds equal documents. The merge
honours the user layer's table order for `vol_views` alone (see
[configuration](configuration.md#layers-and-provenance)); without that rule the
rewritten file would change nothing the tile or the list can see.

The same rewrite follows every batch that puts a tab's table into the user
file — a fork by a field or column edit, a new tab — in the merged order as
it stood before the batch. A keyed write appends a key the file lacks, and
the merge reads the user file's table order as the tab order, so left
appended a tab forked after another would take that tab's place with no
move asked; rewritten, a copy lands at its merged position and a new tab
follows the rest, and only a move changes the order. A rewrite that fails
fails its batch: memory reverts to the batch's starting documents and the
failure is shown, while the keyed writes that already landed stay in the
file, as for any partial write.

The document's `default` key is edited as the tab's `Default` field, a
`Destination::TopLevel` field: its batch key is the field's key, not the
object's name, so the existing keyed write path serves it; on it renders the
tab's name as the key's value, off renders nothing, which the write path
spells as removing the key. The field forks nothing — turning it on for a
shipped tab copies no `[tab]` into the user file. Turning it off on the tab
a lower layer's `default` names is refused before the draft steps
(`Domain::step_refusal`, consulted by the step path ahead of the draft:
`<tab> is the shipped default — make another tab the default instead`): the
user file has no key to remove, so the write would change nothing and the
field would snap back on the next read. A `default` the user layer holds is
the user's to remove. `Domain::document_keys` names such keys;
the browse list never lists them, and deleting the tab the user layer's
`default` names removes the key with it (`document_keys_naming_target`),
only when the tab leaves the configuration altogether — deleting the user's
copy of an overridden tab reveals the lower copy, which the key still rightly
names. The names `default` and `config_version` are reserved
(`Domain::reserved_names`), and names collide case-insensitively
(`Domain::names_collide`), as the document's reader decides duplicates, so
the naming prompt refuses `Vol` beside `vol` with `'Vol' already exists —
open it instead` and a dialog gate never writes a tab the tile would skip.

Every field is `Destination::Doc`: a column's label, group and glyphs live
in the tab's own table, so the domain has no presentation overlay and no
layer badges. A column is a list item carrying its own `[[columns]]` table
(`ListItem::source`), which is what makes the `columns` list copy-and-drop
rather than move: the catalogue is a fixed list of families, so `space`
clones the row into the tab under its column's head (de-duplicated ` ·2`),
`x` drops a member outright and the catalogue keeps the family; `space` on a
member refuses, since a vol column has no hidden state to tick. `enter` on a
member opens the vol column stage (`ColumnDoor::VolColumn`): the family, the
parts it takes, the two glyphs and the label and group, read from the item's
table (`volviews::column_fields`). Each change folds back into that table
(`fold_into_from`, which resolves a dataset family by its catalogue row, so a
dataset keyed by the underlying alone carries no tenor) and renames the row
by its head, and the stage's rows are re-derived from the folded item, so a
family change shows the new family's parts at once and the painted stage is
re-keyed to the new name at revalidation. A `Raw` column — text the
pickers cannot express, or a `[[columns]]` table the reader refuses,
carried as it was read and named by its measure or spark text (`column
<n>` without one) — has one display-only `measure` row, and the writer
puts its table back in its place (`volviews::to_table_from`), so a save
never deletes a hand-written column the reader only warned about; the
reader's warning stands in the draft. The tab's name row is
display-only too: the scaffold has no rename path, and a typed name the
writer could not honour would fork the inherited copy for nothing; a new
tab's row shows the name the prompt committed (`volviews::new_fields`). An
empty
tab writes `columns = []`, which the reader accepts, so a new tab reaches the
file as more than a bare header.

A dialog can open on the tile's own object. `TileContent::config_object`
answers the document and name a tile shows (`vol_views` and the shown tab);
`render::open` asks the focused tile's occupant, keeps the answer only when
it names the dialog's document, and selects that browse row. A tile naming
another document, or no tile, leaves the first row selected. Validation adds
one warning only the dialog can raise: a column over a dataset the live
schema declares but the data engine's launch schema lacks warns `<dataset>
needs a restart to paint` — the catalogue offers such a dataset the moment a
reload brings it, the schema is restart-required, and the write is right,
only early. The launch set is `ObjectDialogState::startup_datasets`, read at
open from the shell's startup `datasets` layers (the baseline restart-required
is measured against, not the builtin layer alone, which would warn falsely
about a desk or user dataset present at launch), and travels on the draft
(`Draft::startup_datasets`) through `validate_against_startup`, the one door
every draft this dialog holds passes through — stage entry and the rebuild
after a failed write's revert — so the warning does not vanish mid-edit. A
dataset the live schema lacks too is the resolver's to refuse; a restart
would not paint it.

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
remove that entry before creating over it.

The frame's scope expression dialog creates a named expression outside the
object dialogs: `mod+s` in any of its modes names the typed text, refusing a
malformed or reserved name and one any layer of `expressions.toml` already
holds (including one queued a moment ago), applies the dialog's own Enter
checks on the text, and queues `[name] expression = "<text>"` for the user
layer of `expressions.toml` through the shared pending batch. In the term editor the name then
replaces the term (see
[input and dialogs](input-and-dialogs.md#frame-expression)). A scope-bar
named chip's click opens the expression's definition in the Scope dialog's
[definition step](input-and-dialogs.md#definition-step).

Delete removes a user-defined object. Revert requires an inherited object to
restore and removes the user's definition and associated view presentation
where present. A presentation-only view override can therefore be reverted
without ever having copied its definition. These object-wide operations are
unavailable inside a Column stage. Removals bypass draft validation errors so an invalid object can still be
removed or reverted.

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
