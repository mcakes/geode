# Geode Phase 4 — Frame Features, Diagnostics and Config Editor Design

Phase 4 puts a face on the frame state Phase 3 built and ships the two
remaining day-one modules. It builds the scope bar with dimension pickers
and a live text filter, the as-of selector with an unmissable historical
indicator, the tile-local filter layer, the atomic flip across tiles, a
diagnostics module on a new `tracing` foundation, and a config editor
that saves through the hot-reload path. It also consumes the rest of
Phase 3's "explicitly not in Phase 3" list: column personalisation
persisted to the view and the "reload data now?" prompt.

This document is subordinate to
`2026-08-28-geode-foundation-design.md` (referenced below as "the
foundation spec", with bare `§n` references pointing into it), to
`2026-08-30-geode-phase-2-data-design.md` ("the Phase 2 spec", `P2 §n`),
to `2026-09-03-geode-phase-3-blotter-design.md` ("the Phase 3 spec",
`P3 §n`), and to `docs/PHILOSOPHY.md`. Where it contradicts any of them
it says so in §2 — those are deliberate amendments made in writing, per
the charter's own rule.

Phase 4 is one spec and three implementation plans, sequenced 4a → 4b →
4c (§10). Each plan is its own branch, reviewed and merged on its own.

## 1. Scope

### 1.1 What Phase 4 delivers

**4a — Frame features** (`geode-shell`, `geode-data`, `geode-core`,
`geode-blotter`):

- The scope bar in the toolbar's middle: slot readout, one chip per
  dimension selection, text and expression chips, a live text field, and
  the as-of marker. Rebuilt only when `Frame::versions()` changes.
- Carried dimensions: a `dimension` may declare the grain whose key
  determines it (`currency` at `instrument`), becoming groupable,
  scopeable and pickable at that grain and finer without joining a
  key. The split checks the dependency per file and reports a
  violation as degraded health.
- A `categorical` flag on schema columns — the one rule that decides
  which columns are ENUM-interned at ingest, which get a picker, and
  which the text rewrite applies to.
- Dimension pickers: `frame::pick` (choose a categorical column, then
  its values) and one `frame::pick_<column>` action per categorical
  column known at startup, fed by a new `Request::Distinct` on
  `DataHandle`.
- A text filter that requeries on every keystroke, made affordable by
  compiling the pattern over ENUM dictionaries rather than rows, with
  the bench as the gate.
- Load-time validation that `textual = true` is only declared on a
  column the scope compiler can route.
- The as-of selector: a modal taking `HH:MM`, `HH:MM:SS`, RFC 3339 or
  `live`, with recent generation times as presets; a window-wide warning
  stripe and a status-bar segment while historical; `:asof undo`.
- `:filter` — the tile layer of §4.2, set per tile, shown in the tile
  header, serialised with the tile.
- Scope undo and redo as bounded stacks, replacing the one-level
  `previous_scope`.
- Saved scopes: `scopes.toml` read from every layer, `:scope save
  <name>` writing the user layer, recall from the palette.
- The atomic flip of §4.3: a barrier in the frame so every following tile
  swaps to a new scope, grouping or as-of in one notify pass.

**4b — Diagnostics** (`geode-core`, `geode-data`, `geode-shell`,
`geode-app`, new crate `geode-diagnostics`):

- `tracing` as the workspace's logging vocabulary; every `eprintln!`
  migrates. Three layers installed by `geode-app`: stderr, an in-memory
  ring buffer in `geode-core`, and daily-rotated files. Per-subsystem
  levels from `[log]` in the `app` doc, reloadable live.
- A shell-owned `Diagnostics` entity fed by the app bridge: source
  health with history, generations, config diagnostics with provenance,
  dropped-event count, frame and requery histograms.
- `Request::Catalog` on `DataHandle`: per-dataset generations, rows
  (live/archive table estimates), and freshness against an as-of.
  Database-wide bytes (as built: `database_bytes`/`memory_bytes` on the
  perf section, not per-dataset — MIN-4, final review round 2).
- The diagnostics module: five sections (sources, data, config, log,
  perf), each a keyboard-navigable list, including the effective-config
  explainer of §8.
- Panic boundaries: one file's ingest under `catch_unwind` marking the
  source degraded; a panic hook that writes the ring tail to a crash
  file before a render-thread panic exits.
- A status-bar summary of source states and config errors.

**4c — Config editor** (`geode-core`, `geode-shell`, `geode-blotter`,
`geode-data`, `geode-app`, new crate `geode-config-editor`):

- An editor tile over gpui-component's `EditorState` with TOML
  highlighting and line numbers; `config::edit` and `:edit <doc>` open a
  doc-and-layer picker; built-in docs open read-only with `:copy user`.
- Validation on every edit: `toml_edit` syntax errors at line and
  column, then the doc's own `from_doc` reader, surfaced through the
  editor's `DiagnosticSet`. `Diagnostic` gains an optional key path so
  reader errors land on the offending key.
- `ctrl+s` saves atomically through one shell-owned config write door,
  and the existing watcher applies the reload with last-good semantics;
  the outcome shows in the tile.
- Column personalisation: widths, order and hidden columns from the
  blotter, persisted to a user-layer `view_presentation.toml` merged
  over the view after the named-object merge.
- The "reload data now?" prompt for a `sources` or `datasets` change,
  backed by a swappable `DataHandle` and a `DataService` restart on the
  background executor.

### 1.2 Done state

Phase 4 is done when `geode --demo` opens and: typing in the scope bar
narrows every blotter as you type, one requery per keystroke, with the
1M-row zero-match bench case inside the §7.1 budget for ENUM columns;
`mod+p` then `book` then two selections flips every visible tile to the
same scope in the same paint; `mod+p` then `currency` lists the demo's
currencies with counts, and `:group currency,underlying_ref` groups by
a carried dimension with position-grain cells blank at the currency
level; `:asof 14:05` paints the warning stripe
and the status segment and every tile reads historical, `:live` clears
both and `:asof undo` restores them; `:filter model_code = 'EURP'`
narrows one tile, marks it `filtered`, and survives a restart; `:scope
save mine` then `scope: mine` in the palette recalls it; a diagnostics
tile shows the demo source healthy, its generations with sizes, the log
tail, `:level ingest debug` taking effect on the next poll, and the
effective config with every leaf's layer; the config editor opens
`views` in the user layer, squiggles a column name no dataset has,
saves a good edit, and the blotter reflects it without restart; editing
`sources` prompts and reloading data works without restart; a dragged
column width survives a restart; no `eprintln!` remains outside tests;
and every behaviour above has a mutation entry.

### 1.3 Explicitly not in Phase 4

- The workspace scope layer (§4.2). Global and tile only. No workspace
  has a user story for differing from the app yet, and a third layer
  needs its own bar segment, session persistence and `:scope workspace`
  form.
- Vim-modal editing inside the config editor (§8). Insert mode only;
  `escape` leaves the buffer.
- Multi-window (§3.6), scenario datasets (P2 §3.7), the sidecar split,
  and as-of diffing (§4.5).
- Re-registering `frame::pick_<column>` actions on a live `datasets`
  reload. A `datasets` change is already restart-required (P3 §4.5);
  the pick actions follow the same rule.
- Re-registering `scope::<name>` actions (§3.11) on a live `scopes`
  reload, for the same reason: a scope saved or edited after startup is
  reachable from the palette and `:scope load <name>` right away, but
  gets no `scope::<name>` action — and so no keymap binding — until
  restart.
- Any change to how tiles are created or split. `docs/modules.md` is a
  draft roster for later phases and does not bear on this one.
- A picker doc. Every categorical column is pickable; the only
  configuration is the per-column `categorical` flag in the dataset
  schema (§3.3), which the column's storage encoding already needs.

## 2. Amendments to earlier designs

Each is a deliberate change to a spec sentence, recorded here so the
older document does not have to be re-read with a correction in mind.

1. **§4.1 and §8, "dimension-picker definitions" as config.** Struck.
   Pickers derive from the schema: every column any dataset declares
   `categorical` (§3.3) — a column whose vocabulary is small enough to
   be an ENUM, whatever its role — plus every derived dimension, gets a
   picker. A per-column key binding is the keymap's job (`mod+b =
   "frame::pick_book"`), not a picker doc's.
2. **§4.3, the text field.** The foundation spec is silent on when the
   text filter fires. It fires on every keystroke (§3.2). The pool's
   coalescing is the backstop; the ENUM rewrite (§3.5) is what makes it
   affordable.
3. **§8, "named objects override whole-object by name".** Gains one
   exception: `view_presentation.toml` (§5.6) is merged *over* a view
   after the named-object merge, so a personal column width does not
   copy the whole view into the user layer and cut it off from desk
   changes.
4. **P3 §4.1, `previous_scope: Option<Scope>`.** Becomes two bounded
   stacks (§3.8). P3 §10.3 asked the scope bar to decide; it decided.
5. **P3 §4.4, the readout.** Replaced by the scope bar (§3.1). The order
   — slot, chips, text field, as-of — resolves the 3b review's M7
   (readout order) in favour of as-of last, as §4.4 lists it, with the
   warning background across the whole bar kept.
6. **P3 §1.3, "explicitly not in Phase 3".** Consumed, except
   multi-window, scenario datasets and the sidecar split (§1.3 above).
7. **§10.1 and P3 §5.1, ingest failures to stderr.** Ingest and query
   errors become `tracing` events (§4.1). The status-bar label stays.
8. **P3 §4.5, "sources changed — restart to apply".** The diagnostic
   stays, and gains a prompt (§5.7) that performs the restart in-process.
9. **Scope validation of `textual`.** Not in any spec, found by
   measurement (§7): a text filter over a textual column no grain
   carries as a dimension fails the whole statement. `textual = true` on
   such a column is a load-time error for that column (§3.5).
10. **P2 §3.6, "dimension columns are stored as DuckDB ENUM types".**
    Becomes "categorical columns are". Every `Dimension` is categorical
    by default, so nothing already ingested changes encoding; an
    `Attribute` can opt in and a high-cardinality `Dimension` can opt
    out.
11. **P2 §3.1–3.3, the grain vocabulary.** A `dimension` is no longer
    only a column of a grain's built-in key. It may declare the grain
    whose key determines it and is then *carried* by that grain and
    every finer one (§3.3): groupable, scopeable and pickable there,
    stored as a payload column, never added to a key. The built-in keys
    and the pair canonicalisation are unchanged.

## 3. Phase 4a — the frame's face

### 3.1 The scope bar

The toolbar's middle, which P3 §4.4 gave to the readout, becomes the
scope bar. Left to right:

- **Slot.** As today: `2 · underlying_ref / book / position_ref`, or
  `view default`.
- **Chips.** One per `DimensionSelection` with values: `book ∈ BK001,
  BK002`, collapsing to `book ∈ {7}` past two values. A `text "spx"`
  chip and an `expr` chip (the expression's source text, elided past 40
  characters) when set. A contradicted scope (`Scope::impossible`)
  paints one chip `∅ book` in the error token, naming the dimension
  `Scope::columns()` reports, so a scope selecting nothing never reads
  as "no scope".
- **The text field.** The existing `filter_input`, made live (§3.2).
- **As-of.** `AS OF 14:05` in the warning token, with the warning
  background across the whole bar, as P3 §4.4 already paints.

Every chip is clickable: a dimension chip opens its picker, the text and
expr chips focus the field or the command line with the value loaded,
and a chip's close glyph drops it. Every mouse route has a keyboard
twin: the picker, `:scope drop <dimension>`, `:scope text` (empty
clears), `:scope clear`.

The bar is built by a pure function `scopebar::layout(&Frame) ->
Rc<ScopeBarModel>` cached on `Frame::versions()` exactly as
`Frame::readout` is today, and `readout` is deleted. `render` reads the
cache; a cache hit is a refcount bump.

### 3.2 The text field

Typing sets `Frame.scope.text` on every keystroke. Each change bumps the
scope version and every following tile requeries; the pool coalesces
per key, so a burst of keystrokes costs one query in flight plus one
queued, never a backlog. `enter` blurs the field back to the shell with
the value kept; `escape` restores the value the field had when it was
focused and blurs. `mod+/` focuses it (§4.3).

The field is an `Input`, so while it is focused the input owns the keys
and the shell's chords are unavailable; that is the existing focus
model, and `escape` is the way out, same as the command line.

An empty field clears the text filter. Whitespace-only is empty.

### 3.3 Categorical columns and the pickers

**The flag.** `ColumnSpec` gains `categorical: bool`, read from the
dataset doc:

```toml
[risk_snapshot.columns.currency]
type = "utf8"
role = "attribute"
grain = "position"
categorical = true          # small vocabulary: ENUM-stored, pickable
```

Defaults: `true` for `Dimension`, `false` for `Key`, `Measure` and
`Attribute`. Only a `utf8` column may be categorical; declaring it on
any other type is a load-time error for that column. The flag means
"this column's vocabulary is small enough to be an ENUM", and one
function, `DatasetSpec::categorical_columns()`, is read by the three
things that care: the store's ENUM interning at ingest (replacing
`ddl::dimension_columns`), the picker roster, and the text rewrite
(§3.5). A picker and an ENUM are the same judgement about a column, so
they come from the same flag; nothing can be pickable without being
ENUM-stored or the other way round.

A `Dimension` with a large vocabulary — a per-trade reference someone
chose to group by — can declare `categorical = false`: it stays
groupable and scopeable through the expression filter, loses its
picker, and is stored as `VARCHAR`. The compiler's dictionary-code
paths (`dict_value`, the ENUM `try_cast`) already tolerate a plain
string column because as-of tables are plain strings in every era.

**Carried dimensions.** Today a `dimension` must be one of the columns
in a grain's hardcoded key (`book`, `lhu`, `position_ref`,
`counterparty`, `instrument_ref`, `underlying_ref`, `underlying2_ref`);
nothing else can be grouped by, and a selection on anything else fails
with "not carried as a dimension by any grain". That leaves
`currency`, `model_code` and `expiry` — one value per instrument, and
things a trader groups by — stuck as attributes that can be scoped
through the expression filter but never grouped or picked.

A dimension may now name the grain whose key determines it:

```toml
[risk_snapshot.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"     # exactly one currency per instrument_ref
```

This is a *carried* dimension. It is not part of any key: the key stays
the minimal row identity, and adding a functionally dependent column
to it would change nothing about the rows while misdescribing what
identifies them. Instead it is carried by its grain and by every finer
grain, meaning every grain whose key includes the declaring grain's
key (`instrument` → `underlying` → `underlying_pair`; not `position`,
whose rows span instruments). `ColumnRole::Dimension` gains an
`Option<Grain>`; `None` is a built-in key column and anything else
there is a load-time error, which also turns the bench's
`underlying2_ref` failure (§7) into a diagnostic.

One function, `DatasetSpec::carried_at(grain) -> Vec<&ColumnSpec>`,
returns a grain's dimension key columns plus every carried dimension
whose grain the key includes, and the four consumers read it:

- **The split.** A carried dimension is a payload column of its own
  grain's table and of every finer grain's, selected as `any_value`
  over the key group rather than added to the `GROUP BY`. Because the
  dependency is a claim the schema makes, the split checks it per file
  — one `HAVING count(distinct currency) > 1` over the key — and a
  violation is a `Degraded` health reason naming the column and the
  count of offending keys. The row is still written with one of the
  values, so the load succeeds; the honesty principle says the file's
  disagreement with the schema is reported, not hidden.
- **Scope routing.** `evaluable_at` treats a carried dimension as
  evaluable at every grain that carries it, and `route` reaches it by
  membership from grains that do not, exactly as it reaches a key
  dimension today.
- **The tree compiler.** The grouping-column check ("must be a
  dimension key of some grain") becomes "must be carried by some
  grain", and attribution follows: a view grouped by `currency` shows
  its position-grain measures as `NonAttributable` at that level,
  since a position can span currencies — the existing rule for cross
  gamma at an underlying grouping, applied where the schema says it
  applies.
- **Interning.** Carried dimensions are categorical by default like
  every dimension, so they are ENUM-stored, pickable, and covered by
  the text rewrite.

In the demo schema `currency`, `model_code` and `expiry` become
carried dimensions at `instrument` grain; `business_date` stays an
attribute. The demo schema also declares `textual = true` on its string
columns, since none is textual today and the scope bar's text field
would otherwise do nothing in `--demo`.

**The pickers.** `frame::pick` opens a two-stage modal through
`open_shell_dialog`:

1. **Column.** A fuzzy list (`PaletteState` over `PaletteItem`s) of
   every pickable column: the union over `config.datasets` of
   categorical columns, plus every derived dimension, sorted by name,
   each labelled with its role and the datasets that carry it.
   `enter` moves to stage two; `escape` closes.
2. **Values.** The column's distinct values under the current era and
   the frame's scope *minus this dimension's own selection*, each with
   its row count, fuzzy-filtered by the same field. `tab` toggles the
   highlighted value (as built: `space` is a printable character the
   filter field must take), `ctrl+a` ticks every value the filter
   currently shows, `ctrl+x` clears the ticked set (as built: `ctrl+n`
   is already "down" on every list surface in the shell), `enter`
   applies, `escape` cancels. `ctrl+a` is reclaimed from "select all
   text" inside `GeodeModal` for this the same way `tab` already is
   (`init_reclaimed_keybindings`). The list opens with the current
   selection pre-ticked. Applying replaces the dimension's selection;
   applying an empty selection drops the chip.

The values list is a `VirtualList`: a real underlying dictionary runs to
thousands of rows.

**As built (2026-09-08, Phase 4b M14; corrected in Task 1 fix round 1,
MAJ-1):** the values list is a gpui `uniform_list` rather than
gpui-component's `VirtualList` — not because no such primitive exists
(it does: `gpui_component::{VirtualList, v_virtual_list}`), but because
every row here is the same height. `VirtualList`'s per-item sizing
buys nothing over rows that never vary, and `uniform_list` is the
primitive whose scroll handle the rest of the shell already uses:
`ShellView::picker_scroll` (a `gpui::UniformListScrollHandle`, distinct
from the plain `ScrollHandle` the other three dialogs use) is what
keeps keyboard navigation's `selected` index scrolled into view, since
`uniform_list` only tracks scroll-follow through its own handle type.

`frame::pick_<column>` opens stage two directly for that column. One is
registered per pickable column at startup, so the palette lists
`Pick: book`, `Pick: underlying_ref`, … and a keymap can bind any of
them. `mod+p` is bound to `frame::pick` by default; the per-column
actions ship unbound.

While the distinct query is in flight the list shows `loading…`; a
failed query shows the failure text in the list and nothing is applied.
The outcome carries a tag and the picker drops any outcome older than
its latest request, the same discipline tiles use for snapshots.

### 3.4 `Request::Distinct`

```rust
pub enum Request {
    Query(QueryParams),
    Distinct(DistinctParams),     // new
    Catalog(CatalogParams),       // 4b, §4.5
    Cancel { key: QueryKey },
    ReplaceViews { .. },
    Shutdown,
}

pub struct DistinctParams {
    pub key: QueryKey,
    pub tag: u64,
    pub column: String,
    /// The frame's scope with this column's own selection removed.
    pub scope: Scope,
    pub as_of: AsOf,
}

pub struct DistinctOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub column: String,
    /// Sorted by value; `Err` is the failure text.
    pub values: Result<Vec<(String, u64)>, String>,
}
```

`DataEvent::Distinct(DistinctOutcome)` is delivered on the sink like
every other event. The bridge routes it to `ShellView::deliver_distinct`
rather than to a tile.

The query runs on the pool like any other, under the same era routing:
for each dataset carrying the column, `SELECT col, count(*) FROM <table
for era> WHERE <scope predicate> GROUP BY 1`, unioned and re-summed
across datasets by value. The scope predicate is compiled by the
existing scope lowering for that dataset at the column's own grain, so
a text or expression filter in force narrows the counts too. The
`QueryKey` namespace is shared with tiles; the shell reserves one key
per open picker, and there is at most one open picker.

### 3.5 The text filter over ENUM dictionaries

Measured (§7): at 1M rows with eight textual columns, a text filter
costs a flat ~50 ms of scan before any result is built, because each
`ILIKE` decodes every row's ENUM back to text. The needle that matches
nothing costs the same as the one that matches half the rows.

The rewrite: for a textual column that is also categorical (§3.3), and
therefore ENUM-stored in the live era, the compiler evaluates the
pattern over the dictionary and emits an `IN` over the matching values
instead of an `ILIKE` over rows. A row's value is by
construction one of the dictionary entries, so the two predicates
select the same rows; the pattern (`%needle%`, case-insensitive, with
`like_pattern`'s escaping) is applied to hundreds of strings instead of
a million.

Mechanically, the term for an ENUM column becomes

```sql
"book" in (select v from unnest(enum_range(null::"book_enum")) t(v)
           where v ilike ? escape '\')
```

with the same bound parameter, so the compiled statement's shape stays
one prepared statement per view and parameter counts do not change.

**As built (2026-09-07), amended from the paragraph above.** The
archived table still carries plain `VARCHAR` (P2: as-of skips ENUM
interning on the column itself), but the rewrite is no longer gated to
the live era. It shipped that way first, on the reasoning that
`refresh_enum` (`crates/geode-data/src/store/ddl.rs`) built the type
from the live table's distinct values only, so an archived row could
hold a value absent from the dictionary and the `IN` would silently drop
it. That made every keystroke in the scope bar slow under an as-of era —
the pre-rewrite floor, once per measure grain — because as-of routes to
`archive union all live` (spec §6.5) and the gate sent every textual
column down the row-scan branch regardless of whether it was
categorical. The root cause was the gate, not the era: `refresh_enum`
now reads live **and** archive (its call site is the same one, in
`crates/geode-data/src/ingest/load.rs`'s interning loop, run right after
each publish — which is also when the outgoing generation moves to the
archive, spec §4.3 — so no row in either table can hold a value the type
lacks), and the compiler's gate in `scope_sql.rs` checks only whether
the type exists (`existing_enum_types`), not which era it is compiling
for. A categorical column's `ILIKE` takes the dictionary path in every
era; non-categorical columns — keys, and any attribute or dimension that
did not opt in — are plain strings in every era and keep the row scan.

The gate: the bench in §7 re-run after the rewrite, with the zero-match
case over ENUM columns only inside 50 ms at 1M rows, and the residual
cost of the plain-string columns recorded in `docs/perf.md`. If the
subquery form does not plan well, the fallback is to resolve the
dictionary in Rust once per generation and bind the literal list; the
plan carries both.

**As built (2026-09-07), amended again: the literal-list form
shipped.** A headless probe on the demo's own schema and `tree` view —
wider than the bench's fixture (seven categorical textual columns
across three measure grains, not four across two) — measured the
subquery form's own gate case (depth 2, needle matching nothing) at
105 ms live / 218 ms as-of with the demo's key columns textual, and
78 ms / 169 ms without: the subquery form's open question (§11.1) is
resolved against it. The cause is not row count — the needle matches
nothing — but planning cost: an OR of several correlated `IN (select …
enum_range …)` subqueries still has to be planned and probed by DuckDB
even when every one of them is empty, and that cost recurs on every
requery regardless of match count.

The fallback the paragraph above named is what shipped:
`compile_scope` (`crates/geode-data/src/query/scope_sql.rs`) now
resolves each categorical column's matches once at compile time —
the same `select v from unnest(enum_range(null::{ty})) t(v) where v
ilike ? escape '\'` above, but run synchronously on the compile
connection ahead of `submit`, not embedded in the query's own
predicate — and binds the matching values as one delimiter-joined
varchar split by `string_split` in SQL, the same shape a dimension
selection already uses (spec §6.2): the statement text, and therefore
the prepared plan, stays independent of match count. A column with no
matches drops its term entirely instead of compiling to an
always-false subquery; if every column drops (or the dataset declares
no textual columns at all), the whole filter collapses to the literal
`false` — the invariant this form has to hold by construction, since
there is no longer a subquery for DuckDB itself to prove empty: a text
filter that matches nothing must select nothing, never fall through to
contributing no clause and silently widening the scope to everything.
Non-categorical columns are unaffected — still a plain `ILIKE` row
scan in every era.

Measured on this crate's own bench fixture (`cargo bench -p geode-data
--bench query -- text_none_depth_2`, 1M rows, before = the subquery
form, after = the literal-list form; full table and the demo-schema
probe numbers in `docs/perf.md`, "Phase 4a: the text filter,
literal-list form"):

| Case | subquery form | literal-list form |
|---|---|---|
| `1000000_rows_text_none_depth_2` | 32.276 ms | **5.2226 ms** |
| `1000000_rows_text_none_depth_2_asof` | 80.798 ms | **15.448 ms** |

The gate holds with far more headroom than either prior form left:
5.2 ms live, 15.4 ms as-of, against the 50 ms contract.

**Validation.** The dataset reader rejects `textual = true` on a column
that the scope compiler cannot route as a dimension at any grain, with
a diagnostic naming the column and the reason; the check uses the same
routing function the compiler does, so the two cannot drift. The plan
places it wherever the other column-level checks already run. Today such a column
fails every text-filtered query on the dataset with a `Sql` error at
query time, which is the wrong place and the wrong severity.

### 3.6 The as-of selector

`frame::as_of` (`mod+t`) opens a modal with a text field and a list:

- The field accepts `HH:MM` and `HH:MM:SS` (today, local time, as
  `:asof` already does), RFC 3339, or `live`. A value that parses shows
  the resolved instant beside the field in local time with its zone
  abbreviation (as built: not UTC — one modal, one clock, the trader's);
  one that does not shows the error inline and `enter` does nothing.
- The list shows recent generation times across datasets, newest
  first, each as `14:05:12 · risk_snapshot / EOD · 3 books`. In 4a the
  frame keeps them itself: `note_published` gains the event's dataset,
  batch and instant and records the last 32 in a `VecDeque`; 4b moves
  that list into the `Diagnostics` entity (§4.4) and the modal reads it
  there. Selecting one sets as-of to that instant. These are honest presets: as-of resolves to
  "the newest generation at or before T", so the generation times are
  exactly the instants that change what is shown.

`frame::live` returns to live. Both directions remember the previous
as-of in `Frame.previous_as_of`, and `:asof undo` / `frame::as_of_undo`
swaps back. One level: an as-of change is one deliberate act, unlike a
scope that accretes.

**The indicator.** While historical: the scope bar's warning background
as today; a 3 px stripe in the warning token directly under the toolbar,
spanning the window; and a status-bar segment `AS OF 14:05 · :live to
return`. §4.5 says nothing on screen may look live when it is not; the
stripe is the part that survives a maximised tile hiding the bar's
detail.

**As built (2026-09-07, the as-of baseline fix):** the generation
predicate every as-of query applies is a `gen_id` range plus a tuple
semi-join, pushed into both sides of the era relation (archive and
live), not the per-generation OR chain this originally emitted. Both
sides are always read because `publish_file` is one transaction per
grain while the resolve and the query execution run on separate
connections, so a relation that trusted only the side the resolve saw
could silently miss a partition moved mid-publish. Numbers in
`docs/perf.md`'s "Phase 4a: the as-of baseline" section.

### 3.7 The tile layer: `:filter`

The blotter's `:` vocabulary gains

| Line | Effect |
|---|---|
| `:filter <expr>` | Set this tile's expression filter; parsed by `parse_expr`, validated against the tile's dataset; errors at the caret. |
| `:filter text <words>` | Set this tile's text filter; bare `:filter text` (no words) clears it, matching `:scope text`. |
| `:filter clear` | Clear both. |

The tile's `Scope` is the `tile` argument `Frame::effective_scope`
already accepts. A tile with a filter shows `filtered` in its header
strip beside `pinned` and `unscoped`, and the filter is serialised in
the tile's session record as `filter = { expr = "...", text = "..." }`.
An `unscoped` tile still applies its own filter; unscoped means
ignoring the frame, not ignoring itself.

### 3.8 Undo and redo

```rust
pub struct Frame {
    scope: Scope,
    scope_undo: Vec<Scope>,      // bounded, 32 deep
    scope_redo: Vec<Scope>,
    previous_as_of: Option<AsOf>,
    ..
}
```

Every scope mutation pushes the outgoing scope on `scope_undo`, clears
`scope_redo`, and drops the oldest entry past 32. `:scope undo` /
`frame::scope_undo` (`mod+z`) pops to `scope_redo`; `:scope redo` /
`frame::scope_redo` (`mod+shift+z`) reverses it. A no-op mutation (the
same scope set again) pushes nothing, so undo never appears to do
nothing.

The text field is the one exception: keystrokes coalesce into one undo
entry per focus session — the scope as it stood when the field took
focus, pushed on the first keystroke that changes it — not one per
character. Otherwise undo after typing `spx` walks back `sp`, `s`, and
the trader's previous scope is four presses away.

### 3.9 Saved scopes

`scopes.toml` is already a recognised atomic doc name that nothing
reads:

```toml
config_version = 1

[eurp_spx]
dimensions = { model_code = ["EURP"], underlying_ref = ["SPX"] }
text = ""
expression = "npv > 0"
```

`ScopeSpec::from_doc` lives in `geode-core` beside the other readers;
each named scope is atomic at depth one, so a user layer overriding
`eurp_spx` replaces it whole. A scope naming a column no dataset
declares is an error for that scope only. `Scope::validate` is the
check, run at load with the diagnostics reported like a view's.

`:scope save <name>` writes the global scope to the user layer's
`scopes.toml` through the config write door (§5.4) and adds it to the
frame in place. `:scope load <name>` and the palette entries `Scope:
<name>` set it, through the normal undoable path. A desk layer's
`scopes.toml` is the sharing mechanism of §4.3, with no further
machinery.

### 3.10 The flip barrier

§4.3: "all tiles flip together, never a half-updated screen". Today
each tile requeries on its own and paints on its own delivery.

```rust
pub struct FlipBarrier {
    /// The versions this flip is for; outcomes for older versions are
    /// already stale by the tile's own tag check.
    versions: FrameVersions,
    awaiting: HashSet<QueryKey>,
    opened: Instant,
}

impl Frame {
    /// Called by the shell after a scope/grouping/as-of mutation with
    /// the keys of every visible tile that follows the changed field.
    pub fn open_flip(&mut self, keys: impl IntoIterator<Item = QueryKey>);
    /// A tile has its snapshot for `versions` staged. Returns `true`
    /// when the barrier just emptied; the frame bumps `versions.flip`
    /// and notifies.
    pub fn arrived(&mut self, key: QueryKey, versions: FrameVersions) -> bool;
    /// Deadline sweep, driven by the shell's existing 500 ms tick and
    /// by the blotter on delivery: past 250 ms the barrier releases
    /// with whatever has arrived.
    pub fn sweep(&mut self, now: Instant) -> bool;
    pub fn flip(&self) -> u64;
}
```

A tile whose outcome matches an open barrier's versions stores the
snapshot as `staged` instead of promoting it, then calls `arrived`.
When the barrier empties or the deadline passes, `versions.flip` bumps
and every tile, in the same observe pass, promotes `staged` if it has
one. A tile that failed keeps its last-good snapshot with the existing
stale marker, and its failure counts as arrival so one broken tile
never holds the rest. A tile hidden when the barrier opened is not in
the set and requeries on `set_visible` as today.

`data` and `config` version bumps do not open a barrier: a publish is
per dataset by nature, and a tile that refreshes alone on new data is
correct, not half-updated. Only `scope`, `grouping` and `as_of` open
one. The barrier is `Frame` state because the frame is the one entity
every tile already observes; adding a second observable would double
the notify traffic for nothing.

The deadline is 250 ms: five times the §7.1 requery budget, so a
healthy frame never hits it, and short enough that a single slow tile
delays the rest by less than a beat.

**As built (2026-09-08, Phase 4b M14; corrected in Task 1 fix round 1,
MIN-5):** the sweep is driven by the shell's existing ~500 ms
reload-poll tick alone, not also by a fresh `cx.spawn` timer per
scope/grouping/as-of mutation — an earlier version spawned one such
detached timer on every mutation (on top of the tick), so a burst of
keystrokes spawned a burst of timers all racing to sweep the same
barrier. `on_frame_changed` no longer spawns anything; the
reload-poll loop calls `Frame::sweep` at the top of its body,
unconditionally on every tick, before it goes on to flush a dirty
session and (`.await`) run a background `reload::scan` of the desk and
user config dirs. The practical effect is a barrier released "on the
poll loop's next iteration after 250 ms have passed" — normally
≤ 500 ms, but bounded by that loop's *whole iteration* (the 500 ms
timer, then the session flush, then the config scan), not by the
500 ms interval alone: a slow scan (a desk dir on a network mount, say)
lengthens every barrier's worst case by exactly as much, since the
sweep already ran earlier in that same iteration and nothing schedules
a second one until the loop comes back around. A healthy frame still
never gets near either number.

### 3.11 Actions and keys

| Action | Default | Effect |
|---|---|---|
| `frame::focus_text` | `mod+/` | Focus the scope bar's text field. |
| `frame::pick` | `mod+p` | Open the dimension picker. |
| `frame::pick_<column>` | unbound | Open the values picker for that column. |
| `frame::as_of` | `mod+t` | Open the as-of selector. |
| `frame::live` | unbound | Return to live (remembers the previous as-of). |
| `frame::as_of_undo` | unbound | Swap back to the previous as-of. |
| `frame::scope_clear` | unbound | Clear the global scope (undoable). |
| `frame::scope_undo` / `frame::scope_redo` | `mod+z` / `mod+shift+z` | The stacks of §3.8. |
| `scope::<name>` | unbound | Load a saved scope; one per name, palette category "Scope". |

All in the `shell` context, so they work with any tile focused and are
unavailable while an input has focus, same as every shell chord.

As built: `mod+/`, `mod+p`, `mod+t`, `mod+z` and `mod+shift+z` ship
exactly as bound above. `frame::live`, `frame::as_of_undo` and
`frame::scope_clear` ship unbound as the table says — every registered
action is a palette entry regardless of binding, so all three are
palette-only until a keymap layer binds them; that is the intended
route, not a gap. `scope::<name>` is registered the same way
`frame::pick_<column>` is (`defaults::register_scope_actions`, called
from `main.rs` right after `register_pick_actions`), one per saved
scope, unbound, category "Scope" — see §1.3 for the live-reload
exception it shares with the pick actions. `keymap.mod = "ctrl"` is invalid config (Task 4b): it
is refused with an error diagnostic at load and reload and the alias
falls back to the default, because the shipped literal `ctrl+…`
bindings (`ctrl+1..9` workspace switching, `ctrl+0`, `ctrl+k`,
`ctrl+/`, …) are fixed and `mod` exists precisely so the user-facing
chords above can move without ever landing on one of them.

The `:` vocabulary (P3 §4.3) gains `:scope drop <dimension>`, `:scope
redo`, `:scope save <name>`, `:scope load <name>`, `:asof undo`, and
the `:filter` forms of §3.7. Completions follow the existing `commands`
table: dimension names after `drop`, saved-scope names after `load`.

### 3.12 Session

`session.toml` gains `[frame]`: the global scope (dimensions, text,
expression source), the active slot, and as-of. A restored as-of paints
the indicator from the first frame, so a session that was historical
yesterday never opens looking live. The blotter's tile record gains the
`filter` table of §3.7. Nothing else persists: undo stacks and the
barrier are session-local by nature.

## 4. Phase 4b — diagnostics

### 4.1 `tracing`

`tracing` becomes a workspace dependency; `tracing` and
`tracing-subscriber` are already in `Cargo.lock` through gpui, and
`tracing-appender` is added for file rotation. Every `eprintln!` outside
tests (26 sites across `geode-app`, `geode-shell` and `geode-data`)
becomes an event with one of these targets:

| Target | Emitted by |
|---|---|
| `geode::ingest` | discovery, staging, publish, retention |
| `geode::query` | pool, compile, distinct, catalog |
| `geode::config` | load, merge, reload, writes |
| `geode::session` | layout and tile persistence |
| `geode::shell` | focus, dispatch, modals, the bridge's drain |
| `geode::theme` | theme and font loading |

Levels: `error` for anything that lost data or a feature (a failed load,
a rejected reload, a dropped event); `warn` for degraded-but-running (a
source pending too long, a config warning, a `try_cast` blank); `info`
for lifecycle (published, reloaded, restarted); `debug` and `trace` for
the rest. Code on the UI thread emits at `warn` or above only —
formatting a message allocates, and an `info` per frame is a defect by
the charter's per-frame-churn rule.

`geode-app::main` installs the subscriber before anything else runs:

```rust
let (filter, reload_handle) = reload::Layer::new(Targets::from(levels));
tracing_subscriber::registry()
    .with(filter)
    .with(fmt::layer().with_writer(std::io::stderr))
    .with(geode_core::log::RingLayer::new(ring.clone()))
    .with(fmt::layer().with_writer(rolling))
    .init();
```

The `reload_handle` and `ring` go into `ShellServices` so the shell can
change levels and the diagnostics tile can read the tail.

**As built (2026-09-08, Task 2; corrected in fix round 1, MAJ-1/MAJ-2/
MIN-6):** `geode-data` turned out to already be silent — zero
`eprintln!` sites, confirmed by grep before migrating — so its three
`tracing` call sites (a publish, the scheduler's `Health` arm, the
scheduler's `Polled` arm) are new instrumentation, not a migration; the
26-site table above is really `geode-app` + `geode-shell` +
`geode-blotter`. The filter is not a flat `Targets::from(levels)`:
`LogLevels::to_targets` builds `Targets::new().with_default(Level::
WARN).with_target("geode", self.default)` before layering per-suffix
overrides, so a third-party crate's own `tracing` output is capped at
`warn` regardless of `[log] default` — only `geode::*` targets follow
the configured level. `Health::Failed` logs through one choke point in
`geode-data`, `log_health_event`, at `error`; every other `Health`
variant logs at `warn` or below — the bridge (`geode-app`) does **not**
also log these events a second time at a different severity: its own
copies were deleted outright (fix round 1, MAJ-2) rather than dropped
to `debug`, since `geode-data`'s lines already carry more detail
(book/row counts) and a disabled `debug!` still costs a level check on
the UI thread for no benefit. The one UI-thread log site below `warn`
that remains, `profiling_hook.rs`'s `info!` dump, is called out
explicitly in that file's module doc as the deliberate exception to
"code on the UI thread emits at `warn` or above only." The automated
guard for "no `eprintln!` outside tests"
(`no_eprintln_outside_tests_in_workspace_src`, `geode-app/src/main.rs`)
tracks `#[cfg(test)]`/`#[cfg(any(test, feature = "test-support"))]`
scope by brace depth rather than a plain grep, so a debug `eprintln!`
legitimately left inside an inline test module doesn't trip it.

### 4.2 The ring buffer

`geode_core::log::Ring`, capacity 4,096 records, fixed at construction:

```rust
pub struct Record {
    pub at: SystemTime,
    pub level: Level,
    pub target: &'static str,   // tracing targets are 'static
    pub message: String,
    pub seq: u64,
}

pub struct Ring { inner: Mutex<RingInner> }   // records: Box<[Option<Record>]>, head, seq
impl Ring {
    pub fn push(&self, r: Record);
    /// Records with `seq > since`, oldest first, copied into `out`
    /// (cleared first). The reader supplies the buffer, so a tile that
    /// follows the tail reuses one `Vec` for the life of the tile.
    pub fn drain_since(&self, since: u64, out: &mut Vec<Record>);
    pub fn latest_seq(&self) -> u64;
}
```

The writer allocates the message `String` on the emitting thread inside
the `Layer`'s `on_event`; the reader allocates nothing on a hit and only
`clone`s records newer than its `since`. The mutex is held for a copy,
never for formatting. Overwrite on wrap; nothing blocks a writer.

**As built (2026-09-08, Task 2; corrected in fix round 1, MIN-9):**
`drain_since` walks backward from the newest slot (descending `seq`)
and `break`s the moment it reaches a record at-or-before `since` (or an
unwritten pre-wrap slot) — everything further back is provably older,
by the ring's own monotonic invariant — rather than scanning all 4,096
slots on every call; a `#[cfg(test)]` `Ring::drain_visits` counter
proves the bound directly. `Ring::oldest_seq()` was added beyond the
spec's original vocabulary: `None` when nothing's been pushed, the
record at `head` once the ring has wrapped, the record at index 0
before the first wrap — the diagnostics tile's log section uses it to
report "N records lost" when the tail it's been following has fallen
behind a wrap. `a_hit_allocates_nothing_in_the_reader`
(`crates/geode-core/src/log/mod.rs`) pins the reuse-the-buffer claim
directly: a second drain returning the same count of records must not
grow the `Vec`'s capacity.

### 4.3 Files and levels

The file layer writes `logs/geode.YYYY-MM-DD.log` under the user data
dir with `tracing-appender`'s daily rotation and a seven-file cap
applied at startup. It is installed by `geode-app`, which is where the
file rule's standing exception (config, session, now logs) lives; no
module writes a file.

`[log]` in the `app` doc:

```toml
[log]
default = "info"
ingest = "debug"
```

Keys are the target suffixes; the merged doc becomes a `Targets` filter.
A reload that changes `[log]` calls the reload handle and nothing else;
`:level <target> <level>` in the diagnostics tile does the same and
writes the user layer's `app.toml` through the config write door.

**As built (2026-09-08, Task 2/4; corrected in Task 2 fix round 1,
MIN-3/MIN-7):** "the user data dir" resolves concretely to
`config_dirs().1` (`%APPDATA%/geode` or `$HOME/.config/geode`); log
files land at `<that>/logs/geode.YYYY-MM-DD.log`, trimmed to the newest
seven at startup (`crash::trim_log_files`). **The daily file rolls on
the UTC date, not the trader's local one** — `tracing-appender`'s
`Builder` has no local-clock rotation option to switch to. Phase 4a's
"times are the trader's local clock throughout" ruling governs every
*displayed* time (the as-of selector, the scope bar, the status
segment); it does not reach the log file's own name, which is UTC by
the library's own constraint, documented rather than worked around
(the crash file's timestamp, §4.7, is UTC for the identical reason). A
present-but-non-table `[log]` (the typo `log = "debug"` for `[log]\n
default = "debug"`) is a warning, not a silent no-op; an unknown target
key under `[log]` is likewise a warning, and the key is dropped. The
config write door has no 4b-specific implementation: `:level`'s persist
(`log_persist::persist_log_level_to_user_config`) reuses
`theme::write_atomic` and `persist_slot_to_user_config`'s `toml_edit`
pattern as a seventh caller, exactly as Phase 4c's own config-write-door
task will later migrate it along with the other six — nothing new was
built here on purpose.

### 4.4 The `Diagnostics` entity

Shell-owned, beside `Frame`, observed by the diagnostics tile, fed by
the bridge:

```rust
pub struct Diagnostics {
    pub sources: BTreeMap<String, SourceState>,   // health, since, last poll, next poll, history: VecDeque<(Instant, Health)> (16)
    pub datasets: BTreeMap<String, DatasetState>, // generations: Vec<Generation> from Published + Catalog
    pub config: Vec<Diagnostic>,                  // load + every reload, latest first, bounded
    pub dropped_events: u64,
    pub restart_required: Option<String>,
    pub frame_hist: FrameHistogram,               // moved from the overlay's state; the overlay reads it here
    version: u64,
}
```

Every write bumps `version` and notifies; the tile compares versions and
rebuilds only on change. `Published`, `Health`, `Diagnostics`, and
`Catalog` events all land here; `ShellView::set_data_status` is deleted
and the status bar reads a summary from this entity instead.

**As built (2026-09-08, Task 4; corrected in fix rounds 1–4, CRIT-1,
MAJ-4/MAJ-5, NEW-1, NEW-4, NEW-5/NEW-6):** `Health` moved from `geode-data` to `geode-core`
(`geode-data::health` is now a re-export) — the interface above has
`geode_shell::diagnostics` naming `Health` directly, which would have
put a `geode-data` dependency on `geode-shell` and broken "shell and
data never depend on each other" on day one of the entity's existence.
`SourceState.health` is `Option<Health>`, and `summary()` counts only
sources with a *real* health note — a configured-but-not-yet-reported
source is not counted at all, in either direction (CRIT-1: the section
below shows it separately as "no report yet"). The value a source's
health note actually carries is the WORSE of two independently-tracked
lanes, discovery (content-blind: is anything currently stuck or
malformed on disk) and load (content-aware: did the last publish or
load attempt succeed) — final review round 3, NEW-4: a single shared
last-value map let a routine, content-blind clean poll silently clear a
real, unfixed `Degraded`/`Failed` a publish had set, within about one
poll interval and with nothing actually corrected. The load lane is
keyed per BATCH and rolled up as the worst of them (round 4, NEW-6): a
`Degraded` generation stays live and queryable, so one batch publishing
cleanly says nothing about another's still-degraded rows, and only that
batch's own next publish replaces its entry. "Worse" is an explicit
severity rank, never `Health`'s derived `Ord` — which compares the
`reason` string once two variants tie, so two simultaneous `Degraded`s
were ordered by the alphabet and one was dropped — and an equal rank is
decided by whichever slot changed most recently, with an identical
re-report changing nothing so that repeated clean polls cannot flap it
(round 4, NEW-5). `config: Vec<Diagnostic>`
is not an unconditionally-appended log: `note_config` *replaces* the
current batch wholesale and is a no-op when the new batch is
byte-identical to the old one (MAJ-5) — an unchanged reload (e.g. the
one `:level`'s own persist write triggers) must not re-inflate the
error count — with every real change also appended to a separately
capped `config_history`. `data_diagnostics` is its own separately
capped, separately counted population, fed by the app from data-layer
diagnostics distinct from config diagnostics (NEW-1, fix round 2) — the
two must never share one count, or a config reload and a data error
would silently erase each other's number. `summary() -> Rc<str>` is
cached on `version`, so a repeated read (the status bar, every render)
clones a refcount rather than rebuilding a string; it composes as
`"sources 3 ok · 1 degraded · config 2 errors · data 1 error · 5
dropped"`, every segment omitted at zero — see §4.8's own as-built note
for why `restart_required` is not one of them any more. `watch()` and
the later-added `request_catalog()` (§4.6) share one contract worth
stating plainly: both queue a side effect (`pending_catalog_request =
true`) but neither calls `cx.notify()` itself — they have no
`Context`. A caller MUST notify in the same update block
(`diagnostics.update(cx, |d, cx| { d.watch(); cx.notify(); })`), or the
queued request sits unseen by the bridge's `cx.observe(&diagnostics,
..)` drain until some unrelated mutation happens to notify later;
`watch`'s doc comment states this as the reason it returns `bool`
(always `true` today) rather than `()` — the return type exists to make
"a request was queued, you now owe a notify" visible at the signature,
not just in prose. `Diagnostics.frame_hist` is copied from `ShellView::
perf` on the shell's existing ~500 ms reload-poll tick and only while
`watchers() > 0` (open question 2, resolved) — see `docs/perf.md`'s
Phase 4b section for the allocation contract this pins in tests.

### 4.5 `Request::Catalog`

```rust
pub struct CatalogParams { pub key: QueryKey, pub tag: u64, pub as_of: AsOf }

pub struct CatalogSnapshot {
    pub datasets: Vec<DatasetCatalog>,   // name, partitions, live_rows/archive_rows (est.)
    pub database_bytes: u64,             // sum(block_size * total_blocks), pragma_database_size()
    pub used_blocks: u64,
    pub block_size: u64,
    pub memory_bytes: u64,               // sum(memory_usage_bytes), duckdb_memory()
    pub threads: u64,                    // current_setting('threads')
}

pub struct DatasetCatalog {
    pub name: String,
    pub partitions: Vec<PartitionCatalog>,
    pub live_rows: u64,      // estimated_size (a row ESTIMATE, not bytes), live tables
    pub archive_rows: u64,   // the same, over archive tables
}

pub struct PartitionCatalog {
    pub batch: String,
    pub book: Option<String>,        // None is the bookless partition — real, not missing
    pub generations: Vec<GenerationInfo>,
    pub resolved_gen: Option<i64>,   // the gen_id `as_of` resolves to; None under AsOf::Live
}
```

**As-built correction (MIN-4, final-review fix):** the original sketch
above named `live_bytes`/`archive_bytes` and a top-level `CatalogSnapshot.
resolved: Vec<(String, Option<i64>)>`. The shipped types instead carry a
row *estimate* (`live_rows`/`archive_rows`, correctly labelled "rows
(est.)" on screen — DuckDB's `estimated_size` is not a byte count) on
`DatasetCatalog`, and put `resolved_gen` on each `PartitionCatalog`
rather than in one dataset-keyed list on the snapshot — the per-
partition placement is what the data section's marker (below) actually
needs, since a resolved generation is a property of one partition's
timeline, not the whole dataset. Three of the five computed catalog
fields are wired into the perf section — `database_bytes`,
`memory_bytes` and `threads` (`"database {} (checkpointed) · memory {}
· threads {}"`) — rather than left computed-and-unread. `used_blocks`
and `block_size` remain computed (the same `pragma_database_size()`
round trip already pays for them) but displayed by nothing (MIN-4,
final review round 2 — recorded as a known gap, not fixed): a future
reader may wire them in beside `database_bytes` or drop them from
`CatalogSnapshot` outright.

Built on the data thread from `file_generations`, `pragma_database_size`
and per-table storage info, and delivered as `DataEvent::Catalog`. The
shell requests one when a diagnostics tile becomes visible and after
each `Published` while one is visible, coalesced by tag like every other
request. The exact DuckDB functions and their output columns are
verified by execution in the plan, not assumed: this is the class of
defect Phase 3's prerequisites document warns about, an accessor tested
against a fixture rather than against what DuckDB produces.

**As built (2026-09-08, Task 3; corrected in fix round 1, MAJ-2/MAJ-3):**
verification by execution still found two live gaps in the pinned
DuckDB (1.10505.0). `current_setting('threads')` comes back typed
`BIGINT`, not `VARCHAR` as originally verified — read via an explicit
`::varchar` cast so the column type can't surprise the reader again.
`pragma_database_size()`'s `total_blocks`/`block_size` (and so
`database_bytes`) read `0` until the database has been checkpointed —
WAL content not yet flushed to disk isn't counted; `store::
retention::sweep`'s own cadence is what moves it in real usage, so a
tile opened right after a burst of uncommitted ingest can legitimately
show `0 B` until the next sweep. `file_generations_for` is bounded by
an `exists` join against the `generations` table (MAJ-2) rather than
reading every row `file_generations` has ever recorded — an orphaned
row for a generation that no longer exists (superseded, retention-swept)
must not leak into the catalog. Partitions group by `(batch, book)`
together, never either half alone (MAJ-3): two same-book partitions in
different batches, or a bookless partition sharing a batch with a
booked one, are distinct rows, and a bookless partition is kept as its
own partition with its own live generation, not dropped. `duckdb_
tables()` is scoped to `schema_name = 'main'`; `database_bytes`/
`used_blocks` are `coalesce(sum(...))` over `pragma_database_size()`
rather than the first row, future-proofing a second `ATTACH`; the
resulting `CatalogSnapshot` also carries `block_size`. `SchedulerEvent::
Polled` gained `next_in: Duration`, and the mapped `DataEvent::Polled`'s
`next` is `at.checked_add(next_in).unwrap_or(at)` — saturating rather
than panicking on a pathological poll interval. `Request::Catalog`
answers on the service thread directly, never the query pool, per the
plan's own ruling — its doc comment says so, and `geode-app::bridge`'s
event match keeps explicit no-op arms for `Catalog`/`Polled` rather
than a wildcard, so a future `DataEvent` variant still fails to compile
unhandled.

### 4.6 The module

`geode-diagnostics` exposes a `ModuleFactory` of kind `diagnostics`.
`diagnostics::open` (`mod+shift+d`, the chord the deleted probe held)
opens one in the focused tile's workspace, or focuses an existing one.
The tile observes `Diagnostics`, `Frame` (for `RequeryStats` and as-of)
and reads the ring.

Five sections, switched by `:section <name>` or `[` / `]`, each a
list in the mono face with `j`/`k`/`gg`/`G`/`ctrl+d`/`ctrl+u` and `/`
filtering, rebuilt only when the observed version changes:

- **sources** — name, path, priority, readiness rule, health with
  detail, since, last poll, next poll. Sorted worst first.
- **data** — dataset › batch › book, each with generation id, published
  at, rows (live or archive, row estimates — no per-dataset bytes, MIN-4
  final review round 2); the row the current as-of resolves to marked
  when historical. Collapsible by dataset with `zo`/`zc`.
- **config** — every diagnostic with layer, file, line where known;
  then the effective-config explainer: each doc as a tree, every leaf
  as `path = value  [user]` using `Config::explain`. `/` filters by
  path.
- **log** — the ring tail, oldest at the top, following unless the
  cursor is moved off the end; `/` filters by target or level;
  `:level <target> <level>` changes and persists.
- **perf** — the frame histogram, `RequeryStats`, dropped events;
  `:overlay` toggles `perf::toggle_overlay`.

The module renders no `DataTable`; these are lists under a few hundred
rows, and `uniform_list` covers the log (as built: not `VirtualList` —
see the as-built note below; every row is the same height, so
`uniform_list`'s cheaper fixed-height model is sufficient and the
`VirtualList` per-item-sizing cost §3.4's picker pays is not needed
here).

**As built (2026-09-08, Task 5; corrected in fix rounds 1–2, MAJ-1
through MAJ-8, MIN-3/MIN-7):** `diagnostics::open` resolves through a
generic door, `ShellView::open_module(kind)`, not diagnostics-specific
plumbing — it focuses an existing occupant of the given kind (searched
across the tree and every dock) or splits the focused tile and stashes
`pending_kind_for_new_tile` for `ensure_occupants` to consume on the
next pass; two rapid `mod+shift+d` presses before any render settles
split only one tile, not two (`open_module` early-returns when a
request for the same kind is already pending). The config section's
effective-config explainer walks `Config::doc_names()` (added in fix
round 1 — an alphabetical iterator over every doc actually loaded)
rather than a hand-maintained doc-name list, and recurses into TOML
arrays with indexed paths (`keymap.bindings.0.keys.j`, MAJ-8) instead
of stringifying an array whole onto one row, capped at 2,000 leaves per
doc with a trailing "… N more" row. The log and config sections' `/`
filter is a plain substring match over the whole formatted row/path —
broader than "filter by target or level" reads literally, but it
satisfies the given examples and is judged the more useful behaviour
for a trader typing into `/`. The data section's resolved-generation
marker refreshes on an as-of change while the tile is visible through
`Diagnostics::request_catalog()` (MAJ-7, a new method sharing `watch`'s
own caller-must-notify contract, §4.4); the tile's own frame-relevance
comparison is narrowed to exactly the two `FrameVersions` fields any
section reads — `as_of` and `config` — so a scope- or grouping-only
frame change does not rebuild it (further narrowed per-section by the
final review's MAJ-4: `as_of` only matters while showing `Data`,
`config` only while showing `Config`), and `FrameVersions.flip` was
never among the fields compared (see CLAUDE.md's own maintainer note on
`flip`). `CatalogSnapshot` also now carries the `AsOf` it was resolved
under (final review, MIN-5): the marker requires that to equal the
frame's *current* `as_of`, not just `resolved_gen == gen_id`, so the
brief window between an `At(T1) -> At(T2)` change and the fresher
catalog's arrival shows no marker at all rather than momentarily
marking the generation `T1` resolved to next to a scope bar already
reading `T2`. Closing a tile now unwatches
the entity generically, in `ensure_occupants`, for every module kind —
not a diagnostics-specific fix — closing what had been a real watcher
leak (MAJ-2). `rows: Rc<Vec<Row>>` on the tile means `render` clones a
refcount, not the row list, between paints (MAJ-4); the log section's
own per-render allocation was closed the same way, with a persistent
`drain_buf: Vec<Record>` reused across drains (MAJ-5) and the tail's row
builder reading `self.records.make_contiguous()` directly rather than
cloning it. The log tail scrolls the cursor into view on `move_cursor`/
`top`/`bottom`/rebuild (MAJ-1, `scroll_to_item`), and reports "N records
lost" via `Ring::oldest_seq()` when the ring has wrapped past the
tile's own `since`. `since` is seeded from `ring.latest_seq()` at
construction, not `0` (MIN-3, final review), so a tile opened after the
ring already holds more records than its capacity does not report
records it never had as "lost" on its first drain; `lost_records`
itself is recomputed on every log-section rebuild that finds new
records, and left in place — a real, still-true number — on one that
finds none.

**Two gaps recorded rather than fixed (MIN-11, final review), both
judged harmless:** `:level` accepts only the six target suffixes
(`commands.rs`), not `default` — `[log] default` can only be changed by
editing the file directly, defensible since `default` is a
whole-process floor, not a per-target override the tile's own
vocabulary is about. And the log section shows neither the current
`LogLevels` nor which level is in force for any target — `Diagnostics.
levels` is carried on the entity and displayed by no section, so a
trader who runs `:level ingest debug` has no in-tile confirmation it
took effect beyond watching debug-level lines start appearing.

**Four display checks remain unverified** — no
display was available in the implementing environment: `mod+shift+d`
opening a split tile, `]`/`[` cycling sources → data → config → log →
perf, `:level ingest debug` showing in the log section, and that
command's write landing in `<user_dir>/app.toml`'s `[log]` table. All
four are covered by unit/integration tests at every seam they cross;
none has been looked at on a painted screen.

### 4.7 Panic boundaries

§10.1: a crashed background worker restarts, logs loudly, and marks its
source degraded. The query pool already does this (Phase 3
prerequisites §2). 4b adds:

- **Ingest.** One file's load (discover → stage → split → publish) runs
  under `catch_unwind`. A panic logs at `error` with the file and the
  payload, marks the source `Degraded { reason }`, and the runner
  continues with the next file. The runner thread itself is not
  restarted, because it never dies: the boundary is per file.
- **Crash file.** A panic hook, installed after the subscriber, writes
  `crash-<timestamp>.log` under the user data dir with the panic
  message, location, the ring's contents and the last 32 dispatched
  action ids, then defers to the previous hook. The action tail is a
  small ring in `ShellView::dispatch`, written to on the UI thread
  without allocation (a fixed array of `ActionId`).

**As built (2026-09-08, Task 6; corrected in fix round 1, MAJ-1/MAJ-2):**
**an ingest load panic marks the source `Health::Failed`, not
`Degraded` as written above** — a ruling taken at plan time and kept
through implementation: a panic is a failed load, not a
degraded-but-running state, and the last good generation stays live
regardless. All four background `catch_unwind` boundaries this codebase
has — the ingest load, its pop-time catalog recheck, a discovery poll,
and a query pool worker, not only the "one file's load" the bullet
above names — run under a new `geode_core::panic::contained` (a
thread-local, depth-counted RAII guard), and the panic hook checks
`geode_core::panic::is_contained()` before deciding what to do: a
process panic hook fires before any `catch_unwind` gets a chance, so
without this check every one of those four boundaries doing exactly
what it exists for (catching a panic, keeping the app running) would
still produce a crash file. A **contained** panic logs at `error` and
writes nothing; only an **uncontained** panic — one nothing caught —
calls `write_crash_file`. The action tail is FNV-1a hashes
(`[u64; 32]` in `ActionTail`), not `ActionId`s directly — cloning a
`String` id on every dispatch would violate the same allocation
discipline the ring itself follows; `ActionRegistry::hash_names()`
hands the crash hook a snapshot to resolve hashes back to ids. The
crash file's name gained millisecond resolution and collision handling
beyond the `<timestamp>` above: `crash-<YYYYMMDD-HHMMSS-mmm>.log`,
opened with `OpenOptions::create_new`, retried with a `-1`, `-2`, …
suffix on a same-instant collision rather than truncating an existing
file, and pruned to the newest 10 after every write (sharing a
`prune_files` helper with the log directory's own seven-file trim,
§4.3). The timestamp is UTC, matching the log files' own daily-rotation
clock (§4.3's as-built note) rather than the trader's local time. The
hook's lock acquisitions (the action tail, the resolved-hash names map)
use `try_lock`/`try_read` with a fallback placeholder rather than
blocking, so a panic triggered from inside a lock holder cannot
deadlock the hook itself. Not addressed, and recorded rather than
fixed: a panic before the hook installs — during config/schema load,
session read, or demo emission, all of which run earlier in `main` —
leaves no crash file, only whatever the log file already captured.

### 4.8 Status bar

The one-line label becomes a summary from `Diagnostics`: `sources 3 ok
· 1 degraded`, the config error count when non-zero, the dropped-event
count when non-zero, and the restart message. Clicking it, or
`diagnostics::open`, opens the tile.

**As built (2026-09-08, Task 4/5; corrected in fix round 1, MAJ-4):**
the summary does **not** include a `restart required: …` segment as
written above — `shell/render.rs`'s status bar already paints its own,
separate `restart_required` segment, and embedding the same message
inside `Diagnostics::summary()` too duplicated it on screen. The
implemented shape is `"sources 3 ok · 1 degraded · config 2 errors ·
data 1 error · 5 dropped"`, config and data error counts kept as two
separate, never-merged counters (§4.4's as-built note), every segment
omitted when its count is zero, and the whole string `""` (never
rendered — the call site's own `(!s.is_empty()).then_some(..)`) when
there is nothing to report. Clicking the segment and `diagnostics::open`
both resolve to the same door, `ShellView::open_module("diagnostics",
..)` (§4.6's as-built note).

## 5. Phase 4c — the config editor

### 5.1 The tile

`geode-config-editor` exposes a `ModuleFactory` of kind
`config_editor`. `config::edit` (palette) or `:edit <doc>` from any
tile opens a picker listing every recognised doc name × layer, marked
`exists` or `new` and with the builtin layer marked `read-only`. Picking
one opens the file's text in an `EditorState` with `.language("toml")`
and `.line_number(true)`. The tile record carries `doc` and `layer`.

TOML highlighting needs the `tree-sitter-toml` feature on the
gpui-component dependency; the workspace's manifest turns it on. The
Windows CI job already builds bundled DuckDB, so a C toolchain is not a
new requirement there; the plan verifies both platforms build.

The editor is read-only for the builtin layer and for a desk file the
process cannot write (checked at open by attempting to open for append
and closing). `:copy user` on a read-only doc creates the user-layer
file with the same text and reopens it writable.

### 5.2 Validation as you type

On every buffer change:

1. Parse with `toml_edit`. A syntax error becomes one diagnostic at the
   error's line and column, severity error, source `toml`.
2. On a clean parse, run the doc's own reader — the same `from_doc` the
   loader uses for that doc name (`SchemaSpec`, `ViewSpec`,
   `GroupingSlots`, `ScopeSpec`, keymap, theme, sources, app) — against
   the parsed table alone. Each returned `Diagnostic` becomes an editor
   diagnostic, source `geode`, at the span of its key path when it has
   one (§5.3), else at line one.

Diagnostics go into the editor's `DiagnosticSet` via `diagnostics_mut`,
which the pinned checkout renders as squiggles with a hover popover.
A TOML parse is microseconds for any config file this app has; the
readers are the same order. Both run synchronously on the UI thread on
the change event, with no debounce, and the plan measures the largest
shipped doc (the keymap) to confirm it stays well under a millisecond.

Running the reader on one layer alone means a user-layer view that
overrides a desk view's columns validates against its own text, not
the merged result. That is the honest thing to validate — the file
being edited — and the reload after save reports anything the merge
turns up.

### 5.3 `Diagnostic` gains a key path

```rust
pub struct Diagnostic {
    pub severity: Severity,
    pub layer: Option<Layer>,
    pub file: Option<PathBuf>,
    /// The TOML key path of the offending value, dotted, when the
    /// reader knows it: `views.tree.columns.3.name`.
    pub path: Option<String>,
    pub message: String,
}
```

Readers fill `path` where they already know the key, which is most
places: a view's unknown column, a grouping slot's bad element, a
scope's unknown dimension, an unknown key in an atomic doc. The editor
resolves a path to a span by walking the `toml_edit` document and
reading the item's `span()`. `Display` appends ` (at path)` when set,
so the diagnostics tile and stderr gain it for free.

### 5.4 Saving and the write door

`theme::write_atomic` and the two `persist_to_user_config` variants
(theme, font size, groupings) are generalised into one shell-owned door:

```rust
pub mod config_write {
    pub fn read(layer: Layer, doc: &str, services: &ShellServices) -> io::Result<String>;
    pub fn write(layer: Layer, doc: &str, text: &str, services: &ShellServices) -> io::Result<()>;
    pub fn edit(layer: Layer, doc: &str, services: &ShellServices,
                f: impl FnOnce(&mut DocumentMut)) -> io::Result<()>;
}
```

`write` is the temp-file-and-rename the theme module does today; `edit`
is the read-modify-write with `toml_edit` that every keyed persist
(slot save, scope save, log level, presentation) uses so comments and
unrelated keys survive. Modules call these through `ShellServices` and
open no file themselves.

`ctrl+s` in the `config_editor` key context writes the buffer. The
watcher notices within its 500 ms poll and runs the normal reload with
last-good semantics. The tile subscribes to the reload outcome
(`ShellEvent::ConfigReloaded`, plus a new `ShellEvent::ReloadRejected
(Vec<Diagnostic>)`) and shows `saved · applied` or `saved · rejected:
<n> errors` in its header, with the rejected diagnostics merged into the
editor's set. A rejected save is on disk and not live, which is
exactly what last-good means, and the tile says so.

### 5.5 Focus

The editor takes focus and types in insert mode. `escape` returns focus
to the shell, so every tiling chord works with the editor on screen;
`i` or `enter` with the tile focused re-enters the buffer. The tile's
mouse-down re-arms the shell's `pending_focus_restore`, as every
focus-holding occupant must (CLAUDE.md). The editor's own key context is
`config_editor`; only `ctrl+s` and `escape` are bound there, and the
`NoAction` reclaim that `geode_blotter::init` applies to `DataTable` is
not needed because the editor is meant to own the keys while focused.

### 5.6 Column personalisation

The blotter already receives `TableEvent::ColumnWidthsChanged` and
`TableEvent::MoveColumn`. It gains `:hide <column>`, `:show <column>`,
`:cols reset`, and `:cols save`. Widths and moves record into the
tile's plan immediately; `:cols save` and the two toggles write:

```toml
# view_presentation.toml, user layer
config_version = 1

[tree]
order = ["book", "npv", "delta01"]
hidden = ["cross_gamma02"]
[tree.width]
npv = 120
```

`ViewPresentationSpec::from_doc` in `geode-core`; atomic at depth one
per view name. It is merged over the `ViewSpec` after the named-object
merge, in the loader, so `ViewSpec.columns` order, a `hidden: bool` on
`ViewColumn`, and `ColumnPresentation.width` reflect it before any
module sees the view. Columns named here that the view lacks are
warnings and ignored, so a desk renaming a column never breaks a
personal file. `:cols reset` deletes the view's table from the user
file.

Widths and moves are not written on every drag: a drag emits many
events, and the write is a file. `:cols save` writes; a session end
(`take_dirty_session_write`) writes any tile whose plan differs from its
view's presentation. Both go through `config_write::edit`.

### 5.7 The reload prompt and the swappable handle

A `sources` or `datasets` change currently sets `restart_required`.
4c opens a shell modal instead: "Sources or datasets changed. Reload
data now? Open blotters requery when it completes." with `Reload now`
and `Later`. `Later` keeps the label and the modal does not reappear
until the next such change.

`Reload now`:

1. The bridge tells the current `DataService` to shut down on the
   background executor (the existing `DataHandle::shutdown` path: never
   on the UI thread).
2. It spawns a new service from the reloaded config with a fresh sink
   feeding the same drain.
3. `DataHandle` becomes a door over a swappable inner
   (`Arc<ArcSwap<Inner>>` or an `Arc<RwLock<Arc<Inner>>>` read once per
   `send`), and the bridge swaps it, so every tile's existing clone
   sends to the new service.
4. `Frame::note_published` bumps `data`, every visible tile requeries,
   and `Diagnostics.restart_required` clears.

A request sent between steps 1 and 3 reaches the old service's closed
queue and returns `false` from `send`, which callers already handle as
"retry on the next trigger". Nothing is silently dropped: the mutation
entry for this behaviour deletes the swap and expects the requery after
the reload to fail loudly rather than answer from the old database.

The demo's generated source directory and database path follow the
same route, so `--demo` exercises the prompt by editing the compiled-in
layer's `sources.toml` copy in the user dir.

## 6. Error handling

- **Data problems** stay non-modal and local. A failed distinct query
  shows its text in the picker list. An as-of with no generation at or
  before it leaves tiles on the existing no-data path with the stripe
  still painted. A rejected reload keeps last-good and reports in the
  editor tile, the status bar and the diagnostics config section. A
  failed restart (the new service fails to open) logs at `error`, keeps
  the old service running, and tells the user in the modal.
- **User errors** stay at the point of entry. Expression errors at the
  caret in the command line; a text field never errors. Editor
  diagnostics at line and column. A `:scope load` of an unknown name,
  `:filter` on an unknown column, `:level` with a bad level: one line
  on the command line, no dialog.
- **Bugs** have the boundaries of §4.7.

## 7. Performance

The §7 budgets are contracts. What Phase 4 adds to the measured record:

**The text filter at 1M rows, before the rewrite.** Measured on
2026-09-06 with the `query` bench's schema marking all eight string
columns textual (as a real desk schema would), full `tree` view:

| Needle | Rows matched | Full tree, unscoped | Depth 2, unscoped | Full tree + 3-book selection |
|---|---|---|---|---|
| `bk00` (broad) | 456k | 119 ms | 45 ms | 56 ms |
| `bk007` (narrow) | 46k | 71 ms | 61 ms | 27 ms |
| `zzz` (nothing) | 0 | 63 ms | 63 ms | 26 ms |

The same depth-2 tree with no text filter is 10.3 ms; the 3-book scoped
tree is 23.9 ms (`docs/perf.md`). The ~50 ms floor is the row scan, paid
regardless of matches. The gate for §3.5 is the zero-match case over
ENUM columns only, inside 50 ms, with the plain-string residual
recorded. The bench keeps the text cases permanently, with `textual`
declared on the schema's string columns, as
`{rows}_rows_text_{broad,narrow,none}_{unscoped,depth_2,with_books}`.

**Other contracts.** The scope bar, the picker list and every
diagnostics section rebuild only on a version change. The ring's reader
allocates nothing on a hit. The flip barrier's deadline sweep is a
comparison on the existing 500 ms tick. The editor's validation is
measured on the largest shipped doc and recorded. The diagnostics tile
open with the log following must not move the frame histogram's p95,
measured in `--demo` with the overlay.

## 8. Tests and the harness

Weights per §10.3: data ≫ shell logic ≫ modules.

**Pure, no window:** chip summarisation including the contradiction
chip; the picker's selection state machine (toggle, select-all under a
filter, clear, pre-tick, apply-empty drops); as-of parsing for every
accepted form and the local-time rule; the undo and redo stacks with
the bound and the text-field coalescing; the barrier state machine
(open, arrive, empty, deadline, failure-as-arrival, hidden tile
excluded); `ScopeSpec` and `ViewPresentationSpec` readers with their
per-object atomicity; the ring (capacity, wrap, `drain_since`, two
writers); the key-path-to-span walk; the doc-and-layer picker's listing.

**Data layer:** carried dimensions — present in the declaring grain's
table and every finer one, absent from `position`; grouping by one
yields the same sums as grouping by the key it depends on; a
position-grain measure is `NonAttributable` under it; a file violating
the dependency loads with a `Degraded` reason naming the column; a
`dimension` with no grain outside the built-in key is a load error.
The `categorical` default per role and the `utf8`-only rule; interning
follows the flag (an opted-in attribute comes back as a dictionary, an
opted-out dimension as plain strings). `Distinct` under
scope-minus-own-dimension, under
as-of, unioned across two datasets; the ENUM rewrite selecting the
same rows as the row scan for a property-tested set of needles
(including `%`, `_` and `\`); the `textual` validation; `Catalog`'s
shape and sums against DuckDB by execution; the handle swap's
loud-failure property; the ingest boundary on an injected panic.

**Modules (`TestAppContext`):** "a picker commit requeries every visible
tile exactly once and they promote in one notify pass"; "typing three
characters submits three queries and paints the last"; "`:filter` marks
the tile and narrows only it"; "a Health event shows in the sources
section"; "`:level ingest debug` changes the filter and persists";
"a save reaches the reload path and the tile shows the outcome";
"escape from the editor restores shell chords"; "a hidden column is
absent after a session round-trip".

**Harness:** an entry per behaviour above, each with the 6th-argument
test filter. The entries most worth writing first, because their
mutations are silent wrong-data: the scope-minus-own-dimension rule
(mutate to full scope: counts shrink to the current selection); the
ENUM rewrite (mutate the pattern to a prefix match: `bk007` still
matches); failure-as-arrival (mutate to never arrive: the deadline
hides the defect unless the test asserts the flip time); the
presentation merge order (mutate to merge before the named-object
merge: a desk view override silently discards the personal width); and
the handle swap.

## 9. Crate layout after Phase 4

```
geode-app              + tracing init, Diagnostics feed, DataService restart, roster of three
  ├─ geode-blotter         + :filter, staged snapshot, column commands and events
  ├─ geode-diagnostics     NEW: five sections over Diagnostics and the ring
  ├─ geode-config-editor   NEW: editor tile, validation, save through the write door
  ├─ geode-shell           + scope bar, picker, as-of modal, Diagnostics entity, flip barrier,
  │                          undo/redo, saved scopes, pick_* actions, config_write
  ├─ geode-data            + carried dimensions in split, routing and compile; Distinct and
  │                          Catalog; interning and text rewrite by `categorical`; swappable
  │                          handle; ingest boundary; textual validation
  └─ geode-core            + Dimension { grain }, carried_at, `categorical` flag, log ring,
                             Diagnostic.path, ScopeSpec, ViewPresentationSpec
geode-demo-data          unchanged
```

The two new module crates depend on `shell`, `data` and `core`, never
on each other or on `geode-blotter`. `shell` and `data` still never
depend on each other. Neither module opens a file or socket; config
reads and writes go through `config_write`, logs through `tracing`.
Every new target carries `bench = false`.

## 10. Sequencing

Three plans, three branches, each merged before the next starts, each
following the working rhythm: worktree, subagent-driven tasks with a
ledger, review per task, one whole-branch review, all four CI checks
green on macOS and Windows, `--changed` mutation after every task, the
full harness at branch end.

**4a**

1. Data, part one — the grain vocabulary: carried dimensions through
   `carried_at`, the split's `any_value` payload and dependency check,
   routing, the grouping check and attribution, the load-time rules;
   the `categorical` flag with interning switched to it; the demo
   schema's three carried dimensions and its `textual` declarations.
   This touches everything `CLAUDE.md` names as mutation-mandatory, so
   it is its own task with its own harness entries and review before
   anything is built on it.
2. Data, part two: `Request::Distinct`; the ENUM rewrite with the
   bench cases added permanently and the gate met or the Rust-side
   fallback taken; `textual` validation. Mutation entries for each.
3. Core and frame: `ScopeSpec`; undo and redo stacks; `previous_as_of`;
   `scopebar::layout` replacing `readout`; session `[frame]`.
4. The bar: chips, the live text field, `mod+/`, the contradiction chip.
5. Pickers: the modal, `deliver_distinct`, `frame::pick` and the
   per-column actions.
6. As-of: the modal with presets, the stripe, the status segment,
   `frame::live` and undo.
7. Blotter: `:filter`, the `filtered` marker, session round-trip.
8. The flip barrier: frame state, blotter staging, deadline sweep.
9. Docs: `CLAUDE.md`, `docs/perf.md` with the text-filter table before
   and after, harness entries reconciled.

**4b**

1. `tracing` foundation: dependency, subscriber, targets, the `eprintln!`
   migration, `[log]`, the ring, files.
2. `Request::Catalog` with its DuckDB functions verified by execution.
3. The `Diagnostics` entity, the bridge feed, the status bar summary,
   `set_data_status` deleted.
4. The module crate: five sections, `:section`, `:level`, `:overlay`.
5. Panic boundaries: the ingest `catch_unwind`, the crash hook, the
   action tail.
6. Docs and harness.

**4c**

1. `Diagnostic.path` filled by every reader; `config_write` generalised
   from the three persist paths.
2. The editor crate: picker, open, validate, save, focus, `:copy user`,
   `ShellEvent::ReloadRejected`.
3. `ViewPresentationSpec`, the merge, the blotter's column commands and
   event handling, session-end write.
4. The reload prompt: swappable `DataHandle`, restart on the executor,
   the modal, the demo route.
5. Docs and harness.

## 11. Open questions

None block the plans. Recorded so they are not rediscovered:

1. **Subquery versus literal list for the ENUM rewrite** (§3.5). The
   plan measures both; the spec accepts either.
2. **Where the frame histogram lives.** §4.4 moves it into
   `Diagnostics` so the perf section and the overlay read one value. If
   the overlay's render path cannot cheaply read an entity, it keeps its
   own and the section reads the overlay's.
3. **Per-column pick actions on a wide schema.** A dataset with fifty
   categorical columns registers fifty palette entries. Acceptable at the
   demo's scale; a real desk schema decides whether a category filter
   in the palette is needed.
4. **The presentation merge and `ViewSpec` identity.** Merging over the
   view in the loader means `Config::explain` reports the view's layer,
   not the presentation's, for a width. The config section shows the
   presentation doc separately, which is enough for now.
