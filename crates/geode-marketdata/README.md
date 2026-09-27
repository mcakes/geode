# geode-marketdata

The market-data panel module: one tile per `PanelSpec`, painting one
document of a document dataset as a grid (pivoted on two axes, or a row
per document row with the value columns laid flat) with a draft of unsent
edits over the top. CVI and DIVIDEND are the panel specs built; each
panel's roster kind is its own (`cvi`, `dividend`) while every panel
shares the `marketdata` key context. `:upload [target]` assembles the entire
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
| `core::spec`, `core::matrix` | Panel vocabulary and prepared grids built from a snapshot plus draft. |
| `core::draft` | Typed edits, label-based rebase, same-date group guards, and `DocumentBase` identity (source time plus optional store generation). |
| `core::upload` | Typed whole-document assembly and row-order-independent echo comparison; minted labels are ignored and floats allow one ULP. |
| `core::cursor`, `core::menu` | Grid navigation (wrapping, and `step_clamped` for a live selection) and the action list's rows (`geode_tile::menu` rows over action ids, hints as live chords). Numeric nudging and date fields are re-exported from `geode-core` and `geode-widgets`. |
| `core::bulk` | Selection-wide edit rules: whether a typed value lands in a cell of each kind, one arrow step's delta per column, and the `set`/`stepped` notices that count skips by reason. |
| `commands` | The `:` line: `:rebase`, `:revert`, `:auto`, `:bump`, `:upload`, `:autosize [reset]` and the rest, parsed to data. |
| `header` | Prepared identity, attributes, draft/upload feedback (notices through `geode_tile::notice`), source time, shared date-field rendering, and the menu control. |
| `tile` | `MarketDataTile`: requests one document by key through `DataHandle`, stages under the barrier, owns the cursor, the editor, the draft and the parked drafts per underlying. |
| `tile::select` | The `V`/`v` grid selection: its state doors, label-anchored resolution, and every verb that takes it as operand (`y`, `d`, `:bump`, the bulk commit, the live step and its undo). |
| `delegate` | `MatrixDelegate`, the `TableDelegate` over gpui-component's table. Holds `:autosize`'s fitted widths by column label (`__row_axis` for the row labels), which `column()` prefers over the fixed defaults. |
| `popup` | Underlying picker and cell-choice state and rendering over `geode_tile::popover`; the action menu is `geode_tile::menu`'s. Menu/picker anchor at the header; choices anchor beneath their target cell. |
| `content` | The `TileContent` wrapper and `MarketDataFactory`, one per `PanelSpec`, plus the module's `ACTIONS` and `DEFAULT_KEYMAP` fragment. |

## Commands

```sh
cargo test -p geode-marketdata
cargo bench -p geode-marketdata    # matrix model and draft
```

## Rules this crate pins

- `:autosize` and the shell's `tile::autosize_columns` run one method,
  `MarketDataTile::autosize_columns`, which measures every prepared row.
  With no document or no rows, a fit refuses with "nothing loaded to fit"
  and keeps its widths. The widths persist in the session record's
  `column_widths`. On a later
  document, a label that is no longer present is ignored and a new column
  takes the default width. Columns remain non-resizable by drag.

- `MatrixModel::build` refuses holes, repeats, NULL axes and more than one
  value column under `Columns::Axis`. Structural changes rebuild the prepared
  grid. Ordinary cell commits patch it when possible, avoiding a full schedule
  rebuild; edits to a Sent draft rebuild to clear sent presentation throughout.
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
  restored panel does not. Every panel kind accepts an underlying launch
  context.
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

## Input and popup contracts

The panel reports `normal`, `visual`, `menu`, or `insert` in the shared
`marketdata` context, with `select == rows|block` added whenever a selection
is live. An open editor or popup outranks the selection: a selection editor
is `insert`. Editors, underlying and choice inputs, and upload confirmation
use insert mode. Insert bindings leave shell chords available; confirmation consumes
every key, including chords, while armed; a pointer press on the tile or focus
leaving cancels it, and a delivery that moves the painted document withdraws
it silently. The upload confirm is `geode_tile::confirm`'s; after an answer
the shell's focus restoration path returns the keyboard to the tile.

Action-menu stepping, hover, picking and painting are `geode_tile::menu`'s:
the menu opens on its first enabled action; motion counts enabled actions and
skips disabled rows, headings and separators without wrapping, and from a row
that is not an action lands on the first enabled one; hover can light a
refused action, which takes no fill, and Enter or a click on it makes its
reason the notice and keeps the menu open; key hints are the live keymap's,
resolved when the menu opens, and follow a reload while the menu is open. A
key-only row whose action the keymap binds nowhere shows an empty lane;
`Upload`, `Rebase` and `Revert edits` fall back to their `:` verbs.

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
