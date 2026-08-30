# Geode Phase 2 — Data Layer Design

Phase 2 builds `geode-data`: the `DataService` that ingests the desk's
risk files, stores them durably, and serves scope-framed, grouped,
joined snapshots to the UI inside the §7.1 budgets.

This document is subordinate to
`2026-08-28-geode-foundation-design.md` (referenced below as "the
foundation spec", with bare `§n` references pointing into it) and to
`docs/PHILOSOPHY.md`. Where it contradicts the foundation spec it says
so explicitly in §2 — those are deliberate amendments made in writing,
per the charter's own rule.

## 1. Scope

### 1.1 What Phase 2 delivers

- Persistent DuckDB storage: schema, live/archive split, retention.
- A CSV directory adapter with sentinel-gated readiness, multi-path
  sources, tolerant schema handling, and prioritized ingestion.
- Generations keyed to source time, per-file publication, per-book
  freshness, and as-of resolution over the archive.
- The view compiler: view definitions → SQL, with scope predicates,
  cross-dataset joins, and grain-aware grouping and aggregation.
- Immutable columnar snapshots delivered to the UI thread, with
  cancellation and latest-wins coalescing.
- A throwaway debug tile proving the end-to-end path on screen.
- A rebuilt `geode-demo-data` at the real data grain, and criterion
  benchmarks over ingest and requery.

### 1.2 Done state

Phase 2 is done when a running Geode, pointed at a directory of
generated risk files, paints real data in a tile; when a file landing
in that directory updates the tile without dropping a frame; when
as-of resolves to an older generation; and when the benchmarks show
requery inside §7.1's budget at 1M rows.

The debug tile (§7) is deliberately throwaway. It exists because the
§7.1 requery budget is specified end-to-end — "query + snapshot
handoff + first painted frame" — and cannot be measured headless. The
blotter deletes it in Phase 3.

### 1.3 Explicitly not in Phase 2

- The blotter, or any real module. No `Module` trait is built here.
- The scope bar, dimension pickers, and time-travel UI (Phase 4). The
  scope *model* and *compiler* are built; the interaction surface is
  not.
- Scenario datasets (spot ladders, bucketed vega/rho). The storage and
  ingestion pattern accommodates them; their schemas are not designed
  here.
- Diffing two as-of times (§4.5 defers it; persistence makes it
  cheaper, but it is still deferred).
- Streaming adapters, Parquet/Iceberg sources, the sidecar
  data-daemon split (§2).

## 2. Amendments to the foundation design

Six changes. Each arises from the real shape of the desk's data, which
was clarified after the foundation spec was written.

**2.1 Storage is persistent, not in-memory.** §5.3 specifies "DuckDB,
embedded, in-memory". It becomes a DuckDB database file on local disk,
which is the system of record. Rationale: history must survive
relaunch, and CSV ingest is slow enough that re-paying it on every
start is unacceptable. An in-memory hot tier remains available as a
*measured* optimization; it is not assumed, because a dual store
doubles the coherency surface for a win that has not been demonstrated.

**2.2 The spill tier is dropped.** §6's optional Parquet spill existed
to relieve memory pressure that persistence removes. If it ever
returns it will be to bound archive file size, not to keep live data
resident.

**2.3 Generations are published per file, not per dataset.** §5.1
describes a whole-dataset atomic swap and §6 a metadata-only rename of
live into archive. Neither survives contact with the real file layout:
`risk_snapshot` is fed by many files that land independently, some
carrying several books, some splitting one book across two. The
publication unit becomes the source file (§4.3), and the archive move
becomes a per-file copy rather than a free rename (§4.2).

**2.4 The dataset's as-of is derived, not primary.** Books refresh
independently, so freshness is tracked per file, rolled up per book,
and a dataset's headline as-of is the *oldest* book in the current
scope — the same stalest-input rule §5.4 applies across joins.

**2.5 The view compiler lives in the data layer.** §5.1 says the
blotter compiles view definitions to SQL. It does not: `geode-data`
does. Grain-aware aggregation (§6.3) is a correctness mechanism, not a
rendering concern, and it must be identical in every consumer. This
also makes it testable headless, which §10.3 prefers.

**2.6 Cross-dataset joins are day-one.** §5.4 defers join consumers to
a later phase. They are required now: the flagship blotter row joins
three grains of measures plus instrument reference data, and the
motivating vol-inline case joins a fourth dataset.

## 3. The data model

### 3.1 The desk hierarchy

Desks contain books. Books contain LHUs (Logical Hedge Units —
folders grouping positions within a book). LHUs contain positions. A
position is the unit of trading: it may have several legs, and trading
PnL and sales credit are position-level concepts. A leg is an
*instrument*, identified by `instrument_ref`. Naming is not
structural: a position `XYZ` often has legs `XYZa`, `XYZb`, but that
cannot be relied on.

An instrument has one or more underlyings, and almost always at least
two — even a mono-underlying derivative carries risk to the equity and
to the currency. Greeks are therefore per underlying per instrument.
Instruments carry contractual detail (strike, expiry). Instruments can
be reused across positions; positions can appear in multiple LHUs; a
position in one LHU can face multiple counterparties.

Single-underlying greeks are therefore keyed:

```
(book, lhu, position_ref, instrument_ref, underlying_ref, counterparty)
```

Cross-gamma is finer still: it is a property of an underlying *pair*,
not of an underlying. On a worst-of over SPX/RUT/NDX, SPX-RUT cross
gamma and SPX-NDX cross gamma are distinct numbers that only the pair
distinguishes. The most atomic risk grain is thus:

```
(book, lhu, position_ref, instrument_ref, underlying_ref,
 underlying2_ref, counterparty)
```

### 3.2 Grain is the organizing principle

Source files are flat, at the atomic underlying grain, so a measure
that belongs to a coarser grain is *repeated* across a row group. Any
`SUM` over such a column double-counts — by a factor of two for a
vanilla, more for a basket — and summed NPV per book is precisely what
the blotter's top-level rollup shows.

Two alternatives were rejected:

- **One wide table, coarse measures carried on one row and NULL
  elsewhere.** Sums work, no join needed, but the carried value
  disappears when a scope predicate excludes the carrier row — for
  example scoping to `underlying = SPX` when NPV was carried on the
  currency row. A correctness cliff triggered by an ordinary trader
  action, with no visible symptom.
- **One wide table with non-additive columns the compiler refuses to
  sum.** Honest, but it gives up rollups the desk actually needs.

Instead, ingestion **splits each source file by grain**, one target
table per distinct grain, deduplicating coarser measures. Every
measure declares its grain in config; adding a grain later is a config
change, not a schema migration. The four grains present today were
arrived at by successive correction during design — each correction
moving a measure or splitting a table — which is the strongest
argument for keeping the assignment declarative.

The cost is joins, and it is smaller than it looks: the compiler
aggregates each grain to the grouping key in its own subquery and
joins the *aggregates* — one row per group — never the raw tables
(§6.3).

### 3.3 Tables

Seven tables, named by grain rather than by measure family. `K` below
abbreviates the common leading key `(book, lhu, position_ref,
counterparty)`:

| Table | Grain |
|---|---|
| `measures_underlying_pair` | `K + (instrument_ref, underlying_ref, underlying2_ref)` |
| `measures_underlying` | `K + (instrument_ref, underlying_ref)` |
| `measures_instrument` | `K + (instrument_ref)` |
| `measures_position` | `K` |
| `instrument_ref` | `(instrument_ref)` |
| `implied_vol_surface` | `(underlying_ref, as_of, expiry, strike_or_moneyness)` |
| `implied_vol_summary` | `(underlying_ref, as_of)` |

**Pair canonicalization.** Cross gamma is symmetric — SPX-RUT and
RUT-SPX are the same second derivative — so if the source emits both
orderings, summing cross gamma over a book would double-count. Ingest
canonicalizes each pair to a sorted `(underlying_ref,
underlying2_ref)` and deduplicates, with a disagreement between the
two orderings' values reported through the §3.5 conflict detector
rather than silently averaged or dropped.

### 3.4 Measure assignment

Numeric suffixes on greeks are **bump sizes**, not underlying indices:
`Delta01/02/05` is the same greek at 1%, 2% and 5% shocks;
`Rho010` is a 10bp shock. Bump size stays a fixed column family rather
than a dimension — it is tiny, stable, and each variant is its own
blotter column. This is unlike §5.4's scenario data, which is
genuinely thousands wide and belongs in long form.

FX-converted `_USD` twins accompany the greek families and theta. The
NPV and PnL columns appeared without `_USD` twins in the supplied
column list; that asymmetry is assumed real and is one of the things
§10's real-header check should confirm.

**`measures_underlying_pair`** — `CrossGamma02`, `CrossGamma05`.

Cross gamma is the mixed second derivative across two underlyings, so
the pair is what identifies it (§3.1).

**`measures_underlying`** — `Delta01`, `Delta02`, `Delta05`,
`Gamma01`, `Gamma02`, `Gamma05`, `Vega01`, `NormalizedVega01`,
`Skew01`, `Rho010`, `RhoRFR010`, `RhoOIS010`.

Rho sits at underlying grain because rho's underlying *is* the
currency. RFR and OIS are curve variants of the same measure.

**`measures_instrument`** — `NPV`, `DailyPNL`, `DailyM2MPNL`,
`DailyFXPNL`, `CleanThetaBusinessDay`, `RealizedTheta`.

Theta is instrument-grain because it has no underlying: it is one
time-decay number per instrument.

**`measures_position`** — `DailyTradingPNL`, `SC`.

Trading PnL and sales credit are position-level by definition (§3.1).

### 3.5 Reference data and conflicts

`instrument_ref` has no upstream source on day one. It is derived at
ingest by deduplicating instrument attributes — `Strike`, `Expiry`,
`Currency`, `ModelCode`, underlying refs — out of the risk files. It
is designed to be fed by a real source later with no change visible to
consumers.

Because instruments are reused across positions and books, and files
are per book, the same `instrument_ref` arrives from many files whose
attribute values may disagree. Three causes, one mechanism:

1. **Staleness** — one file is simply older. Resolution: newest source
   time wins.
2. **Genuine upstream inconsistency** — two systems disagree. Geode
   surfaces it in diagnostics naming the disagreeing files; it does
   not paper over it.
3. **A wrong grain assignment on our side** — a column that varies
   systematically is evidence it is not an instrument property at all.

The same detector applies to coarse measures: `DailyPNL` is repeated
across an instrument's underlying rows and must be identical on every
one. A disagreement is the same signal. Conflict counts per column are
a first-class diagnostic, precisely because the grain assignments
in §3.4 are informed guesses.

### 3.6 Schema policy

Schemas are declared in config: names, types, textual-for-search
flags, formatting hints, **and grain**. §5.2's tolerance splits in two,
because columns legitimately differ across files (not every book is
run with every greek):

- A column declared **optional** and absent is recorded per file and
  visible in diagnostics, without a warning. Absence is expected.
- A column declared **required** and absent is a health warning naming
  the file.
- Extra source columns pass through.

Dimension columns are stored as DuckDB `ENUM` types, which is §7.2's
"interned at ingest" and lets the renderer compare and format on small
integer codes (§6.6 — a plain `VARCHAR` would not be dictionary-encoded
on the way out). Because an ENUM's value set is fixed at declaration,
ingest widens it when a new dimension value appears, which is a
metadata operation on the small dimension vocabularies this applies to.

### 3.7 The implied vol datasets

`implied_vol_surface` holds the full surface in long form. Its columns
and source are not yet specified; the join key to risk data is
`underlying_ref`, which is coarser than instrument grain.

`implied_vol_summary` holds a handful of scalars per underlying and is
what the blotter joins to for inline columns, so no point-selection
rule is needed at query time.

**Charter constraint.** PHILOSOPHY §1 names "a vol surface
interpolation" as the archetype of what belongs upstream. Deriving the
summary from the surface is therefore permitted only as *selection* —
taking the value at a listed ATM strike and listed expiry, which is
filtering and thus view-shaping. If a constant-maturity or interpolated
ATM value is wanted, it must be produced upstream and ingested, or
PHILOSOPHY §1 must be amended deliberately and in writing. Preference
is to ingest a summary upstream publishes.

## 4. Storage and persistence

### 4.1 One database file

A single DuckDB database file in the platform application data
directory — `~/Library/Application Support/Geode/geode.duckdb` on
macOS, `%LOCALAPPDATA%\Geode\` on Windows — overridable in config.

Readers and the writer are separate `Connection`s on one shared
`Database` within the process. Per §5.3 the pool of read connections
serves view queries and one dedicated writer connection serves ingest.

### 4.2 Live and archive

Per dataset, two tables:

- **`<dataset>_live`** — exactly the current rows for every file
  partition. No generation column, no history predicate, size
  independent of retention. This is the property §6 was buying, and it
  is what keeps §7.1's requery budget reachable by construction.
- **`<dataset>_archive`** — append-only, every row stamped with its
  `gen_id`. Touched only by as-of queries and the retention sweeper.

The metadata-only rename of §6 is not available under per-file
publication, so the archive move is a copy. It is proportional to one
file's rows, not the dataset's, and runs on the ingest thread well off
the UI path.

### 4.3 The publication unit is the file

Because a file may carry several books and a book may be split across
files, the replacement key is `source_file_id`, not `book`.
Publishing file F atomically replaces exactly the rows previously
loaded from F. This handles both awkward cases with no waiting and no
configured file-to-book mapping — discovery stays automatic.

One transaction on the writer connection:

```
INSERT INTO <ds>_archive SELECT *, <current_gen> FROM <ds>_live
    WHERE source_file_id = F;
DELETE FROM <ds>_live WHERE source_file_id = F;
INSERT INTO <ds>_live SELECT * FROM <staging>;
```

Publishes serialize; one at a time through the single writer.

### 4.4 Generations, source time, and the backfill guard

A generation is identified by its **source time**, taken from the
sentinel (§5.3) — never from ingest order, and never from mtime, which
any copy, restore or archive extraction corrupts. Historical backfill
(§5.4) loads old files after new ones, so ingest order carries no
information.

Two consequences:

- As-of resolution orders by source time throughout: the newest
  generation at or before T, per file partition.
- **The publish rule is guarded.** A file's rows enter `live` only if
  its source time is newer than what live currently holds for that
  partition. Otherwise they are written straight to `archive`. Without
  this guard a backfill would silently overwrite this morning's risk
  with last Tuesday's.

### 4.5 Freshness metadata

A `file_generations` table records, per source file: file id, path,
size, mtime, sentinel source time, `gen_id`, loaded-at, row count, and
health state.

Freshness rolls up: a book's freshness is the oldest of its
contributing files; a dataset's headline as-of is the oldest book in
the effective scope. Any tile mixing datasets or cadences shows
per-dataset freshness, never one misleading timestamp (§5.4).

### 4.6 Retention

Per-dataset config, by count and/or age, enforced over the archive by
a background sweeper. Bounded by disk rather than RAM, so defaults are
generous. Evictions are logged, and the oldest available source time
per dataset is published for the time-travel UI.

The sweeper also owns DuckDB checkpoint scheduling, since a checkpoint
can stall the writer and must not land mid-refresh.

## 5. Ingestion

No crate other than `geode-data` opens a file or a socket (§2). All of
this lives behind `DataService`.

### 5.1 Sources and discovery

A source is config-declared: adapter, **a list of directory globs**, a
refresh interval, a readiness strategy, a priority, and a column map
from source names to dataset schema names. Multiple sources may feed
one dataset — the federation seam of §5.1 — with rows carrying a
`source` column and conflicts resolved by declared precedence.

Discovery polls on the configured interval. Filesystem watches are not
used: `notify` is unreliable over SMB, which §11 already flags, and
polling degrades honestly where watches fail silently. Every read
carries a timeout and runs on a thread nothing waits on.

Change detection is `(size, mtime)` of the CSV and its sentinel,
recorded in `file_generations`. Config exposes a content-hash mode for
sources where timestamps cannot be trusted.

### 5.2 Readiness

The primary readiness strategy is the **sentinel**. Risk files are
accompanied by `<name>.done`, written when the CSV is complete. A CSV
is eligible only when its sentinel exists and is at least as new as
the CSV.

The sentinel is read first and always. It is small, and its declared
column list allows schema validation *before* opening a
multi-hundred-megabyte CSV — drift is caught in milliseconds rather
than after a long parse.

States are honest rather than binary:

- CSV without sentinel: **pending**, not broken. After a configurable
  timeout it becomes "pending too long" in source health.
- Sentinel without CSV: an error.
- Sentinel older than its CSV: pending — the file is being rewritten.

The fallback strategy, declared per source for sources with no
sentinel convention (vol data, and instrument reference once it has an
upstream), requires `(size, mtime)` stable across two consecutive
polls before reading.

### 5.3 The sentinel document

JSON. The adapter **requires only two fields — source time and the
expected column list — ignores every field it does not recognise, and
reports a missing required field as a health error naming the file.**
The real shape may therefore differ from the mock below in every
respect but those two without breaking anything.

The exact production shape is an open prerequisite (§10). The mock
`geode-demo-data` emits, and which the tests are written against:

```json
{
  "dataset": "risk_snapshot",
  "as_of": "2026-08-30T14:32:05Z",
  "business_date": "2026-08-30",
  "books": ["BK003", "BK011"],
  "row_count": 184203,
  "columns": ["Book", "LHU", "PositionRef", "..."]
}
```

`row_count` and `books` are guesses worth keeping even as guesses: a
declared row count makes truncated-file detection free, and a declared
book list lets the cold-start planner order work without opening a
single CSV.

### 5.4 Priority ladder and cold start

Ingest is a priority queue, not a sweep. Not all files are equal and
the desk has strong priors about what it wants to see first.

Startup performs a **sentinel-only scan** across all configured paths
— cheap, no CSV opened — and builds a work plan, worked in
config-declared priority order. Defaults:

1. Latest generation per book of the risk datasets. Current risk on
   screen first.
2. Latest generation of everything else: vol, instrument reference,
   scenario data when it exists.
3. Historical backfill of older files not already in the database,
   bounded by the retention horizon.

Backfill is **chunked and preemptible**: a newly landed live file
jumps the queue at the next chunk boundary, so a long historical load
can never starve the foreground.

A warm start reads no CSV at all before the first sweep completes:
live tables are queryable the moment the database opens, so tiles
paint real data well inside §7.1's one-second startup budget. A cold
start is a full ingest with honest progress, the shell interactive
throughout, tiles showing loading state.

### 5.5 Load pipeline

Per eligible file:

1. Parse the sentinel; validate its column list against the declared
   schema; resolve source time.
2. `read_csv` into a staging table. DuckDB's native reader does the
   parsing — it is multi-threaded and keeps the ingest loop small.
3. Apply the source's column map in the projection.
4. Split by grain into the target shapes (§3.2), deduplicating coarser
   measures and recording disagreements (§3.5).
5. Publish per §4.3, subject to the backfill guard of §4.4.

Step 5 always runs on the single writer connection. Steps 1–4 run on
an ingest worker, but staging is itself a write, so whether workers
can stage concurrently on their own connections or must queue behind
the writer is the open concurrency question of §5.6 — the pipeline is
correct either way, and only throughput differs.

Publication broadcasts `(dataset, file, gen)` over a bounded channel
into gpui's async runtime; the shell requeries visible tiles. Bounded
means a storm of refreshes coalesces rather than queues — latest-wins,
the same discipline as §7.3's query coalescing.

### 5.6 Parallelism

A bounded worker pool (`ingest.workers`) parses and stages in
parallel; publish transactions serialize through the single writer per
§5.3. Whether additional concurrent writer connections buy anything is
a benchmark question and is deliberately not assumed here.

§7.1's contract stands: a background refresh of any size may never
drop a foreground frame. Ingest is off-thread, publish transactions
are short, and readers hold their own connections.

### 5.7 Failure and health

Per §10.1, data problems are never modal and never fatal. A load that
fails at any step leaves `live` untouched and degrades that file's
health. The last good generation stays live.

Ingest tasks run behind a `catch_unwind` boundary: a panicking task
restarts, logs loudly, and marks its source degraded.

Health is per file, rolled up per book, per dataset, and per source,
and is the diagnostics module's raw material in Phase 4.

## 6. Query path

### 6.1 View definitions

Declarative config (§5.1): datasets and joins, columns, derived
columns as SQL expressions, default grouping and aggregations, sort,
and formats. Users create views through the UI or by writing config;
both produce the same file (PHILOSOPHY §5).

The compiler folds the effective scope in and emits one statement.

### 6.2 Scope: state, type, compiler

Scope *state* lives in the shell (§4.3); the scope *compiler* lives in
`geode-data`; and those two crates may never depend on each other
(§2). Therefore the scope **value type** lives in `geode-core`:
dimension selections, the text filter, and the parsed expression AST.

The expression parser lives in `geode-core` too — pure, window-free,
and the natural home for §10.3's property tests that generated scope
stacks always compose to valid, correct SQL. It parses a restricted
WHERE-clause grammar validated against the schema, not raw SQL.

The three predicate kinds of §4.1 compile as:

- **Dimension selections** — bound list parameters.
- **Text filter** — an OR of `ILIKE` predicates over columns declared
  textual.
- **Expression filter** — the validated AST lowered to SQL.

Values are **bound as parameters, never spliced into SQL text**.

**duckdb-rs cannot bind a list parameter** — `Value::List` binding is
explicitly an error, verified against 1.10505. So a dimension selection
of several hundred books uses a **temp table plus semi-join**: the
selection is written to a connection-local temp table through the
Appender and the predicate becomes `book in (select v from
scope_book)`. Values stay bound, the statement text is stable
regardless of selection size so the prepared plan stays cacheable, and
it scales past any placeholder limit.

Two alternatives were measured to agree exactly and are kept as
fallbacks: generating `in (?, ?, …)` with one placeholder per value
(simple, but statement text varies with N, defeating plan caching), and
binding one delimiter-joined varchar unpacked by `string_split`
(stable text, one parameter, but it fails on values containing the
delimiter).

Layering (§4.2) is resolved in the shell: global AND workspace AND
tile, with unscoped tiles opting out. The compiler receives one
effective scope.

### 6.3 Grain-aware aggregation

The compiler's central job. For a grouping tuple G and a set of
requested measures spanning several grains, each grain is aggregated
to G in its own subquery, and the *aggregates* are joined
(illustrative — `G` stands for the grouping tuple):

```
select ... from
  (select G, sum(delta01) ... from measures_underlying
       where <scope> group by G) u
  full join (select G, sum(npv) ... from measures_instrument
       where <scope> group by G) i using (G)
  full join (select G, sum(daily_trading_pnl) ... from
       measures_position where <scope> group by G) p using (G)
```

Summing a position-grain measure therefore never sees the
underlying-level row fan-out. Double-counting is impossible by
construction rather than by discipline, and the join is at group
cardinality — tens or hundreds of rows — not at table cardinality.

Two distinct mismatches arise between a measure's grain and the rest
of the query. They get different answers.

**Grouping mismatched with the measure's grain.** Two properties,
evaluated per grouping tuple, not per view:

- **Additive** — every measure row belongs to exactly one group. Safe
  to sum; children total to their parent.
- **Determined** — the group identifies a unique measure key, so a
  value exists for the row, but sibling groups repeat it and children
  do not total to their parent.

That yields three states, carried per column per level as
`Attribution`:

| State | Condition | Cell |
|---|---|---|
| `Additive` | grouping is a function of the measure's key | the aggregate |
| `DeterminedNonAdditive` | grouping determines a unique measure key but repeats it across siblings | the value, marked |
| `NonAttributable` | neither | NULL, marked |

Worked example — grouping `lhu > underlying > position`, showing
trading PnL (position grain):

| Level | Group tuple | State |
|---|---|---|
| 1 | `lhu` | `Additive` — every position sits in exactly one LHU |
| 2 | `lhu, underlying` | `NonAttributable` — a position has several underlyings |
| 3 | `lhu, underlying, position` | `DeterminedNonAdditive` — the position is identified, but repeats across its underlying siblings under a blank parent |

Greeks, being underlying-grain, are `Additive` at all three levels of
the same grouping. It is only measures coarser than the grouping path
that degrade, and they can recover at a deeper level, as level 3 shows.

`NonAttributable` never invents an allocation: attributing a
position's trading PnL to one of its underlyings is financial
reasoning and belongs upstream (PHILOSOPHY §1).
`DeterminedNonAdditive` shows the real number with a marker meaning
*do not total this column*, because a trader who has drilled to a
position row should see that position's PnL. The marker must be
unmistakable — under a blank parent, an unmarked repeated value reads
as a bug. Rendering it is Phase 3's; computing it is Phase 2's.

Rejected: showing the value on one arbitrary sibling and blanking the
rest so the visible column totals. It reintroduces the carrier-row
failure of §3.2 — the number vanishes when scope excludes that
sibling.

**Scope finer than the measure's grain** — scoping to one underlying
while showing trading PnL. Here a well-defined answer does exist, and
it is neither ignoring the predicate nor blanking the column. The
compiler splits the effective scope by the finest grain each predicate
references. Predicates naming columns present at the subquery's grain
apply directly; finer ones apply as a **semi-join** against the grain
where those columns do exist:

```sql
select K, sum(daily_trading_pnl) from measures_position mp
where <predicates available at position grain>
  and exists (select 1 from measures_underlying mu
              where mu.<K> = mp.<K>
                and <finer predicates>)
group by K
```

The resulting number means "trading PnL of positions that have SPX
risk". It is emphatically not "the SPX share of trading PnL", and set
beside greeks in the same row — which genuinely are the SPX portion —
that distinction is exactly the kind of ambiguity PHILOSOPHY §3
forbids leaving unmarked. So the column carries `SemiJoined` naming
the dimensions applied by membership rather than directly.

The same mechanism covers scope predicates over `instrument_ref`
attributes such as `ModelCode`: direct at instrument grain and finer,
semi-join at position grain.

Semi-join is the default because it is what a trader usually means,
but it is a per-view, per-measure config choice — `semi_join`,
`unscoped`, or `blank` — since a desk-summary view may legitimately
want the unscoped total.

**Pair measures aggregated to a coarser grouping need a declared
rule.** Rolling cross gamma up by `underlying_ref` is ambiguous:
canonicalization (§3.3) means an SPX-RUT pair would otherwise land
only under whichever name sorts first, which is arbitrary. Cross gamma
is therefore `NonAttributable` at an underlying-level grouping unless
the view definition declares a rule — attribute to both sides, or to
neither. It is `Additive` at groupings at or coarser than instrument,
which is the common case.

**The tree is one query, not one query per node.** A blotter grouping
is a hierarchy, and every level is a prefix of the grouping tuple, so
the compiler emits a single `ROLLUP(lhu, underlying, position_ref)`
returning all levels in one result, with `GROUPING()` identifying the
level of each row. `Attribution` is computed per level from the schema
alone and travels with the snapshot.

The consequence matters more than the mechanism: expanding and
collapsing become pure UI operations against data already in hand —
§7.1's <8ms budget — rather than a requery per node against the 50ms
one. Lazy per-node queries remain the fallback if level cardinality
ever explodes on a deep grouping, which is a benchmark question, not
an assumption.

### 6.4 Joins

Join keys are declared in schema config. Day-one joins:
`measures_* ⋈ instrument_ref` on `instrument_ref`, and
`measures_underlying ⋈ implied_vol_summary` on `underlying_ref`.

A joined view is as stale as its stalest input (§5.4), and per-dataset
freshness travels with the snapshot so no tile can show one
misleading timestamp.

### 6.5 As-of routing

The same compiled SQL, aimed at archive tables, with the generation
per file partition resolved as the newest at or before the requested
time (§4.4). Because datasets and books refresh independently, the
resolved state is "each partition as it stood at T" — the question a
trader is actually asking (§4.5).

The live path carries no generation predicate at all; only the as-of
path pays for history.

### 6.6 Snapshots

Results come back from DuckDB as Arrow record batches, zero-copy,
wrapped in a `Snapshot` newtype in `geode-core` exposing typed column
accessors — `f64_column(name) -> &[f64]`, dictionary columns as codes
plus dictionary.

**`query_arrow` yields many batches, not one** — 2048 rows each, so a
50k-row result arrives as 25 batches. A `&[f64]` spanning the whole
column therefore requires concatenation. `Snapshot` concatenates once
at construction, because blotter results are *aggregates* — one row per
visible group, typically tens to thousands — while the million rows are
scanned inside DuckDB and never cross the boundary. For the rare
ungrouped large result, chunked accessors remain available and the
concatenation is skipped.

**Dictionary encoding is not automatic.** A plain `VARCHAR` column
arrives as `StringArray`; only a column typed as a DuckDB `ENUM` comes
back `Dictionary(UInt8, Utf8)`. §3.6's interning therefore requires
declaring ENUM types for dimension columns at ingest, which is a
storage decision, not a query-time one.

Arrow stays an implementation detail: modules get a stable API and
take no Arrow dependency, while §7.2's `Arc`-shared pointer-swap
handoff and interned dimension strings come free. No row objects are
materialized anywhere.

A snapshot carries its data plus provenance: per-dataset generations
and source times, health state, and two orthogonal per-column markers
from §6.3 — `ScopeSemantics` (`Direct` or `SemiJoined { dimensions }`,
how the scope was applied) and `Attribution` (`Additive`,
`DeterminedNonAdditive`, or `NonAttributable`, per grouping level).
Phase 2 computes both; Phase 3 decides how they are drawn. The debug
tile prints them verbatim.

### 6.7 Concurrency and cancellation

Per §7.3:

- A pool of read connections serves queries; the UI thread never holds
  one.
- One in-flight query per view, latest-wins coalescing. Leaning on a
  regroup key five times yields one query.
- Every request and result is generation-tagged; a stale result
  arriving after a newer request is dropped, never rendered.
- A superseded query is interrupted, not awaited.
- Channels are bounded; backpressure surfaces as source health, never
  as UI stall.

## 7. The vertical slice

A shell-owned debug tile, explicitly labelled throwaway, showing:
configured datasets, per-book freshness and generation, row counts,
source health, and a raw preview of the first N rows of a view.

It exists to measure input-to-painted-frame honestly and to force the
`DataService` API to be designed against a real consumer. It is not a
module, does not use a `Module` trait (none exists yet), and is
deleted when the blotter lands.

## 8. Crate layout

```
geode-core   scope model + expression parser, schema and grain
             declarations, dataset/view config model, Snapshot type,
             ids, errors
geode-data   sources, CSV adapter, sentinel parsing, ingest queue,
             store (live/archive/retention), view compiler, query
             pool, health
geode-shell  scope/as-of state; the throwaway debug tile
```

`shell` and `data` never depend on each other. `geode-app` remains the
only crate where everything meets. `geode-data` is the only crate that
opens a file.

## 9. Testing and benchmarks

Test weight goes data layer ≫ shell logic ≫ modules (§10.3). Phase 2
is the data layer, so this is the heaviest testing phase so far. TDD
applies per house rules.

### 9.1 Rebuilding `geode-demo-data`

The current generator emits a flat toy schema
(`book/desk/model_code/underlying/instrument` with scalar greeks) that
does not resemble the real data. It feeds the benchmarks, so it is
rebuilt or the benchmarks measure the wrong thing.

It must emit a realistic **source directory**, deterministic from a
seed and capable of 1M rows:

- Per-book CSVs at the real atomic grain, with the measure families
  of §3.4 — some files carrying several books, one book split across
  two files.
- Multi-underlying instruments including at least one three-underlying
  worst-of, so pair rows, pair canonicalization, and the
  underlying/pair grain split are exercised rather than assumed.
- Optional greek columns absent from some files, so §3.6's tolerance
  is exercised by fixtures rather than only by unit tests.
- A `.done` sentinel per CSV, plus at least one CSV with no sentinel
  yet, so readiness logic is exercised too.
- Multiple business dates, so backfill and as-of have something to
  resolve.
- Instrument attribute disagreements across files, so §3.5's conflict
  detector is exercised.

No checked-in fixture files (§7.4).

### 9.2 Tests

- **Round-trip integration:** adapter → DuckDB → view compiler →
  snapshot, over generated fixtures.
- **Generation semantics:** per-file publication, the backfill guard,
  derived book and dataset freshness, retention eviction, as-of
  resolution across mixed cadences.
- **Schema drift:** optional-absent, required-absent, extra columns,
  sentinel/CSV column disagreement.
- **Conflict detection:** disagreeing instrument attributes and
  disagreeing repeated coarse measures.
- **Readiness:** pending, pending-too-long, orphan sentinel, rewritten
  CSV, and the two-poll fallback.
- **Grain correctness:** the property that matters most — aggregating
  a coarse measure over any grouping and any scope never
  double-counts. Property-tested.
- **Grain mismatch semantics (§6.3):** over a multi-level grouping,
  every level gets the right `Attribution` — the `lhu > underlying >
  position` case must come back `Additive`, `NonAttributable`,
  `DeterminedNonAdditive` in that order, with the leaf carrying the
  position's true PnL and level 2 carrying NULL. A scope finer than a
  measure's grain yields the semi-joined total marked `SemiJoined`
  with the right dimensions named; the `unscoped` and `blank`
  per-view overrides do what they say. Asserted on values, not only on
  markers — a semi-join that silently ignored its predicate would
  otherwise pass.
- **Rollup shape:** a `ROLLUP` query returns every level exactly once,
  `GROUPING()` identifies levels correctly, and children of an
  `Additive` parent sum to it.
- **Scope compilation:** property tests that generated scope stacks
  compose to valid, correct SQL (§10.3), plus parser error cases with
  caret positions.
- **The slice:** gpui `TestAppContext`.

### 9.3 Benchmarks

Criterion, over the generated data, CI-gated on regression (§7.4):

- Ingest throughput per file size, and full cold-start wall time.
- Requery at 1M rows: grouped, joined across four grains, scoped —
  the shape the app actually runs. Budget < 50ms end-to-end (§7.1).
- Snapshot handoff cost.
- As-of query against archive versus the same query against live.

### 9.4 CI risk

The `duckdb` crate's `bundled` feature compiles DuckDB from C++
source. **Measured on an M-series Mac at duckdb 1.10505: 127s wall,
1382s CPU** for a clean build — so on a CI runner with fewer cores,
expect several times the wall time. It is the right choice — no
system-install dependency, reproducible from `Cargo.lock` — but
`libduckdb-sys` must be cached across CI runs or every push pays it
twice (macOS and Windows). Caching is an explicit task in the
implementation plan, not an afterthought.

The API assumptions above were verified against a real bundled build
before the plan was written: persistent database with a second
connection via `try_clone`, `read_csv` at 37ms for 50k rows,
`query_arrow`, zero-copy `&[f64]`, `ROLLUP` with `GROUPING()`, a
`Send + Sync` `interrupt_handle()`, and the publish transaction of §4.3
at 3ms. Three assumptions failed and are corrected above (§6.2 list
binding, §6.6 batch chunking, §3.6 ENUM dictionary encoding).

## 10. Open questions and prerequisites

Blocking implementation, not the spec:

1. **A real `.done` sample.** The adapter is written against the mock
   of §5.3 and the two required fields; a sample confirms field names
   and the source-time format.
2. **A real CSV header.** The column list in §3.4 came from a sketch
   plus corrections. The schema is config-declared, so corrections are
   cheap, but the initial declaration should be written against a real
   header.
3. **How the file lays out pair rows.** Either the source emits one
   row per underlying with `underlying2_ref` populated only on cross
   rows, or one row per pair with the single-underlying greeks
   repeated across an instrument's pairs. The target tables are the
   same either way; the ingest deduplication differs, and a sample
   settles it. Whether both pair orderings appear is part of the same
   question (§3.3).

Non-blocking, resolvable in config:

4. `Rho010_USD` was absent from the supplied column list while
   `RhoRFR010_USD` appeared twice; assumed a slip and the family
   assumed symmetric.
5. The desk-to-book mapping has no source — `Desk` is not a CSV
   column. Config-declared for now.
6. `implied_vol_surface`'s columns and source are unspecified (§3.7).
7. Whether concurrent writer connections improve ingest throughput
   (§5.6) — a benchmark question.
