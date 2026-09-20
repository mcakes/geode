//! Synthetic market-data documents (market-data-documents plan, Task 10).
//! `geode-demo-data` depends on `geode-core` alone here — it produces
//! [`geode_core::document::DocumentRows`] and never writes XML; turning
//! those rows into wire bytes is `geode-documents`' job
//! (`geode_documents::CviKind::write`), called from `geode-app`'s demo
//! bus, not from here.

/// The CVI document kind (spec §6.3): a seeded generator over a fixed
/// node ladder and eight listed monthly expiries.
pub mod cvi {
    use chrono::{Datelike, NaiveDate};
    use geode_core::document::{Column, DocumentRows, Value};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::collections::HashMap;

    /// The CVI node ladder (market-data spec §6.3), fixed across every
    /// document this generator ever produces — `CviKind::write` refuses a
    /// grid whose node list is not the same on every term (spec's "full
    /// and positional" rule), so there is exactly one ladder here, not
    /// one drawn per call.
    pub const NODES: [f64; 12] = [
        -20.0, -15.0, -10.0, -5.0, -2.5, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 3.5,
    ];

    /// Eight listed expiries per document (Task 10 brief): one per month,
    /// starting the month `anchor` itself falls in.
    const EXPIRY_MONTHS: usize = 8;

    /// The maximum a single walk step ever moves one node's `param`
    /// between two successive calls for the same key — small enough that
    /// the smile stays recognisably the same shape from one document to
    /// the next, big enough that `successive_documents_drift` never sees
    /// two calls collide on an identical grid by chance.
    const MAX_WALK_STEP: f64 = 0.02;
    const MIN_WALK_STEP: f64 = 0.0005;

    /// The per-slice values (2026-09-17): `forward` is the spot carried
    /// out to the term at a per-key rate drawn once from this range (a
    /// fraction per month, so 0.2–0.5%/month), `atm` is a decimal vol
    /// held inside `ATM_RANGE` and walked per publish, `skew` a
    /// negative slope held inside `SKEW_RANGE` and walked the same way.
    const CARRY_PER_MONTH: std::ops::RangeInclusive<f64> = 0.002..=0.005;
    const ATM_RANGE: std::ops::RangeInclusive<f64> = 0.15..=0.30;
    const SKEW_RANGE: std::ops::RangeInclusive<f64> = -2.0..=0.0;
    const ATM_WALK_STEP: f64 = 0.004;
    const SKEW_WALK_STEP: f64 = 0.02;
    const DAYS_PER_MONTH: f64 = 30.4375;

    /// A per-underlying starting level: three named benchmarks (SPX,
    /// NDX, RUT), and a stable hash for anything else so an unfamiliar
    /// vocabulary still gets a plausible, deterministic spot rather than
    /// one default value shared by every other underlying.
    fn base_spot_ref(key: &str) -> f64 {
        match key {
            "SPX" => 7650.0,
            "NDX" => 22000.0,
            "RUT" => 2300.0,
            other => 1000.0 + (fnv1a(other) % 5_000) as f64,
        }
    }

    /// FNV-1a: a small, stable (not `HashMap`'s randomised default)
    /// string hash, used only to seed a key's own RNG and to spread
    /// `base_spot_ref`'s "other" branch — nothing here needs
    /// cryptographic strength, only that the same key always hashes the
    /// same way across processes, which `RandomState` does not promise.
    fn fnv1a(s: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// The smile/skew a key's very first document starts from: a
    /// deterministic function of the node and the term's position in the
    /// ladder, so two generators built with the same seed produce an
    /// identical first document before any walk step has ever been
    /// drawn (`same_seed_same_documents`).
    fn baseline_param(node: f64, term_idx: usize) -> f64 {
        -0.01 * node + 0.02 * (term_idx as f64 + 1.0).ln()
    }

    /// One seeded walk step of at most `step`, then held inside `range`:
    /// a clamp rather than a reflection, because a demo value at the
    /// edge of its range for a publish or two is what a real feed does.
    fn walk_within(
        rng: &mut StdRng,
        value: f64,
        step: f64,
        range: &std::ops::RangeInclusive<f64>,
    ) -> f64 {
        let magnitude = rng.random_range(step * 0.1..step);
        let sign: f64 = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
        (value + magnitude * sign).clamp(*range.start(), *range.end())
    }

    /// The per-key, per-term slice values between publishes. `carry` is
    /// fixed from the key's first call (so `forward` follows `spot_ref`
    /// and never drifts on its own); `atm` and `skew` are one walk state
    /// per term, term-major beside `CviGenerator::walk`.
    struct SliceWalk {
        carry: f64,
        atm: Vec<f64>,
        skew: Vec<f64>,
    }

    /// The third Friday of `year`/`month` — CVI's listed-expiry
    /// convention. Its own test below pins the day-of-week arithmetic
    /// against a hand-checked date, so a future refactor of the offset
    /// math cannot silently drift the whole expiry ladder by a week.
    fn third_friday(year: i32, month: u32) -> NaiveDate {
        let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid calendar month");
        let first_weekday = first.weekday().num_days_from_monday(); // Mon=0..Sun=6
        const FRIDAY: u32 = 4; // chrono::Weekday::Fri.num_days_from_monday()
        let first_friday_day = 1 + (FRIDAY + 7 - first_weekday) % 7;
        first
            .with_day(first_friday_day + 14)
            .expect("the third Friday of a month is always within it")
    }

    /// `EXPIRY_MONTHS` monthly listed expiries, ascending, starting at the
    /// first month whose third Friday is ON OR AFTER `anchor` —
    /// `the_grid_is_full_and_term_major`'s "terms sorted" rests on this
    /// always producing an ascending list.
    ///
    /// The anchor's own calendar month is skipped when that month's
    /// listed expiry has already passed (an anchor drawn the day after a
    /// month's third Friday, say): a document whose nearest term already
    /// expired is not a plausible live CVI grid, and starting from
    /// `(anchor.year(), anchor.month())` unconditionally used to produce
    /// exactly that.
    fn expiries(anchor: NaiveDate) -> Vec<NaiveDate> {
        let (mut y0, mut m0) = (anchor.year(), anchor.month());
        if third_friday(y0, m0) < anchor {
            if m0 == 12 {
                y0 += 1;
                m0 = 1;
            } else {
                m0 += 1;
            }
        }
        (0..EXPIRY_MONTHS)
            .map(|i| {
                let total = m0 as i32 - 1 + i as i32;
                let y = y0 + total.div_euclid(12);
                let m = (total.rem_euclid(12) + 1) as u32;
                third_friday(y, m)
            })
            .collect()
    }

    /// A seeded CVI document generator (Task 10): the same node ladder
    /// and expiry ladder on every call for a key, `spot_ref` fixed from
    /// that key's very first call, and `param` drifting by a small
    /// seeded random-walk step from the previous call — everything else
    /// held identical, so two successive documents for one key are
    /// visibly the same shape, just moved.
    pub struct CviGenerator {
        seed: u64,
        underlyings: Vec<String>,
        anchor: NaiveDate,
        expiries: Vec<NaiveDate>,
        /// Fixed once per key, at its first `next_document` call — never
        /// redrawn afterwards (`successive_documents_drift`: two calls
        /// for one key share `spot_ref`).
        spot: HashMap<String, f64>,
        /// One walk state per key, term-major (spec §6.3's row order),
        /// length `expiries.len() * NODES.len()`.
        walk: HashMap<String, Vec<f64>>,
        /// The per-slice values' own walk, one entry per term.
        slices: HashMap<String, SliceWalk>,
        rngs: HashMap<String, StdRng>,
    }

    impl CviGenerator {
        /// Seeded; `anchor` is the business date every document this
        /// generator produces carries as its `anchor_date` attribute.
        pub fn new(seed: u64, underlyings: Vec<String>, anchor: NaiveDate) -> CviGenerator {
            CviGenerator {
                seed,
                expiries: expiries(anchor),
                underlyings,
                anchor,
                spot: HashMap::new(),
                walk: HashMap::new(),
                slices: HashMap::new(),
                rngs: HashMap::new(),
            }
        }

        pub fn underlyings(&self) -> &[String] {
            &self.underlyings
        }

        /// The next document for `key`: the same node ladder, the same
        /// eight listed expiries, `spot_ref` fixed from this key's first
        /// call, and `param` drifted by one seeded walk step per node
        /// from the previous call for this key.
        pub fn next_document(&mut self, key: &str) -> DocumentRows {
            let n_terms = self.expiries.len();
            if !self.walk.contains_key(key) {
                // First call for this key: seed its own RNG from a
                // stable hash of the key (never the process's random
                // `HashMap` state), draw `spot_ref` once, and lay down
                // the deterministic baseline smile.
                let mut rng = StdRng::seed_from_u64(self.seed ^ fnv1a(key));
                let jitter: f64 = rng.random_range(-0.002..=0.002);
                let spot = base_spot_ref(key) * (1.0 + jitter);
                let baseline: Vec<f64> = (0..n_terms)
                    .flat_map(|t| NODES.iter().map(move |n| baseline_param(*n, t)))
                    .collect();
                // The slice values' starting points: one carry rate per
                // key, an ATM level with a gentle upward term structure,
                // a skew that flattens with the term — each drawn after
                // the spot and the smile, so the existing draws keep
                // their order.
                let carry = rng.random_range(CARRY_PER_MONTH);
                let atm0: f64 = rng.random_range(0.16..=0.24);
                let skew0: f64 = rng.random_range(-1.6..=-0.8);
                let atm = (0..n_terms)
                    .map(|t| (atm0 + 0.004 * t as f64).clamp(*ATM_RANGE.start(), *ATM_RANGE.end()))
                    .collect();
                let skew = (0..n_terms)
                    .map(|t| {
                        (skew0 + 0.05 * t as f64).clamp(*SKEW_RANGE.start(), *SKEW_RANGE.end())
                    })
                    .collect();
                self.spot.insert(key.to_string(), spot);
                self.walk.insert(key.to_string(), baseline);
                self.slices
                    .insert(key.to_string(), SliceWalk { carry, atm, skew });
                self.rngs.insert(key.to_string(), rng);
            } else {
                let rng = self
                    .rngs
                    .get_mut(key)
                    .expect("an rng is seeded alongside every key's walk state");
                let params = self
                    .walk
                    .get_mut(key)
                    .expect("checked present by the branch above");
                for p in params.iter_mut() {
                    // The mutation entry "the generator's drift" targets
                    // this step: forcing it to 0 must make two
                    // successive documents for one key compare equal,
                    // which `successive_documents_drift` alone catches.
                    let magnitude = rng.random_range(MIN_WALK_STEP..MAX_WALK_STEP);
                    let sign: f64 = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
                    *p += magnitude * sign;
                }
                let slices = self
                    .slices
                    .get_mut(key)
                    .expect("a slice walk is seeded alongside every key's walk state");
                for a in slices.atm.iter_mut() {
                    *a = walk_within(rng, *a, ATM_WALK_STEP, &ATM_RANGE);
                }
                for k in slices.skew.iter_mut() {
                    *k = walk_within(rng, *k, SKEW_WALK_STEP, &SKEW_RANGE);
                }
            }

            let spot_ref = self.spot[key];
            // A demo document is produced at most every few seconds by
            // one background thread (`geode_app::demo_bus`), never on
            // the receiver hot path PHILOSOPHY §6 governs — this clone
            // is the one allocation `next_document` makes beyond the
            // axis columns it has to build regardless.
            let params = self.walk[key].clone();

            let total = n_terms * NODES.len();
            let mut terms = Vec::with_capacity(total);
            let mut nodes = Vec::with_capacity(total);
            // The slice values in the long form: repeated on every node
            // row of their term, which is the shape `CviKind::write`
            // requires (a slice whose rows disagree is refused).
            let mut forward = Vec::with_capacity(total);
            let mut atm = Vec::with_capacity(total);
            let mut skew = Vec::with_capacity(total);
            let slices = &self.slices[key];
            for (t, term) in self.expiries.iter().enumerate() {
                let months = (*term - self.anchor).num_days() as f64 / DAYS_PER_MONTH;
                let fwd = spot_ref * (1.0 + slices.carry * months);
                for node in NODES {
                    terms.push(*term);
                    nodes.push(node);
                    forward.push(fwd);
                    atm.push(slices.atm[t]);
                    skew.push(slices.skew[t]);
                }
            }

            DocumentRows {
                key: vec![key.to_string()],
                attributes: vec![
                    ("anchor_date".to_string(), Value::Date(self.anchor)),
                    ("spot_ref".to_string(), Value::F64(spot_ref)),
                ],
                axes: vec![
                    ("term".to_string(), Column::Date(terms)),
                    ("node".to_string(), Column::F64(nodes)),
                ],
                values: vec![
                    ("param".to_string(), Column::F64(params)),
                    ("forward".to_string(), Column::F64(forward)),
                    ("atm".to_string(), Column::F64(atm)),
                    ("skew".to_string(), Column::F64(skew)),
                ],
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use geode_core::schema::{ColumnRole, ColumnSpec, ColumnType, DatasetSpec, Family};

        fn anchor() -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, 9, 12).unwrap()
        }

        fn underlyings() -> Vec<String> {
            vec!["SPX".to_string(), "NDX".to_string()]
        }

        /// Hand-built rather than through `SchemaSpec::from_doc` +
        /// `config::test_support` (Task 10 brief): pulling in
        /// `geode-core`'s `test-support` feature here would be one more
        /// moving part for the same dataset every other test builds by
        /// hand already declares in `examples/demo-config/datasets.toml`.
        fn cvi_dataset_spec() -> DatasetSpec {
            let col = |name: &str, ty: ColumnType, role: ColumnRole, textual: bool| ColumnSpec {
                name: name.to_string(),
                source_name: None,
                ty,
                required: true,
                textual,
                categorical: false,
                role,
            };
            DatasetSpec {
                name: "cvi_params".to_string(),
                family: Family::Document,
                key: vec!["underlying_ref".to_string()],
                axes: vec!["term".to_string(), "node".to_string()],
                local: false,
                columns: vec![
                    col(
                        "underlying_ref",
                        ColumnType::Utf8,
                        ColumnRole::Dimension { grain: None },
                        true,
                    ),
                    col("term", ColumnType::Date, ColumnRole::Axis, false),
                    col("node", ColumnType::F64, ColumnRole::Axis, false),
                    col("param", ColumnType::F64, ColumnRole::Value, false),
                    col("forward", ColumnType::F64, ColumnRole::Value, false),
                    col("atm", ColumnType::F64, ColumnRole::Value, false),
                    col("skew", ColumnType::F64, ColumnRole::Value, false),
                    col(
                        "anchor_date",
                        ColumnType::Date,
                        ColumnRole::Attribute { grain: None },
                        false,
                    ),
                    col(
                        "spot_ref",
                        ColumnType::F64,
                        ColumnRole::Attribute { grain: None },
                        false,
                    ),
                ],
                series_retention: None,
            }
        }

        #[test]
        fn same_seed_same_documents() {
            let mut a = CviGenerator::new(42, underlyings(), anchor());
            let mut b = CviGenerator::new(42, underlyings(), anchor());
            assert_eq!(a.next_document("SPX"), b.next_document("SPX"));
            assert_eq!(a.next_document("NDX"), b.next_document("NDX"));
            // A second call for the same key on two identically-seeded
            // generators must also agree — same walk step, not just the
            // same baseline.
            assert_eq!(a.next_document("SPX"), b.next_document("SPX"));
        }

        #[test]
        fn successive_documents_drift() {
            let mut g = CviGenerator::new(7, underlyings(), anchor());
            let first = g.next_document("SPX");
            let second = g.next_document("SPX");
            assert_eq!(first.key, second.key);
            assert_eq!(first.axes, second.axes, "term/node axes do not change");
            assert_eq!(
                first.attributes, second.attributes,
                "anchor_date/spot_ref are fixed once a key's first document is drawn"
            );
            assert_ne!(first.values, second.values, "param must drift");
            let atm = |doc: &DocumentRows| doc.values[2].clone();
            assert_eq!(atm(&first).0, "atm");
            assert_ne!(atm(&first).1, atm(&second).1, "atm drifts per publish");
            let forward = |doc: &DocumentRows| doc.values[1].clone();
            assert_eq!(forward(&first).0, "forward");
            assert_eq!(
                forward(&first).1,
                forward(&second).1,
                "forward follows the fixed spot and carry, and does not walk"
            );
        }

        /// The per-slice values (2026-09-17): each is constant across a
        /// term's twelve nodes — the shape `CviKind::write` refuses
        /// otherwise — `forward` carries the spot out with the term,
        /// `atm` is a decimal vol and `skew` a negative slope, both
        /// held in their ranges across many publishes.
        #[test]
        fn slice_values_are_constant_within_a_term_and_in_range() {
            let mut g = CviGenerator::new(3, underlyings(), anchor());
            for publish in 0..40 {
                let doc = g.next_document("SPX");
                let names: Vec<&str> = doc.values.iter().map(|(n, _)| n.as_str()).collect();
                assert_eq!(names, ["param", "forward", "atm", "skew"]);
                let col = |i: usize| match &doc.values[i].1 {
                    Column::F64(v) => v.clone(),
                    other => panic!("value {i} is f64, got {other:?}"),
                };
                let (forward, atm, skew) = (col(1), col(2), col(3));
                let spot = match doc.attributes[1].1 {
                    Value::F64(s) => s,
                    _ => panic!("spot_ref is f64"),
                };
                let mut previous_forward = spot;
                for t in 0..8 {
                    let rows = t * NODES.len()..(t + 1) * NODES.len();
                    for (name, column) in [("forward", &forward), ("atm", &atm), ("skew", &skew)] {
                        let first = column[rows.start];
                        assert!(
                            column[rows.clone()].iter().all(|v| *v == first),
                            "publish {publish}, term {t}: {name} must be constant across the slice"
                        );
                    }
                    assert!(
                        forward[rows.start] > previous_forward,
                        "publish {publish}, term {t}: forward grows with the term"
                    );
                    assert!(
                        forward[rows.start] < spot * 1.05,
                        "a small carry, not a different level: {} against spot {spot}",
                        forward[rows.start]
                    );
                    previous_forward = forward[rows.start];
                    assert!(
                        ATM_RANGE.contains(&atm[rows.start]),
                        "atm {}",
                        atm[rows.start]
                    );
                    assert!(
                        SKEW_RANGE.contains(&skew[rows.start]),
                        "skew {}",
                        skew[rows.start]
                    );
                }
            }
        }

        #[test]
        fn the_grid_is_full_and_term_major() {
            let ds = cvi_dataset_spec();
            let mut g = CviGenerator::new(1, underlyings(), anchor());
            let doc = g.next_document("SPX");
            assert_eq!(doc.rows(), 8 * NODES.len());

            let (_, term_col) = &doc.axes[0];
            let Column::Date(terms) = term_col else {
                panic!("axis 0 is 'term', a date column");
            };
            let mut distinct: Vec<NaiveDate> = Vec::new();
            for t in terms {
                if distinct.last() != Some(t) {
                    assert!(
                        !distinct.contains(t),
                        "term {t:?} appears in two non-adjacent blocks"
                    );
                    distinct.push(*t);
                }
            }
            let mut sorted = distinct.clone();
            sorted.sort();
            assert_eq!(distinct, sorted, "terms must be sorted ascending");

            doc.validate(&ds)
                .expect("a full, term-major CVI grid validates against the dataset");
        }

        #[test]
        fn third_friday_of_september_2026_is_the_18th() {
            // Hand-checked: 2026-09-01 is a Tuesday, so the Fridays that
            // month are the 4th, 11th, 18th and 25th.
            assert_eq!(
                third_friday(2026, 9),
                NaiveDate::from_ymd_opt(2026, 9, 18).unwrap()
            );
        }

        /// Part 2 residual: `expiries` used to start at the anchor's OWN
        /// month regardless of whether that month's third Friday had
        /// already passed — an anchor drawn the day after September's own
        /// listed expiry (2026-09-18) still opened the ladder with a term
        /// already one day expired. The ladder must start at the first
        /// month whose third Friday is ON OR AFTER the anchor.
        #[test]
        fn expiries_skip_a_month_whose_third_friday_has_already_passed() {
            let anchor = NaiveDate::from_ymd_opt(2026, 9, 19).unwrap();
            let first = expiries(anchor)[0];
            assert_eq!(
                first,
                NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
                "September's own third Friday (the 18th) is already behind the \
                 anchor, so the ladder must open on October's"
            );
        }
    }
}
