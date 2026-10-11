# geode-scenario

Scenario tiles: five tile kinds that paint aggregated scenario risk (vega
K/T, vega by maturity, spot ladder, by maturity, rho) for whatever the frame
or a followed link group selects. The shapes are configuration
(`scenarios.toml`, read by `geode_core::scenario`); this crate ships the
builtin five, the pivot and the tile.

## Module map

- `core/spec.rs`: `BUILTIN_SCENARIOS` (`builtin_scenarios.toml`) and
  `BUILTIN_KINDS`. Behind `test-support` (and in tests): `builtin_shape`
  reads one structurally, `builtin_shape_checked` returns one as
  `load_scenarios` hands it over against the demo schema, and `demo`
  holds that schema (`DEMO_DATASETS`, `demo_schema`). The tests here check
  the five against the demo `datasets.toml`, so a renamed demo column
  refuses a builtin in CI.
- `core/pivot.rs`: `Pivot::build` turns the tile's grouped snapshot (and
  the second, column-totals snapshot) into cells, Σ, diff and heat domain.
  Every Σ comes from the snapshot's subtotal and total rows, never from the
  cells; a Σ along a scenario axis does not exist; the Σ from the second
  query is blanked while the pair's generations disagree. Diff takes its
  base from the row a scoped leaf carries (a label only the axis domain
  supplies is no base); a diff measure whose base value is NULL stays
  absolute; either sets `diff_base_missing`. A diffed column has no Σ
  entry, since the total row is absolute. The heat domain is per column
  for a ladder (each column its own unit) and matrix-wide for an axis
  `columns`; it counts finite painted cells only, excluding Σ cells and
  the base row's diffed cells.
  `Err` (a stale or foreign delivery, or a shape never checked) when the
  snapshot's grouping does not match the shape's, it lacks `row_depth`,
  the shape has not been checked against the schema, a measure or its
  USD twin column is missing, the shape paints no measure, or the totals
  snapshot is grouped wrongly (or lacks `row_depth`).
  `fixtures` (behind `test-support`) builds shapes and snapshots for the
  tile's tests and the bench.
- `content.rs`: `ScenarioFactory`, one per accepted shape (its kind is
  the shape's name; every kind shares the `scenario` context, `ACTIONS`
  and `DEFAULT_KEYMAP`, so the app ships the fragment from the first
  factory only, `without_keymap` on the rest). `launch_state` opens a
  tile on an underlying as a `[filter.dimensions]` selection.
  `ScenarioContent` is the `TileContent` door.
- `delegate.rs`: the `TableDelegate`. A label column pinned left, the
  pivot's columns, Σ edges where the pivot has sums. Cells are prepared
  into a `WindowCache` from a palette resolved once per theme signature
  (at construction and on a theme change, never in render); `render_td`
  only reads them. The cursor and a rows or block selection live over
  data cells only; the selection's per-column totals are unsummable down
  a scenario rows axis.
- `tile/mod.rs`: `ScenarioTile`: keys (`V`/`v` selections, `y`/`ctrl+c`
  yank as TSV, `c` currency without a requery, `p` paint, `m` diff, `.`
  menu, and the shell's grid motions with counts), the `:` commands
  (`filter`, `unscoped`, `asof`, `scale`; frame-wide verbs refused by
  name), notices and the session record. `escape` clears the selection
  and a live refusal (every verb clears the refusal); with neither it
  dismisses the standing notice, and with nothing to clear it is
  unhandled. Motions and presses scroll the cursor cell into view.
  The footer reports the refusal, then a query error, then the picture's
  own notice; a query error stands until a picture installs, so `c` and
  `m` (which rebuild the pivot) never hide it. `tile/menu.rs` holds the
  `.` menu (open and close only so far).
- `tile/data.rs`: the read chain. The axis domains (one distinct per
  axis, unscoped, restricted to the shape's dataset, read again only when
  a publication on the dataset or the as-of moved), the main query at full
  depth (`max_depth` = every level, so the Σ rows ride along) and, for a
  matrix with a Σ row, the totals query, all in sequence under one
  `FollowingQuery` tag and the tile's key: the trio is one barrier
  arrival. The tile follows the scope (unless `:unscoped`), the as-of
  (unless pinned by `:asof`), its dataset's publications and config, never
  the frame's grouping. A followed-group change drops the picture and asks
  again. Hiding keeps the chain in flight; closing cancels by key and
  answers an open barrier through the deferred door. A refused submission
  or an unresolved named expression is a danger notice and answers the
  barrier. The picture's notices: diff with no base row in scope, and no
  data yet (no leaf and no generation); an empty scope under a generation
  is an empty grid with the domains painted and no notice.
- `header.rs`: the shared header strip (title and readout; mode, links,
  health, ×), the table, and the footer (the reported notice; the
  selection's totals or the cursor readout).
- `lib.rs`: `CONTEXT` (`scenario`, the keymap context and action prefix)
  and `init`, which binds the keys gpui-component's DataTable would
  otherwise swallow.
- `benches/pivot.rs`: build for a 40 × 40 matrix.

Dependencies: `geode-core`, `geode-shell`, `geode-tile`, `geode-data`,
gpui and gpui-component. The `test-support` feature turns on geode-core's
own, which the fixtures' snapshot builders need.
