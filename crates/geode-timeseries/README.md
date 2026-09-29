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
| `core` | Pure model, range, source resolution, request building, chart-model preparation, session conversion, the three menus' rows (action list, range, frequency) as `geode_tile::menu` rows over `Pick`, the expression field's series-name completion (`core::complete`), and the absolute `#rrggbb` color with the picker's pick mapping (`core::rgb`). |
| `commands` | The tile-local `:` vocabulary. |
| `tile` | The retained entity, frame observation, visibility (hiding keeps the series query; `closed` cancels it and answers the barrier), verbs, `:` dispatch, focus, and chart cache key. |
| `tile::data` | Fetch submission, series queries and delivery filtering over `geode_tile::following` (the barrier staging and promotion rules), and the post-step `release_view` after each promotion and delivery. |
| `tile::popups` | Opening, input routing, commits, cancellation, focus, and pointer controls for six transient surfaces, including the reusable component color picker, the menus, and the range and frequency trigger doors. |
| `tile::pointer` | Chart wheel, drag-pan, and split-drag gestures using chart hit testing. |
| `popup` | State and rendering for the series list, add picker, custom dates editor and the expression field's completion list, plus expression-editor state; rows share `geode_tile::popover::row_shell`; the menus are `geode_tile::menu`'s. Date fields and the inline expression editor have their own renderers; the component renders its own color picker. The notice line paints through `geode_tile::notice`. |
| `header` | Prepared chips, the range and frequency triggers (each hangs its own popup and shows an open state while it is up), the action-menu button, inline expression field, color-picker trigger, and empty state. Paints through `geode_tile::header::frame` (22 px): badge, range and frequency triggers and slot chips on the left; the health chip (worst health over the series' sources; the series popup keeps per-series detail) and `⋯` from the shared cluster. |
| `content` | `TileContent` wrapper, factory, actions, keymap fragment, and the retired ids' renames (`RENAMED_ACTIONS`). The popups' row steps are not in the fragment: they are the shell's shared `motion::menu_down`/`menu_up`. |

## Commands

```sh
cargo test -p geode-timeseries
cargo bench -p geode-timeseries
```

## Invariants

- `Changed` identifies which mutations require fetching, querying, chart
  preparation, chrome refresh, or session persistence.
- Fetch requests cover waiting source/identity pairs, deduplicated within the
  tile. One completion may unblock several slots or tiles.
- `Ok(0)` fetch completion still triggers a query because coverage is known.
- `ChartKey` contains everything chart preparation reads and excludes cursor
  movement.
- One closer owns every popup and blurs a focused editor before dropping it.
- The series list and the three menus publish `tilelist` (beside
  `popup == series|menu`), so the shared menu steps (`j`/`k`, arrows) reach
  them as the internal `list_down`/`list_up` verbs; the list wraps, a menu
  clamps over its enabled rows. The tile never publishes `grid`, so its own
  `h`/`l` pan and `g`/`shift+g` jump are never shadowed by a grid motion.
- `:` remains local to this tile.
- A series is named by its label (`Slot::label`); slot numbers never reach
  the screen, a notice, or the `:` vocabulary. `:rule`, `:color`, `:yaxis`,
  and `:remove` take an optional series name first and otherwise act on the
  selection (`Model::target`); an expression is reachable only by selection.
- Names resolve in one place (`core::resolve::find_source`): an exact pair,
  else the default source's identity, else a unique identity. A name that
  fits several series is refused with their labels, never resolved to the
  first. Expressions reference source series only.
- `core::session` rewrites a pre-`version = 2` table's `sN` handles to names
  on restore: a source as its full `identity@source` (`resolve::name_for`,
  never the default-aware label), an expression operand as `(text)`. A text
  with no handle is read as current. A text it cannot rewrite becomes a
  `legacy` slot: failed, never sent (`request::params` skips it), saved with
  `legacy = true` so the next restore retries, and replaced by an edit.
  Numbering continues past every number a legacy text names, so the retry
  never resolves a handle to a newer series. Completion leaves out a label
  that names more than one series.
- Menu rows are prepared on open, on chrome rebuilds, and on frame changes
  while the menu is up; a rebuild keeps the highlight on its row, or snaps it
  to the nearest action. Key hints are action identities resolved against the
  live keymap at those moments and again when the keymap is republished, so
  an open menu follows a reload; an action bound nowhere shows an empty lane.
  Theme or named-color changes can trigger a chrome rebuild during render.
  A frequency the point cap refuses is disabled and carries the model's
  refusal; range presets are validated when chosen.
- Menu picks and empty-state buttons dispatch registered actions, or write a
  range or frequency through the model's own setters. Other pointer controls
  share model operations and change processing with keyboard commands.
- Chart presses and ordinary header controls allow shell click-to-focus. The
  range and frequency triggers toggle in the capture phase, like the `⋯`
  button, because an open popup's `on_mouse_down_out` would close it first.
  Popup rows and the component color trigger consume their own presses.
- Every painted popup's outside press goes through `outside_press`, which
  closes the popup only if it is still the one up: a trigger's capture-phase
  press may already have swapped another popup in.
- A pointer gesture ends at the same tail as its key: pan and zoom at
  `view_moved`, a split at `apply_changed`.
- A view move with statistics on requeries only when no series request is in
  flight; otherwise it waits and the answer (success or error) releases one
  request for the latest view. Superseding would interrupt the running query
  in the pool, so a pan faster than one query would leave the density strip
  and percentiles frozen until the pan stopped. Any other query change still
  supersedes at once.
- The color picker writes to the slot number it was opened for, never to the
  cursor, through `Model::set_color` and `apply_changed`, the same path as
  `:color`. The target and featured colors it writes against (`PickContext`)
  outlive the popup, because the hex field's `enter` closes the popover
  before its commit arrives.
- A pick is compared in 8-bit channels with a one-step tolerance
  (`core::within_a_step`), because the component's hex field truncates. A pick
  within a step of what the target already paints changes nothing, and one
  within a step of a featured color is that color. Slider steps apply live,
  so Escape or a click outside closes the picker and keeps any slider change
  already applied.
- The picker is an insert popup: `holds_focus` asks whether the picker's
  focus handle contains the focused element, so the hex field and swatches
  keep the keyboard away from the tile's single-key commands. A close the
  component makes by itself (a swatch pick) is blurred by the one closer
  before the element is dropped.

## Range and frequency menus

`r` (or the range trigger) opens the range menu: the seven presets written out
with their short labels as text, then `Custom dates…` with its live chord (`c`
as shipped) painted as a key. `f` (or the frequency trigger) opens the
frequency menu; a frequency the point cap refuses over the range as resolved
under the frame's as-of is a disabled row reading `over cap`, and choosing it
gives the full refusal as the notice. Both tick the value in force and open
with the highlight on it. There is no frequency step key.

The custom dates editor opens on From's day segment and digits edit the date
at once. Tab switches fields; Enter commits the fields as an absolute range,
even if unchanged, and validation failures keep the draft open. Escape returns
to the range menu with `Custom dates…` highlighted. Relative presets use UTC
calendar arithmetic and remain relative in sessions. Absolute ranges store
inclusive UTC dates; reopening them retains their stored dates despite as-of
clipping of queries.

The expression field completes loaded series names (`Model::series_names`).
`core::complete::name_at` finds the name at the caret with the expression
tokenizer's own character classes (`geode_core::series::expr`), so the word
boundary cannot drift from what the parser reads; a caret at a name's start
is in it, and a caret in a number offers nothing. The `Completion` list is
rebuilt on open, on the input's Change event, after Enter's expansion, and
on a `SeriesSettings` change, never in render. Tab and Shift+Tab reach the
field's own listener: the shell root reclaims them in `GeodeShell`, and
`crate::init` also binds them to `NoAction` in `EXPR_CONTEXT` for hosts
without that root. Each write is a `Write` applied as one range replace
(`set_selected_range` then `replace`), so it keeps the input's undo history;
its Change event is recorded as an echo and skipped, so the cached range
stays over the written name and repeated Tab cycles the same list. A Tab
whose caret is not where the last write left it re-ranks at the live caret
first. A cached range that does not fit the live text (bounds or a char
boundary at either end) writes nothing rather than panicking. Enter expands a
unique inexact name, then re-ranks, before resolving. A row press writes the
same way, stops propagation,
and the list surface occludes, so the shell root (which focuses only a hovered
hitbox) never takes the keyboard from the field.

## Color and menu contracts

The action menu's `Color…` opens the picker for the selected slot. Palette
and named featured colors retain their identities when selected; a custom
color is opaque RGB8, persists as lowercase `#rrggbb`, and receives no theme
adaptation or readability adjustment. `:color [series] #rrggbb` accepts exactly six
hex digits in either case and stores Custom directly; it does not remap a
palette-identical hex value to a palette slot. Malformed session hex leaves
the restored slot's default color. Cycling color from a name or Custom
restarts at the first palette entry.

Featured colors are resolved at open. After the no-op check against the
current painted target, picks within one byte step per channel map to the
nearest featured entry by summed channel distance; ties keep the first.
Escape and outside close discard uncommitted hex preview, but preserve slider
changes already applied to the model.

A menu opens on the enabled value in force, else its first enabled action.
Keyboard navigation skips disabled actions, separators, and headings, and
from a row that is not an action lands on the first enabled action. Hover can
still select a disabled action, which stays unlit; Enter or a click on it
reports the refusal and leaves the menu open. Refresh preserves that selected
action.
