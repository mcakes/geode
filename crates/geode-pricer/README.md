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
| `columns`, `dataset`, `views` | Column vocabulary, its `pricer` dataset declaration (computed; mirrors the vocabulary), views read from `views.toml` over the `pricer` dataset (hidden columns skipped), prepared column plans, and cell text. |
| `package` | A package row's aggregated cells: its legs' distinct values in leg order joined with `/`, and the package quantity while the legs fit its template; how an edit to one of those cells maps onto its legs. |
| `cell` | Cell commit validation, the typeahead vocabularies, the expiry date commit, and nudging. |
| `entry` | Where `o` and `shift+o` land, lifting a typed package out of a leg position, the entry bar's label, and entry history. |
| `complete` | Entry-bar completion: the slot at the caret, suggestions, hint, and the Tab cycle. |
| `clip` | The yank register and where `p`/`shift+p` land. |
| `tree` | Package expansion and the visible-row walk. |
| `commands` | The `:` vocabulary (including `:autosize [reset]`): parse and completions. |
| `storage` | The frozen `pricer_sheets` declaration; conversion between sheets, document rows, and a document answer. |
| `select` | What a grid selection reaches on the sheet: the leaf lines an edit writes (`lines_of`), the top-most rows a verb or total acts on (`top_most`), the `g p` and `shift+j`/`shift+k` plans with their refusals, position risk totals (`risk_totals`), and the bulk notices' skip counts. |

The tile:

| Module | Holds |
|---|---|
| `store` | The `SheetStore` seam, addressed by key/tag with `Loaded::Refused(Refusal)` for a load that never went out and `save`/`forget` returning `Result<(), Refusal>`; `MemorySheetStore` (in-memory, the tests' fake, whose `set_save_refusal`/`set_load_refusal`/`set_forget_refusal` choose the refusal kind and `set_refusing`/`set_load_refused` are `Busy` shorthands) and `DuckSheetStore` (the store `geode-app` wires: `pricer_sheets` document reads/writes over `DataHandle`, with a `known`-names cache fed from the diagnostics catalog and the store's own confirmed writes). |
| `grid` | The prepared `GridModel`, rebuilt on change. |
| `paint` | The per-theme paint memo, floored to a readable ratio. |
| `delegate` | The table delegate: cells and their column colors (sign, named), the tree column (indent, chevron, template tag), editor, expiry date field. |
| `header` | The prepared header row (notices as `geode_tile::notice::Notice`), the sheet name control and rename field, and the footer. |
| `popup` | The typeahead, the entry bar's completion list, the sheet picker (`sheet_rows`, `SheetPicker`), and `PricerPick` (what a menu row does). The menu, popup geometry, the `:rm` confirm and the header notices paint through `geode-tile`. |
| `session` | The tile's session record, including `:autosize`'s fitted widths (`column_widths`, read leniently). |
| `content` | The factory, keymap fragment, actions, settings, and the read-only `UnderlyingSource` seam. |
| `tile` | `PricerTile`: modes, verbs, repricing, write-behind, load; `tile_columns` reports the plan's columns under the sheet's view with the cursor's column active, so the shell's `Edit column in view…` opens the Views dialog there. |
| `tile::select` | The `V`/`v` selection's state doors (start, clear, re-resolve, footer extent and totals), the selection verbs, the one-typed-value commit, and the live step (`bulk_step`, `settle_bulk`, `take_back_steps`). |

The application uses `DuckSheetStore`: sheets are `pricer_sheets` documents in
DuckDB and survive a restart. Session records retain sheet names and UI state;
a restored tile loads its sheet's live generation.

## Commands

```sh
cargo test -p geode-pricer
cargo bench -p geode-pricer
```

The `test-support` feature exposes read-only accessors a host's tests observe
a tile through (`PricerTile::sheet`, `PricerTile::is_loading`,
`PricerTile::sheet_field_text`). `geode-app`'s
dev-dependencies enable it; the crate's self dev-dependency keeps `-p` and
`--workspace` builds on one feature set.

## Invariants

- Completion never runs in render; the tile refreshes it on every text change,
  history step, commit and reload, and a Tab at a moved caret re-ranks first.
  A completion write is one range replace (one undo step) whose own `Change`
  is skipped as its echo, so the Tab cycle survives it. `lib::init` reclaims
  `tab`/`shift-tab` in the bar's `PricerEntry` context (and the sheet
  picker's `PricerSheetPicker`, where `tab` completes) from gpui-component's
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
- A package row's qty and eight text columns aggregate its legs: the
  distinct values, compared as values, in leg order joined with `/` and
  spelled as a line's cell spells them. Barrier columns read only barrier
  legs. Shifts group by the spelled effective value (an own 2.04 and an
  inherited 2.0 are one `+2.0`), an unset part among set ones paints `—`,
  and the cell paints inherited only when every leg inherits. Qty is the
  package quantity (first leg qty over the template's first weight) while
  the legs fit the template, else the list of distinct leg quantities.
- A package cell's edit maps by position onto the distinct values it shows,
  validates every part through the line cell's `edit_for` before anything
  applies, and applies as one undo entry (one reprice). A commit that
  changes no leg is no edit. Package rows open a plain text editor, even
  for expiry and type. The editor and the commit group by the planned
  column's format, the one the cell paints with, so a view's precision
  override counts the same parts in all three. The editor records the text
  it opened on; a commit whose cell would now open on other text (a
  template reload moved the legs into or out of the template's form, which
  changes whether a qty rescales by weight) closes with `MOVED`.
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
  That backoff is for `Refusal::Busy` only. A `Stopped` refusal arms no retry
  and sets `stopped`: the header shows `STOPPED` over every other notice and
  `submit` returns at once for the rest of the tile's life, so no refresh
  tick, edit, or `:price` asks a service that will never come back.
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
- A line's expiry edits in `geode_widgets::datefield`'s pure field (a
  package row's expiry edits as text, above); the
  tile owns its focus handle (what `holds_focus` and the shell's insert
  predicate read) and routes keys through `datefield::route` in
  `date_field_key` before they bubble to the shell. The painter and key
  routing use the shared widget without depending on sibling feature modules.
  A tenor seeds from the app clock's today, never
  `chrono::Local`. The tenor note is kept on the editor and restored after
  any key or refusal until the field commits or cancels.
- `cell::commit_edits` answers an empty `Vec` (and `cell::commit_date`
  `Ok(None)`) when every parsed value equals what the line or leg holds
  (`cell::changed` compares values: qty, own shifts, instrument). The
  tile's `finish_commit` closes the editor without an edit, so an unchanged
  commit in any cell records no undo entry, reprices nothing and saves
  nothing.
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
- The tile arrives at flip barriers itself; it submits no view query
  (`geode_tile::following::arrive_immediately`).
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
  submitted answers `Loaded::Refused(Refusal)`, which the tile treats as a
  failed load (`save_blocked`) naming the refusal's kind, never as a
  `Pending` that will silently never resolve. `save`/`forget` only queue a
  write — `Ok` means admitted, not written — and the confirmed outcome
  reaches the tile separately, by sheet name. A `Busy` save refusal paints
  `NOT_SAVED` and the next edit retries; a `Stopped` one sets `save_stopped`
  and paints `SAVE_STOPPED`, after which `save_now` never calls the store
  again (it repaints the notice, so `:name`, which clears the save slot,
  still shows it).
- `:e`/`:new` flush the outgoing sheet (a refused flush keeps the tile on
  it), release its name, cancel its pricing and retire its pricing tag (line
  ids restart per sheet), and reset undo, expansion, cursor and every
  per-sheet save state. `:e` of the tile's own name is a no-op except on a
  `save_blocked` sheet, which it reloads. `:name` forgets the old name only after a save under
  the new one is confirmed, and is refused on a sheet whose load failed (its
  fallback would replace the real document). `:rm` refuses every open name.
- The `:rm` confirmation is `geode_tile::confirm`'s: a focused prompt in
  the header that consumes every key (bare `y` confirms), with the door's
  Yes/No buttons (`y` and "any other key"), the tile in `insert` mode while
  armed, cancelled by focus leaving or a pointer press anywhere but the two
  buttons, blurred before it drops. A `:` command arriving under it
  withdraws it unanswered. After an answer the shell's focus restoration
  path returns the keyboard to the tile.
- The sheet picker and the rename field are pointer forms of `:e`/`:rm` and
  `:name`: a pick goes through `edit_sheet`/`arm_remove`, and the field's
  text through `commands::parse` and `rename`, so no refusal is restated.
  The picker paints through `geode_tile::popover` (surface, `row_shell`,
  `empty_row`, `anchor_popup`). Both report `mode == insert` and count in
  `holds_focus`, blur before they drop, and close on any other verb, `:`
  and `/`. `RenameBlock` is the rename refusal known before a name is typed
  (the menu greys the row with its reason). The name's press listener runs
  in the capture phase (a second click toggles the picker closed before its
  outside-press closer runs) and prevents default so no focus-tracking
  ancestor takes the new field's focus. The name's outside-press listener
  (not hover-gated, so it hears presses on surfaces painted over the tile)
  clears `last_press_on_name`, so a double-click renames only when both
  presses hit the name. A press with any modifier opens nothing and leaves
  default alone: mod+drag and mod+double-click fullscreen stay the shell's.
  `lib::init` reclaims `tab` in the rename field's `PricerRename` context,
  and the field consumes it, so the keyboard never leaves the open field.
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
- `paint` prepares grid-row text colors and tests their contrast across
  every bundled theme. Row text is checked against its base, hover, and
  selection backgrounds; menu colors are `geode_tile::menu::MenuPaint`'s.
- A view column's `color` paints as in the blotter: `sign` tints a negative
  measure bearish and a positive one bullish, a named color from
  `colors.toml` tints the column and its header (resolved through
  `geode_tile::colour::ColourCache`, invalidated when the factory's
  `colors` `Arc` changes); a stale cell stays muted and a failed one danger
  whatever the column's color (`paint::cell_colour`). Bearish, bullish and
  named-color text on a package row is not floored against the package
  ground the way the row's own paint is; a display check across the bundled
  themes is pending.
  This sweep does not cover every header or typeahead
  token, and the bounded adjustment is not a guarantee for arbitrary themes.
- A disabled action can hold the menu highlight but paints no highlight
  fill. Picking it reports its reason and keeps the menu open.
- A menu command's title is its palette title (`content::action_title`) and
  its key hint the action's live chord (`:price` when the keymap binds none,
  an empty lane for the other actions), resolved when the menu opens or its
  rows rebuild and again on every keymap publish while it is open. The menu
  opens on its first enabled action; keyboard navigation skips separators,
  section headers, and disabled rows, and from a row that is not an action
  lands on the first enabled one. Disabled actions can still hold the
  highlight after a pointer move or a rebuild of the rows under it.
- Default column widths are checked against labels and representative large
  values at the largest font size, including padding and cursor borders.
  These samples are not numeric limits: an overflowing right-aligned value
  can still lose leading digits.

- A selection is anchored by `LineId` and the plan column's vocabulary
  name and re-resolved in `sync_cursor`, which every cursor move and every
  model install (a delivery, an edit, a reload) runs; render and the
  delegate only read the prepared `resolved`, extent and totals. A lost
  anchor clears it with a footer notice; a sheet replace (`:e`, `:new`, a
  load) clears it first, silently, since line ids restart per sheet and
  would re-resolve onto unrelated lines. A verb that
  ends the selection clears it before its edit, and restores and
  re-resolves it when the edit is refused.
- Edits act on lines (`lines_of`, deduplicated, so a package selected with
  its own leg writes the leg once); verbs and totals act on top-most rows
  (`top_most`), since a package already carries its legs. Totals are
  `qty × value` per line and the folded sum per package, and a column with
  any unpriced or failed row is `None` (painted `—`), never a partial sum.
- A typed commit writes the cursor's column only, under `V` and `v`, each
  line judged on its own instrument, as one `apply_edits` batch. A selected
  package's qty (commit or step) goes through `package::commit`, so the legs
  move by the template's weights; its legs are dropped from the per-line
  targets, and a list-form package is refused. A commit over a selection
  that leaves the cursor cell as it opened writes nothing (`Editor::*::initial`:
  a choice with `moved` unset and the query empty or the opening option, a
  date on its opening day with no digit `typed`, a text field with no live
  step on its opening text): the cell's own value filled across the targets
  would be a wrong block from a no-op gesture.
- The live step (`Editor::Text::bulk`, opened only on a steppable column with
  a selection live) applies each press through `apply_batch` without
  recording it: all or nothing, a refusal from any cell refusing the press.
  A line's stepped cells compose into one `SetInstrument` and one
  `SetShift` per press (`cell::edit_on` over the press's working copy):
  both rewrite the whole record, so edits built per column from the sheet
  would have a later column put back an earlier one's step. An inherited
  shift's empty text steps from the sheet's value (`cell::step_from`, shared
  with the single-cell nudge), so it moves from what the cell paints; from
  zero, a painted `+2.0` went to `+1.0` on `up`. The before and after marks
  are keyed by `LineId`, so a press's bookkeeping stays linear.
  The press's inverse joins the bulk before the rebuild, so a rebuild that
  drops the editor records it. `enter` untouched records the steps as one
  entry (none when they net to zero, nothing written when none was taken);
  every other close (`close_editor`) rolls them back only while they are the
  sheet's last change — `edit_seq`, bumped by every `after_edit`, unchanged
  and every stepped line's qty, instrument and shift as the last step left
  them — and otherwise records them. `flush_save` (close, quit) settles a
  stepped bulk by the same rule (`take_back_steps`: rolled back while it is
  the last change, else recorded and saved) and closes its editor before the
  final save; an unstepped one stays with its open editor. A sheet replace
  drops the bulk unrecorded (`forget_steps`). A palette verb closes the
  editor first, so the palette's `undo` mid-step rolls the steps back and
  then undoes the entry before them.
- Pointer selection goes through the same `start_selection`/`clear_selection`
  doors as the keys (`PricerTile::pointer`, fed `CellPointer` by the
  delegate on mouse-down). The delegate's `drag_origin` is set only by a
  press that a cell, the tree cell or the gutter caught, and cleared by any
  release, so a button held from elsewhere never drags a selection. A press
  in the open editor's own cell, or on a chevron, sets `inner_press` so the
  row's bubbling handler does not report it; the editor's press reports
  nothing and the chevron's reports a plain press.

## Known limitations

- Column widths are fixed pixels and do not follow font size. The defaults
  fit the tested samples at the largest font step and leave more space at
  smaller steps. `:autosize` (or the palette's "Autosize columns") fits
  every column to the visible grid rows at the current rem size, so a
  collapsed package's legs are not measured. It refuses with "nothing loaded
  to fit" while the sheet loads or has no rows. It stores the widths by
  vocabulary name (`__tree` for the tree) in the session record. A font
  change does not rescale fitted widths; run `:autosize` again. A fitted
  width also overrides a view width changed later, until `:autosize reset`
  or the next `:autosize`, which replaces every kept width with a fresh fit.
- Columns can be dragged to reorder and resized with the pointer; both act
  on the open tile only. A drag lands in the delegate's `move_column` hook,
  which emits `ColumnMoved` (plan indices) for the tile to apply to its
  `ColumnPlan`; the cursor re-finds its column by name and the rebuild
  permutes every row's cells. A view change or reload rebuilds the plan from
  the view, restoring its order. A released resize handle reports
  `ColumnWidthsChanged`, and the tile records the width under the column's
  vocabulary name beside the `:autosize` fits, since the refresh every
  rebuild runs re-reads `column()` and would otherwise drop it; `:autosize
  reset` drops it with them, and the next `:autosize` replaces it. A rebuild
  during a held resize drag (a delivery or timer reprice while the handle is
  down) resets the in-progress width, so that drag is lost on release. The
  tree column is pinned and neither moves nor resizes. Persistent order and
  width belong to the Views dialog.
- Grid selections are one contiguous row range or rectangle. There is no
  paste of a yanked TSV block (`p` puts only rows a `V` yank remembered), a
  count is ignored by `d`, `shift+j`/`shift+k`, `g p` and `g u` while selecting,
  a typed value under `v` fills one column, and a list-form package's qty
  cannot be bulk-set or stepped (its skip reads only `refused`).
