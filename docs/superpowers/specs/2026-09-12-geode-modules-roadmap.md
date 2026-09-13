# Geode — Modules Roadmap

**Date:** 2026-09-12
**Status:** Rulings recorded; slice 1 specified separately
**Governs:** the decomposition of the module roster sketched in
`docs/modules.md` into sub-projects, the order they are built in, and
the architectural rulings made while cutting them. Each slice gets its
own design spec that must conform to this document, the foundation
design and `docs/PHILOSOPHY.md`.

## 1. What the sketch asks for

`docs/modules.md` names roughly eighteen modules: the blotter (built),
a scenario panel of aggregated risk grids, a watchlist editor and vol
watchlist, five small market-data panels fed by XML documents over
Solace (CVI params, repo curve, dividend schedule, correlation and
skew, index compositions) four of which upload edited documents back
to Sophis, a line pricer and a package pricer, three vol and time
series viewers over KDB, OPRA and Bloomberg, an instrument viewer,
trade history, broker quotes and sales credit reports. Its interaction
section asks for the blotter's cursor to drive other tiles' scope, and
for launch actions carrying the underlying or position under the
cursor into a new tile.

Most of that rides rails that exist. Two gaps block whole families of
modules, three shell mechanisms are new, and one item is a charter
matter. The rest of this document is the decomposition that follows.

## 2. Rulings

Each of these was decided in the 2026-09-12 brainstorm and is binding
on every slice below.

1. **Solace documents arrive whole.** One message carries one complete
   object for one key (all of SPX's CVI parameters, say) and replaces
   the previous copy. There are no row-level deltas. The
   file-as-publication-unit model therefore carries over unchanged:
   message is file, key is batch.
2. **Cross-tile scope is named link groups.** A small fixed set of
   groups, each holding a scope with its own version. A tile emits into
   at most one group and follows one of: the global scope, a group, or
   its own pin. Rejected: follow-the-focused-tile (breaks the moment
   the followed panel is focused to navigate it) and explicit
   tile-to-tile (dies with the tile, needs a tile-naming scheme the
   shell lacks).
3. **Vendor systems are reached directly.** The app links the Solace
   client, speaks kdb IPC and calls Sophis itself; there is no desk
   gateway. Consequence: vendor adapters are feature-gated crates
   beside the data crate, CI never builds them, and a source naming an
   adapter the running build lacks is a failed source with that reason,
   never a compile or startup failure. The socket rule widens by one
   word: the *data tier* — the data crate and its adapter crates — is
   the only place a socket opens.
4. **Uploads are global but not execution-class.** An upload to Sophis
   changes the desk's official market data, and the user has ruled that
   this does not need the foundation design's execution safety layer
   (confirmation semantics, kill switch, audit log) designed first. An
   upload is an ordinary egress request with a confirm prompt and a log
   line.
5. **No real sources until the work machine.** Development is off the
   desk network: there is no Solace bus, no KDB, no Sophis, no Bloomberg
   or Reuters, and no sample of the real documents. Every adapter shape
   is built against a simulator that is a first-class adapter in the
   mould of `--demo`'s generated CSVs. The vendor client is a thin shim
   written later, blind, against a deliberately tiny trait.
6. **Small matrices are painted directly.** The CVI, repo, dividend,
   correlation and index-composition panels are a header block plus a
   few dozen to a few hundred cells. They are painted with plain gpui
   elements — no virtualised table, no shared table component, no
   blotter embedding. An earlier proposal to lift the blotter's table
   into a shared crate and build these panels on it was rejected as
   overcomplicating: the data is not aggregatable and the cell counts
   do not need virtualisation. The blotter stays exactly as it is.
7. **Two dataset families, side by side.** Measure datasets keep the
   grain vocabulary untouched. A new *document* family declares an
   identity key, axes and document-level attributes, and is served by a
   plain select with the module pivoting. Rejected: replacing the grain
   enum with a declared key model for everything (attribution rests on
   the grains forming a prefix chain, and reopening the compiler buys
   nothing any sketched module needs) and growing the enum with a
   custom variant (every match sprouts an arm faking a prefix chain).
   Axes join the *measure* family in slice 2, where bucket vega needs
   them.
8. **Parsing is code, not a config path map.** The CVI document's
   params align positionally with its nodes, which a path map cannot
   express. XSDs for every document will be available on the work
   machine; each document kind gets a typed parser and writer, hand-
   written now and XSD-generated later behind the same two functions.
   Config keeps only which topic carries which kind and which dataset
   it feeds.
9. **Market-data panels edit before upload.** The panel owns a draft
   over the received document, paints it as a diff, and a newer message
   arriving mid-edit never clobbers it.

## 3. The two load-bearing gaps

**Every dataset today must be keyed by the risk hierarchy.** The grain
enum (`geode_core::schema::Grain`) is closed, every key starts with
`book`, `lhu`, `position_ref`, and `validate_dataset` errors on a
dataset lacking them. A CVI set keyed by underlying and expiry, a repo
curve keyed by underlying and tenor, an index composition keyed by
index and constituent, a broker quote keyed by quote id: none can be
declared. Scenario datasets have a second version of the problem:
bucket vega is position-keyed but carries many rows per position, one
per tenor and strike, which the §3.5 conflict detector would read as
disagreement. Ruling 7 answers the first; slice 2 answers the second.

**One source adapter, and it polls a directory.** The sketch names
Solace, KDB, REST, WebSocket, Bloomberg, Reuters, Mongo and Nemo files.
These are four shapes, not eight: polled files (exists), subscribed
documents (arrive whole, replace by key), on-demand fetch (a tile asks
for a slice; nothing arrives unasked), and local (user-authored rows
the app itself writes, for the pricer). Slice 1 builds the second;
slice 3 the third; slice 4 the fourth.

## 4. New shell mechanisms

- **Link groups** (ruling 2) in `Frame`: emit and follow per tile,
  serialised with the tile, the flip barrier extended to group
  versions unchanged. Slice 2.
- **Launch with context.** `ShellView::open_module` plus an argument
  table handed to the factory as restored state (the `pending_tiles`
  map can already carry it). `ModuleFactory` declares which context
  kinds a module accepts; `TileContent` answers what is under its
  cursor; the palette offers "Open CVI for SPX.Z" rows dynamically.
  Context kinds are shared dimension names. Slice 2.
- **Module-shipped keymap fragments.** `geode_shell::defaults` carries
  a reserved copy of every module's action ids so the built-in keymap
  can bind them before the roster registers. That will not scale to
  fifteen modules; a module ships its own default bindings through
  its factory, merged as a layer beneath the user's. Slice 1, since it
  is the third module.
- **Egress** (ruling 4): an upload request through the data tier with
  an outcome delivered to the tile. Slice 1.

## 5. Sub-projects

- **A. The document family and shared dimensions.** Ruling 7. Shared
  dimensions are by name and role only: a scope selection on
  `underlying_ref` applies to every dataset declaring a dimension of
  that name; a dataset lacking one is marked not applicable rather
  than erroring. Built in slice 1 (document family) and slice 2 (axes
  on measures).
- **B. The adapter tier.** Rulings 3 and 5. Subscribed documents in
  slice 1; fetch in slice 3; local in slice 4.
- **C. Link groups and launch context.** Ruling 2. Slice 2.
- **D. The market-data family.** Rulings 6, 8, 9. One crate, one panel
  parameterised by a spec, one roster kind per document kind. CVI in
  slice 1; repo, dividends, correlation and index compositions follow
  as config and a generator each.
- **E. Scenario panel.** Axes on measure datasets, axis attribution
  (an axis is never summed across), the pivot grid, T×K and T×S. Needs
  A and C. Slice 2.
- **F. Fetch and charts.** The fetch adapter shape with KDB first,
  cache datasets with their own retention, a chart crate on the
  2026-08-29 spike's routing rule (paths for lines, images for
  surfaces), the slice and term-structure viewers, then the time-series
  viewer. Needs B. Slice 3.
- **G. Pricer.** A local dataset of typed lines, an upstream price
  request whose response is ingested, scenario output. The charter
  bites here: pricing is upstream, the app only sends and shows. Needs
  A, B, C, E. Slice 4.
- **H. Watchlists.** A named list of underlyings is a saved scope with
  one dimension selection; the scopes dialog exists and the TODO's
  "duplicate and edit" is the missing piece. Any gap.

Trade history, broker quotes and sales credit are long row lists rather
than small matrices. Their shape is left undecided until one of them is
next up; a general row query over a view (scope, sort, limit, no
aggregation) is the likely data-tier piece and is deliberately not
built in slice 1.

## 6. Order

Vertical slices, each dominated by the platform piece it exercises, so
the piece is shaped by a module that needs it — the way the blotter
shaped the frame and the diagnostics tile shaped the health seam.
A is designed whole (one spec per half, but the two halves must not
disagree) and built in two parts, each with the slice that needs it.

1. **CVI Params, end to end** —
   `docs/superpowers/specs/2026-09-12-geode-market-data-documents-design.md`.
   Parts 1 (the document family, storage and publication) and 2 (the
   adapter tier, the coalescer, the subscribed-source pipeline and the
   demo bus) are done, 2026-09-13; Parts 3 (the panel) and 4 (egress)
   remain.
2. **Scenario panel** — axes on measures, link groups, launch context,
   the pivot grid with its cell-count measurement.
3. **Vol viewers** — fetch, cache datasets, charts.
4. **Pricer.**
5. **Watchlists** whenever there is a gap.

## 7. Not decided here

- The daemon split the foundation design defers (§2). Direct Solace
  sessions per app instance are acceptable for one trader; revisit when
  a second instance or a second window needs the same subscription.
- The real topic structure on the desk's bus, the Sophis upload API,
  and whether documents carry their own timestamps — all wait for the
  XSDs and the work machine (ruling 5).
- Percentiles on the term-structure viewer: a percentile rank of the
  current value against ingested history is an aggregate, and so
  view-shaping under the charter, but the ruling is deferred to slice 3.
