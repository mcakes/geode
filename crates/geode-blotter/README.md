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
| `core` | Column plans, expansion paths, visible-row traversal, cursor movement, find, selection summaries, TSV export, command parsing, and visible-window formatting without GPUI. |
| `delegate` | `TableDelegate` adapter with prepared rows and cached cell text. Owns selection and paint caches; reports cell gestures and chevron clicks to the tile. |
| `tile` | `BlotterTile`, the entity per tile: local query overrides, requests through `DataHandle`, frame observation, snapshot staging and application, header and footer rendering. |
| `content` | The `TileContent` wrapper and `BlotterFactory`, the roster entry the app builds with the data handle. |
| `colour_cache` | Caches each named color's base and sign variants until theme inputs or definitions change. |

## Commands

```sh
cargo test -p geode-blotter
cargo bench -p geode-blotter   # the pure core
```

## Invariants

- `geode_blotter::init` overrides the table's navigation bindings with
  `NoAction` after component initialization. Row clicks can briefly focus
  the table; these overrides keep its component actions inactive while
  the shell owns blotter key routing.
- A `NonAttributable` cell is NULL. Read numeric columns only through
  `f64_at`/`f64_value`; the format cache is the one place a cell becomes
  text.
- `FrameVersions.flip` is excluded from `follows_changed` on purpose: it
  tells an already-staged tile to promote, not to requery.
- A staged snapshot is promoted only while its followed frame counters
  still match, including watched data and configuration. Pins exempt only
  the corresponding frame changes. Tile-local requeries clear the stage so
  an older result cannot overwrite a new local query.
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
  summable columns with additive values produce a sum and mean; `†` marks
  non-additive values and `‡` marks unsummable columns.
- Every mouse selection gesture reaches the tile as a `CellPointer`, and
  only through `pointer`. A cell or gutter press records `drag_origin`; the
  row's own mouse-down (`render_tr`) reports a press at the cursor's column
  only when no cell caught it, so a click on the filler beside the cells is
  a plain click too. The table's `SelectRow` never touches the selection:
  it arrives on mouse-up after the press already moved the cursor, and the
  keyboard's echo carries the cursor's own row.
- Summary parts, the `rows × cols` extent, and the `†`/`‡` legend flags
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
