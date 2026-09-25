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
| `storage` | Conversion between sheets and document rows. |

The tile:

| Module | Holds |
|---|---|
| `store` | The `SheetStore` seam and the in-memory store. |
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
  (`save_blocked`); a change not yet accepted by the store (`dirty`) is saved
  when the tile closes. The save state has its own header slot, which pricing
  notices and `escape` never touch.
- In the free underlying typeahead, `enter` takes the highlighted option only
  when the query equals it case-insensitively or the highlight was moved with
  a key or a click; a pointer hover moves the highlight but does not count as
  moving it, so otherwise `enter` commits the typed text.
- A row's own ground (package, entry) is painted by `render_tr` on the row,
  never per cell, so the table's hover and selected-row fills stay visible.
- Every text colour the tile adds is floored against the ground it paints on
  and swept over every bundled theme with no exception list (`paint`),
  including the action menu's four.
- A menu command's title is its palette title (`content::action_title`); the
  menu's highlight never rests on a separator or section header.
- Default column labels and widths fit a worst-case value at the largest font
  size (`delegate`'s fit test): a right-aligned cell that overflows loses its
  leading digits.
