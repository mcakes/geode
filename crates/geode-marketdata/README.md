# geode-marketdata

The market-data panel module: one tile kind per configured panel
(`panels.toml`), painting one document of a document dataset as a grid
(pivoted on two axes, or a row per document row with the value columns laid
flat) with a draft of unsent edits over the top. CVI and dividend ship as
builtin panel configuration (`core/builtin_panels.toml`). Each panel's
roster kind is its own name (`cvi`, `dividend`), while every panel shares
the `marketdata` key context. `:upload [target]` assembles the entire
painted document with its draft and asks for confirmation. Bare `y` submits;
any other key cancels and is consumed. The frame and painted generation must
both be live, and the tile allows only one outstanding upload.

Transport success marks an unchanged Editing draft Sent. A delivery with a
different source time, or the same time and a different known generation,
is checked separately against the sent document: a matching echo clears the
draft, while a different echo retains it over its base. Switching underlying
gives up that draft's echo check; an outstanding outcome for another
underlying is only a notice. Request admission and transport success do not establish publication.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#market-data-documents).

## Layout

| Module | Holds |
|---|---|
| `core::spec`, `core::matrix` | Panel vocabulary (re-exported from `geode_core::panel`), `BUILTIN_KIND_ACTIONS`, and the builtin panels (`BUILTIN_PANELS`, `builtin_panel`). `MatrixIndex`: labels, states, row sources and `label_index`, built on delivery, structural edit and bulk step; values read on demand from its own snapshot and the draft; `format_cell` is the one formatter. |
| `core::draft` | Typed edits, label-based rebase, same-date group guards, and `DocumentBase` identity (source time plus optional store generation). |
| `core::upload` | Typed whole-document assembly and row-order-independent echo comparison; minted labels are ignored and floats allow one ULP. |
| `core::cursor`, `core::menu` | Grid navigation over `geode_tile::motion` (the shared rules; `step_clamped` for a live selection), with the attribute strip outside the wrap cycle, and the action list's rows (`geode_tile::menu` rows over action ids, hints as live chords). Numeric nudging and date fields are re-exported from `geode-core` and `geode-widgets`. |
| `core::bulk` | Selection-wide edit rules: whether a typed value lands in a cell of each kind, one arrow step's delta per column, and the `set`/`stepped` notices that count skips by reason. |
| `commands` | The `:` line: `:rebase`, `:revert`, `:auto`, `:bump`, `:upload`, `:autosize [reset]` and the rest, parsed to data. |
| `header` | Prepared identity, attributes, draft/upload feedback, source time (and its prepared stale label), and shared date-field rendering. Paints through `geode_tile::header::frame`: kind badge, underlying and attributes on the left; state, incomplete rows, echo, upload error and the confirm prompt as cluster status; the notice, the time, the health chip (the panel's dataset) and `⋯` from the shared cluster. |
| `tile` | `MarketDataTile`: requests one document by key through `DataHandle`, runs its document request through `geode_tile::following` (following `as_of` and its watched document's data; hiding keeps the request in flight, `closed` cancels it and answers the barrier), owns the cursor, the editor, the draft and the parked drafts per underlying. |
| `tile::select` | The `V`/`v` grid selection: its state doors, label-anchored resolution, and every verb that takes it as operand (`y`, `d`, `:bump`, the bulk commit, the live step and its undo). |
| `delegate` | `MatrixDelegate`, the `TableDelegate` over gpui-component's table, and the window (`geode_tile::grid::WindowCache<MdCell>`) `render_td` reads. It fills the window for the range the table reports (`visible_rows_changed`) and refills the recorded range on every install; before any report it prepares the first `FIRST_WINDOW` rows. Holds `:autosize`'s fitted widths by column label (`__row_axis` for the row labels), which `column()` prefers over the fixed defaults. |
| `popup` | Underlying picker and cell-choice state and rendering over `geode_tile::popover`; the action menu is `geode_tile::menu`'s. Menu/picker anchor at the header; choices anchor beneath their target cell. |
| `content` | The `TileContent` wrapper and `MarketDataFactory`, one per accepted panel (only the first ships `DEFAULT_KEYMAP`; the rest are built `without_keymap`), plus the module's `ACTIONS`, `DEFAULT_KEYMAP` fragment (no grid motions and no menu steps: both are the shell's shared `motion::*` bindings) and the retired ids' renames (`RENAMED_ACTIONS`). |

## Commands

```sh
cargo test -p geode-marketdata
cargo bench -p geode-marketdata    # matrix index, window fill and draft
```

## Rules this crate pins

- `:autosize` and the shell's `tile::autosize_columns` run one method,
  `MarketDataTile::autosize_columns`, which measures the rows in the
  window — what the table last showed — never the whole document.
  With no document or no rows, a fit refuses with "nothing loaded to fit"
  and keeps its widths. The widths persist in the session record's
  `column_widths`. On a later
  document, a label that is no longer present is ignored and a new column
  takes the default width. Columns remain non-resizable by drag.

- `MatrixIndex::build` refuses holes, repeats, NULL axes and more than one
  value column under `Columns::Axis`. Structural changes rebuild the index; a
  one-cell commit writes the draft and refills that one window cell; edits to a
  Sent draft rebuild to clear sent presentation throughout. `serialize` (the
  session tick) captures group sizes from the installed index and builds
  nothing. `/`'s search text is prepared once per index build; a one-cell edit
  re-prepares its row under a hidden label.
- The delegate's window holds the rows the table last reported, filled in
  `visible_rows_changed` and refilled over that recorded range on every
  install (the table does not re-report an unchanged range), so painting reads
  prepared text and a delivery formats only the rows on screen. A cell the
  window lacks paints blank, and an open editor or choice popup on it still
  paints. Yank, a selection's TSV, find and the editors format on demand
  through `MatrixIndex::format_cell`, so they cover rows off screen and can
  never format differently from the window.
- A fuzzy `/` result table paints from `delegate::FindCells`: the index and
  draft as `/` opened them, and a `RowCache` of the rows the find table
  reports, formatted there through `MatrixIndex::md_cell`. Rows no longer
  reported are dropped, so it holds about a screenful.
- Model installation pairs the delegate update with `TableState::refresh` so
  cached headers follow the document. In CVI, table column 0 is a fixed row label
  outside the cursor grid. Dividend hides its row identity: column 0 is the first
  value column, fixed left, with no index offset.
- Draft states: `Behind { newer }` keeps painting the base when available; `:rebase`
  re-places by label; `:revert` while `Behind` drops base and edits. The
  `:auto` policy is applied only on a real transition, never on a
  redelivery or the first usable delivery after a restore. Without the saved
  base snapshot, a restored Behind draft paints the delivered grid while its
  unresolved cell edits remain withheld.
- `DocumentBase::differs_from` compares source time and, when both are known,
  store generations. Unknown generations fall back to source time, so they
  cannot detect a same-time republish. Retaining a base snapshot, capturing
  its group sizes, and reusing an echo verdict require exact pair equality.
  Sessions and parked drafts preserve known IDs in `base_generation`.
- Numeric cells keep their declared type through editing, draft persistence,
  and upload assembly. Integer parsing avoids a floating-point round trip;
  integer bumps validate all results before applying any edits and refuse
  fractional deltas or addition overflow. Bump deltas are parsed as `f64`.
- `close_popup_with_window` is the one popup closer; it and `close_editor`
  blur only when their own field is focused, and `close_editor` blurs
  before dropping the `InputState`, in that order and both halves.
- Grid and attribute clicks close the previous editor; double-click opens
  the selected value. A press inside the open editor's own cell (a value cell
  or a row label) belongs to the editor: the delegate reports no pointer event
  for it and the tile ignores the table's `SelectCell` on that cell, so caret
  placement never cancels an edit or drops a provisional row. Date-segment
  clicks are consumed within the field so they select a segment without
  closing it. Header controls allow propagation for
  shell focus handling; popup row presses are consumed above the grid.
- Cell paint precedence is deleted, sent, inserted, then edited. Deleted rows
  use muted strike-through with no fill; other marked cells retain foreground
  text over their state fill. Header/date-field tones and marked cell fills have
  bundled-theme contrast tests.
- `cvi_reanchor` and `cvi_recalc_forward` remain unimplemented and refuse.
  Ordinary document upload uses the adapter path without local recalculation.
- `BUILTIN_KIND_ACTIONS` is every kind action this tile dispatches; a panel
  offers them by id, and `geode-app` registers them. `builtin_panel` reads
  `core/builtin_panels.toml` structurally, for tests and benches only; the
  check against datasets and document kinds is `geode-app`'s, where a desk or
  user layer may replace or refuse these panels.
- `MarketDataFactory::new` leaks the panel's kind name once, because the
  shell keys kinds by `&'static str`. `panels` is restart-required, so
  factories are built once per launch and the leak is one short string per
  accepted panel per launch.
- The line-number gutter (`[ui] line_numbers`) sits beside the pinned cell,
  outside its cursor border, state fill, and deletion strike. Changing the
  setting refreshes the table's cached column widths.
  Numbers count painted rows, including inserts and marked deletions. Relative
  mode shows distance from the cursor, with an absolute number on its own row;
  while the cursor is in the attribute strip, all numbers are absolute.
- Cell editors share the text's alignment: row labels left, values right.
  Text inputs omit their own frame and horizontal padding. Grid date fields
  use flush segments inside the cursor border. Header attribute editors use
  the value box's existing frame, preserving its top and height; date fields
  also preserve its width, while text fields may grow. Fixed column widths
  can still constrain dates.
- `LABEL_WIDTH`/`CELL_WIDTH` are not on the rem scale, a known gap:
  `TableDelegate::column` has no window to read a rem from.
- A panel opened through an add (palette, tile picker, `open_with`,
  duplicate) with no underlying opens the underlying picker at once; a
  restored panel does not. Every panel kind accepts `["underlying_ref"]`, the
  `DimensionContext` column it opens on.
- The grid selection is anchored by row label and column name and
  re-resolved in `sync_cursor` on every cursor or model change; a lost anchor
  clears it with a notice, never a nearest-row guess. `cursor_to_attr` and
  `set_key` clear it first. `Resolved` and the footer extent text are
  prepared at those change points; render only reads them.
- A `Rows` selection's edits skip the leading `slice_columns`
  (`selection_cells`); a `Block` is its rectangle. Every selection edit is
  gated by `held_refusal` then `edit_base`, and judges every member before it
  writes any: a step is all-or-nothing, and a bulk commit that nothing accepts
  is refused with the editor open.
- A selection edit acts only from a member cell (`selection_holds`):
  `begin_edit` refuses a slice value inside a `Rows` selection, and
  `commit_bulk` and `bulk_step` check again. While a selection editor is
  open, `dispatch` refuses every verb that would move or end the selection
  (`changes_selection`). A delivery that loses the anchor still clears it;
  `bulk_step` then answers `None` and the arrows nudge the text.
- A selection editor carries a `Bulk` only on a number cursor cell. It holds
  the draft as `i` found it (`before`), as the last step left it (`after`),
  the painted base, and the upload state beside `before` (`sent`, the echo
  line, an upload error). Closing the editor restores `before` and that
  state only while the draft's work still equals `after` and the painted base
  is unchanged; a moved base keeps the steps with `steps kept: the document
  moved`, any other draft change closes silently. Every commit that keeps its
  value takes the `Bulk` out before `close_editor`, which would otherwise
  undo it, and every verb that reads the draft closes the editor first.
- Emitting into a link group (`TileContent::emission`), a panel posts its
  underlying (the first part of its document key) as a one-value
  `underlying_ref` scope; with no underlying it posts nothing, which leaves
  the group's scope as it was. The column name is a literal: every built-in
  document dataset keys first on `underlying_ref`, as `accepts()` and the
  launch path also assume, and a panel over a dataset keyed first on another
  column would post under the wrong column. Its board entry is the draft the upload
  builder assembles (`core::upload::assemble` over the painted base, the
  installed index and the draft): the panel's dataset, its document key and
  the whole document's rows. The entry is posted while the draft is not
  clean, a `Behind` or `Sent` draft included, so the board holds exactly
  what the panel paints; a clean panel and a reverted draft post no
  document. A draft the builder refuses posts no document either, and the
  scope is still posted.
- The assembled rows are cached (`Emitted`) on the underlying, the painted
  base snapshot's allocation and the draft. An unchanged pull returns the
  same `Arc`, which the frame compares by allocation and reads as no change;
  a republish at the same source time is another snapshot and is
  reassembled; a refusal is cached like a result, so a draft the builder
  refuses is walked once, not once per pull. The cache is touched only
  inside `draft_rows`, never by an edit route. It keeps one snapshot, one
  rows allocation and one draft clone alive until the next pull replaces
  them, or clears them because nothing is unsent: bounded at one document
  per panel.
- `emits` is true before an underlying is named or a document has arrived:
  the shell drops a restored membership for a tile that answers false right
  after create. `watch_emission` observes the tile entity, so every route
  that moves the underlying, the draft or the painted document must notify
  it: a one-cell commit, which refills its cell through the table entity,
  notifies the tile as well.
- A panel emits only. It can be set to follow a group and shows the chip,
  but it does not take its underlying from the group and does not read the
  frame's scope. The tile stores no group; its header reads `link_chips`
  from its frame handle at paint.

## Input and popup contracts

`i` and Enter open the cell editor with the caret at the end of its text;
`I` (`shift+i`, `marketdata::edit_start`) opens it at the start. Both routes
work in normal and selection modes and share the same edit guards. Text
attributes follow the same rule; date fields and choice pickers open as usual.
Text placement uses `geode_tile::edit::EditCaret`.

The panel reports `normal`, `visual`, `menu`, or `insert` in the shared
`marketdata` context, with `select == rows|block` added whenever a selection is
live. An open editor or popup outranks the selection: a selection editor is
`insert`. Editors, underlying and choice inputs, and upload confirmation use
insert mode. Insert bindings leave shell chords available; confirmation
consumes every key, including chords, while armed; its Yes and No buttons are
`y` and any other key; a pointer press anywhere else on the tile or focus
leaving cancels it. A delivery that moves the painted document or the
draft withdraws it unanswered (neither submit nor cancel runs) and says so in
the notice, `upload cancelled: a new document arrived`. The upload confirm is
`geode_tile::confirm`'s; after an answer the shell's focus restoration path
returns the keyboard to the tile.

Action-menu stepping, hover, picking and painting are `geode_tile::menu`'s:
the menu opens on its first enabled action; its steps are the shared
`motion::menu_down`/`menu_up` (`j`/`k`, arrows), which reach it through the
`tilelist` flag the panel publishes beside `mode == menu`, so the grid under
it never moves; a step counts enabled actions and skips disabled rows,
headings and separators without wrapping, and from a row that is not an
action lands on the first enabled one; hover can light a
refused action, which takes no fill, and Enter or a click on it makes its
reason the notice and keeps the menu open; key hints are the live keymap's,
resolved when the menu opens, and follow a reload while the menu is open. A
key-only row (`Load underlying…`, a kind action) whose action the keymap binds
nowhere shows an empty lane; `Upload`, `Rebase`, `Revert edits` and the three
policy rows (`:auto hold|rebase|replace`) fall back to their `:` verbs.

Underlying and choice lists retain every ranked match but paint a moving window
of at most twelve rows. Hover changes selection, and clicks commit. Underlying
selection survives catalogue replacement by key text; parked-draft phrases
only decorate labels and are not searchable. No-match text cannot become a new
underlying through the picker. Choice commits recheck the target's row/column
labels before writing. Both surfaces close through the focus-aware popup closer.

The command parser accepts `key` as an unlisted alias for `underlying`. `set`
normalizes spacing within multiword values; the tile validates the value type.
Completion returns unfiltered token candidates for the shell to rank. Rebase
is suggested only while Behind. Parsing and completion have different delimiters:
parsing splits whitespace, while completion also recognizes commas. Revert,
rebase, and menu ignore trailing words; bump also leaves tokens after its optional
axis unchecked. Upload, underlying, and auto reject extra arguments.
