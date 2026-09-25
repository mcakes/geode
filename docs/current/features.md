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
delivery or structural edit and patched for an ordinary cell commit.

Edits live in a `Draft` over a base generation. A newer delivery marks the
draft behind rather than silently rebasing it. Rebase resolves edits by row and
column labels, keeping stable intent across reordered documents. The module
supports numeric, date, text, and closed-choice cells, row insertion/deletion,
and kind-specific actions.

Document egress is not built. `:upload` and related actions report that
limitation rather than pretending to persist a draft upstream.

## Timeseries

`geode-timeseries` owns a chart tile composed from source series and arithmetic
expressions. Its pure model tracks slots, range, frequency, axis mode,
statistics, cursor, popups, and session state. Each mutation returns a
`Changed` bitset so the tile can distinguish fetch, query, chart, chrome, and
session work.

Source identities resolve through the configured fetch sources. Fetch requests
ask only for uncovered spans; completion is broadcast by `(identity, source)`
so every interested tile requeries, including when zero new rows were needed.
Series queries return aligned struct-of-arrays values, percentiles, bins, and
coverage. Expression slots may narrow the result to buckets shared by their
operands.

`geode-chart` is independent of series and shell concepts. Its pure core owns
scales, axes, layout, viewport, crosshair, hit-testing, decimation, and palette
derivation. `ChartElement` paints an immutable `ChartModel` through
gpui-component's plot surface. Paths and chrome are cached by the values that
affect them; cursor movement does not rebuild the data model.

Every verb has a pointer route beside its key, and both take the tile's one
`dispatch` path. The header's `⋯` button and a right-click on a chip open an
action menu (`.` from the keyboard) listing the openers, the cursor slot's
verbs, the frequency steps, the two toggles with their state, and the view
reset; a disabled row names its reason as the notice. A chip click selects
its slot, a click on its swatch shows or hides it, and the `range · freq`
readout opens the range popup, which also carries a frequency row whose chips
write at once and leave the popup open. Over the chart, a wheel zooms about
the pointer (the dominant axis wins, so a sideways wheel pans instead), a drag
on a plot pans, and a drag on the band between two panes moves the split. A
drag ends on release, on a release outside the window, or on the first move
that arrives with no button held. The empty tile offers the add and compose
verbs as buttons under its hint. A double-click is the shell's fullscreen
toggle and arms nothing here.

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
contains panics, supports cancellation between lines, and answers every line
with either values or an error.

`geode-pricer` is the line-pricer module: a pure core (a struct-of-arrays
sheet, edits and undo, shorthand parsing and rendering, package folding, column
planning, and document storage conversion) and the `pricer` tile registered in
the application roster.

### The tile

The tile shows one named sheet under a single dense header: the sheet name,
its view, any sheet-wide shift chips, `N pricing…` while lines are stale, the
configured pricer's name, and the last priced time, which reads `stale` once
it is older than the shell's `stale_after`. Lines and packages are rows of one
table; a package row sums its legs and opens and closes like a tree node
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
| `.` | The action menu: price all, group, ungroup, undo, redo, delete row, and one row per view |

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
commits the highlighted option. A commit whose line was deleted, or whose
column moved under a view change, is refused with a footer message. A click in
the grid, including a package chevron, cancels an open editor or entry field
and never commits it, and acts on the row it was painted on: the entry
placeholder is a row, so closing it moves the rows below up, but a click below
it still lands on (or toggles, or double-click edits) the row the trader
aimed at. A click or double-click on the placeholder itself only closes it. A
`:` command or a `/` search closes the menu and any open field first. A click
outside the grid leaves a text editor open until the next grid click or verb,
as in the market-data panel; the typeahead popup closes on an outside click.
Both the entry field and the cell editor blur before they drop, and no chord
is bound while one is open, so `ctrl+k` still opens the palette. Either field
puts the tile in insert mode, so bare and shifted letters and digits are typed
into it and never reach a shell binding (`shift+d` would otherwise duplicate
the tile).

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
for an older revision is ignored, and the line, still stale, is resubmitted. A
hidden tile cancels its in-flight work by key and submits nothing until shown,
keeping its stale marks.

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

A sheet is saved as a whole document one idle second after its last change.
Closing the tile saves any change not yet saved, whether it was still waiting
on the idle timer or was refused by the store. A refused save paints a notice
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

While a load is pending the header reads `loading…`; `escape` does not clear
it (it is the only sign the load has not answered), and the answer does.

The session record keeps the sheet name, view, refresh setting, cursor line,
and open packages (only those still on the sheet: a deleted or ungrouped
package's id stays in the tile's open set until a load or `z shift+r` /
`z shift+m` replaces the set, so an undo reinstates it open, but it is never
saved). A new tile takes the next free `untitled-N` name, and names open in
another tile are skipped.

**Known limitation:** the sheet store is in memory until the DuckDB store
lands. A sheet survives closing and reopening its tile within one run, not a
restart; a restored name with no document opens empty with a notice.

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
