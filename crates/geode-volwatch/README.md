# geode-volwatch

The vol watchlist tile: a watchlist's members as rows of a vol board.

- `core/views.rs`: the five views (Changes, Levels, Rich / cheap, Term, Skew)
  as column tables, and the board ask each view implies.
- `content`, `tile`: the factory, actions, keymap and tile entity.
- `tile/header.rs`: the header (switch control, count, lookback chips,
  sort, missing and failed-dataset chips, the shared cluster with one
  source time per dataset), the view strip and the footer (notice and
  cursor readout). Every string it paints is prepared outside render.
