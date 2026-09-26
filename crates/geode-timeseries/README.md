# geode-timeseries

The timeseries tile: fetchable source series and arithmetic expressions plotted
across one or two panes, with statistics, density, and cursor inspection.
Sessions retain the query range and display settings; pan and zoom bounds are
not persisted.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#timeseries).

## Layout

| Module | Holds |
|---|---|
| `core` | Pure model, range, source resolution, request building, chart-model preparation, session conversion, and the action menu's rows. |
| `commands` | The tile-local `:` vocabulary. |
| `tile` | The retained entity, frame observation, verbs, `:` dispatch, focus, and chart cache key. |
| `tile::data` | Fetch submission, series queries, delivery filtering, and flip-barrier staging and promotion. |
| `tile::popups` | Opening, input routing, commits, cancellation, focus, and pointer controls for five transient surfaces. |
| `tile::pointer` | Chart wheel, drag-pan, and split-drag gestures using chart hit testing. |
| `popup` | State and rendering for the series list, add picker, range editor, and action menu, plus expression-editor state. Series and picker rows share a row shell; menu rows, range fields, and the inline expression editor have separate renderers. |
| `header` | Prepared chips and controls, the action-menu button, inline expression field, and empty state. |
| `content` | `TileContent` wrapper, factory, actions, and keymap fragment. |

## Commands

```sh
cargo test -p geode-timeseries
cargo bench -p geode-timeseries
```

## Rules this crate pins

- `Changed` decides which work a mutation causes; do not replace it with a
  blanket query and rebuild.
- Fetch every slot still in `Fetching`; one completed pair may unblock several
  tiles.
- `Ok(0)` fetch completion still triggers a query because coverage is known.
- `ChartKey` contains everything chart preparation reads and excludes cursor
  movement.
- One closer owns every popup and blurs a focused editor before dropping it.
- `:` remains local to this tile.
- Menu picks and empty-state buttons dispatch registered actions. Other pointer
  controls share model operations and change processing with keyboard commands.
- A chart press and a header control never stop propagation, so the shell's
  click-to-focus still runs; popup rows do, because they sit on their own
  occluding surface.
- A pointer gesture ends at the same tail as its key: pan and zoom at
  `view_moved`, a split at `apply_changed`.
