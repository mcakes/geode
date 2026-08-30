//! Deterministic synthetic risk data at the desk's real grain (spec §9.1).
//! Seeded: same config always yields identical data. Struct-of-arrays per
//! PHILOSOPHY §6 — no row objects.

mod generate;
mod model;

pub use generate::{GeneratorConfig, generate};
pub use model::RiskBatch;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn cfg(rows: usize) -> GeneratorConfig {
        GeneratorConfig {
            rows,
            seed: 42,
            business_dates: 2,
        }
    }

    #[test]
    fn same_seed_yields_identical_data() {
        let a = generate(&cfg(2_000));
        let b = generate(&cfg(2_000));
        assert_eq!(a.position_ref, b.position_ref);
        assert_eq!(a.delta01, b.delta01);
        assert_eq!(a.cross_gamma02, b.cross_gamma02);
    }

    #[test]
    fn emits_ordered_pairs_per_instrument() {
        let b = generate(&cfg(5_000));
        // Every row names two distinct underlyings.
        for i in 0..b.len() {
            assert_ne!(b.underlying_ref[i], b.underlying2_ref[i], "row {i}");
        }
        // At least one instrument has three underlyings, hence six rows.
        let mut per_instrument: std::collections::HashMap<&str, HashSet<&str>> = Default::default();
        for i in 0..b.len() {
            per_instrument
                .entry(&b.instrument_ref[i])
                .or_default()
                .insert(&b.underlying_ref[i]);
        }
        assert!(
            per_instrument.values().any(|u| u.len() >= 3),
            "expected at least one worst-of with 3+ underlyings"
        );
    }

    #[test]
    fn coarse_measures_repeat_identically_within_their_grain() {
        let b = generate(&cfg(5_000));
        // NPV is instrument-grain: identical on every row of an instrument.
        let mut seen: std::collections::HashMap<&str, f64> = Default::default();
        for i in 0..b.len() {
            let e = seen.entry(&b.instrument_ref[i]).or_insert(b.npv[i]);
            assert_eq!(
                *e, b.npv[i],
                "npv varies within instrument {}",
                b.instrument_ref[i]
            );
        }
    }

    #[test]
    fn single_underlying_greeks_repeat_across_a_row_s_pairs() {
        let b = generate(&cfg(5_000));
        let mut seen: std::collections::HashMap<(&str, &str), f64> = Default::default();
        for i in 0..b.len() {
            let key = (b.instrument_ref[i].as_str(), b.underlying_ref[i].as_str());
            let e = seen.entry(key).or_insert(b.delta01[i]);
            assert_eq!(
                *e, b.delta01[i],
                "delta01 varies within (instrument, underlying)"
            );
        }
    }

    #[test]
    fn cross_gamma_is_symmetric_across_orderings() {
        let b = generate(&cfg(5_000));
        let mut seen: std::collections::HashMap<(&str, String), f64> = Default::default();
        for i in 0..b.len() {
            let (u1, u2) = (&b.underlying_ref[i], &b.underlying2_ref[i]);
            let canon = if u1 <= u2 {
                format!("{u1}|{u2}")
            } else {
                format!("{u2}|{u1}")
            };
            let e = seen
                .entry((b.instrument_ref[i].as_str(), canon))
                .or_insert(b.cross_gamma02[i]);
            assert_eq!(
                *e, b.cross_gamma02[i],
                "cross gamma differs between orderings"
            );
        }
    }

    #[test]
    fn dimensions_have_realistic_bounded_cardinality() {
        let b = generate(&cfg(20_000));
        let books: HashSet<_> = b.book.iter().collect();
        let underlyings: HashSet<_> = b.underlying_ref.iter().collect();
        let lhus: HashSet<_> = b.lhu.iter().collect();
        assert!((2..=20).contains(&books.len()), "books: {}", books.len());
        assert!(underlyings.len() <= 10);
        assert!(
            lhus.len() > books.len(),
            "each book should hold several LHUs"
        );
    }
}
