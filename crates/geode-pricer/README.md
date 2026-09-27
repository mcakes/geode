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
| `sheet` | Struct-of-arrays rows, packages, inherited shifts, stable line IDs, and `sole_underlying` (a row's own underlying, or a package's when its legs share one). |
| `edit` | The one mutation door and undo records. |
| `undo` | The tile's bounded, strictly last-in first-out undo/redo stack. |
| `shorthand` | Parsing and rendering lines and packages against a `TemplateSet`. |
| `template` | Template names, the `pricer_templates` reader and `TemplateSet`. |
| `columns`, `views` | Column vocabulary, prepared column plans, and cell text. |
| `cell` | Cell commit validation, the typeahead vocabularies, the expiry date commit, and nudging. |
| `entry` | Where `o` lands, lifting a typed package out of a leg position, the entry bar's label, and entry history. |
| `complete` | Entry-bar completion: the slot at the caret, suggestions, hint, and the Tab cycle. |
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
| `delegate` | The table delegate: cells, the tree column (indent, chevron, template tag), editor, expiry date field. |
| `header` | The prepared header row and footer. |
| `popup` | The typeahead, the entry bar's completion list, and the `.` action menu. |
| `session` | The tile's session record. |
| `content` | The factory, keymap fragment, actions, settings, and the read-only `UnderlyingSource` seam. |
| `tile` | `PricerTile`: modes, verbs, repricing, write-behind, load. |

The application uses `DuckSheetStore`: sheets are `pricer_sheets` documents in
DuckDB and survive a restart. Session records retain sheet names and UI state;
a restored tile loads its sheet's live generation.

## Commands

```sh
cargo test -p geode-pricer
cargo bench -p geode-pricer
```

The `test-support` feature exposes read-only accessors a host's tests observe
a tile through (`PricerTile::sheet`, `PricerTile::is_loading`). `geode-app`'s
dev-dependencies enable it; the crate's self dev-dependency keeps `-p` and
`--workspace` builds on one feature set.

## Invariants

- Completion never runs in render; the tile refreshes it on every text change,
  history step, commit and reload, and a Tab at a moved caret re-ranks first.
  A completion write is one range replace (one undo step) whose own `Change`
  is skipped as its echo, so the Tab cycle survives it. `lib::init` reclaims
  `tab`/`shift-tab` in the bar's `PricerEntry` context from gpui-component's
  focus cycling.
- Entry completion suggests configured underlyings, upcoming monthly expiries,
  tenors, option types, templates, and barrier kinds. It replaces the token at
  the caret (one slash-separated part for expiries or strikes); quantities,
  strikes, and barrier levels have hints but no suggestions. Tab/Shift-Tab
  cycle candidates, a pointer press accepts a row, and Enter parses the typed
  line without implicitly accepting the highlight.
- The app supplies `[pricing] underlyings` through `UnderlyingList`, which trims
  and uppercases names, drops blanks and duplicates, and preserves first occurrence
  order. Tiles cache names by provider revision. An absent setting clears the list;
  a non-array value warns and keeps the previous list on reload, while non-string
  array elements warn and are skipped. Suggestions do not restrict typed names.
- A package typed at a leg position becomes a root after the containing package;
  a single line still becomes a leg. After insertion the bar advances from the
  actual landing place, so further rows follow the new root.
- Every edit passes through `Sheet::apply`, which returns the undo operation.
  New tile edits use `PricerTile::apply_edit`/`apply_edits`; undo and redo
  apply through the LIFO history. Loading replaces the sheet. Deliveries,
  stale marking, and sheet metadata updates have separate paths.
- Package rows derive from their legs; they are not independent instruments.
  Their pricing timestamp is the oldest present leg-attempt timestamp,
  including failed attempts.
- Shorthand rendering uses a template only while the legs still match its
  current table (an overflowing quantity never matches); otherwise it prints
  the legs one per line. The grid keeps the shorthand as the row's find key
  and paints only a package's template token. Loading accepts unresolved
  template names because stored instruments remain sufficient for repricing.
- `TemplateSet::from_doc_over` keeps the last valid definition per name.
  An entry dropped with an error keeps the previous set's definition of
  its name, in the entry's own position. A name absent from the document
  is removed.
- A sheet built by `Sheet::new` or `from_rows` carries the builtin template
  set. `PricerTile::adopt_templates` is the only place a tile's sheet gets
  the factory's configured set; it runs wherever a sheet is installed (open,
  load, `:e`, `:new`) and on every reload, so the entry bar parses against
  the configured set. A reload also reprints an open bar's history.
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
  latest submission is dropped whole. Consecutive refusals overlay the header
  notice without replacing it, log once per streak, and schedule retries from
  one second up to a thirty-second cap. Admission or a submit with no further
  work needed ends the streak. Only one retry timer is pending at a time.
- Package expansion IDs survive edits because IDs are not reused, allowing
  undo to restore an open package. Loading prunes the set; session output
  includes only packages still present. Restoring a leg selects it and opens
  its parent if necessary.
- The grid model is built on change and installed through `install_model`
  only, never in render.
- Every field (entry, text editor, expiry date field) blurs before it drops,
  and a click in the grid (a chevron included) cancels an open editor without
  committing it. A date segment's mouse-down stops propagation, so a click
  aimed into the field selects a segment rather than cancelling it.
- A press that closes the entry bar, on a cell or a chevron, hands its
  resolved line to the next press only: the bar's close moves the table up
  on screen, so the second press of the same double-click lands on a lower
  row. A double-click (and its tree-column cursor move) uses the handed-on
  line.
- `add_below` with the bar already open (a palette dispatch) refocuses its
  field: the palette's commit focuses the shell root first, and an open bar
  without focus reads `insert` while shell bindings take shifted letters.
- The expiry always edits in `geode_widgets::datefield`'s pure field; the
  tile owns its focus handle (what `holds_focus` and the shell's insert
  predicate read) and routes keys through `datefield::route` in
  `date_field_key` before they bubble to the shell. The painter and key
  routing use the shared widget without depending on sibling feature modules.
  A tenor seeds from the app clock's today, never
  `chrono::Local`. The tenor note is kept on the editor and restored after
  any key or refusal until the field commits or cancels.
- `cell::commit` and `cell::commit_date` answer `Ok(None)` when the parsed
  value equals what the line holds (`cell::changed` compares values: qty,
  own shifts, instrument). The tile's `finish_commit` closes the editor
  without an edit, so an unchanged commit in any cell records no undo entry,
  reprices nothing and saves nothing.
- In-grid fields (`delegate::cell_input`) are `Input::appearance(false)` with
  no horizontal padding, at the row's height, in the cell's alignment: the
  cell's cursor border is the only frame. `:` and `/` close
  the menu and any open field first.
- Model installation resolves an open editor by line ID and column kind,
  updating its plan index and the cursor column together. If the line or
  column disappears, the field closes with `MOVED`; deferred window access
  blurs its retained input only if it still owns focus. Chrome rebuilds
  refresh open-menu rows and keep the highlight on an action or view.
- `g m` opens the module picker with the cursor row's underlying as launch
  context. A package contributes an underlying only when all its legs share
  one; an empty sheet or mixed-underlying package contributes none.
- The tile arrives at flip barriers itself; it submits no view query.
- An empty sheet is never saved. A sheet whose load failed is never saved
  (`save_blocked`); a change not yet queued by the store (`dirty`), or whose
  queued save was reported failed (`save_failed`), is saved when the tile
  closes, and at quit (`PricerFactory::flush_all`, called by the app before
  it stops the data service; both routes go through `flush_save`). An
  accepted save is only queued: `PricerFactory::save_answered` settles it by
  sheet name. The app delivers every outcome, in the writer's order, so the
  last to arrive is the latest queued save's. Only a confirmed
  outcome updates the production store's known names through
  `note_saved`/`note_forgotten`. The save state has its own header slot,
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
- The `:rm` confirmation uses a focused prompt in the
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
- A package row's ground is painted by `render_tr` on the row,
  never per cell, so the table's hover and selected-row fills stay visible.
- The line-number gutter (`[ui] line_numbers`, read from the `UiSettings`
  global and observed) sits beside the tree cell, outside its depth indent,
  so numbers share one lane at every depth. The tree column widens by the
  gutter; the observer refreshes the table's cached widths. Numbers count
  cursor rows (lines, packages, visible legs) — the index `NG` jumps to and
  `Nj`/`Nk` count.
  Relative mode measures from the cursor row and numbers absolutely with no
  cursor row. `refresh_numbers` prepares the text and width outside render,
  before every `refresh`, keyed by row count, relative cursor row and
  mode. Gutter text uses the row's floored muted paint (the row's
  own text paint on the cursor row).
- `paint` prepares grid-row and action-menu text colors and tests their
  contrast across every bundled theme. Row text is checked against its base,
  hover, and selection backgrounds; menu text against popover and enabled
  highlight backgrounds. This sweep does not cover every header or typeahead
  token, and the bounded adjustment is not a guarantee for arbitrary themes.
- A disabled action can hold the menu highlight but paints no highlight
  fill. Picking it reports its reason and keeps the menu open.
- A menu command's title is its palette title (`content::action_title`); the
  menu's keyboard navigation skips separators, section headers, and disabled
  rows. Disabled actions can still hold the highlight after a pointer move,
  opening the menu, or rebuilding its rows.
- Default column widths are checked against labels and representative large
  values at the largest font size, including padding and cursor borders.
  These samples are not numeric limits: an overflowing right-aligned value
  can still lose leading digits.

## Known limitations

- The action menu's key hints are the default bindings, written into the
  menu; a user rebind is not reflected there. The market-data action list has
  the same limitation.
- Column widths are fixed pixels and do not follow font size. The defaults
  fit the tested samples at the largest font step and leave more space at
  smaller steps.
