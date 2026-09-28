//! Implementations of `geode_core::pricing::Pricer`, and the stand-in
//! `geode_core::vol::VolModel`.
//!
//! This calculation leaf depends on `geode-core` and exposes implementations
//! through its `Pricer` and `VolModel` traits. [`MockPricer`] supplies
//! deterministic demo and test results; [`DemoVolModel`] is the smooth
//! stand-in vol-surface evaluator `[vol] model = "demo"` selects. `geode-app` registers available implementations in its
//! `PricerRegistry`; an unknown `[pricing] adapter` produces per-line errors
//! without preventing startup.

pub mod black;
pub mod demo_vol;
pub mod spline;

pub use demo_vol::{DEMO_VOL_MODEL, DemoVolModel};

use geode_core::pricing::{
    Instrument, MarketOverrides, OptionKind, PriceRequest, PriceResult, Pricer, PricingError,
    Strike,
};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::Duration;

/// The name `[pricing] adapter` uses for the mock, and its default.
pub const MOCK_PRICER: &str = "mock";

/// An instrument on this underlying is refused, making the failed-line path
/// testable without a real library.
pub const REFUSED_UNDERLYING: &str = "FAIL";

/// Deterministic, cheap, and NOT a model. The numbers come from a hash
/// of the instrument's fields; the shift terms are shaped so a spot
/// shift moves the price in delta's sign and a vol shift in vega's, so
/// a trader bumping shifts sees plausible motion. A call's delta and rho
/// are positive and a put's negative; gamma and vega are positive;
/// theta is negative.
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
        Ok(PriceResult {
            price,
            delta,
            gamma,
            vega,
            theta,
            rho,
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
        assert_ne!(a.price, c.price, "a different strike is a different number");
    }

    #[test]
    fn a_call_has_positive_delta_and_a_put_negative_and_the_other_greeks_keep_their_signs() {
        let p = MockPricer::new();
        for (kind, sign) in [(OptionKind::Call, 1.0), (OptionKind::Put, -1.0)] {
            for strike in [90.0, 100.0, 110.0, 4000.0, 5000.0] {
                let r = p
                    .price(&req(vanilla("NDX", kind, strike), 0.0, 0.0))
                    .unwrap();
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
            let base = p
                .price(&req(vanilla("SPX", kind, 5000.0), 0.0, 0.0))
                .unwrap();
            let up = p
                .price(&req(vanilla("SPX", kind, 5000.0), 5.0, 0.0))
                .unwrap();
            assert_eq!(
                (up.price - base.price).signum(),
                base.delta.signum(),
                "{kind:?}"
            );
            let volup = p
                .price(&req(vanilla("SPX", kind, 5000.0), 0.0, 2.0))
                .unwrap();
            assert!(
                volup.price > base.price,
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
        assert_ne!(rv.price, rb.price);
        assert!(rb.delta < 0.0, "a put barrier is still a put");
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
                (up.price - base.price).signum(),
                base.delta.signum(),
                "{kind:?}"
            );
            p.set_overrides(&overrides(&[("SPX", 4800.0)])).unwrap();
            let down = p.price(&r).unwrap();
            assert_eq!(
                (down.price - base.price).signum(),
                -base.delta.signum(),
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
