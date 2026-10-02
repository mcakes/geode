//! Seeded CVI grids and dividend schedules as [`geode_core::document::DocumentRows`].
//! Generators retain independent state per key. `geode-app`'s demo bus passes
//! their rows to `geode-documents` for wire encoding; this module performs no I/O.

/// Stable per-key hash for RNG seeds, fallback spot levels and dividend IDs.
/// It keeps output reproducible across processes; cryptographic strength is
/// unnecessary.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Seeded CVI grids with a fixed node ladder and eight monthly listed expiries.
pub mod cvi {
    use super::fnv1a;
    use chrono::{Datelike, NaiveDate};
    use geode_core::document::{Column, DocumentRows, Value};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::collections::HashMap;

    /// The same node ladder appears on every term. `CviKind::write` requires
    /// a full grid with matching node positions across terms.
    pub const NODES: [f64; 12] = [
        -20.0, -15.0, -10.0, -5.0, -2.5, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 3.5,
    ];

    /// Eight consecutive monthly expiries, beginning with the first whose
    /// third Friday is on or after the anchor date.
    const EXPIRY_MONTHS: usize = 8;

    /// Bounds for the nonzero step applied to each node parameter per publish.
    const MAX_WALK_STEP: f64 = 0.02;
    const MIN_WALK_STEP: f64 = 0.0005;

    /// Forward uses a fixed per-key carry rate of 0.2–0.5% per month. ATM
    /// volatility and skew walk independently within their respective ranges.
    const CARRY_PER_MONTH: std::ops::RangeInclusive<f64> = 0.002..=0.005;
    const ATM_RANGE: std::ops::RangeInclusive<f64> = 0.15..=0.30;
    const SKEW_RANGE: std::ops::RangeInclusive<f64> = -2.0..=0.0;
    const ATM_WALK_STEP: f64 = 0.004;
    const SKEW_WALK_STEP: f64 = 0.02;
    const DAYS_PER_MONTH: f64 = 30.4375;

    /// Starting spot levels for named indices, with a deterministic per-key
    /// fallback for other underlyings.
    fn base_spot_ref(key: &str) -> f64 {
        match key {
            "SPX" => 7650.0,
            "NDX" => 22000.0,
            "RUT" => 2300.0,
            other => 1000.0 + (fnv1a(other) % 5_000) as f64,
        }
    }

    /// Initial smile parameters depend only on the node and term position.
    fn baseline_param(node: f64, term_idx: usize) -> f64 {
        -0.01 * node + 0.02 * (term_idx as f64 + 1.0).ln()
    }

    /// A seeded step of magnitude below `step`, clamped to `range`.
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

    /// Per-key slice state. Carry is fixed, so forward remains tied to spot;
    /// ATM and skew each retain one walk value per term.
    struct SliceWalk {
        carry: f64,
        atm: Vec<f64>,
        skew: Vec<f64>,
    }

    /// The monthly listed-expiry convention: the third Friday.
    pub(crate) fn third_friday(year: i32, month: u32) -> NaiveDate {
        let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid calendar month");
        let first_weekday = first.weekday().num_days_from_monday(); // Mon=0..Sun=6
        const FRIDAY: u32 = 4; // chrono::Weekday::Fri.num_days_from_monday()
        let first_friday_day = 1 + (FRIDAY + 7 - first_weekday) % 7;
        first
            .with_day(first_friday_day + 14)
            .expect("the third Friday of a month is always within it")
    }

    /// Eight ascending monthly third Fridays, all on or after `anchor`.
    /// Skip the anchor month when its listed expiry has passed.
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

    /// A CVI generator with independent seeded state per key. Node and expiry
    /// axes, spot, carry and forward stay fixed. Node parameters, ATM and skew
    /// walk on subsequent calls for that key.
    pub struct CviGenerator {
        seed: u64,
        underlyings: Vec<String>,
        anchor: NaiveDate,
        expiries: Vec<NaiveDate>,
        /// Spot is drawn once per key and retained for all subsequent documents.
        spot: HashMap<String, f64>,
        /// Term-major node parameters, with `expiries.len() * NODES.len()` entries
        /// per key.
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

        /// Return the initial grid or advance this key’s node, ATM and skew walks.
        /// Other keys’ state and RNG sequences are unaffected.
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
                // Carry is fixed per key. Initial ATM rises with term;
                // skew flattens with term.
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
                    // Each node takes an independent nonzero step.
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
            // The document owns its parameter column; retain the walk
            // state for this key's next publish.
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

        /// CVI schema matching `examples/demo-config/datasets.toml`, assembled
        /// directly to validate generated rows without loading configuration.
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
                computed: false,
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
            // The whole-values compare above is satisfied by `atm`/`skew`
            // drifting alone (they walk independently of `param`), so
            // the walk step's own effect is pinned on `param` by itself.
            let param = |doc: &DocumentRows| doc.values[0].clone();
            assert_eq!(param(&first).0, "param");
            assert_ne!(
                param(&first).1,
                param(&second).1,
                "param itself must drift, not only the slice values"
            );
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

        /// Each slice value repeats across its term’s twelve nodes. Forward
        /// grows with the term; ATM and skew remain within their walk bounds.
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

        /// The first expiry must be on or after the anchor, including anchors
        /// after the current month’s listed expiry.
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

/// Seeded dividend schedules with stable row identities, amount walks,
/// status promotions and periodic appends. Changing values and row sets
/// exercise rebasing an open draft against incoming documents.
pub mod dividend {
    use super::fnv1a;
    use chrono::{Days, NaiveDate};
    use geode_core::document::{Column, DocumentRows, Value};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::collections::HashMap;

    /// Closed status vocabulary shared with `DividendKind`. The app tests
    /// agreement between the two crates so this generator does not need a
    /// dependency on the document codec.
    pub const STATUSES: [&str; 4] = ["estimated", "declared", "paid", "cancelled"];

    /// Index schedules have 30–40 rows with repeated ex-dates. Other keys
    /// have 8–12 quarterly rows and may include one additional special.
    fn is_index(key: &str) -> bool {
        matches!(key, "SPX" | "NDX" | "RUT")
    }

    /// Amount step bounds and a positive floor, in per-share cash units.
    const MAX_WALK_STEP: f64 = 0.05;
    const MIN_WALK_STEP: f64 = 0.001;
    const MIN_AMOUNT: f64 = 0.01;

    /// At creation, non-cancelled rows with ex-dates from today through this
    /// window are declared. Past rows are paid; later rows are estimated.
    const NEAR_DAYS: i64 = 30;

    /// Every third republish promotes the nearest estimated row to declared,
    /// regardless of its distance from the fixed schedule date.
    const PROMOTE_EVERY: u32 = 3;

    /// Every fifth republish appends a new future row, allowing incoming
    /// schedules to grow while a draft remains open.
    const APPEND_EVERY: u32 = 5;

    /// One in twenty rows is cancelled, drawn once at creation.
    const CANCEL_CHANCE: f64 = 0.05;

    /// A dividend row retains its identity and dates for the generator’s
    /// lifetime. Amount may walk; only estimated status is eligible for
    /// promotion, so cancelled rows remain cancelled.
    struct Row {
        /// Creation-order suffix in `D<hash>-<ordinal>`, independent of ex-date
        /// sort order. Appended rows receive new ordinals.
        ordinal: usize,
        ex_date: NaiveDate,
        announced_date: NaiveDate,
        pay_date: NaiveDate,
        amount: f64,
        status: String,
    }

    /// Apply a seeded amount step, clamping at the positive `MIN_AMOUNT` floor.
    fn walk_amount(rng: &mut StdRng, amount: f64) -> f64 {
        let magnitude = rng.random_range(MIN_WALK_STEP..MAX_WALK_STEP);
        let sign: f64 = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
        (amount + magnitude * sign).max(MIN_AMOUNT)
    }

    /// Create a row with an announcement date 30–60 days before its ex-date
    /// and a pay date 14–28 days after it. Cancellation overrides status: past
    /// ex-dates are paid, dates within `NEAR_DAYS` are declared, and later
    /// dates are estimated.
    fn new_row(rng: &mut StdRng, ordinal: usize, today: NaiveDate, ex_date: NaiveDate) -> Row {
        let announced_date = ex_date - Days::new(rng.random_range(30..=60));
        let pay_date = ex_date + Days::new(rng.random_range(14..=28));
        let amount = rng.random_range(0.15..=2.50);
        let cancelled = rng.random_bool(CANCEL_CHANCE);
        let status = if cancelled {
            "cancelled"
        } else if ex_date < today {
            "paid"
        } else if (ex_date - today).num_days() <= NEAR_DAYS {
            "declared"
        } else {
            "estimated"
        }
        .to_string();
        Row {
            ordinal,
            ex_date,
            announced_date,
            pay_date,
            amount,
            status,
        }
    }

    /// Generate dates roughly `step_days` apart, with a short lookback from
    /// `today` and seeded jitter in each date and interval.
    fn spaced_dates(
        rng: &mut StdRng,
        today: NaiveDate,
        count: usize,
        step_days: f64,
    ) -> Vec<NaiveDate> {
        let lookback: i64 = rng.random_range(0..=(step_days as i64).max(1));
        let mut date = today - Days::new(lookback as u64);
        let mut dates = Vec::with_capacity(count);
        for _ in 0..count {
            let jitter: i64 = rng.random_range(-3..=3);
            let jittered = if jitter >= 0 {
                date + Days::new(jitter as u64)
            } else {
                date - Days::new((-jitter) as u64)
            };
            dates.push(jittered);
            let step: i64 = (step_days as i64 + rng.random_range(-5..=5)).max(1);
            date = date + Days::new(step as u64);
        }
        dates
    }

    /// Draw distinct indices by rejection sampling, preserving draw order.
    /// Stable order keeps duplicate-date placement and row ordinals
    /// reproducible for a given seed.
    fn distinct_indices(rng: &mut StdRng, count: usize, bound: usize) -> Vec<usize> {
        let mut chosen = Vec::with_capacity(count);
        while chosen.len() < count {
            let candidate = rng.random_range(0..bound);
            if !chosen.contains(&candidate) {
                chosen.push(candidate);
            }
        }
        chosen
    }

    /// Build an index schedule with 30–40 rows and two or three same-ex-date
    /// pairs, or a regular schedule with 8–12 quarterly rows and an optional
    /// special. Distinct base indices keep index pairs on different dates.
    fn build_schedule(rng: &mut StdRng, today: NaiveDate, key: &str) -> Vec<Row> {
        let mut rows = Vec::new();
        if is_index(key) {
            let pairs = rng.random_range(2..=3);
            let total = rng.random_range(30..=40);
            let distinct = total - pairs;
            let mut dates = spaced_dates(rng, today, distinct, 24.0);
            for i in distinct_indices(rng, pairs, dates.len()) {
                dates.push(dates[i]);
            }
            for ex_date in dates {
                rows.push(new_row(rng, rows.len(), today, ex_date));
            }
        } else {
            let count = rng.random_range(8..=12);
            let dates = spaced_dates(rng, today, count, 91.0);
            for ex_date in dates {
                rows.push(new_row(rng, rows.len(), today, ex_date));
            }
            if rng.random_bool(0.3) {
                // An occasional special: one further-out row off the
                // regular quarterly grid.
                let offset = rng.random_range(400..=700);
                let ex_date = today + Days::new(offset);
                rows.push(new_row(rng, rows.len(), today, ex_date));
            }
        }
        rows
    }

    /// Walk one or two distinct rows on every republish, independently of
    /// the promotion and append cadence.
    fn walk_amounts(rng: &mut StdRng, rows: &mut [Row]) {
        let n = if rows.len() >= 2 && rng.random_bool(0.5) {
            2
        } else {
            1
        };
        for i in distinct_indices(rng, n, rows.len()) {
            rows[i].amount = walk_amount(rng, rows[i].amount);
        }
    }

    /// Promote the nearest estimated row, regardless of its distance from
    /// the schedule date. Leave other statuses unchanged; do nothing if no
    /// estimated row remains.
    fn promote_nearest_estimated(rows: &mut [Row]) {
        if let Some(row) = rows
            .iter_mut()
            .filter(|r| r.status == "estimated")
            .min_by_key(|r| r.ex_date)
        {
            row.status = "declared".to_string();
        }
    }

    /// Append a row 60–120 days after the latest existing ex-date, retaining
    /// all existing identities and dates.
    fn append_row(rng: &mut StdRng, today: NaiveDate, rows: &mut Vec<Row>) {
        let last_ex = rows.iter().map(|r| r.ex_date).max().unwrap_or(today);
        let ex_date = last_ex + Days::new(rng.random_range(60..=120));
        let ordinal = rows.len();
        rows.push(new_row(rng, ordinal, today, ex_date));
    }

    /// Build the `DividendKind` columns with rows sorted by `(ex_date, id)`.
    /// The stored schedule remains in creation order, which determines IDs.
    fn build_document(key: &str, today: NaiveDate, rows: &[Row]) -> DocumentRows {
        let hash = fnv1a(key) % 100_000;
        let ids: Vec<String> = rows
            .iter()
            .map(|r| format!("D{hash}-{}", r.ordinal))
            .collect();
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by(|&a, &b| (rows[a].ex_date, &ids[a]).cmp(&(rows[b].ex_date, &ids[b])));

        let mut sorted_ids = Vec::with_capacity(rows.len());
        let mut ex_dates = Vec::with_capacity(rows.len());
        let mut announced_dates = Vec::with_capacity(rows.len());
        let mut pay_dates = Vec::with_capacity(rows.len());
        let mut amounts = Vec::with_capacity(rows.len());
        let mut statuses = Vec::with_capacity(rows.len());
        for i in order {
            sorted_ids.push(ids[i].clone());
            ex_dates.push(rows[i].ex_date);
            announced_dates.push(rows[i].announced_date);
            pay_dates.push(rows[i].pay_date);
            amounts.push(rows[i].amount);
            statuses.push(rows[i].status.clone());
        }

        DocumentRows {
            key: vec![key.to_string()],
            attributes: vec![
                ("currency".to_string(), Value::Utf8("USD".to_string())),
                ("schedule_date".to_string(), Value::Date(today)),
            ],
            axes: vec![("dividend_id".to_string(), Column::Utf8(sorted_ids))],
            values: vec![
                ("ex_date".to_string(), Column::Date(ex_dates)),
                ("announced_date".to_string(), Column::Date(announced_dates)),
                ("pay_date".to_string(), Column::Date(pay_dates)),
                ("amount".to_string(), Column::F64(amounts)),
                ("status".to_string(), Column::Utf8(statuses)),
            ],
        }
    }

    /// A seeded schedule per key. Existing row IDs and dates remain stable
    /// as amounts walk, estimated rows become declared, and new rows append.
    pub struct DividendGenerator {
        seed: u64,
        underlyings: Vec<String>,
        today: NaiveDate,
        /// One schedule per key, in creation order — never resorted;
        /// `build_document` sorts a fresh copy for every call instead.
        rows: HashMap<String, Vec<Row>>,
        rngs: HashMap<String, StdRng>,
        /// Per-key publish count excluding the initial schedule, used for
        /// promotion and append cadence.
        republishes: HashMap<String, u32>,
    }

    impl DividendGenerator {
        /// Seeded; `today` is the business date every document this
        /// generator produces carries as its `schedule_date` attribute,
        /// and the date every row's status is judged past/near/far
        /// against.
        pub fn new(seed: u64, underlyings: Vec<String>, today: NaiveDate) -> DividendGenerator {
            DividendGenerator {
                seed,
                underlyings,
                today,
                rows: HashMap::new(),
                rngs: HashMap::new(),
                republishes: HashMap::new(),
            }
        }

        pub fn underlyings(&self) -> &[String] {
            &self.underlyings
        }

        /// The next document for `key`: a freshly built schedule on the
        /// first call, or the existing one walked (and, every third
        /// republish promoted, every fifth extended) on every call
        /// after.
        pub fn next_document(&mut self, key: &str) -> DocumentRows {
            if !self.rows.contains_key(key) {
                // First call for this key: seed its own RNG from a
                // stable hash of the key (never the process's random
                // `HashMap` state) and draw the whole starting schedule
                // from it.
                let mut rng = StdRng::seed_from_u64(self.seed ^ fnv1a(key));
                let rows = build_schedule(&mut rng, self.today, key);
                self.rows.insert(key.to_string(), rows);
                self.rngs.insert(key.to_string(), rng);
                self.republishes.insert(key.to_string(), 0);
            } else {
                let rng = self
                    .rngs
                    .get_mut(key)
                    .expect("an rng is seeded alongside every key's schedule");
                let rows = self
                    .rows
                    .get_mut(key)
                    .expect("checked present by the branch above");
                let count = self
                    .republishes
                    .get_mut(key)
                    .expect("seeded alongside the schedule");
                *count += 1;
                let n = *count;
                // Amounts walk every time; promotion and appending use
                // separate cadences against the same publish count.
                walk_amounts(rng, rows);
                if n.is_multiple_of(PROMOTE_EVERY) {
                    promote_nearest_estimated(rows);
                }
                if n.is_multiple_of(APPEND_EVERY) {
                    append_row(rng, self.today, rows);
                }
            }
            build_document(key, self.today, &self.rows[key])
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::HashSet;

        fn today() -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, 9, 19).unwrap()
        }

        fn underlyings() -> Vec<String> {
            vec!["SPX".to_string(), "NDX".to_string(), "XYZ".to_string()]
        }

        fn ex_dates_of(doc: &DocumentRows) -> &[NaiveDate] {
            assert_eq!(doc.values[0].0, "ex_date");
            match &doc.values[0].1 {
                Column::Date(v) => v,
                other => panic!("value 0 is ex_date, a date column, got {other:?}"),
            }
        }

        fn ids_of(doc: &DocumentRows) -> &[String] {
            match &doc.axes[0].1 {
                Column::Utf8(v) => v,
                other => panic!("axis 0 is dividend_id, a utf8 column, got {other:?}"),
            }
        }

        fn statuses_of(doc: &DocumentRows) -> &[String] {
            assert_eq!(doc.values[4].0, "status");
            match &doc.values[4].1 {
                Column::Utf8(v) => v,
                other => panic!("value 4 is status, a utf8 column, got {other:?}"),
            }
        }

        #[test]
        fn same_seed_same_documents() {
            let mut a = DividendGenerator::new(42, underlyings(), today());
            let mut b = DividendGenerator::new(42, underlyings(), today());
            assert_eq!(a.next_document("SPX"), b.next_document("SPX"));
            assert_eq!(a.next_document("XYZ"), b.next_document("XYZ"));
            // A second call for the same key on two identically-seeded
            // generators must also agree — the same walk, promotion and
            // append decisions, not just the same starting schedule.
            assert_eq!(a.next_document("SPX"), b.next_document("SPX"));
        }

        #[test]
        fn an_index_schedule_has_same_day_pairs() {
            let mut g = DividendGenerator::new(1, underlyings(), today());
            let doc = g.next_document("SPX");
            let ex_dates = ex_dates_of(&doc);
            assert!(
                (30..=40).contains(&ex_dates.len()),
                "expected 30-40 rows, got {}",
                ex_dates.len()
            );
            let mut counts: HashMap<NaiveDate, usize> = HashMap::new();
            for d in ex_dates {
                *counts.entry(*d).or_insert(0) += 1;
            }
            let paired = counts.values().filter(|&&c| c > 1).count();
            assert!(
                paired >= 2,
                "expected at least two same-ex-date pairs, got {paired}"
            );
        }

        #[test]
        fn ids_are_stable_across_republishes() {
            let mut g = DividendGenerator::new(9, underlyings(), today());
            let first = g.next_document("XYZ");
            let first_ids: HashSet<String> = ids_of(&first).iter().cloned().collect();
            for _ in 0..3 {
                g.next_document("XYZ");
            }
            let fifth = g.next_document("XYZ");
            let fifth_ids: HashSet<String> = ids_of(&fifth).iter().cloned().collect();
            assert!(
                first_ids.is_subset(&fifth_ids),
                "every id in the first document must still be present in the fifth: \
                 missing {:?}",
                first_ids.difference(&fifth_ids).collect::<Vec<_>>()
            );
        }

        #[test]
        fn a_republish_appends_by_the_fifth() {
            let mut g = DividendGenerator::new(11, underlyings(), today());
            let first_rows = g.next_document("XYZ").rows();
            let mut last_rows = first_rows;
            for _ in 0..5 {
                last_rows = g.next_document("XYZ").rows();
            }
            assert!(
                last_rows > first_rows,
                "expected a row appended by the fifth republish: {first_rows} -> {last_rows}"
            );
        }

        #[test]
        fn rows_are_sorted_by_ex_date_then_id() {
            let mut g = DividendGenerator::new(5, underlyings(), today());
            let doc = g.next_document("SPX");
            let ex_dates = ex_dates_of(&doc);
            let ids = ids_of(&doc);
            for i in 1..ex_dates.len() {
                let prev = (ex_dates[i - 1], &ids[i - 1]);
                let curr = (ex_dates[i], &ids[i]);
                assert!(prev <= curr, "row {i} out of order: {prev:?} > {curr:?}");
            }
        }

        #[test]
        fn every_status_is_in_the_closed_set() {
            let mut g = DividendGenerator::new(3, underlyings(), today());
            for key in ["SPX", "NDX", "XYZ"] {
                for _ in 0..8 {
                    let doc = g.next_document(key);
                    for s in statuses_of(&doc) {
                        assert!(
                            STATUSES.contains(&s.as_str()),
                            "status '{s}' is not one of {STATUSES:?}"
                        );
                    }
                }
            }
        }

        /// The initial document applies the past/near status rules. Cancellation
        /// is an independent override accepted in either date range.
        #[test]
        fn near_rows_are_declared_on_the_first_document() {
            let mut g = DividendGenerator::new(1, underlyings(), today());
            let doc = g.next_document("SPX");
            let ex_dates = ex_dates_of(&doc);
            let statuses = statuses_of(&doc);

            let mut saw_past = false;
            let mut saw_near = false;
            for (ex, status) in ex_dates.iter().zip(statuses.iter()) {
                let days = (*ex - today()).num_days();
                // This test covers the past and near date ranges.
                if *ex < today() {
                    saw_past = true;
                    assert!(
                        status == "paid" || status == "cancelled",
                        "past row {ex} should read paid (or cancelled), got {status}"
                    );
                } else if days <= NEAR_DAYS {
                    saw_near = true;
                    assert!(
                        status == "declared" || status == "cancelled",
                        "near row {ex} ({days}d) should read declared \
                         (or cancelled), got {status}"
                    );
                }
            }
            assert!(saw_past, "expected at least one past row for this seed/key");
            assert!(
                saw_near,
                "expected at least one row within 30 days of today for this seed/key"
            );
        }

        /// The third republish changes exactly the nearest estimated row to
        /// declared; it neither changes other statuses nor appends rows.
        #[test]
        fn the_third_republish_promotes_the_nearest_estimated_row() {
            let mut g = DividendGenerator::new(21, underlyings(), today());
            g.next_document("XYZ"); // creation
            g.next_document("XYZ"); // republish 1
            let second = g.next_document("XYZ"); // republish 2 (no promotion yet)
            let third = g.next_document("XYZ"); // republish 3 (promotes)

            let second_ids = ids_of(&second);
            let second_ex_dates = ex_dates_of(&second);
            let second_statuses = statuses_of(&second);
            let third_ids = ids_of(&third);
            let third_statuses = statuses_of(&third);

            let second_map: HashMap<&str, &str> = second_ids
                .iter()
                .map(String::as_str)
                .zip(second_statuses.iter().map(String::as_str))
                .collect();
            let third_map: HashMap<&str, &str> = third_ids
                .iter()
                .map(String::as_str)
                .zip(third_statuses.iter().map(String::as_str))
                .collect();
            assert_eq!(
                second_map.len(),
                third_map.len(),
                "no append should land between the second and third republish \
                 (PROMOTE_EVERY and APPEND_EVERY never coincide this early)"
            );

            let changed: Vec<&str> = third_map
                .iter()
                .filter(|(id, status)| second_map.get(*id) != Some(*status))
                .map(|(id, _)| *id)
                .collect();
            assert_eq!(
                changed.len(),
                1,
                "expected exactly one status change on the third republish, got {changed:?}"
            );
            let changed_id = changed[0];
            assert_eq!(second_map[changed_id], "estimated");
            assert_eq!(third_map[changed_id], "declared");

            let expected_id = second_ids
                .iter()
                .zip(second_ex_dates.iter())
                .zip(second_statuses.iter())
                .filter(|(_, status)| *status == "estimated")
                .min_by_key(|((_, ex), _)| **ex)
                .map(|((id, _), _)| id.as_str())
                .expect("expected at least one estimated row left to promote for this seed/key");
            assert_eq!(
                changed_id, expected_id,
                "the promoted row must be the nearest-dated estimated row, not just any of them"
            );
        }
    }
}

/// Demo option chains: one document per `(underlying, expiry)`, built from
/// the underlying's latest demo CVI document through the stand-in vol
/// model, so the chain sits near the surface the viewer draws and the
/// difference pane shows small moving residuals. A simulator, not a
/// market: expiries past the CVI's last term take that term's smile.
pub mod chain {
    use super::fnv1a;
    use crate::documents::cvi::third_friday;
    use chrono::{DateTime, Datelike, NaiveDate, Utc};
    use geode_core::document::{Column, DocumentRows, Value};
    use geode_core::vol::{Coordinate, Grid, SliceRequest, VolModel};
    use geode_pricing::{DemoVolModel, black};
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::collections::HashMap;

    /// Monthly expiries per underlying, from the month after the anchor.
    pub const EXPIRIES: usize = 12;
    /// Strikes per expiry: 44 below the at-the-money strike, it, 15 above.
    pub const STRIKES: usize = 60;
    const BELOW: i64 = 44;
    const ABOVE: i64 = 15;
    /// Per-publish residual step and its bound, in vol.
    pub const RESIDUAL_STEP: f64 = 0.001;
    pub const RESIDUAL_BOUND: f64 = 0.005;
    /// No mid vol below this, whatever the curve plus residual says.
    pub const MIN_VOL: f64 = 0.005;
    /// The minimum tick: a wing quote whose bid would price under it has
    /// no bid, and the quote carries its ask side alone.
    pub const MIN_BID: f64 = 0.05;

    /// One underlying's rotation position, residual walks and RNG.
    struct State {
        next: usize,
        residuals: Vec<Vec<f64>>,
        rng: StdRng,
    }

    pub struct ChainGenerator {
        seed: u64,
        underlyings: Vec<String>,
        expiries: Vec<NaiveDate>,
        state: HashMap<String, State>,
    }

    /// The 1-2-5 step nearest `x`: 42 → 50, 121 → 100, 12.6 → 10.
    fn nice_step(x: f64) -> f64 {
        let p = 10f64.powf(x.log10().floor());
        [1.0, 2.0, 5.0, 10.0]
            .iter()
            .map(|m| m * p)
            .min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs()))
            .expect("four candidates")
    }

    fn expiries_after(anchor: NaiveDate) -> Vec<NaiveDate> {
        (1..=EXPIRIES as i32)
            .map(|i| {
                let total = anchor.month() as i32 - 1 + i;
                third_friday(
                    anchor.year() + total.div_euclid(12),
                    (total.rem_euclid(12) + 1) as u32,
                )
            })
            .collect()
    }

    fn date_attr(doc: &DocumentRows, name: &str) -> Option<NaiveDate> {
        doc.attributes.iter().find_map(|(n, v)| match v {
            Value::Date(d) if n == name => Some(*d),
            _ => None,
        })
    }

    fn f64_attr(doc: &DocumentRows, name: &str) -> Option<f64> {
        doc.attributes.iter().find_map(|(n, v)| match v {
            Value::F64(x) if n == name => Some(*x),
            _ => None,
        })
    }

    /// The CVI's term dates.
    fn terms(cvi: &DocumentRows) -> Option<&[NaiveDate]> {
        cvi.axes.iter().find_map(|(n, c)| match c {
            Column::Date(d) if n == "term" => Some(d.as_slice()),
            _ => None,
        })
    }

    /// The CVI's first term, for expiries before its range.
    fn first_term(cvi: &DocumentRows) -> Option<NaiveDate> {
        terms(cvi)?.iter().min().copied()
    }

    /// The CVI's last term, for expiries past its range.
    fn last_term(cvi: &DocumentRows) -> Option<NaiveDate> {
        terms(cvi)?.iter().max().copied()
    }

    /// The half-spread in vol: wider in the wings, and never as wide as
    /// `mid` itself, so the bid vol stays above zero at the vol floor.
    fn half_spread(mid: f64, strike: f64, forward: f64) -> f64 {
        (0.0025 + 0.03 * (strike / forward - 1.0).abs()).min(mid / 2.0)
    }

    impl ChainGenerator {
        pub fn new(seed: u64, underlyings: Vec<String>, anchor: NaiveDate) -> ChainGenerator {
            ChainGenerator {
                seed,
                underlyings,
                expiries: expiries_after(anchor),
                state: HashMap::new(),
            }
        }

        pub fn underlyings(&self) -> &[String] {
            &self.underlyings
        }

        pub fn expiries(&self) -> &[NaiveDate] {
            &self.expiries
        }

        /// The underlying's next expiry in rotation, priced off `cvi`.
        /// Panics if `cvi` is not a demo CVI document the stand-in model
        /// can slice: the demo bus hands it the CVI generator's own output.
        pub fn next_document(
            &mut self,
            underlying: &str,
            cvi: &DocumentRows,
            now: DateTime<Utc>,
        ) -> DocumentRows {
            let seed = self.seed;
            let state = self
                .state
                .entry(underlying.to_string())
                .or_insert_with(|| State {
                    next: 0,
                    residuals: vec![vec![0.0; STRIKES]; EXPIRIES],
                    rng: StdRng::seed_from_u64(seed ^ fnv1a(underlying) ^ 0xC4A1),
                });
            let idx = state.next;
            state.next = (state.next + 1) % EXPIRIES;
            let expiry = self.expiries[idx];

            let first = first_term(cvi).expect("a demo CVI document has terms");
            let last = last_term(cvi).expect("a demo CVI document has terms");
            // The model refuses a date outside the terms: an expiry beyond
            // either end takes that end's smile.
            let curve_date = expiry.clamp(first, last);
            let slice = |grid: Grid| {
                DemoVolModel
                    .slice(
                        cvi,
                        &SliceRequest {
                            expiry: curve_date,
                            coordinate: Coordinate::Strike,
                            grid,
                            density: false,
                        },
                    )
                    .expect("the demo CVI slices at any date within its terms")
            };
            // An empty grid returns the forward alone.
            let forward = slice(Grid::Dense(0)).forward;
            let inc = nice_step(forward * 0.0055);
            let atm = (forward / inc).round() * inc;
            let strikes: Vec<f64> = (-BELOW..=ABOVE)
                .map(|i| atm + i as f64 * inc)
                .filter(|k| *k > 0.0)
                .collect();
            let curve = slice(Grid::At(strikes.clone()));

            let State { residuals, rng, .. } = state;
            let residuals = &mut residuals[idx];
            for r in residuals.iter_mut() {
                let step = rng.random_range(-RESIDUAL_STEP..=RESIDUAL_STEP);
                *r = (*r + step).clamp(-RESIDUAL_BOUND, RESIDUAL_BOUND);
            }

            let anchor = date_attr(cvi, "anchor_date").expect("a demo CVI document has an anchor");
            let t = ((expiry - anchor).num_days() as f64).max(0.5) / 365.0;
            let otm = |k: f64, vol: f64| {
                let call = black::call_price(forward, k, vol, t);
                if k >= forward {
                    call
                } else {
                    call - (forward - k)
                }
            };
            let n = strikes.len();
            let mut bid_vol = Vec::with_capacity(n);
            let mut ask_vol = Vec::with_capacity(n);
            let mut mid_vol = Vec::with_capacity(n);
            let mut bid = Vec::with_capacity(n);
            let mut ask = Vec::with_capacity(n);
            for (i, p) in curve.points.iter().enumerate() {
                let mid = (p.vol + residuals[i]).max(MIN_VOL);
                let h = half_spread(mid, p.strike, forward);
                let (b, a) = (mid - h, mid + h);
                let b_price = otm(p.strike, b).max(0.0);
                mid_vol.push(mid);
                ask_vol.push(a);
                ask.push(otm(p.strike, a).max(0.0));
                // A sub-tick bid is no bid: the side goes whole (vol and
                // price NaN together), as the chain document requires.
                if b_price < MIN_BID {
                    bid_vol.push(f64::NAN);
                    bid.push(f64::NAN);
                } else {
                    bid_vol.push(b);
                    bid.push(b_price);
                }
            }

            let spot_ref = f64_attr(cvi, "spot_ref").expect("a demo CVI document has a spot");
            DocumentRows {
                key: vec![
                    underlying.to_string(),
                    expiry.format("%Y-%m-%d").to_string(),
                ],
                attributes: vec![
                    ("forward".into(), Value::F64(forward)),
                    ("spot_ref".into(), Value::F64(spot_ref)),
                    (
                        "quote_time".into(),
                        Value::Utf8(now.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
                    ),
                ],
                axes: vec![("strike".into(), Column::F64(strikes))],
                values: vec![
                    ("bid_vol".into(), Column::F64(bid_vol)),
                    ("ask_vol".into(), Column::F64(ask_vol)),
                    ("mid_vol".into(), Column::F64(mid_vol)),
                    ("bid".into(), Column::F64(bid)),
                    ("ask".into(), Column::F64(ask)),
                ],
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::documents::cvi::CviGenerator;
        use chrono::TimeZone;

        fn anchor() -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, 9, 12).unwrap()
        }
        fn now() -> DateTime<Utc> {
            Utc.with_ymd_and_hms(2026, 9, 12, 14, 0, 0).unwrap()
        }
        fn cvi() -> DocumentRows {
            CviGenerator::new(42, vec!["SPX".into()], anchor()).next_document("SPX")
        }
        fn f64s(rows: &DocumentRows, name: &str) -> Vec<f64> {
            let col = rows
                .values
                .iter()
                .chain(rows.axes.iter())
                .find(|(n, _)| n == name)
                .map(|(_, c)| c)
                .unwrap();
            let Column::F64(v) = col else {
                panic!("{name} is f64")
            };
            v.clone()
        }

        #[test]
        fn expiries_are_twelve_third_fridays_from_next_month() {
            let g = ChainGenerator::new(1, vec!["SPX".into()], anchor());
            assert_eq!(g.expiries().len(), EXPIRIES);
            assert_eq!(
                g.expiries()[0],
                NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()
            );
            assert_eq!(
                g.expiries()[11],
                NaiveDate::from_ymd_opt(2027, 9, 17).unwrap()
            );
            assert!(g.expiries().windows(2).all(|w| w[1] > w[0]));
        }

        #[test]
        fn successive_calls_rotate_through_every_expiry_with_a_two_part_key() {
            let mut g = ChainGenerator::new(1, vec!["SPX".into()], anchor());
            let cvi = cvi();
            let keys: Vec<Vec<String>> = (0..EXPIRIES + 1)
                .map(|_| g.next_document("SPX", &cvi, now()).key)
                .collect();
            for (i, k) in keys.iter().take(EXPIRIES).enumerate() {
                assert_eq!(
                    k,
                    &vec![
                        "SPX".to_string(),
                        g.expiries()[i].format("%Y-%m-%d").to_string()
                    ]
                );
            }
            assert_eq!(keys[EXPIRIES], keys[0], "the rotation wraps");
        }

        #[test]
        fn strikes_are_sixty_sorted_distinct_and_round() {
            let mut g = ChainGenerator::new(1, vec!["SPX".into()], anchor());
            let doc = g.next_document("SPX", &cvi(), now());
            let strikes = f64s(&doc, "strike");
            assert_eq!(strikes.len(), STRIKES);
            assert!(strikes.windows(2).all(|w| w[1] > w[0]));
            assert!(
                strikes.iter().all(|k| (k / 50.0).fract() == 0.0),
                "SPX steps by 50: {strikes:?}"
            );
        }

        #[test]
        fn mid_sits_within_the_residual_bound_of_the_stand_in_curve() {
            let mut g = ChainGenerator::new(1, vec!["SPX".into()], anchor());
            let cvi = cvi();
            // Publish every expiry sixty times so the first expiry's
            // residuals walk ~60 steps of RESIDUAL_STEP, far enough to hit
            // the bound unclamped; keep the first expiry's latest document.
            let first = g.next_document("SPX", &cvi, now());
            let mut doc = first.clone();
            for _ in 0..(EXPIRIES * 60) {
                let d = g.next_document("SPX", &cvi, now());
                if d.key == first.key {
                    doc = d;
                }
            }
            let strikes = f64s(&doc, "strike");
            let expiry = NaiveDate::parse_from_str(&doc.key[1], "%Y-%m-%d").unwrap();
            let curve = DemoVolModel
                .slice(
                    &cvi,
                    &SliceRequest {
                        expiry,
                        coordinate: Coordinate::Strike,
                        grid: Grid::At(strikes),
                        density: false,
                    },
                )
                .unwrap();
            for (p, mid) in curve.points.iter().zip(f64s(&doc, "mid_vol")) {
                assert!(
                    (mid - p.vol).abs() <= RESIDUAL_BOUND + 1e-12 || mid == MIN_VOL,
                    "strike {}: mid {mid} vs curve {}",
                    p.strike,
                    p.vol
                );
            }
        }

        #[test]
        fn bid_and_ask_bracket_mid_and_prices_are_ordered() {
            let mut g = ChainGenerator::new(1, vec!["SPX".into()], anchor());
            let cvi = cvi();
            for _ in 0..EXPIRIES {
                let doc = g.next_document("SPX", &cvi, now());
                let (bid, mid, ask) = (
                    f64s(&doc, "bid_vol"),
                    f64s(&doc, "mid_vol"),
                    f64s(&doc, "ask_vol"),
                );
                for i in 0..bid.len() {
                    // A dropped bid side is NaN; where present it brackets.
                    assert!(
                        (bid[i].is_nan() || (bid[i] > 0.0 && bid[i] < mid[i])) && mid[i] < ask[i],
                        "{i}: {} {} {}",
                        bid[i],
                        mid[i],
                        ask[i]
                    );
                }
                let (bp, ap) = (f64s(&doc, "bid"), f64s(&doc, "ask"));
                assert!(
                    bp.iter()
                        .zip(&ap)
                        .all(|(b, a)| b.is_nan() || (*b >= 0.0 && b <= a))
                );
            }
        }

        #[test]
        fn a_bid_under_the_minimum_tick_is_dropped_whole() {
            let mut g = ChainGenerator::new(1, vec!["SPX".into()], anchor());
            let cvi = cvi();
            let mut one_sided = Vec::new();
            for _ in 0..EXPIRIES {
                let doc = g.next_document("SPX", &cvi, now());
                let (bid, bid_vol) = (f64s(&doc, "bid"), f64s(&doc, "bid_vol"));
                let (ask, ask_vol) = (f64s(&doc, "ask"), f64s(&doc, "ask_vol"));
                let strikes = f64s(&doc, "strike");
                let forward = f64_attr(&doc, "forward").unwrap();
                for i in 0..bid.len() {
                    assert_eq!(bid[i].is_nan(), bid_vol[i].is_nan(), "{i}");
                    assert!(bid[i].is_nan() || bid[i] >= MIN_BID, "{i}: {}", bid[i]);
                    assert!(ask[i].is_finite() && ask_vol[i].is_finite(), "{i}");
                    if bid[i].is_nan() {
                        assert!(strikes[i] > forward, "only the far call wing");
                        one_sided.push((doc.key[1].clone(), strikes[i]));
                    }
                }
            }
            // Seed 1 from the 2026-09-12 anchor: the two nearest expiries'
            // top call strikes (8300-8450 for 2026-10-16, 8450 for
            // 2026-11-20) price under the tick and go one-sided.
            assert!(
                one_sided.contains(&("2026-10-16".to_string(), 8450.0)),
                "{one_sided:?}"
            );
        }

        #[test]
        fn an_expiry_beyond_the_cvi_takes_its_last_term() {
            // The demo CVI has eight terms from the anchor month; the chain's
            // last expiries lie past them and must still publish.
            let mut g = ChainGenerator::new(1, vec!["SPX".into()], anchor());
            let cvi = cvi();
            let docs: Vec<DocumentRows> = (0..EXPIRIES)
                .map(|_| g.next_document("SPX", &cvi, now()))
                .collect();
            let last = docs.last().unwrap();
            assert_eq!(f64s(last, "mid_vol").len(), STRIKES);
            assert!(
                f64s(last, "mid_vol")
                    .iter()
                    .all(|v| v.is_finite() && *v > 0.0)
            );
        }

        #[test]
        fn a_generated_chain_carries_the_cvi_spot_a_nearby_forward_and_the_quote_time() {
            let mut g = ChainGenerator::new(1, vec!["SPX".into()], anchor());
            let doc = g.next_document("SPX", &cvi(), now());
            let attr = |name: &str| {
                doc.attributes
                    .iter()
                    .find(|(n, _)| n == name)
                    .unwrap()
                    .1
                    .clone()
            };
            assert_eq!(
                attr("quote_time"),
                Value::Utf8("2026-09-12T14:00:00Z".into())
            );
            let Value::F64(spot) = attr("spot_ref") else {
                panic!()
            };
            assert!(spot > 7000.0);
            let Value::F64(fwd) = attr("forward") else {
                panic!()
            };
            assert!(fwd > spot * 0.95 && fwd < spot * 1.1);
            assert_eq!(doc.axes[0].0, "strike");
            assert_eq!(
                doc.values
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>(),
                ["bid_vol", "ask_vol", "mid_vol", "bid", "ask"]
            );
        }

        #[test]
        fn the_half_spread_never_reaches_mid_even_at_the_floor() {
            let forward = 7650.0;
            for strike in [forward * 0.5, forward, forward * 1.5] {
                let h = half_spread(MIN_VOL, strike, forward);
                let bid = MIN_VOL - h;
                assert!(bid > 0.0 && bid < MIN_VOL, "K {strike}: bid vol {bid}");
            }
        }

        #[test]
        fn an_expiry_past_the_cvi_prices_at_its_own_time_to_expiry() {
            // The last expiry lies past the CVI's terms: its smile is the
            // last term's, but its time to expiry is its own.
            let mut g = ChainGenerator::new(1, vec!["SPX".into()], anchor());
            let cvi = cvi();
            let doc = (0..EXPIRIES)
                .map(|_| g.next_document("SPX", &cvi, now()))
                .last()
                .unwrap();
            let expiry = NaiveDate::parse_from_str(&doc.key[1], "%Y-%m-%d").unwrap();
            assert!(expiry > last_term(&cvi).unwrap());
            let anchor_date = date_attr(&cvi, "anchor_date").unwrap();
            let t = (expiry - anchor_date).num_days() as f64 / 365.0;
            let forward = f64_attr(&doc, "forward").unwrap();
            let strikes = f64s(&doc, "strike");
            let i = strikes.iter().position(|k| *k >= forward).unwrap();
            let expected = black::call_price(forward, strikes[i], f64s(&doc, "ask_vol")[i], t);
            let ask = f64s(&doc, "ask")[i];
            assert!((ask - expected).abs() < 1e-9, "ask {ask} vs {expected}");
        }

        #[test]
        fn two_generators_with_one_seed_agree_and_underlyings_walk_independently() {
            let cvi = cvi();
            let mut a = ChainGenerator::new(7, vec!["SPX".into(), "NDX".into()], anchor());
            let mut b = ChainGenerator::new(7, vec!["SPX".into(), "NDX".into()], anchor());
            b.next_document("NDX", &cvi, now());
            // By bit pattern: a dropped bid side is NaN, which `==` never
            // matches.
            let bits = |d: DocumentRows| {
                let cols: Vec<Vec<u64>> = d
                    .values
                    .iter()
                    .chain(d.axes.iter())
                    .map(|(_, c)| match c {
                        Column::F64(v) => v.iter().map(|x| x.to_bits()).collect(),
                        _ => panic!("chain columns are f64"),
                    })
                    .collect();
                (d.key, d.attributes, cols)
            };
            assert_eq!(
                bits(a.next_document("SPX", &cvi, now())),
                bits(b.next_document("SPX", &cvi, now()))
            );
        }
    }
}
