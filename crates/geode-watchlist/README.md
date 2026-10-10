# geode-watchlist

The Watchlist tile: one named list of underlyings (`watchlists.toml`) per
tile. The list model lives in `geode_core::watchlist`; the resolved
members arrive through `geode_shell::watchlist::WatchlistGlobal`, which the
app's bridge replaces on every change; this crate hosts the editor and
writes only through the frame's config door. The tile issues no data
request and never depends on `geode-data`.

## Module map

- `content.rs`: `WatchlistFactory` (kind `watchlist`; accepts no launch
  dimension; no data handle) and `WatchlistConfig`, the snapshot the app
  pushes on startup and on every reload (the schema, derived dimensions,
  saved scopes and named expressions a rule is validated against).
  `set_config` stores it and calls `config_changed` on every live tile;
  `set_refresh` installs the bridge's cache refresh, by list name, as the
  tile's `shift+r` route (the factory's hook is the tile's only route to
  the bridge). Also the tile's `TileContent` door, `ACTIONS` (category
  `Watchlist`) and the `DEFAULT_KEYMAP` fragment (contexts `normal`,
  `visual`, `insert`, `menu` and `rules`). New, Clone, Rename, Delete,
  Revert and Rules… have no default chord: the palette and the `⋯` menu
  reach them.
- `core/session.rs`: the session table (`version`, `name`,
  `sort = [column, "asc" | "desc"]` over `name`, `origin` or `reference`,
  `cursor` holding a member name). An unreadable key is dropped with a
  notice and the rest kept.
- `core/history.rs`: `History`, the shown list's undo and redo stacks over
  `geode_core::watchlist::edit` entries and its optimistic pending object:
  every verb works over `current` (the pending object while one awaits
  its reload, else the snapshot's definition), so two quick edits compose;
  `reloaded` applies the in-flight chain rule (a reload equal to the
  pending object or carrying another surface's change drops it; one equal
  to the base or to an earlier in-flight object keeps the later edits);
  `refused` drops it and remembers the names a refused write would have
  changed, so an undo that skips them says `not saved` rather than
  `changed elsewhere`.
- `core/prompt.rs`: `Prompt` (what the field asks: `AddName`) and
  `submit`, the typed answer's step: a trimmed name to add, or a refusal
  (`type a name` for a blank).
- `core/rows.rs`: `WatchRow`, `members(state, pending)` and
  `rows(state, pending, reference)`, the members and the grid's rows: the
  snapshot's members, or, with a pending definition (an edit awaiting its
  reload), `resolve_members` re-run over the snapshot's rule names with
  the pending `include` and `exclude`, so a manual change shows at once
  and survives a snapshot that moved on under the same definition; the
  verbs read the same origins (`x` on a name added a moment ago sees it as
  manual). Each row carries the reference table's `name` cell
  (`underlyings`, `None` when absent or NULL), whether the table holds the
  key at all (`in_reference`), and `pending` when its manual state differs
  between the two definitions. `origin_text` spells the origin column:
  `manual`, `rule 1`, `rules 1, 3`, `manual + rule 2`, `excluded (rule 1)`,
  `excluded (manual)`, `excluded` for an exclusion nothing supplies, with
  ` · pending` while the row awaits its reload. Rules are numbered from 1.
- `core/grid.rs`: `GridModel` over `WatchRow`s: the sort order, the `/`
  filter (fuzzy over the name and the reference name, keeping the order),
  a cursor and a row selection held by name so a rebuild keeps them on
  their rows; `after_verb(rows)` keeps the cursor's shown index instead
  (the next row lands under it) and ends the selection; `counts()` is
  `(live, excluded)` over every row whatever the filter shows. The default
  order is by name with excluded rows last; `Origin` sorts by its text,
  `Reference` by the reference name with a missing one last either way,
  ties keep the default order. A restored cursor is a seed that waits for
  the snapshot holding its row.
- `tile/`: the hosted entity. It observes `WatchlistGlobal` (the lists),
  `ReferenceGlobal` (reference names), `AppClock` (the `as of` time), the
  frame (the shell's word on its config writes) and `Chords` (menu hints).
  `tile/header.rs` paints the header: the switch control
  (`Watchlist: <name> ▾`, also the switch chord, `g w` by default),
  `<n> names` (the grid's live count), `<k> rules` (warning tone while any
  rule is bad), the resolution state
  (`resolving…`; `as of <time>` on the display clock; `failed` in the
  warning tone after a failed resolution, with the last good time), the
  winning layer's badge, then the shared cluster with `⋯` and ×. The
  switcher hangs under the name and lists every list alphabetically, the
  shown one ticked. `tile/field.rs` is the prompt field: a bar under
  the header with the typeahead hung from it, its commit rule
  (`answer_value`: the highlighted name when the highlight was moved or
  typed out in full ignoring case, else the text as typed) and its paint;
  the tile owns its lifetime, focus and writes. `tile/table.rs` is the
  grid's `TableDelegate` over rows
  the tile prepares (`Prepared::build`) whenever the snapshot, the
  reference tables, the shown list, the sort or the filter changes; render
  reads it only. It reports a row press, a right press and a header sort
  click as events the tile answers.

## Showing a list

A tile showing nothing (new, or its list removed by a snapshot change)
opens the switcher at once and says why in its empty state (`no watchlist
shown — <chord> switches`, or `<name> no longer exists — <chord> switches`,
naming the switch chord as the keymap binds it, `g w` by default, or the
palette title `Watchlist: Switch` when it binds none); with no list defined
the empty state names `Watchlist: New…` instead and the switcher refuses
with `no watchlists to switch to`. The switcher opens by
itself only on the first snapshot that holds lists (a restored tile is
built before the bridge publishes them) and when the shown list goes away;
a snapshot change that leaves a nothing-shown tile as it was keeps a
closed switcher closed.

## The grid

Columns: `name` (with a muted `not in reference` mark beside a name the
reference table does not hold), `reference` (the table's `name` cell,
blank when absent or NULL) and `origin` (muted). Excluded rows paint in
the muted text tone throughout. A list with no members paints the grid's
empty state (`No names`); a filter that keeps nothing says
`No matching names`.

Order: by name, excluded rows last. `:sort <name|origin|reference>
[asc|desc]` (a bare column is `asc`); `origin` sorts by its text as shown
(`rule 10` before `rule 2`; the ` · pending` suffix participates); a bare
`:sort` restores the default;
the header's sort control cycles desc → asc → default as every grid
tile's does (`SortOrder::click_cycle`), another column starting at desc.
The sort is the tile's and survives a switch; the session saves it.

`/` narrows over the name and the reference name as the query is typed,
keeping the order, with each word's match marked in its column; enter
keeps the filter, escape restores the one in force when the search began.
The counts in the header ignore the filter. A filter hiding the cursor's
row rests the cursor on the nearest shown row and returns it when the row
shows again; the session saves the trader's row meanwhile.

Cursor and selection: the shared grid motions (`j`/`k`, counts, `g g`,
`shift+g`, the page keys) move a cursor held by name, so a snapshot or
reference change keeps it on its row; a cursor nobody has moved keeps its
index. `v`/`shift+v` start a row selection at the cursor (a bare step then
clamps rather than wrap past the anchor); a sort, a filter, or a rebuild
that removes the cursor's row or the anchor's ends it. A row press moves
the cursor, shift-press extends a selection to it; a double-click is two
presses and nothing more. A right press moves the cursor there (a row of
a live selection keeps the selection) and opens the `⋯` menu hung from
the pointer. The session saves the cursor's name; a restored cursor waits
for the snapshot that holds its row.

The `⋯` menu (`.`) lists Add name, Remove name (disabled with `no row`
while no row is under the cursor), Rules…, Resolve now (each disabled with
`no watchlist shown` while none is), Undo and Redo (disabled with `nothing
to undo` / `nothing to redo` while the history has nothing that way), then
Switch…, then New…, Clone…, Rename…, Delete… and Revert…, each with its
live chord. Rules… and the object verbs are not built yet: a pick, or
their key, shows `not yet available` as a status notice.

## Editing members

Every edit is the whole object written through the frame's config door
(`ConfigEdit { doc: watchlists, object: <name>, value: to_toml(next) }`;
the frame stamps the tile as its origin) and shown at once: the history
holds the next object as pending, `rows()` re-derives the grid over it (a
pending manual add paints `manual · pending`, a pending exclusion muted),
and the snapshot carrying the write drops the copy. A reload carrying the
tile's own earlier write keeps the later ones (the in-flight chain); a
reload carrying any other change is the truth and drops them; the shell's
`Refused` notice drops the pending object and replaces the verb's word.

`o`/`enter` (`watchlist::add`) opens the add field, a free typeahead over
the reference table's keys and every list's names, sorted; a name already
a member is not hidden but refused at commit, naming where it comes from
(`DAX is already here from rule 1`), with the field left open under the
reason. `up`/`down` move the highlight; `enter` commits (the highlighted
name when the highlight was moved or equals the typed text ignoring case,
else the text as typed); `escape`, a press on the grid or any other verb
closes it unwritten; the field is the keyboard owner (`insert` mode)
while open and is blurred before it is dropped. The verb says `added
<name>`, or `restored <name>` when the name was excluded.

`x` (`watchlist::remove`) acts on the selection, else the cursor's row,
by origin: a manual name leaves `include` (`removed <name>`), a
rule-supplied one is excluded (`excluded <name> — rule <i> still supplies
it; x again restores`), one that is both does both in one write (`removed
and excluded <name>`), an excluded one is restored (`restored <name>`); a
selection is counted (`removed 2 names, excluded 1 name`); with nothing to
change it says `nothing to remove`. The cursor keeps its shown index and a
selection ends.

`u`/`ctrl+r` replay the history one step over the current object (`undid
1 change`, `redid 2 changes`). A change another surface made since is
skipped (`— 1 changed elsewhere`); one left by the tile's own refused
write is `— 1 not saved`; a replay that skipped everything writes nothing.
Undo puts a name back by hand, so it lands at the end of `include`: the
object is restored, not the file's order.

`shift+r` (`watchlist::refresh`) calls the factory's refresh hook with
the shown name; the header reads `resolving…` with the next snapshot. A
tile hosted without the hook says `resolve now is not wired`. Showing
another list forgets the history and closes an open field; a snapshot
that removes the shown list does the same, and writes nothing.

Notices, in order of precedence: the verb's own word (or the shell's
`TileNotice` about its write, taken on every frame notification: `Forked`
as status, `Refused` as danger), then the standing resolution notices from
the snapshot (`not resolved: <error> — shift+r retries` as danger after a
failed resolution; `rule <i> failed: <reason> — shift+r retries` as a
warning per bad rule), then the session-restore notices. A verb's notices
last until the next verb or another list is shown; the standing ones
until the next resolution changes them, and dismissed they hide until
their text changes; session-restore notices until the trader's first key
or press in the tile. `escape` (`watchlist::cancel`) peels one layer at a
time: an open field first (it owns the keys), then a menu (the surface on
top, which may be acting on the selection), then a live selection, then
the warning and danger notices showing, each as a click on one would;
with nothing to dismiss it is unhandled.

## Performance

`benches/grid.rs` measures the rebuild one member edit costs on the UI
thread before table preparation and paint, over 5,000 names from three
rules with a hundred manual includes and fifty exclusions, an origin sort
and a `/` filter active: `rows::rows` with a pending definition, then
`GridModel::after_verb`. Reference (2026-10-10, loaded machine, load
average 35 to 48; the same run read 6.0 and 12.9 ms under load 17 to 78):

| Bench | Result |
|---|---|
| `watchlist_rebuild_after_edit_5k` | 4.0 ms |
| `watchlist_rows_and_grid_5k` (no pending definition) | 1.7 ms |

## Known limitations

- Rules… and the object verbs (new, clone, rename, delete, revert) are
  not built yet: each says `not yet available`.
- The add field's typeahead offers names only: the reference table's keys
  and the names any list already holds.
- The add field's choice list does not close on a press outside it (a
  press on the field itself is outside the list); a press on the grid, or
  any verb, closes the field.

The behavior as the trader sees it is in
[features](../../docs/current/features.md#watchlists).

## Commands

```sh
cargo test -p geode-watchlist
cargo bench -p geode-watchlist
```
