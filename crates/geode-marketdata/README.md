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
generation is compared separately: a matching echo clears the draft, while a
different echo retains it over its base. Switching underlying gives up that
draft's echo check; an outstanding outcome for another underlying is only a
notice. Request admission and transport success do not establish publication.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#market-data-documents).

## Layout

| Module | Holds |
|---|---|
| `core::spec`, `core::matrix` | Panel vocabulary and prepared grids built from a snapshot plus draft. |
| `core::draft` | Edits restored/rebased by row and column labels, including same-date group sizes that guard dividend rebases. |
| `core::upload` | Typed whole-document assembly and row-order-independent echo comparison; minted labels are ignored and floats allow one ULP. |
| `core::cursor`, `core::menu` | Grid navigation and available actions. Numeric nudging and date fields are re-exported from `geode-core` and `geode-widgets`. |
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
- `cvi_reanchor` and `cvi_recalc_forward` remain unimplemented and refuse.
  Ordinary document upload uses the adapter path without local recalculation.
- `LABEL_WIDTH`/`CELL_WIDTH` are not on the rem scale, a known gap:
  `TableDelegate::column` has no window to read a rem from.
