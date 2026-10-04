# geode-classifications

The Classifications tile: one derived dimension (`dimensions.toml`) per tile,
every source value with its label. The editing model lives in
`geode_core::classification`; this crate hosts it, and writes only through
the frame's config door.

## Module map

- `content.rs`: `ClassificationsFactory` (kind `classifications`; accepts no
  launch dimension) and `ClassificationsConfig`, the snapshot the app pushes
  on startup and on every reload (derived dimensions, pinned schema, views,
  each classification's winning layer and the ones a user copy shadows).
  `set_config` stores it and calls `config_changed` on every live tile. Also
  the tile's `TileContent` door, `ACTIONS` (category `Classifications`) and
  the `DEFAULT_KEYMAP` fragment. New, rename, delete and revert have no
  default chord: the palette and the `⋯` menu reach them; Export CSV,
  Export CSV with unclassified and Import CSV likewise.
- `core/session.rs`: the session table (`version`, `name`,
  `sort = [column, "asc" | "desc"]`, `cursor`). An unreadable key is dropped
  with a notice and the rest kept.
- `core/grid.rs`: `GridModel`, the grid's pure state: rows from
  `classification::rows`, the sort order, the `/` filter (`Narrow` over
  source and label, order kept, highlights kept per row), a cursor and a
  rows-only selection, both held by source value so a rebuild keeps them on
  their rows. A label blank after trimming is unclassified everywhere
  (counts, sort, paint). Unclassified labels and values not in the data
  sort last in either direction. A sort or filter change ends a selection.
  A restored cursor waits for the values that hold it, then is dropped once
  a successful answer arrives without it (a failed read keeps it waiting).
  A cursor nobody moved keeps its index across a rebuild; a moved one (or
  one a selection starts from) follows its row. A filter hiding a moved
  cursor's row rests it on the nearest shown row and returns it when the
  row shows again; the session saves that hidden row (a waiting restored
  cursor first). After a label verb (`relabelled`) the cursor keeps its
  shown index instead, so labelling the top unclassified row leaves the
  cursor on the next one; the verb ends a selection. A rebuild that removes
  the cursor's row while a selection is live ends the selection.
- `core/history.rs`: `History`, the undo and redo stacks over the shown
  classification and the optimistic object an edit produced. Every verb
  works over `current` (the pending object, else the configuration's), so
  two edits before a reload compose; undo and redo replay row by row over
  it and report rows another surface changed since. Every edit queued
  since the configuration's object (`base`) is in flight until a reload
  carries it. A reload carrying the pending object drops it; one carrying
  the base or an earlier in-flight object (a later edit was queued after
  that write fired, or something else changed) keeps it, so a later label
  does not flash off and the next edit builds on it; any other object is
  another surface's write and drops it, as a refusal does. Showing another classification, or
  a rename, delete or revert, forgets all. `record` takes an object a
  whole plan made (an import) as one entry, so one undo step reverts it;
  an empty entry records nothing.
- `core/files.rs`: `FileOp`, the export or import waiting on its answer
  (tag, classification, path), and the notices' text: `exported`, the
  import question, `nothing_to_change`, `rejected_notice` (the first
  `REJECTED_LISTED` = 20 rows, then `and <k> more`), `imported`, the
  `not applied` refusals, `ready` (a held plan) and `replaced`.
- `core/prompt.rs`: `Prompt` (`NewName`, `NewColumn`, `Rename`) and
  `submit`, which validates the trimmed answer (`validate_name`,
  `validate_source`) into the next `Step`: the column question, a create,
  a rename, or a refusal that keeps the prompt open.
- `tile/`: the hosted entity. `tile/header.rs` paints the header: the
  switch control (`Classification: <name> ▾`, also `g c`), the source
  column, `<n> values` and `<k> unclassified` from the grid, and the winning
  layer's badge, then the shared cluster with `⋯` and ×. The switcher hangs
  under the name and lists every classification alphabetically, the shown
  one ticked. A tile showing nothing (new, or its classification removed by
  a reload) opens the switcher at once and says why in its empty state;
  with no classification defined the empty state names
  `Classification: New…`.
- `tile/table.rs`: the `TableDelegate` over prepared rows: source (headed
  by the source column), label (headed by the classification; `—` muted
  when unclassified, `not in data` beside a map-only value) and `rows`.
  Rows are identified by source value; a press reports `RowPressed`, a
  header sort control `SortClicked`. The wrapper keeps window focus out of
  the table. A dragged column width is recorded in the delegate, and the
  table is refreshed only when a heading, a sort mark or the rem changes,
  so a filter or new values never re-lay the columns. While the label
  editor is open the cursor row's label cell paints its field, with the
  ranked labels hung under it.
- `tile/editor.rs`: `LabelEditor`, the free typeahead over the labels in
  use, and its commit rule: the highlighted label when the highlight was
  moved (`up`/`down`, a row press) or equals the typed text ignoring case;
  else the typed text trimmed, never re-cased; blank clears. The case
  comparison is Unicode lowercasing. The prefill is the targets' label only
  when they all share one. Also `PromptField`, the bar under the header
  that New and Rename ask in; its column step hangs a closed choice over
  `source_columns`, painted by the editor's list painter.

## Values and grid

Showing a classification, a reload that changes its source column, and
`shift+r` (`classifications::refresh`) send one `DataHandle::distinct` read
keyed by the tile (`QueryKey(tile id)`, live, unscoped) with a fresh tag;
an answer with another tag or column is dropped. A refused read (`Busy`) or
a failed one leaves the map's rows on screen with a header notice
(`values not loaded: … — shift+r retries`); `Stopped` says so without the retry
hint. Keys: the shell's grid motions, `v`/`shift+v` start a row selection,
`escape` ends it. A row press moves the cursor, shift-press extends a
selection, a double-click dispatches `classifications::edit`. `/` narrows
the rows; escape restores the filter in force when the search began.
`:sort <source|label|rows> [asc|desc]` orders the grid (bare `:sort`
restores the default order: unclassified first, then by label); a header
sort control cycles desc → asc → default (`SortOrder::click_cycle`, as
every grid tile's header does). The sort is saved in the session.

## Editing labels

The verbs act on the row selection, else the cursor's row. `enter`/`c`
(and a row double-click) open the editor, prefilled and selected; in it
the tile is in `insert` mode, `up`/`down` move the highlight, `enter`
writes, `escape` or a press elsewhere closes it unwritten. `x` clears,
`y y` (or `ctrl+c`) copies the cursor row's label (unclassified copies as a clear), `p`
(or `ctrl+v`) pastes it, `u` undoes and `ctrl+r` redoes. Each write first checks the
classification's source column (`validate_source`): a hand-written one
over a column no classification may map is never written, with a
`not saved:` notice, since the config door writes even when the reload is
then rejected. Otherwise the whole object goes through
`FrameRef::queue_config_edits` and shows at once. Undo that skips rows
changed elsewhere says how many; rows a refused write of the tile's own
touched (`History::unsaved`) are counted apart, as `not saved`. Opening the
editor puts the cursor on its row (`GridModel::put_cursor`), so a rebuild
reordering the rows keeps the field on the row it writes.

A right press on a row (`table::RowContext`, with the pointer's window
position) moves the cursor there, keeping a live selection the row is in,
and opens the `⋯` menu hung from the pointer. While a values read is on its
way the header shows `loading values…` (`HeaderModel::loading`), cleared by
its answer, a failure, or a refusal.

Notices: the shell's `TileNotice`s are taken on every frame notification;
`Forked` shows as status, `Refused` as danger and drops the optimistic
edit. The shell refuses to every tile whose edits joined a batch whose write
failed or whose merge was rejected (kept last good), so a label shown ahead
of the reload never outlives a write that did not land. A verb's notices last until the next verb or another classification
is shown; session-restore notices until the trader's first key or press in
the tile; the switcher's `no classifications to switch to` while that
holds. The switcher opens by itself only on the first snapshot and when
the shown classification goes away.

A warning or danger notice can be dismissed: a click on it, or `escape` in
normal mode (`classifications::cancel`, which first closes a menu and ends
a selection; from the palette under an armed question it answers "no" and
goes no further) when nothing else answers it. Both do the same to the
same notice (`dismiss_notice`): a transient one — a verb's outcome, `g c`'s
`no classifications to switch to`, or what the session restore dropped —
is cleared, so the key repeated says it again; the standing one (the
values notice) is hidden,
not cleared: it stays hidden while `rebuild_chrome` keeps reporting it and
shows again once it stops and returns (`geode_tile::notice::Dismissals`,
pruned in `rebuild_chrome`). Status notices (`nothing copied…`) are never
dismissed. `escape` with nothing to dismiss is unhandled; like any
dispatched verb it still counts as the trader's first action and clears
the session-restore notices.

## New, rename, delete and revert

Registered actions only (the palette and the `⋯` menu), never `:`
commands. The `⋯` menu lists Set, Clear, Copy and Paste label, then New…,
Rename…, Delete, Revert… (only over a lower copy) and Refresh values,
then Export CSV…, Export CSV with unclassified… and Import CSV…;
a row that cannot act says why in its lane and in full when picked.

New asks a name, then a source column from a closed choice (the
highlighted column answers; enter with nothing typed takes the first), and
writes an empty classification. Rename asks the new name (seeded and
selected). Each answer is validated before anything is written; a refusal
shows under the field and keeps it open. Rename, Delete and Revert then ask
y/n on the confirm bar (`geode_tile::confirm`). Rename and Delete name how
many groupings, views, saved scopes and named expressions still name the
classification (`references`): these are not rewritten. Revert names the
lower copy's layer instead; the name stays defined. A rename writes the new object and
removes the old in one `queue_config_edits` batch; delete and revert remove
the user definition. Rename and delete act only on a classification the
user layer owns outright: a desk or builtin one cannot be removed from the
user layer, and removing a user copy over a lower layer's would leave that
copy under the old name (Revert… is the verb for that, and its question
names the lower copy's layer). A classification with no recorded layer is
refused too (`can't tell where <name> is defined`). After a
create or rename the tile shows the new name (`Saving <name>…` until the
reload carries it); after a delete the switcher opens without it. A
`Refused` notice puts back what was shown before. A confirmed revert is
awaited too (`reverting`): until the reload no longer shows a user copy over
the lower one, a label verb or the editor on that classification is refused
with `reverting <name>…`, since an edit built on the user copy would replace
the removal in the shell's batch. A `Refused` notice ends the wait. While the prompt or a
question is up the tile is in `insert` mode; any other verb closes the
prompt or answers the question no.

A verb that changes nothing (`x` on unclassified rows, `u` with nothing to
undo, a replay that skips every row) is not a relabel: it keeps a live
selection and a waiting cursor.

## Export and import

Registered actions only (palette and the `⋯` menu's last group), no
chord, never `:` commands. The tile does no file I/O: both go through
`DataHandle::text_file` under the tile's key and a fresh `file_tag`, and
`on_file` acts only on the answer to the waiting `FileOp` (an overtaken
one is dropped). A `Refusal` shows as a notice (`… — try again` when busy).

Export opens `prompt_for_new_path` on `<name>.csv` in `file_dir` (the
folder of the last file exported or imported, else the home directory).
The answer is awaited in `cx.spawn`; `export_to` re-checks that the same
classification is shown and, for the unclassified variant, that the values
are loaded and current (no read on its way, no values notice), then
writes `classification::export` over `History::current`, so an unsaved
edit is in the file.

Import opens `prompt_for_paths` (one file) and reads with
`TextFileOp::Read { max_bytes: MAX_IMPORT_BYTES }`. The read text is
planned by `plan_import` on the background executor over a copy of the
current object; `import_planned` drops a plan another file operation
overtook and refuses one landing after a switch or a source-column change.
Arming a confirm takes focus, so the question is armed only while
`keyboard_free` holds: the tile is the focused tile (`set_focused`, kept
from the shell's render, never notifying) with no `/` search, editor,
prompt or question open, and window focus is nothing or contains the
table (the shell root, not a palette or another input). Otherwise the
plan waits in the one `held` slot (`HeldImport`, a newer plan replacing
it) with `import of <file> ready — focus this tile to answer`.
`offer_held_later` defers `offer_held` past the current update (the shell
calls `set_focused` mid-draw) when the tile gains focus, a search ends, the
editor or prompt closes, or a question is answered; `offer_held` re-checks
the tag, classification and source column (refusing with the same notices)
before arming. A newer export or import overtaking an import at any step
(read, plan, held) says `import of <file> replaced by a newer file
operation` (`overtake_import`). A refused file (header, size, parse) shows `import refused:
…`; rejected rows show one warning; a plan changing nothing says so and
asks nothing. Otherwise the confirm bar holds `Pending::Import` with the
plan. `y` (`apply_import`) re-checks the classification and its source
column, refuses while reverting and when `source_writable` fails, applies
the plan over `History::current` (counted rows only; a row already equal is
skipped, so the applied count can differ from the question's), `record`s it
and queues one `ConfigEdit`. The apply runs on the UI thread.

## Known limitations

- `distinct` skips computed datasets, so a column only a computed dataset
  carries shows its mapped values alone.
- Rename and delete do not rewrite references; `references` misses ad hoc
  lane chains, pricer views and view sort keys.
- The open dialog does not start in `file_dir`.
- A held import is offered again only on the tile's own transitions; a
  palette or command line closing over the focused tile does not re-offer
  it until the tile is refocused or an input of its own closes.
- Applying an import at `y` runs on the UI thread (about 28 ms at 100,000
  changed rows).

The behavior as the trader sees it is in
[features](../../docs/current/features.md#classifications).

## Commands

```sh
cargo test -p geode-classifications
```
