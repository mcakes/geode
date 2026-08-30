//! Deterministic synthetic risk data at the desk's real grain (spec §9.1).
//! Seeded: same config always yields identical data. Struct-of-arrays per
//! PHILOSOPHY §6 — no row objects.

mod emit;
mod generate;
mod model;

pub use emit::{EmitOptions, EmittedDirectory, EmittedFile, emit_directory};
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
    fn reaches_the_requested_row_count() {
        // Book and LHU cardinality is fixed, so position count is what must
        // scale. Before this was enforced the generator capped near 3k rows
        // per business date and the §7.4 million-row benchmarks would have
        // silently measured a few thousand.
        for target in [1_000usize, 20_000, 250_000] {
            let b = generate(&GeneratorConfig {
                rows: target,
                seed: 42,
                business_dates: 1,
            });
            assert_eq!(b.len(), target, "target {target}");
        }
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

    #[test]
    fn emits_csvs_and_sentinels_with_the_awkward_cases() {
        let dir = tempfile::tempdir().unwrap();
        let batch = generate(&cfg(20_000));
        let out = emit_directory(&batch, &EmitOptions::new(dir.path())).unwrap();

        assert!(out.files.len() >= 4, "expected several files");

        // A book split across two files — per business date, since each
        // date produces its own generation of every file.
        let dates: HashSet<&String> = batch.business_date.iter().collect();
        let split: Vec<&str> = out
            .files
            .iter()
            .filter(|f| f.books == vec!["BK000".to_string()])
            .map(|f| f.csv_path.file_stem().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            split.len(),
            2 * dates.len(),
            "BK000 must be split across two files per business date: {split:?}"
        );
        for date in &dates {
            for part in [1, 2] {
                let want = format!("risk_{date}_BK000_part{part}");
                assert!(split.contains(&want.as_str()), "missing {want}");
            }
        }

        // A file carrying more than one book.
        assert!(
            out.files.iter().any(|f| f.books.len() > 1),
            "expected a multi-book file"
        );

        // Exactly one CSV without a sentinel (readiness: pending).
        let pending: Vec<_> = out
            .files
            .iter()
            .filter(|f| f.sentinel_path.is_none())
            .collect();
        assert_eq!(pending.len(), 1);

        // Optional columns absent from at least one file.
        assert!(
            out.files
                .iter()
                .any(|f| !f.columns.iter().any(|c| c == "Skew01")),
            "expected a file missing an optional column"
        );

        for f in &out.files {
            assert!(f.csv_path.exists());
            if let Some(s) = &f.sentinel_path {
                assert!(s.exists());
            }
        }
    }

    #[test]
    fn sentinel_json_carries_source_time_and_columns() {
        let dir = tempfile::tempdir().unwrap();
        let batch = generate(&cfg(5_000));
        let out = emit_directory(&batch, &EmitOptions::new(dir.path())).unwrap();
        let f = out
            .files
            .iter()
            .find(|f| f.sentinel_path.is_some())
            .unwrap();
        let text = std::fs::read_to_string(f.sentinel_path.as_ref().unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert!(v["as_of"].as_str().unwrap().starts_with("20"));
        assert_eq!(v["row_count"].as_u64().unwrap() as usize, f.rows);
        let cols: Vec<String> = v["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap().to_string())
            .collect();
        assert_eq!(cols, f.columns);
        assert!(
            cols.contains(&"Delta01".to_string()),
            "source spelling, not snake_case"
        );
        assert!(cols.contains(&"Delta01_USD".to_string()));
    }

    #[test]
    fn csv_row_count_matches_the_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let batch = generate(&cfg(5_000));
        let out = emit_directory(&batch, &EmitOptions::new(dir.path())).unwrap();
        for f in &out.files {
            let text = std::fs::read_to_string(&f.csv_path).unwrap();
            assert_eq!(text.lines().count(), f.rows + 1, "{:?}", f.csv_path);
        }
    }

    #[test]
    fn conflicting_instrument_attributes_are_planted() {
        let dir = tempfile::tempdir().unwrap();
        let batch = generate(&cfg(20_000));
        let opts = EmitOptions::new(dir.path());
        let out = emit_directory(&batch, &opts).unwrap();
        assert!(
            !out.conflicting_instruments.is_empty(),
            "the fixture must plant at least one attribute disagreement"
        );
    }
}
