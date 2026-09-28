# geode-blotter

Configured views rendered as collapsible, keyboard-driven hierarchies.
Header markers identify grouping pins, scope overrides, filters, and as-of
requests. Non-attributable measures remain NULL instead of displaying a
misleading total.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#blotter).

## Layout

| Module | Holds |
|---|---|
| `core` | Column plans, expansion paths, visible-row traversal, cursor movement, find, selection summaries, cursor-row launch context, TSV export, command parsing, and visible-window formatting without GPUI. |
| `delegate` | `TableDelegate` adapter with prepared rows and cached cell text. Holds `:autosize`'s fitted widths by column name, which `column()` prefers over the plan's; `fit_columns` measures the header and the format cache's window only. Owns selection and paint caches; reports cell gestures and chevron clicks to the tile. |
| `tile` | `BlotterTile`, the entity per tile: local query overrides, requests through `DataHandle`, frame observation over `geode_tile::following`, snapshot application, header and footer rendering. The header notice is a `geode_tile::notice::Notice`: dropped sorts and selections are warnings, query and configuration failures danger. |
| `content` | The `TileContent` wrapper and `BlotterFactory`, the roster entry the app builds with the data handle. |
| `colour_cache` | Caches each named color's base and sign variants until theme inputs or definitions change. |

## Commands

```sh
cargo test -p geode-blotter
cargo bench -p geode-blotter   # the pure core
```

## Invariants

- `:autosize` and the shell's `tile::autosize_columns` run one method,
  `BlotterTile::autosize_columns`. It measures only rows in the format
  cache (the window the table last asked for), never the whole snapshot. A
  wider value outside that window does not widen the column. With no plan
  or no cached rows, a fit refuses with "nothing loaded to fit" and keeps
  its widths.
- A view switch clears the fitted widths, and a restored record whose view
  is gone starts without them. `apply_snapshot` drops only the tree column's
  width (key `""`) when the grouping differs from the plan's. That method is
  the one place every grouping change reaches the delegate. The session
  record keeps the widths under `column_widths`.
- A fitted width overrides the view's `presentation.width`, including one
  changed later, until `:autosize reset` or a refit.
- `geode_blotter::init` overrides the table's navigation bindings with
  `NoAction` after component initialization. Row clicks can briefly focus
  the table; these overrides keep its component actions inactive while
  the shell owns blotter key routing.
- An ungrouped dimension displays its common value when every contributing
  row agrees. Disagreement, including NULL alongside a value, displays muted
  `mixed`; all-NULL or absent rows display blank. `Snapshot::is_mixed_at`
  distinguishes mixed cells from other NULL cells. Sorting puts values, mixed,
  then blanks in both directions; yank preserves the marker. Numeric dimensions
  sort numerically and display without measure scaling or rounding.
- A `NonAttributable` cell is NULL. Read numeric columns only through
  `f64_at`/`f64_value`; the format cache is the one place a cell becomes
  text.
- The query runs on `geode_tile::following`. `Followed` names the counters
  an answer depends on (scope unless unscoped, grouping unless pinned, as-of
  unless pinned, always watched data and configuration); it decides both
  requery and promotion, so the two agree. `flip` is never a requery input.
  Tile-local requeries clear the stage because they move no frame counter.
  An unresolved named scope arrives at the barrier but keeps what it acted
  on (`Unanswered::KeepActed`): the configuration change that defines the
  name is the retry. A view the configuration no longer defines takes the
  same path: it answers the barrier at once, supersedes the query still out
  for the old view, and the reload that restores the view is the retry.
- Hiding cancels nothing: an in-flight view query's reply applies while
  hidden, unless a followed counter moved since it asked
  (`Delivered::Superseded`: dropped, not applied, not an arrival; the reshow
  asks again). `closed` (removal) cancels the query by key and answers any
  open barrier still waiting on the tile.
- A chevron click and a row double-click are `space`: both go through
  `expand_at_cursor`, the path `zo`/`zc`/`za` take. The chevron listener
  stops propagation and ignores `click_count() > 1`.
- `v` selects a cell block and `V` selects whole rows. Repeating the active
  kind clears the selection; switching kind keeps its anchor. Anchors use
  row paths and column names across sorting, column moves, and redelivery.
  Losing the anchor row, or a block's anchor column, clears the selection
  with a notice. The compatibility action `blotter::visual` resolves to
  `blotter::visual_rows` with a warning.
- Selection summaries include only the selected rows without a selected
  ancestor, avoiding double-counted group totals. Only compiler-marked
  summable columns with additive values produce a footer total; `†` marks
  non-additive values and `‡` marks unsummable columns.
- `g m` opens another module using the cursor row's underlying. The grouping
  must contain `underlying_ref`, and the cursor must be at or below its level
  with a non-NULL value. A visual selection does not change the launch context.
- `tile_columns` reports the plan's non-tree columns and the cursor's column
  for the shell's edit-column actions; the tree column is never active, and
  derived view columns are flagged so Schema can leave them out.
- Frame scope names resolve against current expression definitions before a
  query is submitted. Missing names show an error, invalidate older pending
  results, and release the tile's flip-barrier wait. Updating definitions
  triggers a retry; unscoped tiles use only their local filter.
- Every mouse selection gesture reaches the tile as a `CellPointer`, and
  only through `pointer`. A cell or gutter press records `drag_origin`; the
  row's own mouse-down (`render_tr`) reports a press at the cursor's column
  only when no cell caught it, so a click on the filler beside the cells is
  a plain click too. The table's `SelectRow` never touches the selection:
  it arrives on mouse-up after the press already moved the cursor, and the
  keyboard's echo carries the cursor's own row.
- Summary totals, the `rows × cols` extent, and the `†`/`‡` legend flags
  are prepared in `refresh_selection`; render only reads them. The strip's
  per-column colors (header color for the label, the cells' sign colors
  for totals) are memoized by `ensure_summary_paint` per summary
  generation and theme, with invalidation when named definitions change.
  An unchanged stamp reuses the prepared footer colors.
- `apply_snapshot` rebuilds the column plan on every delivery and swaps on
  inequality: labels, widths, formats, and colors can change even when
  column names and indices stay the same.
- Sorts store column names. `SortSpec.column` is resolved against the fresh
  plan on every rebuild in `apply_snapshot`. The cursor stores a position;
  rebuilds and `move_column` preserve its column by resolving the old column
  name in the new plan. If that column disappears, the cursor falls back to
  clamping its previous position. A removed sort column clears the sort and
  records `dropped_sort` for the tile to report.
- `ColourCache` is keyed on `(Anchors, Tokens)`; `set_colours` invalidates
  it when the definitions' `Arc` identity changes. The delegate separately
  memoizes theme-to-input conversion using all consumed theme colors.
- The gutter (`[ui] line_numbers`) is painted inside the tree cell, and
  `on_ui_settings` must call `TableState::refresh` because the pinned
  gpui-component caches column widths.
- The whole tree cell stays in `px`: column widths are the
  `view_presentation.toml` pixel contract.
- Restored filters are parsed for syntax; malformed expressions are dropped
  with a warning. Restoration does not validate column names against the
  current schema. Interactive `:filter` commands do that validation.
- TSV export includes display labels and raw, unscaled numbers, with blanks
  for NULL. Embedded tabs and newlines in text fields are not escaped.
