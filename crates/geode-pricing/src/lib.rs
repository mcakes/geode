//! Implementations of `geode_core::pricing::Pricer`, and the stand-in
//! `geode_core::vol::VolModel`.
//!
//! This calculation leaf depends on `geode-core` and exposes implementations
//! through its `Pricer` and `VolModel` traits. [`MockPricer`] supplies
//! deterministic demo and test results; [`DemoVolModel`] is the smooth
//! stand-in vol-surface evaluator `[vol] model = "demo"` selects. `geode-app`
//! registers them in `geode-data`'s `PricerRegistry` and `VolModelRegistry`,
//! respectively. An unknown `[pricing] adapter` produces per-line errors
//! without preventing startup; an unknown vol model refuses each vol batch.

pub mod black;
pub mod demo_vol;
pub mod spline;

pub use demo_vol::{DEMO_VOL_MODEL, DemoVolModel};

use geode_core::pricing::{
    Currency, Instrument, MarketOverrides, Measure, OptionKind, PriceRequest, PriceResult, Pricer,
    PricingError, Strike,
};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::Duration;

/// The name `[pricing] adapter` uses for the mock, and its default.
pub const MOCK_PRICER: &str = "mock";

/// An instrument on this underlying is refused, making the failed-line path
/// testable without a real library.
pub const REFUSED_UNDERLYING: &str = "FAIL";

/// Deterministic, cheap, and NOT a model. Seeded analytics come from a
/// hash of the instrument's fields; the shift terms are shaped so a spot
/// shift moves the price in delta's sign and a vol shift in vega's, so
/// a trader bumping shifts sees plausible motion. The 14 bumped measures
/// derive from those analytics (a 1%/2%/5% spot bump for the deltas and
/// gammas, one vol point for the vegas, ten basis points for the rhos),
/// so a call's deltas and rhos are positive and a put's negative, gammas
/// and vegas are positive, and theta is negative. Each measure's `_usd`
/// twin is the local value at a rate fixed per underlying, in a currency
/// fixed per underlying.
#[derive(Debug)]
pub struct MockPricer {
    delay: Duration,
    /// The last `set_overrides`, as the real library's `PricingDataSource` would hold it.
    overrides: Mutex<MarketOverrides>,
}

impl MockPricer {
    pub fn new() -> MockPricer {
        MockPricer {
            delay: Duration::ZERO,
            overrides: Mutex::new(MarketOverrides::default()),
        }
    }

    /// Sleeps `delay` per call: the test hook for the tile's slow path.
    pub fn with_delay(delay: Duration) -> MockPricer {
        MockPricer {
            delay,
            overrides: Mutex::new(MarketOverrides::default()),
        }
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

/// A deterministic currency and USD rate per underlying name.
fn currency_of(underlying: &str) -> (Currency, f64) {
    const TABLE: [(&str, f64); 4] = [("USD", 1.0), ("EUR", 1.08), ("JPY", 0.0067), ("HKD", 0.128)];
    let mut h = std::collections::hash_map::DefaultHasher::new();
    underlying.hash(&mut h);
    let (code, rate) = TABLE[(h.finish() % 4) as usize];
    (
        Currency::parse(code).expect("table codes are well-formed"),
        rate,
    )
}

impl Pricer for MockPricer {
    fn name(&self) -> &str {
        MOCK_PRICER
    }

    fn set_overrides(&self, overrides: &MarketOverrides) -> Result<(), PricingError> {
        if overrides
            .spot
            .values()
            .any(|s| !(s.is_finite() && *s > 0.0))
        {
            return Err(PricingError(
                "spot override must be a positive finite number".to_string(),
            ));
        }
        *self.overrides.lock().unwrap_or_else(|e| e.into_inner()) = overrides.clone();
        Ok(())
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
        let mut price =
            base + delta * base * req.shifts.spot_pct / 100.0 + vega * req.shifts.vol_pts;
        // The mock's reference spot: the absolute strike, or 100 for a
        // percent strike. A spot override moves the price from it in
        // delta's sign, so a higher spot raises a call and lowers a put.
        // Not a model; plausible motion for a trader typing `:spot`.
        let reference = match req.instrument.strike() {
            Strike::Absolute(k) => k,
            Strike::Percent(_) => 100.0,
        };
        let overrides = self.overrides.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(spot) = overrides.spot.get(req.instrument.underlying()) {
            price += delta * (spot - reference);
        }
        // Bumped measures from the analytics: a 1%/2%/5% spot bump for
        // delta and gamma, one vol point for vega, ten basis points for
        // rho. Skew and normalized vega are fixed multiples of vega.
        // Currency and FX follow the underlying's hash so one underlying
        // always prices in one currency.
        let bump = |pct: f64| reference * pct;
        let mut local = [0.0; Measure::COUNT];
        local[Measure::Npv.index()] = price;
        local[Measure::Delta01.index()] = delta * bump(0.01);
        local[Measure::Delta02.index()] = delta * bump(0.02);
        local[Measure::Delta05.index()] = delta * bump(0.05);
        local[Measure::Gamma01.index()] = 0.5 * gamma * bump(0.01).powi(2);
        local[Measure::Gamma02.index()] = 0.5 * gamma * bump(0.02).powi(2);
        local[Measure::Gamma05.index()] = 0.5 * gamma * bump(0.05).powi(2);
        local[Measure::Vega01.index()] = vega;
        local[Measure::NormalizedVega01.index()] = vega * 0.5;
        local[Measure::Skew01.index()] = sign * vega * 0.1;
        local[Measure::Rho010.index()] = rho * 0.10;
        local[Measure::RhoRfr010.index()] = rho * 0.06;
        local[Measure::RhoOis010.index()] = rho * 0.04;
        local[Measure::CleanThetaBusinessDay.index()] = theta;
        let (currency, rate) = currency_of(req.instrument.underlying());
        let mut usd = local;
        for v in &mut usd {
            *v *= rate;
        }
        Ok(PriceResult {
            currency,
            local,
            usd,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::pricing::{
        Barrier, BarrierKind, Expiry, Instrument, MarketOverrides, OptionKind, PriceRequest,
        Pricer, Shifts, Strike, Vanilla,
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
        PriceRequest {
            instrument,
            shifts: Shifts { spot_pct, vol_pts },
        }
    }

    /// A percent-strike vanilla at a tenor, for the measure tests.
    fn pct(underlying: &str, tenor: &str, strike_pct: f64, kind: OptionKind) -> Instrument {
        Instrument::Vanilla(Vanilla {
            underlying: underlying.into(),
            expiry: Expiry::Tenor(tenor.into()),
            strike: Strike::Percent(strike_pct),
            kind,
        })
    }

    #[test]
    fn bumped_measures_share_the_analytic_signs_and_usd_is_converted() {
        let p = MockPricer::new();
        let r = p
            .price(&req(pct("SPX", "3m", 100.0, OptionKind::Call), 0.0, 0.0))
            .unwrap();
        assert!(r.get(Measure::Npv, false) > 0.0);
        assert!(
            r.get(Measure::Delta01, false) > 0.0,
            "a call's delta01 is positive"
        );
        assert!(
            r.get(Measure::Delta02, false) > r.get(Measure::Delta01, false),
            "a bigger bump is a bigger number"
        );
        assert!(r.get(Measure::Gamma01, false) > 0.0 && r.get(Measure::Vega01, false) > 0.0);
        assert!(r.get(Measure::CleanThetaBusinessDay, false) < 0.0);
        assert!(r.get(Measure::Rho010, false) > 0.0);
        let rate = r.get(Measure::Npv, true) / r.get(Measure::Npv, false);
        assert!(rate.is_finite() && rate > 0.0);
        for m in Measure::ALL {
            let (l, u) = (r.get(m, false), r.get(m, true));
            assert!(
                (u - l * rate).abs() < 1e-9,
                "{m:?}: usd {u} is not local {l} × {rate}"
            );
        }
    }

    #[test]
    fn currency_is_deterministic_per_underlying_and_well_formed() {
        let p = MockPricer::new();
        let a = p
            .price(&req(pct("SPX", "3m", 100.0, OptionKind::Call), 0.0, 0.0))
            .unwrap();
        let b = p
            .price(&req(pct("SPX", "6m", 90.0, OptionKind::Put), 0.0, 0.0))
            .unwrap();
        assert_eq!(a.currency, b.currency, "one underlying, one currency");
        assert!(Currency::parse(a.currency.as_str()).is_some());
    }

    #[test]
    fn the_same_request_answers_the_same_numbers() {
        let p = MockPricer::new();
        let a = p
            .price(&req(vanilla("SPX", OptionKind::Call, 5000.0), 0.0, 0.0))
            .unwrap();
        let b = p
            .price(&req(vanilla("SPX", OptionKind::Call, 5000.0), 0.0, 0.0))
            .unwrap();
        assert_eq!(a, b);
        let c = p
            .price(&req(vanilla("SPX", OptionKind::Call, 5100.0), 0.0, 0.0))
            .unwrap();
        assert_ne!(
            a.get(Measure::Npv, false),
            c.get(Measure::Npv, false),
            "a different strike is a different number"
        );
    }

    #[test]
    fn a_call_has_positive_delta_and_a_put_negative_and_the_other_greeks_keep_their_signs() {
        let p = MockPricer::new();
        for (kind, sign) in [(OptionKind::Call, 1.0), (OptionKind::Put, -1.0)] {
            for strike in [90.0, 100.0, 110.0, 4000.0, 5000.0] {
                let r = p
                    .price(&req(vanilla("NDX", kind, strike), 0.0, 0.0))
                    .unwrap();
                assert!(r.get(Measure::Npv, false) > 0.0);
                assert!(
                    r.get(Measure::Delta01, false) * sign > 0.0,
                    "{kind:?} delta01 {}",
                    r.get(Measure::Delta01, false)
                );
                assert!(
                    r.get(Measure::Gamma01, false) > 0.0
                        && r.get(Measure::Vega01, false) > 0.0
                        && r.get(Measure::CleanThetaBusinessDay, false) < 0.0
                );
                assert!(r.get(Measure::Rho010, false) * sign > 0.0);
            }
        }
    }

    #[test]
    fn a_spot_shift_moves_the_price_in_deltas_sign_and_a_vol_shift_in_vegas() {
        let p = MockPricer::new();
        for kind in [OptionKind::Call, OptionKind::Put] {
            let base = p
                .price(&req(vanilla("SPX", kind, 5000.0), 0.0, 0.0))
                .unwrap();
            let up = p
                .price(&req(vanilla("SPX", kind, 5000.0), 5.0, 0.0))
                .unwrap();
            assert_eq!(
                (up.get(Measure::Npv, false) - base.get(Measure::Npv, false)).signum(),
                base.get(Measure::Delta01, false).signum(),
                "{kind:?}"
            );
            let volup = p
                .price(&req(vanilla("SPX", kind, 5000.0), 0.0, 2.0))
                .unwrap();
            assert!(
                volup.get(Measure::Npv, false) > base.get(Measure::Npv, false),
                "{kind:?}: vega is positive so vol up is price up"
            );
        }
    }

    #[test]
    fn a_barrier_prices_and_differs_from_its_vanilla() {
        let p = MockPricer::new();
        let v = vanilla("SPX", OptionKind::Put, 5000.0);
        let b = Instrument::Barrier(Barrier {
            vanilla: match v.clone() {
                Instrument::Vanilla(v) => v,
                _ => unreachable!(),
            },
            level: 4200.0,
            barrier: BarrierKind::DownOut,
        });
        let rv = p.price(&req(v, 0.0, 0.0)).unwrap();
        let rb = p.price(&req(b, 0.0, 0.0)).unwrap();
        assert_ne!(rv.get(Measure::Npv, false), rb.get(Measure::Npv, false));
        assert!(
            rb.get(Measure::Delta01, false) < 0.0,
            "a put barrier is still a put"
        );
    }

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
            assert_eq!(
                (up.get(Measure::Npv, false) - base.get(Measure::Npv, false)).signum(),
                base.get(Measure::Delta01, false).signum(),
                "{kind:?}"
            );
            p.set_overrides(&overrides(&[("SPX", 4800.0)])).unwrap();
            let down = p.price(&r).unwrap();
            assert_eq!(
                (down.get(Measure::Npv, false) - base.get(Measure::Npv, false)).signum(),
                -base.get(Measure::Delta01, false).signum(),
                "{kind:?}"
            );
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
        assert_eq!(
            p.price(&r).unwrap(),
            base,
            "an override AT the reference moves nothing"
        );
        p.set_overrides(&overrides(&[("SPX", 101.0)])).unwrap();
        assert!(p.price(&r).unwrap().get(Measure::Npv, false) > base.get(Measure::Npv, false));
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
        assert_eq!(
            p.price(&r).unwrap(),
            with,
            "a refused set leaves the previous overrides in place"
        );
    }

    #[test]
    fn the_refused_underlying_is_an_error_and_the_delay_is_honoured() {
        let p = MockPricer::new();
        let err = p
            .price(&req(
                vanilla(REFUSED_UNDERLYING, OptionKind::Call, 100.0),
                0.0,
                0.0,
            ))
            .unwrap_err();
        assert_eq!(err.0, "refused by the mock");
        let slow = MockPricer::with_delay(Duration::from_millis(30));
        let t = Instant::now();
        slow.price(&req(vanilla("SPX", OptionKind::Call, 100.0), 0.0, 0.0))
            .unwrap();
        assert!(t.elapsed() >= Duration::from_millis(30));
        assert_eq!(slow.name(), MOCK_PRICER);
    }
}
