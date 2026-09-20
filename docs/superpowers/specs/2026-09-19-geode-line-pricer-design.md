# Geode — Line Pricer

**Date:** 2026-09-19
**Status:** Approved in brainstorm; implementation plan to follow
**Governs:** the line pricer module (`geode-pricer`), the pricing
library seam (`geode-pricing`, the `Pricer` trait, the mock), the
data tier's pricing worker and local publish request, and the first
written exception to the charter's "lens, not a brain" rule. It is
roadmap sub-project G, slice 1, taken ahead of slices 2 and 3 (see
§3).
**Conforms to:** `docs/PHILOSOPHY.md` as amended by §2.1 of this
document, the foundation design
(`2026-08-28-geode-foundation-design.md`), the modules roadmap
(`2026-09-12-geode-modules-roadmap.md`), and the market-data
documents design (`2026-09-12-geode-market-data-documents-design.md`)
and its panel-header follow-up
(`2026-09-14-geode-market-data-panel-header-design.md`), whose module
conventions (insert mode, `holds_focus`, blur-then-drop, the
`DataTable` delegate, keymap fragments) this module reuses verbatim.

## 1. Scope

### 1.1 What this delivers

A tile in which each row is an option instrument, rows group into
packages, and every row shows a price and greeks answered by a pricing
library linked into the binary:

- a **sheet** of lines, typed in as one-line shorthand
  (`-5 SPX DEC26 95%/105% CS`), edited cell by cell, grouped into
  packages whose rows sum their legs;
- two instrument variants, **vanilla** and **barrier**, and seven
  package templates (§6.3), with the row model shaped so that a
  multi-underlying instrument's per-underlying children are a new row
  kind in slice 2, not a restructure;
- **shifts**: spot (%) and vol (points) per line, with a sheet-wide
  value inherited by any line that has none;
- **views**: named column sets from a `pricer_views` config doc over
  a fixed column vocabulary, two bundled;
- **automatic repricing** on every change, marked stale until the
  answer lands, plus a periodic refresh and a `:price` verb;
- **sheets that persist without a save verb**: every sheet is a
  document in a builtin local dataset in DuckDB, written behind the
  tile's in-memory truth, restored with the session, browsable as-of;
- a **pricing seam** the real library drops into later: one
  synchronous trait, a deterministic mock now, the real crate as a
  feature-gated leaf crate CI never builds.

### 1.2 What this deliberately leaves out

- Multi-underlying instruments and per-underlying breakdown rows
  (slice 2; the row model reserves the kind).
- Rate and dividend shifts, theta roll, listed-price columns.
- Nemo file import, scenario output, launch-with-context to the CVI,
  repo or dividend panels (roadmap slice 2 mechanisms).
- A pricer object dialog for `pricer_views`; a presentation doc for
  column widths (columns are not resizable, the market-data panel's
  known gap).
- Storing pricing results as data. Only definitions are persisted;
  a reopened sheet reprices.
- A pricing request from any tile but the pricer.

### 1.3 Terms

- **Line**: one instrument row with its own price request.
- **Package**: a group row whose legs are lines; it has no instrument
  and no request, and its numbers are sums of its legs' (§6.4).
- **Sheet**: the ordered rows of one pricer tile plus its sheet-wide
  shifts, view and refresh interval; identified by name.
- **Revision**: a per-line counter bumped by every edit that changes
  the line's request; a result carries the revision it answered.
- **Shorthand**: the one-line grammar of §6.3, both parsed and
  rendered.

## 2. Rulings

Decided in the 2026-09-19 brainstorm; binding on the plan.

1. **The pricing library is called per instrument, synchronously,
   and is self-contained, with an overridable data source.** It
   fetches its own market data through a `PricingDataSource` instance
   the library provides and the app may override — spot levels now,
   CVI and dividend documents later (user ruling 2026-09-19, during
   Part 1). Geode assembles no surface, resolves no percent strike and
   no tenor. `Strike::Percent` and `Expiry::Tenor` pass through the
   seam untouched. **Overrides are stateful on the library's instance**
   (a second user ruling the same day, over a per-request field):
   `Pricer::set_overrides(&MarketOverrides)` then `price`, and the
   worker makes the pair batch-scoped — set once per batch, then every
   line of that batch — so two tiles sharing the worker can never see
   each other's overrides, since the worker prices one batch at a
   time.
2. **A pricing request is a data-tier request answered through the
   one delivery door.** `DataHandle::price` → a pricing worker inside
   `DataService` → `DataEvent::Price` → `Delivery::Price`, routed by
   key like every query. Rejected: a module-owned worker thread
   (a second place a vendor dependency lives, and a delivery outside
   the door the diagnostics tile watches).
3. **Sheets live in DuckDB as documents in a local dataset, behind a
   `SheetStore` seam.** Rejected: named TOML files under the user
   config directory (a module reading its own files needs a read door
   the shell has no precedent for, and the migration to DuckDB would
   leave two dead doors) and session-only storage (a closed tile
   would lose its sheet). The seam stays so the tile's tests run
   against an in-memory fake and so the store could be swapped.
4. **No save verb.** A sheet is written automatically after every
   edit burst. `:name`, `:e`, `:new` and `:rm` are the whole sheet
   vocabulary; nothing is ever dirty.
5. **Lines and packages now, the tree shaped for per-underlying
   children.** `RowKind` has `Line` and `Package` in slice 1 and
   `Underlying` reserved; depth is at most two now and three later.
6. **Shorthand to create, cells to adjust.** `o` opens a one-line
   field whose text parses to a line or a whole package; `i` edits a
   cell with the market-data insert machinery.
7. **Views are a config doc over a code vocabulary.** `pricer_views`
   names columns from `core::columns`; presentation keys are the
   blotter's (`label`, `width`, `format`).
8. **Shifts are per line, spot and vol, with a sheet-wide inherited
   value.** An own value wins; an inherited one paints muted.
9. **Reprice automatically, and on a timer.** Every request-changing
   edit reprices its line; `:refresh` sets an interval; `:price`
   reprices everything.
10. **The charter changes in writing** (§2.1), as a general rule
    rather than a one-off exception.

### 2.1 Charter amendment

`docs/PHILOSOPHY.md` §1 gains this paragraph after "This line is the
load-bearing wall of the design":

> **In-process calculation.** A calculation may live inside the
> binary only as a leaf crate: one with no dependency on the shell, a
> module or the data crate, reached through the same request-and-
> outcome door a remote service would use (a keyed request in, a keyed
> outcome back, never a direct call from a module), so that it can be
> moved out of the process without a caller changing. Such a crate is
> upstream intelligence that happens to be linked in — a microservice
> that lives in our binary — and the rest of the app treats it
> exactly as it treats any other upstream: it sends definitions and
> shows what comes back. The pricing library behind the line pricer
> (`geode-pricing`) is the first; the rule is the pattern for the
> next. The app itself still performs no financial arithmetic: a
> package row summing its legs' returned numbers is aggregation, and
> nothing in a module interpolates, solves or converts.

## 3. Amendments to earlier designs

- **Roadmap §5 G and §6 item 4.** The pricer is built now, ahead of
  slices 2 and 3, and without link groups, launch context or the
  scenario panel; the pieces of G that need them (scenario output,
  launch to market-data panels) wait for slice 2. The "local" adapter
  shape of roadmap §3 is built here as the local publish request
  (§5.3), by the module that needs it.
- **Roadmap §4, "Egress".** `Delivery` gains `Price` before it gains
  `Upload`; Part 4 egress takes the identical shape (a keyed request,
  a keyed outcome, one delivery variant).
- **Market-data documents design §8.6.** The blur-then-drop rule and
  `holds_focus` ownership rule apply to this module's two inputs (the
  entry field and the cell editor) unchanged; the design's "a second
  module wanting text entry (the pricer) inherits the rule unchanged"
  is now the case.
- **`geode_core::log::TARGETS`** grows from six to seven with
  `geode::pricing` (§10.2).

## 4. Crates and layering

```
geode-app          wires the roster, registers the mock pricer, bridges DataEvent::Price
  ├─ geode-pricer  the module: pure core + PricerTile (depends on shell, pricing, core)
  ├─ geode-data    the pricing worker and the local publish request (depends on pricing)
  └─ geode-pricing the seam: instrument vocabulary, Pricer trait, MockPricer (depends on core only)
```

`geode-pricing` is a leaf under §2.1: it depends on `geode-core`
alone, never on the shell, a module or the data crate. The real
library arrives as `geode-pricing-<vendor>`, feature-gated, a peer of
`geode-pricing` that implements its trait; CI never builds it, and a
config naming a pricer the running binary lacks fails every line with
that reason (§10.1), never startup. Both new crates carry
`bench = false` on their lib targets.

## 5. The pricing seam

### 5.1 Vocabulary (`geode-pricing`)

```rust
pub enum Instrument {
    Vanilla(Vanilla),
    Barrier(Barrier),
}
pub struct Vanilla { pub underlying: String, pub expiry: Expiry, pub strike: Strike, pub kind: OptionKind }
pub struct Barrier { pub vanilla: Vanilla, pub level: f64, pub barrier: BarrierKind }
pub enum OptionKind { Call, Put }
pub enum BarrierKind { UpIn, UpOut, DownIn, DownOut }
pub enum Expiry { Date(NaiveDate), Tenor(String) }   // "3m", "6w", "1y" — validated as <n><d|w|m|y>, never resolved here
pub enum Strike { Absolute(f64), Percent(f64) }      // 95% is Percent(95.0)
pub struct Shifts { pub spot_pct: f64, pub vol_pts: f64 }
pub struct PriceRequest { pub instrument: Instrument, pub shifts: Shifts }
pub struct PriceResult { pub price: f64, pub delta: f64, pub gamma: f64, pub vega: f64, pub theta: f64, pub rho: f64 }
pub struct PricingError(pub String);
/// What the app overrides in the library's `PricingDataSource` (ruling 1):
/// spot levels by underlying now; CVI and dividend documents are later
/// fields. Plain data — the library interprets it, the app never does.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MarketOverrides { pub spot: BTreeMap<String, f64> }
pub trait Pricer: Send + Sync {
    fn name(&self) -> &str;
    /// Replace the overridable market data for every `price` call that
    /// follows, until the next call here. Stateful on purpose (ruling 1);
    /// the worker calls it once per batch.
    fn set_overrides(&self, overrides: &MarketOverrides) -> Result<(), PricingError>;
    fn price(&self, req: &PriceRequest) -> Result<PriceResult, PricingError>;
}
```

Every value is per unit of the instrument. `Instrument` and its parts
are plain data with `PartialEq`; the module compares them to decide
whether an edit changed the request (§9.3).

### 5.2 `MockPricer`

Deterministic and cheap. `price` is derived from a hash of the
instrument's fields so the same request always answers the same
numbers; the shift terms are applied so that a positive `spot_pct`
moves `price` in the sign of `delta` and a positive `vol_pts` in the
sign of `vega`, so a trader bumping shifts sees plausible motion.
A call's delta is positive and a put's negative; gamma and vega are
positive; theta is negative. Nothing here is a model, and the doc
comment says so. Two test hooks, set at construction: a fixed per-call
`delay: Duration` (default zero) and a refusal rule — an instrument
whose underlying is `FAIL` returns `PricingError("refused by the
mock")`. `name()` is `"mock"`. Overrides: the mock keeps the last
`MarketOverrides` it was given behind a `Mutex`; a spot override for
a line's underlying moves the price by `delta × (override − reference)`
where the reference spot is the absolute strike, or `100` for a
percent strike, so a higher spot raises a call and lowers a put.
`set_overrides` refuses a spot that is not a positive finite number
(`PricingError("spot override must be a positive finite number")`),
which is the testable failure path for the worker's whole-batch rule.

### 5.3 The data-tier seam (`geode-data`)

Two new requests and two new events.

```rust
// handle.rs
Request::Price(PriceParams)            // DataHandle::price(params) -> bool
Request::Publish(LocalPublish)         // DataHandle::publish(local) -> bool
// geode_core::pricing
pub struct PriceParams  { pub key: QueryKey, pub tag: u64, pub submitted: Instant, pub overrides: MarketOverrides, pub lines: Vec<PriceLine> }
pub struct PriceLine    { pub id: u64, pub revision: u64, pub request: PriceRequest }
pub struct PriceOutcome { pub key: QueryKey, pub tag: u64, pub submitted: Instant,
                          pub results: Vec<(u64, u64, Result<PriceResult, String>)> }  // (id, revision, result)
pub struct LocalPublish { pub dataset: String, pub rows: DocumentRows }
// service.rs
DataEvent::Price(PriceOutcome)
DataEvent::Published { .. }            // unchanged; a local publish emits it like any other
```

**The pricing worker.** One thread, started by `DataService::open`
from `DataServiceConfig.pricer: Option<Arc<dyn Pricer>>`, separate
from the DuckDB query pool, draining its own bounded queue. Rules:

- **Latest wins per key.** A `PriceParams` for a key with one already
  queued replaces it; a request arriving while that key's batch is
  running is queued and runs after it. The outcome carries the tag of
  the request it answers.
- **Cancel by key.** `Request::Cancel { key }` (the existing variant)
  also drops that key's queued batch and stops a running one at the
  next line boundary; the lines already priced are delivered.
- **Contained per line.** Each `price` call runs under
  `geode_core::panic::contained`; a panic is that line's `Err` with
  the message, and the worker continues. The worker never dies; a
  shutdown request ends it.
- **Overrides once per batch.** Before a batch's first line the
  worker calls `set_overrides(&params.overrides)` under the same
  boundary; a refusal or a panic there fails EVERY line of the batch
  with `overrides refused: <reason>` and prices none of them (a line
  priced against the wrong data source is worse than no price). The
  next batch sets its own.
- **No pricer configured** (`pricer` is `None`): every line answers
  `Err("pricer \"<name>\" is not built into this binary")`, where
  `<name>` is the configured name (§8.3).
- A full request channel (`DataHandle::price` returning `false`) is
  "not queued"; the tile keeps the lines stale and resubmits on the
  next frame, as every other request does.

**The local publish.** `Request::Publish` builds a `DocumentJob` with
`source = "local"`, `source_time = received_at = now`, and hands it to
the ingest runner's `submit_document`, which already takes a queued
document ahead of any file and publishes it under `contained`. The
publish emits `DataEvent::Published` as any document does, so the
catalog and the diagnostics tile see it. A dataset accepts a local
publish only if its spec says `local = true` (§7.2); a publish to any
other dataset is refused with an error event, never written.

### 5.4 Delivery (`geode-shell`)

```rust
pub enum Delivery {
    Query(QueryOutcome),
    Price(PriceOutcome),
}
```

`Delivery::key()` answers `outcome.key` for both. Adding the variant
makes the compiler name every occupant's `match`; the blotter, the
market-data panel and the diagnostics tile add an explicit
`Delivery::Price(_) => {}` arm, no wildcard (the existing rule).

### 5.5 Registration and config

```toml
# app.toml, user or desk layer
[pricing]
adapter = "mock"      # default "mock"; the name a Pricer answers from name()
refresh = "30s"       # default periodic reprice; "off" disables; a sheet may override
```

`geode-app` keeps a `PricerRegistry` beside `AdapterRegistry`
(`register(Arc<dyn Pricer>)`, `get(name)`); `main` registers
`MockPricer` unconditionally (it is the only pricer that exists) and a
feature-gated vendor crate registers itself the same way. `bridge::
data_setup` resolves `[pricing] adapter` against the registry; an
unknown name yields `pricer: None` plus a config-section warning
naming the missing pricer, and every line's error says the same
(§10.1). A change to `[pricing]` at reload is a
`restart to apply` stripe, like `sources`.

## 6. The sheet (pure core, `geode-pricer::core`)

### 6.1 Struct of arrays

```rust
pub struct Sheet {
    pub name: String,
    pub view: String,
    pub sheet_shift: OwnShifts,          // both fields Option<f64>
    pub overrides: MarketOverrides,      // sheet-wide, by underlying: spot levels now (ruling 1)
    pub refresh: Option<Duration>,       // None = the app default
    // per row, in sheet order; a package's legs follow it contiguously
    ids: Vec<LineId>,                    // per-sheet monotonic u64, never reused
    kind: Vec<RowKind>,                  // Line | Package { template: Template } | (slice 2) Underlying
    parent: Vec<Option<u32>>,
    instrument: Vec<Option<Instrument>>, // None on a package
    qty: Vec<i64>,                       // signed; default 1; sell is negative
    shift: Vec<OwnShifts>,               // per-line own values; None = inherit the sheet's
    revision: Vec<u64>,
    result: Vec<Option<PriceResult>>,
    state: Vec<LineState>,               // Fresh | Stale | Failed(String)
    priced_at: Vec<Option<DateTime<Utc>>>,
    next_id: u64,
}
```

The tree is the array plus a depth: a row's depth is 0 for a root, 1
for a leg. `Sheet::children(row) -> Range<usize>` is the contiguous
run after a package; `Sheet::roots()` walks depth-0 rows. There is no
separate index to keep in step.

`Sheet::request(row) -> Option<PriceRequest>` is the one place a
line's request is assembled: its instrument and its **effective**
shifts (`own.or(sheet)`, each field independently, `0.0` when both are
`None`).

### 6.2 Edits and undo

Every mutation goes through one door:

```rust
pub enum Edit {
    Insert { at: usize, rows: Vec<RowSpec> },      // a line, or a package with its legs
    Remove { at: usize },                          // a package removes its legs
    SetInstrument { row: usize, instrument: Instrument },
    SetQty { row: usize, qty: i64 },
    SetShift { row: usize, shift: OwnShifts },
    Move { row: usize, delta: isize },             // within the parent
    Group { first: usize, count: usize, template: Template },
    Ungroup { row: usize },
    SetSheetShift(OwnShifts),
    SetSpotOverride { underlying: String, level: Option<f64> },   // None clears
}
pub fn apply(&mut self, edit: Edit) -> Result<Undo, EditError>;  // Undo wraps the inverse Edit(s) and the removed rows' ids
```

`apply` bumps `revision` and sets `Stale` on every line whose
`request()` changed (compared before and after, `PartialEq` on
`PriceRequest`), and on every leg when `SetSheetShift` changes an
inherited value; `SetQty` and `Move` change no request. It returns the
inverse for a bounded undo stack (`u`/`ctrl+r`, 100 entries) owned by
the tile; undo of a `Remove` reinstates the rows with their original
ids and their last results, so a restored row is not re-requested
unless its revision is behind the sheet's (§9.3). `EditError` covers
a `Group` that is not a contiguous run of roots, an `Ungroup` on a
line, and a `Move` off the end; the tile shows each in the footer.

### 6.3 Shorthand

One line, whitespace-separated, case-insensitive:

```
[qty] UNDERLYING EXPIRY STRIKES TYPE [BARRIER]

qty      signed integer, default 1; a leading - is sell           -5   10
EXPIRY   Z26 | DEC26 | 20DEC26 | 3m | 6w | 1y                      (Z26 and DEC26 are the third Friday; a tenor passes through as Expiry::Tenor)
         IMM month codes: F G H J K M N Q U V X Z = Jan … Dec
STRIKES  one or more, /-separated; each an absolute or a percent   5000   95%/105%
TYPE     C  P              one leg: call, put
         CS PS             two strikes: +K1 −K2 calls (puts)
         STRD              one strike: +call +put
         STRG              two strikes: +put K1 +call K2
         RR                two strikes: −put K1 +call K2
         FLY               three strikes: +1 −2 +1 calls
         CAL               two expiries as E1/E2 and one strike: +far −near calls
BARRIER  UI | UO | DI | DO  level      only after C or P              SPX DEC26 5000 C DO 4200
```

`Template` is the enum `{Custom, CS, PS, STRD, STRG, RR, FLY, CAL}`
and a template is a table: for each leg, its sign, which strike index
and which expiry index it takes, and its option kind. The parser is a
pure function `parse(text) -> Result<Parsed, ParseError { offset,
message }>` where `Parsed` is one `RowSpec` (a line) or a package
`RowSpec` with its legs. Errors, each with the offending token's
offset: unknown type, wrong strike count for the type, wrong expiry
count, a barrier on a package, a barrier without a level, an unknown
barrier kind, an unparseable number, an unparseable expiry, an empty
line. A month form — the IMM code `Z26` or the name `DEC26` — resolves to
a date at parse time by the calendar rule "third Friday of the month"
— this is a date convention, not a financial calculation, and the spec
says so; holidays are not considered. The IMM letter table (`F G H J K
M N Q U V X Z`) is one constant with a test; a two-digit year is
`20yy`.

`shorthand(row) -> String` renders a line or a package back in the
same grammar (a package prints its template form when its legs still
match the template's table, `custom` legs one per line otherwise). A
date expiry that falls on a third Friday renders as its IMM code
(`Z26`), any other date as `20DEC26`, a tenor as typed.
It is the tree column's label, what `y y` yanks, and it round-trips
through `parse` for every template and both variants (a test).

### 6.4 Package rows

A package has no instrument and no request. For each of the six
result fields, its painted value is `Σ qty_leg × value_leg` over its
legs, recomputed by `Sheet::fold_packages` after every delivery and
every edit. A package is `Stale` while any leg is, `Failed` naming the
first failed leg while any leg is, and `Fresh` otherwise. A leg keeps
its own `qty`, so a 1×2 ratio spread is a callspread whose second leg
is edited. `Group` over a run of roots makes a `Custom` package;
`Ungroup` promotes the legs back to roots in place.

### 6.5 Columns and views

`core::columns` is the fixed vocabulary:

| Group | Column | Editable | Applies to |
|---|---|---|---|
| Instrument | `qty`, `underlying`, `expiry`, `strike`, `type` | yes | every line |
| Instrument | `barrier`, `barrier_type` | yes | `Barrier` only |
| Shifts | `spot_shift`, `vol_shift` | yes | every line |
| Results | `price`, `delta`, `gamma`, `vega`, `theta`, `rho` | no | every row |
| Status | `priced_at`, `status` | no | every row |

Each is a `ColumnDef { name, kind: ColumnKind, editable, applies_to,
default_format: ColumnFormat, default_width: f32 }`. A cell whose
column does not apply to the row paints blank; on a package row the
instrument columns paint blank and the shift columns paint blank (a
package has no shifts of its own).

The `pricer_views` doc:

```toml
config_version = 1

[vanilla]
columns = ["qty", "underlying", "expiry", "strike", "type", "spot_shift", "vol_shift",
           "price", "delta", "gamma", "vega", "theta", "rho"]
[[vanilla.columns]]   # optional per-column presentation, the blotter's keys
name = "price"
format = { precision = 2 }
```

Either the array form (names only) or the `[[view.columns]]` table
form (name plus `label`, `width`, `format = { precision, scale,
negative }`) per view, resolved through `geode_core::view::
ColumnFormat` and `ColumnPresentation` so formatting code is shared.
An unknown column name is an error diagnostic on the doc and the view
drops it; a view with no valid column is dropped with an error. Two
builtin views ship: `vanilla` (as above) and `barrier` (`vanilla` plus
`barrier` and `barrier_type` after `type`). Desk and user layers
override by name. `ColumnPlan::build(view, sheet)` yields the table's
columns in view order after the tree column.

## 7. Storage

### 7.1 The `SheetStore` seam

```rust
pub trait SheetStore {
    fn load(&self, name: &str, key: QueryKey, tag: u64) -> bool;    // answered by a Delivery::Query carrying the document
    fn save(&self, sheet: &Sheet) -> bool;                          // a local publish
    fn list(&self, key: QueryKey, tag: u64) -> bool;                // answered by the catalog
    fn remove(&self, name: &str) -> bool;
}
```

The production implementation wraps `DataHandle`; the tests' fake
answers synchronously from a map. The tile never holds a
`DataHandle` directly.

### 7.2 The dataset

The module's builtin config layer declares:

```toml
[pricer_sheets]
family = "document"
local = true                      # new: accepts Request::Publish; has no source
key = ["sheet"]
axes = ["line"]

[pricer_sheets.columns.sheet]
type = "utf8"
role = "dimension"
textual = true
[pricer_sheets.columns.line]
type = "i64"
role = "axis"                     # the LineId
# value columns, one row per line or package row:
#   order i64; kind utf8 (line | package); template utf8 (custom | cs | ps |
#   strd | strg | rr | fly | cal | "" on a line); parent i64 (the parent's
#   line id, -1 on a root); qty i64; underlying utf8; expiry_kind utf8
#   (date | tenor); expiry utf8 (2026-12-20 | 3m); strike_kind utf8
#   (abs | pct); strike f64; option_kind utf8 (call | put | "" on a
#   package); barrier_kind utf8 ("" | ui | uo | di | do); barrier f64;
#   spot_shift_own i64 (0 = inherit); spot_shift f64; vol_shift_own i64;
#   vol_shift f64 — each declared as
[pricer_sheets.columns.qty]
type = "i64"
role = "value"
# document-level attributes, constant within one sheet:
#   view utf8; sheet_spot_shift_own i64; sheet_spot_shift f64;
#   sheet_vol_shift_own i64; sheet_vol_shift f64; refresh utf8
#   ("" | 30s | off) — each declared as
[pricer_sheets.columns.view]
type = "utf8"
role = "attribute"
```

The declaration is the datasets doc's own column form (the CVI
dataset's), one `[pricer_sheets.columns.<name>]` table per column; the
comment lists the set once rather than spelling out twenty tables
here. `local` is a new top-level dataset key, `validate_dataset`
accepts it on the document family only.

**Retention.** `RetentionPolicy` exists in the store but nothing
configures or runs the sweeper yet (Phase 4a's handoff: "decide when
the sweeper is wired"). The edit history of a sheet is bounded in
code: the local publish path passes `keep_generations = 200` for a
`local` dataset when it publishes, and the plan's Part 4 wires the
sweep for local datasets alone, on the publish, since a sheet is the
first dataset whose generations arrive by the hundred. Wiring the
sweeper for every dataset stays the separate decision it was.

`Sheet::to_rows()` and `Sheet::from_rows()` are the pair; a test
round-trips every template, both variants and every shift state. The
three store rules and how the sheet meets them:

- **A zero-row document is refused.** A sheet with no lines is never
  published; its name lives in the tile's session record until its
  first line. Removing the last line publishes nothing, so the last
  non-empty generation stays as history, and `load` of a name with no
  document is an empty sheet, not an error.
- **Values have no NULL.** Every optional is a pair (`*_own` flag plus
  value) or a text kind plus value, as above.
- **A publish bumps the frame's `data` version and every visible
  tile requeries.** `DatasetSpec.local` is the gate: the bridge's
  `Published` arm skips the frame bump for a local dataset and still
  notes it for diagnostics. A future tile reading sheets follows the
  dataset through its own document request. A test in `geode-app`
  pins that a local publish leaves `FrameVersions.data` unchanged.

A local dataset cannot name a source (`SourceSpec::from_doc` refuses
a `[sources]` entry naming a dataset with `local = true`, at the
source's own `dataset` key), and the document family's own rules
(utf8 key, dimensions in key, no `book`) hold unchanged.

### 7.3 Write-behind

The tile's `Sheet` is the truth for painting. After any `apply`, the
tile arms a one-second idle timer; when it fires, `save` publishes
the whole sheet once. A second edit inside the window re-arms it.
A failed publish (`false` from the handle, or a failure event) shows
a header notice and retries on the next edit burst; the sheet is not
lost until the tile is, and even then the session record names it.
There is no `:w`.

### 7.4 Names, restore and the open set

A new tile opens `untitled-N` for the first `N` with no document and
no open tile. The tile's session record (`serialize`) is `{ sheet,
view, cursor: line id, expanded: [line ids], refresh }`; restore
requests the sheet's document, paints a `loading` notice until it
arrives (the market-data first-delivery shape), installs it, and
reprices everything. A restored name whose document is gone opens
empty under that name with a notice. The factory holds the set of
sheet names open across its tiles; `:e` of an open name is refused
("open in another tile"), so two writers never race one document.
`:rm` refuses an open name too, confirms through the dialogs'
`ConfirmAnswer`, and removes by name: the store's `remove` publishes
nothing (there is no document delete in the store) — it removes the
name from the tile's list by asking the catalog to forget the batch
through the existing retention path, which drops its generations at
the next sweep. **Until a "forget batch" exists in the store, `:rm`
is refused with "not built yet"** and the plan carries that item as
its own task, so the store change is reviewed on its own.

## 8. The tile (`geode-pricer::tile`)

### 8.1 Structure

`PricerTile` is the gpui entity; `PricerContent: TileContent` and
`PricerFactory: ModuleFactory` (kind and context both `pricer`) sit
in `content.rs` as the market-data panel's do. The factory holds the
loaded views (`Rc<RefCell<Views>>`, replaced by `set_views` on
reload), the app default refresh, the pricer's name for the header,
the open-sheet name set, and the `SheetStore`. The table is a
`gpui_component::table::DataTable` over `SheetDelegate: TableDelegate`
holding `model: Rc<GridModel>`, the cursor mirror, and the open
editor, all `pub(crate)`; `geode_pricer::init(cx)` rebinds
`DataTable`'s own keys to `NoAction`, the copy the other two grids
carry.

### 8.2 Grid model and painting

`GridModel::build(&Sheet, &Expansion, &ColumnPlan, theme inputs)`
prepares every visible row's cells as `SharedString`s with a paint
per cell, on every edit, delivery and expansion change. A sheet is
hundreds of rows; the bench pins the build at 1,000 lines against the
8 ms budget, and `install_model` → `TableState::refresh` is the only
way a model reaches the table. Column 0 is the tree column, pinned
left: indent by depth, a chevron on package rows (`▾`/`▸`) whose
click is `space` through a module-local `ChevronClicked` event on the
`TableState`, the blotter's idiom re-implemented, nothing lifted; then
the row's shorthand (a package: its template and its legs' underlying
and expiries). The cursor never enters column 0's editor but the
entry field paints there (§8.4).

Paints, all through the existing doors and swept over every bundled
theme with no exception list: a `Stale` result cell in `muted` text;
a `Failed` row's result cells as `—` in `Tone::DangerText`; an
inherited shift `muted`, an own value `foreground`; a package row's
ground the theme's `secondary` with its sums in `foreground`; the
cursor row and cell the blotter's. A pending entry row paints its
field in the tree column with the blotter's row ground.

### 8.3 Header and footer

One dense row: the sheet name; the view name; the sheet-wide shifts as
neutral chips (`spot +2%`, `vol −1`) or nothing; the pricer's name
(`mock`); the last priced time, stale-marked past `stale_after` like
the market-data header; and, while any line is stale, `N pricing…`.
Notices (a load failure, a refused publish, a missing pricer) paint
in the header in `Tone::WarningText`. The footer carries the cursor
row's failure message, an edit refusal, or a parse error with its
caret column, and is otherwise empty; it is always laid out so the
table's height never changes.

### 8.4 Modes and inputs

Key context `pricer` with `mode` in `normal`, `insert`, `entry` and
`menu`. Two tile-owned `InputState`s exist at most one at a time: the
cell editor (insert) and the entry field (entry). Both follow the
market-data rules verbatim: `holds_focus` answers for the open one,
`close_*` blurs only when its own field is focused and then drops it,
a click anywhere cancels an open editor, and the shell's chords
(`ctrl+k` and the rest) are never shadowed.

**Entry.** `o` inserts a placeholder row after the cursor row (inside
a package, as a leg after the cursor leg; on a package row, as its
first leg) and paints the field in its tree column; `shift+o` before.
`enter` parses: success replaces the placeholder with the parsed
rows, submits their requests, and opens a fresh placeholder below so
a book of lines is typed without another `o`; a parse error keeps the
text and paints the message and caret in the footer. `escape` removes
the placeholder. The entry mode's keymap fragment binds only `enter`,
`escape`, `up` and `down` (history of the sheet's own lines, most
recent first).

**Insert.** `i`, `enter` and a double-click on an editable cell open
the editor with the cell's current text; a non-editable cell says
`read-only` in the footer. Parsing per column kind: `qty` an integer;
`strike` and `barrier` a number or a percent where the column allows;
the shifts a signed number, an empty commit meaning "inherit";
`expiry` any of the grammar's expiry forms; `underlying` and `type` (and
`barrier_type`) open a `geode_shell::choice::ChoiceList` typeahead
over, respectively, the underlyings already on the sheet plus the
catalog's known underlyings when the frame has one, and the
vocabulary. `up`/`down` nudge a numeric editor by the painted
precision through `nudge_text`; `shift` steps ten. A commit whose
cell moved is refused. A commit applies one `Edit` and reprices what
changed.

### 8.5 Normal-mode keys

Shared with the two other grids where they overlap: `j` `k` `h` `l`,
`g g`, `shift+g`, `^` `$` `home` `end`, `ctrl+d` `ctrl+u` `ctrl+f`
`ctrl+b` `pagedown` `pageup`, `y` `y y` `y c`, `n` `shift+n` with
`/`, `escape`. Tree keys are the blotter's: `space`, `z o`, `z c`,
`z a` on a package row; `z R`, `z M` for all.

| Key | Action id | Effect |
|---|---|---|
| `o` / `shift+o` | `pricer::add_below` / `add_above` | open the entry field |
| `i` / `enter` | `pricer::edit` | edit the cell |
| `d d` | `pricer::delete` | remove the row (a package with its legs); no confirm, `u` is one key away |
| `u` / `ctrl+r` | `pricer::undo` / `redo` | sheet-level |
| `p` / `shift+p` | `pricer::put_below` / `put_above` | duplicate the last yanked or deleted rows, fresh ids |
| `shift+j` / `shift+k` | `pricer::move_down` / `move_up` | within the parent |
| `g p` (count) | `pricer::group` | the cursor row and the next count−1 roots become a `custom` package |
| `g u` | `pricer::ungroup` | the package under the cursor |
| `.` | `pricer::menu` | the action menu (market-data popup reused): price, group, ungroup, view, name |

Every action is registered by the factory, so the palette lists it.

### 8.6 Commands

```
:view <name>                switch the sheet's view (completions: the loaded views)
:name <name>                rename the sheet (refused if the name is open or exists)
:e <sheet>                  switch this tile to another sheet (completions: the catalog's names)
:new                        open the next untitled sheet in this tile
:rm <sheet>                 remove a sheet no tile has open, confirmed (see §7.4's deferral)
:shift spot <n>|clear       sheet-wide spot shift, inherited by lines with no own value
:shift vol <n>|clear        sheet-wide vol shift
:spot <underlying> <level>|clear   sheet-wide spot override for one underlying (ruling 1); `:spot clear` drops all
:price                      reprice every line now
:refresh <dur>|off|default  periodic reprice for this sheet
:group  :ungroup            the key verbs, for the palette and menu
```

`core::commands` is the pure parse and completion vocabulary, as the
other modules'. An unknown verb or a bad argument is the command
line's inline error.

## 9. Repricing

### 9.1 When

After every `apply`, and on each refresh tick, and on `:price`, the
tile collects every line that is `Stale` and not already in flight at
its current revision and submits one `PriceParams` for the frame,
tagged with the tile's request counter. In-flight is a map `id →
revision` cleared by delivery or cancel.

### 9.2 Delivery

`Delivery::Price(outcome)` for the tile's key: for each `(id,
revision, result)`, if the line exists and its revision equals the
answered one, install the result (or `Failed`) and `priced_at`, mark
`Fresh`, and remove it from in-flight; otherwise drop it. Then
`fold_packages`, rebuild the grid model, and if any line is still
`Stale` and not in flight (an edit landed during the round trip),
resubmit. An outcome tagged older than the latest submission is
dropped whole, the query rule. A delivery for another key is ignored.

### 9.3 What changes a request

`Sheet::request(row)` compared before and after an `apply`. So: an
instrument field, an own shift, a sheet-wide shift on a line that
inherits it, a spot override on the line's underlying (overrides ride
in `PriceParams`, not in the line's request, so `Sheet::request` is
unchanged and the tile marks every line on that underlying stale
itself). Not: `qty`, `Move`, `Group`, `Ungroup`, a view switch, a
rename. Undo of a `Remove` reinstates results with the row, so it
requests nothing; undo of a `SetInstrument` restores the old
instrument, which is a request change, so it re-requests (the old
result is not cached).

### 9.4 The refresh timer

The tile's own, `cx.spawn` with a timer, running only while visible
(`set_visible` starts and stops it) and only while the sheet has a
line. Each tick marks every line `Stale` and submits. The interval is
the sheet's `refresh`, else `[pricing] refresh`, else 30 s; `off`
disables. The header's `stale_after` mark reads `priced_at` against
the shell's `stale_after`, the blotter's rule, so a stopped pricer is
visible even with the timer off.

### 9.5 Close and hide

`set_visible(false)` cancels in flight by key and stops the timer;
closing the tile cancels, stops, and blurs-then-drops any open input.
A hidden tile keeps its stale marks and resubmits on show.

## 10. Errors and diagnostics

### 10.1 Taxonomy

By the charter's three classes:

- **User errors** (a parse error, an edit refusal, an unknown verb):
  inline, with a caret or a footer line, never a dialog.
- **Data problems** (a line the pricer refuses, a contained panic, a
  missing pricer, a failed load or publish): the row paints `—` in
  danger text with the message in the footer, the header carries the
  notice, the log carries the line; the tile stays usable. A missing
  pricer names itself: `pricer "vendor" is not built into this
  binary` on every line and in the config-section diagnostics.
- **Bugs** (a delivery for a row that does not exist, a revision in
  the future): dropped and logged at `warn` with the ids.

### 10.2 Logging

A seventh tracing target `geode::pricing`: the worker logs one
`debug` line per batch (key, line count, elapsed, failures) and
`warn` on a contained panic or a refused request. `TARGETS` becomes
`[&str; 7]`, the `[log]` key set grows by `pricing`, and the test
that pins the count moves with it.

## 11. Demo

`--demo` needs nothing new: the mock is registered in every build,
`[pricing] adapter = "mock"` is the default, and the demo layer adds
`pricer_views` (the two bundled views) and the `pricer_sheets`
dataset comes from the module's builtin layer. The demo's known
underlyings feed the underlying typeahead through the catalog.

## 12. Tests, harness and benchmarks

Weight goes seam ≫ core ≫ tile.

- **`geode-pricing`**: same request, same numbers; monotone under
  `spot_pct` in the sign of delta and `vol_pts` in the sign of vega;
  a put's delta negative, a call's positive; `FAIL` refused; the
  delay honoured.
- **`geode-data`**: latest-wins per key (three requests, one runs);
  cancel drops the queued batch and stops at a line boundary with the
  priced lines delivered; a panic inside `price` is one line's `Err`
  and the next line prices; `pricer: None` errors every line with the
  configured name; `Request::Publish` lands a generation the document
  request reads back; a publish to a non-local dataset is refused
  unwritten; a local dataset naming a source fails validation.
- **`geode-app`**: a local publish leaves `FrameVersions.data`
  unchanged and still reaches diagnostics; a non-local one bumps it.
- **`geode-pricer` core**: a parse table with every template, both
  variants, every expiry form (IMM code, month name, full date,
  tenor) and every error offset; `shorthand` round-trips through
  `parse`; `apply` then its `Undo` is identity for every `Edit`;
  package sums with signed quantities; a result with an old revision
  is dropped and a current one installed; `Group` refuses a
  non-contiguous run; `Move` stays within the parent; `to_rows`/
  `from_rows` round-trips every shift state; `ColumnPlan` for each
  bundled view over each variant; the unknown-column diagnostic.
- **Tile** (`TestAppContext`, the recording fixture's harness): `o`
  then a line then `enter` adds a row and submits one request;
  delivery paints the numbers; editing the strike marks it stale and
  resubmits; the older revision's delivery is ignored; `dd` then `u`
  restores the row with its numbers and no request; `:shift spot 2`
  reprices only inheriting lines; the refresh tick marks all stale;
  `:e` refuses an open name; restore requests the document and
  reprices; hide cancels by key; the editor blurs before it drops
  (the market-data test copied); `Delivery::Price` for another key is
  ignored; the bundled-theme sweep over every new paint.
- **Benchmarks** (`cargo bench -p geode-pricer`, criterion,
  `harness = false`): parse 1,000 lines; `GridModel::build` at 1,000
  lines; `apply` plus undo at 1,000 lines; results to `docs/perf.md`
  against the 8 ms budget.
- **Harness entries**, each naming its test: the revision drop, the
  package sign, the inherited shift, latest-wins, cancel at the line
  boundary, the contained panic, `local` skipping the frame bump, the
  zero-row refusal, undo as inverse, `qty` changing no request.

## 13. Docs

- This spec; the §2.1 paragraph in `docs/PHILOSOPHY.md`.
- `CLAUDE.md`: a status row and a **Pricer** rules block (the delivery
  variant, the `local` gate, latest-wins, the revision rule, the
  no-save rule, blur-then-drop for two inputs).
- `docs/phase-history.md`: the paragraph.
- The roadmap's §5 G and §6 item 4 updated per §3.
- `docs/perf.md`: the three bench numbers.

## 14. Sequencing

Four parts, each mergeable on its own:

1. **Seam and data tier.** `geode-pricing` (vocabulary, trait,
   mock), `PriceParams`/`PriceOutcome`/`LocalPublish` in `geode-core`,
   the worker and both requests in `geode-data`, `local` on
   `DatasetSpec` with its validation, `Delivery::Price` and the three
   explicit arms, `PricerRegistry`, the bridge arm with the frame-bump
   gate, `[pricing]`, the tracing target, the charter amendment.
2. **Core.** `Sheet`, `Edit`/`Undo`, the parser and renderer, the
   templates, `fold_packages`, `columns`, the `pricer_views` doc and
   `ColumnPlan`, `to_rows`/`from_rows`, the benches.
3. **Tile.** Factory, content, delegate, grid model, header and
   footer, entry and insert modes, keys and commands, repricing, the
   timer, session record, the theme sweep. Sheets are in-memory only
   at the end of this part (the fake store), so the tile is reviewable
   before storage lands.
4. **Storage.** The `pricer_sheets` dataset, the production
   `SheetStore`, write-behind, `:e`/`:name`/`:new`, restore, the
   open-name set; `:rm` behind the store's "forget batch" item.

Slice 2 (per-underlying children, basket products, per-underlying
shifts) is its own spec against this one.

## 15. Open questions

- Whether the real library wants a batch call; the seam is per line
  and a batching implementation of `Pricer` can be added behind it
  without a caller changing.
- Whether `priced_at` should also stamp the library's own market-data
  time when the real one exposes it; `PriceResult` gains a field then.
- The `:rm` path (§7.4): a store-level "forget batch" is the cleanest
  answer and is deferred to its own task.

## 16. As built (Part 1, 2026-09-19)

- The vocabulary, the `Pricer` trait and `PriceParams`/`PriceOutcome`/
  `LocalPublish` live in `geode_core::pricing`, not `geode-pricing`:
  the shell names `PriceOutcome` in `Delivery` and must not depend on
  a calculation crate (§2.1). `geode-pricing` holds `MockPricer` and
  is where vendor crates go. §4 and §5.1 read with that substitution.
- `PricerConfig` (`geode_data::pricing`) carries the configured name
  beside the optional pricer so a missing one names itself; an empty
  name says "no pricer is configured".
- The worker's queue is bounded by distinct keys (`PRICE_BOUND` = 64);
  a replacement for a queued key always fits.
- A local publish's ingest-sink arms emit no `Health` (no declared
  source has a lane); a failed one is an error `Diagnostics` event
  plus `LoadEnded`.
- Retention for local datasets is NOT wired in Part 1 (nor is the
  sweeper for anything else); §7.2's "keep_generations = 200" is
  Part 4's.
- Overrides (Task 7b, ruling 1 amended): the real library takes an
  overridable `PricingDataSource`, so overrides are STATEFUL
  (`Pricer::set_overrides` then `price`), batch-scoped by the worker —
  set once per batch before its first line; a refusal or a panic
  fails every line of the batch with `overrides refused: <reason>`
  and prices none of them. `MarketOverrides { spot: BTreeMap<String,
  f64> }`, `PriceParams.overrides`, and `Instrument::strike()` were
  added for it. The mock's reference spot is the absolute strike, or
  100 for a percent strike; it refuses a non-positive or non-finite
  spot with `spot override must be a positive finite number` and
  keeps the previous overrides in place.
- A worker-refused batch is answered, not dropped (Task 6):
  `DataService::price` returns `()`; when the worker's own bounded
  queue refuses a batch, the service emits `DataEvent::Price` itself
  with every line `Err("the pricing queue is full; resubmit")` and
  logs a warning naming the key and tag. `DataHandle::price`'s `false`
  means only that the request channel itself refused — a batch that
  reaches the service always gets an outcome.
- The `[pricing]` unknown-adapter diagnostic reads `pricer "<name>"
  ([pricing] adapter) is not built into this binary (have: <names>);
  every priced line will say so` (Task 8).
- The worker's undelivered-outcome warning reads "a price outcome for
  key {} was not delivered; further refusals are not logged" (Task 5)
  — there is no counter, so only the first is logged.
- `LogLevels` stores suffix keys (`"pricing"`, not `"geode::pricing"`)
  like every other target; this is the existing, correct shape, not a
  Part 1 gap.
- Final-review fix wave: the `[pricing]` restart baseline is narrowed
  to the `adapter` key alone (`ShellView::pricing_baseline`,
  `hot_reload::apply_reload`) — `refresh` is a live sheet setting from
  Part 3 onward and must never demand a restart, so only the adapter
  choice, which really is baked into the running data engine, gates
  it.
- Final-review fix wave: a local publish's document arm in the ingest
  runner emits no `IngestEvent::Started` (and so no `DataEvent::
  Loading`) when `job.source == LOCAL_SOURCE` — a sheet autosave must
  never blink the ingest progress strip. The unconditional `LoadEnded`
  after every publish is untouched; the strip tolerates a `LoadEnded`
  with no matching `Started`.
- Unverified on a real window: nothing in Part 1 paints.

## 17. As built (Part 2, 2026-09-20)

The core landed as `geode-pricer::core` with these resolutions of
§6/§7 (plan `2026-09-20-line-pricer-part-2-core.md`, "Decisions made
in planning"):

- `RowSpec` is an enum (`Line` | `Package { template, legs }`) and IS
  the parser's answer; `Insert` takes a `Place` (`Root { at }` |
  `Leg { package, leg }`), not a bare index.
- `Remove`'s inverse is `Edit::Restore { at, rows: Vec<RowRecord> }`;
  `Undo { inverse: Vec<Edit> }`; `Sheet::undo` answers the redo;
  `Group` carries `id: Option<LineId>` for undo's sake.
- `SetSpotOverride` stales its underlying's lines inside `apply`
  (§9.3's "the tile marks" is the sheet's job).
- `Sheet::refresh` is `Refresh { Default, Off, Every(Duration) }`.
- Spot overrides persist as one `spot_overrides` utf8 attribute
  (`UND=LEVEL;…`); §7.2's column list gains it (ruling 1 was amended
  after §7.2 was written).
- `ColumnPlan::build(view)` takes no sheet.
- A package may be empty. Package state precedence is `Failed` >
  `Stale` > `Fresh`; a package's result is `Some` only when every leg
  has one.
- `cell_text` (`core::columns`) is the pure half of §8.2's grid
  model.
- A custom package's shorthand is its legs one per line.
- The two bundled views are `BUILTIN_VIEWS` in the crate; §11's "the
  demo layer adds `pricer_views`" is unnecessary.
- `Instrument::vanilla()`/`expiry()` were added to
  `geode_core::pricing`; `"pricer_views"` to
  `config::merge::atomic_depth`.
- `PRICER_SHEETS_DECLARATION` (the §7.2 dataset as TOML) lives in
  `core::storage`; Part 4 wires it.
- Benches: parse, `apply` + undo (two shapes) and the storage round
  trip; `GridModel::build` is Part 3's. Numbers in `docs/perf.md`.
- Nothing in Part 2 paints; the crate is not yet in the app's
  dependency graph.

Four execution deviations, found in review and worth recording beside
the fifteen decisions above:

- `Sheet::take_out(at)` returns `()`: a `RowRecord` taken after
  neighbouring rows have moved carries a stale parent index, so a
  caller takes `record(at)` BEFORE removing (the review of Task 4
  found the brief's `remove` doing it after, and fixed it).
- The inverse of `Ungroup` on an EMPTY package is `Restore { at,
  rows: [its record] }`, not `Group { count: 0 }` (which `apply`
  refuses); `Sheet::undo` is documented as NOT atomic — an inverse
  refused mid-batch leaves earlier inverses applied.
- `geode_core::schema::validate_document`'s numeric-only VALUE-column
  rule now exempts `local` document datasets (f64/i64/utf8/date, the
  axis/attribute set): spec §7.2 declares utf8 per-row values and the
  numeric rule was written for feed documents. `local` is forced
  `false` outside the document family, so the gate is unreachable
  elsewhere. **Part 4 must verify the DuckDB publish path binds a
  utf8 VALUE column and quotes `order`/`kind`/`parent`/`template` as
  identifiers** — if it cannot, the row shape needs re-encoding and
  this exemption reverts.
- `Sheet` derives `Debug`; the unused `set_kind` helper was deleted
  rather than kept behind `allow(dead_code)`; no `allow(dead_code)`
  remains in the crate. The parser checks the expiry count BEFORE the
  strike count for a template (so `SPX DEC26/MAR27 5000 CS` points at
  the expiries token), and classifies a trailing token after `C`/`P`
  as a barrier kind only when it parses as one or a level token
  follows it (so `... C extra` is "unexpected token").

Twelve harness entries cover it: the eleven named in §12 above (an
old revision's delivery, a package's signed sum, a failed leg's sum,
undo of a remove, an inherited sheet shift, `SetQty`'s untouched
request, a spot override's stale sweep, `Group`'s contiguous-roots
check, the third-Friday resolution, the unknown-column diagnostic's
severity, and the empty-sheet storage refusal), plus a twelfth in
`geode-core` pinning the `local` VALUE-column exemption's own
boundary (a bool/timestamp value still drops).

`cargo bench -p geode-pricer` at 1,000 lines (criterion medians,
`bae830d`): `parse_1000_lines` 270 µs; `apply_undo_sheet_shift_1000`
1.52 ms; `apply_undo_set_instrument_1000` 6.66 µs;
`to_rows_from_rows_1000` 1.14 ms — all under the 8 ms budget.
`docs/perf.md` has the full conditions.

Deferred minors, none blocking: the row-exists-and-is-line guard is
triplicated across the three cell edits; `from_rows` validates a
leg's parent by id but `Restore` re-parents positionally; override
encoding does no `;`/`=` escaping; a package hopping upward and a
multi-edit redo are untested; `restore` does not validate that `at`
is a root boundary.

Unverified on a real window: nothing in Part 2 paints.
