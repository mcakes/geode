# geode-scenario

Scenario tiles: five tile kinds that paint aggregated scenario risk (vega
K/T, vega by maturity, spot ladder, by maturity, rho) for whatever the frame
or a followed link group selects. The shapes are configuration
(`scenarios.toml`, read by `geode_core::scenario`); this crate ships the
builtin five and the pivot. The tile itself is not built yet.

## Module map

- `core/spec.rs`: `BUILTIN_SCENARIOS` (`builtin_scenarios.toml`) and
  `BUILTIN_KINDS`; `builtin_shape` reads one structurally for tests and
  benches. The tests here check the five against the demo
  `datasets.toml`, so a renamed demo column refuses a builtin in CI.
- `core/pivot.rs`: `Pivot::build` turns the tile's grouped snapshot (and
  the second, column-totals snapshot) into cells, Σ, diff and heat domain.
  Every Σ comes from the snapshot's subtotal and total rows, never from the
  cells; a Σ along a scenario axis does not exist; the Σ from the second
  query is blanked while the pair's generations disagree. Diff takes its
  base from the row a scoped leaf carries (a label only the axis domain
  supplies is no base); a diff measure whose base value is NULL stays
  absolute; either sets `diff_base_missing`. A diffed column has no Σ
  entry, since the total row is absolute. The heat domain counts finite
  painted cells only, excluding Σ cells and the base row's diffed cells.
  `Err` (a stale or foreign delivery, or a shape never checked) when the
  snapshot's grouping does not match the shape's, it lacks `row_depth`,
  the shape has not been checked against the schema, a measure or its
  USD twin column is missing, the shape paints no measure, or the totals
  snapshot is grouped wrongly (or lacks `row_depth`).
- `benches/pivot.rs`: build for a 40 × 40 matrix.

Dependencies: `geode-core` only. `cargo tree -p geode-scenario -e normal |
grep -c gpui` prints `0`.
