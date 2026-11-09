# geode-calibration

The calibration graph tile: one pipeline of the `calibration` document over
one underlying, drawn as a DAG of stages. The tile computes no fit itself:
every run goes through the data tier's calibration door
(`DataHandle::calibrate`) and comes back as a `Delivery::Calibration`.

This crate is at its early slices: the factory, the config push, the
header, the data flow, the painted graph with its cursor keys and filter,
and the pure layout and render model under `core/`. The run verbs, the
action menu and the edits follow in later slices; their keys and actions
are registered and bound now so a binding, a menu row or a footer hint
can name a verb before the tile carries it out.

## Module map

- `lib.rs`: `KIND` (`calibration`) and `init`, which reserves `tab` and
  `shift-tab` in the picker fields' context (`tile::PICKER_CONTEXT`) for
  completion.
- `core/`: the GPUI-free core, computed once per change and never per
  frame. `layout.rs` places a pipeline as a layered DAG in design pixels:
  a stage's layer is the longest path from a source, a layer orders by the
  barycentre of its inputs' rows then its consumers' rows (two sweeps,
  ties by document order), columns and rows spread evenly to the viewport
  down to `MIN_GAP_REM` and past that `content_w`/`content_h` exceed it so
  the tile scrolls; `caption` names each layer (`inputs`, `stage {n} ·
  fit`, `derive`, `check`, `publish`, joined with ` · ` when a layer
  mixes kinds), `neighbour` steps a cursor by `Move` (first input or
  consumer, else the nearest card of the adjacent layer; up and down
  within a layer, no wrap) and `nth` indexes layout order. `model.rs`
  builds the `GraphModel` paint reads: per card the title, `{kind} ·
  {detail}`, the `{HH:MM:SS} · {summary}` status (cut to `STATUS_CHARS`,
  whole in `status_full`), its `Tone`, cursor, preview and filter
  dimming (`geode_shell::palette::fuzzy_match`); per edge its endpoints
  and `EdgeTone` (`Preview` over `Gate` over `Into`/`OutOf`); the
  captions at their columns' x; and the footer line, which names every
  waiting source (`waiting for a, b`, document order) before the cursor's
  `{stage} · {status}`, since a restored cursor must not hide what blocks
  the pipeline.
- `content.rs`: `CalibrationConfig` (what the app pushes: the pipelines
  `from_doc` accepted and `check_against` kept, the schema the service
  serves, the egress targets with the documents each accepts, and every
  finding of both reads), `CalibrationFactory` (kind `calibration`, title
  `Calibration`, accepts `underlying_ref`, launch table `{ underlying =
  "<u>" }`, `set_config` to store a config and tell every live tile), the
  tile's `TileContent` door (`follows()` is `true`), `ACTIONS` and the
  `DEFAULT_KEYMAP` fragment.
- `tile/`: the hosted entity and its tests. `mod.rs` holds the state,
  `dispatch`, the `:` line and `Render`; `data.rs` the data flow (source
  reads, runs, outcomes) and `rebuild_model`; `keys.rs` the cursor moves,
  the `g N` jumps, the filter field's lifecycle, the scroll that keeps the
  cursor's card in view and the viewport measurement; `paint.rs` the body
  (the edge canvas, the captions, the cards) and the footer (the model's
  line or the filter field, the hint row). `tile/tests/harness.rs` hosts
  the tile as the shell does: built by the factory, keys resolved through
  the app's keymap with the module's fragment spliced in (bare keys
  against the tile's context alone in `insert`), deliveries through
  `TileContent::deliver`, config through the factory, a frame drawn after
  every key.

## Commands

```sh
cargo test -p geode-calibration
```

## What it paints

The header: `Calibration · {underlying}` (the followed group's single
underlying, else the tile's own), a muted `{n} stages · {k} fits` for the
shown pipeline (`k` counts its `kalman_fit` stages), then the shared
cluster: the mode icon, the config's findings as notices (a dropped
pipeline is a warning, a structural error danger), link chips, the health
chip for the source stages' datasets, the `⋯` trigger and the shell's ×.
The `⋯` trigger opens nothing yet.

The body: the graph while a pipeline is shown, or `no pipeline` when the
config has none of the tile's name (or no config has been pushed). The
graph is a clipped surface the tile scrolls by its own offset. A `canvas`
under the cards strokes every edge as a cubic Bézier from the right middle
of its input's card to the left middle of its reader's, both control
points at the horizontal midpoint, ending in a small disc at the reader;
the edge's `EdgeTone` picks its color (`Into` the foreground, `OutOf` the
ring, `Gate` the warning color dashed, `Preview` the ring dashed, `Quiet`
the border). Over it one absolutely placed card per stage
(`calibration-card-{tile}-{stage}`): the stage name, `{kind} · {detail}`,
the status line; the `Tone` colors the border and the status (failed
danger, held warning, waiting info, stale muted and italic); the cursor's
card takes the ring and the list's active fill; a card failing the filter
paints at reduced opacity; a status cut to `STATUS_CHARS` carries the
whole line in a tooltip. The layer captions sit above the first row at
each column's x. The body's size is measured in the canvas's prepaint
and, when it changed, handed to the tile after the draw, which rebuilds
the model once at the real viewport; nothing is laid out per frame.

The footer: the model's line (`waiting for a, b`, else `{cursor} ·
{status}`) at the left, or the filter field while `/` has it up; at the
right a hint row (`h j k l` move, `/` filter, `r` run, `shift+r` run all,
`shift+p` publish, `c` connect, `a` add, `d` delete, `.` menu; `enter`
keep and `escape` revert while the field is up), each chord resolved
against the live keymap on a `Chords` change and omitted while its verb
is unbound.

## Keys

Every action is bound in the fragment; the cursor, the jumps, the filter
and the relayout act, the rest consume their key and change nothing yet.

| Key | Action |
|---|---|
| `h` / `left`, `l` / `right` | Cursor to the first input (port order), to the first consumer (document order); else the nearest card of the adjacent layer |
| `k` / `up`, `j` / `down` | Cursor up and down among the stages of one depth, no wrap |
| `g 1`..`g 9` | Cursor to the Nth stage in layout order (layer-major, barycentre rows) |
| `/` | Filter stages: a field in the footer; typing dims the cards failing a fuzzy match as it goes |
| `shift+l` | Relayout the graph from the home scroll (also `:layout`) |
| `r`, `shift+r` | Run from the cursor, run the whole pipeline |
| `shift+p` | Publish |
| `c`, `shift+c` | Connect, disconnect an input |
| `a`, `d` | Add, delete a stage |
| `.` | Action menu |
| `escape` | Close the filter field, else dismiss the header's notices (`calibration::cancel`) |

The shell's shared `motion::*` ids (the palette's steps) move the cursor
as `h j k l` do. With no cursor, the first move lands on the first card in
layout order. Every move writes the session's `cursor` and scrolls the
graph the least that shows the card whole.

Under `mode == insert` (the filter field; later the picker fields) `enter`
commits, `escape` cancels and `up`/`down` step; under `mode == menu` with
`popup == actions` `enter` picks, `escape` closes and `.` toggles; with
`popup == list` `enter` applies and `escape` discards, the steps being the
shared ones. The filter follows the shell's dialog-filter rule: `escape`
restores the filter in force when `/` was pressed; `enter` keeps the typed
filter (an emptied field clears it) and lands the cursor on the first lit
card in layout order, or leaves it when nothing matches.

## Commands and session

The `:` line reads `layout` (the graph placed again from the home scroll,
the cursor left where it is); every other line is refused `unknown
command`. `completions` offers `layout`.

The session table holds `underlying`, `pipeline` and `cursor`, each while
set. A record without `pipeline` means the config's first pipeline, and
once the tile knows which that is it saves the name. The launch table
`{ underlying = "<u>" }` restores through the same reader.

## Failure states

- `no pipeline` in the body: the config holds no pipeline of the tile's
  name, or none has been pushed.
- A header notice per config finding, in the message the diagnostics page
  shows (`calibration: pipeline 'p' dropped: …`); a click or `escape`
  dismisses it, and it shows again when the finding leaves and returns.

## Invariants

- The key context does not opt into counts: the tile has no counted
  motion, and the stage jumps are `g N` chords, so a bare digit is nothing.
- The model is rebuilt when the pipeline, the results, the waiting
  sources, the cursor, the filter or the measured viewport change, never
  per frame; paint reads it and the strings prepared beside it (the card
  element names, the caption texts, the footer hints) and formats
  nothing. The edge canvas builds its paths per paint from the model's
  dozen edges; nothing else allocates per frame beyond the element tree.
- A card's status is cut to `STATUS_CHARS` (20): `CARD_W_REM` less the
  card's padding fits about that many `text_xs` glyphs, and the typical
  `HH:MM:SS · 6 strikes` is exactly 20.
- The tile reads no `Config` itself: everything it needs arrives in
  `CalibrationConfig`, pushed whole, so it never reads a half-updated
  configuration.
- A `set_config` push prunes closed tiles and holds no borrow across a
  tile's update; the tile reads the config just stored.
- Paint formats nothing: the title, the counts, the health question and
  the notices are prepared when the tile is built, when a config is pushed
  and when the followed group's underlying changes.
- A registered action's key is the tile's whether or not the verb acts
  yet: `dispatch` answers `true` for every `calibration::*` id, except
  `cancel` with nothing to dismiss.
