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
| `store` | `SheetStore` load/save interface and the shared process-local memory store. |
| `grid` | The prepared `GridModel`, rebuilt on change. |
| `paint` | The per-theme paint memo, floored to a readable ratio. |
| `delegate` | The table delegate: cells, tree column, entry row, editor. |
| `header` | The prepared header row and footer. |
| `popup` | The typeahead and the `.` action menu. |
| `session` | The tile's session record. |
| `content` | The factory, keymap fragment, actions, and settings. |
| `tile` | `PricerTile`: modes, verbs, repricing, write-behind, load. |

The application uses `MemorySheetStore`; saved sheets survive tile close/reopen
within one process, but not an application restart. Session records retain
sheet names and UI state without persisting the sheet documents.

## Commands

```sh
cargo test -p geode-pricer
cargo bench -p geode-pricer
```

## Rules this crate pins

- Every edit passes through `Sheet::apply`, which returns the undo operation.
  New tile edits use `PricerTile::apply_edit`/`apply_edits`; undo and redo
  apply through the LIFO history. Loading replaces the sheet. Deliveries,
  stale marking, and sheet metadata updates have separate paths.
- Package rows derive from their legs; they are not independent instruments.
- Shorthand rendering uses a template only while the legs still match it.
- Storage conversion preserves stable ordering and explicit ownership of
  inherited versus row-level shifts.
- A submission carries every stale line; an outcome tagged older than the
  latest submission is dropped whole. Consecutive refusals overlay the header
  notice without replacing it, log once per streak, and schedule retries from
  one second up to a thirty-second cap. Admission or a submit with no further
  work needed ends the streak. Only one retry timer is pending at a time.
- A click resolves its grid row to a `LineId` before closing any field: the
  entry placeholder is a grid row, so reading the row after the close names
  the line below.
- Package expansion IDs survive edits because IDs are not reused, allowing
  undo to restore an open package. Loading prunes the set; session output
  includes only packages still present. Restoring a leg selects it and opens
  its parent if necessary.
- The grid model is built on change and installed through `install_model`
  only, never in render.
- Both text inputs blur before they drop, and a click in the grid (a chevron
  included) cancels an open editor without committing it. `:` and `/` close
  the menu and any open field first.
- Model installation resolves an open editor by line ID and column kind,
  updating its plan index and the cursor column together. If the line or
  column disappears, the field closes with `MOVED`; deferred window access
  blurs its retained input only if it still owns focus. Chrome rebuilds
  refresh open-menu rows and keep the highlight on an action or view.
- The tile arrives at flip barriers itself; it submits no view query.
- An empty sheet is never saved. A sheet whose load failed is never saved
  (`save_blocked`); a change not yet accepted by the store (`dirty`) is retried
  through one final attempt when the tile closes. A refusal at close loses
  the unsaved changes when the tile is released. The save state has its own
  header slot, which pricing notices and `escape` never touch.
- In the free underlying typeahead, `enter` takes the highlighted option only
  when the query equals it case-insensitively or the highlight was moved with
  a key or a click; a pointer hover moves the highlight but does not count as
  moving it, so otherwise `enter` commits the typed text.
- A row's own ground (package, entry) is painted by `render_tr` on the row,
  never per cell, so the table's hover and selected-row fills stay visible.
- `paint` prepares grid-row and action-menu text colours and tests their
  contrast across every bundled theme. Row text is checked against its base,
  hover, and selection backgrounds; menu text against popover and enabled
  highlight backgrounds. This sweep does not cover every header or typeahead
  token, and the bounded adjustment is not a guarantee for arbitrary themes.
- A disabled action can hold the menu highlight but paints no highlight
  fill. Picking it reports its reason and keeps the menu open.
- A menu command's title is its palette title (`content::action_title`); the
  menu's highlight never rests on a separator or section header.
- Default column widths are checked against labels and representative large
  values at the largest font size, including padding and cursor borders.
  These samples are not numeric limits: an overflowing right-aligned value
  can still lose leading digits.

## Known limitations

- The action menu's key hints are the default bindings, written into the
  menu; a user rebind is not reflected there. The market-data action list has
  the same limitation.
- Column widths are fixed pixels and do not follow font size. The defaults
  fit the tested samples at the largest font step and leave more space at
  smaller steps.
