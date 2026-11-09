# geode-calibration

The calibration graph tile: one pipeline of the `calibration` document over
one underlying, drawn as a DAG of stages. The tile computes no fit itself:
every run goes through the data tier's calibration door
(`DataHandle::calibrate`) and comes back as a `Delivery::Calibration`.

The crate holds the factory, the config push, the header, the data flow,
the painted graph with its cursor keys and filter, the run and publish
verbs with the y/n bar, the graph edits (connect, disconnect, add and
delete a stage, each a whole-object edit of the `calibration` document
through the shell's config path; `enter` opens a stage in the shell's
dialog), the `.` action menu, the `:` line, and the pure layout, render
model, menu rows and edits under `core/`.

## Module map

- `lib.rs`: `KIND` (`calibration`) and `init`, which reserves `tab` and
  `shift-tab` in the picker field's context (`tile::PICKER_CONTEXT`), so
  `Root`'s focus cycling cannot take the keyboard out of the name field.
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
  `menu.rs` builds the `.` action menu's rows over action ids (`Run from
  here`, `Run all`, `Publish`, then `Open stage`, `Connect…`,
  `Disconnect…`, `Add stage…`, `Delete stage`), each carrying the reason
  it cannot be picked now: no cursor (every row acting on the cursor's
  stage), no pipeline (`Run all`, `Add stage…` and the rest), or the
  publish refusal `p` would give. `Open stage` is the shell's
  `config::calibration` (`OPEN_STAGE`): its pick asks the shell through
  `FrameRef::request_action`, since the tile's dispatch does not carry a
  shell action.
  `edits.rs` is the graph edits as whole-object `ConfigEdit`s over the
  pipeline's typed ports: `open_port` (the first unfilled fixed port,
  which is the port at `inputs.len()` since inputs match fixed ports by
  position, or a variadic kind's slot, never filled), `connect_candidates`
  (same pipeline, not the stage, not an input already, emitting what the
  open port takes or, for a variadic kind with no gate yet, its gate type,
  and not downstream of the stage, which would close a cycle the reader
  refuses by dropping the pipeline), `connect_edit` and `disconnect_edit`
  (the stage's table with `inputs` extended at the port or struck),
  `delete_edits` (`None` for the stage, then each consumer's table without
  it, in document order, with the count of edges into those consumers),
  `add_table` and `add_edit` (`pipeline`, `kind` and the kind's required
  fields at the builtin document's values, no `inputs`), `valid_new_name`
  (`[a-z0-9_]+`, not `request`, not a name the document holds in any
  pipeline, since the writer would replace that stage's table), `KINDS`
  (the add picker's rows, the reader's kind words in its order) and
  `would_read` (the reader's verdict on a batch before it is written: the
  first diagnostic naming the pipeline or one of its stages when the
  pipeline would be dropped).
- `commands.rs`: the `:` line's vocabulary as data: `run [stage]`,
  `publish`, `pipeline <name>`, `layout`, with the usage line each
  refuses with, and `completions` (the verbs; stage names after `run`,
  pipeline names after `pipeline`).
- `content.rs`: `CalibrationConfig` (what the app pushes: the pipelines
  `from_doc` accepted and `check_against` kept, the schema the service
  serves, the egress targets with the documents each accepts, every
  finding of both reads, `sources`, every stage's merged table by name,
  read or refused, which the edits rewrite whole, `pipelines_table`, the
  merged `pipelines` table the pre-write read needs, and `inherited`, the
  winning non-user layer per stage a lower layer defines, which a delete
  refuses; `stage_tables`, `pipelines_table` and `inherited_stages` read
  them off the merged document and its layers for the bridge and the
  tests alike),
  `CalibrationFactory` (kind `calibration`, title
  `Calibration`, accepts `underlying_ref`, launch table `{ underlying =
  "<u>" }`, `set_config` to store a config and tell every live tile), the
  tile's `TileContent` door (`follows()` is `true`), `ACTIONS` and the
  `DEFAULT_KEYMAP` fragment.
- `tile/`: the hosted entity and its tests. `mod.rs` holds the state,
  `dispatch`, the `:` line's routing and `Render`; `data.rs` the data flow
  (source reads, the request builder, runs, outcomes, the pipeline switch)
  and `rebuild_model`; `keys.rs` the cursor moves, the `g N` jumps, the
  filter field's lifecycle, the scroll that keeps the cursor's card in
  view and the viewport measurement; `verbs.rs` the run, publish and
  delete verbs, the `ConfirmHost` (the y/n bar's payload
  `Pending::{Publish, Delete}`), the publish answer's uploads and their
  answers, and the edit queue (`queue_edits`, the one door every graph
  edit leaves through; `take_edit_notices`, the shell's answers read off
  the frame); `picker.rs` the edit pickers (the connect and disconnect
  lists, the kind list and the name field) and their popup; `menu.rs` the
  action menu's lifecycle and `MenuHost`; `pointer.rs` the pointer routes
  (a card's presses, the wheel, the port drag with its `PortDrag` payload
  and eligibility, the notice press), each going through its key's own
  function; `paint.rs` the body (the edge canvas, the captions, the cards
  with their ports and drop targets, the picker under the cursor's card,
  a right-press menu at the pointer) and the footer (the model's line or
  the filter field, the hint row for whatever holds the keys).
  `tile/tests/harness.rs` hosts the tile as the
  shell does: built by the factory, keys resolved through the app's keymap
  with the module's fragment spliced in (bare keys against the tile's
  context alone in `insert`), deliveries through `TileContent::deliver`,
  config through the factory, a frame drawn after every key; the edits a
  tile queues and the actions it requests are read off the stand-in frame
  as the shell's observer reads them, and the shell's answers are posted
  on it (`post_tile_notice_for_test`).

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
chip for the source stages' datasets, the `⋯` trigger (which opens the
action menu, as `.` does) and the shell's ×. The menu hangs from the
header's right edge (one a right press opened hangs from the pointer in
the body instead); an armed y/n question asks on its own bar under the
header, whole at any tile width, with Yes and No buttons.

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
whole line in a tooltip; a strip down the card's right edge, marked by a
dot at its middle where its edges leave, is the port a connect drag
starts from (`calibration-port-{tile}-{stage}`), with the grab cursor and
the bare control's hover fill. The layer captions sit above the first row
at each column's x. The body's size is measured in the canvas's prepaint
and, when it changed, handed to the tile after the draw, which rebuilds
the model once at the real viewport; nothing is laid out per frame.

The footer: the model's line (`waiting for a, b`, else `{cursor} ·
{status}`) at the left, or the filter field while `/` has it up; at the
right a hint row (`h j k l` move, `/` filter, `r` run, `shift+r` run all,
`p` publish, `c` connect, `a` add, `d` delete, `.` menu; `enter`
keep and `escape` revert while the field is up), each chord resolved
against the live keymap on a `Chords` change and omitted while its verb
is unbound.

## Keys

Every action is bound in the fragment.

| Key | Action |
|---|---|
| `enter` | The shell's `config::calibration`: open the cursor's stage in the Calibration stages dialog at its Edit screen; with no stage under the cursor (none, one the pipeline lacks, or a pipeline the reader dropped) the dialog opens on Browse. `config_object` names the stage; also the `⋯` menu's `Open stage` |
| `h` / `left`, `l` / `right` | Cursor to the first input (port order), to the first consumer (document order); else the nearest card of the adjacent layer |
| `k` / `up`, `j` / `down` | Cursor up and down among the stages of one depth, no wrap |
| `g 1`..`g 9` | Cursor to the Nth stage in layout order (layer-major, barycentre rows) |
| `/` | Filter stages: a field in the footer; typing dims the cards failing a fuzzy match as it goes |
| `shift+l` | Relayout the graph from the home scroll (also `:layout`) |
| `r`, `shift+r` | Run from the cursor's stage (`Run { from: Some(cursor) }`), run the whole pipeline; over the documents in hand, under the next run tag |
| `p` | Publish: asks `Publish {series} for {underlying} to {target}? y/n` on the bar; `y` sends `Publish { from: Filter }` on the user lane |
| `c` | Connect: a list under the cursor's card of the stages that may feed it (`Connect {stage} from`; each row `{name}  {kind}`), the highlighted one previewed as a dashed edge into the stage; `enter` queues the stage's table with the pick added to `inputs`; refused `{stage} has no open input` when its ports are all taken, `nothing left to connect to {stage}` when nothing fits |
| `x` | Disconnect: a list of the cursor's stage's inputs in port order (`Disconnect from {stage}`); `enter` queues the table without the pick when the reader still accepts the stage — a variadic or gate input of a publish — and refuses, in the reader's words (`calibration: stage 'parity': parity_fit takes 1 inputs (chain); 0 given`), an input whose loss leaves a fixed-arity stage short, which is changed in the dialog instead; refused `{stage} has no inputs to disconnect` with none |
| `a` | Add: a list of the kinds (`Add stage`, each with a line about it); a pick opens the name field (`Name the {kind}`, placeholder `stage name`); `enter` validates the name (`stage names are lowercase letters, digits and _`, `{name} is already a stage`, `request is reserved for the engine's answer`, the field staying up) and writes nothing: the seeded stage (`pipeline`, `kind`, the kind's required fields at the builtin's values) is proposed through `config_object` and the shell is asked for `config::calibration`, so the dialog opens at the new stage's Edit screen on a draft from the seed, where the inputs are ticked and the save is the write; the cursor stays |
| `d` | Delete: `Delete {stage} and its {n} edges? y/n` on the bar (`Delete {stage}? y/n` with none), `n` the edges into the stages that read it; `y` queues the removal and every reader's table without it as one batch, and the cursor goes to the stage's first input, else its first consumer, else the first other card. Refused for a stage a lower layer defines (`{stage} comes from the {layer} layer — there is nothing of yours to delete`: a user-layer removal would leave it in force) and, in the reader's words, when a reader would lose a required port |
| `.` | Action menu (also the header's `⋯`); `j`/`k` and the arrows step its enabled rows, `enter` picks, `escape` or `.` closes, a press outside closes |
| `escape` | End a live port drag, else answer an armed question no, else close the picker, else close the filter field, else close the menu, else dismiss the header's notices (`calibration::cancel`) |

The shell's shared `motion::*` ids (the palette's steps) move the cursor
as `h j k l` do. With no cursor, the first move lands on the first card in
layout order. Every move writes the session's `cursor` and scrolls the
graph the least that shows the card whole.

Under `mode == insert` (the filter field; the add picker's name field; an
armed question, whose prompt holds the keyboard as a field does) `enter`
commits, `escape` cancels and `up`/`down` step; under `mode == menu` with
`popup == actions` `enter` picks, `escape` closes and `.` toggles; with
`popup == list` (the connect, disconnect and kind lists, fieldless) `enter`
picks and `escape` discards, the steps being the shared `tilelist` ones
(`j`/`k` and the arrows), and `.` is not bound there: a verb reaching the
tile by the palette closes the list before it runs. A pointer press on a
row picks it, a press outside closes the popup. The filter follows the
shell's dialog-filter rule: `escape`
restores the filter in force when `/` was pressed; `enter` keeps the typed
filter (an emptied field clears it) and lands the cursor on the first lit
card in layout order, or leaves it when nothing matches.

## Pointer

Every pointer action is a key's twin and goes through the key's own
function (`tile/pointer.rs`), so the two cannot disagree. A plain left
press is left to propagate, so the shell's tile-focus press beneath still
arrives; a press with any modifier is the shell's (its tile drag and
fullscreen) and does nothing here.

| Pointer | Key | What happens |
|---|---|---|
| Left press on a card | `h j k l` | The cursor (`set_cursor`) |
| Double press on a card | `enter` | The cursor, then the shell is asked for `config::calibration` through the frame (`FrameRef::request_action`), the route a bound `enter` takes; the shell runs it for the focused tile, which the press focused |
| Right press on a card | `.` | The cursor, then the action menu hung by its top-left corner from the pointer in the body rather than from the header; the press stops at the card. A right press on the body off every card opens the menu for the cursor as it stands. `escape`, `.`, a pick or a press outside close it as they close the keyboard's |
| Wheel over the body | — | The tile's own scroll offset moves against the pixel delta (`ScrollDelta::pixel_delta` at the window's line height, so a line-wise mouse wheel and a trackpad agree) and clamps to `content - viewport` on each axis, never below zero. A horizontal delta scrolls x; so does a vertical one with shift held |
| Drag a card's port onto a card | `c` + `enter` | The port on the dragged card's right edge carries `PortDrag { tile, stage }`; when the drag passes gpui's threshold the tile computes once the cards it may land on (`core::edits::drop_targets`: every stage `can_feed` says yes to, `can_feed` being the connect picker's `connect_candidates` asked one name at a time) over the painted pipeline, and every card's `can_drop` reads that set through a handle, so no frame of the drag builds anything; a drag from another tile is never eligible. Over an eligible card the dashed preview edge paints as the picker's does; the drop target is the consumer and the dragged stage the input, and the drop asks `can_feed` again and calls the same validated `connect`, so a drop the reader would refuse shows the reader's danger notice and queues nothing. A connect that went out lands the cursor on the consumer, where `c` + `enter` leaves it, so `x`, `d` and `enter` act on the stage the edit landed on. An ineligible card never takes the drop: nothing previews, nothing is queued and nothing is said, since the drop was never offered. A drag passing the threshold closes the picker and the menu; the preview and the targets leave with the drag: a drop anywhere, a release off the tile, or `escape`, which ends a live drag through `calibration::cancel` (gpui alone would end it only on a release) and is spent on it |
| Left press on a header notice | `escape` | Dismisses the notice as a press always has and, when the notice names a stage (a config finding or the reader's refusal at `stages.<name>…`, kept beside the notice when it was made, never read off the painted text) whose card the painted graph has, lands the cursor on it |

There is no add by pointer: `a` stays the keyboard's and the menu's. A
card whose status was cut still shows the whole line in its tooltip;
hovering starts no drag, since a drag needs a press and a move.

## Commands and session

The `:` line reads `run [stage]` (the whole pipeline, or `stage` and its
consumers; `unknown stage '<s>'` for a name the pipeline lacks), `publish`
(the y/n question `p` asks), `pipeline <name>` (another pipeline of the
document: the results, documents and watches of the old one go, the
question and the menu close, a cursor the new pipeline lacks is dropped,
and a shown tile asks the new sources; `unknown pipeline '<name>'`
otherwise) and `layout` (the graph placed again from the home scroll, the
cursor left where it is). A refusal the key would show as a notice is
the line's answer here (`waiting for a, b`, `publish is held: …`); every
other line is refused `unknown command '<verb>'` or the verb's usage.
`completions` offers the verbs, then stage names after `run ` and
pipeline names after `pipeline `.

## Run and publish

A run (`r`, `shift+r`, `:run`) goes out over every source's documents in
hand under the next run tag, and is refused while no underlying is loaded
(`no underlying`), while a source is still owed (`waiting for a, b`), or
when the data handle's queue is full (`calibration queue full`); `r`
with no cursor is refused `no stage under the cursor`. The refusal is a
warning notice in the header, replaced by the next verb's and cleared
when a verb goes out; `escape` dismisses it.

`p` is refused, naming the object, when the pipeline has no publish stage
(`no publish stage in {pipeline}`), when that stage's last result is held
(`publish is held: {reason}`), and while a publish is in flight (`publish
already in flight`: the user lane is never replaceable in the worker, so
a second one would queue behind the first and run twice). Otherwise the
bar asks `Publish {series} for {underlying} to {target}? y/n`, the series
being those of the fits behind the publish stage's rasterize inputs, in
port order (`repo, box` for the builtin). `y` runs the checks again and
submits `RunAction::Publish { from: Filter }` on the user lane under the
next tag, remembered as the publish in flight; the delivered outcome,
matched by that tag and its `Publish` action, merges its stages as a
run's does and then sends `publish_rows` on, one `UploadParams { key,
tag, target, document, rows }` per document under the tile's own upload
counter. Rows on any other action's outcome go nowhere. Each upload's
answer is a notice: `uploaded {document} {key} to {target} {HH:MM:SS}`
(the key's parts joined with `/`, the time through the app clock); once
every document of the publish has landed, the publish card's status line
ends `· uploaded {HH:MM:SS}` after the engine's words. A failure is
`upload {document} {key} to {target} failed: {reason}` and keeps the time
off the card. An upload never rewrites the stage's result: only an
outcome does, so a publish the engine has since held is still held, still
paints its gate and still refuses `p`. An answer under a tag not out, or
another tile's, is dropped. The question is armed over one underlying,
document and target, all three checked again at `y`; a change of
underlying or pipeline withdraws an armed question unanswered, closes the
menu and forgets the publish and uploads in flight with the rest, so `y`
can never publish for an underlying the question did not name.

The session table holds `underlying`, `pipeline` and `cursor`, each while
set. A record without `pipeline` means the config's first pipeline, and
once the tile knows which that is it saves the name. The launch table
`{ underlying = "<u>" }` restores through the same reader.

## Graph edits

Every edit is a whole-object `ConfigEdit` on the `calibration` document
(`doc`, the bare stage name as `object`, `Some(table)` or `None`), queued
through `FrameRef::queue_config_edits`, which the shell drains on its
frame observer into the object dialogs' pending batch: one debounce, one
user-layer write (an inherited stage is forked, as a dialog edit forks
it), one reload, which pushes the new config and so the new graph. The
tile holds no draft: until the reload the graph is the old one, and a
picker list up when the config lands closes (its rows were about the old
pipeline; the name field stays, checked against the document at `enter`).

Every batch is read before it is written (`core::edits::would_read`: the
document rebuilt from the config's `sources` and `pipelines_table` with
the edits applied, read by `from_doc` and `check_against` over the
config's schema and targets). An edit that would drop the pipeline is
refused as a danger notice in the reader's own words and nothing is
queued, since a written refusal would come back one reload later as a
dropped graph the tile could no longer edit. So a connect writes the
stage's whole table with `inputs` extended at the open port; a disconnect
strikes only an input whose loss still reads (a publish's second raster
or its gate), and a required input is changed in the dialog `enter`
opens; a delete queues the stage's removal and every reader's rewrite in
ONE call, so no reload can see a reader naming a stage that is gone, and
only for a stage whose readers keep their required ports. A delete is also
refused for a stage a lower layer defines (`CalibrationConfig::inherited`,
the winning non-user layer per stage): a whole-object `None` removes the
user layer's copy and nothing below it, so the stage would stay in force
with its readers stripped; hiding an inherited stage needs a tombstone the
config path does not have (`docs/open-items.md`).

An add writes nothing. The name picked seeds a table (`pipeline`, `kind`,
the kind's required fields at the builtin document's values, no `inputs`)
held as `pending_add`, which `config_object` answers with the `seed`, and
the tile asks the shell for `config::calibration`; the shell reads the
proposal in its frame observer, which runs before the tile's own, and
opens the dialog at the new stage's Edit screen on a draft from the seed
(`render::open_new`), where the inputs are ticked and the save is the one
write. The tile's observer then clears the proposal, so `enter` names the
cursor's stage again.

The shell answers on the frame (`TileNotice`), read on the next frame
change: a fork is a status notice (the shell's words, `copied '{stage}' to
your config — r restores …`), kept until the next edit goes out; a
refusal (no writable user directory) a danger notice `escape` dismisses.
The connect picker previews the highlighted candidate as a dashed edge
into the stage (`preview_edge`, rebuilt on a highlight change only) and
drops it when it closes.

## Failure states

- `no pipeline` in the body: the config holds no pipeline of the tile's
  name, or none has been pushed.
- The last valid graph, under the finding's notice, when a reload dropped
  the shown pipeline (`graph_pipeline`: the name is absent from the config
  and a finding names one of the held pipeline's stages or the pipeline).
  The cursor still walks it; the runs, the publish and `enter` read the
  live pipeline alone, so they refuse (`no pipeline`) or open the dialog on
  Browse until the document is fixed. A pipeline absent for any other
  reason (renamed, never declared) is `no pipeline`.
- A header notice per config finding, in the message the diagnostics page
  shows (`calibration: pipeline 'p' dropped: …`); a click or `escape`
  dismisses it, and it shows again when the finding leaves and returns. A
  click on a notice whose finding is at `stages.<name>…` also lands the
  cursor on that card when the painted graph has it (see Pointer).
- A verb's refusal as a warning notice (see Run and publish); an upload
  failure as a danger notice. A menu row that cannot act says why in its
  lane, and a pick on it gives that reason as the notice and keeps the
  menu.

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
- A registered action's key is the tile's: `dispatch` answers `true` for
  every `calibration::*` id, except `cancel` with nothing to dismiss.
- The edits read the live pipeline and the config's `sources` alone,
  never the configuration: a held, dropped pipeline connects, adds and
  deletes nothing (`no pipeline`); a candidate is offered only if the
  reader would accept the result (type, arity, no cycle); and no batch is
  queued the reader would refuse (`would_read` runs before every
  `queue_edits`), so a tile edit never drops the pipeline.
- A delete never names a stage a lower layer defines: `None` removes the
  user layer's copy alone.
- One popup at a time: a picker opening closes the menu and the filter
  field; the name field is blurred before it is dropped.
- Only the latest request of each lane is listened for: a `Run` outcome
  must carry the last run's tag, a publish the last user action's, so a
  late run is never read as a publish answer; `publish_rows` are read only
  off a `Publish` outcome.
- Only an outcome changes a stage's result. An upload's answer counts
  toward the last publish and, when every document landed, hands the time
  to the model (`ModelInput::uploaded_at`), which trails it on the publish
  card; it never writes into the results.
- A question armed over one underlying dies with it: `clear_loaded`
  withdraws the confirm (and closes the menu) on an underlying flip and a
  pipeline switch, and `y` compares the question's underlying, document
  and target with the tile's before anything goes out.
- A verb other than the menu's own closes the menu before it runs; any
  verb but `cancel` reaching the tile past an armed question answers it
  no, as a pointer press anywhere on the tile does.
- The menu's rows are rebuilt with the model, so a row promises only what
  the results allow when it is picked; the `Publish` row and `p` give the
  same refusal.
