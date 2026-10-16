# geode-volwatch

The vol watchlist tile: a watchlist's members as rows of a vol board.

- `core/views.rs`: the five views (Changes, Levels, Rich / cheap, Term, Skew)
  as column tables, and the board ask each view implies.
- `content`, `tile`: the factory, actions, keymap and tile entity.
