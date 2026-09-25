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
  latest submission is dropped whole.
- The grid model is built on change and installed through `install_model`
  only, never in render.
- Both text inputs blur before they drop, and a click in the grid (a chevron
  included) cancels an open editor without committing it. `:` and `/` close
  the menu and any open field first.
- The tile arrives at flip barriers itself; it submits no view query.
- An empty sheet is never saved. A sheet whose load failed is never saved
  (`save_blocked`); a change not yet accepted by the store (`dirty`) is saved
  when the tile closes. The save state has its own header slot, which pricing
  notices and `escape` never touch.
- In the free underlying typeahead, `enter` takes the highlighted option only
  when the query equals it case-insensitively or the highlight was moved;
  otherwise it commits the typed text.
