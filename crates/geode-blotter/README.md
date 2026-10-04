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
| `core` | Column plans, expansion paths (`core::expansion`: `path_of` and `depth_bound` over `geode_core::expansion::Expansion`), visible-row traversal, cursor movement (`core::cursor`, moved by `geode_tile::motion`), find, selection summaries, cursor-row dimension context, TSV export, command parsing, and visible-window formatting without GPUI. |
| `delegate` | `TableDelegate` adapter with prepared rows and cached cell text. Holds `:autosize`'s fitted widths by column name, which `column()` prefers over the plan's; `fit_columns` measures the header and the window (`geode_tile::grid::WindowCache`) only. Owns selection and paint caches; reports cell gestures and chevron clicks to the tile. |
| `header` | The prepared header: dataset time runs (oldest first, each source time parsed once; an unparsable one is never stale), the frame's `AS OF` warning text and the datasets its health question reads, rebuilt when a snapshot lands or the clock changes. |
| `tile` | `BlotterTile`, the entity per tile: a key context that publishes `grid` (so the shell's shared `motion::*` keys reach it), `dispatch` routing every `motion::*` id through `geode_tile::motion`, local query overrides, requests through `DataHandle`, frame observation over `geode_tile::following`, snapshot application, the header painted through `geode_tile::header::frame` (view, grouping and chips on the left; the notice, dataset times and the health chip over its snapshot's provenance datasets in the cluster, then the shell's ×; no `⋯`: the blotter has no action menu) and footer rendering from prepared labels. The header notice is a `geode_tile::notice::Notice`: dropped sorts and selections are warnings, query and configuration failures danger. Every change to it goes through `set_error` (or `set_one_shot` for a dropped sort or selection and the restored view's refusal, which the constructor already raises as one-shot; cleared before the fallback view's first delivery, it is not raised again), which prunes the tile's `geode_tile::notice::Dismissals`. A click on the notice, or `escape` once the selection, the `/` narrowing and the find are gone (each takes its own press first), does the same to it (`dismiss_notice`): a one-shot notice is cleared; a query or configuration failure is standing and is hidden while `error` still holds it, showing again only after the tile stopped reporting it and reports it anew (a good delivery clears it). |
| `content` | The `TileContent` wrapper and `BlotterFactory`, the roster entry the app builds with the data handle. |

## Commands

```sh
cargo test -p geode-blotter
cargo bench -p geode-blotter   # the pure core
```

## Invariants

- `/` cells are formatted in `on_rows` only (`delegate::FindCells`), for the
  rows the result table reports; paint reads. A snapshot landing while `/`
  is open re-indexes and drops every held cell, so no cell of the old
  snapshot paints under the new rows. A column move (`TableEvent::MoveColumn`)
  re-installs `/` against the moved plan, because `/`'s headers read the
  live plan through `render_th`; the main header is not painted while `/` is
  open, so this guards any future route that moves a column under it.
- The blotter always groups: `:group none` (the pricer's flat-sheet pin),
  and `none` anywhere in a column list, is refused
  (`core::commands::GROUP_NONE_REFUSED`), because an empty grouping is one
  grand-total row. A view without `grouping` reads `ungrouped` in the
  header and `view · ungrouped` in the title; before the first query the
  title is the view name alone.
- `:autosize` and the shell's `tile::autosize_columns` run one method,
  `BlotterTile::autosize_columns`. It measures only rows in the window
  (`geode_tile::grid::WindowCache`, the range the table last asked for), never the whole snapshot. A
  wider value outside that window does not widen the column. With no plan
  or no cached rows, a fit refuses with "nothing loaded to fit" and keeps
  its widths.
- A view switch clears the fitted widths, and a restored record whose view
  is gone starts without them. A view over a computed dataset is never the
  blotter's: `:view` neither completes nor opens it, and a record naming one
  opens the fallback view with the refusal as its notice, held through the
  fallback's first snapshot. The fallback (default, else first) skips
  computed views as well. `apply_snapshot` drops only the tree column's
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
  `f64_at`/`f64_value`; `core::cache::cell` is the one place a cell
  becomes text.
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
  open barrier still waiting on the tile. The shell calls `closed` and
  `set_visible` inside its draw, so their arrivals (the close's, and a
  reshow's refused or unconfigured requery) go through
  `geode_tile::following::DeferredDoor`; inline, the release's notify would
  be dropped and every other tile held to the barrier's deadline.
- A chevron click and a row double-click are `space`: both go through
  `expand_at_cursor`, the path `zo`/`zc`/`za` take. The chevron listener
  stops propagation and ignores `click_count() > 1`.
- `v` selects a cell block and `V` selects whole rows. Repeating the active
  kind clears the selection; switching kind keeps its anchor. Anchors use
  row paths and column names across sorting, column moves, and redelivery.
  Losing the anchor row, or a block's anchor column, clears the selection
  with a notice. The compatibility action `blotter::visual` resolves to
  `blotter::visual_rows` with a warning.
- The fragment binds no motions. The key context publishes `grid` in both
  modes; `dispatch` hands every `motion::*` id to `Cursor::apply`, which
  clamps a bare step while a selection is live. The twelve retired motion
  ids (`blotter::down` … `blotter::last_col`) are renames in
  `RENAMED_ACTIONS`, so an old user binding still moves the blotter, in its
  own context, with a warning.
- Selection summaries include only the selected rows without a selected
  ancestor, avoiding double-counted group totals. Only compiler-marked
  summable columns with additive values produce a footer total; `†` marks
  non-additive values and `‡` marks unsummable columns.
- `g m` opens another module on the cursor row's dimension context
  (`core::context`): every column with one value there, read from the
  grouping path (a subtotal carries only its own levels and those above), the
  shown dimension columns, then the hidden context columns the data service
  adds. A NULL, empty or mixed value is absent. The context also carries the
  selection's rows while the cursor is inside a `V` (rows) selection, never a
  `v` block; `g m` opens on the cursor row's values alone. Its `anchor` is the
  cursor row's lower-left in window space, recorded at paint and cleared when
  that row scrolls out of view.
- A blotter can follow a link group (`TileContent::follows`): its query is
  scoped by the frame, so following one queries under the group's scope
  composed with the tile's own `:filter`, in place of the workspace's. An
  `:unscoped` tile ignores a followed group's scope as it ignores the
  workspace's, while its header still shows the chip.
- Emitting into a link group (`TileContent::emission`), the tile reports
  the cursor row's path, its `:filter` layer and its `:unscoped` flag; the
  shell composes them over the tile's base (see `docs/current/shell.md`). It
  posts no board. `core::context::cursor_scope` builds the path: one value
  per grouping level, and on a leaf row (the deepest grouping level) the
  rest of `values_at` after it, so a NULL or mixed leaf value is omitted. A
  group row adds nothing beyond its levels; the total row's path is empty.
  A NULL or empty grouping value on the path refuses
  (`CursorScope::NullIn(column)`): a scope cannot select NULL, and dropping
  the level would widen every follower to all its values. The shared tile
  header shows the refusal from frame state (an empty value too reads `…
  is NULL`); the blotter posts no notice of its own. The tile reads
  the one shown row at the cursor and skips the selection walk
  `dimension_context` does: the shell pulls on every notification while the
  tile emits, and a selection never changes the cursor's path. Before the
  first snapshot it answers `CursorScope::Nothing`, which leaves the
  group's scope as it was. `emits` is true before any snapshot: the shell
  drops a restored membership for a tile that answers false right after
  create. `watch_emission` observes the tile entity, so each route that can
  change the emission must notify it: a delivery, the cursor sync every
  motion, press, sort and tree change ends in, the frame observer's
  promotion of a result held behind a flip, which no delivery paints, and
  the `:filter`/`:unscoped` commands. Reading the shown row, the emission
  follows a sort. `g m` reads `underlying_ref` from the dimension context,
  not from the emission. The tile stores no group; its header reads `link_chips` from its frame
  handle at paint.
- `g .` opens the shell's row menu on that context. A right press on a cell,
  or on a row beside its cells, records the cell in the delegate's
  `pressed_cell` inside the listener (the `press_context` contract) and
  emits `CellPointer::Context`; the tile then
  keeps the cursor and selection when the row is inside a `V` selection, and
  otherwise clears the selection and moves the cursor there, as a plain
  press does. `press_context` answers that row's context once, with `first`
  set to the pressed column when it is a dimension the row carries (a press
  beside the cells uses the cursor's column). A filler row below the data
  opens nothing.
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
- `SortOrder`, its key and click cycles, and the `:sort` grammar and
  completions are `geode_core::sort`, shared with the line pricer; the
  blotter's `SortSpec` and the sibling ranking over a snapshot stay here.
- Sorts store column names. `SortSpec.column` is resolved against the fresh
  plan on every rebuild in `apply_snapshot`. The cursor stores a position;
  rebuilds and `move_column` preserve its column by resolving the old column
  name in the new plan. If that column disappears, the cursor falls back to
  clamping its previous position. A removed sort column clears the sort and
  records `dropped_sort` for the tile to report.
- `ColourCache` is keyed on `(Anchors, Tokens)`; `set_colours` invalidates
  it when the definitions' `Arc` identity changes. The delegate separately
  memoizes theme-to-input conversion using all consumed theme colors.
- `core::cache::cell` takes the `ValueColors` mapping and stores a mapped
  dimension value's color name on `CachedCell.value_color` (tree labels by
  `plan.grouping[depth - 1]`; never on a measure, `mixed` or the grand
  total). Paint only resolves the name through `themed_value_colour`;
  `text_paint` orders it above the column's `color`. Because the window
  holds names from one mapping, `set_colours` also calls `invalidate_cells`
  on an identity change, and `FindCells::install` drops its held cells when
  the `NamedColours` `Arc` differs.
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
