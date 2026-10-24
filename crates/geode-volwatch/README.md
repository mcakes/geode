# geode-volwatch

The vol watchlist tile: one watchlist's members as rows of a vol board,
under tabs read from `vol_views.toml` and a lens that picks the companion
beside every level. Every level, change, rank, window statistic and
sparkline it paints is a cell of the data tier's board request
(`DataHandle::vol_board`); the crate's arithmetic is display (a z from
the board's mean and sd, a rank times a hundred, the scales a chip is
measured against). Behaviour and limits are in the features guide's
"Vol watchlist" section.

## Module map

- `content.rs`: `VolwatchFactory` (kind `volwatch`, title `Vol`, accepts
  no dimension), `VolwatchSettings` (`refresh` clamped to `MIN_REFRESH`,
  `vega`, the tile's own `stale_after`, a day by default since a surface
  publishes daily, and `views`, the resolved tabs; pushed whole by the app
  on a reload), the tile's `TileContent` door (`emits()` is `true`,
  `follows()` is not; `deliver` matches every `Delivery` variant with no
  wildcard: `VolBoard`, `HistoryKeys` and the vega `Query`),
  `with_dims`/`set_dims`/`dims` (the derived dimensions the grouping
  reads, pushed by the app at startup and on every dimensions reload),
  `ACTIONS` (the lens, motion and chrome verbs and the five fold actions:
  toggle, expand, collapse, expand all, collapse all) and the
  `DEFAULT_KEYMAP` fragment (`space` and the `z` fold chords among them).
- `core/`: pure state, tested without gpui.
  - `views.rs`: the tabs from `VolViews` (`builtin()` resolves the shipped
    `BUILTIN_VOL_VIEWS` document: `vol`, `skew`, `varswap`, `surface`).
    `columns` lays a view out under the lens (a pair view as a level and a
    companion column per measure; a single view as one column, headed by
    tenor and delta when every column is a surface point), `measure_id`
    (the width and sort key stem), `family_label` and `unit` (the heads
    and the readout), `order_tenors`, the board `Ask` a view implies
    (`ask`: the union of its measure columns, the window in force whenever
    one exists, each spark, and the band's measures while open;
    `Ask::tables` the datasets its open-form measures read),
    `needs_requery`, which asks again only for something the last ask
    lacked, and `sortable`, the one test `s`, a head press and `:sort`
    share.
  - `lens.rs`: `LensState` (lens, lookback, metric, window, shown) with
    each key's cycle and session word, `z_score` (blank under
    `PCTL_MIN_COUNT` observations or a non-positive sd), `companion` (the
    number the lens picks from a cell), `pctl_side` (a rank's distance
    from the median), `Z_CEILING` and the strip's lens readout.
  - `grouping.rs`: the applicable chain (`applicable`: `underlying_ref`
    and classifications over it, the rest skipped and named; `vocabulary`
    for `:group` completion), the `Pin`, the header's `chip_text`, and the
    tree (`group_rows`: a preorder row list, one group row per label in
    `classification::labels` order, `—` last, members in list order;
    `GroupInfo`/`GroupCell` carry the label, members, present count and
    each measure's mean level, Δ, z and rank over the members that have
    one; the summed vega is the group `Row.vega`; `full_path`,
    `group_name`).
  - `model.rs`: the prepared model. `rows` (members in list order, the
    board's missing at the foot), `Row.path`/`Row.group` make a row a tree
    node (`GridRow::depth`), `prepare` (each cell's text, chip or tint, id
    and muted flag, the column widths; a chip gets the displayed sign and
    the companion at display precision; a single column's tint goes where
    a pair's chip would paint muted; a group row prepares as `label ·
    count` behind the tree lead (`tree_lead`), its mean cells under the
    lens (`group_companion`) and a blank spark), `readout` (the change,
    the z, the rank and the statistics from the ask the board answered; a
    cell with no change names the board's note for its dataset; a group
    row its means and present count), `sort_value` (follows the lens),
    `prepare_band` (`BandModel`) and `header` (`HeaderModel`: the lookback
    chips under moves, the window chips under rich/cheap, the grouping
    chip and whether it is muted).
  - `session.rs`: the session table (list, view name, the lens keys as
    words, sort by column id, cursor, band, the pin as `pinned` or
    `pinned_slot` and the collapsed group paths, each at least one label
    long); an unreadable key drops alone with a notice, and a record
    without the lens keys reads the default lens.
  - `commands.rs`: the `:sort`, `:view`, `:group <dims> | slot N | none`
    and `:unpin` grammar and their completions (`group` completes the
    leaf, then the classifications over it in name order, then `none` and
    `slot`);
    an unsortable column refuses as `s` does; commas alone refuse as a
    bare `group` does and a dimension named twice refuses by name;
    whether a view name, a dimension or a slot exists is the tile's to
    say.
- `tile/`: the hosted entity.
  - `mod.rs`: the entity, the watchlist snapshot settle, the keys and the
    escape order, the tabs (`set_view_by_name`, `step_view`), one door for
    every lens key and chip (`set_lens`; a window or lookback change
    requeries, the rest paint from the board held), `settings_changed` (a
    views push replaces the tabs, keeps the shown one by name or opens
    the default with `view <name> is gone`), the grouping
    (`requested_chain`: the pin or the frame's active grouping; `regroup`
    resolves it against the dimensions, prunes the collapsed set by the
    chain's depth and rebuilds the tree; `dims_changed` on a push), the
    fold verbs (`fold`: toggle, open, close, open all, close all, from the
    keys, the chevron and the menu), the group refusals (`enter` and `t`
    on a group row), the strip's hints, `t`'s refusal, yank (a group's
    member names one per line), emission (the group path, then the
    underlying on a member; a member under `—` posts its underlying alone
    and the `—` group row refuses) and autosize (the shown view's columns only;
    `reset` clears that view's).
  - `data.rs`: the board's request lifecycle (tags, `deliver_board`,
    `deliver_keys`, the refresh and stale timers, the prepared model, the
    band (`rebuild_band`; empty on a group row), the header's dataset
    times).
  - `chrome.rs`: notices, the empty state, the switcher and the `.` action
    menu (the tabs as rows, the shown one ticked; every lens verb as a
    row, the swap disabled with its reason in a pair view; the three fold
    verbs, enabled on a group row or while grouped; inspect disabled on a
    group row with its reason).
  - `table.rs`: the grid's `TableDelegate` and `Paints` (one per theme);
    it formats nothing. It adds the table's fixed chrome (cell padding and
    the cursor border, `chrome_px`) to a prepared width, paints the tree
    lead and a group row's chevron (the press is recorded so the row
    ignores it and the shell's click-to-focus still sees it; the click
    emits `GridEvent::TogglePressed` and is stopped; a release anywhere
    clears the record), floors every
    column at its glyph's own width plus that chrome (`min_widths`, so a
    drag or a restored width cannot cut a chip's sign), truncates a cut
    cell or head with a trailing ellipsis, sizes a group head to the
    heads' text (the table leaves it to the delegate), and paints every
    tint cell in the chip's inset box so a column's levels share one
    right edge. Sized widths (an `:autosize` fit, a header drag through
    `record_widths`) are keyed `<view name>:<column id>` (`sized_key`): a
    column id is shared across views whose heads differ. `restored_widths`
    drops a session entry without a view prefix silently.
  - `header.rs`: the header (the switch control, the lens chips), the
    view strip (the tabs with their digits, the lens readout and the key
    hints), the band's slot and the footer.
  - `band.rs`: the inspection band's paint, docked between the grid and
    the footer, at the grid's text size; the name takes the grid row's
    stale tone.
  - `vega.rs`: book vega, one grouped query joined by name at depth 1.
  - `tests/`: the gpui harness (`harness.rs`, a shell stand-in matching
    the real keymap) and the key, data, chrome and grouping tests.

## Commands

```sh
cargo test -p geode-volwatch
cargo bench -p geode-volwatch
```

The `prepare` bench times `core::model::prepare` for 25 members under the
builtin `vol` tab (19 columns; named `prepare_25x24` after the planned
24); `prepare_25x24_grouped` times the same under five `region` groups
(30 rows); the group rows' means are built by `group_rows` on every row
rebuild (each board landing or refresh and each regroup), outside the
timing. Both measure well under a millisecond (see the performance
guide).
