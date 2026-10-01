# geode-chart

Reusable chart preparation and painting: a kit and one element per chart type.
The kit knows axes, scales, panes, the view window, ticks, mark geometry and
the chrome every chart paints. Each element owns its model and what it paints
inside a pane. Series identities, sources, tiles, and shell state belong to the
caller.

The time chart's use in the timeseries module:
[`docs/current/features.md`](../../docs/current/features.md#timeseries).

## Layout

| Module | Holds |
|---|---|
| `core` | Window-free geometry and labels: y scales and 1-2-5 ticks, axes and panes, layout, the view window, the time x scale with its ticks and crosshair, the linear x scale with its formats and ticks (`linear`), dash and point-mark stroke geometry (`marks`), hit testing, decimation, and theme-derived palette values. |
| `paint` | What elements share once they have a window: rebuild counters, a pane side's resolved y axis, the pane frame (grid and y axes), the x-axis painter, stroke builders, and the stroke ceiling `MAX_STROKE_SEGMENTS`. |
| `timeseries` | `ChartModel` and `ChartSlot`, the immutable prepared input, and `ChartElement`: polylines over a session or continuous time axis, with percentile rules and density bars. |
| `xy` | `XyModel` and `XySlot`, the immutable prepared input, and `XyElement`: lines, solid or dashed, and point marks with a range bar, over a linear x axis that can run reversed. |

Both elements paint through gpui-component's `Plot`, in up to two panes with
four y axes, with cached data paths and chrome.

## Commands

```sh
cargo test -p geode-chart
cargo bench -p geode-chart
cargo run -p geode-chart --example chart
cargo run -p geode-chart --example xy
```

## Contracts

### The kit and its elements

- There is one element per chart type, never one per module. An element
  borrows the kit and owns its model and its paint; a module that needs a
  chart prepares that element's model. A mechanism two elements would
  otherwise each write (a scale, a tick rule, a stroke builder, the pane
  frame) lives in the kit.
- Every model change must advance the model's `version`. Cache keys rely on
  this version rather than hashing all values and presentation fields.
  A chart also needs a stable element ID unique in its window so its caches
  cannot collide with another chart's state.
- Unchanged paints reuse scales, ticks, labels and tessellated paths.
  Cursor-only changes do not invalidate them. Painting still allocates for
  translated path copies, component vectors and tooltip readouts.
- Both panes share the same x range; each y axis scales from its visible
  slots' values within that range, padded by a twentieth of their span.
- Decimation preserves each pixel column's extrema in input order. Each
  finite run can contribute two points per column; missing-value gaps can
  increase output beyond two points per column.
- A `View` carries the narrowest span a zoom may reach, in its own units.
  `View::full` takes two units, which suits bucket indexes and microseconds;
  `View::with_min_span` takes the caller's, and a minimum that is not a
  positive finite number means no floor. `reset` keeps the view's minimum.
- One stroke path holds at most `MAX_STROKE_SEGMENTS` segments. Past gpui's
  own vertex limit a path fails to build and the shape is absent, not
  truncated; the ceiling sits under that limit, and a caller of
  `stroke_segments` bounds what it passes.
- `core::marks::dash_polyline` places only the dashes inside a clip rectangle,
  so a span far longer than the plot costs no more than the plot's own
  dashes. The pattern's phase runs on round corners and through a span's
  clipped-off part, and restarts after a break. The line is solid when the
  dash or the gap is not a positive number, when the period is not finite, and
  when the period is under one pixel. Along a span too long for `f64` to
  resolve the pattern, the part inside the clip is one solid segment.
- Callers use `Palette` to resolve series colors from the theme and pass the
  rem scale to the element. An element reads theme tokens for its grid and
  axes; it never reads module or shell state.

### `timeseries`

- Callers supply ascending epoch-microsecond buckets and equally sized slot
  value arrays. Missing values use `NaN`; non-finite values break polylines.
  Percentiles and density bins arrive computed by the caller, as does the
  model's clock offset.
- Session axes place buckets at equal intervals and close gaps in time.
  Continuous axes preserve elapsed time.
- At most eight percentile rules per slot are painted. Rules outside their
  pane's current y domain are skipped along with their labels.
- `MAX_DENSITY_QUADS` limits density bars across both panes of one chart
  paint. The upper pane is visited first, then the lower pane, with slots
  visited in model order. Bars beyond the limit are omitted. The element reads
  a theme token for the density-strip background.

### `xy`

- A slot is a line or a set of points, and carries its own x values: curves
  and quoted points sit on different grids. `XyModel::new` normalises every
  slot: its arrays are cut to their shared length, a point whose x is not
  finite is dropped, and points out of x order are sorted into it, equal xs
  keeping the order they came in. A slot edited after construction must keep
  its xs finite and ascending, because the window, the nearest point and a
  line's value are binary searches over them.
- `XyModel::full()` is the x range of the slots visible at construction, and
  `(0, 0)` when there are none. A range of one x gives a view with no span,
  which paints every point at the plot's left edge and has no x ticks; a
  caller that wants a single x in the middle of the plot pads the range.
- A line is a polyline through its points in x order; a `NaN` y is a gap. Its
  window reaches one knot past each edge of the view, so the line runs to the
  plot's edges. A window of fewer than two knots is empty: the line paints
  nothing and adds nothing to its axis's domain.
- A point paints a diamond at its mid when the mid is finite, and a vertical
  bar over its range when the range's two ends are finite and apart. A point
  with neither paints nothing. A points slot's window is the points inside
  the view, edges included.
- Each visible slot the view shows something of is one cached stroke path.
  The path key holds the model version, slot number, pane, view, plot
  geometry and the rem, which sizes dashes and markers. Data is clipped to
  its pane's plot.
- Past the stroke ceiling a slot degrades instead of vanishing: a points slot
  with more marks than one path holds paints an even stride of its points and
  the last, and a dashed line with more dashes than that is stroked solid.
  The crosshair and the tooltip still read every point of a thinned slot,
  including those it does not paint.
- `XAxis::reversed` runs the axis right to left. Ticks still come out in
  ascending pixel order.
- Linear x ticks are 1-2-5 steps, labelled with the decimals the step needs;
  `XFormat::Fixed(n)` takes at least `n` decimals. A delta axis instead labels
  the trader's ladder (`5p 10p 25p 50 25c 10c 5c`) in whole percents, keeping
  rungs in priority order (50, the 25s, the 10s, the 5s) so those kept sit at
  least one tick gap apart. With fewer than three rungs kept it falls back to 1-2-5 ticks,
  labelled as deltas with up to two decimals of a percent. A tick at zero
  never carries a minus sign.
- Each y axis reads its own `YFormat`; `Percent` shows a ratio as a percent.
  A pane side whose slots have no finite value in view has no scale, and
  those slots do not paint.
- Inside either plot the crosshair sits on a quoted point when one is within
  the snap radius of the cursor along x, and otherwise follows the cursor.
  The candidates are the points of the visible points slots of both panes
  that the view holds and that have a mark (a finite mid, or a range with
  height); the nearest in pixels wins. A line's knots are never snapped to. In a view with
  no span the crosshair sits on the one x that is painted.
- The tooltip has one row per visible slot. A line reads between its knots,
  and a dash outside its own range or across a gap. A points slot reads only
  a point in view within one pixel column of the crosshair: `mid  lo / hi`,
  the mid alone when it has no range, `—  lo / hi` when it has a range and no
  mid, and a dash otherwise. The title is x in the axis's format, at least as
  fine as one pixel column (the column asks for six decimals at most); a
  delta title has at least one decimal. No readout or title is a zero with a
  minus sign.
