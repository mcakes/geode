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

Transport success marks an unchanged Editing draft Sent. A later document
generation — a different source time, or the same time at a different store
generation — is compared separately: a matching echo clears the draft, while a
different echo retains it over its base. Switching underlying gives up that
draft's echo check; an outstanding outcome for another underlying is only a
notice. Request admission and transport success do not establish publication.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#market-data-documents).

## Layout

| Module | Holds |
|---|---|
| `core::spec`, `core::matrix` | Panel vocabulary and prepared grids built from a snapshot plus draft. |
| `core::draft` | Edits restored/rebased by row and column labels, including same-date group sizes that guard dividend rebases. `DocumentBase` (source time and store generation) is the base a delivery is compared against. |
| `core::upload` | Typed whole-document assembly and row-order-independent echo comparison; minted labels are ignored and floats allow one ULP. |
| `core::cursor`, `core::menu` | Grid navigation and available actions. Numeric nudging and date fields are re-exported from `geode-core` and `geode-widgets`. |
| `commands` | The `:` line: `:rebase`, `:revert`, `:auto`, `:bump`, `:upload` and the rest, parsed to data. |
| `header` | Prepared identity, attributes, draft/upload feedback, source time, shared date-field rendering, and the menu control. |
| `tile` | `MarketDataTile`: requests one document by key through `DataHandle`, stages under the barrier, owns the cursor, the editor, the draft and the parked drafts per underlying. |
| `delegate` | `MatrixDelegate`, the `TableDelegate` over gpui-component's table. |
| `popup` | Action menu, underlying picker, and cell-choice state and rendering. Menu/picker anchor at the header; choices anchor beneath their target cell. |
| `content` | The `TileContent` wrapper and `MarketDataFactory`, one per `PanelSpec`, plus the module's `ACTIONS` and `DEFAULT_KEYMAP` fragment. |

## Commands

```sh
cargo test -p geode-marketdata
cargo bench -p geode-marketdata    # matrix model and draft
```

## Rules this crate pins

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
- `close_popup_with_window` is the one popup closer; it and `close_editor`
  blur only when their own field is focused, and `close_editor` blurs
  before dropping the `InputState`, in that order and both halves.
- Grid and attribute selection close the previous editor; double-click opens
  the selected value. Date-segment clicks are consumed within the field so they
  select a segment without closing it. Header controls allow propagation for
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
  use flush segments inside the cursor border; header date fields retain
  their separate padded frame. Fixed column widths can still constrain dates.
- `LABEL_WIDTH`/`CELL_WIDTH` are not on the rem scale, a known gap:
  `TableDelegate::column` has no window to read a rem from.
- A panel opened through an add (palette, tile picker, `open_with`,
  duplicate) with no underlying opens the underlying picker at once; a
  restored panel does not. Every panel kind accepts an underlying launch
  context.

## Input and popup contracts

The panel reports `normal`, `menu`, or `insert` in the shared `marketdata`
context. Editors, underlying and choice inputs, and upload confirmation use
insert mode. Insert bindings leave shell chords available; confirmation consumes
every key, including chords, while armed.

Action-menu keyboard motion counts enabled actions and skips disabled rows,
headings, and separators without wrapping. Pointer hover can highlight a
refused action so its reason remains accessible.

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
