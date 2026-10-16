# geode-volwatch

The vol watchlist tile: a watchlist's members as rows of a vol board.

- `core/views.rs`: the five views (Changes, Levels, Rich / cheap, Term, Skew)
  as column tables, and the board ask each view implies.
- `content`, `tile`: the factory, actions, keymap and tile entity.
- `tile/header.rs`: the header (switch control, count, lookback chips,
  sort, missing and failed-dataset chips, the shared cluster with one
  source time per dataset), the view strip and the footer (notice and
  cursor readout). Every string it paints is prepared outside render.
- `tile/band.rs`: the inspection band, docked between the grid and the
  footer while open (`enter` toggles it on the cursor row, `escape` closes
  it ahead of a selection, a filter and the notices). It follows the
  cursor and paints `core::model::BandModel` (the fixed-delta surface, the
  three one-year sparks with captions, the context pairs), prepared with
  the readout. Opening grows the board ask; closing never requeries. A
  member the surface lacks refuses with a notice.
