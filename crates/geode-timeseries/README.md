# geode-timeseries

The timeseries tile: fetchable source series and arithmetic expressions plotted
across one or two panes, with statistics, density, and cursor inspection.
Sessions retain the query range and display settings; pan and zoom bounds are
not persisted.

Current behavior and rationale:
[`docs/current/features.md`](../../docs/current/features.md#timeseries).

## Layout

| Module | Holds |
|---|---|
| `core` | Pure model, range, source resolution, request building, chart-model preparation, session conversion, the action menu's rows, and the absolute `#rrggbb` colour with the picker's pick mapping (`core::rgb`). |
| `commands` | The tile-local `:` vocabulary. |
| `tile` | The retained entity, frame observation, verbs, `:` dispatch, focus, and chart cache key. |
| `tile::data` | Fetch submission, series queries, delivery filtering, and flip-barrier staging and promotion. |
| `tile::popups` | Opening, input routing, commits, cancellation, focus, and pointer controls for six transient surfaces, including the reusable component colour picker. |
| `tile::pointer` | Chart wheel, drag-pan, and split-drag gestures using chart hit testing. |
| `popup` | State and rendering for the series list, add picker, range editor, and action menu, plus expression-editor state. Series and picker rows share a row shell; menu rows, range fields, and the inline expression editor have separate renderers. The component renders its own colour picker. |
| `header` | Prepared chips and controls, the action-menu button, inline expression field, colour-picker trigger, and empty state. |
| `content` | `TileContent` wrapper, factory, actions, and keymap fragment. |

## Commands

```sh
cargo test -p geode-timeseries
cargo bench -p geode-timeseries
```

## Rules this crate pins

- `Changed` decides which work a mutation causes; do not replace it with a
  blanket query and rebuild.
- Fetch every slot still in `Fetching`; one completed pair may unblock several
  tiles.
- `Ok(0)` fetch completion still triggers a query because coverage is known.
- `ChartKey` contains everything chart preparation reads and excludes cursor
  movement.
- One closer owns every popup and blurs a focused editor before dropping it.
- `:` remains local to this tile.
- Menu picks and empty-state buttons dispatch registered actions. Other pointer
  controls share model operations and change processing with keyboard commands.
- Chart presses and ordinary header controls allow shell click-to-focus. The
  range readout prevents default ancestor focus while preserving propagation,
  so its newly focused date fields keep the keyboard. Popup rows and the
  component colour trigger consume their own presses.
- A pointer gesture ends at the same tail as its key: pan and zoom at
  `view_moved`, a split at `apply_changed`.
- The colour picker writes to the slot number it was opened for, never to the
  cursor, through `Model::set_colour` and `apply_changed`, the same path as
  `:colour`. The target and featured colours it writes against (`PickContext`)
  outlive the popup, because the hex field's `enter` closes the popover
  before its commit arrives.
- A pick is compared in 8-bit channels with a one-step tolerance
  (`core::within_a_step`), because the component's hex field truncates. A pick
  within a step of what the target already paints changes nothing, and one
  within a step of a featured colour is that colour. Slider steps apply live,
  so Escape or a click outside closes the picker and keeps any slider change
  already applied.
- The picker is an insert popup: `holds_focus` asks whether the picker's
  focus handle contains the focused element, so the hex field and swatches
  keep the keyboard away from the tile's single-key commands. A close the
  component makes by itself (a swatch pick) is blurred by the one closer
  before the element is dropped.

## Range editor

Type a preset as labelled (`1w`, `1m`, `3m`, `6m`, `1y`, `2y`, `5y`). The
leading bare digit narrows the chips; a second digit replaces it, and a matching
unit commits immediately, including with Shift. Invalid digits or units refuse
inline. While a label is pending, Enter refuses and Escape or Backspace clears
the label; the next Escape closes the editor. Other non-chord keys clear the
label before normal date routing.

A field key that changes state, or a segment click, makes subsequent digits
edit dates until the popup reopens. Tab and ineffective field keys leave
keyboard presets available. Preset chips remain clickable in either mode.
Enter without a pending label commits the fields as an absolute range, even
if unchanged; validation failures keep the draft open. Relative presets use
UTC calendar arithmetic and remain relative in sessions. Absolute ranges store
inclusive UTC dates; reopening them retains their stored dates despite as-of
clipping of queries.

## Colour and menu contracts

The action menu's `Colour…` opens the picker for the selected slot. Palette
and named featured colours retain their identities when selected; a custom
colour is opaque RGB8, persists as lowercase `#rrggbb`, and receives no theme
adaptation or readability adjustment. `:colour s<n> #rrggbb` accepts exactly six
hex digits in either case and stores Custom directly; it does not remap a
palette-identical hex value to a palette slot. Malformed session hex leaves
the restored slot's default colour. Cycling colour from a name or Custom
restarts at the first palette entry.

Featured colours are resolved at open. After the no-op check against the
current painted target, picks within one byte step per channel map to the
nearest featured entry by summed channel distance; ties keep the first.
Escape and outside close discard uncommitted hex preview, but preserve slider
changes already applied to the model.

Menu keyboard navigation skips disabled actions, separators, and headings.
Hover can still select a disabled action, which stays unlit; picking it reports
the refusal and leaves the menu open. Refresh preserves that selected action.
