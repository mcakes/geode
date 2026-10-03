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
- `tile/`: the hosted entity. `tile/header.rs` paints the header: the
  switch control (`Classification: <name> ▾`, also `g c`), the source
  column, the map's size and the winning layer's badge, then the shared
  cluster with `⋯` and ×. The switcher hangs under the name and lists every
  classification alphabetically, the shown one ticked. A tile showing
  nothing (new, or its classification removed by a reload) opens the
  switcher at once and says why in its empty state; with no classification
  defined the empty state names `Classification: New…`.

## Commands

```sh
cargo test -p geode-classifications
```
