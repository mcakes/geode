# Line Pricer Part 1 (Seam and Data Tier) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pricing seam and the data tier under the line pricer: the instrument vocabulary and `Pricer` trait, the deterministic `MockPricer`, a pricing worker thread inside `DataService` answering `Request::Price` as `DataEvent::Price`, a `Request::Publish` that lands a local document as a generation, the `local = true` dataset flag with its bridge-side gate, `Delivery::Price` in the shell, `[pricing]` config with a restart stripe, a seventh tracing target, and the charter amendment. No tile, no sheet, no parser (Parts 2–4).

**Architecture:** The vocabulary and trait live in `geode_core::pricing` (a leaf the shell may name without depending on a calculation crate); `geode-pricing` holds `MockPricer` and is where vendor implementations will be feature-gated. `geode-data` gains a `pricing` module: a `PricerRegistry` beside `AdapterRegistry`, and a `PricingWorker` thread with a latest-wins-per-key queue, cancel at a line boundary, and `panic::contained` per line. The service serves two new requests and emits one new event; the app bridge routes it to `Delivery::Price` and gates the frame's data bump on `DatasetSpec::local`.

**Tech Stack:** Rust 2024, `std::sync::mpsc` and `Mutex`/`Condvar` (no channel crate in `geode-data`), DuckDB via the existing ingest runner, `chrono 0.4.42`, gpui `TestAppContext` for the bridge and shell tests.

**Spec:** `docs/superpowers/specs/2026-09-19-geode-line-pricer-design.md` §2, §2.1, §3, §4, §5, §7.2 (the `local` flag and the frame-bump gate only), §10, §12 (seam and data-tier bullets), §14 part 1. Read §2 (rulings) and §5 before starting.

**One deviation from the spec, decided in planning:** §4 places the vocabulary and the `Pricer` trait in `geode-pricing`, but §5.4 puts `PriceOutcome` inside `geode_shell::module::Delivery`, and the shell must not depend on a calculation crate (§2.1: a calculation crate is reached through a door, never named by the shell). So the vocabulary, the trait and the request/outcome types live in `geode_core::pricing`, the way `geode_core::document` holds `DocumentKind`; `geode-pricing` holds `MockPricer` alone and remains the home of the future feature-gated vendor crates. Task 10 records this in the spec's as-built section.

## Global Constraints

- `geode-pricing` depends on `geode-core` only. `geode-shell` never depends on `geode-pricing`; it names `geode_core::pricing::PriceOutcome`.
- Every value in `PriceResult` is per unit of the instrument. `Strike::Percent` and `Expiry::Tenor` pass through the seam untouched: nothing in Geode resolves them.
- The pricing worker is one thread, separate from the query pool, draining its own queue: latest wins per key, `Request::Cancel { key }` drops a queued batch and stops a running one at the next line boundary (lines already priced are delivered), every `price` call runs under `std::panic::catch_unwind` + `geode_core::panic::contained`, and the worker never dies on a panic.
- A pricer named in config but absent from the binary is `PricerConfig { pricer: None }`: every line answers `Err("pricer \"<name>\" is not built into this binary")`; an empty name answers `Err("no pricer is configured")`. Never a startup failure.
- `Request::Publish` is refused unwritten for any dataset whose spec is not `local = true`; `local` is accepted on the document family only; a `[sources]` entry naming a local dataset is an error diagnostic.
- A local publish's `DocumentJob.source` is `geode_core::pricing::LOCAL_SOURCE` (`"local"`). The ingest sink emits `Published` (or a `Diagnostics` error on failure) and `LoadEnded` for it, and never a `Health` event: no health lane exists for a source no `[sources]` entry declares.
- The bridge's `Published` arm skips `Frame::note_published` for a local dataset and still calls `Diagnostics::note_published`.
- `EventSink` rule unchanged: `false` means "not delivered", no producer stops on it.
- The seventh tracing target is `geode::pricing`; `TARGETS` is `[&str; 7]`.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets` and `cargo bench --workspace --no-run` pass at the end of every task. Commit at the end of every task.
- `zsh scripts/mutation-check.sh --anchors-only` before the final merge (Task 10).
- Do not touch the query compiler, the blotter's or market-data panel's behaviour (they gain one inert `match` arm each), or any tile code: Part 1 ends at `Delivery::Price` reaching an occupant.
- This plan is written against `main` at `6c67e5a`. The timeseries Part 1 plan (unexecuted) also adds fields to `DataServiceConfig`; whichever lands second rebases its literal sites.

---

## File map

| File | Responsibility |
|---|---|
| `crates/geode-core/src/pricing.rs` (new) | `Instrument`, `Vanilla`, `Barrier`, `OptionKind`, `BarrierKind`, `Expiry`, `Strike`, `Shifts`, `PriceRequest`, `PriceResult`, `PricingError`, `Pricer`, `PriceParams`, `PriceLine`, `PriceOutcome`, `LocalPublish`, `LOCAL_SOURCE` |
| `crates/geode-pricing/` (new crate) | `MockPricer` |
| `crates/geode-core/src/schema/mod.rs` | `DatasetSpec::local`, the `from_doc` read, the validation rule |
| `crates/geode-core/src/source_config.rs` | a source naming a local dataset is an error |
| `crates/geode-core/src/log/mod.rs`, `crates/geode-diagnostics/src/commands.rs` | the seventh target |
| `crates/geode-data/src/pricing/mod.rs` (new) | `PricerConfig`, `PricerRegistry` |
| `crates/geode-data/src/pricing/worker.rs` (new) | `PricingWorker`, `PriceSink`, `PRICE_BOUND` |
| `crates/geode-data/src/handle.rs`, `service.rs` | `Request::{Price, Publish}`, `DataEvent::Price`, `DataService::{price, publish}`, the worker's lifetime, the local-source arms of the ingest sink |
| `crates/geode-shell/src/module.rs` and the five `match` sites | `Delivery::Price` |
| `crates/geode-app/src/main.rs`, `bridge.rs` | `PricerRegistry` filled with the mock, `[pricing] adapter`, the `Price` arm, the local gate |
| `crates/geode-shell/src/shell/mod.rs`, `hot_reload.rs` | `pricing_baseline`, the restart stripe |
| `docs/PHILOSOPHY.md`, `CLAUDE.md`, `docs/phase-history.md`, the roadmap, the spec, `scripts/mutation-check.sh` | Task 10 |

---

### Task 1: The vocabulary and the `Pricer` trait in `geode_core::pricing`

**Files:**
- Create: `crates/geode-core/src/pricing.rs`
- Modify: `crates/geode-core/src/lib.rs:14` (add `pub mod pricing;` after `pub mod panic;`)
- Test: `crates/geode-core/src/pricing.rs` (`mod tests`)

**Interfaces:**
- Produces (all `pub`, all in `geode_core::pricing`):
```rust
pub const LOCAL_SOURCE: &str = "local";
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum OptionKind { Call, Put }
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum BarrierKind { UpIn, UpOut, DownIn, DownOut }
#[derive(Debug, Clone, PartialEq)] pub enum Expiry { Date(chrono::NaiveDate), Tenor(String) }
impl Expiry { pub fn tenor(text: &str) -> Result<Expiry, String>; }   // "3m" | "6w" | "1y" | "10d", case-insensitive, stored lower-case
#[derive(Debug, Clone, Copy, PartialEq)] pub enum Strike { Absolute(f64), Percent(f64) }
#[derive(Debug, Clone, PartialEq)] pub struct Vanilla { pub underlying: String, pub expiry: Expiry, pub strike: Strike, pub kind: OptionKind }
#[derive(Debug, Clone, PartialEq)] pub struct Barrier { pub vanilla: Vanilla, pub level: f64, pub barrier: BarrierKind }
#[derive(Debug, Clone, PartialEq)] pub enum Instrument { Vanilla(Vanilla), Barrier(Barrier) }
impl Instrument { pub fn underlying(&self) -> &str; pub fn kind(&self) -> OptionKind; }
#[derive(Debug, Clone, Copy, PartialEq, Default)] pub struct Shifts { pub spot_pct: f64, pub vol_pts: f64 }
#[derive(Debug, Clone, PartialEq)] pub struct PriceRequest { pub instrument: Instrument, pub shifts: Shifts }
#[derive(Debug, Clone, Copy, PartialEq)] pub struct PriceResult { pub price: f64, pub delta: f64, pub gamma: f64, pub vega: f64, pub theta: f64, pub rho: f64 }
#[derive(Debug, Clone, PartialEq, Eq)] pub struct PricingError(pub String);
pub trait Pricer: Send + Sync { fn name(&self) -> &str; fn price(&self, req: &PriceRequest) -> Result<PriceResult, PricingError>; }
#[derive(Debug, Clone, PartialEq)] pub struct PriceLine { pub id: u64, pub revision: u64, pub request: PriceRequest }
#[derive(Debug, Clone)] pub struct PriceParams { pub key: QueryKey, pub tag: u64, pub submitted: Instant, pub lines: Vec<PriceLine> }
#[derive(Debug)] pub struct PriceOutcome { pub key: QueryKey, pub tag: u64, pub submitted: Instant, pub results: Vec<(u64, u64, Result<PriceResult, String>)> }
#[derive(Debug, Clone)] pub struct LocalPublish { pub dataset: String, pub rows: crate::document::DocumentRows }
```

- [ ] **Step 1: Write the failing tests**

Create `crates/geode-core/src/pricing.rs` with only the `mod tests` block below (the module's items come in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn spx_call() -> Instrument {
        Instrument::Vanilla(Vanilla {
            underlying: "SPX".into(),
            expiry: Expiry::Date(NaiveDate::from_ymd_opt(2026, 12, 18).unwrap()),
            strike: Strike::Absolute(5000.0),
            kind: OptionKind::Call,
        })
    }

    #[test]
    fn a_tenor_is_digits_then_one_unit_letter_and_is_stored_lower_case() {
        assert_eq!(Expiry::tenor("3m").unwrap(), Expiry::Tenor("3m".into()));
        assert_eq!(Expiry::tenor("6W").unwrap(), Expiry::Tenor("6w".into()));
        assert_eq!(Expiry::tenor("1y").unwrap(), Expiry::Tenor("1y".into()));
        assert_eq!(Expiry::tenor("10d").unwrap(), Expiry::Tenor("10d".into()));
        for bad in ["", "m", "3", "3mm", "3q", "3 m", "-3m", "3.5m"] {
            assert!(Expiry::tenor(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_request_is_equal_when_every_field_is_and_differs_on_a_shift() {
        let a = PriceRequest { instrument: spx_call(), shifts: Shifts::default() };
        let b = PriceRequest { instrument: spx_call(), shifts: Shifts::default() };
        assert_eq!(a, b);
        let c = PriceRequest { instrument: spx_call(), shifts: Shifts { spot_pct: 1.0, vol_pts: 0.0 } };
        assert_ne!(a, c);
    }

    #[test]
    fn an_instrument_answers_its_underlying_and_kind_through_a_barrier() {
        let b = Instrument::Barrier(Barrier {
            vanilla: match spx_call() { Instrument::Vanilla(v) => Vanilla { kind: OptionKind::Put, ..v }, _ => unreachable!() },
            level: 4200.0,
            barrier: BarrierKind::DownOut,
        });
        assert_eq!(b.underlying(), "SPX");
        assert_eq!(b.kind(), OptionKind::Put);
        assert_eq!(spx_call().kind(), OptionKind::Call);
    }

    #[test]
    fn the_local_source_name_is_the_word_local() {
        assert_eq!(LOCAL_SOURCE, "local");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-core pricing::`
Expected: compile error — `Expiry`, `Instrument` and the rest are not defined.

- [ ] **Step 3: Write the module**

Prepend to `crates/geode-core/src/pricing.rs`, above `mod tests`:

```rust
//! The pricing seam's vocabulary (line-pricer spec §5.1).
//!
//! This is the one place Geode describes an option to a pricing
//! library, and the one place a library answers. The library is upstream
//! intelligence that happens to be linked in (PHILOSOPHY §1, "In-process
//! calculation"): nothing here resolves a percent strike against spot or
//! a tenor against a calendar — `Strike::Percent` and `Expiry::Tenor`
//! pass through untouched, because doing otherwise would be financial
//! reasoning in the app.
//!
//! The types live in `geode-core` rather than `geode-pricing` so the
//! shell can name [`PriceOutcome`] in its delivery enum without
//! depending on a calculation crate; `geode-pricing` holds the
//! implementations (the mock now, feature-gated vendors later).

use crate::document::DocumentRows;
use crate::query::QueryKey;
use chrono::NaiveDate;
use std::time::Instant;

/// The `source` a local publish (`Request::Publish`) is stamped with.
/// No `[sources]` entry ever declares it, so the ingest sink reports no
/// health for it (spec §5.3).
pub const LOCAL_SOURCE: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    Call,
    Put,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierKind {
    UpIn,
    UpOut,
    DownIn,
    DownOut,
}

/// A date, or a tenor the library resolves itself (`3m`, `6w`, `1y`,
/// `10d`).
#[derive(Debug, Clone, PartialEq)]
pub enum Expiry {
    Date(NaiveDate),
    Tenor(String),
}

impl Expiry {
    /// Validates `<digits><d|w|m|y>`, case-insensitive, and stores it
    /// lower-case. Never resolves it: that is the library's calendar,
    /// not ours.
    pub fn tenor(text: &str) -> Result<Expiry, String> {
        let lower = text.to_ascii_lowercase();
        let (digits, unit) = match lower.char_indices().last() {
            Some((i, unit)) => (&lower[..i], unit),
            None => return Err("empty tenor".to_string()),
        };
        if !matches!(unit, 'd' | 'w' | 'm' | 'y') {
            return Err(format!("tenor '{text}': the unit must be one of d, w, m, y"));
        }
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("tenor '{text}': expected digits then one unit letter"));
        }
        Ok(Expiry::Tenor(lower))
    }
}

/// Absolute (`5000`) or percent of spot (`95%` is `Percent(95.0)`); the
/// library resolves a percent, never the app.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Strike {
    Absolute(f64),
    Percent(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Vanilla {
    pub underlying: String,
    pub expiry: Expiry,
    pub strike: Strike,
    pub kind: OptionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Barrier {
    pub vanilla: Vanilla,
    pub level: f64,
    pub barrier: BarrierKind,
}

/// Slice 1's two variants (spec §1.1). Multi-underlying products are a
/// later variant, not a restructure.
#[derive(Debug, Clone, PartialEq)]
pub enum Instrument {
    Vanilla(Vanilla),
    Barrier(Barrier),
}

impl Instrument {
    fn vanilla(&self) -> &Vanilla {
        match self {
            Instrument::Vanilla(v) => v,
            Instrument::Barrier(b) => &b.vanilla,
        }
    }

    pub fn underlying(&self) -> &str {
        &self.vanilla().underlying
    }

    pub fn kind(&self) -> OptionKind {
        self.vanilla().kind
    }
}

/// Spot in percent, vol in points; both `0.0` when unshifted.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Shifts {
    pub spot_pct: f64,
    pub vol_pts: f64,
}

/// What one line asks. `PartialEq` is load-bearing: the sheet compares
/// a line's request before and after an edit to decide whether to
/// reprice it (spec §9.3).
#[derive(Debug, Clone, PartialEq)]
pub struct PriceRequest {
    pub instrument: Instrument,
    pub shifts: Shifts,
}

/// Per unit of the instrument, every field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceResult {
    pub price: f64,
    pub delta: f64,
    pub gamma: f64,
    pub vega: f64,
    pub theta: f64,
    pub rho: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingError(pub String);

/// One synchronous, self-contained call per instrument (spec ruling 1).
/// The library fetches or is handed its own market data; the app hands
/// it a definition and shifts and shows what comes back.
pub trait Pricer: Send + Sync {
    fn name(&self) -> &str;
    fn price(&self, req: &PriceRequest) -> Result<PriceResult, PricingError>;
}

/// One line of a batch: `id` is the sheet's line identity and `revision`
/// the edit counter the answer must still match to be installed.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceLine {
    pub id: u64,
    pub revision: u64,
    pub request: PriceRequest,
}

/// The pricing request (spec §5.3): one batch per tile per frame, keyed
/// and tagged like a query.
#[derive(Debug, Clone)]
pub struct PriceParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub lines: Vec<PriceLine>,
}

/// The answer, addressed to the key that asked. A cancelled batch
/// carries the lines priced before the cancel landed.
#[derive(Debug)]
pub struct PriceOutcome {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    /// `(id, revision, result)` per line, in request order.
    pub results: Vec<(u64, u64, Result<PriceResult, String>)>,
}

/// A document the app itself authored, to be published as a generation
/// of a `local = true` dataset (spec §5.3, §7.2).
#[derive(Debug, Clone)]
pub struct LocalPublish {
    pub dataset: String,
    pub rows: DocumentRows,
}
```

Add `pub mod pricing;` to `crates/geode-core/src/lib.rs` after `pub mod panic;`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-core pricing::`
Expected: 4 passed.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy -p geode-core --all-targets -- -D warnings
git add crates/geode-core/src/pricing.rs crates/geode-core/src/lib.rs
git commit -m "core: the pricing seam's vocabulary and Pricer trait (line-pricer §5.1)"
```

---

### Task 2: `geode-pricing` and `MockPricer`

**Files:**
- Create: `crates/geode-pricing/Cargo.toml`, `crates/geode-pricing/src/lib.rs`
- Modify: `Cargo.toml` (root: add `"crates/geode-pricing",` to `members` after `"crates/geode-documents",`; add `geode-pricing = { path = "crates/geode-pricing" }` to `[workspace.dependencies]` after the `geode-shell` line)
- Test: `crates/geode-pricing/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: everything in `geode_core::pricing` from Task 1.
- Produces:
```rust
pub struct MockPricer { delay: Duration }
impl MockPricer {
    pub fn new() -> MockPricer;                      // no delay
    pub fn with_delay(delay: Duration) -> MockPricer;
}
impl Default for MockPricer { … }                    // = new()
impl Pricer for MockPricer { fn name(&self) -> &str { "mock" } … }
pub const MOCK_PRICER: &str = "mock";
pub const REFUSED_UNDERLYING: &str = "FAIL";
```

- [ ] **Step 1: Create the crate**

`crates/geode-pricing/Cargo.toml`:

```toml
[package]
name = "geode-pricing"
version.workspace = true
edition.workspace = true
publish.workspace = true

[lib]
bench = false

[dependencies]
geode-core.workspace = true
```

`crates/geode-pricing/src/lib.rs`, with only the tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::pricing::{
        Barrier, BarrierKind, Expiry, Instrument, OptionKind, PriceRequest, Pricer, Shifts, Strike,
        Vanilla,
    };
    use std::time::{Duration, Instant};

    fn vanilla(underlying: &str, kind: OptionKind, strike: f64) -> Instrument {
        Instrument::Vanilla(Vanilla {
            underlying: underlying.into(),
            expiry: Expiry::Tenor("3m".into()),
            strike: Strike::Absolute(strike),
            kind,
        })
    }

    fn req(instrument: Instrument, spot_pct: f64, vol_pts: f64) -> PriceRequest {
        PriceRequest { instrument, shifts: Shifts { spot_pct, vol_pts } }
    }

    #[test]
    fn the_same_request_answers_the_same_numbers() {
        let p = MockPricer::new();
        let a = p.price(&req(vanilla("SPX", OptionKind::Call, 5000.0), 0.0, 0.0)).unwrap();
        let b = p.price(&req(vanilla("SPX", OptionKind::Call, 5000.0), 0.0, 0.0)).unwrap();
        assert_eq!(a, b);
        let c = p.price(&req(vanilla("SPX", OptionKind::Call, 5100.0), 0.0, 0.0)).unwrap();
        assert_ne!(a.price, c.price, "a different strike is a different number");
    }

    #[test]
    fn a_call_has_positive_delta_and_a_put_negative_and_the_other_greeks_keep_their_signs() {
        let p = MockPricer::new();
        for (kind, sign) in [(OptionKind::Call, 1.0), (OptionKind::Put, -1.0)] {
            for strike in [90.0, 100.0, 110.0, 4000.0, 5000.0] {
                let r = p.price(&req(vanilla("NDX", kind, strike), 0.0, 0.0)).unwrap();
                assert!(r.price > 0.0);
                assert!(r.delta * sign > 0.0, "{kind:?} delta {}", r.delta);
                assert!(r.gamma > 0.0 && r.vega > 0.0 && r.theta < 0.0);
                assert!(r.rho * sign > 0.0);
            }
        }
    }

    #[test]
    fn a_spot_shift_moves_the_price_in_deltas_sign_and_a_vol_shift_in_vegas() {
        let p = MockPricer::new();
        for kind in [OptionKind::Call, OptionKind::Put] {
            let base = p.price(&req(vanilla("SPX", kind, 5000.0), 0.0, 0.0)).unwrap();
            let up = p.price(&req(vanilla("SPX", kind, 5000.0), 5.0, 0.0)).unwrap();
            assert_eq!((up.price - base.price).signum(), base.delta.signum(), "{kind:?}");
            let volup = p.price(&req(vanilla("SPX", kind, 5000.0), 0.0, 2.0)).unwrap();
            assert!(volup.price > base.price, "{kind:?}: vega is positive so vol up is price up");
        }
    }

    #[test]
    fn a_barrier_prices_and_differs_from_its_vanilla() {
        let p = MockPricer::new();
        let v = vanilla("SPX", OptionKind::Put, 5000.0);
        let b = Instrument::Barrier(Barrier {
            vanilla: match v.clone() { Instrument::Vanilla(v) => v, _ => unreachable!() },
            level: 4200.0,
            barrier: BarrierKind::DownOut,
        });
        let rv = p.price(&req(v, 0.0, 0.0)).unwrap();
        let rb = p.price(&req(b, 0.0, 0.0)).unwrap();
        assert_ne!(rv.price, rb.price);
        assert!(rb.delta < 0.0, "a put barrier is still a put");
    }

    #[test]
    fn the_refused_underlying_is_an_error_and_the_delay_is_honoured() {
        let p = MockPricer::new();
        let err = p.price(&req(vanilla(REFUSED_UNDERLYING, OptionKind::Call, 100.0), 0.0, 0.0)).unwrap_err();
        assert_eq!(err.0, "refused by the mock");
        let slow = MockPricer::with_delay(Duration::from_millis(30));
        let t = Instant::now();
        slow.price(&req(vanilla("SPX", OptionKind::Call, 100.0), 0.0, 0.0)).unwrap();
        assert!(t.elapsed() >= Duration::from_millis(30));
        assert_eq!(slow.name(), MOCK_PRICER);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-pricing`
Expected: compile error — `MockPricer` not defined.

- [ ] **Step 3: Write the mock**

Prepend to `crates/geode-pricing/src/lib.rs`:

```rust
//! Implementations of `geode_core::pricing::Pricer` (line-pricer spec
//! §4, §5.2).
//!
//! This crate is a leaf under PHILOSOPHY §1's "In-process calculation":
//! it depends on `geode-core` alone, nothing in the shell, a module or
//! the data crate depends on it except by the `Pricer` door, and the real
//! vendor library lands beside [`MockPricer`] behind a feature gate CI
//! never builds. `geode-app` registers what this build has in a
//! `PricerRegistry`; a `[pricing] adapter` naming anything else fails
//! every line with that reason, never startup.

use geode_core::pricing::{Instrument, OptionKind, PriceRequest, PriceResult, Pricer, PricingError};
use std::hash::{Hash, Hasher};
use std::time::Duration;

/// The name `[pricing] adapter` uses for the mock, and its default.
pub const MOCK_PRICER: &str = "mock";

/// An instrument on this underlying is refused, so the failed-line path
/// is testable without a real library (spec §5.2).
pub const REFUSED_UNDERLYING: &str = "FAIL";

/// Deterministic, cheap, and NOT a model. The numbers come from a hash
/// of the instrument's fields; the shift terms are shaped so a spot
/// shift moves the price in delta's sign and a vol shift in vega's, so
/// a trader bumping shifts sees plausible motion. A call's delta and rho
/// are positive and a put's negative; gamma and vega are positive;
/// theta is negative.
#[derive(Debug, Clone)]
pub struct MockPricer {
    delay: Duration,
}

impl MockPricer {
    pub fn new() -> MockPricer {
        MockPricer { delay: Duration::ZERO }
    }

    /// Sleeps `delay` per call: the test hook for the tile's slow path.
    pub fn with_delay(delay: Duration) -> MockPricer {
        MockPricer { delay }
    }
}

impl Default for MockPricer {
    fn default() -> Self {
        MockPricer::new()
    }
}

/// `Debug` of an `Instrument` is a deterministic function of its fields
/// (f64s print exactly), which is all a hash for a mock needs.
fn seed(instrument: &Instrument) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{instrument:?}").hash(&mut h);
    h.finish()
}

impl Pricer for MockPricer {
    fn name(&self) -> &str {
        MOCK_PRICER
    }

    fn price(&self, req: &PriceRequest) -> Result<PriceResult, PricingError> {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        if req.instrument.underlying() == REFUSED_UNDERLYING {
            return Err(PricingError("refused by the mock".to_string()));
        }
        let s = seed(&req.instrument);
        let sign = match req.instrument.kind() {
            OptionKind::Call => 1.0,
            OptionKind::Put => -1.0,
        };
        let base = 1.0 + (s % 10_000) as f64 / 100.0; // 1.00 ..= 100.99
        let delta = sign * (0.20 + ((s >> 16) % 60) as f64 / 100.0); // |0.20 ..= 0.79|
        let gamma = 0.001 + ((s >> 24) % 100) as f64 / 10_000.0;
        let vega = 0.05 + ((s >> 32) % 50) as f64 / 100.0;
        let theta = -(0.005 + ((s >> 40) % 50) as f64 / 1_000.0);
        let rho = sign * 0.1 * (1.0 + ((s >> 48) % 10) as f64 / 10.0);
        let price = base + delta * base * req.shifts.spot_pct / 100.0 + vega * req.shifts.vol_pts;
        Ok(PriceResult { price, delta, gamma, vega, theta, rho })
    }
}
```

Root `Cargo.toml`: add the member and the workspace dependency as listed under **Files**.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-pricing`
Expected: 5 passed.

- [ ] **Step 5: Format, lint, bench-check, commit**

```bash
cargo fmt && cargo clippy -p geode-pricing --all-targets -- -D warnings && cargo bench --workspace --no-run
git add Cargo.toml Cargo.lock crates/geode-pricing
git commit -m "pricing: geode-pricing crate with the deterministic MockPricer (line-pricer §5.2)"
```

---

### Task 3: `DatasetSpec::local`

**Files:**
- Modify: `crates/geode-core/src/schema/mod.rs` (`DatasetSpec` at L37–50; `from_doc`'s `family` read at L203–220 and the `DatasetSpec { … }` literal at L268; `validate_dataset` at L334)
- Modify: `crates/geode-core/src/source_config.rs` (`SourceSpec::from_doc`, the `dataset` match at L352–370)
- Modify (literal sites gain `local: false`): `crates/geode-core/src/scope/mod.rs:578`, `crates/geode-demo-data/src/documents.rs:344`, `crates/geode-data/src/store/catalog.rs:745`, and any other the compiler names
- Test: `crates/geode-core/src/schema/mod.rs` (`mod tests`), `crates/geode-core/src/source_config.rs` (`mod tests`)

**Interfaces:**
- Produces: `DatasetSpec { …, pub local: bool }` (`false` unless declared); a `local = true` on the measure family is an error diagnostic at `datasets.<name>.local` and the flag is cleared; a non-bool `local` is a warning and `false`; `SourceSpec::from_doc` refuses a source whose `dataset` is local with an error at `sources.<name>.dataset`.

- [ ] **Step 1: Write the failing tests**

In `crates/geode-core/src/schema/mod.rs`'s `mod tests` (it already has a helper that parses a datasets doc into `(SchemaSpec, Vec<Diagnostic>)` — reuse whatever `from_doc`-driving helper the existing document-family tests use; the tests below call it `parse`):

```rust
    #[test]
    fn local_is_read_on_a_document_dataset_and_defaults_to_false() {
        let (schema, diags) = parse(
            r#"
[sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]
[sheets.columns.sheet]
type = "utf8"
role = "dimension"
[sheets.columns.line]
type = "i64"
role = "axis"
[sheets.columns.qty]
type = "i64"
role = "value"

[cvi]
family = "document"
key = ["u"]
axes = ["t"]
[cvi.columns.u]
type = "utf8"
role = "dimension"
[cvi.columns.t]
type = "date"
role = "axis"
[cvi.columns.v]
type = "f64"
role = "value"
"#,
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(schema.dataset("sheets").unwrap().local);
        assert!(!schema.dataset("cvi").unwrap().local);
    }

    #[test]
    fn local_on_a_measure_dataset_is_an_error_and_cleared() {
        let (schema, diags) = parse(
            r#"
[risk]
local = true
[risk.columns.book]
type = "utf8"
role = "dimension"
grain = "book"
[risk.columns.npv]
type = "f64"
role = "measure"
"#,
        );
        let d = diags
            .iter()
            .find(|d| d.path.as_deref() == Some("datasets.risk.local"))
            .expect("a diagnostic at the key");
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("document family"), "{}", d.message);
        assert!(!schema.dataset("risk").unwrap().local, "cleared");
    }

    #[test]
    fn a_non_bool_local_is_a_warning_and_false() {
        let (schema, diags) = parse(
            r#"
[sheets]
family = "document"
local = "yes"
key = ["sheet"]
axes = ["line"]
[sheets.columns.sheet]
type = "utf8"
role = "dimension"
[sheets.columns.line]
type = "i64"
role = "axis"
[sheets.columns.qty]
type = "i64"
role = "value"
"#,
        );
        let d = diags.iter().find(|d| d.path.as_deref() == Some("datasets.sheets.local")).unwrap();
        assert_eq!(d.severity, Severity::Warning);
        assert!(!schema.dataset("sheets").unwrap().local);
    }
```

In `crates/geode-core/src/source_config.rs`'s `mod tests` (reuse its existing helper that builds a `SchemaSpec` from a datasets doc and calls `SourceSpec::from_doc`; the document-family cross-check test at the `adapter '…' needs a document family dataset` message is the template):

```rust
    #[test]
    fn a_source_naming_a_local_dataset_is_refused() {
        let schema = schema_from(
            r#"
[sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]
[sheets.columns.sheet]
type = "utf8"
role = "dimension"
[sheets.columns.line]
type = "i64"
role = "axis"
[sheets.columns.qty]
type = "i64"
role = "value"
"#,
        );
        let (sources, diags) = sources_from(
            r#"
[feed]
dataset = "sheets"
paths = ["/x/*.csv"]
"#,
            &schema,
        );
        assert!(sources.is_empty());
        let d = diags.iter().find(|d| d.path.as_deref() == Some("sources.feed.dataset")).unwrap();
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("local"), "{}", d.message);
    }
```

(`schema_from`/`sources_from` are whatever the file's existing tests call their two fixtures; if the names differ, use the file's names and do not add a second copy.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p geode-core local_`
Expected: compile error on `.local` (no such field).

- [ ] **Step 3: Implement**

`schema/mod.rs`:

1. `DatasetSpec` gains, after `axes`:
```rust
    /// The app itself is this dataset's writer (`Request::Publish`); no
    /// `[sources]` entry may feed it, and the bridge does not bump the
    /// frame's data version when it publishes (line-pricer spec §7.2).
    /// Document family only.
    pub local: bool,
```
2. In `from_doc`, beside the `family` read (after it, before `key`/`axes`):
```rust
            let local = match ds_value.get("local") {
                None => false,
                Some(v) => match v.as_bool() {
                    Some(b) => b,
                    None => {
                        diags.push(Diagnostic {
                            severity: Severity::Warning,
                            layer: None,
                            file: None,
                            message: format!("dataset '{ds_name}': 'local' must be true or false; treated as false"),
                            path: Some(format!("datasets.{ds_name}.local")),
                        });
                        false
                    }
                },
            };
```
   and the literal at L268 gains `local,`.
3. In `validate_dataset`, before `if ds.is_document() {`:
```rust
    if ds.local && !ds.is_document() {
        diags.push(Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            message: format!("dataset '{}': 'local' is accepted on the document family only", ds.name),
            path: Some(format!("datasets.{}.local", ds.name)),
        });
        ds.local = false;
    }
```

`source_config.rs`, in the `dataset` match's `Some(d) if schema.dataset(d).is_some()` arm — split it so a local dataset is refused first:
```rust
                Some(d) if schema.dataset(d).is_some_and(|ds| ds.local) => {
                    diags.push(diag(
                        Severity::Error,
                        name,
                        Some("dataset"),
                        format!("'{d}' is a local dataset: the app is its only writer and no source may feed it"),
                    ));
                    continue;
                }
                Some(d) if schema.dataset(d).is_some() => d.to_string(),
```

Every `DatasetSpec { … }` literal the compiler names gains `local: false,`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-core local_ && cargo test --workspace`
Expected: the four new tests pass; the workspace is green.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates
git commit -m "core: DatasetSpec::local — app-written document datasets no source may feed (line-pricer §7.2)"
```

---

### Task 4: The seventh tracing target, `geode::pricing`

**Files:**
- Modify: `crates/geode-core/src/log/mod.rs:10–19`
- Modify: `crates/geode-diagnostics/src/commands.rs:247–262` (the test iterates `TARGETS`; check its assertion still holds and add the new suffix to any literal list it compares against)
- Test: `crates/geode-core/src/log/mod.rs` (`mod tests`)

**Interfaces:**
- Produces: `pub const TARGETS: [&str; 7]` ending in `"geode::pricing"`; `[log] pricing = "debug"` accepted by `LogLevels::from_doc`.

- [ ] **Step 1: Write the failing test** (in `log/mod.rs`'s `mod tests`, beside the existing `from_doc` tests)

```rust
    #[test]
    fn pricing_is_a_known_log_target() {
        assert_eq!(TARGETS.len(), 7);
        assert!(TARGETS.contains(&"geode::pricing"));
        let config = crate::config::test_support::config_from("app", "[log]\npricing = \"debug\"\n");
        let (levels, diags) = LogLevels::from_doc(&config);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(levels.targets.iter().any(|(t, l)| t == "geode::pricing" && *l == tracing::Level::DEBUG));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-core pricing_is_a_known_log_target`
Expected: FAIL — `TARGETS.len()` is 6 and the `[log]` key is warned as unknown.

- [ ] **Step 3: Implement**

```rust
/// The seven targets every subscriber layer and `[log]` key name (by
/// suffix, e.g. `ingest` → `geode::ingest`) know about. `geode::pricing`
/// is the pricing worker's (line-pricer spec §10.2).
pub const TARGETS: [&str; 7] = [
    "geode::ingest",
    "geode::query",
    "geode::config",
    "geode::session",
    "geode::shell",
    "geode::theme",
    "geode::pricing",
];
```

Run `cargo test -p geode-diagnostics known` and fix whatever literal that test compares against.

- [ ] **Step 4: Verify**

Run: `cargo test -p geode-core log:: && cargo test -p geode-diagnostics`
Expected: green.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add crates/geode-core/src/log/mod.rs crates/geode-diagnostics/src/commands.rs
git commit -m "log: geode::pricing is the seventh target (line-pricer §10.2)"
```

---

### Task 5: `PricerConfig`, `PricerRegistry` and the `PricingWorker`

**Files:**
- Create: `crates/geode-data/src/pricing/mod.rs`, `crates/geode-data/src/pricing/worker.rs`
- Modify: `crates/geode-data/src/lib.rs` (add `pub mod pricing;` after `pub mod ingest;`)
- Test: both new files (`mod tests`)

**Interfaces:**
- Consumes: `geode_core::pricing::{Pricer, PriceParams, PriceOutcome, PriceLine, PriceRequest, PriceResult}`, `geode_core::query::QueryKey`.
- Produces:
```rust
// geode_data::pricing
#[derive(Clone, Default)]
pub struct PricerConfig { pub name: String, pub pricer: Option<Arc<dyn Pricer>> }
impl PricerConfig {
    pub fn with(pricer: Arc<dyn Pricer>) -> PricerConfig;   // name from pricer.name()
    pub fn missing(name: &str) -> PricerConfig;               // pricer: None
    /// The error every line answers when `pricer` is `None`.
    pub fn missing_reason(&self) -> String;                   // "no pricer is configured" | "pricer \"x\" is not built into this binary"
}
#[derive(Default, Clone)]
pub struct PricerRegistry { … }
impl PricerRegistry { pub fn register(&mut self, pricer: Arc<dyn Pricer>); pub fn get(&self, name: &str) -> Option<Arc<dyn Pricer>>; pub fn names(&self) -> Vec<String>; }
// geode_data::pricing::worker
pub type PriceSink = Arc<dyn Fn(PriceOutcome) -> bool + Send + Sync>;
pub const PRICE_BOUND: usize = 64;                            // distinct keys queued
pub struct PricingWorker { … }
impl PricingWorker {
    pub fn spawn(config: PricerConfig, sink: PriceSink) -> PricingWorker;
    /// Latest wins per key: a queued batch for the same key is replaced in place. `false` when
    /// PRICE_BOUND distinct keys are already queued or the worker is shut down. Never blocks.
    pub fn request(&self, params: PriceParams) -> bool;
    /// Drops a queued batch for `key`; a running one stops at the next line boundary.
    pub fn cancel(&self, key: QueryKey);
    pub fn shutdown(&self);                                   // idempotent; also on Drop
}
```

- [ ] **Step 1: Write the failing tests**

`crates/geode-data/src/pricing/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::pricing::{PriceRequest, PriceResult, PricingError};

    struct Named(&'static str);
    impl Pricer for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn price(&self, _: &PriceRequest) -> Result<PriceResult, PricingError> {
            Err(PricingError("unused".into()))
        }
    }

    #[test]
    fn the_registry_answers_by_name_and_lists_sorted() {
        let mut r = PricerRegistry::default();
        r.register(Arc::new(Named("vendor")));
        r.register(Arc::new(Named("mock")));
        assert!(r.get("mock").is_some());
        assert!(r.get("nope").is_none());
        assert_eq!(r.names(), vec!["mock", "vendor"]);
    }

    #[test]
    fn a_missing_pricer_names_itself_and_an_empty_name_says_none_is_configured() {
        assert_eq!(
            PricerConfig::missing("vendor").missing_reason(),
            "pricer \"vendor\" is not built into this binary"
        );
        assert_eq!(PricerConfig::default().missing_reason(), "no pricer is configured");
        let with = PricerConfig::with(Arc::new(Named("mock")));
        assert_eq!(with.name, "mock");
        assert!(with.pricer.is_some());
    }
}
```

`crates/geode-data/src/pricing/worker.rs`:

```rust
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use geode_core::pricing::{
        Expiry, Instrument, OptionKind, PriceLine, PriceRequest, PriceResult, Pricer,
        PricingError, Shifts, Strike, Vanilla,
    };
    use std::sync::Mutex;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    /// Prices anything but "FAIL" (an error) and "BOOM" (a panic), after
    /// `delay`, and records every underlying it was asked.
    pub(crate) struct FakePricer {
        pub(crate) asked: Arc<Mutex<Vec<String>>>,
        pub(crate) delay: Duration,
    }
    impl Pricer for FakePricer {
        fn name(&self) -> &str {
            "fake"
        }
        fn price(&self, req: &PriceRequest) -> Result<PriceResult, PricingError> {
            self.asked.lock().unwrap().push(req.instrument.underlying().to_string());
            std::thread::sleep(self.delay);
            match req.instrument.underlying() {
                "FAIL" => Err(PricingError("refused".into())),
                "BOOM" => panic!("the fake pricer exploded"),
                _ => Ok(PriceResult { price: 1.0, delta: 0.5, gamma: 0.0, vega: 0.0, theta: 0.0, rho: 0.0 }),
            }
        }
    }

    pub(crate) fn line(id: u64, underlying: &str) -> PriceLine {
        PriceLine {
            id,
            revision: 1,
            request: PriceRequest {
                instrument: Instrument::Vanilla(Vanilla {
                    underlying: underlying.into(),
                    expiry: Expiry::Tenor("3m".into()),
                    strike: Strike::Absolute(100.0),
                    kind: OptionKind::Call,
                }),
                shifts: Shifts::default(),
            },
        }
    }

    pub(crate) fn params(key: u64, tag: u64, underlyings: &[&str]) -> PriceParams {
        PriceParams {
            key: QueryKey(key),
            tag,
            submitted: Instant::now(),
            lines: underlyings.iter().enumerate().map(|(i, u)| line(i as u64 + 1, u)).collect(),
        }
    }

    fn worker(delay: Duration) -> (PricingWorker, Arc<Mutex<Vec<String>>>, std::sync::mpsc::Receiver<PriceOutcome>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: PriceSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = PricingWorker::spawn(
            PricerConfig::with(Arc::new(FakePricer { asked: asked.clone(), delay })),
            sink,
        );
        (w, asked, rx)
    }

    fn next(rx: &std::sync::mpsc::Receiver<PriceOutcome>) -> PriceOutcome {
        rx.recv_timeout(Duration::from_secs(10)).expect("an outcome")
    }

    #[test]
    fn a_batch_is_priced_line_by_line_and_answered_under_its_key_and_tag() {
        let (w, _, rx) = worker(Duration::ZERO);
        assert!(w.request(params(7, 3, &["SPX", "FAIL", "NDX"])));
        let o = next(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(7), 3));
        assert_eq!(o.results.len(), 3);
        assert_eq!(o.results[0].0, 1);
        assert!(o.results[0].2.is_ok());
        assert_eq!(o.results[1].2.as_ref().unwrap_err(), "refused");
        assert!(o.results[2].2.is_ok());
        w.shutdown();
    }

    #[test]
    fn latest_wins_per_key_while_queued() {
        // The first batch holds the worker; the second and third for key 9
        // queue behind it and only the third runs.
        let (w, asked, rx) = worker(Duration::from_millis(50));
        assert!(w.request(params(1, 1, &["A"])));
        assert!(w.request(params(9, 1, &["OLD"])));
        assert!(w.request(params(9, 2, &["NEW"])));
        let first = next(&rx);
        assert_eq!(first.key, QueryKey(1));
        let second = next(&rx);
        assert_eq!((second.key, second.tag), (QueryKey(9), 2));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "only two outcomes");
        assert!(!asked.lock().unwrap().iter().any(|u| u == "OLD"), "{:?}", asked.lock().unwrap());
        w.shutdown();
    }

    #[test]
    fn cancel_drops_a_queued_batch_and_stops_a_running_one_at_the_line_boundary() {
        let (w, asked, rx) = worker(Duration::from_millis(40));
        assert!(w.request(params(1, 1, &["A", "B", "C", "D", "E"])));
        assert!(w.request(params(2, 1, &["Q"])));
        std::thread::sleep(Duration::from_millis(60)); // inside line A or B of key 1
        w.cancel(QueryKey(2));
        w.cancel(QueryKey(1));
        let o = next(&rx);
        assert_eq!(o.key, QueryKey(1));
        assert!(o.results.len() < 5, "stopped early: {}", o.results.len());
        assert!(!o.results.is_empty(), "the lines already priced are delivered");
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "key 2 never ran");
        assert!(!asked.lock().unwrap().iter().any(|u| u == "Q"));
        w.shutdown();
    }

    #[test]
    fn a_panicking_line_is_that_lines_error_and_the_next_line_prices() {
        let (w, _, rx) = worker(Duration::ZERO);
        assert!(w.request(params(3, 1, &["BOOM", "SPX"])));
        let o = next(&rx);
        assert!(o.results[0].2.as_ref().unwrap_err().contains("panicked"), "{:?}", o.results[0]);
        assert!(o.results[1].2.is_ok());
        assert!(w.request(params(3, 2, &["SPX"])), "the worker is still alive");
        assert_eq!(next(&rx).tag, 2);
        w.shutdown();
    }

    #[test]
    fn no_pricer_answers_every_line_with_the_configured_name() {
        let (tx, rx) = channel();
        let sink: PriceSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = PricingWorker::spawn(PricerConfig::missing("vendor"), sink);
        assert!(w.request(params(4, 1, &["SPX", "NDX"])));
        let o = next(&rx);
        for (_, _, r) in &o.results {
            assert_eq!(r.as_ref().unwrap_err(), "pricer \"vendor\" is not built into this binary");
        }
        w.shutdown();
    }

    #[test]
    fn the_queue_is_bounded_by_distinct_keys_and_a_stopped_worker_refuses() {
        let (w, _, _rx) = worker(Duration::from_millis(200));
        assert!(w.request(params(0, 1, &["A"]))); // running
        for k in 1..=PRICE_BOUND as u64 {
            assert!(w.request(params(k, 1, &["A"])), "key {k} fits");
        }
        assert!(!w.request(params(PRICE_BOUND as u64 + 1, 1, &["A"])), "one over the bound is refused");
        assert!(w.request(params(1, 2, &["A"])), "a replacement for a queued key always fits");
        w.shutdown();
        assert!(!w.request(params(99, 1, &["A"])));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data pricing::`
Expected: compile error — the module does not exist.

- [ ] **Step 3: Implement**

`crates/geode-data/src/pricing/mod.rs`:

```rust
//! The pricing seam's data-tier side (line-pricer spec §5.3, §5.5): which
//! `Pricer` this build has, and the worker that runs it.

pub mod worker;

use geode_core::pricing::Pricer;
use std::collections::HashMap;
use std::sync::Arc;

pub use worker::{PRICE_BOUND, PriceSink, PricingWorker};

/// The pricer the service runs. `pricer: None` is a configured name this
/// build cannot serve — every line answers [`PricerConfig::missing_reason`],
/// never a startup failure (spec §4, roadmap ruling 3 applied to a
/// library).
#[derive(Clone, Default)]
pub struct PricerConfig {
    pub name: String,
    pub pricer: Option<Arc<dyn Pricer>>,
}

impl PricerConfig {
    pub fn with(pricer: Arc<dyn Pricer>) -> PricerConfig {
        PricerConfig { name: pricer.name().to_string(), pricer: Some(pricer) }
    }

    pub fn missing(name: &str) -> PricerConfig {
        PricerConfig { name: name.to_string(), pricer: None }
    }

    pub fn missing_reason(&self) -> String {
        if self.name.is_empty() {
            "no pricer is configured".to_string()
        } else {
            format!("pricer \"{}\" is not built into this binary", self.name)
        }
    }
}

/// The pricers this build has, keyed by [`Pricer::name`]. Shaped exactly
/// like [`crate::adapter::AdapterRegistry`]: `geode-app` is the one crate
/// that knows what was compiled in.
#[derive(Default, Clone)]
pub struct PricerRegistry {
    pricers: HashMap<String, Arc<dyn Pricer>>,
}

impl PricerRegistry {
    pub fn register(&mut self, pricer: Arc<dyn Pricer>) {
        let name = pricer.name().to_string();
        if self.pricers.insert(name.clone(), pricer).is_some() {
            tracing::warn!(
                target: "geode::pricing",
                "pricer '{name}' registered twice; the later registration wins"
            );
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Pricer>> {
        self.pricers.get(name).cloned()
    }

    /// Sorted, so a diagnostic listing them reads the same way twice.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.pricers.keys().cloned().collect();
        names.sort();
        names
    }
}
```

`crates/geode-data/src/pricing/worker.rs`:

```rust
//! The pricing worker (line-pricer spec §5.3): one thread, its own
//! queue, separate from the DuckDB query pool.
//!
//! Rules, each pinned by a test below:
//! * latest wins per key — a queued batch for a key is replaced by a
//!   newer one in place; a batch arriving while its key is running queues
//!   behind it;
//! * cancel by key drops the queued batch and stops a running one at the
//!   next line boundary, delivering the lines already priced;
//! * every `price` call runs under `catch_unwind` + `panic::contained`,
//!   so a panic is one line's error and the worker keeps going;
//! * no pricer configured answers every line with the configured name.

use super::PricerConfig;
use geode_core::pricing::{PriceOutcome, PriceParams};
use geode_core::query::QueryKey;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

pub type PriceSink = Arc<dyn Fn(PriceOutcome) -> bool + Send + Sync>;

/// Distinct keys that may wait; one batch per key.
pub const PRICE_BOUND: usize = 64;

#[derive(Default)]
struct Queue {
    order: VecDeque<QueryKey>,
    pending: HashMap<QueryKey, PriceParams>,
    running: Option<QueryKey>,
    cancel_running: bool,
    shutdown: bool,
}

pub struct PricingWorker {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl PricingWorker {
    pub fn spawn(config: PricerConfig, sink: PriceSink) -> PricingWorker {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let thread = {
            let queue = Arc::clone(&queue);
            std::thread::Builder::new()
                .name("geode-pricing".into())
                .spawn(move || run(queue, config, sink))
                .expect("spawn the pricing worker")
        };
        PricingWorker { queue, thread: Mutex::new(Some(thread)) }
    }

    pub fn request(&self, params: PriceParams) -> bool {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.shutdown {
            return false;
        }
        let key = params.key;
        if let Some(slot) = q.pending.get_mut(&key) {
            *slot = params;
        } else {
            if q.order.len() >= PRICE_BOUND {
                return false;
            }
            q.order.push_back(key);
            q.pending.insert(key, params);
        }
        cvar.notify_all();
        true
    }

    pub fn cancel(&self, key: QueryKey) {
        let (lock, _) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.pending.remove(&key).is_some() {
            q.order.retain(|k| *k != key);
        }
        if q.running == Some(key) {
            q.cancel_running = true;
        }
    }

    pub fn shutdown(&self) {
        {
            let (lock, cvar) = &*self.queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.shutdown = true;
            q.cancel_running = true;
            cvar.notify_all();
        }
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

impl Drop for PricingWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(queue: Arc<(Mutex<Queue>, Condvar)>, config: PricerConfig, sink: PriceSink) {
    let (lock, cvar) = &*queue;
    let mut refusal_logged = false;
    loop {
        let params = {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(key) = q.order.pop_front() {
                    if let Some(p) = q.pending.remove(&key) {
                        q.running = Some(key);
                        q.cancel_running = false;
                        break p;
                    }
                }
                q = cvar.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let started = std::time::Instant::now();
        let mut results = Vec::with_capacity(params.lines.len());
        let mut failures = 0usize;
        for line in &params.lines {
            {
                let q = lock.lock().unwrap_or_else(|e| e.into_inner());
                if q.cancel_running || q.shutdown {
                    break;
                }
            }
            let result = match &config.pricer {
                None => Err(config.missing_reason()),
                Some(pricer) => {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        geode_core::panic::contained(|| pricer.price(&line.request))
                    }));
                    match outcome {
                        Ok(Ok(r)) => Ok(r),
                        Ok(Err(e)) => Err(e.0),
                        Err(payload) => {
                            let message = payload
                                .downcast_ref::<&str>()
                                .map(|s| s.to_string())
                                .or_else(|| payload.downcast_ref::<String>().cloned())
                                .unwrap_or_else(|| "non-string panic payload".to_string());
                            tracing::warn!(
                                target: "geode::pricing",
                                "pricer panicked on line {} of key {}: {message}",
                                line.id, params.key.0
                            );
                            Err(format!("pricer panicked: {message}"))
                        }
                    }
                }
            };
            if result.is_err() {
                failures += 1;
            }
            results.push((line.id, line.revision, result));
        }
        {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.running = None;
            q.cancel_running = false;
        }
        tracing::debug!(
            target: "geode::pricing",
            "priced {} of {} line(s) for key {} tag {} in {:?} ({failures} failed)",
            results.len(), params.lines.len(), params.key.0, params.tag, started.elapsed()
        );
        let delivered = sink(PriceOutcome {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            results,
        });
        if !delivered && !refusal_logged {
            refusal_logged = true;
            tracing::warn!(
                target: "geode::pricing",
                "a price outcome for key {} was not delivered; further refusals are counted, not logged",
                params.key.0
            );
        }
    }
}
```

Add `pub mod pricing;` to `crates/geode-data/src/lib.rs` after `pub mod ingest;`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-data pricing::`
Expected: 8 passed (2 in `pricing::tests`, 6 in `pricing::worker::tests`). If `cancel_drops_a_queued_batch…` is flaky on a loaded machine, raise the per-line delay to 80 ms and the sleep to 120 ms; do not loosen the assertions.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy -p geode-data --all-targets -- -D warnings
git add crates/geode-data/src/pricing crates/geode-data/src/lib.rs
git commit -m "data: PricerRegistry, PricerConfig and the PricingWorker (line-pricer §5.3)"
```

---

### Task 6: `Request::{Price, Publish}`, `DataEvent::Price`, the service wiring, the local-source ingest arms

**Files:**
- Modify: `crates/geode-data/src/handle.rs` (`Request` at L25–45; methods after `catalog` at L144; `serve` arms at L215–289)
- Modify: `crates/geode-data/src/service.rs` (`DataServiceConfig` at L42–62; `DataEvent` at L65–122; `DataService` fields at L554–592; `open` — the ingest sink's `Published`/`Failed` arms at L789–930 and the struct literal at L1228–1236; `cancel` at L1480; `shutdown` at L1583; new `price`/`publish` methods beside `document` at L1419)
- Modify: `crates/geode-data/src/lib.rs:16` (re-export `PricerConfig`, `PricerRegistry` — `pub use pricing::{PricerConfig, PricerRegistry};`)
- Modify (every `DataServiceConfig { … }` literal gains `pricer: PricerConfig::default()`): `service.rs` tests `service()` L1609, `document_service()` L1654, `subscribed_service_for` L1752, `handle.rs` tests (L413, L479, L556 and any other), `crates/geode-app/src/bridge.rs:103` (Task 8 replaces the default there)
- Test: `crates/geode-data/src/service.rs` (`mod tests`), `crates/geode-data/src/handle.rs` (`mod tests`)

**Interfaces:**
- Produces:
```rust
// handle.rs
Request::Price(PriceParams)
Request::Publish(LocalPublish)
impl DataHandle { pub fn price(&self, params: PriceParams) -> bool; pub fn publish(&self, publish: LocalPublish) -> bool; }
// service.rs
DataServiceConfig { …, pub pricer: PricerConfig }
DataEvent::Price(PriceOutcome)
impl DataService {
    /// `false` when the worker's queue refused it (bounded) — the caller resubmits next frame.
    pub fn price(&self, params: PriceParams) -> bool;
    /// Refuses (an error `Diagnostics` event, nothing written) unless the dataset is declared `local`.
    pub fn publish(&self, publish: LocalPublish);
}
```
- `cancel(key)` also cancels the pricing worker's batch for `key`; `shutdown` stops the worker after the pool.

- [ ] **Step 1: Write the failing tests**

In `service.rs`'s `mod tests`, a local-dataset fixture and four tests. The fixture needs a `local = true` document dataset; add it to `crate::store::ddl::tests_support` beside `cvi_dataset()` (L554) as:

```rust
    /// A `local = true` document dataset for the publish tests
    /// (line-pricer spec §7.2): one key, one axis, one value.
    pub(crate) fn local_dataset() -> DatasetSpec {
        let text = r#"
[sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]
[sheets.columns.sheet]
type = "utf8"
role = "dimension"
[sheets.columns.line]
type = "i64"
role = "axis"
[sheets.columns.qty]
type = "i64"
role = "value"
"#;
        let doc = geode_core::config::LayerDoc::builtin("datasets", text).unwrap();
        let (schema, diags) = SchemaSpec::from_doc(&geode_core::config::merge_docs(&[doc]));
        assert!(diags.is_empty(), "{diags:?}");
        schema.datasets.into_iter().find(|d| d.name == "sheets").unwrap()
    }

    pub(crate) fn sheet_rows(sheet: &str, qty: &[i64]) -> DocumentRows {
        DocumentRows {
            key: vec![sheet.to_string()],
            attributes: Vec::new(),
            axes: vec![("line".to_string(), Column::I64((1..=qty.len() as i64).collect()))],
            values: vec![("qty".to_string(), Column::I64(qty.to_vec()))],
        }
    }
```

(Match the way `cvi_dataset()` builds its `SchemaSpec` from text; copy its exact merge call if it differs from `merge_docs(&[doc])`.)

Then in `service.rs` tests:

```rust
    fn local_service() -> (tempfile::TempDir, DataService, std::sync::mpsc::Receiver<DataEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(crate::store::ddl::tests_support::local_dataset());
        schema.datasets.push(crate::store::ddl::tests_support::cvi_dataset());
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            pricer: PricerConfig::with(Arc::new(crate::pricing::worker::tests::FakePricer {
                asked: Default::default(),
                delay: Duration::ZERO,
            })),
        })
        .unwrap();
        (dir, service, rx)
    }

    fn until<T>(rx: &std::sync::mpsc::Receiver<DataEvent>, mut pick: impl FnMut(DataEvent) -> Option<T>) -> T {
        loop {
            let e = rx.recv_timeout(Duration::from_secs(30)).expect("an event");
            if let Some(t) = pick(e) {
                return t;
            }
        }
    }

    #[test]
    fn a_local_publish_lands_a_generation_the_document_request_reads_back() {
        let (_d, service, rx) = local_service();
        service.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: crate::store::ddl::tests_support::sheet_rows("untitled-1", &[1, -2, 3]),
        });
        let (dataset, batch) = until(&rx, |e| match e {
            DataEvent::Published { dataset, batch, .. } => Some((dataset, batch)),
            _ => None,
        });
        assert_eq!((dataset.as_str(), batch.as_str()), ("sheets", "untitled-1"));
        service
            .document(&DocumentParams {
                key: QueryKey(5),
                tag: 1,
                submitted: Instant::now(),
                dataset: "sheets".into(),
                document_key: vec!["untitled-1".into()],
                as_of: AsOf::Live,
            })
            .unwrap();
        let snapshot = until(&rx, |e| match e {
            DataEvent::Query(o) if o.key == QueryKey(5) => Some(o.snapshot.unwrap()),
            _ => None,
        });
        assert_eq!(snapshot.rows(), 3);
    }

    #[test]
    fn a_local_publish_emits_no_health_event_and_a_load_ended() {
        let (_d, service, rx) = local_service();
        service.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: crate::store::ddl::tests_support::sheet_rows("s", &[1]),
        });
        let mut saw_published = false;
        let mut saw_ended = false;
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
            match e {
                DataEvent::Health { source, .. } => panic!("no health lane for a local publish, got {source}"),
                DataEvent::Published { .. } => saw_published = true,
                DataEvent::LoadEnded => {
                    saw_ended = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_published && saw_ended);
    }

    #[test]
    fn a_publish_to_a_dataset_that_is_not_local_is_refused_unwritten() {
        let (_d, service, rx) = local_service();
        service.publish(LocalPublish {
            dataset: "cvi_params".into(),
            rows: crate::store::ddl::tests_support::cvi_doc("SPX.Z"),
        });
        let diags = until(&rx, |e| match e {
            DataEvent::Diagnostics(d) => Some(d),
            DataEvent::Published { dataset, .. } => panic!("written: {dataset}"),
            _ => None,
        });
        assert!(diags.iter().any(|d| d.severity == Severity::Error && d.message.contains("not a local dataset")), "{diags:?}");
        let catalog = service.catalog(&CatalogParams { key: QueryKey(1), tag: 1, as_of: AsOf::Live });
        assert!(
            catalog.snapshot.datasets.iter().all(|d| d.name != "cvi_params" || d.partitions.is_empty()),
            "nothing landed for cvi_params"
        );
    }

    #[test]
    fn a_price_request_reaches_the_sink_as_a_price_event_and_cancel_reaches_the_worker() {
        let (_d, service, rx) = local_service();
        assert!(service.price(crate::pricing::worker::tests::params(11, 4, &["SPX", "FAIL"])));
        let o = until(&rx, |e| match e {
            DataEvent::Price(o) => Some(o),
            _ => None,
        });
        assert_eq!((o.key, o.tag), (QueryKey(11), 4));
        assert!(o.results[0].2.is_ok());
        assert!(o.results[1].2.is_err());
        service.cancel(QueryKey(11)); // must not panic or block
    }
```

(Use the file's existing `cvi_doc` fixture's real signature; if it takes no argument, drop the `"SPX.Z"`. Adjust `catalog.snapshot.datasets`/`partitions` to the real `CatalogSnapshot`/`DatasetCatalog` field names in `geode_core::query` — the assertion is "no partition exists for `cvi_params`".)

In `handle.rs`'s `mod tests`, beside `a_catalog_request_reaches_the_sink_as_a_catalog_event` (L556), copying its real-service setup verbatim but with the local dataset:

```rust
    #[test]
    fn price_and_publish_requests_reach_the_real_service() {
        let dir = tempfile::tempdir().unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(crate::store::ddl::tests_support::local_dataset());
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn(
            DataServiceConfig {
                db_path: dir.path().join("geode.duckdb"),
                schema,
                views: Vec::new(),
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
                adapters: Default::default(),
                documents: Default::default(),
                pricer: PricerConfig::missing("vendor"),
            },
            sink,
        );
        assert!(handle.price(crate::pricing::worker::tests::params(2, 9, &["SPX"])));
        assert!(handle.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: crate::store::ddl::tests_support::sheet_rows("a", &[7]),
        }));
        let mut priced = false;
        let mut published = false;
        while !(priced && published) {
            match rx.recv_timeout(std::time::Duration::from_secs(30)).unwrap() {
                DataEvent::Price(o) => {
                    assert_eq!((o.key, o.tag), (QueryKey(2), 9));
                    assert_eq!(o.results[0].2.as_ref().unwrap_err(), "pricer \"vendor\" is not built into this binary");
                    priced = true;
                }
                DataEvent::Published { dataset, .. } if dataset == "sheets" => published = true,
                _ => {}
            }
        }
        handle.shutdown();
        assert!(!handle.price(crate::pricing::worker::tests::params(2, 10, &["SPX"])), "refused after shutdown");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-data local_ price_and_publish a_price_request`
Expected: compile errors — `pricer` field, `Request::Price`, `DataEvent::Price`, `publish`.

- [ ] **Step 3: Implement**

`handle.rs`:
- Imports gain `geode_core::pricing::{LocalPublish, PriceParams}`.
- `Request` gains, after `Catalog(CatalogParams)`:
```rust
    /// The line pricer's batch (line-pricer spec §5.3), answered by the
    /// pricing worker as `DataEvent::Price`.
    Price(PriceParams),
    /// A document the app authored, published as a generation of a
    /// `local = true` dataset (spec §5.3, §7.2).
    Publish(LocalPublish),
```
- Methods after `catalog`:
```rust
    /// Queue a pricing batch. `false` means it was not queued (the
    /// request channel or the worker's queue is full); the tile keeps
    /// its lines stale and resubmits next frame.
    pub fn price(&self, params: PriceParams) -> bool {
        self.send(Request::Price(params))
    }

    /// Queue a local publish. Refused (an error `Diagnostics` event,
    /// nothing written) unless the dataset is declared `local`.
    pub fn publish(&self, publish: LocalPublish) -> bool {
        self.send(Request::Publish(publish))
    }
```
- `serve` arms, after `Request::Catalog`:
```rust
            Request::Price(params) => {
                if !service.price(params) {
                    tracing::warn!(target: "geode::pricing", "the pricing queue is full; a batch was dropped");
                }
            }
            Request::Publish(publish) => service.publish(publish),
```
(`service.price` refusing here has no key to answer under without cloning the params first; take `params.key` into a local before the move if you want it in the log line.)

`service.rs`:
- Imports gain `crate::pricing::{PricerConfig, PricingWorker, PriceSink}` and `geode_core::pricing::{LocalPublish, PriceOutcome, PriceParams, LOCAL_SOURCE}`.
- `DataServiceConfig` gains `pub pricer: PricerConfig,` last.
- `DataEvent` gains, after `Catalog(CatalogOutcome)`:
```rust
    /// A pricing batch's answer (line-pricer spec §5.3), addressed to
    /// the tile key that asked.
    Price(PriceOutcome),
```
- `DataService` gains, after `pool: QueryPool,` (drop order: an independent worker, stopped in `shutdown` after the pool):
```rust
    pricing: PricingWorker,
```
- In `open`, after the pool is spawned (L774):
```rust
        let price_sink: PriceSink = {
            let sink = Arc::clone(&sink);
            Arc::new(move |o| sink(DataEvent::Price(o)))
        };
        let pricing = PricingWorker::spawn(config.pricer.clone(), price_sink);
```
  and the struct literal gains `pricing,`.
- In the ingest sink's `IngestEvent::Published` arm, after the `tracing::info!` and the `DataEvent::Published` send (bind its `bool` as `delivered` as the arm already does) and BEFORE `health_tracker.report_load_and_emit`:
```rust
                    // A local publish (line-pricer spec §5.3) has no
                    // source a `[sources]` entry declares, so no health
                    // lane: `Published` and `LoadEnded` only.
                    if source == LOCAL_SOURCE {
                        let _ = sink(DataEvent::LoadEnded);
                        return delivered;
                    }
```
- In the `IngestEvent::Failed` arm, before its `report_load_and_emit`:
```rust
                    if source == LOCAL_SOURCE {
                        let delivered = sink(DataEvent::Diagnostics(vec![Diagnostic {
                            severity: Severity::Error,
                            layer: None,
                            file: None,
                            message: format!("local publish of {dataset}/{batch} failed: {reason}"),
                            path: None,
                        }]));
                        let _ = sink(DataEvent::LoadEnded);
                        return delivered;
                    }
```
  (Keep the arm's existing `log_ingest_failure(&dataset, &batch, &reason)` call above it, so the failure is logged either way.)
- New methods beside `document`:
```rust
    /// The line pricer's batch (spec §5.3). `false` when the worker's
    /// bounded queue refused it.
    pub fn price(&self, params: PriceParams) -> bool {
        self.pricing.request(params)
    }

    /// Publish an app-authored document (spec §5.3, §7.2). The dataset
    /// must be declared `local = true`: anything else is refused with an
    /// error diagnostic and nothing is written. Accepted, the rows go
    /// through the ingest runner's document lane exactly as a
    /// subscribed document does — same validation, same `contained`
    /// boundary, same `Published` event — stamped `LOCAL_SOURCE`.
    pub fn publish(&self, publish: LocalPublish) {
        let local = self
            .config
            .schema
            .dataset(&publish.dataset)
            .is_some_and(|d| d.local);
        if !local {
            tracing::warn!(
                target: "geode::ingest",
                "refused a local publish to '{}': not a local dataset",
                publish.dataset
            );
            let _ = (self.sink)(DataEvent::Diagnostics(vec![Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!(
                    "refused a local publish to '{}': not a local dataset (declare `local = true` on a document dataset)",
                    publish.dataset
                ),
                path: None,
            }]));
            return;
        }
        let now = chrono::Utc::now();
        self.ingest.submit_document(DocumentJob {
            source: LOCAL_SOURCE.to_string(),
            dataset: publish.dataset,
            rows: publish.rows,
            source_time: now,
            received_at: now,
            bytes: 0,
        });
    }
```
  `DataService` does not currently keep the sink; add `sink: EventSink` as a field (after `config`) set from `open`'s argument (`Arc::clone(&sink)` before the sink is moved into closures), so `publish` can refuse through it.
- `cancel`:
```rust
    pub fn cancel(&self, key: QueryKey) {
        self.pool.cancel(key);
        self.pricing.cancel(key);
    }
```
- `shutdown`: add `self.pricing.shutdown();` after `self.pool.shutdown();`.
- Every `DataServiceConfig { … }` literal named by the compiler gains `pricer: PricerConfig::default(),` (tests) — the bridge's literal too, for now.

`lib.rs`: `pub use pricing::{PricerConfig, PricerRegistry};`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-data && cargo test --workspace`
Expected: green. If `a_publish_to_a_dataset_that_is_not_local_is_refused_unwritten` hangs, the `Diagnostics` event is not being sent through the stored sink — check the field was set from `open`'s argument, not a clone taken after the closures moved it.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates
git commit -m "data: Request::{Price, Publish}, DataEvent::Price, the pricing worker in DataService (line-pricer §5.3)"
```

---

### Task 7: `Delivery::Price` and the explicit arms

**Files:**
- Modify: `crates/geode-shell/src/module.rs` (`Delivery` at L30–53; placeholder `deliver` at L314–317; recording `Recorded` at L361–368 and `deliver` at L575–582)
- Modify: `crates/geode-shell/src/shell/tests/occupants.rs:63–65` (the `WatchingContent` arm) and add one test beside L1461
- Modify: `crates/geode-blotter/src/content.rs:122–126`, `crates/geode-marketdata/src/content.rs:200–204`, `crates/geode-diagnostics/src/lib.rs:107–113`
- Test: `crates/geode-shell/src/shell/tests/occupants.rs`

**Interfaces:**
- Produces: `Delivery::Price(PriceOutcome)`; `Delivery::key()` answers it; `Recorded::Priced(TileId, u64)` (tile, tag) in the recording module.

- [ ] **Step 1: Write the failing test** (in `shell/tests/occupants.rs`, directly after `a_delivery_reaches_the_tile_addressed_by_its_key_and_no_other`)

```rust
/// `Delivery::Price` (line-pricer spec §5.4) rides the same router:
/// keyed like a query, delivered to that tile alone.
#[gpui::test]
fn a_price_delivery_is_routed_by_key_like_a_query(cx: &mut gpui::TestAppContext) {
    use crate::module::Delivery;
    use geode_core::pricing::PriceOutcome;
    use geode_core::query::QueryKey;
    use std::time::Instant;

    let (services, log) = services_with_recorder();
    let (window, mut cx) = open_shell(cx, services);
    let shell = shell_of(&window, &mut cx);
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let first = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile().unwrap());
    dispatch_and_draw(&shell, &mut cx, "tile::add_rec");
    let second = shell.read_with(&cx, |s, _| s.services.workspaces.active().focused_tile().unwrap());

    cx.update(|window, cx| {
        shell.update(cx, |s, cx| {
            s.deliver(
                Delivery::Price(PriceOutcome {
                    key: QueryKey(second.0),
                    tag: 7,
                    submitted: Instant::now(),
                    results: Vec::new(),
                }),
                window,
                cx,
            );
        });
    });

    let log = log.borrow();
    assert!(
        log.iter().any(|r| matches!(r, crate::module::recording::Recorded::Priced(t, 7) if *t == second)),
        "{log:?}"
    );
    assert!(
        !log.iter().any(|r| matches!(r, crate::module::recording::Recorded::Priced(t, _) if *t == first)),
        "{log:?}"
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-shell --features test-support a_price_delivery`
Expected: compile error — no `Delivery::Price`, no `Recorded::Priced`.

- [ ] **Step 3: Implement**

`module.rs`:
```rust
/// What the shell routes to a tile by its id (market-data spec §8.6,
/// line-pricer spec §5.4). One variant per outcome kind, no wildcard
/// arms anywhere: adding a variant makes the compiler name every site.
#[derive(Debug)]
pub enum Delivery {
    Query(QueryOutcome),
    Price(PriceOutcome),
}

impl Delivery {
    pub fn key(&self) -> QueryKey {
        match self {
            Delivery::Query(outcome) => outcome.key,
            Delivery::Price(outcome) => outcome.key,
        }
    }
}
```
with `use geode_core::pricing::PriceOutcome;` at the top. Placeholder: `Delivery::Query(_) | Delivery::Price(_) => {}` is a wildcard in spirit — write two arms: `Delivery::Query(_) => {}` and `Delivery::Price(_) => {}`. Recording: `Recorded` gains `Priced(TileId, u64),` and `deliver` gains
```rust
                Delivery::Price(outcome) => {
                    self.log.borrow_mut().push(Recorded::Priced(self.tile, outcome.tag));
                }
```
Blotter, market-data and diagnostics `content.rs`/`lib.rs`: add `Delivery::Price(_) => {}` with a one-line comment ("this tile never prices; an outcome addressed here is a routing bug the shell already logs"). The shell test's `WatchingContent`: the same arm.

- [ ] **Step 4: Verify**

Run: `cargo test --workspace && cargo check -p geode-shell --features test-support --all-targets`
Expected: green; the new test passes.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates
git commit -m "shell: Delivery::Price routed by key; every occupant names it (line-pricer §5.4)"
```

---

### Task 7b: `MarketOverrides` and `Pricer::set_overrides`, batch-scoped in the worker

Added mid-execution (user ruling 2026-09-19, spec ruling 1 amended): the real library takes a `PricingDataSource` instance the app may override (spot levels now, CVI and dividend documents later), and overrides are STATEFUL on the library's instance: `set_overrides` then `price`. The worker makes the pair batch-scoped.

**Files:**
- Modify: `crates/geode-core/src/pricing.rs` (`MarketOverrides`, the trait method, `PriceParams.overrides`)
- Modify: `crates/geode-pricing/src/lib.rs` (`MockPricer` holds the last overrides; the reference-spot rule; the refusal)
- Modify: `crates/geode-data/src/pricing/worker.rs` (`run`: set once per batch under the boundary; whole-batch failure; `FakePricer` records overrides; `params` helper gains the field)
- Modify: `crates/geode-data/src/service.rs` (only if a `PriceParams` literal exists outside the `params` helper — the refusal answer in `DataService::price` builds a `PriceOutcome`, not params, so probably nothing)
- Test: all three files' `mod tests`

**Interfaces:**
- Consumes: Task 1's vocabulary, Task 2's mock, Task 5's worker.
- Produces (in `geode_core::pricing`):
```rust
/// What the app overrides in the library's `PricingDataSource` (spec ruling 1):
/// spot levels by underlying now; CVI and dividend documents are later fields.
/// Plain data — the library interprets it, the app never does.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MarketOverrides { pub spot: BTreeMap<String, f64> }
pub trait Pricer: Send + Sync {
    fn name(&self) -> &str;
    /// Replace the overridable market data for every `price` call that follows,
    /// until the next call here. Stateful on purpose (spec ruling 1); the worker
    /// calls it once per batch, so a batch's lines all see the same overrides and
    /// no other batch's.
    fn set_overrides(&self, overrides: &MarketOverrides) -> Result<(), PricingError>;
    fn price(&self, req: &PriceRequest) -> Result<PriceResult, PricingError>;
}
pub struct PriceParams { pub key: QueryKey, pub tag: u64, pub submitted: Instant, pub overrides: MarketOverrides, pub lines: Vec<PriceLine> }
```
- The worker rule: before a batch's first line, `set_overrides(&params.overrides)` runs under `catch_unwind(AssertUnwindSafe(|| contained(|| …)))`; an `Err(e)` or a panic fails EVERY line of the batch with `format!("overrides refused: {reason}")` (the panic message for a panic) and prices none; the outcome is still emitted with every line's `(id, revision, Err)`. With `pricer: None` the missing-reason rule applies as before (no `set_overrides` call).
- The mock: `MockPricer { delay, overrides: Mutex<MarketOverrides> }`; `set_overrides` refuses any spot that is not a positive finite number with `PricingError("spot override must be a positive finite number")` and otherwise stores a clone; `price` adds `delta * (override - reference)` to the price when `overrides.spot` has the line's underlying, where `reference` is the absolute strike or `100.0` for a percent strike.

- [ ] **Step 1: Write the failing tests**

`crates/geode-core/src/pricing.rs` `mod tests`, add:

```rust
    #[test]
    fn overrides_default_to_none_and_compare_by_value() {
        let a = MarketOverrides::default();
        assert!(a.spot.is_empty());
        let mut b = MarketOverrides::default();
        b.spot.insert("SPX".into(), 5000.0);
        assert_ne!(a, b);
        assert_eq!(b.clone(), b);
    }
```

`crates/geode-pricing/src/lib.rs` `mod tests`, add (reuse the file's `vanilla`/`req` helpers):

```rust
    fn overrides(pairs: &[(&str, f64)]) -> MarketOverrides {
        let mut o = MarketOverrides::default();
        for (u, s) in pairs {
            o.spot.insert(u.to_string(), *s);
        }
        o
    }

    #[test]
    fn a_spot_override_moves_the_price_in_deltas_sign_from_the_reference_spot() {
        let p = MockPricer::new();
        for kind in [OptionKind::Call, OptionKind::Put] {
            let r = req(vanilla("SPX", kind, 5000.0), 0.0, 0.0);
            p.set_overrides(&MarketOverrides::default()).unwrap();
            let base = p.price(&r).unwrap();
            p.set_overrides(&overrides(&[("SPX", 5200.0)])).unwrap();
            let up = p.price(&r).unwrap();
            assert_eq!((up.price - base.price).signum(), base.delta.signum(), "{kind:?}");
            p.set_overrides(&overrides(&[("SPX", 4800.0)])).unwrap();
            let down = p.price(&r).unwrap();
            assert_eq!((down.price - base.price).signum(), -base.delta.signum(), "{kind:?}");
            // An override for another underlying changes nothing.
            p.set_overrides(&overrides(&[("NDX", 20000.0)])).unwrap();
            assert_eq!(p.price(&r).unwrap(), base);
        }
    }

    #[test]
    fn a_percent_strike_uses_one_hundred_as_its_reference_spot() {
        let p = MockPricer::new();
        let r = PriceRequest {
            instrument: Instrument::Vanilla(Vanilla {
                underlying: "SPX".into(),
                expiry: Expiry::Tenor("3m".into()),
                strike: Strike::Percent(95.0),
                kind: OptionKind::Call,
            }),
            shifts: Shifts::default(),
        };
        p.set_overrides(&MarketOverrides::default()).unwrap();
        let base = p.price(&r).unwrap();
        p.set_overrides(&overrides(&[("SPX", 100.0)])).unwrap();
        assert_eq!(p.price(&r).unwrap(), base, "an override AT the reference moves nothing");
        p.set_overrides(&overrides(&[("SPX", 101.0)])).unwrap();
        assert!(p.price(&r).unwrap().price > base.price);
    }

    #[test]
    fn a_non_positive_or_non_finite_spot_override_is_refused_and_the_previous_one_stays() {
        let p = MockPricer::new();
        let r = req(vanilla("SPX", OptionKind::Call, 5000.0), 0.0, 0.0);
        p.set_overrides(&overrides(&[("SPX", 5200.0)])).unwrap();
        let with = p.price(&r).unwrap();
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let err = p.set_overrides(&overrides(&[("SPX", bad)])).unwrap_err();
            assert_eq!(err.0, "spot override must be a positive finite number");
        }
        assert_eq!(p.price(&r).unwrap(), with, "a refused set leaves the previous overrides in place");
    }
```

`crates/geode-data/src/pricing/worker.rs` `pub(crate) mod tests`: extend `FakePricer` with `pub(crate) overrides_seen: Arc<Mutex<Vec<MarketOverrides>>>` (every existing constructor site in this file and in `service.rs` gains `overrides_seen: Default::default()`), implement `set_overrides` on it to push a clone and refuse when the map contains the key `"REFUSE"` (`Err(PricingError("refused overrides".into()))`); `params` gains `overrides: MarketOverrides::default()`; add a `params_with_overrides(key, tag, underlyings, overrides)` helper; then add:

```rust
    #[test]
    fn overrides_are_set_once_per_batch_before_its_first_line() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: PriceSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = PricingWorker::spawn(
            PricerConfig::with(Arc::new(FakePricer { asked: asked.clone(), delay: Duration::ZERO, overrides_seen: seen.clone() })),
            sink,
        );
        let mut o = MarketOverrides::default();
        o.spot.insert("SPX".into(), 5000.0);
        assert!(w.request(params_with_overrides(1, 1, &["SPX", "NDX"], o.clone())));
        assert!(w.request(params_with_overrides(2, 1, &["SPX"], MarketOverrides::default())));
        next(&rx);
        next(&rx);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "once per batch, not per line: {seen:?}");
        assert_eq!(seen[0], o);
        assert_eq!(seen[1], MarketOverrides::default());
        w.shutdown();
    }

    #[test]
    fn refused_overrides_fail_every_line_of_the_batch_and_price_none() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: PriceSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = PricingWorker::spawn(
            PricerConfig::with(Arc::new(FakePricer { asked: asked.clone(), delay: Duration::ZERO, overrides_seen: Default::default() })),
            sink,
        );
        let mut bad = MarketOverrides::default();
        bad.spot.insert("REFUSE".into(), 1.0);
        assert!(w.request(params_with_overrides(3, 1, &["SPX", "NDX"], bad)));
        let o = next(&rx);
        assert_eq!(o.results.len(), 2);
        for (_, _, r) in &o.results {
            assert_eq!(r.as_ref().unwrap_err(), "overrides refused: refused overrides");
        }
        assert!(asked.lock().unwrap().is_empty(), "no line was priced against the wrong data source");
        assert!(w.request(params(3, 2, &["SPX"])), "the worker is still alive");
        assert!(next(&rx).results[0].2.is_ok());
        w.shutdown();
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core pricing:: && cargo test -p geode-pricing && cargo test -p geode-data pricing::`
Expected: compile errors — `MarketOverrides`, `set_overrides`, `overrides` field.

- [ ] **Step 3: Implement**

`geode-core/src/pricing.rs`: add `use std::collections::BTreeMap;`, the `MarketOverrides` struct and the trait method as in **Interfaces**, and `pub overrides: MarketOverrides,` on `PriceParams` between `submitted` and `lines`, with the doc line "The sheet's overrides for this batch; the worker sets them once before the first line (spec §5.3)."

`geode-pricing/src/lib.rs`:
```rust
pub struct MockPricer {
    delay: Duration,
    /// The last `set_overrides`, as the real library's `PricingDataSource` would hold it.
    overrides: Mutex<MarketOverrides>,
}
```
(`new`/`with_delay` initialise it to `Mutex::new(MarketOverrides::default())`; drop `#[derive(Clone)]` if `Mutex` refuses it, or implement `Clone` by hand cloning the inner value — `Debug` stays derived.)
```rust
    fn set_overrides(&self, overrides: &MarketOverrides) -> Result<(), PricingError> {
        if overrides.spot.values().any(|s| !(s.is_finite() && *s > 0.0)) {
            return Err(PricingError("spot override must be a positive finite number".to_string()));
        }
        *self.overrides.lock().unwrap_or_else(|e| e.into_inner()) = overrides.clone();
        Ok(())
    }
```
and in `price`, after `price` is computed:
```rust
        // A spot override moves the price from the mock's reference spot —
        // the absolute strike, or 100 for a percent strike — in delta's
        // sign, so a higher spot raises a call and lowers a put. Not a
        // model; plausible motion for a trader typing `:spot`.
        let overrides = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(spot) = overrides.spot.get(req.instrument.underlying()) {
            let reference = match req.instrument.strike() {
                Strike::Absolute(k) => k,
                Strike::Percent(_) => 100.0,
            };
            price += delta * (spot - reference);
        }
```
`Instrument::strike(&self) -> Strike` does not exist yet: add it to `geode_core::pricing::Instrument` beside `underlying`/`kind` (`self.vanilla().strike`, `Strike` is `Copy`).

`geode-data/src/pricing/worker.rs`, in `run`, after `let mut failures = 0usize;` and before the line loop:
```rust
        // Spec §5.3 "Overrides once per batch": a refusal or a panic here
        // fails the whole batch — a line priced against the wrong data
        // source is worse than no price.
        let overrides_failed: Option<String> = match &config.pricer {
            None => None,
            Some(pricer) => {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    geode_core::panic::contained(|| pricer.set_overrides(&params.overrides))
                }));
                match outcome {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(format!("overrides refused: {}", e.0)),
                    Err(payload) => {
                        let message = panic_message(&payload);
                        tracing::warn!(target: "geode::pricing", "set_overrides panicked for key {}: {message}", params.key.0);
                        Some(format!("overrides refused: pricer panicked: {message}"))
                    }
                }
            }
        };
```
then at the top of the line loop body (after the cancel check), `if let Some(reason) = &overrides_failed { failures += 1; results.push((line.id, line.revision, Err(reason.clone()))); continue; }`. Factor the existing payload-downcast chain into `fn panic_message(payload: &Box<dyn Any + Send>) -> String` so both sites share it.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p geode-core pricing:: && cargo test -p geode-pricing && cargo test -p geode-data && cargo test --workspace`
Expected: green (the service and handle tests build `params` through the helper and need no change; if a `PriceParams` literal exists elsewhere the compiler names it — add `overrides: MarketOverrides::default()`).

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates
git commit -m "pricing: MarketOverrides and Pricer::set_overrides, batch-scoped in the worker (line-pricer ruling 1)" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 8: `geode-app` wiring — the registry, `[pricing] adapter`, the `Price` arm, the local gate

**Files:**
- Modify: `crates/geode-app/Cargo.toml` (add `geode-pricing.workspace = true`)
- Modify: `crates/geode-app/src/main.rs` (L130–140: build a `PricerRegistry` beside the adapters; `build_shell_services` signature at L645 and its `data_setup` call at L746)
- Modify: `crates/geode-app/src/bridge.rs` (`DataSetup` at L40–59; `data_setup` at L71–124; `Bridge` at L196; `start` at L215; the `DataEvent` match — new `Price` arm after `Query` at L522, the `Published` arm at L527)
- Test: `crates/geode-app/src/bridge.rs` (`mod tests`)

**Interfaces:**
- Produces: `data_setup(config, db_path, adapters, pricers: PricerRegistry) -> Option<DataSetup>`; `DataSetup.local_datasets: HashSet<String>`; `Bridge.local_datasets: Rc<HashSet<String>>`; `[pricing] adapter` (default `"mock"`) resolved through the registry, an unknown name appended to `DataSetup.diagnostics` as a warning naming the registry's names.

- [ ] **Step 1: Write the failing tests** (in `bridge.rs`'s `mod tests`)

```rust
    #[test]
    fn pricing_adapter_resolves_through_the_registry_and_an_unknown_name_is_a_diagnostic() {
        let mut pricers = geode_data::PricerRegistry::default();
        pricers.register(Arc::new(geode_pricing::MockPricer::new()));
        let dir = tempfile::tempdir().unwrap();
        // The default: no [pricing] section at all.
        let config = geode_core::config::test_support::config_from("datasets", "");
        let setup = data_setup(&config, dir.path().join("a.duckdb"), Default::default(), pricers.clone()).unwrap();
        assert_eq!(setup.config.pricer.name, "mock");
        assert!(setup.config.pricer.pricer.is_some());
        assert!(!setup.diagnostics.iter().any(|d| d.message.contains("pricer")));

        let config = geode_core::config::test_support::config_from("app", "[pricing]\nadapter = \"vendor\"\n");
        let setup = data_setup(&config, dir.path().join("b.duckdb"), Default::default(), pricers).unwrap();
        assert_eq!(setup.config.pricer.name, "vendor");
        assert!(setup.config.pricer.pricer.is_none());
        let d = setup.diagnostics.iter().find(|d| d.message.contains("pricer")).unwrap();
        assert_eq!(d.severity, Severity::Warning);
        assert!(d.message.contains("vendor") && d.message.contains("mock"), "{}", d.message);
    }
```

(If `data_setup` returns `None` for a config with no datasets doc, build the config from a one-dataset `datasets` text instead — look at how `test_shell_services` builds its `Config` and reuse that text.)

And, modelled line for line on `loading_and_load_ended_reach_the_diagnostics_entity` (L968) for its `Bridge`/`attach` setup:

```rust
    #[gpui::test]
    fn a_local_publish_does_not_bump_the_frames_data_version_but_a_normal_one_does(
        cx: &mut gpui::TestAppContext,
    ) {
        let window = open_test_window(cx, test_shell_services());
        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(),
            Vec::new(),
            NamedColours::default(),
            SchemaSpec::default(),
            DerivedDimensions::default(),
            FindStyle::default(),
            Duration::from_secs(900),
        ));
        let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let dropped = Arc::new(AtomicU64::new(0));
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(handle.clone(), &CVI, Duration::from_secs(900))),
            handle,
            factory,
            events: rx,
            dropped: dropped.clone(),
            sources: Vec::new(),
            local_datasets: Rc::new(["pricer_sheets".to_string()].into_iter().collect()),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let shell = window.root(&mut vcx).unwrap().read_with(&vcx, |root, _| {
            root.view().clone().downcast::<ShellView>().unwrap()
        });
        let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
        let diagnostics = shell.read_with(&vcx, |s, _| s.diagnostics().clone());
        let before = frame.read_with(&vcx, |f, _| f.versions().data);

        tx.try_send(DataEvent::Published {
            dataset: "pricer_sheets".into(),
            batch: "untitled-1".into(),
            gen_id: 1,
            books: vec![None],
        })
        .unwrap();
        vcx.run_until_parked();
        assert_eq!(frame.read_with(&vcx, |f, _| f.versions().data), before, "a local publish is not a data change");
        assert!(diagnostics.read_with(&vcx, |d, _| d.last_published().is_some_and(|p| p == "pricer_sheets")),
            "diagnostics still saw it");

        tx.try_send(DataEvent::Published {
            dataset: "risk_snapshot".into(),
            batch: "EOD".into(),
            gen_id: 2,
            books: vec![Some("BK1".into())],
        })
        .unwrap();
        vcx.run_until_parked();
        assert_eq!(frame.read_with(&vcx, |f, _| f.versions().data), before + 1);
    }

    #[gpui::test]
    fn a_price_event_is_delivered_to_the_shell_as_delivery_price(cx: &mut gpui::TestAppContext) {
        // Same setup as above; a Price event for a tile key that has no
        // occupant must be dropped by the router without panicking, and
        // one for a live recording tile must reach it. The shell's own
        // router test (Task 7) covers the second half; this pins the
        // bridge arm exists and routes.
        let window = open_test_window(cx, test_shell_services());
        let (handle, _rx) = DataHandle::for_tests();
        let factory = Rc::new(BlotterFactory::new(
            handle.clone(), Vec::new(), NamedColours::default(), SchemaSpec::default(),
            DerivedDimensions::default(), FindStyle::default(), Duration::from_secs(900),
        ));
        let (tx, rx) = async_channel::bounded::<DataEvent>(EVENT_BOUND);
        let bridge = Bridge {
            marketdata: Rc::new(MarketDataFactory::new(handle.clone(), &CVI, Duration::from_secs(900))),
            handle,
            factory,
            events: rx,
            dropped: Arc::new(AtomicU64::new(0)),
            sources: Vec::new(),
            local_datasets: Default::default(),
        };
        cx.update(|cx| attach(&bridge, window, cx));
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        tx.try_send(DataEvent::Price(geode_core::pricing::PriceOutcome {
            key: QueryKey(999),
            tag: 1,
            submitted: std::time::Instant::now(),
            results: Vec::new(),
        }))
        .unwrap();
        vcx.run_until_parked(); // no panic, nothing to assert beyond arrival
    }
```

(`Diagnostics::last_published` may not exist under that name — use whatever `note_published` records; the assertion is only that the diagnostics entity noticed the dataset. If nothing observable is recorded, assert on `d.version()` having moved instead.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-app pricing_adapter a_local_publish a_price_event`
Expected: compile errors — `data_setup` arity, `local_datasets`, no `Price` arm.

- [ ] **Step 3: Implement**

`Cargo.toml` (geode-app): `geode-pricing.workspace = true`.

`main.rs`, beside the adapter registry (L130–137):
```rust
    // Every build has the mock (line-pricer spec §5.5); a vendor crate,
    // when one exists, registers itself here behind its feature gate.
    let mut pricers = geode_data::PricerRegistry::default();
    pricers.register(Arc::new(geode_pricing::MockPricer::new()));
```
Thread `pricers` through `build_shell_services(…, adapters, pricers, cx)` into `bridge::data_setup(&config, db, adapters, pricers)`.

`bridge.rs`:
- `DataSetup` gains `pub local_datasets: HashSet<String>,`.
- `data_setup(config, db_path, adapters, pricers: PricerRegistry)`: after the schema is built,
```rust
    let pricer_name = config
        .get("app", "pricing.adapter")
        .and_then(|v| v.as_str())
        .unwrap_or(geode_pricing::MOCK_PRICER)
        .to_string();
    let pricer = match pricers.get(&pricer_name) {
        Some(p) => PricerConfig::with(p),
        None => {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                layer: config.explain("app", "pricing.adapter"),
                file: None,
                message: format!(
                    "[pricing] adapter = \"{pricer_name}\" is not built into this binary (have: {}); every priced line will say so",
                    pricers.names().join(", ")
                ),
                path: Some("app.pricing.adapter".to_string()),
            });
            PricerConfig::missing(&pricer_name)
        }
    };
    let local_datasets: HashSet<String> = schema
        .datasets
        .iter()
        .filter(|d| d.local)
        .map(|d| d.name.clone())
        .collect();
```
  and the `DataServiceConfig` literal gains `pricer,`; the `DataSetup` literal gains `local_datasets,`.
- `Bridge` gains `pub local_datasets: Rc<HashSet<String>>,`; `start` fills it from `setup.local_datasets`.
- `attach`: capture `let local_datasets = Rc::clone(&bridge.local_datasets);` before the drain task, and in the match:
```rust
                    DataEvent::Price(outcome) => {
                        shell.update(cx, |s, cx| s.deliver(Delivery::Price(outcome), window, cx));
                    }
```
  and in the `Published` arm, wrap the frame half:
```rust
                        if local_datasets.contains(&dataset) {
                            // Line-pricer spec §7.2: a sheet autosave is
                            // not a data change for the workspace; a
                            // tile reading sheets follows the dataset
                            // through its own document request.
                        } else {
                            let frame = shell.read(cx).frame().clone();
                            let publish = geode_shell::frame::Publish { dataset, batch, books: books.len(), at: chrono::Utc::now() };
                            frame.update(cx, |f, cx| {
                                f.note_published(publish);
                                cx.notify();
                            });
                        }
```
  (the `diagnostics.update(.. note_published(&dataset) ..)` call stays above, unconditional — it borrows `dataset` before the move).
- Every `Bridge { … }` literal in the tests gains `local_datasets: Default::default(),`.

- [ ] **Step 4: Verify**

Run: `cargo test -p geode-app && cargo run -p geode-app -- --demo 1000` (start, confirm the log has no `pricer` warning, quit)
Expected: green; the demo starts.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates
git commit -m "app: PricerRegistry with the mock, [pricing] adapter, the Price arm and the local-publish gate (line-pricer §5.5, §7.2)"
```

---

### Task 9: `[pricing]` changes are a restart stripe

**Files:**
- Modify: `crates/geode-shell/src/shell/mod.rs` (fields at L845–848: add `pricing_baseline: Option<toml::Value>` after `datasets_baseline`; `ShellView::new` at L1520/L1575: seed it from `services.config.get("app", "pricing").cloned()`)
- Modify: `crates/geode-shell/src/shell/hot_reload.rs:329–336` (the `restart` list)
- Test: `crates/geode-shell/src/shell/tests/reload.rs` (beside the sources test at L1040)

**Interfaces:**
- Produces: a reload whose `[pricing]` table differs from the one the data engine started with adds `pricing` to the `… changed — restart to apply` message; reverting clears it (same rule as `sources`).

- [ ] **Step 1: Write the failing test** (in `tests/reload.rs`, copying the sources test's fixture set-up — the `events` recorder, `open_shell`, the `BUILTIN_KEYMAP` layer — exactly as that test does)

```rust
#[gpui::test]
fn a_pricing_change_requires_a_restart_and_a_revert_clears_it(cx: &mut gpui::TestAppContext) {
    // Same shell and `events` recorder as `a_sources_change_requires_a_restart…` above.
    let (shell, events, mut cx) = shell_with_event_recorder(cx);
    let mut with_pricing = Config::load(&ConfigSources {
        builtin: vec![
            LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap(),
            LayerDoc::builtin("app", "[pricing]\nadapter = \"vendor\"\n").unwrap(),
        ],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| s.apply_reload(std::mem::take(&mut with_pricing), cx));
    assert!(
        events.borrow().iter().any(|e| matches!(e, ShellEvent::RestartRequired(m) if m.contains("pricing"))),
        "{:?}",
        events.borrow()
    );
    events.borrow_mut().clear();
    let mut reverted = Config::load(&ConfigSources {
        builtin: vec![LayerDoc::builtin("keymap", BUILTIN_KEYMAP).unwrap()],
        ..ConfigSources::default()
    });
    shell.update(&mut cx, |s, cx| s.apply_reload(std::mem::take(&mut reverted), cx));
    assert!(
        !events.borrow().iter().any(|e| matches!(e, ShellEvent::RestartRequired(_))),
        "back at the baseline: {:?}",
        events.borrow()
    );
    assert!(shell.read_with(&cx, |s, _| s.restart_required.is_none()));
}
```

(`shell_with_event_recorder` stands for however the sources test obtains `shell` and `events`; inline that setup rather than inventing a helper if none exists.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-shell --features test-support a_pricing_change`
Expected: FAIL — no `RestartRequired` mentions `pricing`.

- [ ] **Step 3: Implement**

`shell/mod.rs`: the field, with a doc comment pointing at `sources_baseline`'s ("the `[pricing]` table the data engine's pricer was chosen from; a reload that changes it needs a restart, line-pricer spec §5.5"), seeded in `new` beside `sources_baseline`.

`hot_reload.rs`, replace the `restart` computation:
```rust
            let mut restart = [
                ("sources", &self.sources_baseline),
                ("datasets", &self.datasets_baseline),
            ]
            .into_iter()
            .filter(|(name, baseline)| !docs_equal(new_config.layered_docs(name), baseline))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
            if new_config.get("app", "pricing").cloned() != self.pricing_baseline {
                restart.push("pricing");
            }
```

- [ ] **Step 4: Verify**

Run: `cargo test -p geode-shell --features test-support reload`
Expected: green, including the existing sources/datasets restart tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add crates/geode-shell
git commit -m "shell: a [pricing] change is a restart-to-apply stripe (line-pricer §5.5)"
```

---

### Task 10: Charter amendment, docs, harness entries, spec as-built

**Files:**
- Modify: `docs/PHILOSOPHY.md` (§1, after "quickly calculate X" gets judged against it.")
- Modify: `CLAUDE.md` (status table: a row; load-bearing rules: a **Pricer (Part 1)** block; the harness count at L126 — set it to the real `grep -c '^run_mutation' scripts/mutation-check.sh` after the entries below are added; the `[log]` bullet's "six `geode::*` targets" becomes seven)
- Modify: `docs/phase-history.md` (a paragraph), `docs/superpowers/specs/2026-09-12-geode-modules-roadmap.md` (§5 G, §6 item 4: "Part 1 built 2026-09-19, see the line-pricer spec"), the line-pricer spec (a "§16 As built (Part 1)" section)
- Modify: `scripts/mutation-check.sh` (entries, before the anchors-only block at the end)

- [ ] **Step 1: The charter**

Insert the §2.1 paragraph from the spec verbatim into `docs/PHILOSOPHY.md` §1 after the "quickly calculate X" sentence, as its own paragraph headed **In-process calculation.**

- [ ] **Step 2: Harness entries** (each anchor is a verbatim substring of the code as written in this plan; re-read each file before anchoring and run `--anchors-only` after)

```sh
run_mutation "pricing: latest wins per key while queued" \
  crates/geode-data/src/pricing/worker.rs \
  '        if let Some(slot) = q.pending.get_mut(&key) {
            *slot = params;
        } else {' \
  '        if false {
        } else {' \
  geode-data latest_wins_per_key_while_queued

run_mutation "pricing: cancel stops a running batch at the line boundary" \
  crates/geode-data/src/pricing/worker.rs \
  '                if q.cancel_running || q.shutdown {
                    break;
                }' \
  '                if q.shutdown {
                    break;
                }' \
  geode-data cancel_drops_a_queued_batch_and_stops_a_running_one_at_the_line_boundary

run_mutation "pricing: a panic is contained per line" \
  crates/geode-data/src/pricing/worker.rs \
  '                        Err(payload) => {' \
  '                        Err(payload) => std::panic::resume_unwind(payload),
                        #[allow(unreachable_patterns)]
                        Err(payload) => {' \
  geode-data a_panicking_line_is_that_lines_error_and_the_next_line_prices

run_mutation "pricing: no pricer names the configured one" \
  crates/geode-data/src/pricing/mod.rs \
  '            format!("pricer \"{}\" is not built into this binary", self.name)' \
  '            "no pricer is configured".to_string()' \
  geode-data no_pricer_answers_every_line_with_the_configured_name

run_mutation "service: a publish to a non-local dataset is written" \
  crates/geode-data/src/service.rs \
  '            .is_some_and(|d| d.local);
        if !local {' \
  '            .is_some_and(|d| d.local);
        if false {' \
  geode-data a_publish_to_a_dataset_that_is_not_local_is_refused_unwritten

run_mutation "service: a local publish reports health for a source nobody declared" \
  crates/geode-data/src/service.rs \
  '                    if source == LOCAL_SOURCE {
                        let _ = sink(DataEvent::LoadEnded);
                        return delivered;
                    }' \
  '' \
  geode-data a_local_publish_emits_no_health_event_and_a_load_ended

run_mutation "core: local is accepted on a measure dataset" \
  crates/geode-core/src/schema/mod.rs \
  '    if ds.local && !ds.is_document() {' \
  '    if false {' \
  geode-core local_on_a_measure_dataset_is_an_error_and_cleared

run_mutation "core: a source may feed a local dataset" \
  crates/geode-core/src/source_config.rs \
  '                Some(d) if schema.dataset(d).is_some_and(|ds| ds.local) => {' \
  '                Some(d) if false && schema.dataset(d).is_some_and(|ds| ds.local) => {' \
  geode-core a_source_naming_a_local_dataset_is_refused

run_mutation "bridge: a local publish bumps the frame" \
  crates/geode-app/src/bridge.rs \
  '                        if local_datasets.contains(&dataset) {' \
  '                        if false {' \
  geode-app a_local_publish_does_not_bump_the_frames_data_version_but_a_normal_one_does

run_mutation "shell: a Price delivery is routed to the wrong key" \
  crates/geode-shell/src/module.rs \
  '            Delivery::Price(outcome) => outcome.key,' \
  '            Delivery::Price(outcome) => QueryKey(outcome.key.0 + 1),' \
  geode-shell a_price_delivery_is_routed_by_key_like_a_query

run_mutation "shell: a pricing change needs no restart" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '            if new_config.get("app", "pricing").cloned() != self.pricing_baseline {' \
  '            if false {' \
  geode-shell a_pricing_change_requires_a_restart_and_a_revert_clears_it
```

The contained-panic entry's replacement must compile: if `resume_unwind` followed by a second `Err` arm is refused by the compiler for the payload type, use the simpler mutation `'geode_core::panic::contained(|| pricer.price(&line.request))'` → `'pricer.price(&line.request)'` and name the test `a_panicking_line_is_that_lines_error_and_the_next_line_prices` — that mutation only survives if the test does not check the crash-hook side, so add to the test an assertion through `geode_core::panic::is_contained()` observed from a panic hook installed for the test, or keep the first form. Prefer the first form; try it.

Run: `zsh scripts/mutation-check.sh --anchors-only && zsh scripts/mutation-check.sh "pricing:" && zsh scripts/mutation-check.sh "service: a publish" && zsh scripts/mutation-check.sh "service: a local" && zsh scripts/mutation-check.sh "core: local" && zsh scripts/mutation-check.sh "core: a source may" && zsh scripts/mutation-check.sh "bridge: a local" && zsh scripts/mutation-check.sh "shell: a Price" && zsh scripts/mutation-check.sh "shell: a pricing"`
Expected: every entry `caught`, no ANCHOR/AMBIG.

- [ ] **Step 3: CLAUDE.md, phase history, roadmap, spec as-built**

CLAUDE.md status row:

| Line pricer Part 1 (2026-09-19) | `geode_core::pricing` (vocabulary, `Pricer`), `geode-pricing` (`MockPricer`), the pricing worker (`Request::Price` → `DataEvent::Price` → `Delivery::Price`), `Request::Publish` for `local = true` datasets, `[pricing] adapter`, `geode::pricing`. Parts 2–4 (core, tile, storage) next. | `2026-09-19-…line-pricer` |

CLAUDE.md rules block **Pricer (Part 1)**:
- The vocabulary and `Pricer` trait are `geode_core::pricing`; `geode-pricing` is implementations only (a leaf; the shell never depends on it). A calculation lives in the binary only as such a leaf, behind the request/outcome door — PHILOSOPHY §1 "In-process calculation".
- The pricing worker is one thread beside the pool: latest wins per key, `Cancel { key }` stops at the next line boundary and delivers what was priced, `catch_unwind` + `contained` per line. `pricer: None` errors every line with `PricerConfig::missing_reason`.
- `Request::Publish` is refused unwritten for a non-`local` dataset; a local publish is source `LOCAL_SOURCE`, emits `Published` + `LoadEnded` and never `Health`; the bridge skips `Frame::note_published` for a local dataset (`Bridge.local_datasets`). A `[sources]` entry naming a local dataset is an error.
- `[pricing]` changes are a restart stripe (`pricing_baseline`), like `sources`.

`docs/phase-history.md`: one paragraph naming the ten tasks, the deviation (vocabulary in core), and the harness entries.

The roadmap: §5 G and §6 item 4 gain "Part 1 (seam and data tier) built 2026-09-19; the local publish is the roadmap §3 'local' shape".

The spec: append

```markdown
## 16. As built (Part 1, 2026-09-19)

- The vocabulary, the `Pricer` trait and `PriceParams`/`PriceOutcome`/`LocalPublish` live in `geode_core::pricing`, not `geode-pricing`: the shell names `PriceOutcome` in `Delivery` and must not depend on a calculation crate (§2.1). `geode-pricing` holds `MockPricer` and is where vendor crates go. §4 and §5.1 read with that substitution.
- `PricerConfig` (`geode_data::pricing`) carries the configured name beside the optional pricer so a missing one names itself; an empty name says "no pricer is configured".
- The worker's queue is bounded by distinct keys (`PRICE_BOUND` = 64); a replacement for a queued key always fits.
- A local publish's ingest-sink arms emit no `Health` (no declared source has a lane); a failed one is an error `Diagnostics` event plus `LoadEnded`.
- Retention for local datasets is NOT wired in Part 1 (nor is the sweeper for anything else); §7.2's "keep_generations = 200" is Part 4's.
- Unverified on a real window: nothing in Part 1 paints.
```

- [ ] **Step 4: Full verification**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo check -p geode-shell --features test-support --all-targets && cargo bench --workspace --no-run && zsh scripts/mutation-check.sh --anchors-only
```
Expected: all green; `--anchors-only` exits 0.

- [ ] **Step 5: Commit**

```bash
git add docs CLAUDE.md scripts/mutation-check.sh
git commit -m "docs+harness: line pricer Part 1 — charter amendment, rules, eleven entries, spec as-built"
```

---

## Self-review notes

- **Spec coverage (Part 1 scope):** §2.1 (Task 10), §4 crates (Tasks 1, 2), §5.1 vocabulary (1), §5.2 mock (2), §5.3 worker/requests/events/local publish (5, 6), §5.4 delivery (7), §5.5 registration/config/restart (8, 9), §7.2 `local` flag and bridge gate (3, 8), §10.2 target (4), §12 seam and data-tier tests (1–9), §12 harness (10), §13 docs (10). Deferred to later parts as the spec says: the `pricer_sheets` dataset, retention, the `SheetStore`, every tile-side test.
- **Type consistency:** `PricerConfig::{with, missing, missing_reason}` (Task 5) used in Tasks 6 and 8; `PricingWorker::{spawn, request, cancel, shutdown}` (5) used in 6; `crate::pricing::worker::tests::{FakePricer, params}` (5) used in 6 — the tests module is `pub(crate)` for that reason; `local_dataset`/`sheet_rows` (6) used in 6's handle test; `Recorded::Priced` (7) used in 7's test; `Bridge.local_datasets: Rc<HashSet<String>>` (8) in both bridge tests; `pricing_baseline: Option<toml::Value>` (9).
- **Names the executor must check against the file, not this plan:** the schema tests' `parse` helper (Task 3), `source_config.rs`'s two fixtures (3), `cvi_doc`'s signature and `CatalogSnapshot`'s field names (6), `Diagnostics`' record of a publish (8), the reload test's fixture set-up (9). Each is called out inline.
- **Placeholders:** none; the one conditional instruction (Task 10's contained-panic entry) gives both forms in full.
