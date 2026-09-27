# geode-blotter

The blotter module: any view definition rendered as a collapsible,
keyboard-driven hierarchy with honest markers for what a cell means
(`pinned`, `unscoped`, `filtered`, `AS OF`, and a NULL for a measure the
compiler declined to sum).

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#blotter).

## Layout

| Module | Holds |
|---|---|
| `core` | The pure half, no gpui: the column plan (every view column resolved to a snapshot index once, with kind, format, width and per-depth attribution), expansion as a set of paths that survive a requery, the visible-row flatten (a DFS that descends only into open nodes), the cursor, `/` find under both styles, yank as TSV (rows or a cell block), `core::select`'s footer aggregate summary over a resolved selection's top-most rows, the `:` vocabulary as data, and the format cache filled for the visible window, never in `render_td`. |
| `delegate` | The `TableDelegate` adapter over gpui-component's `DataTable`. Owns everything the table paints so every `render_td` is a lookup. Emits its own `ChevronClicked` event through a second `EventEmitter` impl on `TableState<BlotterDelegate>`. |
| `tile` | `BlotterTile`, the entity per tile: requests through `DataHandle`, follows the frame's versions, stages under the flip barrier, applies snapshots. |
| `content` | The `TileContent` wrapper and `BlotterFactory`, the roster entry the app builds with the data handle. |
| `colour_cache` | One OKLCH resolve per named color per `(Anchors, Tokens)` pair, so a cell color is a lookup. |

## Commands

```sh
cargo test -p geode-blotter
cargo bench -p geode-blotter   # the pure core
```

## Rules this crate pins

- `geode_blotter::init` binds `DataTable`'s key context to `NoAction` and
  the table is never focused, so it cannot swallow the vim keys the
  blotter's own bindings depend on.
- A `NonAttributable` cell is NULL. Read numeric columns only through
  `f64_at`/`f64_value`; the format cache is the one place a cell becomes
  text.
- `FrameVersions.flip` is excluded from `follows_changed` on purpose: it
  tells an already-staged tile to promote, not to requery.
- A chevron click and a row double-click are `space`: both go through
  `expand_at_cursor`, the path `zo`/`zc`/`za` take. The chevron listener
  stops propagation and ignores `click_count() > 1`.
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
  generation and theme; the tile's render calls it, and a steady frame
  costs one compare.
- `apply_snapshot` rebuilds the column plan on every delivery and swaps on
  inequality. Do not reinstate a cheaper gate.
- Sorts store column names. `SortSpec.column` is resolved against the fresh
  plan on every rebuild in `apply_snapshot`. The cursor stores a position;
  rebuilds and `move_column` preserve its column by resolving the old column
  name in the new plan. If that column disappears, the cursor falls back to
  clamping its previous position. A removed sort column clears the sort and
  records `dropped_sort` for the tile to report.
- `ColourCache` is keyed on `(Anchors, Tokens)` and `set_colours` compares
  the `Arc` pointer and invalidates, or a redefined color paints stale.
- The gutter (`[ui] line_numbers`) is painted inside the tree cell, and
  `on_ui_settings` must call `TableState::refresh` because the pinned
  gpui-component caches column widths.
- The whole tree cell stays in `px`: column widths are the
  `view_presentation.toml` pixel contract.
