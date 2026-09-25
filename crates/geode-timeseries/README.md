# geode-timeseries

The timeseries tile: fetchable source series and arithmetic expressions plotted
across one or two panes, with statistics, density, cursor inspection, and a
session-persistent viewport.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#timeseries).

## Layout

| Module | Holds |
|---|---|
| `core` | Pure model, range, source resolution, request building, chart-model preparation, session conversion, and the action menu's rows. |
| `commands` | The tile-local `:` vocabulary. |
| `tile` | The retained entity, frame observation, verbs, `:` dispatch, focus, and chart cache key. |
| `tile::data` | Deliveries, the fetch of waiting pairs, the query, and flip-barrier staging and promotion. |
| `tile::popups` | Opening, keying, committing, and closing the five popups, plus the chip, swatch, and frequency-chip doors. |
| `tile::pointer` | The chart surface's wheel, drag-pan, and split-drag gestures over the chart's own hit-test. |
| `popup` | Popup state and painting: series list, add picker, expression editor, range editor, and action menu, whose list rows share one row shell. |
| `header` | Prepared chips and controls, the `⋯` button, and the empty state. |
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
- A pointer door dispatches the verb's own action id; it never mutates the
  model on a path its key does not take.
- A chart press and a header control never stop propagation, so the shell's
  click-to-focus still runs; popup rows do, because they sit on their own
  occluding surface.
- A pointer gesture ends at the same tail as its key: pan and zoom at
  `view_moved`, a split at `apply_changed`.
