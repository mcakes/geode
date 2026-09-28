//! Shared instrument, request, and result types for pricing integrations.
//! The pricing library resolves percent strikes against spot and tenors against
//! its calendar; the app passes `Strike::Percent` and `Expiry::Tenor` through.
//!
//! These types let the shell route `PriceOutcome` without depending on a pricing
//! implementation. Implementations live in `geode-pricing`.

use crate::document::DocumentRows;
use crate::query::QueryKey;
use chrono::NaiveDate;
use std::collections::BTreeMap;
use std::time::Instant;

/// The `source` a local publish (`Request::Publish`) is stamped with.
/// No `[sources]` entry ever declares it, so the ingest sink reports no
/// health for it.
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
            return Err(format!(
                "tenor '{text}': the unit must be one of d, w, m, y"
            ));
        }
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!(
                "tenor '{text}': expected digits then one unit letter"
            ));
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

/// Supported option definitions. Both variants contain one underlying;
/// a barrier wraps a vanilla option with a level and barrier kind.
#[derive(Debug, Clone, PartialEq)]
pub enum Instrument {
    Vanilla(Vanilla),
    Barrier(Barrier),
}

impl Instrument {
    /// The vanilla every variant is built on (a barrier wraps one).
    pub fn vanilla(&self) -> &Vanilla {
        match self {
            Instrument::Vanilla(v) => v,
            Instrument::Barrier(b) => &b.vanilla,
        }
    }

    pub fn underlying(&self) -> &str {
        &self.vanilla().underlying
    }

    pub fn expiry(&self) -> &Expiry {
        &self.vanilla().expiry
    }

    pub fn kind(&self) -> OptionKind {
        self.vanilla().kind
    }

    pub fn strike(&self) -> Strike {
        self.vanilla().strike
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
/// reprice it.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceRequest {
    pub instrument: Instrument,
    pub shifts: Shifts,
}

/// ISO 4217 code: three uppercase ASCII letters. `Copy` so a
/// [`PriceResult`] stays `Copy` (the sheet copies results into records
/// and folds them per leg).
///
/// [`Currency::MIXED`] is the one value that is not a code: it marks a
/// fold over results that priced in differing currencies, whose local
/// arrays are then sums of unlike units. It never comes from a pricer
/// (`parse` accepts letters only) and displays as `—`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Currency([u8; 3]);

impl Currency {
    pub const USD: Currency = Currency(*b"USD");

    /// A fold over differing currencies. Not a code: `parse` can never
    /// produce it, so a pricer cannot report it, and a local-currency
    /// figure carrying it is a gap, not a number, wherever it is painted
    /// or summed. The `_usd` arrays under it are still comparable.
    pub const MIXED: Currency = Currency(*b"???");

    /// `None` unless exactly three uppercase ASCII letters.
    pub fn parse(s: &str) -> Option<Currency> {
        let b = s.as_bytes();
        if b.len() == 3 && b.iter().all(|c| c.is_ascii_uppercase()) {
            Some(Currency([b[0], b[1], b[2]]))
        } else {
            None
        }
    }

    pub fn is_mixed(&self) -> bool {
        *self == Currency::MIXED
    }

    /// The code, or `—` for [`Currency::MIXED`].
    pub fn as_str(&self) -> &str {
        if self.is_mixed() {
            return "—";
        }
        std::str::from_utf8(&self.0).expect("constructed from ASCII letters")
    }
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The result vocabulary: risk_snapshot's bumped measures, under its
/// names. Each has a `_usd` twin the pricer also supplies, already
/// converted. The variant order is the column order everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Measure {
    Npv,
    Delta01,
    Delta02,
    Delta05,
    Gamma01,
    Gamma02,
    Gamma05,
    Vega01,
    NormalizedVega01,
    Skew01,
    Rho010,
    RhoRfr010,
    RhoOis010,
    CleanThetaBusinessDay,
}

impl Measure {
    pub const COUNT: usize = 14;
    pub const ALL: [Measure; Measure::COUNT] = [
        Measure::Npv,
        Measure::Delta01,
        Measure::Delta02,
        Measure::Delta05,
        Measure::Gamma01,
        Measure::Gamma02,
        Measure::Gamma05,
        Measure::Vega01,
        Measure::NormalizedVega01,
        Measure::Skew01,
        Measure::Rho010,
        Measure::RhoRfr010,
        Measure::RhoOis010,
        Measure::CleanThetaBusinessDay,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Measure::Npv => "npv",
            Measure::Delta01 => "delta01",
            Measure::Delta02 => "delta02",
            Measure::Delta05 => "delta05",
            Measure::Gamma01 => "gamma01",
            Measure::Gamma02 => "gamma02",
            Measure::Gamma05 => "gamma05",
            Measure::Vega01 => "vega01",
            Measure::NormalizedVega01 => "normalized_vega01",
            Measure::Skew01 => "skew01",
            Measure::Rho010 => "rho010",
            Measure::RhoRfr010 => "rho_rfr010",
            Measure::RhoOis010 => "rho_ois010",
            Measure::CleanThetaBusinessDay => "clean_theta_business_day",
        }
    }

    pub const fn usd_name(self) -> &'static str {
        match self {
            Measure::Npv => "npv_usd",
            Measure::Delta01 => "delta01_usd",
            Measure::Delta02 => "delta02_usd",
            Measure::Delta05 => "delta05_usd",
            Measure::Gamma01 => "gamma01_usd",
            Measure::Gamma02 => "gamma02_usd",
            Measure::Gamma05 => "gamma05_usd",
            Measure::Vega01 => "vega01_usd",
            Measure::NormalizedVega01 => "normalized_vega01_usd",
            Measure::Skew01 => "skew01_usd",
            Measure::Rho010 => "rho010_usd",
            Measure::RhoRfr010 => "rho_rfr010_usd",
            Measure::RhoOis010 => "rho_ois010_usd",
            Measure::CleanThetaBusinessDay => "clean_theta_business_day_usd",
        }
    }

    /// `(measure, usd)` for either spelling; `None` for any other name.
    pub fn from_name(name: &str) -> Option<(Measure, bool)> {
        Measure::ALL.iter().find_map(|m| {
            if m.name() == name {
                Some((*m, false))
            } else if m.usd_name() == name {
                Some((*m, true))
            } else {
                None
            }
        })
    }

    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Per unit of the instrument: every measure in the line's currency and
/// in USD, both scaled and converted by the pricer. `Copy` is
/// load-bearing (see [`Currency`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceResult {
    pub currency: Currency,
    pub local: [f64; Measure::COUNT],
    pub usd: [f64; Measure::COUNT],
}

impl PriceResult {
    pub const fn zero(currency: Currency) -> PriceResult {
        PriceResult {
            currency,
            local: [0.0; Measure::COUNT],
            usd: [0.0; Measure::COUNT],
        }
    }

    pub fn get(&self, m: Measure, usd: bool) -> f64 {
        if usd {
            self.usd[m.index()]
        } else {
            self.local[m.index()]
        }
    }

    pub fn set(&mut self, m: Measure, usd: bool, v: f64) {
        if usd {
            self.usd[m.index()] = v
        } else {
            self.local[m.index()] = v
        }
    }

    /// `self += q × other` over both arrays; the currency stays `self`'s.
    pub fn add_scaled(&mut self, q: f64, other: &PriceResult) {
        for i in 0..Measure::COUNT {
            self.local[i] += q * other.local[i];
            self.usd[i] += q * other.usd[i];
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingError(pub String);

/// Spot-level overrides keyed by underlying. The pricing library interprets
/// these values; document-based market-data overrides are not represented here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MarketOverrides {
    pub spot: BTreeMap<String, f64>,
}

/// One synchronous pricing call per instrument.
/// The library fetches or is handed its own market data; the app hands
/// it a definition and shifts and shows what comes back.
pub trait Pricer: Send + Sync {
    fn name(&self) -> &str;
    /// Replace the overridable market data for every `price` call that follows,
    /// until the next call here. The worker calls it once per batch, so every
    /// line in that batch sees the same overrides.
    fn set_overrides(&self, overrides: &MarketOverrides) -> Result<(), PricingError>;
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

/// A batch of lines to price, addressed by request key and submission tag.
/// The worker applies one override set before pricing the batch's lines.
#[derive(Debug, Clone)]
pub struct PriceParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    /// The sheet's overrides for this batch; the worker sets them once
    /// before the first line.
    pub overrides: MarketOverrides,
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
/// of a `local = true` dataset.
#[derive(Debug, Clone)]
pub struct LocalPublish {
    pub dataset: String,
    pub rows: DocumentRows,
}

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
        let a = PriceRequest {
            instrument: spx_call(),
            shifts: Shifts::default(),
        };
        let b = PriceRequest {
            instrument: spx_call(),
            shifts: Shifts::default(),
        };
        assert_eq!(a, b);
        let c = PriceRequest {
            instrument: spx_call(),
            shifts: Shifts {
                spot_pct: 1.0,
                vol_pts: 0.0,
            },
        };
        assert_ne!(a, c);
    }

    #[test]
    fn an_instrument_answers_its_underlying_and_kind_through_a_barrier() {
        let b = Instrument::Barrier(Barrier {
            vanilla: match spx_call() {
                Instrument::Vanilla(v) => Vanilla {
                    kind: OptionKind::Put,
                    ..v
                },
                _ => unreachable!(),
            },
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

    #[test]
    fn overrides_default_to_none_and_compare_by_value() {
        let a = MarketOverrides::default();
        assert!(a.spot.is_empty());
        let mut b = MarketOverrides::default();
        b.spot.insert("SPX".into(), 5000.0);
        assert_ne!(a, b);
        assert_eq!(b.clone(), b);
    }

    #[test]
    fn an_instrument_answers_its_expiry_and_vanilla_through_a_barrier() {
        let call = spx_call();
        let b = Instrument::Barrier(Barrier {
            vanilla: match &call {
                Instrument::Vanilla(v) => v.clone(),
                _ => unreachable!(),
            },
            level: 4200.0,
            barrier: BarrierKind::DownOut,
        });
        assert_eq!(
            b.expiry(),
            &Expiry::Date(NaiveDate::from_ymd_opt(2026, 12, 18).unwrap())
        );
        assert_eq!(b.vanilla().strike, Strike::Absolute(5000.0));
        assert_eq!(call.expiry(), b.expiry());
    }

    #[test]
    fn every_measure_has_a_distinct_name_and_usd_twin() {
        let mut names: Vec<&str> = Measure::ALL.iter().map(|m| m.name()).collect();
        names.extend(Measure::ALL.iter().map(|m| m.usd_name()));
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 28, "28 distinct names: {names:?}");
        for m in Measure::ALL {
            assert_eq!(m.usd_name(), format!("{}_usd", m.name()));
            assert_eq!(Measure::from_name(m.name()), Some((m, false)));
            assert_eq!(Measure::from_name(m.usd_name()), Some((m, true)));
            assert_eq!(Measure::ALL[m.index()], m);
        }
        assert_eq!(
            Measure::from_name("delta"),
            None,
            "the analytic names are gone"
        );
        assert_eq!(Measure::Npv.name(), "npv");
        assert_eq!(
            Measure::CleanThetaBusinessDay.usd_name(),
            "clean_theta_business_day_usd"
        );
    }

    #[test]
    fn usd_twin_reads_the_usd_array() {
        let mut r = PriceResult::zero(Currency::USD);
        r.set(Measure::Delta01, false, 2.0);
        r.set(Measure::Delta01, true, 3.0);
        assert_eq!(r.get(Measure::Delta01, false), 2.0);
        assert_eq!(r.get(Measure::Delta01, true), 3.0);
        assert_eq!(r.get(Measure::Delta02, true), 0.0);
    }

    #[test]
    fn add_scaled_sums_both_arrays_and_keeps_its_own_currency() {
        let mut sum = PriceResult::zero(Currency::parse("EUR").unwrap());
        let mut leg = PriceResult::zero(Currency::USD);
        leg.set(Measure::Npv, false, 10.0);
        leg.set(Measure::Npv, true, 11.0);
        sum.add_scaled(-2.0, &leg);
        sum.add_scaled(1.0, &leg);
        assert_eq!(sum.get(Measure::Npv, false), -10.0);
        assert_eq!(sum.get(Measure::Npv, true), -11.0);
        assert_eq!(sum.currency.as_str(), "EUR");
    }

    #[test]
    fn a_malformed_currency_is_a_pricing_error_not_a_panic() {
        // The seam's contract: a library that cannot spell its currency
        // answers an error; `Currency::parse` is how it checks.
        let attempt = |code: &str| {
            Currency::parse(code).ok_or_else(|| PricingError(format!("bad currency '{code}'")))
        };
        assert!(attempt("usd").is_err());
        assert!(attempt("USD").is_ok());
    }

    #[test]
    fn a_currency_is_three_uppercase_ascii_letters() {
        assert_eq!(Currency::parse("USD"), Some(Currency::USD));
        assert_eq!(Currency::parse("HKD").unwrap().to_string(), "HKD");
        for bad in ["usd", "US", "USDD", "U$D", "ÜSD"] {
            assert_eq!(Currency::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn mixed_is_no_code_and_displays_as_a_gap() {
        assert!(Currency::MIXED.is_mixed());
        assert!(!Currency::USD.is_mixed());
        assert_eq!(Currency::MIXED.to_string(), "—");
        assert_eq!(Currency::MIXED.as_str(), "—");
        // No spelling reaches the sentinel through the pricer's seam.
        assert_eq!(Currency::parse("???"), None);
        assert_eq!(Currency::parse("—"), None);
    }
}
