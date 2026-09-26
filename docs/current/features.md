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

`[ui] line_numbers` numbers the grid's rows as the blotter numbers its own:
a gutter in the pinned column (the row label, or the first value column
when the label is hidden), which widens by the gutter's width. The gutter
sits beside that cell rather than inside it, so the cursor border, a draft
state's fill and a deleted row's strike stay on the data. Rows count as
painted, inserted and deleted rows included. In `rel` mode with the cursor
in the header strip there is no row to measure from, and the gutter
numbers absolutely.

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
why, for a clean, `Behind`, already-sent or incomplete draft, for a missing
or ineligible target, while another upload from the panel is in flight (`an
upload of <key> to <target> is in flight`), and while the panel is not live
(`upload: the panel shows <time>, not live`): the frame's as-of is
historical, or the generation on screen was delivered for a historical
request. The second case covers the frame gone live before its live
generation is painted — behind the barrier, or indefinitely when the live
requery is refused or fails and the last good generation stays on screen.
An upload is a whole document, and one assembled over an old generation
would revert every untouched row upstream. `y` re-checks the same
condition and sends nothing, naming why, if the panel is no longer live.
Otherwise it assembles the document and asks
`upload N cells, [K attributes, ]A rows added, D removed of <key> to
<target>? (y/n)` in the
header, where the question holds the keyboard: bare `y` submits, and any
other key (consumed, chords included), a pointer press on the tile, or focus
leaving the question cancels with `upload cancelled`. A delivery that
changes the draft or the painted generation while the question stands (a
rebase, a replace, or `Behind`) withdraws it at once with `upload cancelled:
a new document arrived`, since the assembled rows belong to the superseded
document. An `Ok` outcome marks the draft `sent HH:MM` only if the draft,
base included, is still what was submitted; an `Err` keeps it editing and shows
`upload failed: <e>` until the next edit or upload.

Upload state belongs to the underlying that submitted it. Switching
underlying while a draft is sent or its upload is in flight gives up that
draft's echo check: it parks, and restores, as an unsent `Editing` draft,
exactly as a session restore does, and the trader confirms upstream by eye or
uploads again. An outcome that arrives for an underlying no longer shown is a
notice naming it (`upload of <key> to <target> sent` or `... failed: <e>`) and
never touches the draft on screen.

While a draft is `sent`, the next generation delivered for its key with a
different source time is compared with the rows that went out as a multiset
(both sides sorted by every compared column, since the store returns a
document sorted by its axes while the rows went out in painted order), over
every column except a minted row label (`f64` within one ULP). Equal clears the draft, the panel follows the new generation, and the
header reads `sent HH:MM, confirmed HH:MM` until the next edit. Different
keeps the draft `sent` over its base, reads `echo differs (N rows)`, and
refuses edits until `:rebase` (which yields an unsent draft on the new
generation) or `:revert`. The update policy does not apply to a sent draft. An
upstream that only reorders rows reads as confirmed. A delivered document that
cannot be assembled reads `echo not comparable: <why>` and is held the same
way. `:rebase` is refused while a sent draft awaits its echo with no
difference held (it would re-arm an upload of edits already in flight). `sent`
is not persisted: a sent draft restores with its edits, unsent.

**Known limitations.** An upload is a whole document with no concurrency
check: the last writer wins, so an upstream generation that lands between the
panel's last delivery and the trader's `y` is overwritten. An echo that
arrives before the transport's `Ok` is handled as an ordinary delivery of a
draft still `Editing` (the update policy applies, as to any newer generation),
and the late `Ok` then does not enter `Sent`; the demo bus answers `Ok` first,
but a real transport may not.

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
verbs, `Frequency…`, the two toggles with their state, and the view reset; a
disabled row names its reason as the notice. `j`/`k` step over disabled rows
as they do over separators and section headings (in every tile's action
menu); only the pointer rests on a disabled row. A chip click selects its
slot, and a click on its swatch shows or hides it.

The header shows the range and the frequency as two triggers, `1y ▾` and
`1d ▾` (an absolute range shows its dates, `2025-09-26 – 2026-09-26 ▾`). Each
opens its own menu under it, and stays filled while that menu is up; a second
click closes it. `r` and the range trigger open the range menu: the seven
presets written out with their short labels, then `Custom dates…` (`c`). `f`
and the frequency trigger open the frequency menu: the six frequencies with
their short labels. Both menus tick the value in force and open with the
highlight on it (on `Custom dates…` while the range is absolute). `j`/`k`
move, `enter` or a click applies and closes, and `escape` closes; a second `r`
or `f` closes its own menu. A frequency the 500,000-point cap refuses over the
current range is a disabled row carrying the cap's reason, decided when the
menu opens; picking it leaves the reason as the notice. A preset the cap
refuses at the current frequency is refused the same way when picked, and
the menu stays open. `:range` and `:freq` remain the typed routes; there is
no key that steps the frequency.

`Custom dates…` opens a two-field date editor under the range trigger. It
opens on `from`'s day segment, and a digit types into the date at once.
`tab` switches fields, `enter` applies both dates (a backwards range or an
unfinished segment is refused inline, and the editor stays open), and
`escape` returns to the range menu with the highlight on `Custom dates…`. A
second `escape` closes the menu. The editor holds the keyboard, so the tile
reports insert mode while it is open. Over the chart, a wheel zooms about
the pointer (the dominant axis wins, so a sideways wheel pans instead), a drag
on a plot pans, and a drag on the band between two panes moves the split. A
drag ends on release, on a release anywhere off the chart, or on the first
move that arrives with no button held; a modified press or a double-click's
second press arms nothing, because those are the shell's tile gestures. The
empty tile offers the add and compose verbs as buttons under its hint. A
right press focuses a tile exactly as a left one does, so a module's context
menu always opens in the tile whose keys it will answer to.

A slot's colour is a palette index (`1`–`5`), a `[colours]` name, or an
absolute `#rrggbb`. The first two follow the theme and get its readability
floor. An absolute colour is painted exactly as chosen: it ignores the theme
and gets no contrast adjustment, so it can be hard to see on a theme it was
not chosen against. The session stores it as lowercase `#rrggbb`; `[colours]`
names cannot start with `#`, so this is never read as a name, and a malformed
value keeps the slot's default colour. `c` cycles the palette and moves an
absolute or named colour back to palette colour 1.
`:colour s<n> <1..5|name|#rrggbb>` sets any of the three and refuses a
malformed hex by naming the form. The action menu's `Colour…` row (also in
the chip's right-click menu) opens gpui-component's colour picker on the
cursor slot's chip, where its swatch button stands in for the chip's swatch.
The row has no key of its own; `:colour` is the keyboard route. The picker's
featured row shows the five palette colours and then every `[colours]` name,
each resolved when the picker opens. A pick within one 8-bit step per channel
of a featured colour is read as that colour (the nearest, and the first on a
tie) and keeps following the theme. The tolerance exists because the
component's hex field truncates each channel. This means a palette-grid
swatch that happens to match a featured colour, or a `[colours]` name that
resolves to a palette entry's colour, is read as the featured entry. A pick
within a step of the colour the slot already paints changes nothing, so
Enter on the untouched hex field keeps a palette or named colour. Every other
choice (the palette grid, the HSLA sliders, or a hex typed and entered)
becomes absolute, with alpha dropped. Swatch and hex choices commit and close
the picker. Slider steps commit live and leave it open. Escape or a click
outside closes the picker; a slider change already applied stays. The picker
always writes to the slot it was opened on, even if the cursor moves
meanwhile. Removing that slot closes the picker. While the picker holds the
keyboard, the tile reports insert mode, so typing in the hex field never
reaches the tile's single-key commands. Each slider step rebuilds the chart
model and clears its path cache. That is inside the frame budget at daily and
hourly sizes but not at the 500,000-point cap (see the measurement log).

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
type`, `priced at`), and the default widths fit each label and a worst-case
value (`-1,234,567.8900` for a greek, `-1,234,567.89` for a price), inside the
cursor cell's border, at the largest font size; a view's `label` and `width`
still override them. Both bundled views end in a `status` column, which says
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
| `.` | The action menu: `Reprice all lines`; `Group into package` / `Ungroup package`; `Undo` / `Redo`; `Delete row` on its own; then a `View` section with a tick on the current view. Each row names its default key, or on a disabled row the reason; the highlight follows the pointer, and `j`/`k` skip separators, section headers and disabled rows; a disabled row takes no fill, and `enter` or a click there (reached by the pointer) names the reason. Key hints are the default bindings; a rebind is not reflected |

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
commits the highlighted option. An open editor follows its column, and the
cursor with it, through a view change (a config reload or `:view`) that moves
it. When the column leaves the view, or the line leaves the grid, the editor
closes with `the cell moved; edit refused` in the footer and nothing is
committed; that close has no window of its own, so the field is blurred at the
end of the same update, before the next frame. A click in the grid, including a
package chevron, cancels an open editor or entry field and never commits it,
and acts on the row it was painted on: the entry placeholder is a row, so
closing it moves the rows below up, but a click below it still lands on (or
toggles, or double-click edits) the row the trader aimed at. A click or
double-click on the placeholder itself only closes it. A `:` command or a `/`
search closes the menu and any open field first. An open menu re-checks its
rows whenever the tile changes under it (a load answer, a config reload),
keeping its highlight where it was. A click outside the grid leaves a text
editor open until the next grid click or verb, as in the market-data panel; the
typeahead popup closes on an outside click. Both the entry field and the cell
editor are blurred when they close, and no chord is bound while one is open, so
`ctrl+k` still opens the palette. Either field puts the tile in insert mode, so
bare and shifted letters and digits are typed into it and never reach a shell
binding (`shift+d` would otherwise duplicate the tile).

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
