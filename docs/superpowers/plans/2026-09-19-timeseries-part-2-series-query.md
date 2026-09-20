# Timeseries Viewer Part 2 (Series Query) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the series query: the `geode_core::series` vocabulary and expression parser, the SQL compiler that buckets, joins, computes percentiles and histogram bins over the series tables, the pool and service plumbing that runs it as `Request::Series`, and the two `Delivery` variants that carry a series result and a fetch outcome to tiles. No chart, no tile (Parts 3 and 4).

**Architecture:** `geode_core::series` holds the request/outcome types and a pure recursive-descent expression parser whose resolved AST names slots only. `geode_data::query::series` compiles a `SeriesParams` into a `SeriesPlan` (one points statement, one percentile statement per slot, one bins statement per slot, one coverage statement per source slot) and `run_series` executes it into a struct-of-arrays `SeriesResult`. The query pool gains a second payload kind so a `SeriesResult` rides the same coalescing, cancellation and containment as a `Snapshot`. The bridge maps `DataEvent::Series` to `Delivery::Series` (routed by tile key) and `DataEvent::SeriesFetched` to `Delivery::SeriesFetched` (broadcast to every visible occupant).

**Tech Stack:** Rust 2024, DuckDB 1.10505.0 (`time_bucket`, `arg_max`, `quantile_cont`, `width_bucket`), `chrono`, criterion, `tempfile`.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-timeseries-viewer-design.md` §6, §7, §4.10 (the as-built storage the compiler reads), §11, §12 part 2. Rulings §2.1, §2.2, §2.8, §2.11 bind.

## Global Constraints

- The series tables are as Part 1 built them: `{dataset}_series (source VARCHAR, series_id VARCHAR, ts TIMESTAMP, received_at TIMESTAMP, value DOUBLE)`, `{dataset}_series_coverage (source, series_id, from_ts, to_ts, received_at)`; both timestamps are naive UTC, bound as `BIGINT` micros through `make_timestamp(?)` and read through `epoch_us(...)`. Live is `arg_max(value, received_at)` per `(source, series_id, ts)`; as-of is `received_at <= t` plus `ts <= t`.
- Every identity, source, timestamp and fraction is a BOUND parameter; only table names, frequency intervals, bucket-rule aggregates and slot names (`s{n}`, from a `u8`) are formatted into SQL text.
- Spans are half-open `[from, to)`; `window` is inside `range`.
- An expression is lowered over the INNER join of its operands on the bucket (ruling 11: a missing bucket on either side is absent); division emits `case when (den) = 0 then null else (num) / (den) end`.
- The points result is one row per bucket present in ANY source slot, `NULL` where a slot has none; `NULL` becomes `f64::NAN` in `SlotResult::values`.
- The cap is `frequency.buckets_in(range) > 500_000`, refused on the service thread before compilation, with the message shape `"1m over 3y is 1,170,000 points; the cap is 500,000"`.
- `Request::Series` coalesces per `key`, a newer tag supersedes, a cancelled or shut-down query delivers nothing, a panicking one delivers `Err` and the worker survives — the pool's existing contract, now for two payload kinds.
- `Delivery::Series` is routed by the tile's key; `Delivery::SeriesFetched` carries no key and reaches EVERY visible occupant (hidden tiles hold no subscription and requery on `set_visible(true)`). Every `match` on `Delivery` names both new variants explicitly — no wildcard arm.
- `geode-shell` depends on `geode-core` only, never on `geode-data`; `SeriesOutcome` and the series vocabulary live in `geode-core`.
- No per-frame or per-row heap churn beyond the `Vec`s the result owns; `SeriesResult` is built once on the worker.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets`, `cargo bench --workspace --no-run` pass at the end of every task. Commit at the end of every task. `zsh scripts/mutation-check.sh --anchors-only` before the merge (Task 8).

---

## File map

| File | Responsibility |
|---|---|
| `crates/geode-core/src/series/mod.rs` (new) | `Frequency`, `BucketRule`, `SlotKind`, `SeriesSpec`, `SeriesParams`, `SeriesOutcome`, `SeriesResult`, `SlotResult`, `SlotProvenance`, `SERIES_POINT_CAP`, `cap_message` |
| `crates/geode-core/src/series/expr.rs` (new) | `Ast<R>`, `Expr`, `Op`, `RefName`, `parse`, `resolve`, `expression_order` (cycle check), `ParseError` |
| `crates/geode-core/src/lib.rs` | `pub mod series;` |
| `crates/geode-data/src/query/series.rs` (new) | `SeriesPlan`, `Statement`, `compile_series`, `run_series` |
| `crates/geode-data/src/query/pool.rs` | `Payload`, `Work`, `RequestKind::Series`, `RunFn` returning `Payload`, `run_one` dispatch |
| `crates/geode-data/src/query/mod.rs` | `pub mod series;` and re-exports |
| `crates/geode-data/src/service.rs` | `DataService::series`, the `result_sink` Series arm, `DataEvent::Series`, `HealthTracker::load_lane` read, cap check |
| `crates/geode-data/src/handle.rs` | `Request::Series`, `DataHandle::series`, the serve arm |
| `crates/geode-shell/src/module.rs` | `Delivery::{Series, SeriesFetched}`, `key() -> Option<QueryKey>`, placeholder and recording arms |
| `crates/geode-shell/src/shell/occupants.rs` | `ShellView::deliver` broadcast for a key-less delivery |
| `crates/geode-blotter/src/content.rs`, `crates/geode-marketdata/src/content.rs`, `crates/geode-diagnostics/src/lib.rs`, `crates/geode-shell/src/shell/tests/occupants.rs` | the two new arms |
| `crates/geode-app/src/bridge.rs` | `DataEvent::Series` and `SeriesFetched` arms |
| `crates/geode-data/benches/series_query.rs` (new), `docs/perf.md` | the 1M-row bench |
| `scripts/mutation-check.sh`, `CLAUDE.md`, `docs/phase-history.md`, the spec | Task 8 |

---

### Task 1: The series vocabulary in `geode-core`

**Files:**
- Create: `crates/geode-core/src/series/mod.rs`
- Modify: `crates/geode-core/src/lib.rs` (add `pub mod series;`)
- Test: `crates/geode-core/src/series/mod.rs` (`mod tests`)

**Interfaces:**
- Produces (all `pub`, in `geode_core::series`):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Frequency { M1, M5, M15, H1, D1, W1 }
impl Frequency {
    pub const ALL: [Frequency; 6];
    pub fn parse(s: &str) -> Option<Frequency>;      // "1m" "5m" "15m" "1h" "1d" "1w"
    pub fn as_str(self) -> &'static str;
    pub fn seconds(self) -> i64;                     // 60, 300, 900, 3600, 86_400, 604_800
    pub fn interval_sql(self) -> &'static str;       // "interval '1 minute'", "interval '5 minutes'", "interval '15 minutes'", "interval '1 hour'", "interval '1 day'", "interval '1 week'"
    pub fn buckets_in(self, from: DateTime<Utc>, to: DateTime<Utc>) -> u64;  // ceil((to-from)/seconds), 0 when to <= from
    pub fn next(self) -> Frequency; pub fn prev(self) -> Frequency;  // saturating at the ends
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BucketRule { #[default] Last, First, Mean, Min, Max }
impl BucketRule { pub const ALL: [BucketRule; 5]; pub fn parse(s: &str) -> Option<BucketRule>; pub fn as_str(self) -> &'static str; pub fn next(self) -> BucketRule; }
#[derive(Debug, Clone, PartialEq)]
pub enum SlotKind { Source { source: String, identity: String, rule: BucketRule }, Expr(expr::Expr) }
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesSpec { pub slot: u8, pub kind: SlotKind }
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesParams {
    pub key: QueryKey, pub tag: u64, pub submitted: Instant,
    pub dataset: String,
    pub range: (DateTime<Utc>, DateTime<Utc>),
    pub window: (DateTime<Utc>, DateTime<Utc>),
    pub as_of: AsOf,
    pub frequency: Frequency,
    pub series: Vec<SeriesSpec>,
    pub percentiles: Vec<f64>,
    pub bins: Option<u32>,
}
pub const SERIES_POINT_CAP: u64 = 500_000;
pub const MAX_BINS: u32 = 200; pub const MIN_BINS: u32 = 4;
/// "1m over 3y is 1,170,000 points; the cap is 500,000"
pub fn cap_message(frequency: Frequency, from: DateTime<Utc>, to: DateTime<Utc>, points: u64) -> String;
#[derive(Debug, Clone, PartialEq)]
pub struct SlotProvenance { pub loaded: Option<(DateTime<Utc>, DateTime<Utc>)>, pub latest_received_at: Option<DateTime<Utc>>, pub health: Option<Health> }
#[derive(Debug, Clone, PartialEq)]
pub struct SlotResult { pub slot: u8, pub values: Vec<f64>, pub percentiles: Vec<(f64, f64)>, pub bins: Vec<(f64, f64, u32)>, pub provenance: SlotProvenance }
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesResult { pub buckets: Vec<i64>, pub slots: Vec<SlotResult> }
#[derive(Debug)]
pub struct SeriesOutcome { pub key: QueryKey, pub tag: u64, pub submitted: Instant, pub result: Result<SeriesResult, String> }
```
`Health` is `crate::health::Health`. `SeriesParams` validation lives with the compiler (Task 3), not here.

- [ ] **Step 1: Write the failing tests** (`mod tests` at the bottom of `series/mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn frequencies_round_trip_and_step() {
        for f in Frequency::ALL {
            assert_eq!(Frequency::parse(f.as_str()), Some(f));
        }
        assert_eq!(Frequency::parse("2h"), None);
        assert_eq!(Frequency::M1.seconds(), 60);
        assert_eq!(Frequency::W1.seconds(), 604_800);
        assert_eq!(Frequency::D1.interval_sql(), "interval '1 day'");
        assert_eq!(Frequency::M15.interval_sql(), "interval '15 minutes'");
        assert_eq!(Frequency::M1.prev(), Frequency::M1, "saturates");
        assert_eq!(Frequency::W1.next(), Frequency::W1, "saturates");
        assert_eq!(Frequency::M5.next(), Frequency::M15);
        assert_eq!(Frequency::H1.prev(), Frequency::M15);
    }

    #[test]
    fn buckets_in_rounds_up_and_answers_zero_for_an_empty_span() {
        let from = t("2026-01-05T00:00:00Z");
        assert_eq!(Frequency::D1.buckets_in(from, t("2026-01-15T00:00:00Z")), 10);
        assert_eq!(Frequency::D1.buckets_in(from, t("2026-01-15T00:00:01Z")), 11, "a partial bucket counts");
        assert_eq!(Frequency::M1.buckets_in(from, t("2026-01-05T00:00:00Z")), 0);
        assert_eq!(Frequency::M1.buckets_in(t("2026-01-15T00:00:00Z"), from), 0, "to before from is empty");
        // the spec's own example: 1m over 3y
        let three_years = Utc.with_ymd_and_hms(2029, 1, 5, 0, 0, 0).unwrap();
        assert!(Frequency::M1.buckets_in(from, three_years) > SERIES_POINT_CAP);
    }

    #[test]
    fn the_cap_message_names_the_frequency_the_span_and_the_count() {
        let from = t("2026-01-05T00:00:00Z");
        let to = Utc.with_ymd_and_hms(2029, 1, 4, 0, 0, 0).unwrap();
        let n = Frequency::M1.buckets_in(from, to);
        let m = cap_message(Frequency::M1, from, to, n);
        assert!(m.starts_with("1m over 3y is "), "{m}");
        assert!(m.ends_with(" points; the cap is 500,000"), "{m}");
        assert!(m.contains(','), "thousands are grouped: {m}");
        let m = cap_message(Frequency::M1, from, t("2026-01-25T00:00:00Z"), 28_800);
        assert!(m.starts_with("1m over 20d is 28,800 points"), "{m}");
        let m = cap_message(Frequency::M1, from, t("2026-01-05T06:00:00Z"), 360);
        assert!(m.starts_with("1m over 6h is 360 points"), "{m}");
    }

    #[test]
    fn bucket_rules_round_trip_and_cycle() {
        for r in BucketRule::ALL {
            assert_eq!(BucketRule::parse(r.as_str()), Some(r));
        }
        assert_eq!(BucketRule::default(), BucketRule::Last);
        assert_eq!(BucketRule::Max.next(), BucketRule::Last, "cycles");
        assert_eq!(BucketRule::parse("median"), None);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core series::`
Expected: compile error, module `series` not found.

- [ ] **Step 3: Implement `series/mod.rs`**

```rust
//! The timeseries viewer's shared vocabulary (timeseries spec §6.1,
//! §6.4): what a tile asks for and what the query answers. Below both
//! `geode-shell` and `geode-data` for the reason `query.rs` gives — the
//! two may never depend on each other, and the module builds these
//! while the compiler consumes them.

pub mod expr;

use crate::health::Health;
use crate::query::{AsOf, QueryKey};
use chrono::{DateTime, Utc};
use std::time::Instant;

/// The display frequency a series is bucketed to (spec ruling 2: applied
/// by DuckDB on query, never sent to the source).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Frequency {
    M1,
    M5,
    M15,
    H1,
    D1,
    W1,
}

impl Frequency {
    pub const ALL: [Frequency; 6] = [
        Frequency::M1,
        Frequency::M5,
        Frequency::M15,
        Frequency::H1,
        Frequency::D1,
        Frequency::W1,
    ];

    pub fn parse(s: &str) -> Option<Frequency> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Frequency::M1 => "1m",
            Frequency::M5 => "5m",
            Frequency::M15 => "15m",
            Frequency::H1 => "1h",
            Frequency::D1 => "1d",
            Frequency::W1 => "1w",
        }
    }

    pub fn seconds(self) -> i64 {
        match self {
            Frequency::M1 => 60,
            Frequency::M5 => 300,
            Frequency::M15 => 900,
            Frequency::H1 => 3_600,
            Frequency::D1 => 86_400,
            Frequency::W1 => 604_800,
        }
    }

    /// The `time_bucket` interval, a literal the compiler formats into
    /// SQL text — the one place a frequency becomes SQL.
    pub fn interval_sql(self) -> &'static str {
        match self {
            Frequency::M1 => "interval '1 minute'",
            Frequency::M5 => "interval '5 minutes'",
            Frequency::M15 => "interval '15 minutes'",
            Frequency::H1 => "interval '1 hour'",
            Frequency::D1 => "interval '1 day'",
            Frequency::W1 => "interval '1 week'",
        }
    }

    /// How many buckets `[from, to)` spans at this frequency, a partial
    /// bucket counting as one. What the cap (spec §6.3) is measured on.
    pub fn buckets_in(self, from: DateTime<Utc>, to: DateTime<Utc>) -> u64 {
        let secs = (to - from).num_seconds();
        if secs <= 0 {
            return 0;
        }
        (secs as u64).div_ceil(self.seconds() as u64)
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|f| *f == self).expect("every frequency is in ALL")
    }

    pub fn next(self) -> Frequency {
        Self::ALL[(self.index() + 1).min(Self::ALL.len() - 1)]
    }

    pub fn prev(self) -> Frequency {
        Self::ALL[self.index().saturating_sub(1)]
    }
}

/// How the rows inside one bucket become one value (spec ruling 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BucketRule {
    #[default]
    Last,
    First,
    Mean,
    Min,
    Max,
}

impl BucketRule {
    pub const ALL: [BucketRule; 5] = [
        BucketRule::Last,
        BucketRule::First,
        BucketRule::Mean,
        BucketRule::Min,
        BucketRule::Max,
    ];

    pub fn parse(s: &str) -> Option<BucketRule> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BucketRule::Last => "last",
            BucketRule::First => "first",
            BucketRule::Mean => "mean",
            BucketRule::Min => "min",
            BucketRule::Max => "max",
        }
    }

    /// The next rule in `ALL`, wrapping — what a tile's `b` key steps.
    pub fn next(self) -> BucketRule {
        let i = Self::ALL.iter().position(|r| *r == self).expect("in ALL");
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// One slot of a request: a source pair bucketed by a rule, or an
/// expression over other slots (spec §7).
#[derive(Debug, Clone, PartialEq)]
pub enum SlotKind {
    Source {
        source: String,
        identity: String,
        rule: BucketRule,
    },
    Expr(expr::Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesSpec {
    pub slot: u8,
    pub kind: SlotKind,
}

/// One tile's whole question (spec §6.1): every slot in one round trip.
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub dataset: String,
    /// Half-open; the buckets the points cover.
    pub range: (DateTime<Utc>, DateTime<Utc>),
    /// Half-open, inside `range`; what percentiles and bins are computed over.
    pub window: (DateTime<Utc>, DateTime<Utc>),
    pub as_of: AsOf,
    pub frequency: Frequency,
    pub series: Vec<SeriesSpec>,
    /// Fractions in (0, 1); empty is off.
    pub percentiles: Vec<f64>,
    /// `None` is density off; `Some(n)` with `MIN_BINS..=MAX_BINS`.
    pub bins: Option<u32>,
}

/// The most buckets one request may ask for (spec §6.3).
pub const SERIES_POINT_CAP: u64 = 500_000;
pub const MIN_BINS: u32 = 4;
pub const MAX_BINS: u32 = 200;

fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn describe_span(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    let secs = (to - from).num_seconds().max(0);
    if secs >= 365 * 86_400 {
        format!("{}y", secs / (365 * 86_400))
    } else if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else {
        format!("{}h", secs / 3_600)
    }
}

/// The refusal a capped request is answered with (spec §6.3):
/// `1m over 3y is 1,170,000 points; the cap is 500,000`.
pub fn cap_message(frequency: Frequency, from: DateTime<Utc>, to: DateTime<Utc>, points: u64) -> String {
    format!(
        "{} over {} is {} points; the cap is {}",
        frequency.as_str(),
        describe_span(from, to),
        group_thousands(points),
        group_thousands(SERIES_POINT_CAP)
    )
}

/// What a source slot's data is worth (spec §6.4): the coverage hull,
/// the newest fetch, and the load lane's word. `None`s throughout for an
/// expression slot.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotProvenance {
    pub loaded: Option<(DateTime<Utc>, DateTime<Utc>)>,
    pub latest_received_at: Option<DateTime<Utc>>,
    pub health: Option<Health>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlotResult {
    pub slot: u8,
    /// `buckets.len()` long; `NaN` where this slot has no bucket.
    pub values: Vec<f64>,
    /// `(fraction, value)`, in request order; empty when off or when the
    /// window held nothing.
    pub percentiles: Vec<(f64, f64)>,
    /// `(lo, hi, count)` per bin, ascending; empty when off or when the
    /// window held fewer than two distinct values.
    pub bins: Vec<(f64, f64, u32)>,
    pub provenance: SlotProvenance,
}

/// Struct-of-arrays, the shape a chart wants: no tree, no grouping, no
/// attribution, so not a `Snapshot`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesResult {
    /// Epoch microseconds, ascending: the union of every source slot's buckets.
    pub buckets: Vec<i64>,
    /// In request order.
    pub slots: Vec<SlotResult>,
}

/// One series query's answer, addressed to the key that asked.
#[derive(Debug)]
pub struct SeriesOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    /// `Err` is the failure text; the tile keeps its last good model.
    pub result: Result<SeriesResult, String>,
}
```

Add `pub mod series;` to `crates/geode-core/src/lib.rs` beside `pub mod query;`. Create `series/expr.rs` as an empty placeholder with `//! Task 2.` so the module compiles (Task 2 fills it; the `SlotKind::Expr(expr::Expr)` arm needs the type — for this task define in `expr.rs` only `#[derive(Debug, Clone, PartialEq)] pub enum Expr { Slot(u8) }` with a comment `// Task 2 replaces this with the full AST.`).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-core series:: && cargo check --workspace --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-core/src/series crates/geode-core/src/lib.rs
git commit -m "core: the series query vocabulary — Frequency, BucketRule, SeriesParams, SeriesOutcome"
```

---

### Task 2: The expression parser

**Files:**
- Replace: `crates/geode-core/src/series/expr.rs`
- Test: same file (`mod tests`)

**Interfaces:**
- Produces (`geode_core::series::expr`):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Op { Add, Sub, Mul, Div }
#[derive(Debug, Clone, PartialEq)]
pub enum Ast<R> { Ref(R), Num(f64), Neg(Box<Ast<R>>), Bin(Op, Box<Ast<R>>, Box<Ast<R>>) }
/// A reference as typed: a slot handle `s3`, or an identity with an optional `@source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefName { Handle(u8), Identity { identity: String, source: Option<String> } }
/// The resolved tree the compiler consumes: references are slot numbers only.
pub type Expr = Ast<u8>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError { pub position: usize, pub message: String }
pub const ARITHMETIC_ONLY: &str = "arithmetic only: + - * / and parentheses";
pub fn parse(text: &str) -> Result<Ast<RefName>, ParseError>;
impl<R> Ast<R> {
    /// Map every reference through `f`; the first `None` is the error, naming the reference as typed.
    pub fn resolve(self, f: &mut impl FnMut(&R) -> Option<u8>) -> Result<Expr, R>;
}
impl Expr {
    /// The slots this expression reads, ascending, deduplicated.
    pub fn slots(&self) -> Vec<u8>;
}
/// The order expression slots must be compiled in so every operand precedes its user; `Err(slot)` names a slot on a cycle.
pub fn expression_order(specs: &[super::SeriesSpec]) -> Result<Vec<u8>, u8>;
impl RefName { pub fn display(&self) -> String; }   // "s3", "SPX.close", "SPX.close@kdb_hist"
```
- Grammar (spec §7): `expr := term (('+'|'-') term)*`, `term := factor (('*'|'/') factor)*`, `factor := '-' factor | '(' expr ')' | number | ref`, `ref := handle | identity ('@' source)?`, `handle := 's' digit+`. Whitespace is skipped between tokens. A `number` is `digit+ ('.' digit+)?`. An `identity` is `[A-Za-z_][A-Za-z0-9_.]*` (so `SPX.close`, `VIX`, `spx_fwd_1y`); a `source` is `[A-Za-z0-9_-]+`. An identity that cannot be spelled that way (a REST path) must be referenced by its handle — Part 4's chip shows the handle for that reason. Any other character (`^`, `%`, `,`, a `(` directly after an identifier, which would read as a function call) is `ParseError { position, message: ARITHMETIC_ONLY.into() }`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::series::{BucketRule, SeriesSpec, SlotKind};

    fn id(s: &str) -> RefName {
        RefName::Identity { identity: s.into(), source: None }
    }

    #[test]
    fn precedence_and_associativity() {
        // 1 + 2 * 3 - 4 / 2  ==  (1 + (2 * 3)) - (4 / 2)
        let ast = parse("1 + 2 * 3 - 4 / 2").unwrap();
        let expect = Ast::Bin(
            Op::Sub,
            Box::new(Ast::Bin(Op::Add, Box::new(Ast::Num(1.0)), Box::new(Ast::Bin(Op::Mul, Box::new(Ast::Num(2.0)), Box::new(Ast::Num(3.0)))))),
            Box::new(Ast::Bin(Op::Div, Box::new(Ast::Num(4.0)), Box::new(Ast::Num(2.0)))),
        );
        assert_eq!(ast, expect);
        // left-associative: a - b - c == (a - b) - c
        let ast = parse("s1 - s2 - s3").unwrap();
        assert!(matches!(ast, Ast::Bin(Op::Sub, ref l, _) if matches!(**l, Ast::Bin(Op::Sub, ..))));
    }

    #[test]
    fn unary_minus_parentheses_and_every_reference_form() {
        assert_eq!(parse("-s1").unwrap(), Ast::Neg(Box::new(Ast::Ref(RefName::Handle(1)))));
        assert_eq!(parse("-(s1 + 2.5)").unwrap(), Ast::Neg(Box::new(Ast::Bin(Op::Add, Box::new(Ast::Ref(RefName::Handle(1))), Box::new(Ast::Num(2.5))))));
        assert_eq!(parse("SPX.close").unwrap(), Ast::Ref(id("SPX.close")));
        assert_eq!(
            parse("SPX.close@kdb_hist / VIX").unwrap(),
            Ast::Bin(Op::Div, Box::new(Ast::Ref(RefName::Identity { identity: "SPX.close".into(), source: Some("kdb_hist".into()) })), Box::new(Ast::Ref(id("VIX"))))
        );
        assert_eq!(parse("  s12  ").unwrap(), Ast::Ref(RefName::Handle(12)));
        assert_eq!(parse("spx_1y").unwrap(), Ast::Ref(id("spx_1y")), "an identity may start with s and not be a handle");
        assert_eq!(parse("s1x").unwrap(), Ast::Ref(id("s1x")), "a handle is s followed by digits and nothing else");
    }

    #[test]
    fn foreign_tokens_are_refused_with_the_arithmetic_only_message() {
        for text in ["s1 ^ 2", "s1 % 2", "log(s1)", "s1, s2", "s1 & s2", "max(s1, s2)"] {
            let err = parse(text).unwrap_err();
            assert_eq!(err.message, ARITHMETIC_ONLY, "{text}");
        }
        for text in ["", "s1 +", "(s1", "s1 s2", "1.", "+ s1", "* 2"] {
            assert!(parse(text).is_err(), "{text:?} must not parse");
        }
        assert_eq!(parse("s1 ^ 2").unwrap_err().position, 3);
    }

    #[test]
    fn resolve_maps_references_to_slots_and_names_the_first_miss() {
        let ast = parse("SPX.close / s2 + VIX@rest").unwrap();
        let mut lookup = |r: &RefName| match r {
            RefName::Handle(n) => Some(*n),
            RefName::Identity { identity, source } if identity == "SPX.close" && source.is_none() => Some(1),
            _ => None,
        };
        let miss = ast.clone().resolve(&mut lookup).unwrap_err();
        assert_eq!(miss.display(), "VIX@rest");
        let mut lookup = |r: &RefName| match r {
            RefName::Handle(n) => Some(*n),
            RefName::Identity { identity, .. } if identity == "SPX.close" => Some(1),
            RefName::Identity { identity, .. } if identity == "VIX" => Some(3),
            _ => None,
        };
        let expr = ast.resolve(&mut lookup).unwrap();
        assert_eq!(expr.slots(), vec![1, 2, 3]);
    }

    fn spec(slot: u8, kind: SlotKind) -> SeriesSpec {
        SeriesSpec { slot, kind }
    }
    fn source(slot: u8) -> SeriesSpec {
        spec(slot, SlotKind::Source { source: "k".into(), identity: format!("id{slot}"), rule: BucketRule::Last })
    }
    fn expr_over(slot: u8, text: &str) -> SeriesSpec {
        let e = parse(text).unwrap().resolve(&mut |r: &RefName| match r { RefName::Handle(n) => Some(*n), _ => None }).unwrap();
        spec(slot, SlotKind::Expr(e))
    }

    #[test]
    fn expression_order_puts_operands_first_and_names_a_cycle() {
        // s4 = s3 / s1, s3 = s1 - s2: s3 must come before s4 whatever the request order.
        let specs = vec![source(1), source(2), expr_over(4, "s3 / s1"), expr_over(3, "s1 - s2")];
        assert_eq!(expression_order(&specs).unwrap(), vec![3, 4]);
        let cyclic = vec![source(1), expr_over(2, "s3 + s1"), expr_over(3, "s2 * 2")];
        let bad = expression_order(&cyclic).unwrap_err();
        assert!(bad == 2 || bad == 3);
        let self_ref = vec![expr_over(2, "s2 + 1")];
        assert_eq!(expression_order(&self_ref).unwrap_err(), 2);
        assert_eq!(expression_order(&[source(1)]).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn display_spells_a_reference_as_typed() {
        assert_eq!(RefName::Handle(7).display(), "s7");
        assert_eq!(id("VIX").display(), "VIX");
        assert_eq!(RefName::Identity { identity: "SPX.close".into(), source: Some("kdb_hist".into()) }.display(), "SPX.close@kdb_hist");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core series::expr`
Expected: compile errors (`parse`, `Op`, `RefName` not found).

- [ ] **Step 3: Implement `expr.rs`**

```rust
//! The expression language (timeseries spec §7, ruling 8): arithmetic
//! over slots, nothing else. A hand-written recursive-descent parser,
//! pure; the resolved tree names slots only, so an identity never
//! reaches the compiler as text.

use super::SeriesSpec;
use super::SlotKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

/// The tree, generic over how a reference is spelled: `RefName` as
/// parsed, `u8` once resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Ast<R> {
    Ref(R),
    Num(f64),
    Neg(Box<Ast<R>>),
    Bin(Op, Box<Ast<R>>, Box<Ast<R>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefName {
    Handle(u8),
    Identity {
        identity: String,
        source: Option<String>,
    },
}

impl RefName {
    pub fn display(&self) -> String {
        match self {
            RefName::Handle(n) => format!("s{n}"),
            RefName::Identity { identity, source: None } => identity.clone(),
            RefName::Identity { identity, source: Some(s) } => format!("{identity}@{s}"),
        }
    }
}

pub type Expr = Ast<u8>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Byte offset into the text where the parser stopped.
    pub position: usize,
    pub message: String,
}

/// The boundary, said where a trader would cross it (spec §7).
pub const ARITHMETIC_ONLY: &str = "arithmetic only: + - * / and parentheses";

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Ref(RefName),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

fn is_source_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn tokenize(text: &str) -> Result<Vec<(usize, Token)>, ParseError> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let tok = match c {
            '+' => { i += 1; Token::Plus }
            '-' => { i += 1; Token::Minus }
            '*' => { i += 1; Token::Star }
            '/' => { i += 1; Token::Slash }
            '(' => { i += 1; Token::LParen }
            ')' => { i += 1; Token::RParen }
            c if c.is_ascii_digit() => {
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'.' {
                    i += 1;
                    let frac = i;
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                    if i == frac {
                        return Err(ParseError { position: i, message: "a number needs digits after the point".into() });
                    }
                }
                let n: f64 = text[start..i].parse().map_err(|_| ParseError { position: start, message: "not a number".into() })?;
                Token::Num(n)
            }
            c if is_ident_start(c) => {
                while i < bytes.len() && is_ident_char(bytes[i] as char) {
                    i += 1;
                }
                let word = &text[start..i];
                // A `(` right after a word is a function call, which is
                // not arithmetic.
                if i < bytes.len() && bytes[i] == b'(' {
                    return Err(ParseError { position: i, message: ARITHMETIC_ONLY.into() });
                }
                let handle = word
                    .strip_prefix('s')
                    .filter(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|rest| rest.parse::<u8>().ok());
                match handle {
                    Some(n) => Token::Ref(RefName::Handle(n)),
                    None => {
                        let source = if i < bytes.len() && bytes[i] == b'@' {
                            i += 1;
                            let s = i;
                            while i < bytes.len() && is_source_char(bytes[i] as char) {
                                i += 1;
                            }
                            if i == s {
                                return Err(ParseError { position: i, message: "a source name must follow '@'".into() });
                            }
                            Some(text[s..i].to_string())
                        } else {
                            None
                        };
                        Token::Ref(RefName::Identity { identity: word.to_string(), source })
                    }
                }
            }
            _ => return Err(ParseError { position: start, message: ARITHMETIC_ONLY.into() }),
        };
        out.push((start, tok));
    }
    Ok(out)
}

struct Parser<'a> {
    toks: &'a [(usize, Token)],
    pos: usize,
    end: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.toks.get(self.pos).map(|(_, t)| t)
    }

    fn here(&self) -> usize {
        self.toks.get(self.pos).map(|(p, _)| *p).unwrap_or(self.end)
    }

    fn bump(&mut self) -> Option<&'a Token> {
        let t = self.peek();
        self.pos += 1;
        t
    }

    fn expr(&mut self) -> Result<Ast<RefName>, ParseError> {
        let mut lhs = self.term()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => Op::Add,
                Some(Token::Minus) => Op::Sub,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.term()?;
            lhs = Ast::Bin(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn term(&mut self) -> Result<Ast<RefName>, ParseError> {
        let mut lhs = self.factor()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => Op::Mul,
                Some(Token::Slash) => Op::Div,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.factor()?;
            lhs = Ast::Bin(op, Box::new(lhs), Box::new(rhs));
        }
    }

    fn factor(&mut self) -> Result<Ast<RefName>, ParseError> {
        let at = self.here();
        match self.bump() {
            Some(Token::Minus) => Ok(Ast::Neg(Box::new(self.factor()?))),
            Some(Token::LParen) => {
                let inner = self.expr()?;
                match self.bump() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(ParseError { position: self.here(), message: "expected ')'".into() }),
                }
            }
            Some(Token::Num(n)) => Ok(Ast::Num(*n)),
            Some(Token::Ref(r)) => Ok(Ast::Ref(r.clone())),
            Some(_) => Err(ParseError { position: at, message: "expected a value".into() }),
            None => Err(ParseError { position: at, message: "expected a value".into() }),
        }
    }
}

pub fn parse(text: &str) -> Result<Ast<RefName>, ParseError> {
    let toks = tokenize(text)?;
    let mut p = Parser { toks: &toks, pos: 0, end: text.len() };
    let ast = p.expr()?;
    if p.pos != toks.len() {
        return Err(ParseError { position: p.here(), message: "unexpected token".into() });
    }
    Ok(ast)
}

impl<R> Ast<R> {
    pub fn resolve(self, f: &mut impl FnMut(&R) -> Option<u8>) -> Result<Expr, R> {
        Ok(match self {
            Ast::Ref(r) => match f(&r) {
                Some(slot) => Ast::Ref(slot),
                None => return Err(r),
            },
            Ast::Num(n) => Ast::Num(n),
            Ast::Neg(inner) => Ast::Neg(Box::new(inner.resolve(f)?)),
            Ast::Bin(op, l, r) => Ast::Bin(op, Box::new(l.resolve(f)?), Box::new(r.resolve(f)?)),
        })
    }
}

impl Expr {
    pub fn slots(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.collect_slots(&mut out);
        out.sort_unstable();
        out.dedup();
        out
    }

    fn collect_slots(&self, out: &mut Vec<u8>) {
        match self {
            Ast::Ref(s) => out.push(*s),
            Ast::Num(_) => {}
            Ast::Neg(inner) => inner.collect_slots(out),
            Ast::Bin(_, l, r) => {
                l.collect_slots(out);
                r.collect_slots(out);
            }
        }
    }
}

/// A topological order over the expression slots of `specs`, operands
/// first, so the compiler can lower each expression over CTEs that
/// already exist. `Err(slot)` is a slot on a cycle (a self-reference
/// included). Source slots are leaves and are not listed.
pub fn expression_order(specs: &[SeriesSpec]) -> Result<Vec<u8>, u8> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unseen,
        Visiting,
        Done,
    }
    let exprs: Vec<(u8, &Expr)> = specs
        .iter()
        .filter_map(|s| match &s.kind {
            SlotKind::Expr(e) => Some((s.slot, e)),
            SlotKind::Source { .. } => None,
        })
        .collect();
    let mut marks = vec![Mark::Unseen; exprs.len()];
    let mut order = Vec::with_capacity(exprs.len());
    fn visit(i: usize, exprs: &[(u8, &Expr)], marks: &mut [Mark], order: &mut Vec<u8>) -> Result<(), u8> {
        match marks[i] {
            Mark::Done => return Ok(()),
            Mark::Visiting => return Err(exprs[i].0),
            Mark::Unseen => {}
        }
        marks[i] = Mark::Visiting;
        for dep in exprs[i].1.slots() {
            if let Some(j) = exprs.iter().position(|(s, _)| *s == dep) {
                visit(j, exprs, marks, order)?;
            }
        }
        marks[i] = Mark::Done;
        order.push(exprs[i].0);
        Ok(())
    }
    for i in 0..exprs.len() {
        visit(i, &exprs, &mut marks, &mut order)?;
    }
    Ok(order)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-core series:: && cargo clippy -p geode-core --all-targets -- -D warnings`
Expected: PASS. (If clippy objects to the one-line `match` arms in `tokenize`, reformat; keep behaviour.)

- [ ] **Step 5: Commit**

```bash
git add crates/geode-core/src/series/expr.rs
git commit -m "core: the series expression parser — arithmetic only, references resolved to slots, cycle order"
```

---
### Task 3: The compiler — points, percentiles, bins, coverage, as SQL text

**Files:**
- Create: `crates/geode-data/src/query/series.rs`
- Modify: `crates/geode-data/src/query/mod.rs` (`pub mod series; pub use series::{SeriesPlan, Statement, compile_series, run_series};` — `run_series` lands in Task 4, add the export then)
- Test: `crates/geode-data/src/query/series.rs` (`mod tests`, the SQL-text half)

**Interfaces:**
- Produces (`geode_data::query::series`):
```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Statement { pub sql: String, pub params: Vec<duckdb::types::Value> }
#[derive(Debug, Clone, PartialEq)]
pub struct SeriesPlan {
    pub slots: Vec<u8>,                       // request order
    pub points: Statement,                    // col 0: epoch_us(bucket) BIGINT; cols 1..: one DOUBLE per slot, request order
    pub fractions: Vec<f64>,                  // the request's percentiles
    pub percentiles: Vec<(u8, Statement)>,    // per slot when fractions non-empty: one row, one DOUBLE column per fraction (NULL when the window is empty)
    pub bin_count: u32,                       // 0 when off
    pub bins: Vec<(u8, Statement)>,           // per slot when on: rows (lo DOUBLE, hi DOUBLE, k BIGINT 1..=n, count BIGINT)
    pub coverage: Vec<(u8, Statement)>,       // per SOURCE slot: one row (epoch_us(min from_ts), epoch_us(max to_ts), epoch_us(max received_at)), all nullable
}
pub fn compile_series(schema: &SchemaSpec, params: &SeriesParams) -> Result<SeriesPlan, StoreError>;
```
- Errors are `StoreError::Series(String)`.
- SQL shapes (the tests pin them; `{table}` is `store::series::series_table(dataset)`, `{cov}` is `coverage_table(dataset)`, the CTE name of slot `n` is `s{n}` for source and expression slots alike):

Source slot CTE (live):
```sql
s1 as (
  select time_bucket(interval '1 day', ts) as b, arg_max(v, ts) as v
  from (
    select ts, arg_max(value, received_at) as v
    from series_series
    where source = ? and series_id = ? and ts >= make_timestamp(?) and ts < make_timestamp(?)
    group by ts
  )
  group by b
)
```
with `AsOf::At(t)` appending ` and received_at <= make_timestamp(?) and ts <= make_timestamp(?)` (two params, both `micros(t)`) after the range predicate. Rule aggregates: `last` → `arg_max(v, ts)`, `first` → `arg_min(v, ts)`, `mean` → `avg(v)`, `min` → `min(v)`, `max` → `max(v)`.

Expression slot CTE, operands `d1, d2, …` ascending, the first the `from` anchor:
```sql
s3 as (
  select s1.b as b, (<lowered>) as v
  from s1 join s2 on s2.b = s1.b
)
```
Lowering: `Ref(n)` → `s{n}.v`; `Num(x)` → the literal via `format!("{x:?}")` (refused at compile if not finite); `Neg(e)` → `(-(e))`; `Bin(Add|Sub|Mul, l, r)` → `((l) + (r))` etc.; `Bin(Div, l, r)` → `(case when (r) = 0 then null else (l) / (r) end)`. Expression CTEs are emitted in `expression_order`, after every source CTE.

Points statement: the CTEs, then `buckets as (select b from s1 union select b from s2)` over SOURCE slots only, then
```sql
select epoch_us(buckets.b), s1.v, s2.v, s3.v
from buckets left join s1 on s1.b = buckets.b left join s2 on s2.b = buckets.b left join s3 on s3.b = buckets.b
order by buckets.b
```
Percentiles statement for slot `n` (the same CTEs, then): `select quantile_cont(v, 0.05), quantile_cont(v, 0.5), quantile_cont(v, 0.95) from s{n} where b >= make_timestamp(?) and b < make_timestamp(?)`; params are the CTE params followed by `micros(window.0), micros(window.1)`. Fractions are formatted with `{:?}` after validation.

Bins statement for slot `n` (the same CTEs, then):
```sql
, w as (select v from s3 where b >= make_timestamp(?) and b < make_timestamp(?) and v is not null),
  m as (select min(v) as lo, max(v) as hi from w)
select m.lo, m.hi, least(width_bucket(w.v, m.lo, m.hi, 40), 40) as k, count(*)
from w, m where m.lo < m.hi
group by 1, 2, 3 order by 3
```
Coverage statement for source slot `n`: `select epoch_us(min(from_ts)), epoch_us(max(to_ts)), epoch_us(max(received_at)) from series_series_coverage where source = ? and series_id = ?`.

Validation (each an `Err(StoreError::Series(msg))`, in this order): unknown dataset; dataset not series; `range.0 >= range.1`; `window` not inside `range`; no slots; a repeated slot number; a percentile outside `(0, 1)`; `bins` outside `MIN_BINS..=MAX_BINS`; an expression naming a slot the request lacks (message names both); an expression with no slot reference; a cycle (message names the slot from `expression_order`); a non-finite literal.

- [ ] **Step 1: Write the failing tests** (`mod tests` in `query/series.rs`; the fixture builds a `SchemaSpec` with `tests_support::series_dataset()` plus a document dataset for the "not series" case)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ddl::tests_support::{cvi_dataset, series_dataset, ts};
    use duckdb::types::Value;
    use geode_core::query::{AsOf, QueryKey};
    use geode_core::series::expr::{Ast, Op, RefName, parse};
    use geode_core::series::{BucketRule, Frequency, SeriesParams, SeriesSpec, SlotKind};

    pub(super) fn schema() -> SchemaSpec {
        let mut s = SchemaSpec::default();
        s.datasets.push(series_dataset());
        s.datasets.push(cvi_dataset());
        s
    }

    pub(super) fn source(slot: u8, identity: &str, rule: BucketRule) -> SeriesSpec {
        SeriesSpec { slot, kind: SlotKind::Source { source: "demo_kdb".into(), identity: identity.into(), rule } }
    }

    pub(super) fn expr(slot: u8, text: &str) -> SeriesSpec {
        let e = parse(text).unwrap().resolve(&mut |r: &RefName| match r { RefName::Handle(n) => Some(*n), _ => None }).unwrap();
        SeriesSpec { slot, kind: SlotKind::Expr(e) }
    }

    pub(super) fn params(series: Vec<SeriesSpec>) -> SeriesParams {
        SeriesParams {
            key: QueryKey(7),
            tag: 1,
            submitted: std::time::Instant::now(),
            dataset: "series".into(),
            range: (ts("2026-01-05T00:00:00Z"), ts("2026-01-10T00:00:00Z")),
            window: (ts("2026-01-06T00:00:00Z"), ts("2026-01-09T00:00:00Z")),
            as_of: AsOf::Live,
            frequency: Frequency::D1,
            series,
            percentiles: Vec::new(),
            bins: None,
        }
    }

    fn micros(s: &str) -> Value {
        Value::BigInt(crate::store::series::micros(ts(s)))
    }

    #[test]
    fn a_live_source_slot_buckets_the_live_rows_with_its_rule() {
        let plan = compile_series(&schema(), &params(vec![source(1, "SPX.close", BucketRule::Last)])).unwrap();
        let sql = &plan.points.sql;
        assert!(sql.contains("time_bucket(interval '1 day', ts) as b, arg_max(v, ts) as v"), "{sql}");
        assert!(sql.contains("select ts, arg_max(value, received_at) as v"), "{sql}");
        assert!(sql.contains("from series_series"), "{sql}");
        assert!(sql.contains("where source = ? and series_id = ? and ts >= make_timestamp(?) and ts < make_timestamp(?)"), "{sql}");
        assert!(!sql.contains("received_at <="), "live has no as-of predicate: {sql}");
        assert!(sql.contains("buckets as (select b from s1)"), "{sql}");
        assert!(sql.trim_end().ends_with("select epoch_us(buckets.b), s1.v\nfrom buckets left join s1 on s1.b = buckets.b\norder by buckets.b"), "{sql}");
        assert_eq!(
            plan.points.params,
            vec![Value::Text("demo_kdb".into()), Value::Text("SPX.close".into()), micros("2026-01-05T00:00:00Z"), micros("2026-01-10T00:00:00Z")]
        );
        assert_eq!(plan.slots, vec![1]);
        assert!(plan.percentiles.is_empty() && plan.bins.is_empty());
        assert_eq!(plan.coverage.len(), 1);
        assert!(plan.coverage[0].1.sql.contains("from series_series_coverage where source = ? and series_id = ?"), "{}", plan.coverage[0].1.sql);
    }

    #[test]
    fn every_rule_maps_to_its_aggregate() {
        for (rule, agg) in [
            (BucketRule::Last, "arg_max(v, ts) as v"),
            (BucketRule::First, "arg_min(v, ts) as v"),
            (BucketRule::Mean, "avg(v) as v"),
            (BucketRule::Min, "min(v) as v"),
            (BucketRule::Max, "max(v) as v"),
        ] {
            let plan = compile_series(&schema(), &params(vec![source(1, "X", rule)])).unwrap();
            assert!(plan.points.sql.contains(agg), "{rule:?}: {}", plan.points.sql);
        }
    }

    #[test]
    fn an_as_of_filters_received_at_and_ts_with_two_bound_copies_of_the_instant() {
        let mut p = params(vec![source(1, "X", BucketRule::Last)]);
        p.as_of = AsOf::At(ts("2026-01-08T12:00:00Z"));
        let plan = compile_series(&schema(), &p).unwrap();
        assert!(plan.points.sql.contains("and ts < make_timestamp(?) and received_at <= make_timestamp(?) and ts <= make_timestamp(?)"), "{}", plan.points.sql);
        assert_eq!(plan.points.params.len(), 6);
        assert_eq!(plan.points.params[4], micros("2026-01-08T12:00:00Z"));
        assert_eq!(plan.points.params[5], micros("2026-01-08T12:00:00Z"));
    }

    #[test]
    fn an_expression_is_an_inner_join_of_its_operands_with_a_guarded_division() {
        let plan = compile_series(&schema(), &params(vec![source(1, "A", BucketRule::Last), source(2, "B", BucketRule::Last), expr(3, "s1 / s2")])).unwrap();
        let sql = &plan.points.sql;
        assert!(sql.contains("s3 as (\n  select s1.b as b, ((case when (s2.v) = 0 then null else (s1.v) / (s2.v) end)) as v\n  from s1 join s2 on s2.b = s1.b\n)"), "{sql}");
        assert!(sql.contains("buckets as (select b from s1 union select b from s2)"), "expressions never widen the bucket set: {sql}");
        assert!(sql.contains("select epoch_us(buckets.b), s1.v, s2.v, s3.v\n"), "{sql}");
        assert!(sql.contains("left join s3 on s3.b = buckets.b"), "{sql}");
        assert_eq!(plan.slots, vec![1, 2, 3]);
        assert_eq!(plan.coverage.iter().map(|(s, _)| *s).collect::<Vec<_>>(), vec![1, 2], "no coverage for an expression");
    }

    #[test]
    fn expressions_are_emitted_operands_first_whatever_the_request_order() {
        let plan = compile_series(&schema(), &params(vec![expr(4, "s3 - s1"), source(1, "A", BucketRule::Last), expr(3, "s1 * 2"), source(2, "B", BucketRule::Mean)])).unwrap();
        let sql = &plan.points.sql;
        let (s1, s2, s3, s4) = (sql.find("s1 as (").unwrap(), sql.find("s2 as (").unwrap(), sql.find("s3 as (").unwrap(), sql.find("s4 as (").unwrap());
        assert!(s1 < s3 && s2 < s3 && s3 < s4, "{sql}");
        assert!(sql.contains("s3 as (\n  select s1.b as b, ((s1.v) * (2.0)) as v\n  from s1\n)"), "{sql}");
        assert!(sql.contains("s4 as (\n  select s1.b as b, ((s3.v) - (s1.v)) as v\n  from s1 join s3 on s3.b = s1.b\n)"), "{sql}");
        assert!(sql.contains("select epoch_us(buckets.b), s4.v, s1.v, s3.v, s2.v\n"), "the projection is in REQUEST order: {sql}");
        assert_eq!(plan.slots, vec![4, 1, 3, 2]);
    }

    #[test]
    fn negation_and_nesting_lower_with_parentheses() {
        let plan = compile_series(&schema(), &params(vec![source(1, "A", BucketRule::Last), expr(2, "-(s1 + 1.5) * s1")])).unwrap();
        assert!(plan.points.sql.contains("(((-(((s1.v) + (1.5))))) * (s1.v)) as v"), "{}", plan.points.sql);
    }

    #[test]
    fn percentiles_and_bins_are_one_statement_per_slot_over_the_window() {
        let mut p = params(vec![source(1, "A", BucketRule::Last), expr(2, "s1 * 2")]);
        p.percentiles = vec![0.05, 0.5, 0.95];
        p.bins = Some(40);
        let plan = compile_series(&schema(), &p).unwrap();
        assert_eq!(plan.fractions, vec![0.05, 0.5, 0.95]);
        assert_eq!(plan.bin_count, 40);
        assert_eq!(plan.percentiles.len(), 2);
        let (slot, st) = &plan.percentiles[1];
        assert_eq!(*slot, 2);
        assert!(st.sql.ends_with("select quantile_cont(v, 0.05), quantile_cont(v, 0.5), quantile_cont(v, 0.95) from s2 where b >= make_timestamp(?) and b < make_timestamp(?)"), "{}", st.sql);
        assert!(st.sql.contains("s1 as ("), "the stats statements carry the CTEs: {}", st.sql);
        let n = st.params.len();
        assert_eq!(&st.params[n - 2..], &[micros("2026-01-06T00:00:00Z"), micros("2026-01-09T00:00:00Z")]);
        let (slot, st) = &plan.bins[0];
        assert_eq!(*slot, 1);
        assert!(st.sql.contains(", w as (select v from s1 where b >= make_timestamp(?) and b < make_timestamp(?) and v is not null)"), "{}", st.sql);
        assert!(st.sql.contains("least(width_bucket(w.v, m.lo, m.hi, 40), 40) as k"), "{}", st.sql);
        assert!(st.sql.contains("where m.lo < m.hi"), "{}", st.sql);
    }

    #[test]
    fn every_refusal_names_its_reason() {
        let s = schema();
        let refuse = |p: SeriesParams| compile_series(&s, &p).unwrap_err().to_string();
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.dataset = "nope".into();
        assert!(refuse(p).contains("unknown dataset 'nope'"));
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.dataset = "cvi_params".into();
        assert!(refuse(p).contains("not a series dataset"));
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.range = (ts("2026-01-10T00:00:00Z"), ts("2026-01-05T00:00:00Z"));
        assert!(refuse(p).contains("range"));
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.window = (ts("2026-01-01T00:00:00Z"), ts("2026-01-09T00:00:00Z"));
        assert!(refuse(p).contains("window"));
        assert!(refuse(params(vec![])).contains("no slots"));
        assert!(refuse(params(vec![source(1, "A", BucketRule::Last), source(1, "B", BucketRule::Last)])).contains("slot 1 twice"));
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.percentiles = vec![0.5, 1.0];
        assert!(refuse(p).contains("percentile"));
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.bins = Some(3);
        assert!(refuse(p).contains("bins"));
        let e = refuse(params(vec![source(1, "A", BucketRule::Last), expr(2, "s1 + s9")]));
        assert!(e.contains("slot 2") && e.contains("slot 9"), "{e}");
        assert!(refuse(params(vec![expr(2, "2 + 3")])).contains("reference"));
        let e = refuse(params(vec![source(1, "A", BucketRule::Last), expr(2, "s3 + s1"), expr(3, "s2")]));
        assert!(e.contains("cycle"), "{e}");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data --lib query::series`
Expected: compile error, module not found.

- [ ] **Step 3: Implement `query/series.rs` (compile half)**

```rust
//! The series query compiler (timeseries spec §6.2): one plan per
//! request — a points statement over every slot, a percentile and a
//! bins statement per slot, a coverage statement per source slot — all
//! pure string-in string-out with every value bound. The rows it reads
//! are Part 1's: live is `arg_max(value, received_at)` per `ts`, as-of
//! is `received_at <= t` (and `ts <= t`), there is no generation.
//!
//! Every stats statement carries the same CTE prefix as the points
//! statement and so re-runs the bucketing; `docs/perf.md` records what
//! that costs at a million rows and where a single grouping-sets
//! statement would take it if it ever matters.

use crate::store::StoreError;
use crate::store::series::{coverage_table, micros, series_table};
use duckdb::types::Value;
use geode_core::query::AsOf;
use geode_core::schema::SchemaSpec;
use geode_core::series::expr::{Ast, Expr, Op, expression_order};
use geode_core::series::{BucketRule, MAX_BINS, MIN_BINS, SeriesParams, SlotKind};

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub sql: String,
    pub params: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesPlan {
    pub slots: Vec<u8>,
    pub points: Statement,
    pub fractions: Vec<f64>,
    pub percentiles: Vec<(u8, Statement)>,
    pub bin_count: u32,
    pub bins: Vec<(u8, Statement)>,
    pub coverage: Vec<(u8, Statement)>,
}

fn refuse(msg: String) -> StoreError {
    StoreError::Series(msg)
}

fn aggregate(rule: BucketRule) -> &'static str {
    match rule {
        BucketRule::Last => "arg_max(v, ts)",
        BucketRule::First => "arg_min(v, ts)",
        BucketRule::Mean => "avg(v)",
        BucketRule::Min => "min(v)",
        BucketRule::Max => "max(v)",
    }
}

fn lower(e: &Expr) -> Result<String, StoreError> {
    Ok(match e {
        Ast::Ref(n) => format!("s{n}.v"),
        Ast::Num(x) => {
            if !x.is_finite() {
                return Err(refuse(format!("literal {x} is not a finite number")));
            }
            format!("{x:?}")
        }
        Ast::Neg(inner) => format!("(-({}))", lower(inner)?),
        Ast::Bin(Op::Div, l, r) => {
            let (l, r) = (lower(l)?, lower(r)?);
            format!("(case when ({r}) = 0 then null else ({l}) / ({r}) end)")
        }
        Ast::Bin(op, l, r) => {
            let sym = match op {
                Op::Add => "+",
                Op::Sub => "-",
                Op::Mul => "*",
                Op::Div => unreachable!("handled above"),
            };
            format!("(({}) {sym} ({}))", lower(l)?, lower(r)?)
        }
    })
}

fn validate(schema: &SchemaSpec, params: &SeriesParams) -> Result<Vec<u8>, StoreError> {
    let ds = schema
        .dataset(&params.dataset)
        .ok_or_else(|| refuse(format!("unknown dataset '{}'", params.dataset)))?;
    if !ds.is_series() {
        return Err(refuse(format!("dataset '{}' is not a series dataset", ds.name)));
    }
    if params.range.0 >= params.range.1 {
        return Err(refuse("range is empty (from must precede to)".into()));
    }
    if params.window.0 < params.range.0 || params.window.1 > params.range.1 || params.window.0 > params.window.1 {
        return Err(refuse("window must lie inside range".into()));
    }
    if params.series.is_empty() {
        return Err(refuse("no slots".into()));
    }
    let mut seen = std::collections::BTreeSet::new();
    for s in &params.series {
        if !seen.insert(s.slot) {
            return Err(refuse(format!("slot {} twice", s.slot)));
        }
    }
    for f in &params.percentiles {
        if !(*f > 0.0 && *f < 1.0) {
            return Err(refuse(format!("percentile {f} is not inside (0, 1)")));
        }
    }
    if let Some(n) = params.bins
        && !(MIN_BINS..=MAX_BINS).contains(&n)
    {
        return Err(refuse(format!("bins {n} is outside {MIN_BINS}..={MAX_BINS}")));
    }
    for s in &params.series {
        if let SlotKind::Expr(e) = &s.kind {
            let refs = e.slots();
            if refs.is_empty() {
                return Err(refuse(format!("slot {}: an expression must reference at least one slot", s.slot)));
            }
            if let Some(missing) = refs.iter().find(|r| !seen.contains(r)) {
                return Err(refuse(format!("slot {} references slot {missing}, which the request lacks", s.slot)));
            }
        }
    }
    expression_order(&params.series).map_err(|slot| refuse(format!("slot {slot} is on a cycle")))
}

/// The CTE prefix every statement shares, and its bound params in order.
fn ctes(params: &SeriesParams, order: &[u8]) -> Result<(String, Vec<Value>), StoreError> {
    let table = series_table(&params.dataset);
    let interval = params.frequency.interval_sql();
    let mut parts: Vec<String> = Vec::new();
    let mut bound: Vec<Value> = Vec::new();
    let as_of = match &params.as_of {
        AsOf::Live => String::new(),
        AsOf::At(_) => " and received_at <= make_timestamp(?) and ts <= make_timestamp(?)".to_string(),
    };
    for s in &params.series {
        if let SlotKind::Source { source, identity, rule } = &s.kind {
            parts.push(format!(
                "s{n} as (\n  select time_bucket({interval}, ts) as b, {agg} as v\n  from (\n    select ts, arg_max(value, received_at) as v\n    from {table}\n    where source = ? and series_id = ? and ts >= make_timestamp(?) and ts < make_timestamp(?){as_of}\n    group by ts\n  )\n  group by b\n)",
                n = s.slot,
                agg = aggregate(*rule),
            ));
            bound.push(Value::Text(source.clone()));
            bound.push(Value::Text(identity.clone()));
            bound.push(Value::BigInt(micros(params.range.0)));
            bound.push(Value::BigInt(micros(params.range.1)));
            if let AsOf::At(t) = &params.as_of {
                bound.push(Value::BigInt(micros(*t)));
                bound.push(Value::BigInt(micros(*t)));
            }
        }
    }
    for slot in order {
        let spec = params.series.iter().find(|s| s.slot == *slot).expect("order names request slots");
        let SlotKind::Expr(e) = &spec.kind else { unreachable!("order lists expressions only") };
        let deps = e.slots();
        let anchor = deps[0];
        let joins: String = deps[1..]
            .iter()
            .map(|d| format!(" join s{d} on s{d}.b = s{anchor}.b"))
            .collect();
        parts.push(format!(
            "s{n} as (\n  select s{anchor}.b as b, ({v}) as v\n  from s{anchor}{joins}\n)",
            n = slot,
            v = lower(e)?,
        ));
    }
    Ok((format!("with {}", parts.join(",\n")), bound))
}

pub fn compile_series(schema: &SchemaSpec, params: &SeriesParams) -> Result<SeriesPlan, StoreError> {
    let order = validate(schema, params)?;
    let (prefix, bound) = ctes(params, &order)?;
    let slots: Vec<u8> = params.series.iter().map(|s| s.slot).collect();
    let sources: Vec<u8> = params
        .series
        .iter()
        .filter(|s| matches!(s.kind, SlotKind::Source { .. }))
        .map(|s| s.slot)
        .collect();

    let buckets = sources
        .iter()
        .map(|n| format!("select b from s{n}"))
        .collect::<Vec<_>>()
        .join(" union ");
    let projection = slots.iter().map(|n| format!("s{n}.v")).collect::<Vec<_>>().join(", ");
    let joins: String = slots
        .iter()
        .map(|n| format!(" left join s{n} on s{n}.b = buckets.b"))
        .collect();
    let points = Statement {
        sql: format!(
            "{prefix},\nbuckets as ({buckets})\nselect epoch_us(buckets.b), {projection}\nfrom buckets{joins}\norder by buckets.b"
        ),
        params: bound.clone(),
    };

    let window = [Value::BigInt(micros(params.window.0)), Value::BigInt(micros(params.window.1))];
    let mut percentiles = Vec::new();
    if !params.percentiles.is_empty() {
        let cols = params
            .percentiles
            .iter()
            .map(|f| format!("quantile_cont(v, {f:?})"))
            .collect::<Vec<_>>()
            .join(", ");
        for n in &slots {
            let mut p = bound.clone();
            p.extend(window.iter().cloned());
            percentiles.push((
                *n,
                Statement {
                    sql: format!("{prefix}\nselect {cols} from s{n} where b >= make_timestamp(?) and b < make_timestamp(?)"),
                    params: p,
                },
            ));
        }
    }

    let mut bins = Vec::new();
    if let Some(k) = params.bins {
        for n in &slots {
            let mut p = bound.clone();
            p.extend(window.iter().cloned());
            bins.push((
                *n,
                Statement {
                    sql: format!(
                        "{prefix},\n  w as (select v from s{n} where b >= make_timestamp(?) and b < make_timestamp(?) and v is not null),\n  m as (select min(v) as lo, max(v) as hi from w)\nselect m.lo, m.hi, least(width_bucket(w.v, m.lo, m.hi, {k}), {k}) as k, count(*)\nfrom w, m where m.lo < m.hi\ngroup by 1, 2, 3 order by 3"
                    ),
                    params: p,
                },
            ));
        }
    }

    let cov = coverage_table(&params.dataset);
    let coverage = params
        .series
        .iter()
        .filter_map(|s| match &s.kind {
            SlotKind::Source { source, identity, .. } => Some((
                s.slot,
                Statement {
                    sql: format!(
                        "select epoch_us(min(from_ts)), epoch_us(max(to_ts)), epoch_us(max(received_at)) from {cov} where source = ? and series_id = ?"
                    ),
                    params: vec![Value::Text(source.clone()), Value::Text(identity.clone())],
                },
            )),
            SlotKind::Expr(_) => None,
        })
        .collect();

    Ok(SeriesPlan {
        slots,
        points,
        fractions: params.percentiles.clone(),
        percentiles,
        bin_count: params.bins.unwrap_or(0),
        bins,
        coverage,
    })
}
```

Add `pub mod series;` and `pub use series::{SeriesPlan, Statement, compile_series};` to `query/mod.rs`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-data --lib query::series && cargo clippy -p geode-data --all-targets -- -D warnings`
Expected: PASS. Adjust whitespace in the tests only if the formatter changes a `format!` string's literal newlines — it does not, but a `\n` in a string is what the tests pin.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src/query
git commit -m "data: the series query compiler — points, percentiles, bins, coverage as bound SQL"
```

---

### Task 4: `run_series` — executing a plan into a `SeriesResult`, end to end

**Files:**
- Modify: `crates/geode-data/src/query/series.rs`, `crates/geode-data/src/query/mod.rs` (export `run_series`)
- Test: `crates/geode-data/src/query/series.rs` (`mod tests`, the end-to-end half, on a real store)

**Interfaces:**
- Produces: `pub fn run_series(conn: &duckdb::Connection, plan: &SeriesPlan) -> Result<SeriesResult, duckdb::Error>`. Reads the points statement into `buckets: Vec<i64>` and one `Vec<f64>` per slot (`NULL` → `NaN`); for each slot runs its percentile statement (one row; a `NULL` column means the window was empty and the slot's `percentiles` is empty) and its bins statement (rows `(lo, hi, k, count)`; the result is `bin_count` bins `(lo + (k-1)·w, lo + k·w, count)` for `k` in `1..=bin_count` with `w = (hi - lo) / bin_count`, zero counts filled in; no rows → empty); for each source slot its coverage statement into `SlotProvenance { loaded, latest_received_at, health: None }` (an expression slot gets all `None`). `health` is filled by the service (Task 6).

- [ ] **Step 1: Write the failing tests** (append to the `mod tests` of Task 3)

```rust
    use crate::adapter::SeriesRows;
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::series::{SeriesAppendRequest, append_series};
    use geode_core::series::SeriesResult;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store.apply_schema(&series_dataset()).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        (dir, store)
    }

    /// `values` at one-minute steps from `start`, appended for `identity`
    /// under `demo_kdb` with the given `received_at`.
    fn append(store: &Store, identity: &str, start: &str, values: &[f64], received: &str) {
        let start_t = ts(start);
        let rows = SeriesRows {
            ts: (0..values.len()).map(|i| start_t + chrono::Duration::minutes(i as i64)).collect(),
            value: values.to_vec(),
        };
        let ds = series_dataset();
        append_series(
            store,
            &SeriesAppendRequest {
                dataset: &ds,
                source: "demo_kdb",
                identity,
                rows: &rows,
                span: (start_t, start_t + chrono::Duration::days(1)),
                received_at: ts(received),
            },
        )
        .unwrap();
    }

    fn run(store: &Store, p: &SeriesParams) -> SeriesResult {
        let plan = compile_series(&schema(), p).unwrap();
        run_series(store.writer(), &plan).unwrap()
    }

    fn nan_or(v: f64) -> Option<f64> {
        if v.is_nan() { None } else { Some(v) }
    }

    #[test]
    fn daily_buckets_apply_every_rule_over_the_live_rows() {
        let (_d, store) = store();
        // Jan 5: 1,2,3,4 (minutes 14:30..14:33); Jan 6: 10,20
        append(&store, "A", "2026-01-05T14:30:00Z", &[1.0, 2.0, 3.0, 4.0], "2026-01-06T09:00:00Z");
        append(&store, "A", "2026-01-06T14:30:00Z", &[10.0, 20.0], "2026-01-07T09:00:00Z");
        for (rule, day1, day2) in [
            (BucketRule::Last, 4.0, 20.0),
            (BucketRule::First, 1.0, 10.0),
            (BucketRule::Mean, 2.5, 15.0),
            (BucketRule::Min, 1.0, 10.0),
            (BucketRule::Max, 4.0, 20.0),
        ] {
            let r = run(&store, &params(vec![source(1, "A", rule)]));
            assert_eq!(r.buckets, vec![crate::store::series::micros(ts("2026-01-05T00:00:00Z")), crate::store::series::micros(ts("2026-01-06T00:00:00Z"))], "{rule:?}");
            assert_eq!(r.slots.len(), 1);
            assert_eq!(r.slots[0].slot, 1);
            assert_eq!(r.slots[0].values, vec![day1, day2], "{rule:?}");
        }
    }

    #[test]
    fn the_bucket_set_is_the_union_and_a_missing_bucket_is_nan() {
        let (_d, store) = store();
        append(&store, "A", "2026-01-05T14:30:00Z", &[1.0], "2026-01-06T09:00:00Z");
        append(&store, "A", "2026-01-07T14:30:00Z", &[3.0], "2026-01-08T09:00:00Z");
        append(&store, "B", "2026-01-06T14:30:00Z", &[5.0], "2026-01-07T09:00:00Z");
        append(&store, "B", "2026-01-07T14:30:00Z", &[7.0], "2026-01-08T09:00:00Z");
        let r = run(&store, &params(vec![source(1, "A", BucketRule::Last), source(2, "B", BucketRule::Last), expr(3, "s1 + s2")]));
        assert_eq!(r.buckets.len(), 3, "Jan 5, 6, 7");
        let a: Vec<Option<f64>> = r.slots[0].values.iter().copied().map(nan_or).collect();
        let b: Vec<Option<f64>> = r.slots[1].values.iter().copied().map(nan_or).collect();
        let e: Vec<Option<f64>> = r.slots[2].values.iter().copied().map(nan_or).collect();
        assert_eq!(a, vec![Some(1.0), None, Some(3.0)]);
        assert_eq!(b, vec![None, Some(5.0), Some(7.0)]);
        assert_eq!(e, vec![None, None, Some(10.0)], "an expression exists only where every operand does");
    }

    #[test]
    fn a_zero_denominator_is_a_gap_not_an_infinity() {
        let (_d, store) = store();
        append(&store, "A", "2026-01-05T14:30:00Z", &[6.0], "2026-01-06T09:00:00Z");
        append(&store, "B", "2026-01-05T14:30:00Z", &[0.0], "2026-01-06T09:00:00Z");
        let r = run(&store, &params(vec![source(1, "A", BucketRule::Last), source(2, "B", BucketRule::Last), expr(3, "s1 / s2")]));
        assert!(r.slots[2].values[0].is_nan());
    }

    #[test]
    fn an_as_of_before_a_correction_sees_the_original_value() {
        let (_d, store) = store();
        append(&store, "A", "2026-01-05T14:30:00Z", &[100.0], "2026-01-06T09:00:00Z");
        append(&store, "A", "2026-01-05T14:30:00Z", &[101.0], "2026-01-07T09:00:00Z");
        let live = run(&store, &params(vec![source(1, "A", BucketRule::Last)]));
        assert_eq!(live.slots[0].values, vec![101.0]);
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.as_of = AsOf::At(ts("2026-01-06T12:00:00Z"));
        let then = run(&store, &p);
        assert_eq!(then.slots[0].values, vec![100.0]);
        // and an as-of before the bar's own ts hides it entirely
        p.as_of = AsOf::At(ts("2026-01-05T12:00:00Z"));
        let earlier = run(&store, &p);
        assert!(earlier.buckets.is_empty());
    }

    #[test]
    fn percentiles_and_bins_are_computed_over_the_window_only() {
        let (_d, store) = store();
        // Jan 5 (outside the window): 1000. Jan 6..8 (inside, 1m buckets): 1..=8 at 14:30..14:37 on Jan 6.
        append(&store, "A", "2026-01-05T14:30:00Z", &[1000.0], "2026-01-06T09:00:00Z");
        append(&store, "A", "2026-01-06T14:30:00Z", &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], "2026-01-07T09:00:00Z");
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.frequency = Frequency::M1;
        p.percentiles = vec![0.5];
        p.bins = Some(4);
        let r = run(&store, &p);
        assert_eq!(r.slots[0].percentiles, vec![(0.5, 4.5)], "the 1000 outside the window is not counted");
        let bins = &r.slots[0].bins;
        assert_eq!(bins.len(), 4);
        assert_eq!(bins.iter().map(|b| b.2).collect::<Vec<_>>(), vec![2, 2, 2, 2], "{bins:?}");
        assert!((bins[0].0 - 1.0).abs() < 1e-9 && (bins[3].1 - 8.0).abs() < 1e-9, "{bins:?}");
        assert!((bins[1].0 - bins[0].1).abs() < 1e-9, "bins are contiguous");
    }

    #[test]
    fn an_empty_window_has_no_stats_and_a_constant_series_has_no_bins() {
        let (_d, store) = store();
        append(&store, "A", "2026-01-05T14:30:00Z", &[5.0, 5.0, 5.0], "2026-01-06T09:00:00Z");
        let mut p = params(vec![source(1, "A", BucketRule::Last)]);
        p.frequency = Frequency::M1;
        p.percentiles = vec![0.5];
        p.bins = Some(4);
        p.window = (ts("2026-01-08T00:00:00Z"), ts("2026-01-09T00:00:00Z"));
        let r = run(&store, &p);
        assert!(r.slots[0].percentiles.is_empty());
        assert!(r.slots[0].bins.is_empty());
        p.window = (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z"));
        let r = run(&store, &p);
        assert_eq!(r.slots[0].percentiles, vec![(0.5, 5.0)]);
        assert!(r.slots[0].bins.is_empty(), "fewer than two distinct values");
    }

    #[test]
    fn provenance_carries_the_coverage_hull_for_a_source_slot_and_nothing_for_an_expression() {
        let (_d, store) = store();
        append(&store, "A", "2026-01-05T14:30:00Z", &[1.0], "2026-01-06T09:00:00Z");
        append(&store, "A", "2026-01-07T14:30:00Z", &[2.0], "2026-01-08T09:00:00Z");
        let r = run(&store, &params(vec![source(1, "A", BucketRule::Last), expr(2, "s1 * 2")]));
        let p = &r.slots[0].provenance;
        assert_eq!(p.loaded, Some((ts("2026-01-05T14:30:00Z"), ts("2026-01-08T14:30:00Z"))));
        assert_eq!(p.latest_received_at, Some(ts("2026-01-08T09:00:00Z")));
        assert_eq!(p.health, None, "the service fills health");
        let e = &r.slots[1].provenance;
        assert_eq!((e.loaded, e.latest_received_at, e.health.clone()), (None, None, None));
        let r = run(&store, &params(vec![source(1, "NEVER", BucketRule::Last)]));
        assert!(r.buckets.is_empty());
        assert_eq!(r.slots[0].provenance.loaded, None, "an unfetched pair has no hull");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data --lib query::series::tests::daily_buckets_apply_every_rule_over_the_live_rows`
Expected: compile error, `run_series` not found.

- [ ] **Step 3: Implement `run_series`** (append to `query/series.rs`)

```rust
use crate::store::series::from_micros;
use geode_core::series::{SeriesResult, SlotProvenance, SlotResult};

fn nan_if_null(v: Option<f64>) -> f64 {
    v.unwrap_or(f64::NAN)
}

pub fn run_series(conn: &duckdb::Connection, plan: &SeriesPlan) -> Result<SeriesResult, duckdb::Error> {
    let k = plan.slots.len();
    let mut buckets: Vec<i64> = Vec::new();
    let mut values: Vec<Vec<f64>> = vec![Vec::new(); k];
    {
        let mut stmt = conn.prepare(&plan.points.sql)?;
        let mut rows = stmt.query(duckdb::params_from_iter(plan.points.params.iter()))?;
        while let Some(row) = rows.next()? {
            buckets.push(row.get::<_, i64>(0)?);
            for (i, col) in values.iter_mut().enumerate() {
                col.push(nan_if_null(row.get::<_, Option<f64>>(i + 1)?));
            }
        }
    }

    let mut slots = Vec::with_capacity(k);
    for (i, slot) in plan.slots.iter().enumerate() {
        let mut percentiles = Vec::new();
        if let Some((_, st)) = plan.percentiles.iter().find(|(s, _)| s == slot) {
            let mut stmt = conn.prepare(&st.sql)?;
            let mut rows = stmt.query(duckdb::params_from_iter(st.params.iter()))?;
            if let Some(row) = rows.next()? {
                for (j, f) in plan.fractions.iter().enumerate() {
                    match row.get::<_, Option<f64>>(j)? {
                        Some(v) => percentiles.push((*f, v)),
                        None => {
                            percentiles.clear();
                            break;
                        }
                    }
                }
            }
        }

        let mut bins = Vec::new();
        if let Some((_, st)) = plan.bins.iter().find(|(s, _)| s == slot) {
            let n = plan.bin_count as usize;
            let mut stmt = conn.prepare(&st.sql)?;
            let mut rows = stmt.query(duckdb::params_from_iter(st.params.iter()))?;
            let mut counts = vec![0u32; n];
            let mut edges: Option<(f64, f64)> = None;
            while let Some(row) = rows.next()? {
                let lo: f64 = row.get(0)?;
                let hi: f64 = row.get(1)?;
                let kk: i64 = row.get(2)?;
                let c: i64 = row.get(3)?;
                edges = Some((lo, hi));
                if (1..=n as i64).contains(&kk) {
                    counts[(kk - 1) as usize] = c as u32;
                }
            }
            if let Some((lo, hi)) = edges {
                let w = (hi - lo) / n as f64;
                bins = counts
                    .iter()
                    .enumerate()
                    .map(|(b, c)| (lo + b as f64 * w, lo + (b as f64 + 1.0) * w, *c))
                    .collect();
            }
        }

        let mut provenance = SlotProvenance { loaded: None, latest_received_at: None, health: None };
        if let Some((_, st)) = plan.coverage.iter().find(|(s, _)| s == slot) {
            let mut stmt = conn.prepare(&st.sql)?;
            let mut rows = stmt.query(duckdb::params_from_iter(st.params.iter()))?;
            if let Some(row) = rows.next()? {
                let from: Option<i64> = row.get(0)?;
                let to: Option<i64> = row.get(1)?;
                let latest: Option<i64> = row.get(2)?;
                if let (Some(f), Some(t)) = (from, to) {
                    provenance.loaded = Some((from_micros(f), from_micros(t)));
                }
                provenance.latest_received_at = latest.map(from_micros);
            }
        }

        slots.push(SlotResult {
            slot: *slot,
            values: std::mem::take(&mut values[i]),
            percentiles,
            bins,
            provenance,
        });
    }
    Ok(SeriesResult { buckets, slots })
}
```

Export `run_series` from `query/mod.rs`. If DuckDB refuses `least(width_bucket(...), n)` or the `from w, m` cross join, adjust the SQL in Task 3's `compile_series` to the nearest form DuckDB accepts, update the Task 3 text test to the new text, and say so in the report — the behaviour the Task 4 tests pin is the contract.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-data --lib query::series && cargo clippy -p geode-data --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src/query
git commit -m "data: run_series — a plan executed into a struct-of-arrays SeriesResult"
```

---
### Task 5: A second payload kind in the query pool

**Files:**
- Modify: `crates/geode-data/src/query/pool.rs` (`RequestKind` at 45, `QueryRequest` at 50, `QueryResult` at 65, `RunFn` at 106, `worker` at 294–334, `run_one` at 374; the tests' `channel_pool_with`, `boom`, `gated` and every `.snapshot` read)
- Modify: `crates/geode-data/src/service.rs` (`query`, `distinct`, `document` build a `QueryRequest`; the `result_sink` reads `r.snapshot`), `crates/geode-data/src/query/distinct.rs` (its test fixture builds a `QueryRequest` and reads `run_one`'s result), `crates/geode-data/src/query/mod.rs` (re-export `Payload`, `Work`)
- Test: `crates/geode-data/src/query/pool.rs` (`mod tests`)

**Interfaces:**
- Produces:
```rust
pub enum Payload { Snapshot(Snapshot), Series(SeriesResult) }          // Debug
pub enum Work { Query(CompiledQuery), Series(Box<SeriesPlan>) }         // replaces QueryRequest::compiled
pub enum RequestKind { Query, Distinct { column: String }, Series { pairs: Vec<(u8, String, String)> } }   // (slot, source, identity) of every SOURCE slot, so the sink can attach health
pub struct QueryRequest { pub key, pub tag, pub submitted, pub view, pub work: Work, pub grouping, pub provenance, pub kind }
pub struct QueryResult { pub id, pub key, pub tag, pub submitted, pub view, pub payload: Result<Payload, String>, pub kind }
type RunFn = fn(&duckdb::Connection, &QueryRequest) -> Result<Payload, duckdb::Error>;
```
- `run_one` dispatches on `req.work`: `Work::Query(c)` does exactly what it does today and wraps in `Payload::Snapshot`; `Work::Series(plan)` calls `run_series` and wraps in `Payload::Series`. Coalescing, interruption, cancellation, shutdown and containment are untouched: they key on `req.key`, never on the kind.

- [ ] **Step 1: Write the failing test** (in `pool.rs`'s `mod tests`; add beside the existing fixtures)

```rust
    #[test]
    fn a_series_request_rides_the_pool_and_delivers_a_series_payload() {
        use crate::adapter::SeriesRows;
        use crate::query::series::compile_series;
        use crate::store::ddl::tests_support::{series_dataset, ts};
        use crate::store::series::{SeriesAppendRequest, append_series};
        use geode_core::query::AsOf;
        use geode_core::schema::SchemaSpec;
        use geode_core::series::{BucketRule, Frequency, SeriesParams, SeriesSpec, SlotKind};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = series_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::Catalog::new(store.writer()).ensure_tables().unwrap();
        let start = ts("2026-01-05T14:30:00Z");
        append_series(
            &store,
            &SeriesAppendRequest {
                dataset: &ds,
                source: "demo_kdb",
                identity: "A",
                rows: &SeriesRows { ts: vec![start, start + chrono::Duration::minutes(1)], value: vec![1.0, 2.0] },
                span: (start, start + chrono::Duration::days(1)),
                received_at: ts("2026-01-06T09:00:00Z"),
            },
        )
        .unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let params = SeriesParams {
            key: QueryKey(3),
            tag: 9,
            submitted: Instant::now(),
            dataset: "series".into(),
            range: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            window: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            as_of: AsOf::Live,
            frequency: Frequency::D1,
            series: vec![SeriesSpec { slot: 1, kind: SlotKind::Source { source: "demo_kdb".into(), identity: "A".into(), rule: BucketRule::Last } }],
            percentiles: Vec::new(),
            bins: None,
        };
        let plan = compile_series(&schema, &params).unwrap();
        let (pool, rx) = QueryPool::spawn(&store, 1).unwrap();
        pool.submit(QueryRequest {
            key: QueryKey(3),
            tag: 9,
            submitted: Instant::now(),
            view: ViewId("series:series".into()),
            work: Work::Series(Box::new(plan)),
            grouping: Vec::new(),
            provenance: Provenance::default(),
            kind: RequestKind::Series { pairs: vec![(1, "demo_kdb".into(), "A".into())] },
        });
        let r = rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        assert_eq!((r.key, r.tag), (QueryKey(3), 9));
        assert!(matches!(r.kind, RequestKind::Series { .. }));
        match r.payload.unwrap() {
            Payload::Series(res) => {
                assert_eq!(res.slots[0].values, vec![2.0]);
                assert_eq!(res.buckets.len(), 1);
            }
            Payload::Snapshot(_) => panic!("a series request answered with a snapshot"),
        }
        pool.shutdown();
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-data --lib query::pool::tests::a_series_request_rides_the_pool_and_delivers_a_series_payload`
Expected: compile error (`Work`, `Payload` not found).

- [ ] **Step 3: Implement**

In `pool.rs`:

```rust
use crate::query::series::{SeriesPlan, run_series};
use geode_core::series::SeriesResult;

/// What a worker produced (timeseries spec §6.4): a view or document
/// query's `Snapshot`, or a series query's struct-of-arrays result. Two
/// kinds rather than a series `Snapshot` because the chart wants arrays
/// and a series has no tree, grouping or attribution to put in one.
#[derive(Debug)]
pub enum Payload {
    Snapshot(Snapshot),
    Series(SeriesResult),
}

/// The work a request carries: a compiled statement that yields a
/// `Snapshot`, or a series plan that yields a `SeriesResult`. The pool's
/// coalescing, interruption and containment never look inside.
#[derive(Debug, Clone)]
pub enum Work {
    Query(CompiledQuery),
    Series(Box<SeriesPlan>),
}
```

`RequestKind` gains `Series { pairs: Vec<(u8, String, String)> }` with the doc "the `(slot, source, identity)` of every source slot, carried through so the service's sink can attach each pair's load-lane health without re-reading the plan". `QueryRequest.compiled` becomes `work: Work`; `QueryResult.snapshot` becomes `payload: Result<Payload, String>`; `RunFn` returns `Result<Payload, duckdb::Error>`; the worker's `sink(QueryResult { … payload: outcome, … })`. `run_one`:

```rust
pub(crate) fn run_one(conn: &duckdb::Connection, req: &QueryRequest) -> Result<Payload, duckdb::Error> {
    match &req.work {
        Work::Series(plan) => run_series(conn, plan).map(Payload::Series),
        Work::Query(compiled) => {
            let mut stmt = conn.prepare(&compiled.sql)?;
            let batches: Vec<duckdb::arrow::record_batch::RecordBatch> = stmt
                .query_arrow(duckdb::params_from_iter(compiled.params.iter()))?
                .collect();
            let meta: Vec<ColumnMeta> = compiled
                .columns
                .iter()
                .map(|c| ColumnMeta {
                    name: c.name.clone(),
                    attribution_by_depth: c.attribution_by_depth.clone(),
                    scope_semantics: c.scope_semantics.clone(),
                })
                .collect();
            Snapshot::from_batches(batches, meta, req.grouping.clone(), req.provenance.clone())
                .map(Payload::Snapshot)
                .map_err(|e| duckdb::Error::InvalidParameterName(e.to_string()))
        }
    }
}
```

In the pool's tests add one helper and use it wherever a test read `r.snapshot`:

```rust
    /// The snapshot half of a result, for the tests written before a
    /// second payload kind existed.
    fn snapshot(r: QueryResult) -> Result<Snapshot, String> {
        r.payload.map(|p| match p {
            Payload::Snapshot(s) => s,
            Payload::Series(_) => panic!("a view query answered with a series"),
        })
    }
```
The injected `boom`/`gated` `RunFn`s return `Payload::Snapshot(...)` where they returned a `Snapshot`. Every `compiled: …` in a test `QueryRequest` literal becomes `work: Work::Query(…)`.

In `service.rs`: the three `QueryRequest` literals use `work: Work::Query(compiled)`; the `result_sink` closure matches `r.kind` and reads `r.payload`: for `Query` and `Distinct` a `Payload::Snapshot` is unwrapped (a `Payload::Series` under those kinds is `Err("internal: a view query answered with a series")`); add `RequestKind::Series { .. } => true` as a placeholder arm that Task 7 replaces (comment: "Task 7 maps this to `DataEvent::Series`"). In `distinct.rs`'s test fixture, `compiled: compiled.clone()` becomes `work: Work::Query(compiled.clone())` and `run_one(..).unwrap()` is unwrapped through a local `match Payload::Snapshot`.

Re-export `Payload` and `Work` from `query/mod.rs`'s `pub use pool::{…}`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-data --lib query:: && cargo test -p geode-data --lib service && cargo check --workspace --all-targets`
Expected: PASS; every pre-existing pool test still passes through the helper.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src
git commit -m "data: the query pool carries a second payload kind — Work::Series, Payload::Series, RequestKind::Series"
```

---

### Task 6: `Delivery::Series` and `Delivery::SeriesFetched` — the shell, every occupant, the bridge's broadcast

**Files:**
- Modify: `crates/geode-shell/src/module.rs` (`Delivery` at 31–55, `PlaceholderContent::deliver` at 442, `RecordingContent::deliver` at 717 and its `Recorded` enum)
- Modify: `crates/geode-shell/src/shell/occupants.rs` (`ShellView::deliver` at 60–68)
- Modify: `crates/geode-blotter/src/content.rs:124`, `crates/geode-marketdata/src/content.rs:202`, `crates/geode-diagnostics/src/lib.rs:109`, `crates/geode-shell/src/shell/tests/occupants.rs:65`
- Modify: `crates/geode-app/src/bridge.rs` (the `DataEvent::SeriesFetched { .. } => {}` arm at ~714)
- Test: `crates/geode-shell/src/shell/tests/occupants.rs`, `crates/geode-app/src/bridge.rs` (`mod tests`)

**Interfaces:**
- Produces:
```rust
// geode_shell::module
pub enum Delivery {
    Query(QueryOutcome),
    /// A series query's answer (timeseries spec §6.4), routed by the tile's key.
    Series(SeriesOutcome),
    /// A fetch finished (spec §5.4), keyed by the pair, so every visible occupant hears it; a tile holding the pair requeries on `Ok`, marks the slot on `Err`, and one holding nothing ignores it.
    SeriesFetched { source: String, identity: String, result: Result<u64, String> },
}
impl Delivery { pub fn key(&self) -> Option<QueryKey>; }   // None for SeriesFetched
// ShellView
pub fn deliver(&mut self, delivery: Delivery, window: &mut Window, cx: &mut Context<Self>);   // Some(key): the one occupant; None: every occupant of a VISIBLE tile, each handed its own clone
// module::recording
pub enum Recorded { …, Delivered(TileId, u64), SeriesFetched(TileId, String /* "{identity}@{source}" */) }
```
- Every `match delivery` gains two explicit arms. The blotter, market-data and diagnostics occupants ignore both (`Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}` with a one-line comment: this tile asks no series query and holds no pair). The recording fixture logs `Delivered(tile, outcome.tag)` for `Series` and `SeriesFetched(tile, pair)` for `SeriesFetched`.

- [ ] **Step 1: Write the failing tests**

In `crates/geode-shell/src/shell/tests/occupants.rs` (find the module's existing fixtures for a shell with a recording factory — `test_services()` carries a `rec` kind; use the same construction the tests around `WatchingContent` use, and a two-tile layout via the `ctrl+v` test binding):

```rust
    #[gpui::test]
    fn a_key_less_delivery_reaches_every_visible_occupant_and_no_hidden_one(cx: &mut TestAppContext) {
        // Two recording tiles side by side, plus one on a switched-away workspace.
        let (shell, log) = shell_with_recording_tiles(cx, 2);      // helper: existing or write it beside the fixture
        let hidden = add_recording_tile_on_workspace_two(&shell, cx); // helper likewise; workspace 1 stays active
        shell.update_in(cx, |s, window, cx| {
            s.deliver(
                Delivery::SeriesFetched { source: "demo_kdb".into(), identity: "SPX.close".into(), result: Ok(3) },
                window,
                cx,
            )
        });
        let seen: Vec<TileId> = log
            .borrow()
            .iter()
            .filter_map(|r| match r {
                Recorded::SeriesFetched(tile, pair) if pair == "SPX.close@demo_kdb" => Some(*tile),
                _ => None,
            })
            .collect();
        assert_eq!(seen.len(), 2, "both visible tiles, once each: {seen:?}");
        assert!(!seen.contains(&hidden), "a tile on a switched-away workspace is not told");
    }

    #[gpui::test]
    fn a_series_outcome_is_routed_to_its_key_alone(cx: &mut TestAppContext) {
        let (shell, log) = shell_with_recording_tiles(cx, 2);
        let target = TileId(1);
        shell.update_in(cx, |s, window, cx| {
            s.deliver(
                Delivery::Series(SeriesOutcome { key: QueryKey(target.0), tag: 42, submitted: std::time::Instant::now(), result: Ok(SeriesResult::default()) }),
                window,
                cx,
            )
        });
        let delivered: Vec<(TileId, u64)> = log.borrow().iter().filter_map(|r| match r { Recorded::Delivered(t, tag) => Some((*t, *tag)), _ => None }).collect();
        assert_eq!(delivered, vec![(target, 42)]);
    }
```

If no two-tile recording fixture exists, build the smallest one: open the shell with `test_services()`, dispatch the test layer's `ctrl+v` once (a second recording tile appears), and for the hidden tile switch to workspace 2 (`workspace::next` or the existing test helper), add a tile there, switch back. The fixture must return the shared `Rc<RefCell<Vec<Recorded>>>` log the recording factory writes.

In `crates/geode-app/src/bridge.rs`'s `mod tests`, beside `the_drain_task_ends_on_the_first_event_after_the_window_closes` (reuse its `open_test_window` and `Bridge` literal):

```rust
    #[gpui::test]
    fn a_series_fetched_event_is_broadcast_to_the_shell(cx: &mut gpui::TestAppContext) {
        let (window, log) = open_test_window_with_recording_tile(cx);   // a variant of open_test_window whose shell holds one recording tile and returns its log
        let (handle, _rx) = DataHandle::for_tests();
        let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge { /* as the sibling test builds it */ events: rx, .. };
        cx.update(|cx| attach(&bridge, window, cx));
        tx.try_send(DataEvent::SeriesFetched { source: "demo_kdb".into(), identity: "VIX".into(), result: Err("no such symbol".into()) }).unwrap();
        cx.run_until_parked();
        assert!(log.borrow().iter().any(|r| matches!(r, Recorded::SeriesFetched(_, pair) if pair == "VIX@demo_kdb")), "{:?}", log.borrow());
    }
```

`open_test_window_with_recording_tile`: if `test_shell_services()` in that module already uses a roster with the recording factory (check `crates/geode-shell/src/module.rs::recording` for the factory constructor and how `shell/tests/mod.rs::test_services` registers it), build on it; otherwise register `recording::RecordingFactory::new(log.clone())` (whatever its constructor is called) into the roster and put a session with one tile of that kind, as the shell's own tests do.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-shell --features test-support a_key_less_delivery`
Expected: compile error, `Delivery::SeriesFetched` not found.

- [ ] **Step 3: Implement**

`module.rs`:

```rust
use geode_core::series::SeriesOutcome;

#[derive(Debug)]
pub enum Delivery {
    Query(QueryOutcome),
    /// A series query's answer (timeseries spec §6.4), routed by the
    /// tile's key like a `Query`.
    Series(SeriesOutcome),
    /// A fetch finished (timeseries spec §5.4). Keyed by the
    /// `(identity, source)` pair, not a tile: `ShellView::deliver` hands
    /// one to EVERY visible occupant, each its own copy, and a tile
    /// holding the pair requeries on `Ok` (an `Ok(0)` too — the span is
    /// covered, whether just now or already) or marks the slot on `Err`.
    /// A tile holding nothing of the kind ignores it. Plain strings so
    /// the shell, which never names `geode-data`, can carry it.
    SeriesFetched {
        source: String,
        identity: String,
        result: Result<u64, String>,
    },
}

impl Delivery {
    /// The tile id (as a bare `QueryKey`) this delivery is addressed to,
    /// or `None` for one addressed to every visible tile.
    pub fn key(&self) -> Option<QueryKey> {
        match self {
            Delivery::Query(outcome) => Some(outcome.key),
            Delivery::Series(outcome) => Some(outcome.key),
            Delivery::SeriesFetched { .. } => None,
        }
    }
}
```

`occupants.rs`:

```rust
    /// Route a delivery (§5.1): one with a key to the tile whose id it
    /// is, dropped if that tile is gone; one without (`SeriesFetched`)
    /// to every occupant of a tile on screen, each handed its own copy,
    /// since `Delivery` is not `Clone` (`QueryOutcome` is not) and an
    /// occupant takes it by value. Hidden tiles are skipped on purpose:
    /// they hold no subscription and requery on `set_visible(true)`.
    pub fn deliver(&mut self, delivery: Delivery, window: &mut Window, cx: &mut Context<Self>) {
        match delivery.key() {
            Some(key) => {
                if let Some(o) = self.occupants.get(&TileId(key.0)) {
                    o.content.deliver(delivery, window, cx);
                }
            }
            None => {
                let Delivery::SeriesFetched { source, identity, result } = delivery else {
                    unreachable!("the only key-less delivery is SeriesFetched");
                };
                let mut keys = Vec::new();
                self.visible_tile_keys(&mut keys);
                for key in keys {
                    if let Some(o) = self.occupants.get(&TileId(key.0)) {
                        o.content.deliver(
                            Delivery::SeriesFetched { source: source.clone(), identity: identity.clone(), result: result.clone() },
                            window,
                            cx,
                        );
                    }
                }
            }
        }
    }
```

(`visible_tile_keys` already filters placeholders and covers docks; that is the visible set the flip barrier uses.) The placeholder's and the three modules' `deliver` gain `Delivery::Series(_) | Delivery::SeriesFetched { .. } => {}` with the comment named above; `WatchingContent` in the shell tests likewise. The recording fixture:

```rust
                Delivery::Series(outcome) => {
                    self.log.borrow_mut().push(Recorded::Delivered(self.tile, outcome.tag));
                }
                Delivery::SeriesFetched { source, identity, .. } => {
                    self.log.borrow_mut().push(Recorded::SeriesFetched(self.tile, format!("{identity}@{source}")));
                }
```

`bridge.rs`: replace the empty arm with

```rust
                    DataEvent::SeriesFetched { source, identity, result } => {
                        shell.update(cx, |s, cx| {
                            s.deliver(Delivery::SeriesFetched { source, identity, result }, window, cx)
                        });
                    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-shell --features test-support && cargo test -p geode-app && cargo check --workspace --all-targets && cargo check -p geode-shell --features test-support --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell crates/geode-blotter crates/geode-marketdata crates/geode-diagnostics crates/geode-app/src/bridge.rs
git commit -m "shell: Delivery::Series and Delivery::SeriesFetched — routed by key, or broadcast to every visible occupant"
```

---
### Task 7: `Request::Series` end to end — service, handle, `DataEvent::Series`, the bridge arm

**Files:**
- Modify: `crates/geode-data/src/service.rs` (`DataEvent` at 68; `HealthTracker` ~424 gains a read; `result_sink` ~850; a new `DataService::series` beside `document` ~1705)
- Modify: `crates/geode-data/src/handle.rs` (`Request::Series`, `DataHandle::series`, the serve arm)
- Modify: `crates/geode-app/src/bridge.rs` (a `DataEvent::Series` arm)
- Test: `crates/geode-data/src/service.rs` (`mod tests`), `crates/geode-data/src/handle.rs` (`mod tests`), `crates/geode-app/src/bridge.rs` (`mod tests`)

**Interfaces:**
- Produces:
```rust
DataEvent::Series(SeriesOutcome)
impl HealthTracker { pub(crate) fn load_lane(&self, source: &str, batch: &str) -> Option<Health>; }
impl DataService {
    /// Cap, compile, submit. `Err` is the request's own outcome (the serve loop sends it as a `SeriesOutcome`).
    pub fn series(&self, params: &SeriesParams) -> Result<QueryId, StoreError>;
}
Request::Series(SeriesParams)
impl DataHandle { pub fn series(&self, params: SeriesParams) -> bool; }
```
- The `result_sink`'s `RequestKind::Series { pairs }` arm maps `Payload::Series(mut res)` to `DataEvent::Series(SeriesOutcome { key, tag, submitted, result: Ok(res) })` after filling, for every `(slot, source, identity)` in `pairs`, that slot's `provenance.health = tracker.load_lane(&source, &format!("{identity}@{source}"))`; `Err(e)` maps to `result: Err(e)`; a `Payload::Snapshot` under this kind is `Err("internal: a series request answered with a snapshot")`.
- `DataService::series`: `if params.frequency.buckets_in(range.0, range.1) > SERIES_POINT_CAP { return Err(StoreError::Series(cap_message(..))) }` before `compile_series`; submits `QueryRequest { key, tag, submitted, view: ViewId(format!("series:{}", dataset)), work: Work::Series(Box::new(plan)), grouping: vec![], provenance: Provenance::default(), kind: RequestKind::Series { pairs } }` where `pairs` lists every `SlotKind::Source` in request order.
- The serve arm mirrors `Document`'s: a compile or cap `Err` becomes `DataEvent::Series(SeriesOutcome { key, tag, submitted, result: Err(e.to_string()) })`.
- The bridge: `DataEvent::Series(outcome) => shell.update(cx, |s, cx| s.deliver(Delivery::Series(outcome), window, cx))`.

- [ ] **Step 1: Write the failing tests**

`service.rs` (beside `fetch_service`; reuse `FakeFetchAdapter`, `next_series_fetched`, `fetch_params`):

```rust
    fn next_series(rx: &std::sync::mpsc::Receiver<DataEvent>) -> geode_core::series::SeriesOutcome {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Series(o) => return o,
                _ => continue,
            }
        }
    }

    fn series_params(identity: &str) -> geode_core::series::SeriesParams {
        use geode_core::series::*;
        SeriesParams {
            key: QueryKey(7),
            tag: 5,
            submitted: Instant::now(),
            dataset: "series".into(),
            range: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            window: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            as_of: AsOf::Live,
            frequency: Frequency::D1,
            series: vec![SeriesSpec { slot: 1, kind: SlotKind::Source { source: "kdb_hist".into(), identity: identity.into(), rule: BucketRule::Last } }],
            percentiles: vec![0.5],
            bins: None,
        }
    }

    #[test]
    fn a_series_request_answers_with_the_bucketed_values_and_the_pairs_health() {
        let (_d, _calls, service, rx) = fetch_service(None);
        service.fetch(&fetch_params("SPX.close", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"));
        let _ = next_series_fetched(&rx);
        service.series(&series_params("SPX.close")).unwrap();
        let o = next_series(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(7), 5));
        let r = o.result.unwrap();
        assert_eq!(r.buckets.len(), 1);
        assert_eq!(r.slots[0].values, vec![2.0], "the FakeFetch's three bars are 0, NaN (dropped), 2; last wins");
        assert_eq!(r.slots[0].percentiles, vec![(0.5, 2.0)]);
        assert_eq!(r.slots[0].provenance.health, Some(Health::Ok), "the load lane's word rides the outcome");
        assert!(r.slots[0].provenance.loaded.is_some());
    }

    #[test]
    fn a_failed_pairs_health_rides_its_slot() {
        let (_d, _calls, service, rx) = fetch_service(None);
        service.fetch(&fetch_params("broken", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z"));
        let _ = next_series_fetched(&rx);
        service.series(&series_params("broken")).unwrap();
        let o = next_series(&rx);
        let r = o.result.unwrap();
        assert!(r.buckets.is_empty());
        assert!(matches!(r.slots[0].provenance.health, Some(Health::Failed { .. })), "{:?}", r.slots[0].provenance);
    }

    #[test]
    fn a_capped_request_is_refused_before_compilation() {
        let (_d, _calls, service, _rx) = fetch_service(None);
        let mut p = series_params("SPX.close");
        p.frequency = geode_core::series::Frequency::M1;
        p.range = (ts("2026-01-05T00:00:00Z"), ts("2029-01-05T00:00:00Z"));
        p.window = p.range;
        let e = service.series(&p).unwrap_err().to_string();
        assert!(e.contains("1m over 3y is ") && e.contains("; the cap is 500,000"), "{e}");
    }

    #[test]
    fn a_compile_error_is_the_requests_own_outcome_through_the_handle() {
        // Through `DataService::spawn` so the serve loop's error arm is what answers.
        let dir = tempfile::tempdir().unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(crate::store::ddl::tests_support::series_dataset());
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn(
            DataServiceConfig { db_path: dir.path().join("geode.duckdb"), schema, views: Vec::new(), dimensions: DerivedDimensions::default(), query_workers: 1, sources: Vec::new(), adapters: Default::default(), documents: Default::default() },
            sink,
        );
        let mut p = series_params("X");
        p.dataset = "nope".into();
        assert!(handle.series(p));
        let o = next_series(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(7), 5));
        assert!(o.result.unwrap_err().contains("unknown dataset 'nope'"));
        handle.shutdown();
    }
```

`handle.rs`'s tests: extend `fetch_and_identities_are_queued_as_requests` (or add a sibling) so `handle.series(params)` yields `Request::Series(p)` with `p.tag == 5`.

`bridge.rs`'s tests, beside Task 6's broadcast test: `a_series_outcome_is_routed_to_its_tile` — send `DataEvent::Series(SeriesOutcome { key: QueryKey(<the recording tile's id>), tag: 11, .. Ok(SeriesResult::default()) })` and assert the log holds `Recorded::Delivered(tile, 11)`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data --lib service::tests::a_series_request_answers`
Expected: compile error, `DataEvent::Series` / `DataService::series` not found.

- [ ] **Step 3: Implement**

`service.rs`:

```rust
    /// A series query's answer (timeseries spec §6.4), routed by the
    /// tile's key like `Query`.
    Series(SeriesOutcome),
```

`HealthTracker`:

```rust
    /// The load lane's current word for one batch — what a series
    /// outcome carries per slot (spec §6.4). `None` when nothing was ever
    /// reported for it, which is the same as clean.
    pub(crate) fn load_lane(&self, source: &str, batch: &str) -> Option<Health> {
        let sources = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        sources.get(source)?.load.get(batch).map(|v| v.health.clone())
    }
```

The `result_sink` (capture `health_tracker` — clone the `Arc` into the closure beside `sink`):

```rust
                RequestKind::Series { pairs } => {
                    let result = match r.payload {
                        Ok(Payload::Series(mut res)) => {
                            for (slot, source, identity) in &pairs {
                                let key = format!("{identity}@{source}");
                                if let Some(s) = res.slots.iter_mut().find(|s| s.slot == *slot) {
                                    s.provenance.health = health_tracker.load_lane(source, &key);
                                }
                            }
                            Ok(res)
                        }
                        Ok(Payload::Snapshot(_)) => Err("internal: a series request answered with a snapshot".to_string()),
                        Err(e) => Err(e),
                    };
                    sink(DataEvent::Series(SeriesOutcome { key: r.key, tag: r.tag, submitted: r.submitted, result }))
                }
```

`DataService::series`:

```rust
    /// The series query (timeseries spec §6): capped, compiled, and
    /// submitted like a view query, so it shares the pool's cancellation
    /// and per-key coalescing and comes back as `DataEvent::Series`.
    pub fn series(&self, params: &SeriesParams) -> Result<QueryId, StoreError> {
        let points = params.frequency.buckets_in(params.range.0, params.range.1);
        if points > SERIES_POINT_CAP {
            return Err(StoreError::Series(cap_message(params.frequency, params.range.0, params.range.1, points)));
        }
        let plan = compile_series(&self.config.schema, params)?;
        let pairs = params
            .series
            .iter()
            .filter_map(|s| match &s.kind {
                SlotKind::Source { source, identity, .. } => Some((s.slot, source.clone(), identity.clone())),
                SlotKind::Expr(_) => None,
            })
            .collect();
        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            view: ViewId(format!("series:{}", params.dataset)),
            work: Work::Series(Box::new(plan)),
            grouping: Vec::new(),
            provenance: Provenance::default(),
            kind: RequestKind::Series { pairs },
        }))
    }
```

`handle.rs`: the `Request::Series(SeriesParams)` variant (doc: "The timeseries viewer's series query (timeseries spec §6): one round trip per tile, answered as `DataEvent::Series`"), `DataHandle::series`, and the serve arm:

```rust
            Request::Series(params) => {
                if let Err(e) = service.series(&params) {
                    // Same rule as `Query`: a cap or compile failure is this
                    // key's outcome, not a lost request.
                    sink(DataEvent::Series(SeriesOutcome {
                        key: params.key,
                        tag: params.tag,
                        submitted: params.submitted,
                        result: Err(e.to_string()),
                    }));
                }
            }
```

`bridge.rs`: the `DataEvent::Series` arm next to `DataEvent::Query`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p geode-data --lib && cargo test -p geode-app && cargo check --workspace --all-targets && cargo check -p geode-shell --features test-support --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-data/src crates/geode-app/src/bridge.rs
git commit -m "data+app: Request::Series end to end — cap, compile, submit, DataEvent::Series with per-slot health, routed to the tile"
```

---

### Task 8: Harness entries, the 1M-row bench, docs, spec as-built

**Files:**
- Modify: `scripts/mutation-check.sh` (append before the anchors-only block; re-anchor anything the branch moved; update the count in CLAUDE.md with `grep -c '^run_mutation "'`)
- Create: `crates/geode-data/benches/series_query.rs`; modify `crates/geode-data/Cargo.toml` (`[[bench]] name = "series_query" harness = false`), `docs/perf.md`
- Modify: `CLAUDE.md` (status row, load-bearing rules), `docs/phase-history.md`, the spec (`### 6.6 As built (Part 2)` and a §7 note)

- [ ] **Step 1: Harness entries** (anchors must be verbatim substrings of the code as written; re-read each file before anchoring, run each entry filtered, then `--anchors-only`)

```sh
run_mutation "series query: live is the oldest version, not the newest" \
  crates/geode-data/src/query/series.rs \
  'select ts, arg_max(value, received_at) as v' \
  'select ts, arg_min(value, received_at) as v' \
  geode-data an_as_of_before_a_correction_sees_the_original_value

run_mutation "series query: as-of drops the received_at filter" \
  crates/geode-data/src/query/series.rs \
  '" and received_at <= make_timestamp(?) and ts <= make_timestamp(?)".to_string()' \
  '" and ts <= make_timestamp(?) and ts <= make_timestamp(?)".to_string()' \
  geode-data an_as_of_before_a_correction_sees_the_original_value

run_mutation "series query: an expression is an outer join of its operands" \
  crates/geode-data/src/query/series.rs \
  '.map(|d| format!(" join s{d} on s{d}.b = s{anchor}.b"))' \
  '.map(|d| format!(" left join s{d} on s{d}.b = s{anchor}.b"))' \
  geode-data the_bucket_set_is_the_union_and_a_missing_bucket_is_nan

run_mutation "series query: division by zero is not guarded" \
  crates/geode-data/src/query/series.rs \
  'format!("(case when ({r}) = 0 then null else ({l}) / ({r}) end)")' \
  'format!("(({l}) / ({r}))")' \
  geode-data a_zero_denominator_is_a_gap_not_an_infinity

run_mutation "series query: an expression widens the bucket set" \
  crates/geode-data/src/query/series.rs \
  '        .filter(|s| matches!(s.kind, SlotKind::Source { .. }))
        .map(|s| s.slot)
        .collect();' \
  '        .map(|s| s.slot)
        .collect();' \
  geode-data an_expression_is_an_inner_join_of_its_operands_with_a_guarded_division

run_mutation "series query: stats ignore the window" \
  crates/geode-data/src/query/series.rs \
  'from s{n} where b >= make_timestamp(?) and b < make_timestamp(?)")' \
  'from s{n} where b >= make_timestamp(?) or b < make_timestamp(?)")' \
  geode-data percentiles_and_bins_are_computed_over_the_window_only

run_mutation "series query: the top bin folds nothing" \
  crates/geode-data/src/query/series.rs \
  'least(width_bucket(w.v, m.lo, m.hi, {k}), {k}) as k' \
  'width_bucket(w.v, m.lo, m.hi, {k}) as k' \
  geode-data percentiles_and_bins_are_computed_over_the_window_only

run_mutation "series query: a null bucket is zero, not a gap" \
  crates/geode-data/src/query/series.rs \
  '    v.unwrap_or(f64::NAN)' \
  '    v.unwrap_or(0.0)' \
  geode-data the_bucket_set_is_the_union_and_a_missing_bucket_is_nan

run_mutation "series query: the cap is never checked" \
  crates/geode-data/src/service.rs \
  '        if points > SERIES_POINT_CAP {' \
  '        if false {' \
  geode-data a_capped_request_is_refused_before_compilation

run_mutation "series query: the pair's health is not attached" \
  crates/geode-data/src/service.rs \
  '                                    s.provenance.health = health_tracker.load_lane(source, &key);' \
  '                                    let _ = (&key, &health_tracker, &mut s.provenance);' \
  geode-data a_failed_pairs_health_rides_its_slot

run_mutation "expr: precedence is flat" \
  crates/geode-core/src/series/expr.rs \
  '        let mut lhs = self.term()?;' \
  '        let mut lhs = self.factor()?;' \
  geode-core precedence_and_associativity

run_mutation "expr: a cycle is not detected" \
  crates/geode-core/src/series/expr.rs \
  '            Mark::Visiting => return Err(exprs[i].0),' \
  '            Mark::Visiting => return Ok(()),' \
  geode-core expression_order_puts_operands_first_and_names_a_cycle

run_mutation "expr: a function call is accepted" \
  crates/geode-core/src/series/expr.rs \
  '                if i < bytes.len() && bytes[i] == b'"'"'('"'"' {
                    return Err(ParseError { position: i, message: ARITHMETIC_ONLY.into() });
                }' \
  '' \
  geode-core foreign_tokens_are_refused_with_the_arithmetic_only_message

run_mutation "shell: a key-less delivery reaches hidden tiles too" \
  crates/geode-shell/src/shell/occupants.rs \
  '                self.visible_tile_keys(&mut keys);' \
  '                keys.extend(self.occupants.keys().map(|t| QueryKey(t.0)));' \
  geode-shell a_key_less_delivery_reaches_every_visible_occupant_and_no_hidden_one

run_mutation "pool: a series work item runs the view path" \
  crates/geode-data/src/query/pool.rs \
  '        Work::Series(plan) => run_series(conn, plan).map(Payload::Series),' \
  '        Work::Series(_) => Err(duckdb::Error::InvalidParameterName("series".into())),' \
  geode-data a_series_request_rides_the_pool_and_delivers_a_series_payload
```

For the `"expr: a function call is accepted"` entry the replacement is the empty string; if `run_mutation` refuses an empty `to`, replace with a no-op statement `let _ = ();` instead. If a named test does not catch its entry (`caught*` or `SURVIVED`), find the assertion that does and rename the 6th argument — never leave an entry claiming a test it does not have. Run: `zsh scripts/mutation-check.sh "series query:"`, then `"expr:"`, `"shell: a key-less"`, `"pool: a series"`, then `--anchors-only`.

- [ ] **Step 2: The bench** (`crates/geode-data/benches/series_query.rs`)

```rust
//! The series query at a million rows (timeseries spec §11.3) against
//! the §7.1 requery budget of 50 ms: four identities of one-minute bars
//! over a year (~ 250,000 rows each, 1,000,000 total), asked for at `1d`
//! over the year (one slot, and four slots plus a ratio expression) and
//! at `1m` over a month with percentiles and bins on. Rows are appended
//! through `append_series` in day-sized chunks, once per bench process;
//! the timed half is `DataService::series` plus the wait for its
//! `DataEvent::Series`, the round trip a tile pays.

use chrono::{DateTime, Duration, Utc};
use criterion::{Criterion, criterion_group, criterion_main};
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::{AsOf, QueryKey};
use geode_core::schema::SchemaSpec;
use geode_core::series::expr::{Ast, Op};
use geode_core::series::{BucketRule, Frequency, SeriesParams, SeriesSpec, SlotKind};
use geode_data::adapter::SeriesRows;
use geode_data::service::{DataEvent, DataService, DataServiceConfig};
use geode_data::store::series::{SeriesAppendRequest, append_series};
use geode_data::store::{Catalog, Store};
use std::hint::black_box;

const IDENTITIES: [&str; 4] = ["A", "B", "C", "D"];

fn schema() -> SchemaSpec {
    let text = "[series]\nfamily = \"series\"\n";
    let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
    SchemaSpec::from_doc(&doc).0
}

fn service() -> (tempfile::TempDir, DataService, std::sync::mpsc::Receiver<DataEvent>, DateTime<Utc>) {
    let dir = tempfile::tempdir().unwrap();
    let schema = schema();
    let ds = schema.dataset("series").unwrap().clone();
    let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
    store.apply_schema(&ds).unwrap();
    Catalog::new(store.writer()).ensure_tables().unwrap();
    let start: DateTime<Utc> = "2025-01-06T14:30:00Z".parse().unwrap();
    // 250 weekdays × 1,000 minutes ≈ 250,000 rows per identity
    let mut day = start;
    for d in 0..250 {
        let base = day + Duration::days(0);
        for (i, id) in IDENTITIES.iter().enumerate() {
            let rows = SeriesRows {
                ts: (0..1000).map(|m| base + Duration::minutes(m)).collect(),
                value: (0..1000).map(|m| 100.0 + i as f64 + ((d * 1000 + m) % 97) as f64 * 0.01).collect(),
            };
            append_series(&store, &SeriesAppendRequest { dataset: &ds, source: "bench", identity: id, rows: &rows, span: (base, base + Duration::days(1)), received_at: base + Duration::days(1) }).unwrap();
        }
        day += Duration::days(if day.format("%u").to_string() == "5" { 3 } else { 1 });
    }
    drop(store);
    let (service, rx) = DataService::open_channel(DataServiceConfig {
        db_path: dir.path().join("geode.duckdb"),
        schema,
        views: Vec::new(),
        dimensions: DerivedDimensions::default(),
        query_workers: 4,
        sources: Vec::new(),
        adapters: Default::default(),
        documents: Default::default(),
    })
    .unwrap();
    (dir, service, rx, start)
}

fn source(slot: u8, id: &str) -> SeriesSpec {
    SeriesSpec { slot, kind: SlotKind::Source { source: "bench".into(), identity: id.into(), rule: BucketRule::Last } }
}

fn round_trip(svc: &DataService, rx: &std::sync::mpsc::Receiver<DataEvent>, params: &SeriesParams) -> usize {
    svc.series(params).unwrap();
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(120)).expect("no result") {
            DataEvent::Series(o) => return o.result.expect("series query failed").buckets.len(),
            _ => continue,
        }
    }
}

fn bench(c: &mut Criterion) {
    let (_dir, svc, rx, start) = service();
    let year = (start, start + Duration::days(365));
    let base = |series: Vec<SeriesSpec>, frequency: Frequency, range: (DateTime<Utc>, DateTime<Utc>), stats: bool| SeriesParams {
        key: QueryKey(1), tag: 0, submitted: std::time::Instant::now(), dataset: "series".into(),
        range, window: range, as_of: AsOf::Live, frequency, series,
        percentiles: if stats { vec![0.05, 0.5, 0.95] } else { Vec::new() },
        bins: if stats { Some(40) } else { None },
    };
    let one = base(vec![source(1, "A")], Frequency::D1, year, false);
    c.bench_function("series_query/1_slot_1d_1y", |b| b.iter(|| black_box(round_trip(&svc, &rx, &one))));
    let ratio = Ast::Bin(Op::Div, Box::new(Ast::Ref(1)), Box::new(Ast::Ref(2)));
    let four = base(vec![source(1, "A"), source(2, "B"), source(3, "C"), source(4, "D"), SeriesSpec { slot: 5, kind: SlotKind::Expr(ratio) }], Frequency::D1, year, false);
    c.bench_function("series_query/4_slots_plus_ratio_1d_1y", |b| b.iter(|| black_box(round_trip(&svc, &rx, &four))));
    let month = (start, start + Duration::days(31));
    let stats = base(vec![source(1, "A"), source(2, "B")], Frequency::M1, month, true);
    c.bench_function("series_query/2_slots_1m_1mo_with_stats", |b| b.iter(|| black_box(round_trip(&svc, &rx, &stats))));
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

Run `cargo bench -p geode-data --bench series_query` and record the three medians in `docs/perf.md` under `## Timeseries (spec §4.4, Part 1)`'s sibling `## Timeseries series query (spec §6, Part 2)`, house style: conditions, a three-row table, and a "known gap" paragraph stating that every stats statement re-runs the CTE prefix (so a request with both stats on runs `1 + 2k` bucketing passes for `k` slots) and that a single grouping-sets statement is the fix if the `1m` case is over budget. If any median exceeds 50 ms, say so in the report; do not change the compiler in this task.

- [ ] **Step 3: Docs**

- CLAUDE.md: a status row `Timeseries Part 2 (series query)` after Part 1's, and under **Data layer** two rules: "Series query: one `SeriesPlan` per request — points over the UNION of source buckets with `NULL` → `NaN`, an expression over the INNER join of its operands with a guarded division, stats statements per slot over the window that re-run the CTE prefix; every value is bound, only table names, intervals, aggregates and `s{n}` are text. `Delivery::Series` routes by key; `Delivery::SeriesFetched` has no key and `ShellView::deliver` hands every VISIBLE occupant its own copy." and "`Payload::{Snapshot, Series}` and `Work::{Query, Series}` in the pool: coalescing, interruption and containment never look inside." Update the harness count.
- `docs/phase-history.md`: one paragraph in the existing style.
- The spec: `### 6.6 As built (Part 2)` recording: `SeriesParams`/`SeriesOutcome` live in `geode_core::series` (not `query.rs`); `submitted: Instant` was added to both for the §7.1 timing readout; `SlotProvenance.health` is filled by the service from the tracker's load lane, the hull and `latest_received_at` by the plan's coverage statement; stats are one statement per slot each re-running the CTE prefix (with the bench numbers and the grouping-sets fallback named); the identity grammar of §7 (`[A-Za-z_][A-Za-z0-9_.]*`, `@[A-Za-z0-9_-]+`) and that a REST-path identity is referenced by its handle; `Delivery::key()` is `Option<QueryKey>` and the broadcast rule; `RequestKind::Series { pairs }` as the way health reaches the outcome.

- [ ] **Step 4: Full verification**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo check -p geode-shell --features test-support --all-targets && cargo bench --workspace --no-run && zsh scripts/mutation-check.sh --anchors-only`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add scripts/mutation-check.sh crates/geode-data/benches crates/geode-data/Cargo.toml docs CLAUDE.md
git commit -m "docs+harness+bench: timeseries Part 2 — entries, the 1M-row series query bench, as-built"
```

Then finish the branch per `superpowers:finishing-a-development-branch`.

---

## Self-review notes

- **Spec coverage.** §6.1 request types → Task 1; §6.2 compilation → Tasks 3–4; §6.3 cap → Tasks 1, 7; §6.4 outcome and `Delivery::Series` → Tasks 1, 6, 7; §6.5 lifecycle → Part 4 (the tile), not here; §7 grammar, resolution, cycles → Task 2 (resolution against loaded slots is the module's, Part 4, through `resolve` and `expression_order`); §5.4's `Delivery::SeriesFetched` → Task 6; §11.2 harness → Task 8; §11.3 the query bench → Task 8.
- **Deliberately not in Part 2:** the tile, the chart, the picker's use of `CatalogSnapshot::identities`, `[timeseries] default_source`, and any change to `append_series`.
- **Type consistency:** `SeriesParams`, `SeriesOutcome`, `SeriesResult`, `SlotResult`, `SlotProvenance`, `Frequency`, `BucketRule`, `SeriesSpec`, `SlotKind` are `geode_core::series::*` everywhere; `Expr` is `geode_core::series::expr::Expr` (= `Ast<u8>`); `SeriesPlan`/`Statement`/`compile_series`/`run_series` are `geode_data::query::series::*`; `Payload`/`Work` are `geode_data::query::pool::*`; the pair key string is `format!("{identity}@{source}")` at the one site in the sink (Task 7) and in the recording fixture (Task 6).
- **Ordering:** Task 6 (shell) precedes Task 7 (service) so the bridge's `DataEvent::Series` arm has a `Delivery::Series` to map to the moment the event exists; Task 5's placeholder `RequestKind::Series { .. } => true` sink arm keeps the workspace compiling between them.
