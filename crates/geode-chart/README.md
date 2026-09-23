# geode-chart

Reusable chart preparation and painting for the timeseries module. This crate
knows axes, scales, panes, paths, statistics, and interaction geometry; it does
not know series identities, sources, tiles, or the shell.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#timeseries).

## Layout

| Module | Holds |
|---|---|
| `core` | Pure scales, axes, layout, time ticks, viewport, crosshair, decimation, and theme-derived palette values. |
| `model` | `ChartModel`, the immutable prepared input, and one `ChartSlot` per plotted series. |
| `element` | `ChartElement`, painting through gpui-component's `Plot` with cached data paths and chrome. |

## Commands

```sh
cargo test -p geode-chart
cargo bench -p geode-chart
```

## Rules this crate pins

- Geometry and decimation are pure and operate over slices.
- Missing values remain gaps rather than being drawn as zero.
- Density painting is bounded by `MAX_DENSITY_QUADS`.
- A cursor-only change does not rebuild paths or the chart model.
- Theme colors are resolved into `Palette`; paint does not reach through to
  module or shell state.
