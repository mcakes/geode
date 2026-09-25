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

A dividend row's label is Geode's own minted id (the ex date, or
`<date>#n` for the `n`th row sharing that date), not a wire id — same-date
rows are identified by their ordinal among that date's group. Rebase
therefore refuses to carry any cell edit or deleted mark whose label
belongs to a same-date group whose row count changed between the draft's
base and the newer document, reporting it through the same dropped-edit
notice as any other unresolved label, with the reason `row (same-day rows
changed: <was> → <now>)`: a changed count means the ordinals shifted, and
the label might now resolve onto a different dividend. Group sizes are
captured from the painted model whenever it is the draft's own base
(before a rebase, before an update-policy rebase, and before the session
is written); a draft restored from a session written before this guard
existed applies none. **Known limitation:** a pure reorder of two
same-day dividends upstream, with the group's size unchanged, swaps their
minted ids undetectably, so a rebased edit can land on the other row of
the pair — closing this gap needs an upstream row key, which the desk's
XSD may supply.

`:upload [target]` (also the action list's `Upload` row) sends the draft to
an egress target that accepts the panel's document. It is refused, naming
why, for a clean, `Behind`, already-sent or incomplete draft, and for a
missing or ineligible target. Otherwise it assembles the document and asks
`upload N cells, A rows added, D removed of <key> to <target>? (y/n)` in the
header, where the question holds the keyboard: bare `y` submits, and any
other key (consumed, chords included), a pointer press on the tile, or focus
leaving the question cancels with `upload cancelled`. A delivery that
changes the draft or the painted generation while the question stands (a
rebase, a replace, or `Behind`) withdraws it at once with `upload cancelled:
a new document arrived`, since the assembled rows belong to the superseded
document. An `Ok` outcome marks the draft `sent HH:MM` only if the draft,
base included, is still what was submitted; an `Err` keeps it editing and shows
`upload failed: <e>` until the next edit or upload.

While a draft is `sent`, the next generation delivered for its key with a
different source time is compared with the rows that went out, row by row in
document order, over every column except a minted row label (`f64` within one
ULP). Equal clears the draft, the panel follows the new generation, and the
header reads `sent HH:MM, confirmed HH:MM` until the next edit. Different
keeps the draft `sent` over its base, reads `echo differs (N rows)`, and
refuses edits until `:rebase` (which yields an unsent draft on the new
generation) or `:revert`. The update policy does not apply to a sent draft. An
upstream that reorders rows reads as differing. A delivered document that
cannot be assembled reads `echo not comparable: <why>` and is held the same
way. `sent` is not persisted: a sent draft restores with its edits, unsent.

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
scales, axes, layout, viewport, crosshair, decimation, and palette derivation.
`ChartElement` paints an immutable `ChartModel` through gpui-component's plot
surface. Paths and chrome are cached by the values that affect them; cursor
movement does not rebuild the data model.

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

`geode-pricer` currently contains the pure line-pricer core: a struct-of-arrays
sheet, edits and undo, shorthand parsing/rendering, package folding, column
planning, and document storage conversion. It has no tile and is not registered
in the application roster. UI hosting and persistent workflow remain unbuilt.

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
chart preparation, series querying, document parsing, and line-pricer core
operations. Budgets and current gaps are recorded in
[performance.md](performance.md).
