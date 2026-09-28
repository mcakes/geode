//! Undiscounted Black formulas on a forward, for the demo vol model's
//! delta coordinate and density. Zero rate: the forward carries carry.
//! The normal CDF is the Numerical Recipes `erfcc` rational
//! approximation (fractional error below 1.2e-7 everywhere), which is
//! precision enough for a stand-in and keeps the crate free of a numeric
//! dependency.

use std::f64::consts::SQRT_2;

/// Complementary error function, fractional error < 1.2e-7.
fn erfc(x: f64) -> f64 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -z * z - 1.265_512_23
        + t * (1.000_023_68
            + t * (0.374_091_96
                + t * (0.096_784_18
                    + t * (-0.186_288_06
                        + t * (0.278_868_07
                            + t * (-1.135_203_98
                                + t * (1.488_515_87 + t * (-0.822_152_23 + t * 0.170_872_77))))))));
    let r = t * poly.exp();
    if x >= 0.0 { r } else { 2.0 - r }
}

/// Standard normal cumulative distribution.
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * erfc(-x / SQRT_2)
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
        // The erfcc polynomial sums to ~3e-8 at z = 0, so the CDF there is
        // 0.5 to within its documented 1.2e-7, not to 1e-9.
        assert!((norm_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((norm_cdf(1.96) - 0.975_002).abs() < 1e-5);
        assert!((norm_cdf(-1.96) - 0.024_998).abs() < 1e-5);
        assert!((norm_cdf(1.0) + norm_cdf(-1.0) - 1.0).abs() < 1e-9);
        assert!(norm_cdf(10.0) > 0.999_999);
        assert!(norm_cdf(-10.0) < 1e-6);
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
