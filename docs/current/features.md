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
renderer. Hidden tiles may release subscriptions. On becoming visible they
compare followed versions and request anything stale.

Every module follows these interaction rules:

- `:` changes only the focused tile; global changes use actions and palette
  flows.
- `/` searches within the tile.
- Pointer commands have keyboard equivalents.
- Insert-mode inputs own focus only while editing and blur before they close.
- Stack state is visible through the shared marker in the module header.
- Delivery matches are exhaustive so a new outcome cannot be ignored silently.

## Blotter

`geode-blotter` renders any configured view as a collapsible hierarchy. The
data service returns every grouping depth in one `Snapshot`; the pure blotter
core resolves columns, builds visible rows, retains expansion by path, formats
the visible window, and handles cursor, find, and yank behavior.

Attribution metadata decides whether a measure is meaningful at each depth. A
non-attributable cell is shown as NULL rather than a plausible but incorrect
sum. Pinned grouping, unscoped mode, local as-of, filtering, and named colours
are tile state and survive through the module's session record.

The `DataTable` delegate paints a prepared row model. Rendering does not
recompile columns or format the whole dataset. Publication watches are scoped
to the datasets the view reads, and global frame changes are staged through
the flip barrier.

## Market-data documents

`geode-documents` owns typed wire-format parsers and writers. A parser produces
`DocumentRows`, the shared struct-of-arrays representation, without opening a
file or socket. `geode-data` knows only the `DocumentKind` trait; `geode-app`
registers the concrete CVI and dividend kinds.

`geode-marketdata` renders a document as either a matrix or a flat typed table.
`PanelSpec` describes axes and value columns. `MatrixModel` is rebuilt on a
delivery or structural edit. Ordinary cell commits patch it when possible;
editing a Sent draft rebuilds to clear sent styling throughout the grid.

Edits live in a `Draft` over a base identified by the document's source time.
The default Hold policy retains the base snapshot when available after a
document with a different source time arrives. `:auto` selects how later
deliveries resolve such a transition:

| Policy | Effect on unsent edits |
|---|---|
| Hold | Enter Behind and retain the base when available until explicit rebase or revert |
| Rebase | Move edits by row and column labels, reporting labels that cannot be resolved |
| Replace | Discard edits and report how much unsent work was replaced |

Changing policy does not retroactively apply it to a held delivery. Redelivery
of the same source time does not trigger it, and the first usable delivery
after session restoration uses Hold. If the saved base is unavailable, a
restored Behind draft paints the delivered grid while withholding unresolved
cell edits. Automatic rebase also holds when the
incoming document has no rows. Sent drafts follow the separate echo rules
below. A snapshot that cannot build a valid grid leaves the last usable model
and draft unchanged and reports the error.

Source time is not an immutable generation id: a historical delivery can also
put the draft Behind, while different contents republished with the same time
are indistinguishable to this transition logic. Returning to the base time
restores Editing. The module supports numeric, date, text, and closed-choice
cells, row insertion/deletion, and kind-specific actions. See the
[crate guide](../../crates/geode-marketdata/README.md) for grid, popup, and
command-parser contracts.

`[ui] line_numbers` adds a gutter beside the grid's pinned column: the row
label when shown, otherwise the first value column. The column widens for the
gutter while cursor borders, draft fills, and deletion marks stay on the data
cell. Numbering includes inserted and deleted rows in painted order. Relative
mode uses absolute numbers while the cursor is in the header attribute strip.
Numeric and date editors retain the displayed value's alignment and text origin
inside the cell.

Dividend row labels use the ex date and a same-date ordinal (`<date>#n`).
Rebase drops cell edits and deletions in a same-date group whose row count
changed, because the old ordinal may identify a different dividend. The draft
captures group sizes from its painted base before rebase or session save.
Restored drafts without these saved sizes cannot apply this guard. A reorder
within an unchanged-size group remains undetectable and can move an edit to
the wrong dividend.

Session drafts store cell edits with row and column labels, allowing restore
to resolve them against a delivered grid. Attribute serialization has a type
ambiguity: a text attribute that looks like an ISO date restores as a Date.
The saved draft therefore does not preserve every attribute value's type.

### Uploads

`:upload [target]` and the action list's `Upload` row send the edited document
to a configured egress target. Upload requires a complete Editing draft, an
eligible target, and no other upload in flight from the tile. Both the frame
and the painted generation must be live. A live frame can still show a
historical generation while a requery is pending or after it fails; uploading
that document would overwrite untouched rows with old values.

Arming confirmation assembles the rows and snapshots the draft. The header
shows the target and counts of changed cells, attributes, added rows, and
removed rows. Bare unmodified `y` submits that snapshot after rechecking the
live frame, live painted generation, and full draft equality. Every other key
cancels and is consumed, including chords. A pointer press on the tile or loss
of focus also cancels. A delivery that changes the draft or painted generation
withdraws the prompt.

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

A Sent draft compares the next delivered generation with a different source
time against the submitted document. Comparison ignores row order and minted
row labels, compares attributes, and allows one ULP for floating-point values.
Matching contents clear the draft and show `sent HH:MM, confirmed HH:MM` until
the next edit. This is a content match, without an upstream correlation id.

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
| [`tile::data`](../../crates/geode-timeseries/src/tile/data.rs) | Fetch and query submission, delivery freshness, last-good results, and flip-barrier staging and promotion |
| [`tile::pointer`](../../crates/geode-timeseries/src/tile/pointer.rs) | Chart hit testing, wheel navigation, pan and split drags |
| [`tile::popups`](../../crates/geode-timeseries/src/tile/popups.rs) | Popup transitions, keyboard handling, commits, cancellation, and focus |
| [`popup`](../../crates/geode-timeseries/src/popup.rs) | Popup state types and rendering, including shared list-row layout and hit testing |

Settings and tiles obtain configured fetch sources from the shell-published
`SeriesSettings` global. A configured source is not proof that its adapter
started successfully. Fetch requests ask for the selected range; the data tier
subtracts covered spans. Completion is broadcast by `(identity, source)` and
updates affected slots. Visible affected tiles requery on every successful
completion, including zero new rows. Failed requests retain the last good
chart and report a notice or failed slot.

Series queries return aligned points, percentiles, bins, and coverage.
Expression slots may narrow results to buckets shared by their operands.
Delivery tags reject superseded queries. Results requested under a pending
frame flip are staged until promotion is allowed. This coordinates ready
results, but the barrier timeout can release them while lagging tiles still
show older data.

The series list, add picker, expression editor, range editor, action menu, and
colour picker share one `Popup` owner. The list has no text field; input popups
own their fields and key routing. Closing uses one cleanup path and blurs a focused input before
releasing it. Series and add-picker rows share geometry, theme treatment,
identity, and pointer handling, while supplying their own labels, controls,
and activation behavior. The range editor uses separate date-field rows;
the expression editor renders inline below the header in the tile body.

`geode-chart` is independent of series and shell concepts. Its pure core owns
scales, axes, layout, viewport, crosshair, hit-testing, decimation, and palette
derivation. `ChartElement` paints an immutable `ChartModel` through
gpui-component's plot surface. Paths and chrome are cached by the values that
affect them; cursor movement does not rebuild the data model.

The header's `⋯` button, a chip's right-click, and `.` open the action menu.
It offers popup openers, actions for the selected slot, frequency steps,
toggles, and view reset. Keyboard stepping skips disabled rows, separators, and
headings. Pointer selection can rest on a disabled row, which has no highlight
fill; choosing it shows its reason and leaves the menu open. Enabled actions
close the menu before dispatch. Key hints refresh when the menu opens or its
chrome rebuilds, so an open menu can retain old hints after a keymap reload.

Pointer controls and keyboard actions use the same model operations and
change processing:

| Pointer action | Effect |
|---|---|
| Click a series chip / its swatch | Select the slot / toggle its visibility |
| Right-click a series chip | Select the slot and open the action menu |
| Click the range and frequency readout | Toggle the range editor and keep keyboard focus in its input |
| Click a frequency chip in the range editor | Apply immediately, leaving draft dates unchanged; cancelling the editor does not undo frequency |
| Wheel over a plot | Dominant vertical motion zooms about the pointer; dominant horizontal motion pans; ties zoom |
| Drag a plot / the band between panes | Pan / adjust the split |
| Click Add or Compose in an empty tile | Open the corresponding editor |

Chart drags end on release, including a release outside the chart, or on a
move with no button held. Movement outside the chart surface is not tracked.
Modified presses and subsequent presses in a multi-click do not start chart
drags, leaving those gestures available to the shell. Right presses focus the
tile before its context menu handles keys.

A slot's colour is a palette index (`1`–`5`), a `[colours]` name, or an absolute
`#rrggbb`. Palette and named colours follow the theme. Absolute colours receive
no theme or contrast adjustment. Sessions store them as lowercase six-digit
hex; malformed hex restores the slot's default colour. Colour names beginning
with `#` are reserved. `c` cycles the palette, starting at colour 1 from a named
or absolute colour. `:colour s<n> <1..5|name|#rrggbb>` sets the colour directly;
an explicit hex remains absolute even if it matches a palette colour.

The action menu's `Colour…` row opens a picker at the selected slot's chip.
Its featured swatches capture the five palette colours and all named colours
as resolved when the picker opens. Picks are quantized to opaque 8-bit RGB:

- A pick within one step per channel of the slot's currently painted colour
  leaves its colour setting unchanged.
- Otherwise, a pick within that tolerance of a featured swatch retains its
  palette or name identity. The nearest swatch wins, with the first on a tie.
- Other picks become absolute colours, with alpha discarded.

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
allowing the market-data panel and as-of dialog to share behavior without
depending on each other.

## Diagnostics

`geode-diagnostics` presents shell-owned operational state: sources, stored
data, configuration, logs, and performance. The shell's `Diagnostics` entity
and shared log ring supply the state. `:section <name>` and `[`/`]` select a
section; log-level and performance-overlay changes use application actions.
The session saves the section and filter.

| Section | Contents |
|---|---|
| Sources | Descriptions, health, loading activity, and poll times; worst reported health first, unreported sources last |
| Data | Dataset and partition generations, with the resolved generation highlighted for a historical frame as-of |
| Config | Current config-load diagnostics, data-layer diagnostics, prior load batches, and effective values with layer provenance |
| Log | A bounded local tail of new records, with substring filtering and cursor following |
| Perf | Frame and requery timing, dropped-event count, and database resource metrics from the catalog |

The tile rebuilds only the selected section. Diagnostics counters, frame
as-of/config versions, and the log ring sequence gate observer work according
to the section's inputs. Clock changes and local section, filter, or collapse
changes also rebuild. Rendering shares prepared rows and cached header text;
an unrelated performance tick does not walk the config documents.

The tile does not query ordinary view data, but it still participates in frame
arrival so a global flip cannot wait on it indefinitely. Catalog requests are
bounded and coalesced by the app bridge. Hiding the diagnostics surface removes
watched demand while explicit catalog consumers can keep their own demand.
An as-of change requests a fresh catalog while the tile is visible. Until the
catalog's as-of matches the frame, resolved-generation markers are hidden.

Each tile retains at most 4,096 log records and reuses its drain buffer. It
starts at the ring's current sequence when opened. If the ring overwrites
unread records, the next drain reports that gap; this is not a cumulative
loss counter. Moving the cursor stops following, and `G` resumes it.

Source ages reflect the last row rebuild rather than a ticking timer; the
absolute timestamp remains visible. Config output is capped at 2,000 leaves
per document with an omitted-count row, although traversal still visits all
leaves. `/` filters the config and log sections by substring.

See the [crate guide](../../crates/geode-diagnostics/README.md) for the module
map and observer, notification, and allocation contracts.

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
dense header: the sheet name with `view <name>`, any sheet-wide shift chips
(`spot +2.0%`, `vol -1.0`, spelled as the shift cells spell them), `N pricing…`
while lines are stale, `N failed` in danger text while any line's last answer
was a failure, `pricer <name>`, the last priced time, which reads `stale` once
it is older than the shell's `stale_after`, and a `⋯` button at the trailing
edge that opens and closes the action menu (the pointer's `.`). A pending load
paints `loading…` muted in the header and `Loading sheet…` in the empty table;
an empty loaded sheet says `No lines — press o to add one`. A pricer this
binary lacks is named in danger text with its recovery (`set [pricing]
adapter and restart`).

Column headers are words carrying their unit (`spot %`, `vol pt`, `barrier
type`, `priced at`), and default widths are checked against labels and representative large
values (`-1,234,567.8900` for a greek, `-1,234,567.89` for a price) at the
largest supported font size. These examples do not bound every possible
value. A view's `label` and `width` override the defaults. Both bundled views end in a `status` column, which says
`pricing…` on a stale line and a failed line's reason, so neither state is
shown by colour alone. The tree column reserves a fixed chevron slot on every
row, so roots share one leading edge and legs sit one step in; the entry row
opens at the depth it will land at. A long tree label or text cell ends in
`…`; a number never truncates. Cell text is floored to the readable ratio on
the row's own ground and on the table's hover and selected-row grounds.

Lines and packages are rows of one table; a package row sums its legs and opens and closes like a tree node
(`space`/`z a`, `z o`, `z c`, `z shift+r`, `z shift+m`, or its chevron). A
package created in the session opens so its legs show; a restored tile opens
the packages its session record names. Package rows are read-only in every
column.

Normal-mode keys:

| Keys | Effect |
|---|---|
| `o` / `shift+o` | Open a shorthand entry row below / above the cursor; `up`/`down` walk the sheet's own lines as history, `enter` adds the line and opens the next placeholder, `escape` removes it |
| `i`, `enter`, double-click | Edit the cell in place; `up`/`down` (`shift`: ten) step a number by the precision its text carries |
| `d d` | Delete the row (a package with its legs) |
| `u` / `ctrl+r` | Undo / redo; 100 entries, strictly last-in first-out. A step that brings rows back puts the cursor on the first of them, and a package that was open comes back open |
| `y y` / `y c` | Copy the row's shorthand (and remember it for `p`) / the column's cells |
| `p` / `shift+p` | Put the remembered row below / above; a package always lands at a root boundary |
| `shift+j` / `shift+k` | Move the row within its parent |
| `g p` / `g u` | Group the cursor row and the next `count − 1` roots into a custom package / ungroup |
| `.` | Open the action menu |

The action menu offers repricing, grouping, ungrouping, undo, redo, deletion,
and view selection. Key hints show default bindings and do not reflect
rebindings. Keyboard stepping skips disabled rows, separators, and headings.
Pointer selection, the initial highlight, or a rebuilt menu can still leave a
disabled row selected. It has no highlight fill; choosing it shows its reason
and leaves the menu open.

`y` alone is unbound: the key matcher dispatches an exact match at once, so a
binding on `y` would make `y y` and `y c` unreachable. `g` alone is unbound for
the same reason.

The underlying, type, and barrier-type cells edit through a typeahead. The
underlying list offers the sheet's own underlyings and also takes free text.
Ranking is a case-insensitive subsequence match, so the top-ranked option is
only a guess: `enter` commits the highlighted underlying only when the query
equals it (in any case) or the highlight was moved with `up`/`down` or a row
click since the query last changed. Otherwise the typed text is committed
(upper-cased): typing `HSI` with `HSCEI` on the sheet commits `HSI`, and
typing `hscei` commits `HSCEI`. `enter` on an untouched, empty query keeps the
cell's value. Type and barrier type accept only their vocabulary, and `enter`
commits the highlighted option. An open editor tracks its line id and column kind through model and view
changes, moving the cursor with it. If either target disappears, it closes
without committing and shows `the cell moved; edit refused`. Deferred blur
uses the editor's opening window and checks current focus before blurring.

A grid click cancels an editor or entry field before acting on the painted
row's identity. Removing an entry placeholder therefore cannot redirect the
click to a neighboring row. Clicking the placeholder itself only closes it.
Commands and search close open fields and menus. A text editor remains open
after a click outside the grid; a typeahead closes on an outside click.

An open action menu recomputes availability and views when tile chrome
rebuilds, retaining its highlighted action or view when still present. Both
entry and cell editors put the tile in insert mode: bare and shifted letters
and digits are typed into the field. Insert bindings leave shell chords such
as `ctrl+k` available.

The `:` verbs change only this tile: `view <name>`, `shift spot|vol <n>|clear`,
`spot <underlying> <level>|clear`, `price`, `refresh <duration>|off|default`,
`group [n]`, and `ungroup`. `e`, `name`, `new`, and `rm` parse and refuse as
not built yet. `view`, `refresh`, `shift`, `spot`, `group`, and `ungroup` (and
the menu's view rows) are refused while the sheet is still loading, because the
loaded document would replace what they set.

### Repricing

Every edit that changes a line's request bumps that line's revision and marks
it stale. The tile submits when some stale line is not already in flight at its
current revision, and each submission carries every stale line in one batch.
An outcome tagged older than the latest submission is dropped whole. A result
for an older revision is ignored, and the line, still stale, is resubmitted.
Hiding a tile requests cancellation by key and defers further pricing until
shown, keeping its stale marks. Cancellation is best effort and cannot retract
emitted outcomes; matching deliveries can still apply while hidden.

A refused submission (a full request queue, or a data service that is gone)
paints `pricing request refused: …; retrying` over the header notice without
replacing it, and retries: after one second, then doubling per consecutive
refusal up to thirty seconds. The first refusal of a streak logs a warning on
`geode::pricing` with the tile id; the rest of the streak logs nothing. The
streak ends when a submission is admitted or when nothing is left to ask for
(its lines were answered or deleted); the notice it covered then shows again,
and the next refusal starts over at one second. `escape` does not clear it.

The refresh timer marks every line stale and resubmits while the tile is
visible and the sheet has a line. `[pricing] refresh` sets the default
interval (see [configuration](configuration.md)), and `:refresh` overrides it
per sheet. `:price` reprices at once. The tile answers frame flip barriers
itself because it submits no view query, so a scope change never waits on it.

### Persistence

The tile attempts a whole-document save after one idle second following a
change. Closing makes one final attempt for unsaved changes, including a
pending idle save or a previous refusal. The store can refuse again; releasing
the tile then loses those unsaved changes. A refused save paints a notice
in the header's own save slot, separate from pricing notices: a refused pricing
request cannot overwrite it, a later successful request cannot clear it, and
`escape` does not clear it. Only an accepted save does. The next change and
the close both retry. An empty sheet publishes nothing.

If a sheet's document fails to load (its rows do not decode, or the store
answers with an error), the tile shows an empty fallback and the save slot
reads `sheet 'NAME' did not load (…); edits are not saved`. Nothing is
published from that tile, so the fallback cannot become the document's latest
generation. A name with no document is not a failure: it opens empty and saves
normally.

The pending-load indicator remains until the load answers; `escape` does not
clear it.

The session record keeps the sheet name, view, refresh setting, cursor line,
and open packages still present on the sheet. The tile retains deleted or
ungrouped package ids in memory so undo can reopen them, but omits them from
session saves. Loading or expanding/collapsing all packages replaces that set.
A new tile takes the next free `untitled-N` name, skipping names open in
another tile.

The application uses `MemorySheetStore`, shared by its pricer tiles. Accepted
saves survive closing and reopening a tile in the same process. They do not
survive a restart: session restoration retains the sheet name and UI state,
but a missing document opens empty with a notice. Durable storage is not
implemented.

Other known gaps: the underlying typeahead does not yet offer catalogue
underlyings; result cells are not sign-coloured; column widths are the
vocabulary's fixed pixel widths and cannot be resized.

In-process pricing remains an upstream leaf. A feature submits definitions
through the data-service request path and receives outcomes through shell
delivery; it does not call a pricing implementation directly.

## Demo and application composition

`geode-demo-data` generates deterministic risk batches and market-data
documents. `geode-app --demo` writes them under a seed-specific temporary
directory and uses the same ingestion, adapter, parsing, query, and delivery
paths as configured sources. Demo adapters provide subscribed documents and
fetchable series without pretending to be production vendor integrations.

`geode-app` is the composition root. It loads configuration, initializes GPUI
and logging, builds registries, creates the data service and bridge, registers
module factories, installs globals, and opens the window. Cross-layer policy
that depends on the assembled binary belongs here; feature behavior does not.

## Testing and performance

Feature state machines are tested without a window. GPUI tests host the real
tile for focus, key, pointer, popup, and delivery behavior. Render delegates
are tested through prepared models rather than pixel claims unavailable to a
headless context. Display checks remain necessary for exact color, geometry,
and animation.

Benchmarks cover blotter flattening and formatting, market-data model building,
chart preparation, series querying, document parsing, line-pricer core
operations, and the line-pricer grid build. Budgets and current gaps are
recorded in [performance.md](performance.md).
