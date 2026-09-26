# Pricer entry bar and structural tree column

Status: approved in conversation 2026-09-26; spec awaiting review.

Amends the line-pricer design (`2026-09-19-geode-line-pricer-design.md`: §8.2 tree column,
§8.4 entry field).

## 1. Why

Column 0 paints each row's shorthand (`-5 SPX DEC26 95%/105% CS`). On a
line it repeats what the underlying, expiry, strike and type columns
already show, and nothing in it is actionable. It is also where the `o`
placeholder row opens, which makes an extra row that is not part of the
sheet slide into the table. Several click and double-click paths then
have to re-read their row after the entry closes, and five harness
entries guard exactly that shift.

Ruling (user, 2026-09-26): column 0 keeps only structure, and lines are
entered from a bar outside the table.

## 2. Column 0: structural only

- The layout is unchanged: the line-number gutter (when `[ui]
  line_numbers` is on), then the depth indent, then the fixed chevron
  slot, then a **tag**.
- **Tag.** A package row shows its template token (`CS`, `PS`, `STRD`,
  `STRG`, `RR`, `FLY`, `CAL`, `CUSTOM`), whether or not its legs still
  match the template. A line row or a leg shows no tag.
- **Width.** The column is a fixed width: the chevron slot, one indent
  step, and the widest token (`CUSTOM`) at the largest supported font
  size. The existing width check extends to cover it. The gutter still
  widens the column, as it does today. Every row keeps the same leading
  edge.
- `package_label` (the template + underlyings + expiries form) is
  removed. Nothing paints a row's shorthand any more.

## 3. Find

`/`, `n` and `N` keep matching the row's **full shorthand**, so `/SPX
DEC26` finds a row in any view, even when the matching columns are
hidden. `GridRow` keeps the shorthand as a search key that nothing
paints; `GridRow::tree` becomes `GridRow::search` (text) plus
`GridRow::tag` (painted). A package's search key is its template form
while its legs still match the template, and otherwise its tag followed
by its legs' distinct underlyings and expiries. That is today's
`package_label` text, which moves here, so no package loses a match it
has today.

`y y`, `p` and entry history read the shorthand from the `Sheet`, so they
do not change.

## 4. The entry bar

### 4.1 Opening

- `o` opens a one-line field in its own row directly under the header
  and above the column headers, and gives it focus. It sits where the
  timeseries expression editor sits in its tile.
- `shift+o` is removed. Its binding, action and palette entry go, and so
  does its row in the key table.
- Opening is refused while a sheet load is pending
  (`the sheet is still loading`), as it is today.
- `o` while the bar is already open does nothing (the field owns typed
  keys anyway).
- The placeholder hint stays `-5 SPX DEC26 95%/105% CS`.

### 4.2 Where lines land

The bar keeps today's `Entry { place, .. }` and the pure half in
`core::entry`, with one change:

- The place comes from `place_for(sheet, cursor_row, below = true)` when
  the bar opens. On a line, the new line goes below it. On a leg, it
  becomes the next leg of that package. On a package row, it becomes the
  package's first leg. A leg place opens its package.
- **Changed:** with no cursor row, the new line goes at the **end** of
  the sheet (`Place::Root { at: sheet.len() }`, a flat index), where
  today it goes at the start.
- After each successful `enter`, the place advances by `next_place`,
  exactly as now, so a run of `enter`s builds a block in the order it was
  typed. The cursor moves to each new line.
- `place_for`'s `below: bool` parameter goes, because every caller passes
  `true`.

Since no placeholder row shows the landing point any more, the bar shows
it as a muted label in front of the field (`core::entry::target_label`,
from the place alone):

- `Root { at }` with `at == sheet.len()`: `at end`.
- `Root { at }` otherwise: `after <root>`, where `<root>` is the root
  that owns flat row `at - 1`.
- `Leg { package, leg: 0 }`: `into <TAG>`, the package's template token.
- `Leg { package, leg }`: `after <leg>`, flat row `package + leg`.

A row is named by its shorthand, or by its template token when the
shorthand is empty or spans several lines (a custom package). The label
cuts with `…` at the bar's width. It is rebuilt when the place changes,
never per frame.

### 4.3 Keys in the bar

| Key | Effect |
|---|---|
| `enter` | Parse and insert. On success: reprice, move the cursor to the new line, clear the field, keep the bar open. |
| `up` / `down` | Walk the sheet's own lines as history (`core::entry::history`, unchanged). |
| `escape` | Close the bar: blur, then drop. Focus goes back to the table. |

- **Parse error or refused insert.** The text stays and nothing is
  inserted. The message appears in `Tone::DangerText` on a line under
  the field (a parse error with `(column n)`), not in the footer. Any
  edit to the text clears the message.
- **Clicking the table** closes the bar and then handles the click
  normally. There is no longer a placeholder row, so no row index shifts
  when the bar closes.
- **Any other verb or `:` command** closes the bar first, as today's
  `close_entry` does.
- The bar's field counts as insert focus to the shell, as today's entry
  field does, so typing fires no shell binding.

### 4.4 Removed

- `GridRowKind::Entry`, the `entry` parameter of `GridModel::build`, and
  `GridModel::entry_row`.
- The delegate's `entry` mirror, the entry arm in `render_td`, the entry
  row fill in `render_tr`, and the entry handling in `number_rows` and
  the gutter count.
- The tile paths that re-read a clicked row after the entry closes (the
  chevron, cell, double-click and tree-column cases).

## 5. Tests

Tests go through production routes (keys, clicks, focus):

- `o`, typing a line, `enter`, typing a second line, `enter`: both lines
  land below the cursor row in order, and the cursor is on the second.
- `o` with the cursor on a leg: the line becomes the next leg, and the
  package is open.
- `o` with the cursor on a package row: the line becomes its first leg.
- `o` with no cursor row: the line lands at the end of the sheet.
- A parse error keeps the text, inserts nothing, and shows the message
  under the field. Typing clears it.
- `escape` blurs before the drop, and focus returns to the table.
- A click on a table row while the bar is open closes the bar and lands
  on that row.
- Typing into the bar fires no shell binding.
- `shift+o` is unbound.
- The bar's target label reads `after …`, `into CS` and `at end` for
  the places in §4.2.
- Find matches shorthand text that no visible column shows. A package
  whose legs no longer match the template is found by an underlying.
- Column 0 paints the template token on a package row and nothing on a
  line or leg. Its width fits `CUSTOM` at the largest font size.

Harness entries:

- Retire the five entries that guard row re-reads after the entry
  closes, and the placeholder-press entry. The shift they guard no
  longer exists.
- Re-anchor `the entry field is dropped unblurred`, `the entry field is
  not insert mode` and `o below a leg lands before it` to the bar. The
  last one becomes `o on a leg lands after it`.
- Add entries for the no-cursor end placement, the error line clearing
  on edit, and the package tag.

## 6. Docs

- `docs/current/features.md`, pricer section: the tree column paragraph,
  the `o` / `shift+o` key row, and the entry bar.
- `crates/geode-pricer/README.md`: module map and invariants for entry
  and grid.
