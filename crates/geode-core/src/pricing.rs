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
}
