# Pricer package rows: aggregated, editable text cells

Status: approved in conversation 2026-09-27; spec awaiting review.

Amends the line-pricer design (`2026-09-19-geode-line-pricer-design.md`)
§8.2, which paints a package's text cells blank and makes every package cell
read-only.

## 1. Why

A package row says nothing about what it holds except its template tag and
the sums of its legs. A trader reads a call spread's underlying, expiry and
strikes only by opening it. Changing a package's strikes or expiry means
editing each leg, which risks leaving the legs inconsistent.

Rulings (user, 2026-09-27):

- Package rows show `/`-separated aggregations of their legs' values.
- Package rows are editable. An edit maps **by position onto the displayed
  distinct values**: a single value goes to every leg, and a list with one
  part per distinct value replaces each one where it appears.
- The qty column shows and edits the **package quantity** (legs rescale by
  their weights) while the legs fit the package's template. Otherwise it
  falls back to the `/` list of leg quantities.

## 2. Display

On a package row:

- **Text columns** show the distinct values of the package's legs, in leg
  order, joined with `/`, each spelled exactly as a line's cell spells it.
  The text columns are `underlying`, `expiry`, `strike`, `type`, `barrier`,
  `barrier type`, `spot shift` and `vol shift`.
  - Examples:
    - call spread: `SPX`, `Z26`, `7400/7800`, `C`
    - straddle: strike `7600`, type `C/P`
    - calendar: expiry `Z26/H27`
    - fly: `7400/7600/7800`
  - Values are compared as values, not text (the cell editor's rule). A
    percent strike and an absolute strike are different values.
  - `barrier` and `barrier type` read only the legs that have a barrier,
    and are blank when none does.
  - A shift column spells each leg's shift as that leg's own cell would.
    The package cell is muted (`CellState::Inherited`) when every leg
    inherits the sheet value, and own otherwise. Distinctness is by the
    spelled text, so an inherited `2.0%` and an own `2.0%` read as one
    value.
- **Qty** shows the package quantity while the legs fit the package's
  template, computed as in `render_package`: the first leg's qty divided by
  the first leg's weight, then checked leg by leg. When they don't fit, qty
  shows the `/` list of distinct leg quantities.
- **Result columns** (price, greeks) keep their sums. Status and priced-at
  are unchanged.
- These cells are prepared by `GridModel::build` like every other cell,
  never in render. They paint in the package row's paint (§8.2's package
  ground).

Find (`/`) is unchanged: it still matches the row's search key.

## 3. Editing

`i`, `enter` and double-click on a package row's text column or qty open a
**text** editor on the displayed text. This includes expiry and type: the
segmented date field and the C/P typeahead are single-value tools, so they
stay on line rows. The result columns, status and priced-at stay read-only.

A commit is parsed with the same per-value parser the line cell uses
(`parse_expiry`, `parse_strike`, the underlying and type rules,
`parse_barrier_kind`, the shift parser). Every part is validated before
anything changes.

- **A single value** goes to every leg. For `barrier` and `barrier type` it
  goes to every barrier leg. For a shift, an empty value clears every leg's
  own shift (inherit), and a value sets it.
- **A `/` list** needs exactly as many parts as the cell displayed distinct
  values. Each leg whose value was the i-th distinct value gets the i-th
  part. Any other count is refused with a message naming the count and the
  current values: `2 values: 7400/7800`.
- **Qty in package form** (the legs fit the template): a single non-zero
  integer `q` sets each leg to `q × weight`, with checked arithmetic; an
  overflow is refused. A list is refused: `one quantity`.
- **Qty in list form**: maps by position like the text columns. Zero is
  refused.
- The whole commit becomes **one undo entry** and **one reprice**, through
  the tile's existing `apply_edits`. A commit that changes no leg is no
  edit.
- Legs that no longer fit the template after an edit (crossed strikes, a
  mixed expiry) are allowed. The package keeps its name as its tag and
  prints its legs one per line, which is the existing non-fitting
  behaviour.
- Up/down nudging applies when the field holds a single value; a list does
  not nudge.
- On a line row, editing is unchanged.

## 4. Code

- A new pure module, `core::package`:
  - `aggregate(sheet, row, def, format) -> CellText`, for display.
  - `commit(sheet, row, kind, text) -> Result<Vec<Edit>, String>`. An
    empty result means unchanged.
  - Both are built on one internal "distinct values in leg order, with the
    legs of each" helper.
- `columns::cell_text` routes package rows' text and qty columns to
  `package::aggregate`.
- `cell::editor_for` opens `CellEditor::Text(displayed)` for them.
- The cell commit path returns a list of edits for a package. The tile's
  commit calls `apply_edits` for a package and `apply_edit` for a line.
  How that seam is spelled is the plan's decision; the tile does not grow
  package logic.

## 5. Tests

- **Pure display**, for each column:
  - call spread, straddle, fly, calendar;
  - a risk reversal (`P/C` types);
  - barrier-only legs;
  - shifts, mixed own/inherited and all inherited (muted);
  - qty in package form and in list form.
- **Pure edits:**
  - a single value to all legs;
  - position mapping on a spread and on a fly (the body moves once);
  - a straddle's single strike;
  - a calendar's two expiries;
  - a wrong count refused with the message;
  - an invalid part refused with nothing changed;
  - qty rescaling a fly;
  - qty overflow refused;
  - qty list form;
  - an unchanged commit giving no edits;
  - crossed strikes allowed.
- **Tile, through production routes** (`i`, type, `enter`):
  - a spread's strikes move and reprice once;
  - undo restores every leg in one step;
  - the expiry column opens a text editor, not the date field, on a
    package row;
  - a refused count shows the footer message.
- Mutation entries for the position mapping, the count check, the qty
  rescale and one-undo.

## 6. Docs

`docs/current/features.md` (pricer section: package rows) and
`crates/geode-pricer/README.md`.
