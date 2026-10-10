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
- `tile/`: the hosted entity. It observes `WatchlistGlobal` (the lists),
  `ReferenceGlobal` (reference names), `AppClock` (the `as of` time), the
  frame (the shell's word on its config writes) and `Chords` (menu hints).
  `tile/header.rs` paints the header: the switch control
  (`Watchlist: <name> ▾`, also `g w`), `<n> names` (live members), `<k>
  rules` (warning tone while any rule is bad), the resolution state
  (`resolving…`; `as of <time>` on the display clock; `failed` in the
  warning tone after a failed resolution, with the last good time), the
  winning layer's badge, then the shared cluster with `⋯` and ×. The
  switcher hangs under the name and lists every list alphabetically, the
  shown one ticked.

## Showing a list

A tile showing nothing (new, or its list removed by a snapshot change)
opens the switcher at once and says why in its empty state (`no watchlist
shown — g w switches`, or `<name> no longer exists — g w switches`); with
no list defined the empty state names `Watchlist: New…` instead and the
switcher refuses with `no watchlists to switch to`. The switcher opens by
itself only on the first snapshot that holds lists (a restored tile is
built before the bridge publishes them) and when the shown list goes away;
a snapshot change that leaves a nothing-shown tile as it was keeps a
closed switcher closed.

The `⋯` menu (`.`) lists Switch, then New…, Clone…, Rename…, Delete… and
Revert…, each with its live chord. The object verbs are not built yet: a
pick shows `not yet available` as a status notice.

Notices: the shell's `TileNotice`s are taken on every frame notification;
`Forked` shows as status, `Refused` as danger. A verb's notices last until
the next verb or another list is shown; session-restore notices until the
trader's first key or press in the tile. `escape` in normal mode
(`watchlist::cancel`) first closes a menu, else dismisses the warning and
danger notices showing, as a click on one would; with nothing to dismiss
it is unhandled.

## Known limitations

- No grid yet: a shown list paints its header over an empty body. `/`,
  `:sort` and the member verbs land with the grid.

The behavior as the trader sees it is in
[features](../../docs/current/features.md#watchlists).

## Commands

```sh
cargo test -p geode-watchlist
```
