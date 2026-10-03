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
  default chord: the palette and the `⋯` menu reach them.
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
  row shows again.
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
  so a filter or new values never re-lay the columns.

## Values and grid

Showing a classification, a reload that changes its source column, and
`shift+r` (`classifications::refresh`) send one `DataHandle::distinct` read
keyed by the tile (`QueryKey(tile id)`, live, unscoped) with a fresh tag;
an answer with another tag or column is dropped. A refused read (`Busy`) or
a failed one leaves the map's rows on screen with a header notice
(`values not loaded: … — R retries`); `Stopped` says so without the retry
hint. Keys: the shell's grid motions, `v`/`shift+v` start a row selection,
`escape` ends it. A row press moves the cursor, shift-press extends a
selection, a double-click dispatches `classifications::edit`. `/` narrows
the rows; escape restores the filter in force when the search began.
`:sort <source|label|rows> [asc|desc]` orders the grid (bare `:sort`
restores the default order: unclassified first, then by label); a header
sort control cycles desc → asc → default (`SortOrder::click_cycle`, as
every grid tile's header does). The sort is saved in the session.

## Commands

```sh
cargo test -p geode-classifications
```
