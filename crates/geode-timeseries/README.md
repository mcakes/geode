# geode-timeseries

The timeseries tile: fetchable source series and arithmetic expressions plotted
across one or two panes, with statistics, density, cursor inspection, and a
session-persistent viewport.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#timeseries).

## Layout

| Module | Holds |
|---|---|
| `core` | Pure model, range, source resolution, request building, chart-model preparation, and session conversion. |
| `commands` | The tile-local `:` vocabulary. |
| `tile` | The retained entity, frame observation, fetch/query/delivery flow, focus, popup ownership, and chart cache key. |
| `popup` | Series list, add picker, expression editor, and range editor behind one popup state. |
| `header` | Prepared chips and controls. |
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
