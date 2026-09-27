# Grid selection: rows and cell blocks on every grid

Date: 2026-09-26. Status: approved design, awaiting spec review.

## 1. Intent

Every grid tile — blotter, market-data panel (CVI, Dividend), pricer — gets
two selection kinds with one shared behaviour:

- **Rows** (`V`): a contiguous range of whole rows.
- **Block** (`v`): a contiguous rectangle of cells, a row range × a column
  range.

A selection is an operand. It is copied, it is summarised in the footer, and
every row or cell verb that makes sense on it takes it as its operand
instead of the cursor row or cell. Pasting from the clipboard is out of
scope.

Success: the same keys and gestures select on all three grids; `y` copies
exactly the selection; the footer totals never double count a tree; pricer
`g p`, `d`, `J`/`K`, `y` and the market-data edit, nudge and `:bump` act on
the selection; every bulk edit is one undo or revert step.

### Rulings

1. `V` selects rows and `v` selects a block (vim's linewise and
   visual-on-a-grid). The blotter's `v` changes meaning: its old row
   behaviour moves to `V`.
2. Aggregation counts only **top-most selected rows**: a row contributes
   only if no ancestor of it is also selected.
3. The footer shows one aggregate per selected numeric column, never a sum
   across columns.
4. No paste in this work.

## 2. Current state (for orientation)

- Blotter: `core::cursor::{Cursor{row,col}, Mode::Visual{anchor: usize}}`;
  `selection()` gives a row range; `y` writes all-column TSV via
  `core::yank::tsv`. The anchor is a visible-row index and drifts on
  reflatten. The `visual` dispatch arm neither refreshes nor notifies.
- Market-data: `core::cursor::Cursor::{Cell{row,col}, Attr(usize)}`; `y`,
  `y y`, `y c`; `:bump <delta> [row|col]`; `d d`, `o`/`O`; no selection.
- Pricer: `tile::Cursor{line: Option<LineId>, col, last_row}`; `y y`, `y c`,
  `d d`, `J`/`K`, `p`/`P`, `g p` (count of roots), `g u`; undo via
  `Undo{inverse: Vec<Edit>}`; `Edit::Group{first, count, ..}` groups a
  contiguous run of root lines.
- All three use gpui-component 0.6.2 `DataTable`. Its `TableEvent`s carry no
  modifiers and it emits no drag events.

## 3. Architecture

A shared pure core plus per-tile adapters. No shared grid component.

### 3.1 `geode_core::grid::selection` (pure, no gpui)

```rust
pub enum SelectKind { Rows, Block }

pub struct Selection<R, C> {
    pub kind: SelectKind,
    pub anchor: (R, C),   // row identity, column identity
}
```

- `R` and `C` are identities, not indices: blotter row path, pricer
  `LineId`, market-data model row key; column name on every grid.
- `resolve(&self, cursor: (R, C), rows: &impl RowOrder<R>, cols: &impl
  ColOrder<C>) -> Option<Resolved>` returns the display-index row range and,
  for `Block`, the display-index column range (for `Rows`, all columns).
  `None` when the anchor row or column is no longer displayed.
- `Resolved` exposes `contains(row_ix, col_ix)` and the ranges; tiles build
  a per-change membership model from it (§3.4).
- `top_most(rows: Range<usize>, parent_of: impl Fn(usize) -> Option<usize>)`
  yields the display indices of selected rows with no selected ancestor.
- `aggregate(values) -> ColumnAggregate { count, sum, mean, min, max }`
  over `Option<f64>`; `None` and NaN are excluded from every statistic and
  from `count`.

### 3.2 Tile adapter obligations

Each tile supplies: the row identity at a display index and the reverse
lookup; the column identity at a display index and the reverse lookup; the
parent display index of a row (or `None` for flat grids); each cell's
numeric value for aggregation; and, for editing tiles, per-cell editability
and type acceptance. Each tile keeps its own cursor type.

### 3.3 Footer aggregate strip (`geode-shell`)

A `shell` element beside `shell::kbd` that paints a prepared
`Vec<(column label, ColumnAggregate)>`. Format:
`delta Σ 1.24m · μ 0.41m · n 3   gamma Σ 3.4k · μ …`. When the selection
covers one numeric column it adds `min` and `max`. Non-numeric columns are
omitted; if no numeric column is selected the strip shows the selected
row × column count only. Numbers use the tile's existing number formatting.

### 3.4 Rendering

- Selected cells tint with `theme.selection` at 0.35 opacity (today's row
  tint). `Rows` tints whole rows; `Block` tints only the block. The cursor
  keeps `table_active_border` inside the tint.
- Membership and aggregates are recomputed when the selection, cursor, row
  order, column order or data changes, and held on the delegate/tile.
  `render_td`/`render_tr` only look up. No per-frame resolution.

## 4. Behaviour

### 4.1 Lifecycle

- `v` / `V` from normal mode starts a selection anchored at the cursor.
- The other key while selecting switches kind and keeps the anchor; the same
  key again clears it.
- `escape` clears only the selection (not find, not narrowing). A second
  `escape` does what `escape` does in normal mode today.
- Every cursor motion extends the selection and clamps at grid edges rather
  than wrapping (the shell's `vimnav::apply_clamped`). A count moves the
  cursor (`3j`).
- Consuming verbs (`y`, `d`, `g p`, `g u`) end the selection. Repeatable
  edits (nudge, `:bump`, block commit, pricer `J`/`K`) keep it.
- Re-resolution after refresh, re-sort, filter, column move or column hide
  keeps the selection attached to the same rows and columns. If the anchor
  row or column is gone, the selection clears and a notice says so
  (`selection cleared: anchor row no longer shown`). No nearest-row guess.
- Market-data: while a selection is active the cursor cannot enter the
  header attribute strip; `k` at row 0 clamps.
- A selection is tile-local and not persisted.

### 4.2 Common verbs

| Key | With a selection |
|---|---|
| `y` | TSV to the clipboard. `Rows`: header of all shown columns, then each row (blotter keeps its indented tree text). `Block`: header of the block's columns, then the block. Ends the selection. |
| footer | Aggregate strip (§3.3) using the top-most rule (§1 ruling 2). |

In visual modes verbs are single keys (`d`, `y`); the doubled forms are
normal-mode only.

### 4.3 Blotter

Read-only: `y` and the aggregate strip. `V`+`y` reproduces today's `v`+`y`
output exactly. A collapsed group row in a selection is aggregated as
itself; its hidden children are not visited.

### 4.4 Market-data panel

- `i` / `enter` in a selection opens the editor on the cursor cell. Commit
  writes the value to every selected cell that is editable and whose column
  type accepts it. Others are skipped; a notice counts them
  (`set 12 cells, skipped 3 (read-only)`). If none accept, the commit
  refuses and the draft is unchanged.
- The insert-mode nudge (`up`/`down`, `shift` ×10) is a **live relative
  step** (ruling 2026-09-27):
  - Opening the editor on a selection snapshots the draft.
  - While the editor text is untouched, each step moves every selected
    editable Number cell from its own current value (an earlier edit
    included) by its own column's displayed precision. That is
    `nudge_text`'s rule applied per cell, so a block that spans ladder and
    slice columns steps each column at its own places. The steps are
    written to the draft at once and the grid paints them. The editor
    text follows the cursor cell's new value.
  - NULL cells, non-numeric cells and read-only cells are skipped. The
    notice counts both groups (`stepped 9 cells +12, skipped 3`).
  - All steps are validated before any is written, as `Draft::bump` does. A
    fractional step on an integer column refuses the whole press.
  - `enter` keeps the steps (nothing further is written). `escape` restores
    the snapshot, so one escape undoes every step since `i`.
  - Once the user types in the editor it is an absolute edit. Arrows then
    nudge only the text, as they do today, and `enter` writes that value
    to every accepting cell (above), replacing any live steps.
  - When the cursor cell is not numeric, the editor keeps its own
    behaviour (a date field's segment step, a choice list) and the commit
    is absolute.
  - The gates are `:bump`'s (`held_refusal`, `edit_base`: no document,
    `Behind`). Cells on a deleted row are skipped and counted.
  - `space` / `shift+space` (choice step) keeps acting on the cursor cell
    alone in this part.
- The header attribute strip and the row-label column are never selection
  members: a typed row label on many rows would collide, and attributes
  are not grid cells.
- `:bump <delta>` with no axis applies to the selection; with `row|col` it
  keeps today's behaviour.
- `d` in `Rows` deletes the selected rows. `d` in `Block` refuses:
  `d deletes rows — use V`.
- Each bulk operation is one draft change: one revert restores all of it.

### 4.5 Pricer

- `d` (`Rows`) deletes the selected lines and packages.
- `J` / `K` (`Rows`) move the selected block one sibling step as a unit. The
  selection must be siblings under one parent; otherwise refuse
  (`can't move: selection spans packages`). The selection stays active so
  the move repeats.
- `y` (`Rows`) copies the rows' shorthand to the clipboard and the register;
  `p`/`P` then put them all.
- `g p` (`Rows`) groups the selection into one package. It requires
  contiguous top-level lines (what `Edit::Group` accepts); otherwise refuse
  with the reason: `can't group: selection includes a package` or
  `can't group: lines are inside a package`.
- `g u` (`Rows`) ungroups every selected package; lines in the selection are
  ignored; no package selected refuses.
- `y` (`Block`) copies the block as TSV (§4.2) and leaves the register alone.
- Every bulk operation is one undo step: its edits apply as a batch whose
  inverses concatenate into a single undo entry. One `u` restores all of it,
  one `ctrl+r` redoes all of it.
- A count on `g p` keeps its meaning only with no selection.

**Cell edits over a selection** (ruling 2026-09-27):

- **Edits act on lines.** A selected package row stands for all of its legs,
  whether it is open or not. The edited set is the deduplicated set of lines.
  A line is itself; a package is its legs. A package and its own selected
  legs therefore never edit a leg twice. A call spread's strikes step or
  take a typed value leg by leg. The package's own `/`-list cell would
  refuse to nudge.
- **Columns.** Under `V`, `i` and the step act on the cursor's column only,
  down the selected rows. Otherwise one typed value would land in qty,
  strike, barrier and both shifts at once. Under `v` they act on the
  block's columns.
- **Typed value.** `i` / `enter` opens the editor on the cursor cell.
  - On commit, the text goes through each target line's own commit rule
    (`cell::commit`, or the expiry date commit).
  - A read-only column, a barrier on a vanilla line, or a value the
    column's rule refuses is skipped and counted in the notice
    (`set 6 cells, skipped 2 (read-only)`).
  - If nothing accepts, the commit refuses and the editor stays open.
- **Live step.** This is the same rule as §4.4. While the editor text is
  untouched, each `up`/`down` (`shift` ×10) steps every target cell.
  - Each cell steps from its own current text by that column's own nudge
    rule (`cell::nudge`), and the grid repaints. Cells that cannot step are
    skipped and counted.
  - A press is all-or-nothing, and a refused press writes nothing.
  - `enter` keeps every step as **one** undo entry. `escape` rolls back
    every step since `i`, but only while the steps are still the sheet's
    last change.
  - Typing turns the edit absolute.
  - Every step reprices the lines it touched through the tile's ordinary
    repricing path.
- A date or choice cursor cell keeps its own editor behaviour, and its
  commit is absolute.

**Typed value and package qty** (ruling 2026-09-27, execution). A typed
absolute value writes the cursor's column only under `v` as well as `V`:
one text parsed into several column grammars (qty `5` and strike `5`, a type
in the underlying) is a plausible wrong value, so a `v` typed commit no
longer fills the block's other columns. The relative live step still spans
the block's columns under `v`. The qty of a selected package, committed or
stepped, goes through the package's template weights (`package::commit`)
rather than leg by leg, which would flatten a spread into same-signed legs;
its legs are left out of the per-line targets, and a package in list form
is refused. This amends "Columns" and "Typed value" above.

**Footer** (ruling 2026-09-27). While a selection is live the footer shows
the extent. Beside it are totals of `price`, `delta`, `gamma`, `vega`,
`theta` and `rho` over the **top-most** selected rows (§1 ruling 2). A
package already sums its legs, so a package and its own legs never both
count. A refusal notice takes the footer while it stands.

### 4.6 Kind mismatch

Row-only verbs (`d`, `J`, `K`, `g p`, `g u`) in `Block` refuse with a hint
naming `V`. They never act on the rows a block happens to touch.

## 5. Mouse

- **shift+click** on a cell extends from the cursor; with no selection it
  starts a `Block` anchored at the cursor. On the line-number gutter it
  starts or extends `Rows`.
- **Drag** (press then move with the primary button held) across cells
  starts a `Block` anchored at the press; across the gutter, `Rows`.
- A plain click clears the selection and moves the cursor. Double-click
  keeps its current meaning.
- Implemented with mouse handlers on the delegates' cells and gutter that
  read `modifiers.shift` and `pressed_button`, reporting to the tile through
  the same selection entry points the keys use. The ordering relative to the
  table's own `SelectCell`/`SelectRow` emission is proven by a probe before
  the handlers are built (plan task 1). The blotter's
  `cell_selectable(false)` stays.
- A mouse-started selection must leave focus on the tile: a test clicks and
  then types a key.

## 6. Keymaps

Each tile gains a `mode == visual` context. `key_context` reports `visual`
whenever a selection is active (both kinds; the kind is a separate
`select == rows|block` pair for bindings that need it). Visual contexts bind
the motions, `v`, `V`, `escape`, `y`, and the tile's selection verbs from §4.
Normal contexts gain `v` and `shift+v`. Blotter's normal `v` rebinds from
the row mode to the block mode.

## 7. Testing

- Core: resolve in both directions and both kinds; identity survives
  re-sort, column move and a rebuilt row list; anchor loss returns `None`;
  `top_most` for group-plus-children, some children, collapsed group, flat
  grid; aggregates with NULL and NaN; kind switch.
- Tiles, through production routes (key dispatch, mouse events): `V j j y`
  exact clipboard on the blotter; `v l j y` block TSV; footer strip values
  for a group plus its children; market-data block commit with skip count,
  nudge, `:bump`, `d` refusal in `Block`, one-step revert; pricer
  `V j g p` then `u`, grouping refusals, `J` over a block, `y` then `p`;
  shift+click and drag followed by a typed key.
- Mutation-harness entries for: the top-most filter, identity anchoring, the
  single undo entry, the `Block` refusal, the anchor-loss clear.
- Display check: tint, cursor border inside the tint, footer strip, on a
  light and a dark theme.

## 8. Delivery

Three branches, each updating `docs/current/features.md`,
`docs/current/keymaps.md` and the crate README:

1. Core, footer strip, blotter (`V`, `v` blocks, aggregates, mouse, the
   missing notify).
2. Market-data panel.
3. Pricer (including batched undo).

## 9. Known limitations

- No paste.
- Selections are contiguous; no disjoint (ctrl+click) sets.
- Block edits apply one value; no fill series.
