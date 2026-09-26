# geode-pricer

The line-pricer module: a pure core that models a sheet of option lines and
packages and its edits without a window, and the `pricer` tile that hosts it,
registered in the application roster. The tile prices through the data tier's
pricing request (`DataHandle`) and never names a pricing implementation.

Current behavior and status:
[`docs/current/features.md`](../../docs/current/features.md#pricing-and-the-line-pricer).

## Layout

The pure core (`core`, no element, entity, window, or data service):

| Module | Holds |
|---|---|
| `sheet` | Struct-of-arrays rows, packages, inherited shifts, and stable line IDs. |
| `edit` | The one mutation door and undo records. |
| `undo` | The tile's bounded, strictly last-in first-out undo/redo stack. |
| `shorthand`, `template` | Parsing and rendering custom lines and package templates. |
| `columns`, `views` | Column vocabulary, prepared column plans, and cell text. |
| `cell` | Cell commit validation, the typeahead vocabularies, and nudging. |
| `entry` | Where `o`/`shift+o` land, and the entry history. |
| `clip` | The yank register and where `p`/`shift+p` land. |
| `tree` | Package expansion and the visible-row walk. |
| `commands` | The `:` vocabulary: parse and completions. |
| `storage` | The frozen `pricer_sheets` declaration; conversion between sheets, document rows, and a document answer. |

The tile:

| Module | Holds |
|---|---|
| `store` | The `SheetStore` seam, addressed by key/tag with `Loaded::Refused` for a load that never went out; `MemorySheetStore` (in-memory, the tests' fake) and `DuckSheetStore` (the store `geode-app` wires: `pricer_sheets` document reads/writes over `DataHandle`, with a `known`-names cache fed from the diagnostics catalog and the store's own confirmed writes). |
| `grid` | The prepared `GridModel`, rebuilt on change. |
| `paint` | The per-theme paint memo, floored to a readable ratio. |
| `delegate` | The table delegate: cells, tree column, entry row, editor. |
| `header` | The prepared header row and footer. |
| `popup` | The typeahead and the `.` action menu. |
| `session` | The tile's session record. |
| `content` | The factory, keymap fragment, actions, and settings. |
| `tile` | `PricerTile`: modes, verbs, repricing, write-behind, load. |

## Commands

```sh
cargo test -p geode-pricer
cargo bench -p geode-pricer
```

The `test-support` feature exposes read-only accessors a host's tests observe
a tile through (`PricerTile::sheet`, `PricerTile::is_loading`). `geode-app`'s
dev-dependencies enable it; the crate's self dev-dependency keeps `-p` and
`--workspace` builds on one feature set.

## Rules this crate pins

- Every edit passes through `Sheet::apply`, which returns the undo operation.
  In the tile, every mutation of the sheet goes through
  `PricerTile::apply_edit`/`apply_edits`, so the undo stack is strictly LIFO;
  the only other writes are deliveries, `mark_all_stale`, and the name, view,
  and refresh fields.
- Package rows derive from their legs; they are not independent instruments.
- Shorthand rendering uses a template only while the legs still match it.
- Storage conversion preserves stable ordering and explicit ownership of
  inherited versus row-level shifts.
- The `pricer_sheets` declaration is frozen: tables are created with
  `CREATE TABLE IF NOT EXISTS` and publishes insert positionally, so once a
  database holds the dataset its column list and order cannot change without
  a migration (none exists). `geode-app` declares it in the builtin layer and
  replaces any differing layer redeclaration with it, with an error
  diagnostic. `sheet` is `categorical = false`: sheet names are
  not a scope dimension and an autosave must not rebuild an ENUM.
- A document answer decodes (`rows_from_snapshot`) against the declaration's
  column list, not the answer's: a zero-row answer is no document, and a
  missing, wrong-typed or NULL column, attributes that differ between rows, or
  a key naming another sheet is an error naming the column, never a partial
  sheet.
- A submission carries every stale line; an outcome tagged older than the
  latest submission is dropped whole. A refusal streak is its own state over
  the header notice (never written into it), backs off from one second to a
  thirty-second cap, logs once, and ends on an admitted submission or when
  nothing is left to submit.
- A click resolves its grid row to a `LineId` before closing any field: the
  entry placeholder is a grid row, so reading the row after the close names
  the line below.
- The open-package set is never pruned by an edit (ids are never reused, so an
  undo reinstates a package open); it is pruned on load and filtered when the
  session record is written.
- The grid model is built on change and installed through `install_model`
  only, never in render.
- Both text inputs blur before they drop, and a click in the grid (a chevron
  included) cancels an open editor without committing it. `:` and `/` close
  the menu and any open field first.
- Every rebuild re-points an open editor (and the cursor column) at its column
  kind's plan index, or
  closes it with `MOVED` (blurring through its window at the end of the effect
  cycle) when the kind left the plan or the line left the grid. Every chrome
  rebuild re-checks an open menu's rows.
- The tile arrives at flip barriers itself; it submits no view query.
- An empty sheet is never saved. A sheet whose load failed is never saved
  (`save_blocked`); a change not yet queued by the store (`dirty`), or whose
  queued save was reported failed (`save_failed`), is saved when the tile
  closes, and at quit (`PricerFactory::flush_all`, called by the app before
  it stops the data service; both routes go through `flush_save`). An
  accepted save is only queued: `PricerFactory::save_answered` settles it by
  sheet name. The app delivers every outcome, in the writer's order, so the
  last to arrive is the latest queued save's. Only a confirmed
  outcome reaches the store's known names (`note_saved`/`note_forgotten`,
  no-ops on `MemorySheetStore`). The save state has its own header slot,
  which pricing notices and `escape` never touch.
- Loads carry the tile's own `load_tag` (separate from the pricing `tag`);
  `Delivery::Query` under any other tag is dropped, and the answer is decoded
  by `rows_from_snapshot` into `loaded`. A hide cancels a pending load, so
  the next show resubmits it under a fresh tag. `start_load` ends a pricing
  refusal streak.
- `SheetStore::load` is addressed by the caller's `QueryKey`/tag so a
  DuckDB-backed answer can be routed back; a load the store never
  submitted answers `Loaded::Refused`, which the tile treats as a failed
  load (`save_blocked`), never as a `Pending` that will silently never
  resolve. `save`/`forget` only queue a write — `true` means admitted,
  not written — and the confirmed outcome reaches the tile separately, by
  sheet name.
- `:e`/`:new` flush the outgoing sheet (a refused flush keeps the tile on
  it), release its name, cancel its pricing and retire its pricing tag (line
  ids restart per sheet), and reset undo, expansion, cursor and every
  per-sheet save state. `:e` of the tile's own name is a no-op except on a
  `save_blocked` sheet, which it reloads. `:name` forgets the old name only after a save under
  the new one is confirmed, and is refused on a sheet whose load failed (its
  fallback would replace the real document). `:rm` refuses every open name.
- The `:rm` confirm is market-data's upload confirm: a focused prompt in the
  header whose `on_key_down` consumes every key (bare `y` confirms), the
  tile in `insert` mode while armed, cancelled by focus leaving or a pointer
  press, blurred before it drops.
- Known names are the store's (`set_known` from the diagnostics catalog,
  which only adds and never re-adds a name confirmed forgotten until a
  save of it is confirmed; confirmed saves; less confirmed forgets) plus
  `Shared::pending_saves` — names with a save queued and not yet answered,
  counted per name (one per admitted save, less one per outcome; exact
  because every admitted local publish answers once and every answer is
  delivered). `untitled-N` and `:name` treat both as taken. A load of a
  pending-save name waits (`load_waiting`, no request) until the name's last
  queued save has answered: reads and saves are on unordered lanes.
  `Shared::retiring` reserves a name from `:name`/`:rm` until its forget is
  answered; `:e`, `:name`, `:rm`, a restore and the `:rm` confirm's `y`
  (which re-checks `open` too) refuse it, and a rename's
  confirmed save never forgets a name a tile holds. Save outcomes route to
  `Shared::save_origins` (the queuing tile), never by current holder; a
  failure of a `:name`'s old-name save is not painted (the edits travel under
  the new name), and a failed load keeps a standing lost-edits notice beside
  its block. The factory observes the one
  `Diagnostics` entity from its first `create`, comparing the data version,
  and asks for a catalog (with a notify) when none is held.
- In the free underlying typeahead, `enter` takes the highlighted option only
  when the query equals it case-insensitively or the highlight was moved with
  a key or a click; a pointer hover moves the highlight but does not count as
  moving it, so otherwise `enter` commits the typed text.
- A row's own ground (package, entry) is painted by `render_tr` on the row,
  never per cell, so the table's hover and selected-row fills stay visible.
- Every text colour the tile adds is floored against the ground it paints on
  and swept over every bundled theme with no exception list (`paint`),
  including the action menu's four. A row's text is floored on every ground
  the row can wear: its own, the table's hover ground and the selected-row
  ground (which replace it).
- A disabled menu row never takes the highlight fill (market-data's rule).
- A menu command's title is its palette title (`content::action_title`); the
  menu's keyboard highlight never rests on a separator, section header or
  disabled row (`popup::step`); only the pointer lands on a disabled row.
- Default column labels and widths fit a worst-case value at the largest font
  size (`delegate`'s fit test): a right-aligned cell that overflows loses its
  leading digits.

## Known limitations

- The action menu's key hints are the default bindings, written into the
  menu; a user rebind is not reflected there. The market-data action list has
  the same limitation.
- Column widths are pixels and do not follow the font size; the defaults are
  sized for the largest step, so they are generous at the smaller ones.
