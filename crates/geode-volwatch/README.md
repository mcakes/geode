# geode-volwatch

The vol watchlist tile: one watchlist's members as rows of a vol board.
Every level, change, rank and sparkline it paints is a cell of the data
tier's board request (`DataHandle::vol_board`); the crate's only arithmetic
is display (a forward change as a percent, a rank times a hundred, glyph
scales and ranges). Behaviour and limits are in the features guide's
"Vol watchlist" section.

## Module map

- `content.rs`: `VolwatchFactory` (kind `volwatch`, title `Vol`, accepts
  no dimension), `VolwatchSettings` (`refresh` clamped to `MIN_REFRESH`,
  `vega`, `stale_after`, pushed whole by the app on a reload), the tile's
  `TileContent` door (`emits()` is `true`, `follows()` is not; `deliver`
  matches every `Delivery` variant with no wildcard: `VolBoard`,
  `HistoryKeys` and the vega `Query`), `ACTIONS` and the `DEFAULT_KEYMAP`
  fragment.
- `core/`: pure state, tested without gpui.
  - `views.rs`: the five views (Changes, Levels, Rich / cheap, Term, Skew)
    as column tables with `next`/`prev` stepping (`tab`/`shift+tab`,
    wrapping), `order_tenors`, the board `Ask` each view implies
    (the union of its columns' measures, plus the band's while open),
    `needs_requery`, which asks again only for something the last ask
    lacked, and `sortable`, the one test `s`, a head press and `:sort`
    share.
  - `model.rs`: the prepared model. `rows` (members in list order, the
    board's missing at the foot), `prepare` (each cell's text, glyph core,
    id and muted flag, the column widths; glyph hosts get the displayed
    sign and the rank at display precision), `readout` (a cell with no
    change names the board's note for its dataset), `sort_value`,
    `prepare_band` (`BandModel`) and `header` (`HeaderModel`).
  - `session.rs`: the session table (list, view, lookback, sort, cursor,
    band); an unreadable key drops alone with a notice.
  - `commands.rs`: the `:sort` grammar and its completions; an
    unsortable column refuses as `s` does.
- `tile/`: the hosted entity.
  - `mod.rs`: the entity, the watchlist snapshot settle, the keys and the
    escape order, `t`'s refusal, yank, emission and autosize.
  - `data.rs`: the board's request lifecycle (tags, `deliver_board`,
    `deliver_keys`, the refresh and stale timers, the prepared model, the
    header's dataset times).
  - `chrome.rs`: notices, the empty state, the switcher and the `.` action
    menu.
  - `table.rs`: the grid's `TableDelegate` and `Paints` (one per theme);
    it formats nothing.
  - `header.rs`: the header, the view strip, the band's slot and the
    footer.
  - `band.rs`: the inspection band's paint, docked between the grid and
    the footer.
  - `vega.rs`: book vega, one grouped query joined by name at depth 1.
  - `tests/`: the gpui harness (`harness.rs`, a shell stand-in matching
    the real keymap) and the key, data and chrome tests.

## Commands

```sh
cargo test -p geode-volwatch
cargo bench -p geode-volwatch
```

The `prepare` bench times `core::model::prepare` for 25 members under the
Changes view (18 columns, named `prepare_25x24` after the planned 24); it
measures well under a millisecond (see the performance guide).
