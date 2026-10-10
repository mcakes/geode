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
- `core/rows.rs`: `WatchRow` and `rows(state, pending, reference)`, the
  grid's rows: the snapshot's members, or, with a pending definition (an
  edit awaiting its reload), `resolve_members` re-run over the snapshot's
  rule names with the pending `include` and `exclude`, so a manual change
  shows at once and survives a snapshot that moved on under the same
  definition. Each row carries the reference table's `name` cell
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
  shown one ticked. `tile/table.rs` is the grid's `TableDelegate` over rows
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
[asc|desc]` (a bare column is `asc`); a bare `:sort` restores the default;
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
to undo` / `nothing to redo` until the history lands), then Switch…, then
New…, Clone…, Rename…, Delete… and Revert…, each with its live chord. The
member and object verbs are not built yet: a pick, or their key, shows
`not yet available` as a status notice.

Notices: the shell's `TileNotice`s are taken on every frame notification;
`Forked` shows as status, `Refused` as danger. A verb's notices last until
the next verb or another list is shown; session-restore notices until the
trader's first key or press in the tile. `escape` (`watchlist::cancel`)
peels one layer at a time: a menu first (the surface on top, which may be
acting on the selection), then a live selection, then the warning and
danger notices showing, each as a click on one would; with nothing to
dismiss it is unhandled.

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

- The member verbs (add, remove, undo, redo, refresh, rules) and the
  object verbs are not built yet: each says `not yet available`.
- The pending definition is not wired: `rows()` takes it, the tile passes
  `None` until the history lands.

The behavior as the trader sees it is in
[features](../../docs/current/features.md#watchlists).

## Commands

```sh
cargo test -p geode-watchlist
cargo bench -p geode-watchlist
```
