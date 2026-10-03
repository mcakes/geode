# Feature modules

Feature modules turn data and domain state into tiles. Each feature owns its
model, commands, prepared presentation, module factory, and tests. Features do
not depend on one another; they meet through shared vocabulary, the shell
contract, and the data service.

## Common lifecycle

`geode-app` registers a `ModuleFactory` for each available tile kind. The shell
creates an occupant for a `TileId`, sends its initial visibility and stack
state, and later routes actions, find events, local commands, and asynchronous
deliveries through `TileContent`.

A module owns its domain state. It observes the shared `Frame`, requests data
through `DataHandle`, and prepares the immutable or retained model used by its
renderer. Hidden tiles may release subscriptions. A hidden following tile
(blotter, market data, timeseries, vol slice) keeps its in-flight query, whose reply
applies when it lands unless a counter the tile follows moved since it asked;
such a reply is dropped, not applied. On becoming visible these tiles compare
followed versions and request anything stale. A closed following tile cancels
its query and answers any flip barrier still waiting on it. The pricer's own
hide rule is described with the pricer.

Every module follows these interaction rules:

- `:` changes only the focused tile; global changes use actions and palette
  flows.
- `/` routes find input to the tile when it supports local search.
- Pointer commands have keyboard equivalents.
- Insert-mode inputs own focus only while editing and blur before they close.
- Stack state is visible through the shared marker in the module header.
- Delivery matches are exhaustive so a new outcome cannot be ignored silently.

## Shared tile interaction

Modules build their popups, `.` action menus, in-tile y/n confirms and
notice lines from `geode-tile`, so each rule below holds in every tile that
has the surface. The pricer and market-data use all four, timeseries the
popups, menus and notice line, the vol slice viewer the popups and notice
line, and the blotter the notice line. Diagnostics has none of them.

- Grid motions and menu steps are the shared motion vocabulary; see
  [Motion](#motion).
- A popup is deferred above the tile's clip and snaps inside the window with
  an 8-pixel margin. An action menu occludes what it covers, so its hover and
  presses do not reach the tile beneath.
- A menu opens on its first enabled action (timeseries' range and frequency
  menus on the enabled value in force). Key hints resolve from the live
  keymap when the menu opens, when its rows rebuild, and when the keymap is
  republished, so an open menu follows a reload. An action the keymap binds
  nowhere shows an empty lane, or its `:` verb on a row that has one
  (`:price`, `:upload`, `:rebase`, `:revert`, `:auto hold`).
- Keyboard stepping lands only on enabled actions, clamped at either end
  without wrapping; from a row that is not an action it lands on the first
  enabled action. A pointer can light a disabled row, which takes no
  highlight fill. Enter or a click on a disabled row shows its reason and
  keeps the menu open; an enabled pick closes the menu before it dispatches.
  Rows rebuilt under an open menu keep the highlight on its row, or snap it
  to the nearest action.
- A confirm holds the keyboard: bare `y` or the prompt's Yes button
  confirms; any other key, chords included, cancels and is consumed, as does
  its No button; a pointer press anywhere else on the tile (the gap between
  the buttons included) or focus leaving also cancels. A press on a button
  moves no focus and does not cancel first, so the question stands until the
  click lands. A change that moves what the question is about
  withdraws it unanswered: neither the confirm nor the cancel action runs,
  and the prompt's blur is not heard as an `n`. Market-data withdraws on a
  delivery that moves the painted document or the draft and says so in its
  notice (`upload cancelled: a new document arrived`); the pricer withdraws
  on a `:` command with no notice of its own. Each answer blurs the prompt before it
  drops, and the shell's focus restoration path returns the keyboard to the
  tile.
- A notice is a status (muted), warning or danger line in the theme's text
  tones; which of a tile's notices shows is the tile's own precedence.
- Every tile header is `geode_tile::header::frame`: 22 px at the design rem,
  the stack marker first, the module's own left side, then a right cluster in
  a fixed order — the mode icon, status items, notices, source times, the
  health chip, `⋯`.
  Status items and notices shrink: each is one line, cut with an ellipsis
  when it does not fit, and together they take at most half the header
  (`TEXT_SHARE`); a cut notice shows its whole text in its tooltip. Source
  times, the chip and `⋯` never shrink. The left side takes what remains and
  clips, so neither a long left side nor a long notice pushes the times, the
  chip or `⋯` off the tile — they leave it only when the tile is narrower
  than those three alone.
  A stale source time takes the warning text tone. It turns stale while the
  tile is idle, too: market-data arms a wake-up at its source time plus
  `stale_after`, and the blotter one per dataset time, each firing in turn so
  every time run turns stale at its own deadline. They are re-armed by each
  delivery, on show and on a frame flip, and dropped while hidden. A
  reload that changes `stale_after` sets the shared threshold before the
  frame flips, so that flip re-arms an idle tile against the new value. A newer delivery
  clears the wake-up's verdict, so it paints fresh. The health chip appears
  only while a source the tile reads is PendingTooLong (`pending`), Degraded
  (`degraded`) or Failed (`failed`); its tooltip names the worst source and
  its reason, with `+N more` for other unhealthy sources. Clicking it opens
  the diagnostics page and never closes it; it has no key of its own — the
  page's own binding (`mod+d`) is the keyboard route.
- The mode icon is a bare glyph, no fill, at the header's text size: a
  pencil in the floored warning text tone while the tile is in edit mode, a
  dashed selection square in the floored info text tone while it is in
  visual mode, nothing in normal mode or while the action menu is up. Each
  module reads it from the same `mode` its key context publishes (`insert`
  is edit), so the icon and the keys cannot disagree: the pricer and market
  data show both, the blotter only visual (it has no field), the timeseries
  tile only edit (while its add, expression, dates or color popup holds the
  keys; it has no selection). Its tooltip names the mode (`Editing`,
  `Visual selection`) and that `escape` leaves it. The tooltip names the
  default leaving key, `escape`, not a rebound one. Both colors clear 4.5:1
  against the background on every bundled theme.

### Motion

The motion keys are one shared vocabulary: the shell's `motion::*` actions
(palette category "Motion"), bound once in the builtin keymap and applied by
`geode_tile::motion` (see [shared motions](keymaps.md#shared-motions)). A tile
whose grid cursor takes motions publishes `grid`, and every grid motion is
bound under the one context `grid && (mode == normal || mode == visual)`, so
remapping a motion in the keybindings dialog changes it in every grid tile at
once. The blotter, the market-data panel and the line pricer move on them, and
so does the diagnostics page's cursor (rows only). The rules:

- A bare `j`/`k` (`down`/`up`) wraps at the ends, except while a selection is
  live, where it clamps: wrapping past the anchor would invert the selection.
- Every counted move clamps, `1j` included.
- A bare `g g`/`G` goes to the first/last row; a counted one goes to that
  1-based row, clamped to the last (`5G` and `5gg` both land on row 5).
- `ctrl+d`/`ctrl+u` move 5 rows and `ctrl+f`/`ctrl+b` (and
  `pagedown`/`pageup`) 10, times the count.
- `h`/`l` clamp at the first and last column; `^`/`home` and `$`/`end` go to
  the first and last column.
- The arrow keys are bound beside the vim keys, so they move every grid tile.
- A grid with no rows or columns ignores every motion.

Each tile's own behaviour around the shared result:

- Market-data: any upward row motion from row 0 (`k`/`up`, `ctrl+u`,
  `ctrl+b`, `pageup`), bare or counted, enters the header attribute strip
  instead of wrapping when the kind has attributes (see
  [market-data](#market-data-documents)).
- Pricer: a motion dispatched from the palette while the entry bar, the cell
  editor or the action menu is open closes it first, then moves.
- Diagnostics: a bare `G` in the Log section resumes following the tail; any
  other row motion, a counted `G` included, stops following.
- Blotter: the column motions move its column cursor.
- Timeseries has no grid cursor and never publishes `grid`, so its own `h`/`l`
  pan the view and `g`/`shift+g` jump it to the start and end.

Menu and popup-list steps are the shared `motion::menu_down`/`menu_up`
(`j`/`down`, `k`/`up`), bound once under `tilelist`, which a tile publishes
while its `.` menu, or timeseries' series list or range/frequency menu, is
open. An open menu over a grid takes those keys and the grid stays put; a
count steps that many rows. What a step means stays the list's own: a menu
clamps over its enabled rows, the series list wraps like the chips.

### Autosized columns

The blotter, market-data, and pricer tiles fit their columns to their content
on `:autosize` or the palette's "Autosize columns" (`tile::autosize_columns`,
the focused tile only); `:autosize reset` returns to the configured or
default widths. Each tile runs one method for both doors.

- **Measure.** `geode_shell::colfit` counts the characters of the header and
  each cell and multiplies by JetBrains Mono's advance (0.6 em) at `text_sm`
  (0.875 rem). It adds the `XSmall` table cell's padding, read from
  gpui-component, and the 2 px cursor border. The rem is the window's when the
  command runs. Modules add their own cell chrome: the blotter's `px_1` and
  sort icon, and the tree columns' indent and chevron slot. Widths are clamped
  to 2.5–40 rem: at least about three characters, and never so wide that one
  long cell pushes the other columns off-screen. The fit runs once on the UI
  thread and never in render.
- **What is measured.** The blotter measures only the header and the rows in
  its window cache, which holds the window the table last asked to see.
  Formatting a whole snapshot would break the UI budget, so a wider value in a
  row that was never on screen does not widen its column. Market data measures
  and the pricer measure the rows in their windows, as the blotter does: the
  rows the table last asked to see. A wider value in a row that was never on
  screen, or in a collapsed package's legs, does not widen its column.
- **One-row tiles.** All three grids paint only the rows their window holds,
  and the table never reports a visible range of one row. A tile squeezed to
  exactly one row's height and scrolled away from its last window can paint
  that row blank until a scroll or a data change refills it.
- **Nothing to fit.** When there are no rows to measure, `:autosize` refuses
  with "nothing loaded to fit" and keeps the widths it already has. This
  covers a blotter with no snapshot or an empty result, a panel with no
  document or no rows, and a sheet that is still loading or empty. The
  palette action shows the same refusal as a notice. `:autosize reset` always
  runs.
- **Storage.** The delegate holds fitted widths in pixels, keyed by a stable
  column key: the blotter's column name (empty for the tree column), the
  market-data column label (`__row_axis` for row labels), or the pricer's
  vocabulary name (`__tree` for the tree). `column()` prefers a fitted width
  to the default, so every refresh keeps it. A key the current model lacks is
  ignored, and a new column gets its default width. The widths persist in the
  session record's `column_widths` table. A missing or malformed table
  restores as no fitted widths. A restored width is clamped to 25–560 px,
  the range a fit can produce at any font scale (2.5 rem at the 10 px rem to
  40 rem at the 14 px rem).
- **Blotter specifics.** Switching the blotter's view clears its fitted widths.
  A restored record whose view no longer exists opens the fallback view
  without them. A view over a computed dataset, such as the pricer's, is
  neither offered by `:view` completion nor opened by `:view`, which refuses
  with `view '<name>' is over computed dataset '<dataset>', which a module
  answers for; the blotter cannot show it`; a record naming one opens the
  fallback view with that refusal as its notice, which outlives the fallback
  view's first snapshot. The fallback (the explicit default, else the first
  configured view) skips computed views too, so a fresh tile never opens on
  one that sorts first. A grouping change drops the tree column's fitted
  width, because its labels and depths belong to the grouping, and keeps the
  other columns' widths.
- **Fitted beats configured.** A fitted width overrides the configured one,
  including a `presentation.width` or pricer view width changed later, until
  `:autosize reset` or a refit.
- **Limits.** Widths are pixels because `TableDelegate::column` has no window
  from which to rescale rem. After a font-size change, fitted widths behave
  like configured ones: run `:autosize` again. The blotter's header is painted
  in the UI font but measured with the mono advance, which usually
  overestimates it slightly. Market data keeps `col_resizable(false)`; the
  pricer's columns resize and reorder by pointer, on the open tile only, as
  its known gaps under "Pricing and the line pricer" describe. A blotter
  column dragged wider still returns to its fitted or configured width on
  the next refresh.

### Link groups

A tile may follow one of the shell's four link groups and emit into one (see
[link groups](shell.md#link-groups)). The shell owns membership and the
chooser (`mod+u`); a module stores no group. Every module tile paints the
shared header's link chips from its frame handle. What a module contributes
is two capabilities, `TileContent::follows()` and `emits()`, which decide
the rows the chooser offers its tiles, and its emission, which the shell
pulls:

| Module | Follows | Scope it posts | Board it posts | Posts nothing when |
|---|---|---|---|---|
| Blotter | Yes | The cursor row's one `underlying_ref`, as a one-value scope | None | No snapshot has arrived, or the cursor row names no single underlying |
| Pricer | Yes | The cursor row's one underlying, as a one-value `underlying_ref` scope | None | The sheet has no cursor row, or the row is a package across underlyings or a grouping row |
| Market data | No | The panel's underlying, as a one-value `underlying_ref` scope | Its draft document, while the draft is not clean | The panel has no underlying |
| Timeseries | No | Does not emit | | |
| Vol slice | Yes | Does not emit | | |

An emission with no scope leaves the group's scope as it was, so resting the
cursor on a total or a mixed row does not clear what the group's followers
show. A module can emit before it has data: the capability does not depend
on loaded state, so a restored membership survives a tile whose first
answer has not arrived.

Following replaces the frame scope a tile reads with the group's. The
blotter and the pricer query under that scope, so they can follow. The
market-data panel and the timeseries tile never read the frame's scope: a
group would change nothing they show, so they cannot follow, the chooser
offers them no follow row, and a market-data panel does not take its
underlying from a group. A timeseries tile neither follows nor emits and
has no chooser at all. A blotter or pricer set `:unscoped` ignores the
scope of a group it follows, as it ignores the workspace's, while its
header still shows the chip.

The vol slice viewer follows without querying under the scope: it reads
the group's one `underlying_ref` value (`underlying_of`) as its underlying,
and reads the group's board for that underlying's `cvi_params` draft. The
board entry's `DraftMark` is what its draft chip shows (`cvi draft`,
`cvi draft · behind`, `cvi draft · sent`), since the rows cannot tell a live
edit from a held or sent one. See [Vol slice](#vol-slice).

## Blotter

`geode-blotter` renders any configured view as a collapsible hierarchy. The
data service returns grouped rows and attribution metadata in a `Snapshot`;
the pure blotter core resolves columns, builds visible rows, retains expansion by path, formats
the visible window, and handles cursor, find, and yank behavior.

Attribution metadata decides whether a measure is meaningful at each depth. A
non-attributable cell is shown as NULL rather than a plausible but incorrect
sum. The selected view, pinned grouping, unscoped mode, local as-of, and
filtering survive through the module's session record. Named colors come from
shared configuration; cursor, selection, expansion, and sort state are not
persisted by the tile.

The tile's grouping is, in order of precedence: a `:group <columns>` pin; a
`:group slot <n>` pin (the view's own grouping while that slot is empty);
the frame's active slot; the view's own `grouping`. The blotter always
groups: an empty grouping would be one grand-total row, so `:group none`
(the pricer's flat-sheet pin), and `none` anywhere in a column list, is
refused with `the blotter always groups: :group takes columns or \`slot
N\``. A bare `:group` refuses with `group needs columns or \`slot N\``.
`:group` completes the groupable dimensions and `slot`. While a pin holds,
a frame grouping change or slot switch does not regroup the tile; `:unpin`
follows the frame again at once. The pin is saved as `pinned` or
`pinned_slot`.

The `DataTable` delegate paints a prepared row model. Rendering does not
recompile columns or format the whole dataset. Each delivered snapshot builds
a candidate column plan so presentation changes are recognized even when the
column names and indices are unchanged. The shared window cache holds the
visible window; named colors reuse resolved base and sign variants until their
definitions or theme inputs change.

Publication watches are scoped to the datasets the view reads. Global frame
changes use the flip barrier: promotion requires the staged snapshot's followed
counters to match, including watched data and configuration. Local pins exempt
the corresponding frame changes; tile-local requeries clear any staged result.
Hiding a blotter keeps its view query in flight and applies the reply while
hidden, unless a followed counter moved since the query was asked: that reply
is dropped and the tile asks again when shown. Closing it cancels the query
and answers the barrier. A view the configuration no longer defines is shown
as `view '<name>' is not configured` and answers the barrier at once rather
than holding the other tiles to the deadline; a late outcome for the old view
is dropped, and the reload that defines the view again is the retry.
Restored filters are parsed for syntax, with malformed expressions dropped and
logged. Interactive `:filter` commands also validate column names against the
current schema and derived dimensions.

An ungrouped dimension column (a `dimension` column the grouping does not
contain, such as `strike` beside a position tree) shows its value where every
row beneath agrees, a muted `mixed` where they disagree, and blank where none
has a value. Sorting on it puts values first, then `mixed`, then blanks, in both
directions; `y` yanks `mixed` as the word; the selection footer never totals a
dimension column. See [ungrouped dimension
columns](data-path.md#ungrouped-dimension-columns). A numeric dimension such as
`strike` sorts by number and paints its exact value, never rounded by the text
format.

`g m` opens a panel on the cursor row's `underlying_ref`, the column every
panel kind accepts. The blotter reads it from the grouping path, a shown
column, or the hidden context column the data service adds; a row above the
column's grouping level, a mixed value, or a NULL value leaves it absent, and
a row holding no registered context column (an `lhu` subtotal, say) opens the
plain tile picker.

Emitting into a [link group](#link-groups), the blotter posts that same
value as the group's scope: the cursor row's `underlying_ref`, where the row
has exactly one. A row above the column's grouping level, a mixed or NULL
value, and a tile with no snapshot yet post no scope, which leaves the
group's scope as it was. Only the cursor row is read, never the selection: a
selection does not change which underlying the cursor is on. Every cursor
move, tree change and delivery tells the shell to pull, and so does the
promotion of a snapshot held behind a flip barrier, which no delivery
paints; a pull that finds the same underlying writes nothing. The blotter
posts no documents.
Following a group, it queries under the group's scope in place of the
workspace's, composed with its own `:filter` layer.

`g .` opens the shell's [row menu](shell.md#row-menu) on the cursor row,
hung just under it: a section per value that some panel or action takes
(`underlying_ref · SPX`) listing the panels and actions that open on it. When the cursor row has
scrolled out of view the menu hangs at the tile's top-left instead. A
right-click on a row's cell, or on the row beside its cells, opens the same
menu at the pointer, the clicked column's section first when that column is
a dimension the row carries (a click beside the cells counts as the
cursor's column). Blank space below the data opens nothing. A right-click
inside a `V` row selection keeps the cursor and the selection; anywhere
else it clears any selection and moves the cursor to the clicked row first.
A row with nothing to offer (an `lhu` subtotal, say) shows `no actions for
this row` instead.

A row carrying a single `position_ref` or `instrument_ref` (from the grouping
path, a shown column, or the hidden context column) gets an "Open in Nemo" row in that column's section.
Picking it opens `nemo://position/<id>` or `nemo://instrument/<id>` (the id
percent-encoded) through the OS and shows `opened <url>` in the status bar;
it opens the row the menu was opened on, never the rest of a selection. A
subtotal whose rows hold several positions has no single `position_ref`, so
it offers no Nemo row.

The header is the shared 22 px strip. The view, grouping, state chips
(`pinned`, `unscoped`, `filtered`, a tile as-of), the frame's `AS OF` warning
and the in-flight `…` sit on the left and clip when the tile is too narrow.
The grouping reads as its levels joined by ` / `, or a muted `ungrouped` for
no levels (a view without `grouping`); the tile's title reads `view ·
ungrouped` likewise, never a dangling `view · `.
Dataset times sit in the header's right cluster with the health chip,
which covers the datasets of the tile's current snapshot; the notice sits
before them. The chip's question moves with each delivered snapshot: a view
over other datasets drops a chip for the old ones at once. The blotter has no
`⋯` menu.

"Edit column in view…" and "Edit column in schema…" list the blotter's
planned non-tree columns with the cursor's column highlighted
(`TileContent::tile_columns`). Hidden columns and dimensions folded into the
tree are not in the plan and are not offered; reach them through the dialogs.

Motions follow the [shared rules](#motion); the column motions (`h`/`l`,
the arrows, `^`/`$`, `home`/`end`) move the blotter's column cursor.

### Selection

`V` (`blotter::visual_rows`) selects whole rows from the cursor; `v`
(`blotter::visual_block`) selects a rectangular block of cells. Pressing the
other key while a selection is live switches its kind at the same anchor;
pressing the same key again clears it. Every motion extends the selection to
the new cursor position instead of moving alone, and a bare `j`/`k` that
would otherwise wrap clamps at the grid's ends instead — wrapping past the
anchor would silently invert the selection. `y` yanks the selection as TSV: a
row selection copies every column with its header row, and a cell block
copies only its own columns, still with their header.

The anchor is a tree path plus a column name. Sorting, column moves, and
live redelivery preserve that identity and recompute the range to the current
cursor in display order; the intermediate rows or columns can change.
The anchor is the end the selection started from, which may be the range's
last row. If the anchor's row is no longer shown — collapsed, filtered out,
narrowed away — the selection clears and the tile reports "selection cleared:
anchor row no longer shown"; if a block's anchor column is hidden or removed
from the view, it reports "selection cleared: anchor column no longer shown".
No neighbouring row or column is guessed. A row selection never depends on a
column, so hiding one leaves it in place.

While a selection is live, the footer leads with its extent (`12 rows × 3
cols`) and then shows each selected measure column's label and total, parted
by hairlines. The label takes the column's own color, as its header does; the
total is painted the way that column's cells paint the same number (bullish or
bearish for a `sign` column, the sign variant of a sign-tinted named color), in
the grid's monospace face. The footer shows the total only — no count, mean,
or extremes. The total is computed over the selection's top-most rows only: a
group row already carries its children's total, so counting a child as well
would double it. NULL and NaN values do not contribute. A contributing
`DeterminedNonAdditive` value produces a muted `—†`; a summable column with
no contributing values shows `—`.

Whether a column adds up at all is a separate fact, decided by the query
compiler and carried on the snapshot's column metadata (`ColumnMeta::summable`):
only a plain measure whose schema aggregate is `sum` is summable. A `min`,
`max`, or `any` measure, a derived expression (a ratio of sums is not a sum),
a joined column, and anything unmarked are not. Such a column shows a muted
`—‡`, and the footer adds "‡ this column does not add up". This refusal takes
precedence over `†`. Attribution alone
cannot decide this: a `max` measure's values belong to their rows, yet the
total of two maxima is meaningless.

The mouse reaches the same states the keyboard does. A plain click anywhere
on a row, including the empty space beside its cells, clears any selection
and moves the cursor; shift+click extends one, starting a block
from the cursor or, from the line-number gutter, rows. A drag selects
continuously, and whether it selects rows or a block is decided by where the
press that started it landed — the gutter starts rows, a cell starts a
block — so a drag that did not begin with a press on a cell or the gutter
selects nothing. The first `escape` clears the selection alone; a second
escape follows the tile's normal narrowing and find-clearing behavior.

Limitations: a selection is always one contiguous row range or rectangle —
there is no multi-range selection — and there is no paste; `y` is yank-only.

### Move LHU

A row carrying a single `position_ref` also gets "Move LHU…"
(`positions::move_lhu`, from `geode-positions`) in that column's
[row menu](shell.md#row-menu) section, after "Open in Nemo". It moves
positions to another LHU in the position system named by
[`positions.toml`](configuration.md#position-service-configuration); without
one the row is disabled with `no position service configured`.

When the row the menu opened on is inside a `V` row selection, the move takes
every selected top-most row, in display order; otherwise it takes that row
alone. A `v` block selection never rides along. Every acting row must name
exactly one `position_ref`: a selection holding a subtotal over several
positions disables the row with `{n} selected rows hold several positions`,
`n` counting the rows that name none.

Picking it opens a choice dialog, "Move to LHU", over the live `lhu` values,
unscoped, reading `loading…` until they arrive. When every moving position
already shares one LHU (the row's own `lhu` without a selection, or the
`lhu` every selected row names), that LHU is left out. No values left closes
the dialog with `no LHU values to move to`. A pick asks `Move {n}
position|positions to LHU {x}?`: `y` or `enter` sends one command and `n`
or `escape` sends nothing.

The move is request-then-wait: nothing on screen changes when it is sent.
The status bar reads `moving {n} position|positions to LHU {x} · sent`, then
the position system's answer replaces it: `… · accepted`, or `move to LHU
{x} refused: {reason}`. A refusal before the command reaches the position
service reads the same way, with the data service's or the position
worker's reason (`position service busy`, say). The grid regroups only
when a later snapshot carries the move; in the demo that is the next poll
of the rewritten risk files, and a cross-book move keeps the old `Book`
(see [the demo position system](data-path.md#the-demo-position-system)).

## Market-data documents

`geode-documents` owns typed wire-format parsers and writers. A parser produces
`DocumentRows`, the shared struct-of-arrays representation, without opening a
file or socket. `geode-data` knows only the `DocumentKind` trait; `geode-app`
registers the concrete CVI and dividend document kinds, the kind actions, and
one panel factory per accepted panel.

`geode-marketdata` renders a document as either a matrix or a flat typed table.
Panels are configuration (`panels.toml`, see
[configuration](configuration.md#market-data-panels)): a `PanelSpec` names
the dataset, document kind, axes, value columns, header attributes, slices,
formats, and offered kind actions. CVI and dividend ship as builtin panels; a
desk or user panel over a declared document dataset and a registered document
kind becomes a new tile kind after a restart. Kind actions are registered
code: CVI's two, reanchor and recalc forward, are not built, so their menu
rows are disabled and the tile answers "not built yet". A refused panel is
not a tile kind; a saved tile of that kind restores as a placeholder and its
session record is kept for a later restart. `MatrixIndex` (labels, row states
and row sources, no cell text) is rebuilt on a delivery, structural edit or
selection bulk step. The delegate's window holds the rows the table last
reported, refilled over that range on every install. A one-cell commit refills
that one window cell; editing a
Sent draft rebuilds to clear sent styling throughout the grid. Yank, a
selection's TSV and find format through `MatrixIndex::format_cell` on demand,
so they include rows off screen. A fuzzy `/` result table formats only the
rows it shows, through `MatrixIndex::md_cell`, as the table reports them.

Edits live in a `Draft` whose `DocumentBase` contains source time and an
optional store generation. Different source times indicate different data;
equal times also differ when both generations are known and unequal. If
either generation is unknown, comparison falls back to source time and
cannot detect a same-time republish. When a document exists at the selected
as-of, production queries report its generation for live and historical reads.

The default Hold policy retains the base snapshot when available after a
differing delivery. `:auto` selects how unsent edits handle the transition:

| Policy | Effect on unsent edits |
|---|---|
| Hold | Enter Behind and retain the base when available until explicit rebase or revert |
| Rebase | Move edits by row and column labels, reporting labels that cannot be resolved |
| Replace | Discard edits and report how much unsent work was replaced |

Changing policy does not retroactively apply it to a held delivery. Redelivery
of the same generation does not trigger it, and the first usable delivery
after session restoration uses Hold. If the saved base is unavailable, a
restored Behind draft paints the delivered grid while withholding unresolved
cell edits. Automatic rebase also holds when the
incoming document has no rows. Sent drafts follow the separate echo rules
below. A snapshot that cannot build a valid grid leaves the last usable model
and draft unchanged and reports the error.

An older historical generation can put a draft Behind just as a newer live
generation can. Returning to the base restores Editing. A same-time republish
that changes the draft's state produces a notice because its timestamp alone
cannot show the change: Hold offers `:rebase` and `:revert`; automatic rebase
reports that the edits moved. Replacement and dropped-edit notices take
precedence. Redelivery of the same generation and returning to the base do
not produce a republish notice.

The document request follows the frame's as-of and publications of the
panel's own document, through the flip barrier. Hiding a panel keeps its
request in flight: the reply applies while hidden, answers any barrier the
panel was enrolled in, and showing it again asks only if the as-of or the
document moved meanwhile. A reply asked under an as-of or document the panel
has since moved past is dropped rather than applied, so no update policy runs
against it. Closing the panel cancels the request and answers any open
barrier still waiting on it.

Base-snapshot retention and same-day group-guard capture require exact
equality of the source-time/generation pair. They do not use the weaker
unknown-generation fallback: a snapshot with a known generation cannot be
assumed to be a saved base whose generation is unknown.

The module supports numeric, date, text, and closed-choice cells, row
insertion/deletion, and kind-specific actions. Integer cells parse directly
to `i64` and retain that type through drafts and upload assembly, preserving
values above 2^53. Bumps on integer cells use checked integer addition and
refuse fractional deltas or integer overflow; all candidate results are
validated before any edit is written. Bump deltas themselves are parsed as
`f64`, so their precision is limited by that representation. See the
[crate guide](../../crates/geode-marketdata/README.md) for grid, popup, and
command-parser contracts. The `.` action list follows the
[shared menu rules](#shared-tile-interaction): its key hints are the live
keymap's and follow a keymap reload while it is open, the update-policy and
kind-action rows included; `Upload`, `Rebase`, `Revert edits` and the three
policy rows (`:auto hold`, `:auto rebase`, `:auto replace`) fall back to their
`:` verbs when unbound, and the other rows to an empty lane. A disabled row's
reason becomes the notice.

The header is the [shared frame](#shared-tile-interaction): kind badge,
underlying and attributes on the left; then the state, incomplete rows, echo,
upload error and the upload prompt, the notice, the source time (`HH:MM:SS
stale` in the warning text tone once stale), the health chip and `⋯`. The
header's health chip covers the panel's dataset.

A panel opened through an add (palette, tile picker, `open_with`, duplicate)
with no underlying opens the underlying picker at once; a restored panel does
not. Every panel kind accepts `underlying_ref` from the cursor's dimension
context.

The panel moves on the [shared motions](#motion). The header attribute
strip sits outside the wrap cycle: `k` (or `up`) on row 0, bare or counted,
enters the strip when the kind has attributes. From the strip, a downward
motion returns to row 0 at the column the cursor left from; `g g`/`G` return
to the grid at that column, uncounted to the first or last row and counted to
row N. A live selection's motions clamp and never enter the strip.

`i` and Enter open a cell or header attribute editor with the caret at the
end of its text. `I` (`shift+i`) opens it at the start without selecting the
text. Both routes also work over a selection and use the same edit guards;
date fields and choice pickers open as usual.

`[ui] line_numbers` adds a gutter beside the grid's pinned column: the row
label when shown, otherwise the first value column. The column widens for the
gutter while cursor borders, draft fills, and deletion marks stay on the data
cell. Numbering includes inserted and deleted rows in painted order. Relative
mode uses absolute numbers while the cursor is in the header attribute strip.
Numeric and date editors retain the displayed value's alignment and text origin
inside the cell. Opening a header attribute editor preserves the value box's
top and height. Date fields retain its width; text fields may grow. The
attribute strip supplies the frame, so neither editor adds input chrome.

Dividend row labels use the ex date and a same-date ordinal (`<date>#n`).
Rebase drops cell edits and deletions in a same-date group whose row count
changed, because the old ordinal may identify a different dividend. The draft
captures group sizes from its painted base before rebase or session save.
Restored drafts without these saved sizes cannot apply this guard. A reorder
within an unchanged-size group remains undetectable and can move an edit to
the wrong dividend.

Session drafts store cell edits with row and column labels, allowing restore
to resolve them against a delivered grid. `base` stores source time and
`base_generation` stores the generation when known. Parked drafts use the
same encoding, preserving identity across underlying switches. A missing
`base_generation` restores as unknown and uses the source-time comparison
fallback. Attribute serialization has a separate type ambiguity: a text
attribute that looks like an ISO date restores as a Date.

### Selection

The panel's grid selection follows the [blotter's](#selection): `V`
(`marketdata::visual_rows`) selects whole rows and `v`
(`marketdata::visual_block`) a rectangle of cells; the other key switches
kind at the same anchor and the same key again clears. While a selection is
live the tile reports `mode == visual` (see [context
predicates](keymaps.md#context-predicates)); motions extend it and clamp at the
grid's edges, never wrapping and never entering the header attribute strip.
The strip and the row-label column are never members. The first `escape`
clears the selection alone.

The anchor is the row's label and the column's name, so a redelivery, an
inserted row, or a rebase keeps the same cells selected. An anchor no longer
painted clears the selection with "selection cleared: anchor row no longer
shown" (or "anchor column", for a block); no neighbour is guessed. Switching
underlying clears the selection, because the next document's terms can carry
the same labels and a selection must never carry over to another document. A
click on a header attribute leaves the grid and clears it without a notice.

The selection tint, the theme's selection color, overlays each cell's own
edited, sent, or deleted fill (it never replaces it) and sits under the text;
the cursor keeps its border inside the tint. A row selection tints its labels
and every column. The footer shows the extent alone (`2 rows × 3 cols`), with
no totals: a vol or forward ladder does not add up.

In visual mode the verbs are single keys; the doubled normal-mode forms
(`y y`, `y c`, `d d`) are not bound there.

- `y` copies the selection as TSV and ends it. A row selection copies a
  header line (the row-axis name where labels are shown, then every column)
  and each row as `y y` would; a block copies its own columns' header and
  cells, with no label.
- `d` deletes every row of a row selection and ends the selection. When
  every selected row is already deleted it refuses and keeps the selection.
  Over a block it refuses with `d deletes rows — use V` and keeps
  the selection, rather than deleting whole rows the block only partly covers.
- `:bump <delta>` with no axis moves every selected number by `delta` and
  keeps the selection; `:bump <delta> row|col` keeps its cursor-relative
  meaning and ignores the selection. It notices `bumped N cells`. Like a live
  step it is all-or-nothing: a fractional delta over a selection that
  includes an integer column writes nothing.
- `i`, `I` or `enter` opens the editor on the cursor cell, and refuses exactly when
  that cell refuses (a deleted row, a document with nothing to edit) or is
  not a member of the selection (below); it does not look for another member.

On a pivot panel, a row selection's edits skip each term's leading slice
values (forward, atm, skew), which is `:bump row`'s rule: a term's ladder
moves without its forward. A flat panel has no slice values, and a block is
exactly its rectangle. The copy and the tint still cover the slice values.
Because they are not members, `i` on a slice value inside a row selection
opens no editor and says `slice values are not in a row selection — use v`:
the typed value or the steps would otherwise land in the ladder while the
cell the editor showed stayed as it was. A block over the slice columns
edits them.

While the selection editor is open, its members are the edit's operand. The
verbs that would move or end the selection under it — a motion, `V`/`v`, the
`escape` action and `y`, each reachable from the palette in insert mode —
refuse with `finish the edit first — enter or escape`. A delivery that no
longer paints the anchor still clears the selection; the editor then acts on
its own cell, and its arrows nudge the text.

**One typed value.** With a selection live, committing the editor — typed
text, a date field, or a choice picked from the popup — writes the value to
every selected cell that accepts it. A number cell parses it by its column's
declared type, a date cell as a date, a text cell as trimmed text (blank is
refused where the column is required), and a choice cell only when it names an
option exactly: a bulk write has no popup to rank a near miss. Deleted rows
and cells that refuse are skipped and counted, `set 5 cells, skipped 3 (2
deleted, 1 wrong type)`. Every member is judged before any write, so when
nothing accepts, the commit is refused with the editor still open and the
draft untouched. An untouched `enter` writes nothing and closes the editor,
so a no-op gesture never copies one cell's value across the selection:
text still equal to what the editor opened on, a date field no digit was
typed into whose date is unchanged, or `enter` on the option the cell
already holds. Stepping a date or moving the choice highlight is a change;
a click on a choice row is always a pick. The selection stays after a
commit.

**Live steps.** On a number cursor cell with its text untouched, the editor's
arrows (`up`/`down`, `shift+` for ten) step every selected number in the
draft at once, so the grid shows the block as it moves; the header reads
`stepped N cells +S` with the running total. Each cell moves from its exact
current value by one unit of its column's displayed places, or by 1 on an
integer column, and is never rounded to the painted grid: snapping would
silently rewrite each cell's unpainted decimals. Empty, deleted, and
non-number cells are skipped and counted. Each press is all-or-nothing: if
any cell refuses (an overflow), nothing is written. A step refuses while the
draft is Behind or its upload echo differs, as every edit does.

- `enter` on the untouched text keeps the steps and the selection.
- `escape` restores the draft exactly as `i` found it, provided the steps are
  still its last change and the painted document has not moved; a delivery
  held Behind meanwhile stays reported, and a draft that was Sent comes back
  Sent with its echo check. A click elsewhere, a row verb, or a
  switch of underlying closes the editor the same way. If an automatic rebase
  moved the painted document, the steps are kept and the header says `steps
  kept: the document moved`, because the pre-edit draft is keyed to a grid
  no longer shown. If anything else changed the draft meanwhile (a revert, an
  automatic replace, `:set`), the editor closes silently and leaves the draft
  as it is.
- Typing makes the edit absolute. The typed value replaces the steps; a cell
  that refuses it returns to its pre-`i` value rather than keeping a
  half-step. The exception is a painted document that moved while the editor
  was open: then the steps stay, the typed value is written over them, and
  the notice reads `set N cells; steps kept: the document moved`. From then
  on the arrows nudge the editor's text alone.

**Mouse.** A shift+click makes a block from the cursor as it was before the
press to the clicked cell, or a row selection when it lands on a row label
or the line-number gutter. A drag selects
continuously from the cell it started on; where the press landed decides the
kind, and a drag that did not start on a cell, label, or gutter selects
nothing. With a selection live, shift+click extends it. A plain click anywhere
on a row, including beside its cells, clears the selection and moves the
cursor, so a double-click with a selection live opens a single-cell editor. A
press inside the open editor's own cell belongs to the editor (caret
placement, text selection, a date segment or separator): it neither cancels
the edit nor starts a selection; `escape` cancels.

Limitations: a selection is one contiguous row range or rectangle; there is
no paste; `space` is not bound in visual mode, and the choice step reached
from the palette acts on the cursor cell alone; the footer shows no totals.

### Uploads

`:upload [target]` and the action list's `Upload` row send the edited document
to a configured egress target. Upload requires a complete Editing draft, an
eligible target, and no other upload in flight from the tile. Both the frame
and the painted generation must be live. A live frame can still show a
historical generation while a requery is pending or after it fails; uploading
that document would overwrite untouched rows with old values.

Arming confirmation assembles the rows and snapshots the draft. The header
shows the target and counts of changed cells, attributes, added rows, and
removed rows. Bare unmodified `y`, or the prompt's Yes button, submits that
snapshot after rechecking the live frame, live painted generation, and full
draft equality. Every other key cancels and is consumed, including chords; so
does No. A pointer press anywhere else on the tile or loss of focus also
cancels. A delivery that changes the draft or painted generation,
or a switch of underlying, withdraws the prompt unanswered. The prompt is the
shared `geode_tile::confirm` door.

Transport success marks the draft `sent HH:MM` only if the current draft is
still Editing and equals the submitted draft, including its base. Failure
keeps the draft editable and shows `upload failed: <e>`. Service admission,
transport success, and a stored echo are separate stages; see
[document uploads](request-delivery.md#document-uploads) for delivery limits.

Upload state belongs to the submitting underlying. Switching underlying gives
up that draft's echo check and parks it as Editing, even if its upload is
still in flight. A later outcome for an underlying no longer shown produces a
notice naming it and leaves the visible draft alone. Session restoration also
restores nonempty drafts as Editing; upload status and echo tracking are not
saved.

A Sent draft checks a delivery against the submitted document once its base
differs by the source-time/generation rule above. Reusing a cached differing
echo requires exact pair equality, including whether the generation is
known. Content comparison ignores row order and minted row labels, compares
attributes, and allows one ULP for floating-point values. Matching contents
clear the draft and show `sent HH:MM, confirmed HH:MM` until the next edit.
This is a content match, without an upstream correlation id.

Different contents retain the draft over its base and show `echo differs
(N rows)`; an attribute mismatch counts as one additional row. An uncomparable
document shows `echo not comparable: <why>` and is held too. Edits remain
refused until `:rebase` makes an unsent draft on the held generation or
`:revert` discards the draft. The ordinary update policy does not apply while
Sent. Rebase is refused while awaiting an echo with no differing generation
held.

Uploads replace the whole document with no concurrency check: the last writer
wins. A generation that arrives upstream after the panel's last delivery can
be overwritten. An echo delivered before transport success follows the
ordinary Editing update policy. If that changes the draft, the later success
cannot mark it Sent.

### Link group emission

A panel emitting into a [link group](#link-groups) posts where it is and,
while its draft is not clean, the draft document it paints.

The scope is the panel's underlying (the first part of its document key) as
a one-value `underlying_ref` scope. A panel with no underlying posts
nothing, which leaves the group's scope as it was.

The board entry is the panel's draft document: its dataset, its document
key, and the rows the upload builder assembles from the painted base, the
installed index and the draft, whole, as `:upload` would send them. It is
posted while the draft is not clean, so an `Editing`, a `Behind` and a
`Sent` draft are all on the board: the board shows exactly what the panel
paints. The entry carries that state as its `DraftMark`, since a follower
cannot tell the three apart from the rows. A clean panel posts no document,
since a reader has the delivered one. `:revert` takes the document off the
board. A draft the builder
refuses (an inserted dividend row with no amount, say) posts no document;
the scope is still posted.

The assembled rows are cached on the document key, the painted base
snapshot (by allocation) and the draft. A pull with none of the three moved hands
back the same allocation, which the frame reads as no change: no document
walk and no board write. A republish at the same source time is a different
snapshot under an unchanged draft and is reassembled. A refusal is cached
the same way, so a draft that cannot be assembled is not re-walked on every
pull. No edit route touches the cache; an edit costs nothing here until the
shell pulls.

Every route that moves the underlying, the draft or the painted document
notifies the tile, which is what tells the shell to pull. That includes a
one-cell commit, which refills its cell through the table entity, and a
refused `:upload` that cancels an open selection editor and takes its live
steps back out of the draft.

A panel emits only. It does not read the frame's scope and does not take
its underlying from a group, so it cannot follow one: its chooser lists the
emit rows alone. No tile reads the board, so a posted draft is not displayed
anywhere else.

## Timeseries

`geode-timeseries` owns a chart tile composed from source series and arithmetic
expressions. Its pure model tracks slots, range, frequency, axis mode,
statistics settings, cursor, and viewport. Mutations return a `Changed` bitset
so the retained tile can distinguish fetch, query, chart, chrome, and session
work. Popup state belongs to the retained tile, separately from the model.

Runtime responsibilities are split by module:

| Module | Responsibility |
|---|---|
| [`tile`](../../crates/geode-timeseries/src/tile/mod.rs) | Entity state, frame observation, actions and local commands, header preparation, chart cache, and rendering |
| [`tile::data`](../../crates/geode-timeseries/src/tile/data.rs) | Fetch and query submission, delivery freshness, and last-good results over `geode_tile::following` (the barrier staging and promotion rules), with the post-step `release_view` after each promotion and delivery |
| [`tile::pointer`](../../crates/geode-timeseries/src/tile/pointer.rs) | Chart hit testing, wheel navigation, pan and split drags |
| [`tile::popups`](../../crates/geode-timeseries/src/tile/popups.rs) | Popup transitions, keyboard handling, commits, cancellation, and focus |
| [`popup`](../../crates/geode-timeseries/src/popup.rs) | Popup state types and rendering over `geode-tile`'s row shell, anchoring, menus and notice |

Settings and tiles obtain configured fetch sources from the shell-published
`SeriesSettings` global. A configured source is not proof that its adapter
started successfully. Fetch requests ask for the selected range; the data tier
subtracts covered spans. Completion is broadcast by `(identity, source)` and
updates affected slots. Visible affected tiles requery on every successful
completion, including zero new rows. Failed requests retain the last good
chart and report a notice or failed slot.

A series is named by its chip label: its identity, with `@source` only when
the source is not the configured default. Slot numbers stay internal (session
keys, request tags, element IDs) and never appear on screen or in anything the
trader types. `:rule`, `:color`, `:yaxis`, and `:remove` act on the selected
series, or on a series named first (`:color VIX 2`, `:remove
SPX.close@demo_rest`); the word count tells the two apart, so `:color spx`
sets the selected series' color. A bare identity also names a series when it is
unambiguous; otherwise the command is refused with the matching labels, and
with no selection and no name it is refused outright. An expression has no
name, so it is reached only by selection. Completion offers the names where
one may go.

Expressions reference source series by the same names, and only source
series: an expression cannot reference another expression, so there is no
expression ordering and no cycle to refuse. A name that fits more than one
series, including a pair loaded twice under two rules, is refused rather than
resolved to either. A long expression's chip label is cut to 24 characters
ending in `…`.

The expression field (`x`, or `e` on an expression) completes loaded series
names, the only names an expression may reference. The name at the caret is
the run the expression tokenizer reads as one reference (an identity with an
optional `@source`), so `SPX.close/VI` completes `VI`; a caret just before a
name's first character is in that name, and a caret in a number offers
nothing. Loaded names that name exactly one series are ranked against
it with the `:` line's matcher; an empty name (an empty field, or after an
operator, a parenthesis or a space) offers every one. The list hangs under the
field over the chart, showing at most eight rows that scroll with the lit row.
When no unambiguous source names are available, it shows the add-series hint;
this also happens when duplicate loaded pairs leave no usable name.
Tab writes the lit name over the
name at the caret and repeated Tab cycles the same list; Shift+Tab cycles
back, and a first Shift+Tab writes the last. The lit row is the name last
written. A row click writes that name the same way and leaves the keyboard in
the field. Enter first writes in a typed name that is not exact but matches
exactly one loaded name, then commits; with several matches it commits the
text as typed and the resolver names the unknown reference, and the list is
re-ranked against the expanded text. Each completion is one edit in the
field's undo history. A caret moved without typing, including after a Tab,
re-ranks on the next Tab, not before; the list itself shows the ranking from
the last edit. A change of the desk's default source relabels an open list.

A tile's session table is written with `version = 2`. A table with a missing
version or one below 2 may name series in expression text by slot handle (`s3`), and restore
rewrites each handle: a source slot's handle becomes its full
`identity@source`, never the bare identity, so a later change of the default
source cannot retarget it; another expression's handle becomes that
expression's text in parentheses, recursively. A text with no handle is read
as current text. An expression that cannot be rewritten (a cycle, a handle to
a slot the session no longer holds, or a series no name can pick out) keeps
its slot and saved text, shows as failed with the reason, is never queried,
and is saved back marked so a later restore tries again; editing it with `e`
replaces it. Slot numbering continues past every number such a text names, so
a retry can never pick up a newer series that happened to take that number.

Series queries return aligned points, percentiles, bins, and coverage.
Expression slots may narrow results to buckets shared by their operands.
Delivery tags reject superseded queries. Results requested under a pending
frame flip are staged until promotion is allowed. This coordinates ready
results, but the barrier timeout can release them while lagging tiles still
show older data. The tile follows frame as-of changes and ignores grouping
and scope. Hiding keeps a series query in flight, and its answer applies
while hidden unless the followed as-of moved since it was asked, in which case
it is dropped. Returning a hidden tile to visibility refetches its source pairs
(hidden tiles hear no fetch completions), and the refetch's completion
requeries; the show itself queries only if the as-of moved while hidden.
Closing the tile cancels its series query and answers any open flip barrier;
fetches run on. A zoom or pan queued behind an in-flight query is dropped if
the tile is hidden before that query lands; the reshow's refetch completion
requeries the current view, and if that fetch fails nothing asks again until
the next change.
The tile's `/` handler does not implement local find.

The series list, add picker, expression editor, custom dates editor, the three
menus (action list, range, frequency), and color picker share one `Popup`
owner. The list has no text field; input popups
own their fields and key routing. Closing uses one cleanup path and blurs a focused input before
releasing it. Series and add-picker rows share geometry, theme treatment,
identity, and pointer handling, while supplying their own labels, controls,
and activation behavior. The dates editor uses separate date-field rows;
the expression editor renders inline below the header in the tile body.

`geode-chart` is independent of series and shell concepts. It is a kit and
one element per chart type. The kit's window-free core owns scales, axes,
layout, the view window, time and linear x ticks, mark geometry, hit-testing,
decimation, and palette derivation; its paint half owns the pane frame, the
axis painters and the stroke builders the elements share.
`timeseries::ChartElement` paints an immutable `ChartModel`: polylines over a
session or continuous time axis, with percentile rules and density bars.
`xy::XyElement` paints an immutable `XyModel`: lines, solid or dashed, and
point marks with a range bar over a linear x axis that can run reversed, with
a crosshair that snaps to a quoted point. Both paint through gpui-component's
plot surface in up to two panes. Paths and chrome are cached by the values
that affect them; cursor movement does not rebuild the data model. The
timeseries tile hosts the time chart and the vol slice viewer the xy
element.

The header's `⋯` button, a chip's right-click, and `.` open the action menu.
It offers popup openers, actions for the selected slot, `Frequency…`, toggles,
and view reset. It opens on its first enabled action, and keyboard stepping
skips disabled rows, separators, and headings. A pointer can rest on a
disabled row, which has no highlight fill; choosing it shows its reason and
leaves the menu open. Enabled actions close the menu before dispatch. Key
hints are the live keymap's and are re-resolved when the menu opens, when its
chrome rebuilds, and when the keymap is republished; an action the keymap
binds nowhere shows an empty lane.

The header is the shared 22 px strip; its health chip is the worst health
over the series' sources. The header shows the range and the frequency as two
triggers, `1y ▾` and `1d ▾`; an absolute range shows its dates,
`2025-09-26 – 2026-09-26 ▾`. Each trigger opens its own menu under it and
stays filled while that menu (or, for the range, the dates editor) is up; a
second click closes it. `r` and the range
trigger open the range menu: the seven presets written out with their short
labels, then `Custom dates…` (`c`). `f` and the frequency trigger open the
frequency menu: the six frequencies with their short labels. Short labels are
text, not keys; `c` paints as a key. Both menus tick the value in force and
open with the highlight on it (on `Custom dates…` while the range is absolute).
The shared menu keys (`j`/`k` or the arrows) move, Enter or a click applies
and closes, and Escape closes; a second `r` or `f` closes its own menu. A
frequency the 500,000-point cap refuses over the current range, as resolved
under the frame's as-of, is a disabled row reading `over cap`; choosing it
shows the full cap message as the notice. The
rows follow range, frequency, and as-of changes while the menu is open. A
preset the cap refuses at the current frequency is refused when chosen, with
the reason as the notice and the menu left open. `:range` and `:freq` remain
the typed routes; no key steps the frequency.

`Custom dates…` opens a two-field date editor under the range trigger. It
opens on From's day segment and a digit types into the date at once. Tab
switches fields; Enter applies both dates, and a backwards range or an
unfinished segment is refused inline with the editor left open. Escape returns
to the range menu with the highlight on `Custom dates…`; a second Escape closes
the menu. The editor holds the keyboard, so the tile reports insert mode while
it is open.

An outside press closes a popup only if it is still the one up, so pressing a
trigger or `⋯` over another popup swaps popups rather than closing both.

Pointer controls and keyboard actions use the same model operations and
change processing:

| Pointer action | Effect |
|---|---|
| Click a series chip / its swatch | Select the slot / toggle its visibility |
| Right-click a series chip | Select the slot and open the action menu |
| Click the range / frequency trigger | Toggle the range menu (or close the dates editor) / toggle the frequency menu |
| Click a range or frequency menu row | Apply it and close, or show a disabled row's reason |
| Wheel over a plot | Dominant vertical motion zooms about the pointer; dominant horizontal motion pans; ties zoom |
| Drag a plot / the band between panes | Pan / adjust the split |
| Click Add or Compose in an empty tile | Open the corresponding editor |

Chart drags end on release, including a release outside the chart, or on a
move with no button held. Movement outside the chart surface is not tracked.
Modified presses and subsequent presses in a multi-click do not start chart
drags, leaving those gestures available to the shell. Right presses focus the
tile before its context menu handles keys.

A slot's color is a palette index (`1`–`5`), a `[colors]` name, or an absolute
`#rrggbb`. Palette and named colors follow the theme. Absolute colors receive
no theme or contrast adjustment. Sessions store them as lowercase six-digit
hex; malformed hex restores the slot's default color. The slot record's key is
`color`; a record saved under the old `colour` key still restores, without a
notice, and the next save rewrites it. Color names beginning
with `#` are reserved. `c` cycles the palette, starting at color 1 from a named
or absolute color. `:color [series] <1..5|name|#rrggbb>` sets the color
directly; an explicit hex remains absolute even if it matches a palette color.

The action menu's `Color…` row opens a picker at the selected slot's chip.
Its featured swatches capture the five palette colors and all named colors
as resolved when the picker opens. Picks are quantized to opaque 8-bit RGB:

- A pick within one step per channel of the slot's currently painted color
  leaves its color setting unchanged.
- Otherwise, a pick within that tolerance of a featured swatch retains its
  palette or name identity. The nearest swatch wins, with the first on a tie.
- Other picks become absolute colors, with alpha discarded.

The tolerance preserves palette and named choices through the component's hex
field conversion. Swatches and entered hex commit and close; sliders apply
live and stay open. Escape or an outside click closes without undoing slider
changes. Picks target the slot that opened the picker even if the cursor moves;
removing that slot closes it. Picker focus puts the tile in insert mode so hex
input does not invoke single-key tile commands.

Each slider change rebuilds the chart model and clears its path cache. Measured
daily and hourly fixtures fit the frame budget; the 500,000-point fixture does
not. See the [measurement log](../perf.md) for conditions and timings.

`geode-widgets` contains the shared segmented `DateTimeField`. Its pure state
and key routing are separate from a painter that receives presentation values,
allowing the market-data panel, pricer expiry editor, timeseries date editor,
and as-of dialog to share behavior without depending on each other. Hosts own
commit, cancellation, focus, and timezone conversion. A valid stored date can
still have incomplete pending digits: hosts call `complete_pending` before
committing and report its segment error. Segment display text is allocated by
`segments()` and cached by the host for painting. See the
[widget integration contract](../../crates/geode-widgets/README.md#host-integration).

## Vol slice

`geode-volslice` paints one underlying's volatility smiles: a curve per
active expiry for each loaded kind, over an x coordinate the trader picks.
It computes no vol, coordinate or density. Every curve, chain x and density
comes out of the data tier's vol door (`DataHandle::vol_slices`) as a
`VolResult`; subtracting two delivered vols for the difference pane is the
only arithmetic the module does.

**Kinds.** Three, fixed, in header order with the digit that toggles each:
`cvi` (`1`), the published CVI document as of the frame; `cvi draft` (`2`),
the followed group's board draft for the underlying, read now whatever the
as-of; and `chain` (`3`), the option chain's mid vols as points with the
bid-ask range as a bar. A one-sided quote (an absent side is NaN) paints as
a half bar. The published curve is solid and the draft dashed. A chip per
loaded kind sits in the header; a hidden kind's chip is muted and its own
jobs leave the batch. Source identity is absent from the chain's rows, so
there is one chain kind however many sources publish chains.

**Strip.** A column beside the chart lists the sorted union of every loaded
kind's expiries, none before today, each with the digits of the kinds that
have it and a dot in its palette color, filled when the expiry is active.
`j`/`k` move the cursor, `enter` solos the cursor's row and `space` toggles
it; the last active expiry cannot be toggled off. On a focused tile a click
solos a row and a ctrl+click toggles it; the click that merely focuses the
tile changes nothing else (`TileContent::set_focused`). An expiry's color is
its strip position's, so it keeps its color as others are toggled; twelve
expiries cycle the theme's five chart colors. The first strip, and a
restored set naming no listed expiry, front the first row.

**Coordinates.** `x` cycles moneyness, log-moneyness, delta and strike, and
`:x` names one. Delta runs reversed, puts on the left; pans and zooms go
through the axis's scale, so a reversed axis moves the way it reads. A
coordinate change resets the view to the new extent, padded to that
coordinate's narrowest span (a chain strike gap for strike).

**Densities.** `shift+d` (the tile's binding beats the workspace's
duplicate) asks each visible curve's density, painted on the right axis in
the curve's color at a fixed lower opacity. The density is per unit of the
shown coordinate, so its area is about one in each; where delta saturates
the point is NaN and paints as a gap.

**Difference.** `d` opens a chooser of `none` and every ordered pair of
loaded kinds (`:diff <kind> - <kind> | none` too). The pair paints in a lower
pane, its split stepped with `[`/`]` or the divider:

- Curve minus curve is at equal strike: the minuend is evaluated dense and
  the subtrahend at the minuend's strikes through `Grid::Job`, so the two
  never interpolate; it is a line at the minuend's x.
- Curve minus chain is at the chain's strikes: the curve is evaluated `At`
  them and the difference sits at the chain's x as points, negated when the
  chain is the minuend.
- A pair keeps its jobs when one of its kinds is hidden.

**Underlying and following.** The tile reads its own underlying (`u` opens
a picker over the diagnostics catalog's `cvi_params` and `option_chain`
underlyings; `:underlying` sets one) unless it follows a link group, when
it reads the group's single `underlying_ref` value and `u` and
`:underlying` are refused naming the group. A group naming none or several
underlyings paints `no underlying in A`. A tile added with no underlying
and no group prompts with the picker at once. Following is compared through
`FrameView::following()` on every frame notification, since following a
group whose scope was never written moves no version; while following, the
group's scope generation counts as a change. A scope change that still names
the underlying whose documents are loaded asks nothing when no read is out,
nothing is held for a flip, and neither the as-of nor a watched publication
moved since the loaded documents were read (both reads succeeded, under the
same as-of and publications): the documents depend on nothing else, so the
tile answers the flip at once, from a show too, rather than holding it
behind two reads of what is on screen. Documents kept on screen beside a
failed or refused read do not qualify, so the scope change is their retry.
A change of the underlying, or of anything else it follows, refetches. A
change of group, joining or leaving, clears the painted curves and moves the
vol tag before asking again, on show if the tile was hidden: the old group's
draft, painted or still in flight, would otherwise sit dashed under no draft
chip, and stay there if the requery failed.

**Data flow.** The CVI document (full key) and the chain (one-part prefix,
every expiry) are read in sequence under one `FollowingQuery` tag, both
keyed by the tile: the query pool keeps one request per key and the flip
barrier one entry per key, so two concurrent reads would supersede each
other. The pair is one barrier arrival. When both have landed the tile
plans one vol batch for the active expiries and swaps the painted model
when its answer lands, reading results by position against the plan. A
board change (a new draft, or the same draft under a new mark) is never
staged behind a flip: it submits a batch at once, and a draft joins only
beside its own underlying's documents. Hiding keeps the reads in flight;
closing cancels by key, the vol batch included.

**Failures.** The footer shows the first notice and a count of the rest,
in the danger tone; `no underlying` and `no underlying in A` alone are an
empty state, painted muted. A failed job is one notice (`no cvi curve at
<date>: <why>`) and the rest of the batch paints; one cause behind every
job is said once, in the outcome's words, and a difference failing only
because its source curve failed adds none. A pair naming a kind that is not
loaded is the notice `diff <pair>: <kind> is not loaded` rather than an
empty lower pane.

The header names the underlying whose documents are on screen, not one
still being asked about, so a document read in flight or failed never puts
a new name over another underlying's strip and curves. The exception is
the vol round trip after new documents install (see the limitations). A
refused read or batch is worded `document request refused: …` or `vol
request refused: …`, and the next change retries. What stays depends on
whose picture is on screen:

- A refused or failed read (a refused chain read fails the fetch, which
  still answers the barrier) for the underlying on screen keeps its last
  good picture. One for another underlying clears the documents, the strip
  and the curves; the header then names the underlying asked for.
- A refused batch keeps the painted curves only when they were built from
  exactly the documents now loaded: the same underlying, CVI document,
  chain, and draft under the same mark. Curves built from anything else (a
  new underlying's install, a draft that left or changed mark, a
  publication that reinstalled the documents) clear; the strip, chips and
  header stay, so the next change still has a batch to ask.

**Session.** The tile saves its coordinate, hidden kinds, densities, split,
and while set its underlying, active expiries, pair and view; the cursor is
not saved. An unreadable value drops its key with a notice.

**Limitations.**

- Expiries before today are dropped by the viewer only; the dataset
  headline can still pin to an expired document.
- The flip barrier covers the two documents. The vol batch follows them, so
  the curves swap one vol round trip after the flip releases; for that
  round trip the header, chips and strip already name the new underlying
  over the old curves.
- An expiry's color is its strip position's, cycling every five rows, not
  its place among the active expiries. A draft that adds a term shifts every
  row after it, so those expiries change color while the draft is loaded.
- Keyboard zoom anchors at the view's centre, the wheel at the pointer.
- There is no `.` action menu; the header chips and the palette carry the
  actions.
- A standing `vol request refused` notice is retried by the next state
  change (a key, a draft edit, a publication), not by a group scope change
  that keeps the underlying.

See the [crate guide](../../crates/geode-volslice/README.md) for the key
table, session keys and module map.

## Diagnostics

`geode-diagnostics` is the first [page](shell.md#pages): a surface in place
of the tile surface presenting shell-owned operational state in five sections, with
sources, stored data, configuration, logs, and performance. The shell's
`Diagnostics` entity, the shared log ring, the loaded configuration, and the
frame's requery statistics supply the state. `page::toggle_diagnostics`,
bound to `mod+d` by default, opens and closes it from the keymap, the
sidebar button, the palette row "Diagnostics: Open page", or the status
bar's diagnostics summary; Escape with nothing above the page closes it, as
does any workspace switch. The page is retained while the window lives, so
filters, cursors, expansion, and the log tail survive a close and reopen;
the session saves only the selected section. The toolbar stays above the
page with every frame control live, so the as-of the Data section's
resolved markers follow is the one the toolbar shows and can change
(subject to the pinned-workspace limit below).

The header carries a Back control, the current section beside the page title,
and state chips derived from the same inputs as
the status summary (worst source health with its count, config errors, data
errors, and the catalog's arrival time or "catalog pending"). Back dispatches
`page::close`. A rail on the left lists the
sections with native selected buttons and counts: the source count, the
dataset count, config error and warning counts, the error count in the
retained log tail, and the frame p95. A click or the bracket keys select a
section. The content pane is the section's toolbar, its table, and a detail
strip: table rows stay compact while the full details wrap and scroll below.
Copy and `y` copy those details without truncation. Empty views explain
whether there is no data or the filters match nothing. A footer shows the
current keyboard workflow using the shell's keycaps.

| Section | Table and toolbar |
|---|---|
| Sources | Source, Health (title-case label with the reason), Since (clock time and age), Shape, Last poll, Next poll, Ready, Loading. Worst reported health first by variant then name; unreported sources last, and a source known only from an ingest load gets a "no report yet" row with its loading text. Toolbar: a filter over name and health. Detail: the spec lines by shape and the health history. |
| Data | One expandable row per dataset with Partitions, Latest gen, Published, Rows, Resolved, Live, and Loaded; a dataset expands to its generations, the one resolved under a historical frame as-of marked. Toolbar: a case-insensitive filter over dataset names and generation fields (partition/book, generation ID, times, row count, live/archive status), a chip reading `Catalog up to date` or `Refreshing catalog`, Refresh catalog, Expand all, Collapse all. A dataset-name match includes all its generations; leaf-only matches retain the dataset heading and hide unmatched siblings. Filtering temporarily reveals collapsed results; clearing it restores stored expansion. Catalog totals are not narrowed by filtering. |
| Config | Three full-width views: Current issues (config and data lanes), History (prior batches newest first), and Effective values (expandable documents and their leaves, with Key, Value, and Layer from `Config::explain`). The active view owns row navigation and Copy. Search filters issue text or document keys and values; unmatched documents disappear and matches inside collapsed documents are revealed. Open config directory remains available. |
| Log | Time with milliseconds, Lvl, Target, and Message over the retained tail. Toolbar: level toggles, a target select over the targets seen in the tail plus `All targets`, a text filter over message and target, Follow, Clear log, and Log levels. Detail: the full record with a Copy button that puts it on the clipboard. |
| Performance | Aligned median, p95, maximum, and sample-count readouts for frame intervals, query→snapshot, and snapshot→paint, with explanations of each stage. A labeled logarithmic frame-interval histogram shows bucket ranges and counts on hover, with a separate overflow count above 100 ms. Frame cadence is not pure UI work and is not classified against the 8 ms UI budget. UI and requery targets remain explanatory guidance. A Memory section shows process memory (macOS physical footprint, Windows private bytes) with its peak and the time the peak was first seen, then DuckDB memory in use against its limit, temporary files spilled to disk, and the largest DuckDB memory tags, the last three labelled as coming from the last catalog snapshot. Storage, dropped events, refused requests (both warning-toned when non-zero), and the Performance overlay switch share the scrolling region. Missing samples show dashes and zero counts; memory rows read "Not available" until the first sample or catalog snapshot, and on a platform the sampler cannot read. |

Keys in the page's own context: `j`/`k` move the cursor, `g g`/`G` jump,
`ctrl+d`/`ctrl+u` move five rows and `ctrl+f`/`ctrl+b` ten, all with count
prefixes; `[`/`]` cycle sections; `z o`/`z c`, Enter, and Space expand or
collapse the cursor row where it expands (Data datasets, Config documents;
a double-click does the same, a single click only selects); `/` focuses the
section's filter input, which puts the page in insert mode, where Space and
every other bare key type into the filter, and Escape there restores the
entry filter and returns to normal mode. Enter keeps the filter and returns
to navigation; clicking a row does the same. On Performance, which paints
no input, `/` does nothing. `g s` / `g d` / `g c` / `g l` / `g p` jump
directly to sections. Tab and Shift+Tab step the section's views, wrapping
(Config: Current issues, History, Effective values), as do `ctrl+tab` and
`ctrl+shift+tab`; the other sections have no views, so there Tab is
consumed and does nothing rather than moving focus into the chrome. `y`
copies details, `r` refreshes the catalog, `z shift+r` / `z shift+m`
expand/collapse all datasets, and `o` (Config) opens the config directory.
In Log, `-` and `=` show one fewer or one more level as a minimum severity
(ERROR always stays; a hand-picked set steps from its most verbose level
shown and becomes contiguous), `t` / `shift+t` step the target filter
through All targets and the tail's targets, `f` toggles Follow, `ctrl+l`
clears the log, and `shift+l` opens the shell's Set log level… chooser,
the keyboard route to what the Levels popover sets, since the popover's
buttons take no keyboard focus. Escape over an open Levels popover closes
the popover and keeps the page. `alt+backspace` or Reset filters clears only
the visible section’s filters and returns focus to navigation; in Log it
also enables every level and restores All targets. Selection remains on the
same record through refreshes and filtering while it remains visible. The
performance overlay switch's keyboard route is the context-free
`perf::toggle_overlay` (`mod+shift+p`). Every toolbar control's tooltip
names its key, and the footer names the current section's main keys.

Each table has a result strip showing visible and total item counts. Data
counts datasets independently of expanded generation rows; Log excludes loss
notices from record counts and shows its retention limit and follow state.
Effective values reports the documents and leaves actually shown. The detail
header identifies the current row’s position. These strings and Performance
readouts are prepared when their inputs change, outside paint.

Controls that change application state go through a request channel or the
shell-actions handle, never a direct call. A Levels pick queues
`request_level` and the overlay switch `request_overlay_toggle`; the shell
drains, applies, persists, and mirrors them, so the switch shows the shell's
value. Refresh catalog queues an explicit catalog request the bridge serves;
Open config directory dispatches `config::open_directory`. The Levels
popover lists the default level, read-only, then the known targets, then
any target a hand-edited `[log]` names outside that list, each with five
level buttons. It offers no way to add a target: `[log]` keeps only the
known targets across a reload, so an added one could not persist. A section
change blurs a focused filter and closes the popover first, because the next
section may paint neither.

The page rebuilds only the selected section, and only when that section's
inputs changed: its `DiagVersions` counter, plus the frame as-of for Data,
the config version for Config, or new ring records for Log. Clock changes
and local section, filter, or expansion changes also rebuild. Badges and
header chips refresh on any counter change or new record whatever section
is shown, and that refresh is where the log tail is drained, so the Log
badge counts errors in the whole tail. A hidden page drains nothing: a wrap
while it is closed is reported by the drain that shows it again.

The cursor moves on the [shared motions](#motion), rows only (the page
publishes `grid` beside its mode, so the motions reach it in normal mode and
stay out while the filter holds focus). Any motion stops following; a bare
`G` resumes it, and a counted `G` jumps to that row without following. On an
empty section a motion changes nothing, so a following empty log keeps
following through `g g`.

Visibility drives watched demand: opening calls `watch` and closing
`unwatch`, so a closed page holds no catalog demand while explicit
consumers keep theirs. An as-of change requests a fresh catalog while the
page is visible; until the catalog's as-of matches the frame, the resolved
markers are hidden and the Data chip reads pending. The page submits no
view query, and no flip barrier waits on it: the tiles beneath are hidden
while it is open.

The tail retains at most 4,096 records, starts at the ring's sequence when
the page is first created, and reuses its drain buffer. If the ring
overwrites unread records, the next drain leads the table with a loss row
naming the gap measured at that drain, not a cumulative total; Clear
forgets the retained records without moving the drain point. Following
keeps the cursor on the last row; moving the cursor stops it, and `G` or
the Follow switch resumes it.

Source ages tick once a second while the page is visible and Sources is
selected, rewriting the Since cells in place without a rebuild; on any
other section, or a hidden page, the timer is dropped. Every other
timestamp comes from the last rebuild.

Limits: every table row has one height, so full details live in the scrollable
strip. Clear log, level toggles, and target selection use native keyboard
focus without dedicated page shortcuts. Columns resize but do not move or sort, since no
section defines a sort order yet; the config explainer shows at most 2,000
leaves per document with an omitted-count row but still traverses every
leaf; stopped data threads show on the status bar, not in Sources. The page
reads the frame of the workspace that was active when it was first opened:
reopened over a different pinned workspace, its Data section follows that
first workspace's as-of and its catalog chip can stay at "catalog pending"
until the page is opened again from that workspace. A rebind on open is the
planned fix.

See the [crate guide](../../crates/geode-diagnostics/README.md) for the
module map and the observer, notification, and allocation contracts.

## Pricing and the line pricer

`geode_core::pricing` defines the request, instrument, override, result, and
`Pricer` vocabulary. `geode-pricing` contains implementations of that trait.
The current `MockPricer` is deterministic test and demo behavior, not a
financial model. The pricing worker applies one override set per batch,
contains panics, and returns values or an error for each completed line.
Cancellation can drop a queued batch or stop a running batch between lines,
so cancelled requests need not return an outcome for every submitted line.

`geode-pricer` is the line-pricer module: a pure core (a struct-of-arrays
sheet, edits and undo, shorthand parsing and rendering, package folding, column
planning, and document storage conversion) and the `pricer` tile registered in
the application roster.

### The tile

The tile (titled `Pricer · <sheet>`) shows one named sheet under a single
dense header: the sheet name (a control: see [sheets by pointer](#sheets-by-pointer)) with `view <name>`, any sheet-wide shift chips
(`spot +2.0%`, `vol -1.0`, spelled as the shift cells spell them), `N pricing…`
while lines are stale, `N failed` in danger text while any line's last answer
was a failure, `pricer <name>`, the last priced time, which reads `stale` once
it is older than the shell's `stale_after` — re-evaluated on each repaint: the
reprice timer's ticks repaint it, and with `refresh = "off"` an idle pricer's
`stale` waits for its next repaint (known limitation) — and a `⋯` button at the trailing
edge that opens and closes the action menu (the pointer's `.`). The header is
the [shared frame](#shared-tile-interaction): notices paint after the status
items, and the health chip sits between the time and `⋯`. The health chip
covers `pricer_sheets` only (see the pricer README's limits). A pending load
paints `loading…` muted in the header and `Loading sheet…` in the empty table;
an empty loaded sheet says `No lines — press o to add one`. A pricer this
binary lacks is named in danger text with its recovery (`set [pricing]
adapter and restart`).

The sheet moves on the [shared motions](#motion). Column 0 of the cursor
is the first plan column; the tree column is never a target. An empty sheet
takes no motion. A motion dispatched from the palette while the entry bar, the
cell editor or the action menu is open closes it first, then moves.

Column headers are words carrying their unit (`spot %`, `vol pt`, `barrier
type`, `priced at`), and default widths are checked against labels (beside the
header's sort icon) and representative large
values (`-1,234,567.8900` for a greek, `-1,234,567.89` for npv) at the
largest supported font size. These examples do not bound every possible
value. A view's `label` and `width` override the defaults. Columns are
managed as any view's: the Views dialog's column stage (order, hidden, label,
width, format, color) applies to a pricer view, and `hidden` columns leave
the plan. `Edit column in view…` from the palette opens the Views dialog on
the pricer's view at the cursor's column, as it does for a blotter. A view
column's `color` applies as in the blotter: `sign` paints a negative measure
in the theme's bearish color and a positive one bullish, a named color from
`colors.toml` tints the column and its header; a stale cell stays muted and a
failed one danger whatever the column's color. Measures default to `sign`;
a column says `color = "none"` to opt out.
Result columns
carry risk_snapshot's names — `npv`, `delta01`, `gamma01`, `vega01`,
`rho010`, `clean_theta_business_day` and the rest — each with a `_usd` twin
the pricer converts itself; a column means the same thing in a blotter and a
pricer sheet. A package whose legs priced in different currencies, or a
selection total over such lines, paints `—` in its local-currency measure
columns; the `_usd` columns still sum, and are the comparable ones across
currencies. Both bundled views end in a `status` column, which says
`pricing…` on a stale line and a failed line's reason, so neither state is
shown by color alone. Column 0 is a connector tree. A package row shows its
chevron, its template (`CS`, `CUSTOM`) as a neutral chip, a summary of its
legs' distinct expiries and strikes (`Z26 4800/5200`) and a muted leg count
(`· 2 legs`). Each leg hangs from a drawn connector under the package's
chevron: a hairline through the full row height, joining the next leg's
without a gap, and a stub at mid-height toward its text. A package's legs
read as one block: a leg that is not its package's last drops the row
separator below it, so there is no separator between sibling legs, while the
last leg, the package row, bare lines and group rows keep theirs; on the
package's last leg the line stops at its stub. The lines take the `border`
token floored to 3:1 non-text contrast on the leg's own, hover and selected
grounds. The leg's full shorthand paints in muted text; a bare line shows
its full shorthand. Every row reserves the slot, so roots share one
leading edge. A leg's connector sits one and a half depth steps right of its
package's chevron, under the chip's leading glyph, and its text starts as far
right of the chip, so the legs read as nested inside the package. Every
leg of a package, the last included, sits on a faint tint that marks it as
inside its package; bare lines and package rows keep the table's ground. The
tint is the theme's stripe token (`table_even`) where it reads at least 1.04:1
from the table ground, from hover, from the selected-row ground and from the
grouping row's ground while staying fainter than hover; otherwise it is the
faintest blend of the table ground, toward the foreground or away from it,
that does. A leg's text, state colors, `sign` colors, named colors and
gutter are floored on the tint as well as on hover and selected. On Aurora
Light, Default Light and Modus Operandi the selected-row and hover grounds
sit too near the table ground for any such tint, so the tint is held apart
from hover and the group ground only, and selecting a leg changes its ground
only slightly; the cursor cell's border still marks the cursor row, as does
the gutter's own paint when line numbers are on. A grouping row (see
[grouping](#grouping)) carries a ground of its own too; the table's hover
and selected-row fills replace either ground. Column 0 is fixed at a width
that fits a two-leg call spread's package row (`▾ CS Z26 4800/5200 · 2 legs`) at the largest
font; a longer summary ends in `…` and the leg count stays whole. Find (`/`,
`n`, `N`) matches the shorthand column 0 paints on a line or leg; a
package's find key is still its template form (`SPX Z26 4800/5200 CS`), and
a custom package's is its template token, underlyings and the summary it
paints (`CUSTOM SPX Z26 5000/4000`). A long text cell ends in `…`; a number
never truncates. Cell text is floored to the readable ratio on its row's own
ground (the table's base, a leg's tint or a group row's) and the hover and
selected-row grounds.

The entry bar sits between the header and the column headers. A muted label
names where `enter` lands (`after <row>`, `into <TEMPLATE>`, `at top`,
`at end`). A parse error or a refused insert keeps the text and shows the
reason under the field in danger text; any edit clears it. "Add lines
below…" or "Add lines above…" from the palette while the bar is open keeps
its text and place and focuses its field again.

As you type, the bar suggests the part of the line under the caret and a
hint line names what goes there: underlyings from `[pricing] underlyings`,
the next eight monthly expiries and common tenors, `C`, `P` and every
template with how many strikes and expiries it takes, and the four barrier
kinds after a single leg. Tab writes the lit suggestion over the token (only
the `/`-separated part for expiries and strikes) and repeated Tab cycles;
Shift+Tab cycles back; a click writes a suggestion and keeps typing in the
field. `enter` adds the line exactly as typed. `up`/`down` still walk
history.

The list hangs from the bar over the table's top rows, at most eight rows at
a time, and scrolls with the lit row. Each write is one edit in the field's
undo history. With no underlyings configured, the underlying slot's list
says `no underlyings configured ([pricing] underlyings)`. The list covers
the rows under it: a press there never reaches the table, so a click that
closes the bar has to land on a row the list does not cover.

`[ui] line_numbers` adds a gutter beside the tree column, before the depth
indent, so numbers share one lane; the tree column widens by the gutter.
Numbers are muted on every row kind, a package's included, and take the
normal text color on the cursor row. Lines, packages, and an open package's
legs are numbered in painted order — the index `NG` jumps to. Relative mode shows distance
from the cursor row, with its absolute number on that row, and numbers
absolutely when there is no cursor row.

Lines and packages are rows of one table; a package row sums its legs and opens and closes like a tree node
(`space`/`z a`, `z o`, `z c`, `z shift+r`, `z shift+m`, its chevron, or a
double-click on its name in the tree column; a double-click on its value
cells edits them, and one on a leg's name changes nothing). A
package created in the session opens so its legs show; a restored tile opens
the packages its session record names. A package row's text columns show
its legs' distinct values in leg order joined with `/` (a call spread reads
`SPX`, `Z26`, `7400/7800`, `C`); barrier columns read only its barrier legs.
Shift columns group legs by the shift as the cell spells it, show a leg with
no shift as `—` beside set ones (`+2.0/—`), and paint muted only when every
leg inherits the sheet's. Its
qty is the package quantity while the legs fit the template, otherwise the
list of distinct leg quantities. These cells edit (`i`, `enter`,
double-click open a plain text editor, even for expiry and type): one
value goes to every leg; a `/` list with one part per shown value replaces
each where it appears (`7500/7900` moves a spread's two strikes; a fly's
body moves once); a package quantity rescales every leg by its weight. A
list with the wrong part count is refused naming the count and the cell
(`2 values: 7400/7800`) and the editor stays open. Every part is checked
first, and the whole edit is one undo step and one reprice. The editor
groups a cell as the view paints it, so a precision override counts the
same parts on screen, in the editor and in a refusal. If a template reload
under an open package editor changes the text the cell would open on (the
legs now fit the template, or no longer do), `enter` refuses with the
editor's `the cell moved` message rather than read the typed quantity
another way. Result columns stay read-only.
A package row's pricing timestamp is the oldest present leg-attempt timestamp,
including failed attempts; it does not establish that every leg priced successfully.

The shorthand's package types come from the `pricer_templates` configuration
document: the seven built-ins (`CS`, `PS`, `STRD`, `STRG`, `RR`, `FLY`,
`CAL`) plus any the desk or user layer defines, such as a `CONDOR` (see
[configuration](configuration.md#pricing)). A layer's entry of a built-in's
name redefines it. `C` and `P` are single lines, not templates. A type the
set does not know is a parse error that lists what is accepted, `C` and `P`
first and then the templates in document order (`unknown type 'X': C P CS PS
…`). A reload reaches every open tile: the entry bar parses against the new
set at once.

A stored package keeps its template's name whatever the configuration later
says. When that template is removed, or redefined so the package's legs no
longer fit its table, the package still loads with its name as its tag, and
its shorthand (for `y y` and find) prints its legs one per line instead of
the template form. Its legs, quantities, and prices are unchanged; only a
package typed after the change uses the new table.

Normal-mode keys:

| Keys | Effect |
|---|---|
| `o` | Open the entry bar under the header; `enter` adds the line below the cursor row (on a leg, the next leg; on a package, its first leg; with no cursor row, at the end; a package typed inside a package lands just after that package) and keeps the bar open for the next; `up`/`down` walk the sheet's own lines as history; `tab`/`shift+tab` complete the token at the caret; `escape` closes it |
| `shift+o` | The same bar, but the first line lands above the cursor row (on a leg, before that leg in its package; on a package or a top-level line, before it; on the first row, `at top`; on a grouping row, `at end`); each further line lands after the one just added, so a typed run reads top to bottom |
| `i`, `enter`, double-click | Edit the cell in place (a double-click on a package's tree cell, or anywhere on a group row, opens or closes it instead) with the caret at the end of text; `up`/`down` (`shift`: ten) step a number by the precision its text carries, or the expiry date field's active segment |
| `I` (`shift+i`) | Edit the cell with the caret at the start of text, without selecting it; date fields and choice pickers open as usual |
| `d d` | Delete the row (a package with its legs) |
| `u` / `ctrl+r` | Undo / redo; 100 entries, strictly last-in first-out. A step that brings rows back puts the cursor on the first of them, and a package that was open comes back open |
| `y y` / `y c` | Copy the shorthand of what the row shows (and remember it for `p`): a line or package its own, a grouping row its lines, a split package row its legs under that group / the column's cells |
| `p` / `shift+p` | Put the remembered rows below / above as one undo entry, the cursor on the first landed row; when any of them is a package the whole run lands at a root boundary. On a grouping row both put at the end of the sheet and say so |
| `shift+j` / `shift+k` | Move the row within its parent, and under a value grouping within its group (refused under a sort); a row's grip drags it (see [Moving rows by pointer](#moving-rows-by-pointer)) |
| `s` / `shift+s` | Sort by the cursor column: asc → desc → off / abs desc → abs asc → off (see [Sorting](#sorting)) |
| `g p` / `g u` | Package the cursor row and the next `count − 1` roots into a custom package / unpackage it (`:package [n]` / `:unpackage`; a counted one refused under a value grouping or a sort) |
| `g m` | Open a panel on the cursor row's underlying |
| `g .` | Open the row menu on the cursor row |
| `.` | Open the action menu |
| `shift+v` / `v` | Select rows / a block of cells from the cursor (see [Selection](#selection-2)) |

`g m` opens a panel on the cursor row's underlying, as the `underlying_ref`
context column the market-data panels accept: a line's or leg's own, a
package's when its legs share one; otherwise the plain tile picker. `g .`
opens the shell's [row menu](shell.md#row-menu) on the same context, hung
just under the cursor row (at the tile's top-left while that row is scrolled
out of view); a row with no single underlying shows `no actions for this
row`. A right-click on a row's cell, its tree cell, or the row beside its
cells opens the same menu at the pointer, on the clicked row's underlying.
A right-click inside a `V` row selection keeps the cursor and the
selection, closing an open editor (a bulk edit's live steps roll back);
anywhere else it clears any selection, closes an open editor or entry bar,
and moves the cursor to the clicked row, keeping its column. A
right-click inside the open editor's own cell is the editor's and opens no
row menu; blank space below the lines opens nothing.

Emitting into a [link group](#link-groups), the pricer posts the same
underlying `g m` opens on, as the group's scope, so the two never name
different underlyings for one row: a line's or a leg's own, a package's when
its legs share one. A package across underlyings, a grouping row, and a
sheet with no cursor row post no scope, which leaves the group's scope as it
was. Every cursor move, edit and load tells the shell to pull. The pricer
posts no documents.

The action menu offers repricing, packaging, unpackaging, undo, redo, deletion,
the sheet verbs (Open sheet…, Rename sheet…, New sheet, Remove sheet…; see
[sheets by pointer](#sheets-by-pointer)), and view selection. Key hints are
the actions' live chords (`:price` and the sheet rows' `:e`, `:name`, `:new`,
`:rm` when the keymap binds none; an empty lane for the other actions) and
follow a keymap reload while the menu is open. The menu opens on its first
enabled action, and keyboard stepping skips disabled rows, separators, and
headings. A pointer, or rows rebuilt under the highlight, can still leave a
disabled row selected. It has no highlight fill; choosing it shows its reason
in the footer and leaves the menu open. The shared rules are in
[Shared tile interaction](#shared-tile-interaction).

`y` alone is unbound: the key matcher dispatches an exact match at once, so a
binding on `y` would make `y y` and `y c` unreachable. `g` alone is unbound for
the same reason.

The underlying, type, and barrier-type cells edit through a typeahead. The
underlying list offers the sheet's own underlyings and also takes free text.
Ranking is the shared fuzzy match (see
[input and dialogs](input-and-dialogs.md#filtering-choice-and-movement)), so
the top-ranked option is only a guess: `enter` commits the highlighted underlying only when the query
equals it (in any case) or the highlight was moved with `up`/`down` or a row
click since the query last changed. Otherwise the typed text is committed
(upper-cased): typing `HSI` with `HSCEI` on the sheet commits `HSI`, and
typing `hscei` commits `HSCEI`. `enter` on an untouched, empty query keeps the
cell's value. Type and barrier type accept only their vocabulary, and `enter`
commits the highlighted option. An open editor tracks its line id and column kind through model and view
changes, moving the cursor with it. If either target disappears, it closes
without committing and shows `the cell moved; edit refused`. Deferred blur
uses the editor's opening window and checks current focus before blurring.

The expiry cell always edits in a segmented date field (year, month, day), the
same pure field the market-data grid uses: `left`/`right` move between
segments, `up`/`down` (`shift`: ten) step the active one, digits type into it,
`backspace` clears what was typed, `enter` commits and `escape` cancels. A
click on a segment selects it. A date expiry opens on its own date. A tenor
has no date in the pricer (the library's calendar resolves it), so the field
opens on today by the app clock and the footer says so; committing replaces the
tenor with that date, and `escape` leaves the tenor untouched. The footer's
tenor note stays until the field commits or cancels. A half-typed segment
refuses the commit and names itself (`finish the day or backspace`).

A cell commit is one undoable edit. A commit that parses to the value the
cell already holds applies nothing in any cell (no undo entry, no reprice, no
save): values are compared, not text, so `5000.0` on a `5000` strike is no
edit, and an empty shift on an inherited shift stays inherited. An explicit
shift is a change from inherited to own even when it equals the inherited
value.

Open editors paint no field chrome: no background, border,
radius, or horizontal padding. Their text sits where the cell's text sat
(numbers right-aligned, text left) at the row's height, and the cell's cursor
border is the only frame. The date field's segments are flush.

A grid click closes an open editor or the entry bar, then acts on the row it
hit. Closing the bar moves the table up on screen, so a double-click whose
first press closed it edits the line that press hit, not the row that slid
under the pointer; the hand-off lasts for the next press only. A chevron
press that closes the bar hands off the same way, so the cursor stays on the
package it toggled, and the double-click's second press does not toggle it
back.
Commands and search close open fields and menus. A text editor remains open
after a click outside the grid; a typeahead closes on an outside click.

#### Moving rows by pointer

A row `shift+j`/`shift+k` could move shows a grip (⋮⋮) at the left edge of
its tree cell while the pointer is on the row. It is painted over the
column's padding and the cell's edge, so nothing reflows, and sits clear of
a package's chevron. Hover and pressed take the chevron's control paint, and
the cursor is the grab hand. No grip paints on a grouping row, under a sort,
on a package the grouping splits or the scope partly hides, or on a leg of a
split package: each would refuse the keys, so a grip there would only
promise a refusal.

Dragging the grip moves its row; a grip on a row inside a live `V`
selection moves the whole selection, under `move_plan`'s rule (one parent,
one group), and a selection that would refuse the keys refuses at the press
with the keys' footer. A drag of any other row while a selection is live
(a row outside the `V` selection, or any row under a `v` block) ends the
selection as the drag starts, with `selection cleared: a row moved`: the
selection spans painted rows, so a row moved into or out of them would
widen or shift it onto lines nobody picked (a following `d` would delete
the dragged line). A grip click with no drag keeps it, and so does a refused drag (the press refused, or a rebuild mid-drag made the move one the keys refuse): it keeps its refusal in the footer and moves nothing. On a package row the grip overlaps the chevron's slot; a press there is the grip's, never a chevron press. While the button is held a 2 px line in the theme's
drag-border color (the header's column-drop line) marks the nearest legal
gap: between rows of the same sibling set (the roots, or the package's legs)
and, under a value grouping, of the same group. Over a package's open legs
the line snaps to the package's edge, and the empty body below the last
row is the gap after it. No line shows at a gap that would change nothing
(the dragged rows' own edges), over another group's rows or another
package, or off the table, and a release there moves nothing. A rebuild
mid-drag re-prepares the drop against the new rows; one that makes the
move one the keys refuse (a sort turned on, the package split or partly
hidden) refuses the drop, so the line goes and the release moves nothing.
Held within a row of the body's top or bottom edge, the table scrolls on
its own and the line follows the rows passing under the still pointer.
`escape` (any verb, in fact) ends the drag with nothing moved.

The release lands the rows as one undo entry: one `Edit::Move` for a single
row, one per selected row for a block. Rows of other groups, and lines the
scope hides, keep their order. The cursor lands on the dragged row, unless a
selection is live, whose cursor is its moving end and stays. An open editor
or entry bar closes as for any grid click. The grip's own press, click and
double-click never start, extend or clear a selection, move the cursor or
open an editor; every other press and drag selects exactly as before.

Known limitations: the drag paints no ghost of the dragged rows, only the
drop line; the gap is read from the table's uniform row height and scroll
offset, not from painted row bounds; and the line is a row child painted
under the row's cells, so a selection or cursor tint lies over it.

An open action menu recomputes availability and views when tile chrome
rebuilds, retaining its highlighted action or view when still present. Both
entry and cell editors, the date field included, put the tile in insert mode:
bare and shifted letters and digits are typed into the field (the date field
ignores what it cannot use). Insert bindings leave shell chords such
as `ctrl+k` available.

The `:` verbs change only this tile: `view <name>`, `shift spot|vol <n>|clear`,
`spot <underlying> <level>|clear`, `price`, `refresh <duration>|off|default`,
`package [n]`, `unpackage`, `group <columns>`, `group slot <n>`,
`group none`, `unpin`, `e <sheet>`, `new`, `name <sheet>`, and `rm <sheet>`.
`view`, `refresh`, `shift`, `spot`, `package`, `unpackage`, and `name` (and
the menu's view rows) are refused while the sheet is still loading, because
the loaded document would replace what they set. `group` and `unpin` are
not: the grouping belongs to the tile, not the document. `:ungroup` is not
a verb, and a `:group` whose every level the tile would drop (`:group 2`,
`:group nosuchcol`) refuses with `no groupable column in <columns>; :package N
packages lines` and pins nothing — it never packages rows. `:group none`
names no level, so it is not refused: it pins the flat sheet.

The sheet verbs work on this tile's own sheet, or, for `rm`, on a sheet no
tile holds:

- `:e <sheet>` saves the current sheet first when it has unsaved changes
  (a refused save keeps the tile where it is), gives its name back, and loads
  the other sheet (`loading…` until it answers). If that sheet has a save
  still queued, the read waits for the save's answer before it is sent:
  reads and saves run on different lanes, so an earlier read could return
  the generation before the save. A restored tile waits the same way. Switching
  sheets clears undo history, package expansion, cursor, and per-sheet save
  state, and cancels pricing in flight for the outgoing sheet. A sheet open in
  another tile is not switched to at once: two tiles never write one sheet,
  so the header asks `sheet 'x' is open in another tile: open it here and
  close it there? (y/n)` with the `:rm` confirm's keys, buttons and cancels
  (any answer but `y` leaves `sheet not opened`). `y` decides again: this
  tile's own unsaved changes are saved first, then the holding tile's. A
  refused save on either side stops the take there (`sheet 'x' not opened
  here: …`); the holder keeps its sheet and any open field, though this
  tile's own save may already have gone. Otherwise the holder cancels any
  open cell edit, entry bar, sheet field or menu and moves to the next
  `untitled-N` (its footer says `sheet 'x' was opened in another tile;
  opened untitled-N`, adding `(an unfinished edit was dropped)` when a
  cell edit or entry line was open), and the sheet loads here, waiting for
  the holder's save. If that save then fails, this tile's save slot says
  so (`sheet 'x' was not saved: …; its last edits were not stored`): what
  loaded is older than what the holder showed, and this tile's next save
  replaces it. The tile's own name does nothing,
  unless its load failed (`did not load`): then `:e` of it asks again,
  which is the way to retry a refused or failed load in place. Retrying
  discards edits made in the unsaved fallback sheet.
- `:new [sheet]` does the same into an empty sheet with no load: under the
  given name, else the next free `untitled-N`. A name that already exists
  (open in any tile, this one included, a known document, a queued save, or
  a sheet being removed) is refused with `sheet 'x' already exists; :e x
  opens it`, and the tile stays where it is.
- `:name <sheet>` is refused if the name is open, is a known document, or has
  a save still queued (`sheet 'x' already exists`), and while the sheet is
  loading or after its load failed. Otherwise the tile takes the new name at
  once and saves under it; the old name's document is removed only after a
  save under the new name is confirmed. If that save fails, the tile keeps the
  new name, the save slot shows the reason, and the old document stays until a
  later save under the new name is confirmed. An empty sheet saves nothing, so
  its old document stays. From the rename until that removal is answered, the
  old name is reserved: `:e`, `:name` and `:rm` refuse it (`sheet 'x' is
  being removed`) and `untitled-N` skips it; a restored tile naming it opens
  the next `untitled-N` instead (`sheet 'x' is being removed; opened
  untitled-N`). If a tile nevertheless holds the old name when the save is
  confirmed, nothing is removed. `:e`, `:new` or closing the tile before the
  save under the new name is confirmed gives up the removal, so both
  documents remain.
- `:rm <sheet>` is refused for any open sheet (this tile's own: close it or
  `:e` another sheet first) and for a name that is not a document. Otherwise
  the header asks `remove sheet 'x' and all its history? (y/n)` beside Yes
  and No buttons and holds the keyboard (the tile is in insert mode). Bare `y`
  or Yes removes the document and its whole history; any other key, No, a
  pointer press anywhere but the two buttons, or focus leaving it answers no
  (`sheet not removed` in the footer). `y` checks the name
  again: if a tile opened it, or a `:name` began retiring it, while the
  question stood, nothing is removed (`sheet 'x' not removed: it is open in
  another tile` / `…: it is being removed`). A removal the data service
  refuses says so in the footer (`sheet 'x' not removed: the data service is
  busy` / `sheet 'x' not removed: the data service has stopped`); one that fails after admission says so in the
  header. When `:name` retires the old name and the service refuses that
  removal, the header reads `old sheet 'x' not removed: …` with the same
  reason, and the old document stays. The name is reserved, as for `:name`, until the removal
  is answered.

A sheet name may not hold a control character: the store joins document key
parts with `U+001F`.

#### Sheets by pointer

Each sheet verb has a pointer and palette form that takes the command's own
route, so every refusal above applies unchanged. The palette actions
(`Open sheet…`, `Rename sheet…`, `New sheet`, `Remove sheet…`) have no
default key.

- A click on the header's sheet name (tooltip `Sheets`) opens the sheet
  picker under it: a filter field over every known sheet, every sheet a tile
  holds, and this tile's own, opened on the current sheet (ticked). A sheet
  another tile holds says `open`. Typing filters (the shared fuzzy match),
  `up`/`down` step, `tab` completes the field to the highlighted name, a
  hover moves the highlight, and `enter` or a row click picks through `:e`.
  A refusal shows in the footer and keeps the picker open; a sheet another
  tile holds closes the picker for `:e`'s take-over question; `enter` with
  nothing matching says `no sheet matches`. `escape`, a second click on the
  name, or a press outside closes it. The rows are prepared when the picker
  opens and do not follow later catalog changes; the pick itself decides.
- A double-click on the name replaces it with a rename field holding the
  name, all selected. `enter` renames through `:name`'s parse and route; a
  refusal shows in the footer and keeps the field; `escape` or a press
  outside cancels without renaming; `tab` keeps the keyboard in the field.
  A double-click renames only when both presses reached the name: one whose
  first press landed anywhere else (the open picker, another surface painted
  over the tile) is a single click. A press on the name with a modifier held
  is the shell's (a tile drag, a fullscreen double-click) and opens nothing.
  A rename refused whatever the
  name (loading, a failed load, an unconfirmed rename) never opens the
  field; the footer says why, and the menu's row is greyed with the reason.
- The menu's `Remove sheet…` opens the same picker to remove: a pick arms
  `:rm`'s confirm, and the current sheet is refused with `:rm`'s words.
  `New sheet` is `:new` without a name.
- The picker's filter and the rename field put the tile in insert mode, so
  letters typed into them never reach shell bindings. A command, a search,
  or any other verb closes them, as it closes the other fields.

A save's outcome goes to the tile that queued it, not to whichever tile holds
the name now. A tile that has moved on (`:e`, `:new`) and hears its old
sheet's save failed says `sheet 'x' was not saved: …; its last edits were not
stored`; if it is waiting to load that sheet again, the text goes in the save
slot, and a failure of the load that follows keeps it beside the
`did not load` notice. After `:name`, a failed save of the old name is not
reported: the edits travel under the new name, whose own save reports its
outcome. A closed tile's failed save is recorded only by the data tier's error
diagnostic.

`:e` and `:rm` complete from the known sheet names: the diagnostics catalog's
`pricer_sheets` documents (the factory asks for a catalog when it is created
without one), plus this session's confirmed saves, less its confirmed
removals. A later catalog adds names and never drops one, and never brings
back a name this session removed, until a save under that name is
confirmed: the catalog the diagnostics entity holds is refreshed only while
the diagnostics page is visible, so it can predate the removal. A name whose save is
queued but not yet confirmed counts as taken: a new tile's `untitled-N` and
`:name` skip it.

### The frame's scope

The tile follows its frame's scope. The sheet is not stored data, so the
scope is evaluated in process, by `geode_core::scope::eval`, over each sheet
line as a row of the `pricer` dataset; that evaluator is pinned to the SQL a
blotter's query runs (see [the parity contract](data-path.md#queries-and-time-travel)),
so a scope means the same thing on both. The scope is the frame's effective
scope with its named expressions resolved; the pricer has no tile scope
layer (`:filter`). A pricer following a [link group](#link-groups) reads
that group's scope in place of its workspace's.

A line's values are the ones its cells paint. Text columns read the painted
text; `strike` and `barrier` read the number as typed (a percent strike reads
its percent); a shift reads the shift its cell paints, the sheet's when the
line has none; `qty` reads the integer. Two text columns read a scope value
their cell does not paint, so a desk scope matches: `status` reads `fresh`
for a fresh line (whose cell is blank), beside `pricing…` and a failure's
text; `expiry` reads the ISO date `2026-12-18` for a dated expiry, whose cell
paints `Z26` or `20DEC26`, and a tenor's text (`3m`) otherwise. The text
filter therefore finds a dated expiry by `2026-12`, not by `z26`. A measure reads result ×
qty, the position value `risk_snapshot` means by the same name, although the
line's cell paints the per-unit result: `npv < 0` keeps a short line whose
per-unit npv is positive. A leg's `template` is its package's token, because
`template` is a position-grain column and a leg's position is its package; a
bare line has none. A blank cell, a measure on an unpriced or failed line,
and the currency of an unpriced line are NULL. NULL follows SQL: only TRUE
keeps a line, so `npv > 0` hides an unpriced line and so does
`not (currency = 'USD')`. The text filter searches the nine textual
dimensions (see [configuration](configuration.md)).

The scope is re-applied on every model rebuild: an edit, a price delivery, a
load or reload, a clock change and a frame change. A scope over a measure or
`status` moves lines in and out as prices arrive. The tile answers the
frame's flip barrier on every frame change, whether the scope applied, was
refused, or is ignored under `:unscoped`.

What the pricer drops and what it refuses:

- A dimension selection on a column `pricer` lacks (a desk-wide `book`
  selection) is dropped, as any dataset drops a selection it cannot answer.
  It does not blank the pricer.
- `position_ref` and `instrument_ref` count as columns the pricer lacks,
  directly or through a derived dimension. The sheet declares them for
  grouping, but their values are its own `p<id>` / `i<id>`, never a desk
  reference, so a desk scope on them could only hide every line.
- An expression naming a column `pricer` lacks refuses the whole scope, with
  `scope refused: 'book' is not a pricer column`. The pricer never evaluates
  the half of `underlying_ref = 'SPX' and book = 'X'` it knows.
- An unresolved named expression refuses with
  `scope refused: named expression '<name>' is missing` (or `is invalid`).
- A comparison DuckDB would reject refuses the whole scope with
  `scope refused: <reason>`, even when only one line's value fails
  (`underlying_ref = 5` casts every underlying to a number). A refusal that
  depends on no line (`strike like '5%'`) is checked once before any line,
  so an empty sheet refuses it too.
- A refused scope hides nothing. The refusal stands in the header as a
  danger notice while it holds, through deliveries and edits, and goes when
  the frame's scope becomes one the pricer can honour. A transient notice
  covers it while that notice lasts. While a view fallback notice also
  holds, the header shows both: the refusal, ` · `, then the view notice.

Hidden lines stay in the sheet: they keep pricing, saving and repricing on
the refresh timer. The header counts them in muted text as `N hidden`, absent
at zero. A package shows when any of its legs does. A package whose legs
are partly hidden paints only its shown legs: its leg count reads
`· N of M legs`, and its summary, aggregating columns, results, status,
priced time and find key cover only those legs. A selection total counts the
shown legs too. Painting the whole package's fold under a row whose legs are
partly hidden would be a plausible wrong total.

A partly hidden package's row is read-only. These refuse with
`package partly hidden by the scope: edit its legs` and change nothing: `i`,
`enter` or a double-click on its cells; a typed commit or a live step over a
selection containing it; and the structural verbs `d`, `shift+j`/`shift+k`,
`g p` and `g u`, from keys, the `.` menu, `:package` or `:unpackage`, because
each would act on the hidden legs too. A selection containing one refuses
whole. Yank, put, fold and find still work, and a shown leg edits and
deletes as usual. A counted `g p` takes the next sheet rows, hidden ones
included, so it refuses with `a line in that range is hidden by the scope`
when any of them is hidden. `:unscoped` shows the whole package to edit it.

When the scope hides the cursor's line, the cursor moves to the nearest
shown line above it in sheet order, else the nearest below, not to whatever
row slid into its index. A cursor restored from the session onto a hidden
line recovers the same way. An open editor whose line the scope hides is
dropped with `the line is hidden by the scope; edit dropped`.

A line the trader adds can land hidden: an entry-bar insert, a put, or an
undo or redo that restores a line the scope does not match. The line is in
the sheet and prices, but the cursor cannot rest on it, so the footer says
`added line is hidden by the scope (:unscoped shows it)` rather than letting
it vanish without a word.

`:unscoped` makes the tile ignore the frame's scope and show every line; the
header paints a warning `unscoped` chip whose tooltip says it ignores the
shared scope and that `:unscoped` re-attaches it. The flag rides the tile's
session record, as the blotter's does. `:unscoped` again re-applies the
frame's current scope at once.

`shift+j`/`shift+k` (and a `V` block move) step past siblings the scope
hides: one step moves the row past the next shown sibling, so a move always
changes what is painted. Swapping with a hidden sibling would change sheet
order with nothing visible.

Known limitations: the `.`
menu keeps `delete` and `ungroup` enabled on a partly hidden package and
refuses on pick rather than showing them disabled; the scope is re-evaluated
synchronously over every line on every rebuild, so its cost grows linearly
with the sheet (1,000 lines are measured in [performance](performance.md)).

### Grouping

The tile arranges its shown lines under a grouping chain, as a blotter
arranges positions, and follows the frame's grouping the way a blotter
does. The chain is, in order of precedence: a `:group <columns>` pin; a
`:group slot <n>` pin, which reads frame slot `n` (the view's own grouping
while that slot is empty); the frame's active slot; the planned view's own
`grouping`. `:group none` pins the empty chain: the flat sheet, whatever
the frame's grouping, with moves and a counted `g p` as in any flat sheet.
`none` is reserved beside `slot` — never read as a column, and `:group none
<anything>` is refused with `usage: group none`. `:unpin` drops any pin and
the tile follows the frame again at once. `:group` takes columns separated
by spaces or commas and completes the groupable ones, `none` and `slot`; a
bare `:group` refuses with `group needs columns, \`slot N\` or \`none\``,
and `:group slot <n>` on an empty slot with `slot n is empty`. The pin is
saved as `pinned` (an empty array for `none`) or `pinned_slot`. The frame
observer regroups before the tile answers the frame's flip barrier, so a
grouping change paints in step with the other tiles.

The chain's levels are the `pricer` dataset's groupable columns and the
derived dimensions over them (see
[configuration](configuration.md#validation-boundaries)). The tile drops a level it
cannot group by and keeps the rest:

- a column `pricer` lacks (a desk `lhu`), a measure, and a derived
  dimension over either, or over `position_ref` / `instrument_ref`, whose
  values are the sheet's own ids and could only map to NULL;
- a level after `position_ref` or `instrument_ref`, since a line is the
  finest node the tree has and nothing groups inside a package;
- a level already in the chain. The blotter keeps a repeated level (only
  its Groupings dialog refuses one), so a hand-written `[u, u]` groups once
  in the pricer and twice in a blotter.

The header shows the chain as written, kept levels in normal text joined by
a muted `›` and dropped ones muted and struck through, followed by a
neutral `pinned` chip while a pin holds. Nothing is shown for an empty
chain, except under `:group none`, where a muted `ungrouped` stands in the
chain's place beside the `pinned` chip. A `:group` whose every level would
be dropped is refused rather than pinned (see the `:` verbs above), so a
column pin always groups by something; only `none` pins the flat sheet.

`position_ref` and `instrument_ref` are structural levels: `position_ref`
is the package row itself (packages over their legs, bare lines alone) and
`instrument_ref` makes every leg a line of its own with no package row.
Either ends the tree. A chain of structural levels alone paints lines in
sheet order.

Every other level is a value level. A line is grouped by the value it reads
as a scope value (see [the frame's scope](#the-frames-scope)), so a group
and a scope mean the same value: an `expiry` group holds the ISO date
`2026-12-18`, a `status` group `fresh`, a leg's `template` is its
package's. NULL is a group of its own, apart from an empty value. Groups
order numbers by number, text by byte order (`NDX` before `SPX`), and NULL
last; inside a group, lines keep sheet order. The tree label reads the
value as the grouped column's cells spell it (an expiry group `Z26`, a
strike as the strike column formats it), NULL as `—`; a derived dimension
reads its label. An empty package (every leg removed) sits under the values
its own row reads, so under an instrument column it forms a lone NULL
group.

**Group rows.** A group row has a chevron and its label at medium weight,
and a ground of its own (`secondary` over the table ground); hover and
selection replace it as they replace a line's. Its text, and a view's named
column color, are floored to the readable ratio on that ground as well as
the hover and selected ones. Its cells:

- measures: the fold of every leg beneath, qty-weighted, as a package row
  folds its legs. Unlike local currencies paint `—` in the local columns
  while the `_usd` twins still sum; a failed leg makes the sum a danger `—`;
- `status` and `priced at`: the legs' fold, as a package row reads them
  (`pricing…` while any leg reprices, a failure's reason, the oldest
  attempt);
- `qty`: the number of legs beneath;
- the grouped column and every ancestor level's column: the group's value,
  blank for a NULL group;
- every other dimension: its value where every leg agrees, a muted `mixed`
  where two differ or a blank sits beside a value, blank where none has one
  (the blotter's rule).

Groups open and close like packages (`space`/`z a`, `z o`, `z c`, the
chevron). `z c` on a line whose parent is a group (a bare line, or a leg standing
alone under `instrument_ref`) closes the group and lands on it; on a leg
under a package row it closes that package, as without a grouping.
`z shift+r` / `z shift+m` open or close every group and package. Groups
start closed.

Find (`/`, `n`, `shift+n`) searches every row the tile would paint with
every group open, in that order — a group row by its label, a line by its
shorthand, a package by its key, which covers its legs whether or not it is
open. A match inside closed groups opens them (vim's `foldopen`) and the
cursor rests on it; the groups stay open after `escape`. A line that an
entry-bar insert, a put, or an undo or redo lands inside a closed group
opens that group too, so the line paints and the cursor rests on it (a line
the scope hides still reads `added line is hidden by the scope`).

The cursor follows its line across a regroup, landing on the nearest
painted group row when the line's group is closed. When an edit or a
delivery changes the value a line is grouped by, the line moves to its new
group and the cursor (and a `V` selection anchored on it) follows it there;
only a split package, painted once per group, falls back to a group row on
its old path. An open editor whose line a regroup puts inside a closed group
is dropped with `the line moved to another group; edit dropped`. A regroup
to a shorter chain forgets the open state of groups deeper than it.

**Split packages.** A package whose legs fall under different groups
appears under each, with only that group's legs: a calendar under `expiry`
sits under both dates. Its leg count reads `· n of M legs`, and its summary,
cells and find key cover only those legs, as for a package the scope partly
hides. A selection total counts each split row's own legs, so the two rows
of a calendar add up to the whole package.

**What is read-only.** A group row has no line behind it; a split package
row would reach legs another group paints. Both refuse cell edits (`i`,
`enter`, double-click, a selection's typed commit or live step) and the
structural verbs `d`, `shift+j`/`shift+k`, `g p` and `g u` (keys, the `.`
menu, `:package`, `:unpackage`), with `a grouping row: edit its lines` and
`split package: edit its legs` in the footer; a package both split and
partly hidden reads the split reason. A selection containing either refuses
whole. A split package's legs, and the lines under a group, edit as usual.
`y y` on a group row copies the shorthand of its lines, and a `V` selection
over a group row totals and yanks its lines once, whether or not their rows
are selected too. `y y` or `V y` on a split package row yanks that row's
legs, as its cells show them, not the whole package. A selection cannot start on a group row (`select from a
line or package row`), though its moving end can rest on one. `o`,
`shift+o` and `p` / `shift+p` with the cursor on a group row have no line to
land beside: the bar's label reads `at end`, and a put lands at the end of the sheet with `put at
the end of the sheet` in the footer. `g m` on a group row opens the plain
tile picker.

**Moving and packaging under a grouping.** Under a chain with a value level
the painted order is sheet order only inside each group, so
`shift+j`/`shift+k` (counted, and a `V` block) move a row among the siblings
painted in its own group: a step hops the sheet lines of other groups and
hidden lines between them and lands beside the next sibling of the same
group, so the group's painted order changes as the key says and no other
group's does. A root at its group's end refuses with `cannot move past the
end of the group`; a leg moves among its package's legs and stops at the
package's end (`cannot move past the end`). A split package and a leg of
one refuse with the split reason, since their siblings paint under several
groups. Undo restores a move in one step; the cursor and a selection
follow the moved lines. A grip drag holds to the same group (see
[Moving rows by pointer](#moving-rows-by-pointer)). A counted `g p` (or
`:package n`) refuses with `a counted g p packages in the flat sheet: clear
the grouping first`: it takes the next rows in sheet order, which may be
painted under other groups. A plain `g p` still packages its one line, and
`g p` under `V` still packages a contiguous selection. A chain of
structural levels alone moves and packages as the flat sheet does.

Known limitations: the `.` menu keeps `ungroup` enabled on a split package
row and refuses on pick; a group the scope hides entirely loses its open
state, so it comes back closed; a group's label is spelt from its first
leg, so two legs whose values group together but spell differently (an
absolute and a percent strike of the same number) label by the first; a
cursor on a group row is not saved (a restored tile starts on row 0).
Blotter group rows have no ground of their own, so the two tiles' group
rows look different.

### Sorting

The pricer sorts as the blotter does, on the same surface: `s` walks the
cursor column through asc → desc → off and `shift+s` through abs desc → abs
asc → off (either key, pressed while the other's order shows, starts its own
cycle); a header's sort icon walks desc → asc → abs desc → abs asc → off, a
column with no magnitude skipping the absolute pair; `:sort <column> [asc |
desc | abs [asc | desc]]` and `:sort clear` complete the planned columns and
the order words. Only the result measures (`npv`, the greeks and their `_usd`
twins) have a magnitude: `shift+s` on any other column does nothing, and
`:sort <column> abs` on one is its signed direction. The tree column never
sorts and has no icon. The sorted column's header shows the direction's
arrow, and an absolute order adds `|x|` to its label (`npv |x|`).

A sort is display order alone. The sheet's own order, the order lines were
entered and moved into, never changes, and `:sort clear` restores it exactly.
The sort is not saved with the session or the sheet and is not an undoable
edit. It names its column, so a column move keeps it; a view switch or
reload whose plan lacks the column drops it, the rows go back to sheet order,
and the header warns `sort on '<column>' dropped: the column is no longer in
this view`.

Every sibling set ranks on its own, at every level of the tree: grouping rows
among their siblings by their own value in the sorted column (a measure's
fold, a dimension's unanimous value or `mixed`), packages and lines among
theirs by their row's value. A package moves with its legs, which keep sheet
order beneath it. A package split by the grouping or partly hidden by the
scope ranks by the value its row paints, over the legs its node holds. Keys
are typed, never the formatted text: numbers compare as numbers, an expiry
as a date (a tenor, which the pricer never resolves, after every date by its
nominal length), a strike by number (percent strikes after absolute ones),
text by byte order. A package whose legs disagree compares by its distinct
values in leg order, the parts its cell joins with `/`; a shift cell's parts
are its painted ones, legs that spell alike as one part and an unset group's
`—` at its place, after any value. Values come first, then cells with no
single value (`mixed`, a failed line's `—`, a local figure over unlike
currencies, a `NaN` result), then blanks such as an unpriced line, in both
directions; ties keep sheet order.

Prices re-rank a measure sort live: a delivery that changes the order moves
the rows, the cursor staying on its line, and one that leaves the order
alone refills only the window. With no sort, a price delivery does no
ranking work. While a `V` or `v` selection is live the order freezes
instead: a selection spans the painted rows between its ends, so a line
re-ranked into that range would join it unasked (and `d` would delete it).
Values refill in place, and the order the ticks earned applies when the
selection ends (`escape`, a verb that consumes it, a click that clears it).
A sort change (`s`, `shift+s`, a header click, `:sort`, or a view switch or
reload that drops the sort) with a live selection ends the selection first,
with `selection cleared: the sort reordered its rows` in the footer. A
selection verb that refuses (`d`, `g p`, `g u` over a selection) puts the
selection back with the order it held.

The verbs that read sheet adjacency refuse while a sort applies:
`shift+j`/`shift+k` (and a `V` move) with `lines move in sheet order: :sort
clear first`, and a counted `g p` (or `:package n`) with `a counted g p
packages in sheet order: :sort clear first`. A plain `g p` still packages
its line, and `g p` under `V` packages rows that are contiguous in the
sheet; rows adjacent only on screen refuse with `the selected lines are
apart in sheet order: :sort clear first`. `o`, `shift+o`, `p` and `shift+p` still insert
beside the cursor's line in sheet order; the new line paints where it sorts,
and the cursor follows it there. Find (`/`, `n`, `N`), line numbers and
selections follow the painted order, and `y y` and `V y` copy and remember
rows in the order the screen shows them.

Known limitations: the sort icon's arrow is the component's own, and is
re-read from the tile after every click, but has not been checked on a real
display; the `/` result header paints no sort icon (it reserves the icon's
width so labels keep their places); an absolute sort's ` |x|` suffix is not
counted in the default widths, so a long measure label can ellipsize under
one until `:autosize`, which measures it.

### Selection

The sheet's grid selection follows the [blotter's](#selection): `V`
(`pricer::visual_rows`) selects whole rows and `v` (`pricer::visual_block`) a
rectangle of cells; the other key switches kind at the same anchor and the
same key again clears. With no row under the cursor (an empty sheet) both
refuse with `select from a line or package row`. While a selection is live
the tile reports `mode == visual` with a `select == rows|block` pair (see
[context predicates](keymaps.md#context-predicates)); motions extend it and
the first `escape` clears it alone.

The anchor is the row's line id and the column's name, so a repricing, an
edit elsewhere, a move or a view reload keeps the same cells selected. An
anchor no longer painted — its package collapsed, its column dropped from the
view — clears the selection with `selection cleared: anchor row no longer
shown` (or `anchor column`); no neighbour is guessed. `:e` and `:new` clear
it without a notice, because line ids restart per sheet and the anchor would
name an unrelated line of the next one. `:name` keeps it: a rename changes no
line id.

The tint, the theme's selection color, overlays each cell under its text, so
a stale or failed cell's text color still shows;
the cursor keeps its border over it. A row selection also tints the tree
column as each row's handle.

**Verbs.** In visual mode the verbs are single keys; the doubled normal-mode
forms (`y y`, `y c`, `d d`) are not bound there, nor are `p`, `shift+p`,
`u`, `ctrl+r`, `o`, `shift+o`, `n`, `shift+n`, `space`, the `z` folds, `g m`,
`g .` and `.` (the palette still reaches them). A verb that refuses keeps the
selection and says why in the footer; a success notice goes to the header.

- `y` ends the selection. Under `V` it copies the shorthand of the top-most
  selected rows (a package, not also its selected legs, since the package's
  shorthand already carries them), one per line, and remembers their rows, so
  a following `p` or `shift+p` puts them all back at once, as one undo entry
  with the cursor on the first landed row. When any of them is a package,
  the whole run lands at a root boundary, since packages cannot nest. Under
  `v` it copies the block as TSV under its column labels and leaves the
  remembered row as it was: a block is not rows.
- `d` under `V` deletes the top-most selected rows as one undo entry,
  remembers them for `p`, ends the selection, and notices `deleted N rows`. A
  leg selected without its package is deleted as a leg.
- `shift+j`/`shift+k` under `V` slide the selected rows one sibling step as a
  unit — one move of the neighbouring row across the block — and keep the
  selection on the moved lines. Under a value grouping the step is within
  the block's group, as for one row. Rows under different parents refuse
  (`can't move: selection spans packages`), as does the end of the parent
  (`cannot move past the end`) or of the group (`cannot move past the end of
  the group`).
- `g p` under `V` packages the selected root lines into one custom package,
  opens it and puts the cursor on it, ending the selection. It refuses a
  selection that includes a package, lines inside a package, or lines that
  are not contiguous — `Group` takes a run, so a gap would sweep an unselected
  line into the package.
- `g u` under `V` dissolves every top-most selected package as one undo
  entry and ends the selection; with no package selected it refuses with `no
  package selected`.
- Under `v` the row verbs refuse and name `V` (`d deletes rows — use V`,
  `shift+j/k move rows — use V`, `g p packages rows — use V`, `g u
  unpackages rows — use V`): a block's cells are not a set of rows, and acting on its
  rows would edit rows never picked as rows.

A count on `d`, `shift+j`/`shift+k`, `g p` or `g u` is ignored while a
selection is live: the selection names the rows. Motions still take a count.
Every row verb refuses while the sheet is loading.

**Edits act on lines.** `i`, `I` or `enter` opens the editor on the cursor cell,
which must itself be editable: a read-only cursor cell refuses with its own
reason. An edit then reaches each selected line; a selected package stands for
its legs whether it is open or not, and a package selected with one of its
own legs writes that leg once. A double-click is not a bulk edit: its first
press clears the selection, so it opens a single-cell editor.

**One typed value.** Committing typed text, a choice picked from a list, or a
date writes it to the cursor's column on every selected line — under `v`
too, however many columns the block spans: one text parsed into several
column grammars (qty `5` and strike `5`, a type in the underlying) would be a
plausible wrong value. Each line is judged on its own instrument. A package
quantity goes through the package's template weights, as its own cell's edit
does, so a `-5/+5` spread typed `3` becomes `3/-3` rather than `3/3`; a
package whose legs no longer fit its template (the list form) is refused and
none of its legs written. The writes are one undo entry and one reprice; a
cell already holding the value counts as set with no edit, and a commit that
changes nothing records no entry. The header notices `set 5 cells, skipped 3
(2 read-only, 1 n/a)`, counting read-only cells, barrier cells on a vanilla
line (`n/a`), and refused values.
When no selected cell accepts the value, nothing is written and the editor
stays open with `no selected cell accepts '<text>'` in the footer. The
selection stays after a commit. Because the cursor's column is what a commit
writes, `enter` re-checks that the cursor still sits on the editor's cell;
if it does not, nothing is written and the editor closes with `the cell
moved; edit refused`.

**Live steps.** On a qty, strike, barrier, spot shift or vol shift cursor
cell with its text untouched, the editor's `up`/`down` (`shift`: ten) step
every target cell in the sheet at once: the cursor's column under `V`, every
block column under `v`. Each cell steps by the precision its own text
carries. A shift cell that inherits the sheet's shift steps from the value it
paints, so under `:shift spot 2` an `up` makes its own `+3.0`; a line with its
own shift steps from that. A selected package's quantity steps as the package
quantity through the template weights, even when the cursor sits on one of
its legs: that leg moves by its template weight, so a leg weighted negative
goes down on `up`. Each press reprices through the ordinary path,
so the grid shows the block and its prices as they move; the editor follows
its own cell and the header reads `stepped N cells +S` with the running
total. Cells that cannot step (read-only, not a number, a barrier on a
vanilla, a list-form package quantity) are skipped and counted. A press is
all-or-nothing: if the sheet refuses any stepped value (a quantity stepping
to zero), the press writes nothing and the footer says why.

- `enter` on the untouched text keeps the steps as one undo entry; steps that
  net to nothing leave no entry. With no step taken, it writes nothing.
- `escape` takes the steps back out of the sheet, provided they are still its
  last change: no other recorded edit since and every stepped line as the
  last step left it. Otherwise the steps are kept as one undo entry, since
  replaying their inverses would undo the other write. A click elsewhere, a
  verb, the menu, `:` and `/` cancel the same way. The palette's `undo` is
  such a verb: mid-step it takes the steps back, then undoes the entry
  before them. The rollback re-arms the save, replacing any save taken
  mid-step.
- Closing the tile or quitting mid-step takes the steps back by `escape`'s
  rule before the final save and closes the editor, so the save never stores
  steps that were not kept. When the steps are no longer the sheet's last
  change they are recorded as one undo entry instead, and saved with it.
- Typing makes the edit absolute: on `enter` the steps come out, by
  `escape`'s rule, and the typed value replaces them as one entry. From then
  on the arrows nudge the editor's text alone.

A cursor cell that does not step has no live step. An untouched `enter` there
writes nothing either: the editor closes with no notice and no undo entry,
since the cursor cell's own value filled across the selection would be a
plausible wrong block from a no-op gesture. Untouched means a choice list
whose highlight never moved and whose query is empty or the option it opened
on, a date field on the date it opened on (a tenor's today) with no digit
typed, or a text field (a package's expiry or type cell) on its opening text.
A moved or clicked option, a typed option, a changed date, a typed segment,
or edited text commits to the cursor's column as above.

**Footer totals.** While a selection is live the footer shows its extent
(`3 rows × 12 cols`) at the left and, at the right under the risk columns,
one position total for each measure column the view shows (`npv`, `delta01`,
and every other measure, local or `_usd`), painted as that column paints its
numbers. A line counts
`qty × value`; a package counts its own folded sum, which is already weighted
by its legs' quantities. Totals are over the
top-most selected rows, so an open package selected with its legs is not
counted twice. A column with any selected row unpriced or failed shows a muted
`—` rather than a partial sum — a failed line keeps its old result, so the
state is checked, not only the value. A stale line still showing a result
counts. A footer refusal or a line failure takes the footer while it stands.

**Mouse.** A plain press anywhere on a row, including beside its cells,
clears the selection and moves the cursor. A shift press starts a selection
at the cursor as it was before the press — a block from a value cell, rows
from the tree cell or the line-number gutter — and extends it to the pressed
cell; with a selection live, shift+press extends it. A drag selects
continuously from the cell it started on, its kind decided where the press
landed; a drag whose press no cell caught (the header, a scrollbar, a
divider) selects nothing. A chevron press is a plain press: it toggles its
package and clears a live selection, and never starts one, even with shift.
A press inside the open editor's own cell (caret, text selection, a date
segment or separator) belongs to the editor: it neither cancels the edit nor
takes a live step back. Any other gesture closes an open editor first, as a
cancel. A shift press with the entry bar open closes the bar on the click and
leaves the selection live, the keyboard back with the tile.

Limitations: a selection is one contiguous row range or rectangle; `p` puts
only rows a `V` yank remembered, so a copied TSV block cannot be pasted; a
count is ignored on `d`, `shift+j`/`shift+k`, `g p` and `g u` while selecting; a
package quantity in list form cannot be bulk-set or stepped, and its skip
reads only `refused`; a typed value under `v` fills one column, not the
block.

### Repricing

Every edit that changes a line's request bumps that line's revision and marks
it stale. The tile submits when some stale line is not already in flight at its
current revision, and each submission carries every stale line in one batch.
An outcome tagged older than the latest submission is dropped whole. A result
for an older revision is ignored, and the line, still stale, is resubmitted.
Hiding a tile requests cancellation by key and defers further pricing until
shown, keeping its stale marks. Cancellation is best effort and cannot retract
emitted outcomes; matching deliveries can still apply while hidden.

A submission refused because the request queue is full paints `pricing
request refused: the data service is busy; retrying` over the header notice
without replacing it, and retries: after one second, then doubling per
consecutive refusal up to thirty seconds. The first refusal of a streak logs a warning on
`geode::pricing` with the tile id; the rest of the streak logs nothing. The
streak ends when a submission is admitted or when nothing is left to ask for
(its lines were answered or deleted); the notice it covered then shows again,
and the next refusal starts over at one second. `escape` does not clear it.

A submission refused because the data service has stopped arms no retry: the
service is declared stopped and never restarted, so every retry would repeat
the refusal. The header shows `pricing request refused: the data service has
stopped` for the rest of the tile's life, over any other notice, and the tile
submits nothing further: no refresh tick, edit, or `:price` asks the service
again. It logs one warning on `geode::pricing`.

The refresh timer marks every line stale and resubmits while the tile is
visible and the sheet has a line. `[pricing] refresh` sets the default
interval (see [configuration](configuration.md)), and `:refresh` overrides it
per sheet. `:price` reprices at once. The tile answers frame flip barriers
itself because it submits no view query, so a scope change never waits on it.

### Persistence

Sheets are stored in DuckDB as documents of the `pricer_sheets` dataset, one
document per sheet, keyed by the sheet name, with a row per line (the `line`
axis) and the sheet's view, shifts, overrides, and refresh as attributes. The
app declares the dataset in its builtin configuration layer as `local`: only
the app writes it, no source feeds it, and its publications do not advance the
frame's data revision. `sheet` is not categorical, so sheet names never appear
in the frame picker or the groupings. The declaration is frozen: its tables
are created once and written positionally, so a changed column list would put
values into the wrong columns of an existing database. A layer redeclaring it
differently is ignored with an error diagnostic (see
[configuration](configuration.md)); changing it needs a migration, which does
not exist.

Every save is a new generation. A sheet keeps its live generation and the 200
before it; older ones are swept on the writer after a save that crosses the
bound. Loads read the live generation (as-of browsing is not offered). A
removal (`:rm`, or `:name` retiring the old name) deletes the document and its
whole history.

A sheet is saved as a whole document one idle second after its last change.
Closing the tile saves any change not yet saved, whether it was still waiting
on the idle timer, was refused by the store, or was queued and then reported
failed. A save the store accepts is only queued; its outcome arrives later by
sheet name. A refused save (`sheet not saved: the data service is busy; the
next edit retries`) or a failed one (`sheet not saved: <the writer's reason>;
the next edit retries`) paints a notice in the header's own save slot,
separate from pricing notices: a refused pricing request cannot overwrite it,
a later successful request cannot clear it, and `escape` does not clear it. A
confirmed save clears its failure notice unless a newer submission refusal
still needs attention. Outcomes carry no link to the save that produced them;
every one is delivered, in the writer's order, so the last to arrive is the
latest queued save's. The next change and the close both retry. A close with
a save queued but unconfirmed writes nothing extra: the write is already
queued. An empty sheet publishes nothing.

A save refused because the data service has stopped reads `sheet not saved:
the data service has stopped`, and the tile stops asking for the rest of its
life, across `:e`: later edits, the idle timer, `:name`, and the close do not
call the store again, and each keeps that notice showing. The stopped save state and the stopped pricing state are
separate; each is set by its own first `Stopped` refusal.

Quitting the app attempts to queue every unsaved sheet before stopping the data
service. The writer drains queued local saves and removals, subject to the
request-capacity and quit-time limits below.

If a sheet's document fails to load (its rows do not decode, or the store
answers with an error), the tile shows an empty fallback and the save slot
reads `sheet 'NAME' did not load (…); edits are not saved`. Nothing is
published from that tile, so the fallback cannot become the document's latest
generation. A name with no document is not a failure: it opens empty and saves
normally.

While a load is pending the header reads `loading…`; `escape` does not clear
it (it is the only sign the load has not answered), and the answer does. Only
the latest load's answer installs. Hiding the tile cancels a pending load with
its pricing, so the next show asks again. A load that could not be submitted
at all is a failed load (saves blocked), not a pending one. A load starting
ends a standing pricing-refusal streak, so `loading…` is never covered by
`REFUSED`.

The session record keeps the sheet name, view, refresh setting, cursor line,
and open packages (only those still on the sheet: a deleted or unpackaged
package's id stays in the tile's open set until a load or `z shift+r` /
`z shift+m` replaces the set, so an undo reinstates it open, but it is never
saved). It also keeps the grouping pin (`pinned`, the pinned columns, or
`pinned_slot`, a slot 1–9; any other slot is ignored) and the open groups
as `expanded_paths`: one array per open group, a string per value from the
root, `{ null = true }` for a NULL value so it stays apart from an empty
string; a path holding anything else is dropped whole. An open group nested
under a closed one is saved too. A load pending at restore holds the saved
paths until the sheet arrives, and `:e` carries them to the next sheet. A
cursor on a group row is not saved; a line cursor is. A new tile takes the next free `untitled-N` name, skipping names open
in another tile, known documents, and names with a save still queued.
**Known limitation:** before the first catalog arrives a new tile can pick an
`untitled-N` that already has a document this session has not seen; its first
save adds a generation to that document, replacing its live contents. Previous
generations remain available only within the retention limit.

**Known limitations** of storage:

- Known names come from the diagnostics catalog. Until the first catalog
  arrives, `:e` and `:rm` do not complete a sheet this session has not
  saved, and `:rm` refuses it as unknown; `:e` still opens it by name.
- The quit flush publishes through the bounded 64-entry request channel. With
  a very large number of unsaved tiles at quit, some saves can be refused;
  the app is exiting, so nothing retries them.
- gpui waits for quit hooks only 200 ms. A save still running or queued when
  the process exits is lost; the database stays consistent at the previous
  generation.
- If the data service fails to open, requests admitted before that never
  answer (for every request kind, not only sheets): a tile shows `loading…`
  until it switches sheet, and a sheet with a save queued stays taken. Later
  submissions are refused `Stopped`, and the status bar shows `data service
  stopped — restart Geode`.

Other known gaps: the underlying typeahead does not yet offer catalogue
underlyings. Columns can be dragged to
reorder and resized with the pointer; both act on the open tile only. A view
change or reload restores the view's order; a dragged width is kept the way
an `:autosize` fit is (by vocabulary name in the session record, over the
view's width) until `:autosize reset` or the next `:autosize`, which
replaces every kept width. The tree column is pinned and neither moves nor
resizes. Persistent order and width belong to the Views dialog.

In-process pricing remains an upstream leaf. A feature submits definitions
through the data-service request path and receives outcomes through shell
delivery; it does not call a pricing implementation directly.

## Demo and application composition

`geode-demo-data` generates deterministic risk batches and market-data
documents. The [generator guide](../../crates/geode-demo-data/README.md)
describes risk grains, deliberate ingestion edge cases, document sequences,
and the demo configuration files. `geode-app --demo` writes risk files under
a seed-specific temporary directory and streams serialized documents through
its in-process adapter. Its position service, `demo_positions`, rewrites
those risk files to carry a Move LHU, so a move survives a relaunch until
the directory is deleted.
Both use the same ingestion, parsing, query, and delivery paths as configured
sources. Demo configuration remains below desk and user overrides. Cached
sources and database contents are reused, so schema or generator changes may
require clearing the demo directory; see the [application README](../../crates/geode-app/README.md).

Demo series provide deterministic minute bars in fixed weekday sessions from
14:30 to 21:00 UTC, without holiday or daylight-saving rules. One source
advertises identities and another requires manual entry. Document production
and ingestion are asynchronous; starting the app does not guarantee data is
ready for the first frame.

`geode-app` is the composition root. It loads configuration, initializes GPUI
and logging, builds registries, creates the data service and bridge, registers
module factories and the row menu's actions (Open in Nemo, from
`geode-nemo`, then Move LHU, from `geode-positions`), installs globals, and opens the window. Cross-layer policy
that depends on the assembled binary belongs here; feature behavior does not.

## Testing and performance

Feature state machines are tested without a window. GPUI tests host the real
tile for focus, key, pointer, popup, and delivery behavior. Render delegates
are tested through prepared models rather than pixel claims unavailable to a
headless context. Display checks remain necessary for exact color, geometry,
and animation.

`geode-app`'s startup-composition tests build the real services from
configuration directories and open the real shell window, but do not attach
the data bridge: its event drain is woken from the real data thread, which
gpui's test scheduler refuses. The bridge's own tests attach it over a test
data handle, and each feature crate tests delivery into its tiles.

Benchmarks cover blotter flattening and formatting, market-data model building,
chart preparation, series querying, document parsing, line-pricer core
operations, and the line-pricer grid build. Budgets and current gaps are
recorded in [performance.md](performance.md).
