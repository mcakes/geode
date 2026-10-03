//! Undiscounted Black formulas on a forward, for the demo vol model's
//! delta coordinate and density. Zero rate: the forward carries carry.
//!
//! The normal CDF is Hart's double-precision rational approximation (as
//! West, "Better approximations to cumulative normal functions", 2005,
//! sets it out): absolute error near 1e-14 within three standard
//! deviations, relative error below 1e-8 in the tails, and exactly 0.5 at
//! zero. The density is a second difference of call prices over a fine
//! strike grid, which divides a price error by the square of the step: a
//! single-precision approximation (error ~1e-7, with a jump at zero) shows
//! there as visible noise around the money. Kept in-crate, free of a
//! numeric dependency.

/// Past this the lower tail is below any f64 a price can resolve.
const TAIL_END: f64 = 37.0;
/// Where the rational form hands over to the continued fraction: 10/√2.
const RATIONAL_END: f64 = 7.071_067_811_865_47;
/// √(2π).
const SQRT_TAU: f64 = 2.506_628_274_631;
/// Hart's numerator and denominator, highest power first.
const P: [f64; 7] = [
    3.526_249_659_989_11e-2,
    0.700_383_064_443_688,
    6.373_962_203_531_65,
    33.912_866_078_383,
    112.079_291_497_871,
    221.213_596_169_931,
    220.206_867_912_376,
];
const Q: [f64; 8] = [
    8.838_834_764_831_84e-2,
    1.755_667_163_182_64,
    16.064_177_579_207,
    86.780_732_202_946_1,
    296.564_248_779_674,
    637.333_633_378_831,
    793.826_512_519_948,
    440.413_735_824_752,
];

/// Standard normal cumulative distribution.
pub fn norm_cdf(x: f64) -> f64 {
    let z = x.abs();
    let lower = if z > TAIL_END {
        0.0
    } else {
        let gauss = (-z * z / 2.0).exp();
        if z < RATIONAL_END {
            let num = P.iter().fold(0.0, |acc, c| acc * z + c);
            let den = Q.iter().fold(0.0, |acc, c| acc * z + c);
            gauss * num / den
        } else {
            let mut b = z + 0.65;
            for k in [4.0, 3.0, 2.0, 1.0] {
                b = z + k / b;
            }
            gauss / b / SQRT_TAU
        }
    };
    if x > 0.0 { 1.0 - lower } else { lower }
}

fn d1(forward: f64, strike: f64, vol: f64, t: f64) -> f64 {
    let s = vol * t.sqrt();
    ((forward / strike).ln() + 0.5 * s * s) / s
}

/// Black call delta N(d1). Callers guarantee `forward > 0`, `strike > 0`,
/// `vol > 0`, `t > 0`.
pub fn call_delta(forward: f64, strike: f64, vol: f64, t: f64) -> f64 {
    norm_cdf(d1(forward, strike, vol, t))
}

/// Undiscounted Black call price on the forward.
pub fn call_price(forward: f64, strike: f64, vol: f64, t: f64) -> f64 {
    let d1 = d1(forward, strike, vol, t);
    let d2 = d1 - vol * t.sqrt();
    forward * norm_cdf(d1) - strike * norm_cdf(d2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_normal_cdf_is_symmetric_and_matches_tabulated_values() {
        assert_eq!(norm_cdf(0.0), 0.5, "no step at zero");
        for (x, want) in [
            (1.0, 0.841_344_746_068_542_9),
            (1.96, 0.975_002_104_851_779_5),
            (-1.96, 0.024_997_895_148_220_435),
            (-3.0, 0.001_349_898_031_630_094_6),
            (2.5, 0.993_790_334_674_223_8),
        ] {
            assert!((norm_cdf(x) - want).abs() < 1e-14, "{x}: {}", norm_cdf(x));
        }
        // The tail, relative: Φ(-8) is 6.220960574271785e-16.
        assert!((norm_cdf(-8.0) / 6.220_960_574_271_785e-16 - 1.0).abs() < 1e-7);
        assert!((norm_cdf(1.0) + norm_cdf(-1.0) - 1.0).abs() < 1e-15);
        assert_eq!(norm_cdf(-40.0), 0.0);
        assert_eq!(norm_cdf(40.0), 1.0);
    }

    /// A second difference of prices on a fine strike step must not see
    /// the CDF's own error: across the money (where d1 and d2 cross zero)
    /// the curvature of call prices in strike is the smooth lognormal
    /// density, with no step where an approximation changes branch.
    #[test]
    fn call_prices_are_smooth_enough_for_a_fine_second_difference() {
        let (f, vol, t) = (100.0, 0.2, 7.0 / 365.0);
        let h = 0.01;
        let pdf: Vec<f64> = (0..400)
            .map(|i| 98.0 + i as f64 * h)
            .map(|k| {
                (call_price(f, k + h, vol, t) - 2.0 * call_price(f, k, vol, t)
                    + call_price(f, k - h, vol, t))
                    / (h * h)
            })
            .collect();
        let peak = pdf.iter().copied().fold(f64::MIN, f64::max);
        let rough = pdf
            .windows(3)
            .map(|w| (w[0] - 2.0 * w[1] + w[2]).abs())
            .fold(0.0, f64::max);
        assert!(rough / peak < 1e-4, "{}", rough / peak);
    }

    #[test]
    fn an_atmf_call_is_worth_about_forward_times_vol_root_t_times_0_4() {
        // F σ √T · 0.3989 is the small-σ√T limit; at 0.2·1 it is within 1%.
        let p = call_price(100.0, 100.0, 0.2, 1.0);
        assert!((p - 7.965_567).abs() < 1e-3, "{p}");
    }

    #[test]
    fn call_delta_decreases_in_strike_and_is_above_half_at_the_forward() {
        let f = 100.0;
        let deltas: Vec<f64> = [60.0, 80.0, 100.0, 120.0, 160.0]
            .iter()
            .map(|k| call_delta(f, *k, 0.25, 0.5))
            .collect();
        for w in deltas.windows(2) {
            assert!(w[0] > w[1], "{deltas:?}");
        }
        assert!(deltas[2] > 0.5 && deltas[2] < 0.6, "{}", deltas[2]);
        assert!(deltas.iter().all(|d| *d > 0.0 && *d < 1.0));
    }
}
