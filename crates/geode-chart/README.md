# geode-chart

Reusable chart preparation and painting for the timeseries module. This crate
knows axes, scales, panes, paths, supplied statistics, and interaction geometry.
Series identities, sources, tiles, and shell state belong to the caller.

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
cargo run -p geode-chart --example chart
```

## Contracts

- Callers supply ascending epoch-microsecond buckets and equally sized slot
  value arrays. Missing values use `NaN`; non-finite values break polylines.
  Percentiles and density bins arrive computed by the caller.
- Every model change must advance `ChartModel::version`. Cache keys rely on
  this version rather than hashing all values and presentation fields.
  A chart also needs a stable element ID unique in its window so its caches
  cannot collide with another chart's state.
- Session axes place buckets at equal intervals and close gaps in time.
  Continuous axes preserve elapsed time. Both panes share the same x range;
  each y axis scales from its visible slots' values within that range.
- Decimation preserves each pixel column's extrema in input order. Each
  finite run can contribute two points per column; missing-value gaps can
  increase output beyond two points per column.
- Unchanged paints reuse scales, ticks, labels and tessellated paths.
  Cursor-only changes do not invalidate them. Painting still allocates for
  translated path copies, component vectors and tooltip readouts.
- At most eight percentile rules per slot are painted. Rules outside their
  pane's current y domain are skipped along with their labels.
- `MAX_DENSITY_QUADS` limits density bars across both panes of one chart
  paint. The upper pane is visited first, then the lower pane, with slots
  visited in model order. Bars beyond the limit are omitted.
- Callers use `Palette` to resolve series colours from the theme and set the
  model's clock offset, and pass the rem scale to the element. The element
  reads theme tokens for its grid, axes and density-strip background; it
  never reads module or shell state.
