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
| `core` | Pure model, range, source resolution, request building, chart-model preparation, session conversion, the three menus' rows (action list, range, frequency), and the absolute `#rrggbb` colour with the picker's pick mapping (`core::rgb`). |
| `commands` | The tile-local `:` vocabulary. |
| `tile` | The retained entity, frame observation, verbs, `:` dispatch, focus, and chart cache key. |
| `tile::data` | Fetch submission, series queries, delivery filtering, and flip-barrier staging and promotion. |
| `tile::popups` | Opening, input routing, commits, cancellation, focus, and pointer controls for six transient surfaces, including the reusable component colour picker, the menus, and the range and frequency trigger doors. |
| `tile::pointer` | Chart wheel, drag-pan, and split-drag gestures using chart hit testing. |
| `popup` | State and rendering for the series list, add picker, custom dates editor, and the menus (one painter for the action list, range menu and frequency menu), plus expression-editor state. Series and picker rows share a row shell; menu rows, date fields, and the inline expression editor have separate renderers. The component renders its own colour picker. |
| `header` | Prepared chips, the range and frequency triggers (each hangs its own popup and shows an open state while it is up), the action-menu button, inline expression field, colour-picker trigger, and empty state. |
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
- A series is named by its label (`Slot::label`); slot numbers never reach
  the screen, a notice, or the `:` vocabulary. `:rule`, `:colour`, `:yaxis`,
  and `:remove` take an optional series name first and otherwise act on the
  selection (`Model::target`); an expression is reachable only by selection.
- Names resolve in one place (`core::resolve::find_source`): an exact pair,
  else the default source's identity, else a unique identity. A name that
  fits several series is refused with their labels, never resolved to the
  first. Expressions reference source series only.
- `core::session` rewrites a pre-`version = 2` table's `sN` handles to names
  on restore, inlining an expression operand as `(text)`. A text it cannot
  rewrite becomes a `legacy` slot: failed, never sent (`request::params`
  skips it), saved with `legacy = true` so the next restore retries, and
  replaced by an edit.
- A menu's rows are built when it opens, on a chrome rebuild, and on a frame
  change while it is up, never in `render`; a frequency the point cap refuses
  is a disabled row whose reason is the model's own refusal, so a pickable row
  never fails.
- Menu picks and empty-state buttons dispatch registered actions, or write a
  range or frequency through the model's own setters. Other pointer controls
  share model operations and change processing with keyboard commands.
- Chart presses and ordinary header controls allow shell click-to-focus. The
  range and frequency triggers toggle in the capture phase, like the `⋯`
  button, because an open popup's `on_mouse_down_out` would close it first.
  Popup rows and the component colour trigger consume their own presses.
- Every painted popup's outside press goes through `outside_press`, which
  closes the popup only if it is still the one up: a trigger's capture-phase
  press may already have swapped another popup in.
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

## Range and frequency menus

`r` (or the range trigger) opens the range menu: the seven presets written out
with their short labels as text, then `Custom dates…` with `c` painted as a
key. `f` (or the frequency trigger) opens the frequency menu; a frequency the
point cap refuses over the range as resolved under the frame's as-of is a
disabled row reading `over cap`, and choosing it gives the full refusal as the
notice. Both tick the value in force and open with the highlight on it. There
is no frequency step key.

The custom dates editor opens on From's day segment and digits edit the date
at once. Tab switches fields; Enter commits the fields as an absolute range,
even if unchanged, and validation failures keep the draft open. Escape returns
to the range menu with `Custom dates…` highlighted. Relative presets use UTC
calendar arithmetic and remain relative in sessions. Absolute ranges store
inclusive UTC dates; reopening them retains their stored dates despite as-of
clipping of queries.

## Colour and menu contracts

The action menu's `Colour…` opens the picker for the selected slot. Palette
and named featured colours retain their identities when selected; a custom
colour is opaque RGB8, persists as lowercase `#rrggbb`, and receives no theme
adaptation or readability adjustment. `:colour [series] #rrggbb` accepts exactly six
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
