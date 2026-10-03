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
| `commands` | The `:` vocabulary (including `:autosize [reset]`, `:package [n]`/`:unpackage`, `:group <columns>`/`:group slot <n>`/`:group none`/`:unpin`): parse and completions (`:group` completes `rollup::groupable_vocabulary`, `none` and `slot`; `none` is reserved, never a column). |
| `storage` | The frozen `pricer_sheets` declaration; conversion between sheets, document rows, and a document answer. |
| `select` | What a grid selection reaches on the sheet: the leaf lines an edit writes (`lines_of`), the top-most rows a verb or total acts on (`top_most`), the `g p` and `shift+j`/`shift+k` plans with their refusals (`move_plan` steps by a `lands` predicate and an edge refusal, `OFF_END` or `OFF_GROUP_END`), position risk totals (`risk_totals`, and `risk_totals_visible`, which counts only a partly hidden package's shown legs), and the bulk notices' skip counts. |
| `reorder` | Where a moved row may land. `Placements` maps each sheet row to the group paths of the rollup nodes that place it; `same_node` is the move's "same node" predicate under a value grouping (refusing movers painted in several groups or none, `SPANS_GROUPS`). `DropPlan` holds the movers and every sibling they may land beside with its painted grid span: `snap` gives the legal gap nearest a pointer row (none outside the stops' rows), `edits` the one batch of `Edit::Move`s (one per mover not already in place, straight beside the sibling that stays) landing them there, `None` for an illegal or in-place gap. |
| `rollup` | The shown lines under a grouping chain. `effective_chain` drops levels `pricer` cannot group by (a column it lacks, a measure, a derived dimension over either or over a synthetic key, any level after `position_ref`/`instrument_ref`, a repeated level; `value_levels` counts the kept value levels, the regroup's prune depth). `build` partitions shown legs and bare lines by their `SheetRow` value per level (NULL distinct from empty and last; numbers by number, text by byte order), then gathers them by parent in sheet order; a package whose legs fall under several groups appears under each (`split`; `partial` when fewer than all its legs). `position_ref` is the package node, `instrument_ref` makes every leg a leaf. `legs_under` gives a node's legs in sheet order, however a sort ranked its nodes. Pure; the flat case is the empty chain. |
| `sort` | The column sort's display order: `SortSpec` (a vocabulary column by name and a `geode_core::sort::SortOrder`), `is_measure` (only result measures have a magnitude), and `rank`, which reorders the rollup's roots and every group node's children in place by the typed value each row paints in that column (a group's fold or unanimous value, a package's aggregate over its node's legs, a line's cell): values, then `mixed`/gap cells, then blanks in both directions, stable. A package's legs never move. |
| `visibility` | Which lines the frame's scope hides: `apply_scope` runs `geode_core::scope::eval` over each line as a `pricer` dataset row (`SheetRow`, the values its cells paint; measures result × qty; a leg's `template` its package's; `status` `fresh` for a fresh line; `expiry` the ISO date of a dated expiry, a tenor's text) and returns a `Visibility` (shown per sheet row, hidden line count), or a refusal that hides nothing. Selections on columns `pricer` lacks, or on `position_ref`/`instrument_ref` (`NOT_SCOPEABLE`: the sheet's synthetic `p<id>`/`i<id>`), are dropped; an expression naming one refuses. The scope is bound once (`Scope::bind`) before any line, so an empty sheet refuses a row-independent error too. |

The tile:

| Module | Holds |
|---|---|
| `store` | The `SheetStore` seam, addressed by key/tag with `Loaded::Refused(Refusal)` for a load that never went out and `save`/`forget` returning `Result<(), Refusal>`; `MemorySheetStore` (in-memory, the tests' fake, whose `set_save_refusal`/`set_load_refusal`/`set_forget_refusal` choose the refusal kind and `set_refusing`/`set_load_refused` are `Busy` shorthands) and `DuckSheetStore` (the store `geode-app` wires: `pricer_sheets` document reads/writes over `DataHandle`, with a `known`-names cache fed from the diagnostics catalog and the store's own confirmed writes). |
| `grid` | `GridIndex`, rebuilt on structural change: the rollup flattened under the group and package expansions, one row of tree-column facts per painted row (kind, sheet row, id, node, path, depth, tag, text, note, search) and `row_of`. `CellPass` formats the measure cells a window shows: a group row (`GridRowKind::Group`) sums its legs' fold and reads unanimity (`group_cell_text`) only when filled; a split or partly hidden package row paints its node's legs only (`· n of M legs`). |
| `paint` | The per-theme paint memo, floored to a readable ratio; two `RowPalette`s, each a ground and its text floored on it and on hover and selected: `group` (`secondary` over the table ground) and `leg` (`leg_ground`: the stripe token, or the faintest blend clearing `LEG_TINT_RATIO` from the line, hover, selected and group grounds while fainter than hover; selected is let go only where nothing clears it). |
| `delegate` | The table delegate: cells and their column colors (sign, named), a movable row's hover grip (`grips`, prepared by the tile; its press emits `RowGripPressed` and is never a selection gesture; dragging it is a gpui drag of `RowDrag`) and the drop line at the mirrored `drop_gap`, the header's sort icon and `|x|` label from the tile's mirrored sort (a click emits `SortClicked`; the tile steps `click_cycle`), the connector tree column (indent; a group row's chevron and medium-weight label over its own ground; a package's chevron, template chip, summary and muted leg count; a leg's drawn connector lines and shorthand; a bare line's shorthand), editor, expiry date field. |
| `header` | The prepared header row and the footer. Paints through `geode_tile::header::frame`: the sheet name control (and rename field) with its view control (a press toggles the view menu, `MenuKind::Views`, hung under it) and shift chips and the grouping chain (as written, dropped levels struck through, and the `pinned` chip) on the left; `N pricing…`, `N failed` and the pricer label as cluster status; the save notice then the tile notice, the priced time, the health chip (asked about `pricer_sheets` only, which no source feeds — see Known limitations), `⋯` and the shell's × from the shared cluster. |
| `popup` | The typeahead, the entry bar's completion list, the sheet picker (`sheet_rows`, `SheetPicker`), and `PricerPick` (what a menu row does). The menu, popup geometry, the `:rm` confirm and the header notices paint through `geode-tile`. |
| `session` | The tile's session record, including `:autosize`'s fitted widths (`column_widths`, read leniently), the grouping pin (`pinned` / `pinned_slot`) and the open groups (`expanded_paths`, NULL as `{ null = true }`). |
| `content` | The factory, keymap fragment (verbs including `s`/`shift+s` for the sort, field keys and the menu's pick and close keys; the grid motions and the menu steps are the shell's shared `motion::*` bindings), actions, the retired motion ids' renames (`RENAMED_ACTIONS`), settings, and the read-only `UnderlyingSource` seam. |
| `tile` | `PricerTile`: modes (a key context that publishes `grid`, and `tilelist` while the action or view menu is open (one `menu` slot; `menu_kind` says which, and each menu's outside press closes only its own kind, since the other trigger swaps in the capture phase) so the shared menu steps reach it), verbs, repricing, write-behind, load. Every `motion::*` id runs as one verb through `geode_tile::motion`, so it closes an open field or menu first like any other verb. `tile_columns` reports the plan's columns under the sheet's view with the cursor's column active, so the shell's `Edit column in view…` opens the Views dialog there. The frame observer re-reads the frame's effective scope (the empty scope under `:unscoped`), rebuilds when it changed, then the grouping chain (`read_grouping`: pin, slot pin, the frame's active slot, the view's own), rebuilds when either changed, then answers the flip barrier on every path; `rebuild` is the one structural build site — the scope, then `effective_chain` and `rollup::build` under one dimensions borrow, the group expansion pruned, then, under a sort, `sort::rank` over the rollup, then `GridIndex::build` and the window refill. A price delivery (`deliver`, `reprice_all`) runs the same front half through `reprice`; when the effective chain and the whole rollup are exactly equal to the previous ones and no line landed, it refills the window and keeps the index, re-resolving the selection so totals follow the prices. The build keeps a refusal as a standing header notice, and moves a cursor whose line the scope hid to the nearest shown line above (else below) in sheet order. The cursor is an `At` (a line within its group path, or a group path), so a split package's second row and a group row keep their place across a rebuild. |
| `tile::select` | `partly_hidden_refusal`, the one door every structural verb (`d`, `shift+j`/`shift+k`, `g p`, `g u`, and `:package`/`:unpackage`) passes before it mutates — a group row (`GROUP_ROW`), a split package or, for a move, a leg of one (`SPLIT`), a partly hidden one, a counted `g p` under a value grouping (`PACKAGE_GROUPED`), a move or counted `g p` under a sort (`MOVE_SORTED`, `PACKAGE_SORTED`); the `V`/`v` selection's state doors (start, clear, re-resolve, footer extent and totals), the selection verbs, the one-typed-value commit, and the live step (`bulk_step`, `settle_bulk`, `take_back_steps`). |
| `tile::reorder` | `move_lands`, the predicate and edge refusal every move steps by (shown; under a value grouping, the mover's own group); `grip_rows`, which rows paint a grip; the grip drag: the press's plan (`RowDragState`, a `DropPlan` or the keys' refusal), the gap under the pointer from the table's scroll geometry, the edge-band auto-scroll tick, the drop (one undo entry, cursor on the dragged row unless a selection holds it), and `escape`'s cancel. |

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

- `i` and Enter open text cell editors at the end; `I` (`shift+i`,
  `pricer::edit_select`) opens them with the whole text selected, in normal
  and selection modes. Both routes share the same edit guards; date fields and choice
  pickers open as usual. Text placement uses `geode_tile::edit::EditCaret`.
- A scope the pricer cannot evaluate refuses whole and hides nothing: an
  expression column `pricer` lacks, or an evaluator error on any one line
  (the SQL it mirrors fails the whole query). Never drop the failing term or
  line and keep the rest; that narrows the sheet in a way no query does.
  Hidden lines stay in the sheet and keep pricing and saving.
- A column whose sheet values can never equal the desk's must not narrow
  silently: `SheetRow` answers the desk's spelling where the cell paints
  another (`status` `fresh`, `expiry` ISO date), and `position_ref` /
  `instrument_ref` are not scope columns. A new column whose value is
  synthetic joins `NOT_SCOPEABLE`.
- An insert, put, undo or redo that lands a line the scope hides sets the
  footer `HIDDEN_LANDING` (`note_hidden_landing`); a line never vanishes
  without a word. An editor whose line the scope hides closes with
  `SCOPE_DROPPED_EDIT`, not `MOVED`.
- A package shows when any leg does. A partly hidden package's row paints its
  shown legs only (`· N of M legs`, summary, aggregates, fold, find key,
  selection totals) and is read-only: cell edits and every structural verb
  refuse with `PARTLY_HIDDEN`, a selection containing one refuses whole, and
  a counted `g p` over a hidden line refuses with `HIDDEN_IN_RANGE`. A new
  verb that mutates a package's legs must pass `partly_hidden_refusal`.
- A grouping value is the scope value (`SheetRow`): a group and a scope on
  the same column mean the same value, so `expiry` groups by ISO date while
  its tree label reads as the cell does. A group path keeps NULL (`None`)
  apart from the empty string, in the tree and in the session record.
- A group row and a split package's row are read-only (`GROUP_ROW`,
  `SPLIT`) through the same gates as a partly hidden package: each would
  reach lines another row paints. A group row's measures fold every leg
  beneath it and its dimensions read unanimity, never one leg's value; a
  split row folds only its node's legs, so a total over both rows of a
  calendar counts each leg once.
- Under a value grouping painted order is sheet order only inside each
  group: a move (keys or grip) lands only beside siblings painted in the
  mover's own group (`PricerTile::move_lands` over
  `reorder::Placements::same_node`), hopping other groups' lines, so no
  other group's painted order changes; a counted `g p` refuses
  (`PACKAGE_GROUPED`), keyed on `PricerTile::grouped`. A chain of
  structural levels alone is the flat sheet for both.
- A grip drag's plan is prepared at the press and re-prepared on every
  install from the movers' ids, so a rebuild mid-drag never drops onto a
  stale index; the re-preparation re-asks the keys' refusal
  (`move_refusal`: a sort, a read-only or split mover), so a sort turned on
  mid-drag refuses the drop. A drag that would reshape a live selection
  (its row outside a `V` selection, or any `v` block) ends it at the drag's
  first move (`ROW_MOVED_SELECTION`) — only when its plan can move rows:
  a refused drag keeps the selection and its refusal footer (a refusal at
  re-preparation sets that footer). The line shows only at a gap that
  moves something (`DropPlan::target`). The gap comes from the table's scroll geometry (uniform row
  height), never from render. The grip's click stops propagation, so the
  table's `SelectCell`/`DoubleClickedCell` never see it.
- A sort is display order only. `PricerTile::sort` names a vocabulary
  column; `rebuild_as` ranks the freshly built rollup BEFORE comparing it
  with the previous one, so a price that changes a measure sort's order
  rebuilds the index and one that does not refills only; with no sort the
  pass is skipped. The sheet, undo history and session never see the sort.
  `resolve_plan` drops a sort whose column left the plan and sets the
  warning notice (`sort_dropped`). Everything that walks the rollup's
  children (`GridIndex::build`, `find_targets`, `/`, `node_rows`) follows the
  ranked order; anything that needs sheet order reads the sheet or
  `legs_under`, which sorts its output. Under a sort a move and a counted
  `g p` refuse (`MOVE_SORTED`, `PACKAGE_SORTED`), as under a value grouping;
  `g p` over a `V` range apart in the sheet says `GROUP_SORTED`.
- A selection spans painted rows, so the order must not move under it:
  `start_selection` takes `held_order` (`sort::painted_order`) under a sort,
  every rebuild while the selection lives `sort::hold`s the ranked rollup
  back to it, and the first `sync_cursor` after the selection ends rebuilds
  in ranked order (the rebuild drops `held_order`). Every sort change,
  `resolve_plan`'s drop included, ends a live selection first through
  `end_selection_for_sort` (`SORT_CLEARED_SELECTION`). A verb that takes the
  selection before its edit uses `take_selection`/`restore_selection`, which
  carry `held_order` with it and rebuild on refusal, since the refused
  edit's rebuild ran selection-less in ranked order. The header's ` |x|`
  label is prepared by `SheetDelegate::set_sort`, never in render, and
  `:autosize` measures it.
- Yanks keep painted order: `y y` and the `V` rows dedupe in the order
  first met (`select::first_seen`), not by sheet row.
- A line never vanishes into a closed group: an insert, put, undo or redo
  queues its landed ids in `reveal`, and the next `rebuild` opens every group
  enclosing them between the rollup and the grid (`open_groups_of`), so
  `GridIndex::build` keeps its one call site. Find searches
  `grid::find_targets` — every row the grid would paint with every group
  open, in rollup preorder, keyed as those rows' `search` — and a match
  opens its groups (`land_on_node`).
- A fuzzy `/` builds an all-open `GridIndex` once per open (tree labels and
  search text only) and paints its measure cells from `delegate::FindPaint`:
  a `RowCache` of the rows the find table reports, formatted there through
  `CellPass` over the tile's live sheet. A price-only delivery drops them and
  the table re-reports, so the rows shown refill from the new prices. Once the
  tile installs another index, the next report drops every cell and the
  measure columns paint blank rather than read through a stale index, and the
  find's status line says the results are out of date (`FIND_OUT_OF_DATE`).
- `row_at` / `anchor_row` resolve a line painted exactly once to that row
  (`only_row`) wherever it now paints, so an edit or a delivery that changes
  the grouped value keeps the cursor and a `V` anchor on the line; only a
  split package, painted once per node, falls back to a group row on its old
  path. An editor whose line a rebuild leaves inside a closed group closes
  with `REGROUPED_EDIT`, not `MOVED`.
- `:group` whose every level `effective_chain` drops refuses and pins
  nothing (`:group 2` is not a count). `:group none` is the empty chain
  (`Pin::Grouping(vec![])`, session `pinned = []`), exempt from that
  refusal: the flat sheet pinned, a muted `ungrouped` in the header.
  `y y` and `V y` on a split package row yank that node's legs
  (`grid_rows_under`), as the row shows them.
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
- Every line carries a payout currency (`LineSpec`/`RowRecord::currency`,
  `Sheet::currency`), the one its request asks the pricer to report in. The
  shorthand names none, so a parsed line is blank until given one. A blank
  line has no request: it is not submitted or counted as pricing, and its
  status reads `needs currency` ahead of its state; a package with a blank
  leg fails its fold naming that leg (`<leg>: needs currency`). The
  `currency` cell edits (`Edit::SetCurrency`, a request change, so it
  reprices and undoes): exactly three letters in any case, stored upper
  case; empty text clears; anything else is refused with `a currency is
  three letters, e.g. USD`. A package cell maps codes onto its legs like any
  aggregated input. `Sheet::fill_currency` gives a blank line a currency
  without an undo step and never overwrites one; `lines_needing_currency`
  lists the blank lines. A result whose currency differs from the line's
  fails the line (`priced in EUR, asked for USD`) and installs nothing; a
  result that lands after a currency edit answers an older revision and is
  dropped. Yank copies the currency. `pricer_sheets` stores it in
  `currency`, its last value column (`""` when blank; a value that is not a
  code loads blank), so a sheet saved before a line had one reloads it blank.
  `Sheet::shown_result` is the result cells, sort keys, filters and totals
  read: a line still holding an answer in a currency other than its payout
  currency (after a currency edit, or once cleared) reads `Currency::MIXED`,
  so its local measures paint `—` and its `_usd` twins still read; a
  fold over such a leg is mixed too. Totals count a blank line as failed,
  and `status` sorts it as `needs currency`, as it paints.
- Package rows derive from their legs; they are not independent instruments.
  Their pricing timestamp is the oldest present leg-attempt timestamp,
  including failed attempts. The fold keeps the legs' currency when they
  agree and marks it `Currency::MIXED` when they differ: a package whose
  legs priced in different currencies paints `—` in its local-currency
  measure columns, while the `_usd` columns still sum. Its `currency` cell
  joins the legs' payout codes with `/` (a blank leg among set ones paints
  `—`), so the gap says which currencies met.
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
  the legs one per line. The grid keeps a line's or leg's shorthand as both
  its painted tree text and its find key; a package paints a template
  chip, a one-line summary of its legs' expiries and strikes, and a leg
  count, and its find key is its template form (a custom package's is its
  template token, underlyings and that summary). Loading accepts unresolved
  template names because stored instruments remain sufficient for repricing.
- `Sheet::apply` renames a package whose leg set an edit reshapes
  (`SetInstrument`/`SetQty`/`Move` on a leg, a leg `Insert` or `Remove`, a
  fresh `Group`) to `shorthand::identify`: its current name while the legs
  still fit that table, else the first fitting table, else `CUSTOM`. The
  leg edit's inverse ends with `SetTemplate` restoring the replaced name, so
  a stale name no table fits survives an undo. Root inserts, `Restore`
  (undo and loading) and `SetTemplate` never rename.
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
- A double-click on a package row's tree cell runs `tree_verb(None)` on it
  (`space`); only on a package's own row, since `tree_verb` takes a leg to
  its parent. A chevron press sets `chevron_anchor` to the line it toggled,
  which the next press's `SelectCell` moves into `pressed_chevron`, so a
  double-click whose first press was that line's chevron (already a toggle)
  toggles nothing more.
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
- `g m` opens the module picker on a `DimensionContext` holding the cursor
  row's underlying as `underlying_ref`. A package contributes an underlying
  only when all its legs share one; a mixed-underlying package gives an empty
  context and an empty sheet (no cursor row) gives none, so both open the
  plain tile picker.
- Emitting into a link group (`TileContent::emission`), the tile posts that
  same underlying (`cursor_underlying`, which `g m` also reads, so the two
  cannot name different underlyings for one row) as a one-value
  `underlying_ref` scope, and no board. A package across underlyings, a
  grouping row and an empty sheet post no scope, which leaves the group's
  scope as it was. It is read from the sheet on each pull: one row, nothing
  to keep in step. `emits` is true before a sheet has loaded: the shell
  drops a restored membership for a tile that answers false right after
  create. `watch_emission` observes the tile entity, which every cursor
  move, edit and load notifies. The tile stores no group; its header reads
  `link_chips` from its frame handle at paint, and a pricer that follows a
  group (`TileContent::follows` is true) applies that group's scope as its
  frame scope. An `:unscoped` pricer ignores a followed group's scope as it
  ignores the workspace's, while its header still shows the chip.
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
- `:e` of a name another tile holds arms a take-over confirm
  (`PendingSheet::Take`) instead of refusing. `submit_take` re-decides at
  `y`: retiring names refuse, this tile's refused flush stops it before the
  holder is touched, then the holder (found through `Shared::tiles`, never
  this tile, which is mid-update) runs `give_up_sheet`: it saves first (a
  refusal stops the take with the holder untouched), then closes its editor
  and entry bar (cancelled, not committed: line ids restart per sheet),
  sheet fields and menu, then `:new`. Only then does this tile switch; the
  holder's flush queued a save, so the load waits on it, and
  `PricerFactory::save_answered` paints a failure of that save on the
  waiting tile as well as on its origin.
- The `:rm` and take-over confirmations are `geode_tile::confirm`'s (one
  slot, payload `PendingSheet`): a focused prompt in
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
- Two rows paint a ground of their own, decided by
  `SheetDelegate::row_palette` and painted by `render_tr`: a grouping row
  (`Paints::group`) and every package leg (`Paints::leg`, a faint tint
  marking it as inside its package). A leg's or group row's cells, tree
  text, gutter and named colors take that palette; named colors are floored
  through one memo keyed by the row ground. Bare lines and package rows
  keep the table's ground and share the line palette; the template chip
  takes the neutral chip pair (`Paints::chip_fill`, `chip_text`), its text
  floored on the fill over all three row grounds. On three light themes no
  tint fainter than hover clears the selected ground; there the leg tint is
  held apart from hover and the group ground only (the sweep pins the
  list).
- A leg's connector is drawn, not a glyph: a hairline through the centre of
  its slot the full row height (to the stub on the package's last leg) and
  a stub at mid-height to the slot's right edge, in `Paints::connector`
  (`border`, opaque, floored to `NON_TEXT_RATIO` on the leg ground, hover
  and selected).
  The tree column has no vertical padding (`delegate::TABLE_SIZE`'s
  horizontal padding only) so the slot spans the row, and a leg that is not
  its package's last drops the row separator, so consecutive legs' lines
  join with no gap.
- A leg's connector sits `delegate::LEG_SHIFT` (one and a half depth steps)
  right of its package's chevron, under the chip's leading glyph, so the
  legs read as nested; their text starts as far right of the chip. A bare
  line's text starts where a package's chip starts. The
  tree cell lays out slot, chip, text and note with one `TREE_GAP` between
  each (`delegate::tree_gaps`); `fit_columns` and the `TREE_WIDTH` test
  measure the same parts. `TREE_WIDTH` fits `▾ CS Z26 4800/5200 · 2 legs` at
  the Large font; a longer summary ellipsizes and the leg count stays whole.
- The line-number gutter (`[ui] line_numbers`, read from the `UiSettings`
  global and observed) sits beside the tree cell, outside its depth indent,
  so numbers share one lane at every depth. The tree column widens by the
  gutter; the observer refreshes the table's cached widths. Numbers count
  cursor rows (lines, packages, visible legs) — the index `NG` jumps to and
  `Nj`/`Nk` count.
  Relative mode measures from the cursor row and numbers absolutely with no
  cursor row. `refresh_numbers` prepares the text and width outside render,
  before every `refresh`, keyed by row count, relative cursor row and
  mode. Gutter text uses the row palette's floored muted paint (the own
  text paint on the cursor row, `SheetDelegate::gutter_paint`).
- `paint` prepares grid-row text colors and tests their contrast across
  every bundled theme. Row text is checked against its own ground (line,
  leg or group), hover, and selection backgrounds; a second sweep checks
  the leg tint's distinctness (`LEG_TINT_RATIO`); menu colors are
  `geode_tile::menu::MenuPaint`'s.
- A view column's `color` paints as in the blotter: `sign` tints a negative
  measure bearish and a positive one bullish, a named color from
  `colors.toml` tints the column and its header (resolved through
  `geode_tile::colour::ColourCache`, invalidated when the factory's
  `colors` `Arc` changes); a stale cell stays muted and a failed one danger
  whatever the column's color (`paint::cell_colour`). Bearish, bullish and
  named-color text is not floored against the hover and selected row
  grounds the way the row palette is; a display check across the bundled
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
  A local-currency total over rows whose results are not all in one
  currency (a mixed package counts as differing) is `None` too; the `_usd`
  totals still sum.
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
- A right press on a cell, the tree cell or the row beside its cells emits
  `CellPointer::Context` (not in the open editor's own cell, whose press is
  the editor's; `is_editor_cell` is the one check for both buttons). The
  table stops a cell's right press after the cell's own listener, so the row
  hears only presses beside the cells; the shell captures its own. The
  listener records the row in the delegate's `pressed_row` before the emit
  (the `press_context` contract). On the event, inside a live `V` selection
  holding the row the cursor and selection stay, an open editor (the bulk
  one included) closes and the table's right-press row outline clears;
  otherwise it is a plain press's move (selection cleared, editor and entry
  bar closed, the cursor's column kept). `press_context` takes
  `pressed_row` once and answers its context
  (`context_at`, which `dimension_context` shares), with no anchor: the
  shell hangs the menu at the pointer.

## Known limitations

- The idle stale tone waits for a repaint. The last priced time's `stale`
  is decided when the tile renders, and the pricer does not use
  `geode_tile::stale::StaleTimer`: its timer is a reprice interval whose
  ticks repaint. With `refresh = "off"` an idle pricer is not stale-toned
  until its next repaint.
- The health chip never shows in production today. Its question is the
  `pricer_sheets` dataset, which is local: no source loads into it, so no
  source's health maps to it, and the pricer behind the pricing door reads
  no dataset. The chip stays silent until a real pricer declares a dataset
  that a source feeds and the tile adds it to its question.
- Column widths are fixed pixels and do not follow font size. The defaults
  fit the tested samples at the largest font step and leave more space at
  smaller steps. `:autosize` (or the palette's "Autosize columns") fits
  every column to its header and the rows in the window — the rows the
  table last asked to see — at the current rem size, so a wider value in a
  row never on screen, or a collapsed package's legs, is not measured. It
  refuses with "nothing loaded
  to fit" while the sheet loads or has no rows. It stores the widths by
  vocabulary name (`__tree` for the tree) in the session record. A font
  change does not rescale fitted widths; run `:autosize` again. A fitted
  width also overrides a view width changed later, until `:autosize reset`
  or the next `:autosize`, which replaces every kept width with a fresh fit.
- Columns can be dragged to reorder and resized with the pointer; both act
  on the open tile only. A drag lands in the delegate's `move_column` hook,
  which emits `ColumnMoved` (plan indices) for the tile to apply to its
  `ColumnPlan`; the cursor re-finds its column by name and the rebuild
  refills the window under the new order. A view change or reload rebuilds the plan from
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
