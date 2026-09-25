# geode-marketdata

The market-data panel module: one tile per `PanelSpec`, painting one
document of a document dataset as a grid (pivoted on two axes, or a row
per document row with the value columns laid flat) with a draft of unsent
edits over the top. CVI is the one panel spec built; the roster kind is
the panel's own (`cvi`) while every panel shares the `marketdata` key
context. `:upload [target]` sends the draft to an egress target after a
y/n confirm (see `tile.rs`'s `arm_upload`); the echo of a sent draft confirms
or holds it (`echo_of`).

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#market-data-documents).

## Layout

| Module | Holds |
|---|---|
| `core` | The pure half, no element, entity or window: `spec` (what a panel is), `matrix` (`MatrixModel::build`, the prepared grid a frame paints from, built once per delivery or edit), `draft` (edits keyed by cell and resolved across generations by row and column label), `cursor`, `menu` (the action list's rows and why each is or is not pickable), `nudge` (arrow-key stepping of an open editor's text), `upload` (`assemble`: the base generation plus the draft as the `DocumentRows` an upload sends, every value at its declared type; `echo_differs`: rows differing between what was sent and a delivered echo, ignoring a minted row label, `f64` within one ULP), and `datefield` (the segmented date editor). |
| `commands` | The `:` line: `:rebase`, `:revert`, `:auto`, `:bump`, `:upload` and the rest, parsed to data. |
| `header` | The header row, prepared once per change by `HeaderModel::prepare` and painted with no formatting of its own. |
| `tile` | `MarketDataTile`: requests one document by key through `DataHandle`, stages under the barrier, owns the cursor, the editor, the draft and the parked drafts per underlying. |
| `delegate` | `MatrixDelegate`, the `TableDelegate` over gpui-component's table. |
| `popup` | The tile-owned anchored popup for the `⋯` action list and the underlying picker. |
| `content` | The `TileContent` wrapper and `MarketDataFactory`, one per `PanelSpec`, plus the module's `ACTIONS` and `DEFAULT_KEYMAP` fragment. |

## Commands

```sh
cargo test -p geode-marketdata
cargo bench -p geode-marketdata    # matrix model and draft
```

## Rules this crate pins

`CLAUDE.md` has the full list under "Market-data panel". The ones a first
change most often hits:

- `MatrixModel::build` refuses holes, repeats, NULL axes and more than one
  value column under `Columns::Axis`. The flat build is a per-delivery and
  per-commit cost at the edge of the 8 ms budget for a 10,000-row
  schedule; such a panel must patch cells rather than rebuild.
- Every model swap goes through `install_model` and `TableState::refresh`.
  Column 0 is the row label and the cursor never enters it.
- Draft states: `Behind { newer }` keeps painting the base; `:rebase`
  re-places by label; `:revert` while `Behind` drops base and edits. The
  `:auto` policy is applied only on a real transition, never on a
  redelivery or the first delivery after a restore.
- `close_popup_with_window` is the one popup closer; it and `close_editor`
  blur only when their own field is focused, and `close_editor` blurs
  before dropping the `InputState`, in that order and both halves.
- A click anywhere, an attribute click included, cancels an open editor.
- Colours: state lives in the fill, text is `theme.foreground`
  (`cell_paint`); the header chips use `FlooredTones`. Three bundled-theme
  sweeps pin it.
- `KindAction`s (`cvi_reanchor`, `cvi_recalc_forward`) answer "not built
  yet". A built one is an egress request, never in-app arithmetic
  (`docs/PHILOSOPHY.md`: Geode is a lens, not a brain).
- `LABEL_WIDTH`/`CELL_WIDTH` are not on the rem scale, a known gap:
  `TableDelegate::column` has no window to read a rem from.
