//! Scenario batches derived from the risk batch: bucketed vega, a vega spot
//! ladder, a spot ladder, term sensitivities and rho by curve, for the first
//! `coverage` distinct (instrument, underlying) rows. Term sensitivities are
//! one report per measure ([`TERM_REPORTS`]: repo, ATM vol, skew, curvature
//! and dividend sensitivity), each written as its own files into
//! `term_sensitivities`. Each bucket is a share of the position's risk measure so Σ over a bucket
//! axis agrees with the blotter; curves exist only in the instrument's own
//! currency so a scope's untouched curves stay blank.

use crate::model::RiskBatch;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::{BTreeMap, BTreeSet};

/// Distinct (instrument, underlying) rows the demo derives scenarios for.
/// Each row fans out to as many as 110 scenario rows per table, so this
/// bound, not the risk batch size, keeps the scenario store small.
pub const SCENARIO_COVERAGE: usize = 5_000;
pub const TENORS: [&str; 10] = ["1M", "2M", "3M", "6M", "9M", "1Y", "18M", "2Y", "3Y", "5Y"];
pub const MONEYNESS: [f64; 9] = [70.0, 80.0, 90.0, 95.0, 100.0, 105.0, 110.0, 120.0, 130.0];
pub const SPOT_SHIFTS: [f64; 11] = [
    -20.0, -15.0, -10.0, -5.0, -2.0, 0.0, 2.0, 5.0, 10.0, 15.0, 20.0,
];
/// Each tenor's time in years; a bucket's width is its time less the
/// previous tenor's, so the widths sum to the last tenor's five years.
pub const TENOR_YEARS: [f64; 10] = [
    1.0 / 12.0,
    2.0 / 12.0,
    0.25,
    0.5,
    0.75,
    1.0,
    1.5,
    2.0,
    3.0,
    5.0,
];
pub const CURVE_TYPES: [&str; 2] = ["OIS", "RFR"];
/// The reports that feed `term_sensitivities`, one per measure; each
/// report is named for its measure and is its files' name prefix.
pub const TERM_REPORTS: [&str; 5] = ["repo", "atm_vol", "skew", "curvature", "diva"];
pub const SCENARIO_DATASETS: [&str; 5] = [
    "vega_buckets",
    "vega_spot_ladder",
    "spot_ladder",
    "term_sensitivities",
    "rho_curves",
];

/// Identity columns every scenario table carries, in order. The same set
/// as the risk file minus the pair column, so a blotter leaf path drills in.
const IDENTITY: [&str; 11] = [
    "business_date",
    "book",
    "lhu",
    "position_ref",
    "counterparty",
    "instrument_ref",
    "underlying_ref",
    "strike",
    "expiry",
    "currency",
    "model_code",
];

/// Axis columns a scenario table may carry; text, like the identity.
const AXES: [&str; 5] = [
    "tenor",
    "moneyness",
    "spot_shift",
    "curve_ccy",
    "curve_type",
];

/// One scenario row: identity then axis values as text, then measures.
#[derive(Debug, Clone, PartialEq)]
pub struct ScenarioRow {
    pub text: Vec<String>,
    pub measures: Vec<f64>,
}

/// One scenario dataset's rows: identity and axis columns as text, measures
/// as f64 (each with its `_usd` twin emitted by the writer).
#[derive(Debug)]
pub struct ScenarioTable {
    pub dataset: &'static str,
    /// `Some(prefix)` for one of several reports into `dataset`, written as
    /// its own files with that prefix; `None` for a dataset fed by one report.
    pub report: Option<&'static str>,
    /// Canonical column names in emission order (identity, axes, measures).
    pub columns: Vec<&'static str>,
    pub rows: Vec<ScenarioRow>,
}

impl ScenarioTable {
    /// Text columns are identity then axes; measures follow in `columns`.
    /// Read from the column layout, so an empty table still answers.
    pub fn text_columns(&self) -> usize {
        self.columns
            .iter()
            .filter(|c| IDENTITY.contains(c) || AXES.contains(c))
            .count()
    }
}

#[derive(Debug)]
pub struct ScenarioSet {
    pub tables: Vec<ScenarioTable>,
}

/// Tenor weights, a declining share summing to one, so bucket Σ equals the
/// position's measure.
fn tenor_weights() -> [f64; 10] {
    let raw: [f64; 10] = [0.18, 0.15, 0.14, 0.12, 0.10, 0.09, 0.07, 0.06, 0.05, 0.04];
    let s: f64 = raw.iter().sum();
    raw.map(|w| w / s)
}

/// Moneyness weights peaked at the money, summing to one.
fn moneyness_weights() -> [f64; 9] {
    let raw: [f64; 9] = MONEYNESS.map(|m| (-((m - 100.0) / 12.0).powi(2)).exp());
    let s: f64 = raw.iter().sum();
    raw.map(|w| w / s)
}

fn f(v: f64) -> String {
    // Axis values print without trailing zeros so `0` is the base row and
    // `-5` the shift, matching the demo config's `source_name` columns.
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Derives the scenario tables from `batch`, seeded and deterministic: one
/// per dataset, except `term_sensitivities`, which gets one per report in
/// [`TERM_REPORTS`].
///
/// Vega buckets, rho curves and the repo, ATM vol and skew reports are
/// unjittered shares of the position's own measure, so Σ over a bucket axis
/// reproduces the blotter's value; jitter shapes only the vega spot ladder
/// and the curvature report, which have no blotter total to agree with.
/// Diva per bucket is `-delta01 × bucket width in years`: higher dividends
/// lower the forward, so it is opposite in sign to delta and grows with the
/// bucket's time, and Σ over tenors is `-5 × delta01`. It draws nothing
/// from the generator, so the other tables do not depend on it.
pub fn generate_scenarios(batch: &RiskBatch, coverage: usize, seed: u64) -> ScenarioSet {
    let mut rng = StdRng::seed_from_u64(seed);
    // The first `coverage` distinct (instrument, underlying) rows in batch order.
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut rows: Vec<usize> = Vec::new();
    for i in 0..batch.len() {
        if rows.len() == coverage {
            break;
        }
        if seen.insert((
            batch.instrument_ref[i].as_str(),
            batch.underlying_ref[i].as_str(),
        )) {
            rows.push(i);
        }
    }
    // Instrument-level spot sensitivities for the instrument-grain NPV
    // ladder: Σ delta01 and Σ gamma01 over every distinct underlying of the
    // instrument in the whole batch (pair rows repeat a per-underlying value,
    // so each (instrument, underlying) counts once). Computed once per
    // instrument so every row of the instrument carries the identical NPV and
    // the split sees agreeing repeats instead of a conflict.
    let mut counted: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut spot_greeks: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    for i in 0..batch.len() {
        if counted.insert((
            batch.instrument_ref[i].as_str(),
            batch.underlying_ref[i].as_str(),
        )) {
            let e = spot_greeks
                .entry(batch.instrument_ref[i].as_str())
                .or_insert((0.0, 0.0));
            e.0 += batch.delta01[i];
            e.1 += batch.gamma01[i];
        }
    }
    let identity = |i: usize| -> Vec<String> {
        vec![
            batch.business_date[i].clone(),
            batch.book[i].clone(),
            batch.lhu[i].clone(),
            batch.position_ref[i].clone(),
            batch.counterparty[i].clone(),
            batch.instrument_ref[i].clone(),
            batch.underlying_ref[i].clone(),
            f(batch.strike[i]),
            batch.expiry[i].clone(),
            batch.currency[i].clone(),
            batch.model_code[i].clone(),
        ]
    };
    let tw = tenor_weights();
    let mw = moneyness_weights();
    let table =
        |dataset: &'static str, axes: &[&'static str], measures: &[&'static str]| ScenarioTable {
            dataset,
            report: None,
            columns: IDENTITY
                .iter()
                .copied()
                .chain(axes.iter().copied())
                .chain(measures.iter().copied())
                .collect(),
            rows: Vec::new(),
        };

    let mut vega_buckets = table("vega_buckets", &["tenor", "moneyness"], &["vega"]);
    let mut vega_spot = table("vega_spot_ladder", &["tenor", "spot_shift"], &["vega"]);
    let mut spot = table(
        "spot_ladder",
        &["spot_shift"],
        &["npv", "delta", "gamma", "vega", "theta"],
    );
    let mut term: Vec<ScenarioTable> = TERM_REPORTS
        .iter()
        .map(|r| {
            let mut t = table("term_sensitivities", &["tenor"], std::slice::from_ref(r));
            t.report = Some(r);
            t
        })
        .collect();
    let mut rho = table(
        "rho_curves",
        &["tenor", "curve_ccy", "curve_type"],
        &["rho"],
    );

    for &i in &rows {
        let id = identity(i);
        let vega = batch.vega01[i];
        let jitter: Vec<f64> = (0..TENORS.len())
            .map(|_| rng.random_range(0.85..1.15))
            .collect();
        for (t, tenor) in TENORS.iter().enumerate() {
            // Buckets: an exact, unjittered share of vega per (tenor,
            // moneyness), so the 90 cells sum to vega01.
            let tenor_share = vega * tw[t];
            for (m, money) in MONEYNESS.iter().enumerate() {
                let mut text = id.clone();
                text.push(tenor.to_string());
                text.push(f(*money));
                vega_buckets.rows.push(ScenarioRow {
                    text,
                    measures: vec![tenor_share * mw[m]],
                });
            }
            let skew_sign = if t % 2 == 0 { 1.0 } else { -1.0 };
            for shift in SPOT_SHIFTS {
                let s = shift / 100.0;
                let mut text = id.clone();
                text.push(tenor.to_string());
                text.push(f(shift));
                let v =
                    tenor_share * (1.0 - 0.6 * s.abs()) * (1.0 + 0.3 * s * skew_sign) * jitter[t];
                vega_spot.rows.push(ScenarioRow {
                    text,
                    measures: vec![v],
                });
            }
            // One value per term report, in `TERM_REPORTS` order.
            let width = TENOR_YEARS[t] - if t == 0 { 0.0 } else { TENOR_YEARS[t - 1] };
            let values = [
                batch.rho010[i] * 0.1 * tw[t],
                vega * tw[t],
                batch.skew01[i] * tw[t],
                batch.normalized_vega01[i] * 0.2 * tw[t] * jitter[t],
                -batch.delta01[i] * width,
            ];
            for (report, value) in term.iter_mut().zip(values) {
                let mut text = id.clone();
                text.push(tenor.to_string());
                report.rows.push(ScenarioRow {
                    text,
                    measures: vec![value],
                });
            }
            for (c, curve) in CURVE_TYPES.iter().enumerate() {
                let base = if c == 0 {
                    batch.rho_ois010[i]
                } else {
                    batch.rho_rfr010[i]
                };
                let mut text = id.clone();
                text.push(tenor.to_string());
                text.push(batch.currency[i].clone());
                text.push(curve.to_string());
                rho.rows.push(ScenarioRow {
                    text,
                    measures: vec![base * tw[t]],
                });
            }
        }
        for shift in SPOT_SHIFTS {
            let s = shift; // percent units: delta01 is per 1 % move
            let mut text = id.clone();
            text.push(f(shift));
            let (sum_delta, sum_gamma) = spot_greeks[batch.instrument_ref[i].as_str()];
            let npv = batch.npv[i] + sum_delta * s + 0.5 * sum_gamma * s * s;
            let delta = batch.delta01[i] + batch.gamma01[i] * s;
            let gamma = batch.gamma01[i] * (1.0 - 0.9 * (s / 100.0).powi(2) * 4.0);
            let vega_s = vega * (1.0 - 0.6 * (s / 100.0).abs());
            let theta = batch.clean_theta_business_day[i] * (1.0 + 0.8 * s / 100.0);
            spot.rows.push(ScenarioRow {
                text,
                measures: vec![npv, delta, gamma, vega_s, theta],
            });
        }
    }
    let mut tables = vec![vega_buckets, vega_spot, spot];
    tables.extend(term);
    tables.push(rho);
    ScenarioSet { tables }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeneratorConfig, emit_scenario_directory, generate};

    fn set(rows: usize, coverage: usize) -> ScenarioSet {
        let batch = generate(&GeneratorConfig {
            rows,
            seed: 42,
            business_dates: 1,
        });
        generate_scenarios(&batch, coverage, 42)
    }

    /// The dataset's own table, never a report into it.
    fn table<'a>(s: &'a ScenarioSet, name: &str) -> &'a ScenarioTable {
        s.tables
            .iter()
            .find(|t| t.dataset == name && t.report.is_none())
            .unwrap()
    }

    /// One of the term reports, by name.
    fn report<'a>(s: &'a ScenarioSet, name: &str) -> &'a ScenarioTable {
        s.tables.iter().find(|t| t.report == Some(name)).unwrap()
    }

    fn col(t: &ScenarioTable, name: &str) -> usize {
        t.columns.iter().position(|c| *c == name).unwrap()
    }

    #[test]
    fn same_seed_yields_identical_scenarios() {
        let a = set(2_000, 100);
        let b = set(2_000, 100);
        let (va, vb) = (table(&a, "vega_buckets"), table(&b, "vega_buckets"));
        assert_eq!(va.rows.len(), vb.rows.len());
        assert_eq!(va.rows[7].measures, vb.rows[7].measures);
    }

    #[test]
    fn coverage_bounds_the_underlying_rows_and_every_bucket_is_present() {
        let s = set(5_000, 40);
        let v = table(&s, "vega_buckets");
        assert_eq!(v.rows.len(), 40 * TENORS.len() * MONEYNESS.len());
        let sl = table(&s, "spot_ladder");
        assert_eq!(sl.rows.len(), 40 * SPOT_SHIFTS.len());
        let shift = col(sl, "spot_shift");
        assert!(
            sl.rows.iter().any(|r| r.text[shift] == "0"),
            "the base row is present for diff mode"
        );
        let rho = table(&s, "rho_curves");
        assert_eq!(
            rho.rows.len(),
            40 * TENORS.len() * CURVE_TYPES.len(),
            "two curves in the instrument's own currency"
        );
    }

    #[test]
    fn coverage_above_the_batch_covers_everything_once() {
        let batch = generate(&GeneratorConfig {
            rows: 300,
            seed: 42,
            business_dates: 1,
        });
        let mut keys: Vec<(String, String)> = (0..batch.len())
            .map(|i| {
                (
                    batch.instrument_ref[i].clone(),
                    batch.underlying_ref[i].clone(),
                )
            })
            .collect();
        keys.sort();
        keys.dedup();
        let s = generate_scenarios(&batch, SCENARIO_COVERAGE, 42);
        for r in TERM_REPORTS {
            assert_eq!(report(&s, r).rows.len(), keys.len() * TENORS.len(), "{r}");
        }
    }

    #[test]
    fn vega_buckets_sum_to_the_position_vega() {
        let batch = generate(&GeneratorConfig {
            rows: 500,
            seed: 42,
            business_dates: 1,
        });
        let s = generate_scenarios(&batch, 10, 42);
        let v = table(&s, "vega_buckets");
        let (ci, cu) = (col(v, "instrument_ref"), col(v, "underlying_ref"));
        let cv = 0; // `vega` is the table's only measure
        let first = &v.rows[0];
        let total: f64 = v
            .rows
            .iter()
            .filter(|r| r.text[ci] == first.text[ci] && r.text[cu] == first.text[cu])
            .map(|r| r.measures[cv])
            .sum();
        let i = (0..batch.len())
            .find(|&i| {
                batch.instrument_ref[i] == first.text[ci]
                    && batch.underlying_ref[i] == first.text[cu]
            })
            .unwrap();
        assert!(
            (total - batch.vega01[i]).abs() < 1e-6,
            "{total} vs {}",
            batch.vega01[i]
        );
    }

    /// Term sensitivities are one report per measure: each carries the
    /// identity, `tenor` and its own measure only, a row per tenor, and no
    /// table carries several of them.
    #[test]
    fn each_term_report_carries_only_identity_tenor_and_its_measure() {
        let s = set(5_000, 40);
        for r in TERM_REPORTS {
            let t = report(&s, r);
            assert_eq!(t.dataset, "term_sensitivities", "{r}");
            assert_eq!(t.rows.len(), 40 * TENORS.len(), "{r}");
            let expected: Vec<&str> = IDENTITY.iter().copied().chain(["tenor", r]).collect();
            assert_eq!(t.columns, expected, "{r}");
            assert!(t.rows.iter().all(|row| row.measures.len() == 1), "{r}");
        }
        let into_term = s
            .tables
            .iter()
            .filter(|t| t.dataset == "term_sensitivities")
            .count();
        assert_eq!(into_term, TERM_REPORTS.len(), "no combined term table");
    }

    /// Repo, ATM vol and skew are unjittered shares of the position's own
    /// measure, so each report's buckets sum to it.
    #[test]
    fn the_unjittered_term_reports_sum_to_the_position_measure() {
        let batch = generate(&GeneratorConfig {
            rows: 500,
            seed: 42,
            business_dates: 1,
        });
        let s = generate_scenarios(&batch, 10, 42);
        type Share = fn(&RiskBatch, usize) -> f64;
        let expect: [(&str, Share); 3] = [
            ("repo", |b, i| b.rho010[i] * 0.1),
            ("atm_vol", |b, i| b.vega01[i]),
            ("skew", |b, i| b.skew01[i]),
        ];
        for (name, want) in expect {
            let t = report(&s, name);
            let (ci, cu) = (col(t, "instrument_ref"), col(t, "underlying_ref"));
            let first = &t.rows[0];
            let total: f64 = t
                .rows
                .iter()
                .filter(|r| r.text[ci] == first.text[ci] && r.text[cu] == first.text[cu])
                .map(|r| r.measures[0])
                .sum();
            let i = (0..batch.len())
                .find(|&i| {
                    batch.instrument_ref[i] == first.text[ci]
                        && batch.underlying_ref[i] == first.text[cu]
                })
                .unwrap();
            assert!(
                (total - want(&batch, i)).abs() < 1e-9,
                "{name}: {total} vs {}",
                want(&batch, i)
            );
        }
    }

    /// Diva is opposite in sign to delta and scales with the bucket's time:
    /// each bucket is `-delta01 × width`, so a position's buckets sum to
    /// `-5 × delta01`.
    #[test]
    fn diva_opposes_delta_and_scales_with_the_bucket_time() {
        let batch = generate(&GeneratorConfig {
            rows: 500,
            seed: 42,
            business_dates: 1,
        });
        let s = generate_scenarios(&batch, 10, 42);
        let d = report(&s, "diva");
        let (ci, cu, ct) = (
            col(d, "instrument_ref"),
            col(d, "underlying_ref"),
            col(d, "tenor"),
        );
        let mut sums: BTreeMap<(String, String), f64> = BTreeMap::new();
        for r in &d.rows {
            *sums
                .entry((r.text[ci].clone(), r.text[cu].clone()))
                .or_default() += r.measures[0];
        }
        assert_eq!(sums.len(), 10);
        for ((inst, und), total) in &sums {
            let i = (0..batch.len())
                .find(|&i| batch.instrument_ref[i] == *inst && batch.underlying_ref[i] == *und)
                .unwrap();
            let delta = batch.delta01[i];
            assert!(
                (total + 5.0 * delta).abs() < 1e-9,
                "{inst}/{und}: {total} vs -5 × {delta}"
            );
            for r in d
                .rows
                .iter()
                .filter(|r| r.text[ci] == *inst && r.text[cu] == *und)
            {
                let t = TENORS.iter().position(|x| *x == r.text[ct]).unwrap();
                let width = match t {
                    0 => TENOR_YEARS[0],
                    _ => TENOR_YEARS[t] - TENOR_YEARS[t - 1],
                };
                assert!(
                    (r.measures[0] + delta * width).abs() < 1e-12,
                    "{inst}/{und} at {}",
                    r.text[ct]
                );
            }
        }
    }

    #[test]
    fn the_rho_curves_are_the_instrument_currency() {
        let batch = generate(&GeneratorConfig {
            rows: 500,
            seed: 42,
            business_dates: 1,
        });
        let s = generate_scenarios(&batch, 10, 42);
        let rho = table(&s, "rho_curves");
        let (ci, cc, ccy) = (
            col(rho, "instrument_ref"),
            col(rho, "curve_ccy"),
            col(rho, "currency"),
        );
        for r in &rho.rows {
            assert_eq!(r.text[cc], r.text[ccy], "{}", r.text[ci]);
        }
    }

    fn measure(t: &ScenarioTable, r: &ScenarioRow, name: &str) -> f64 {
        r.measures[col(t, name) - r.text.len()]
    }

    #[test]
    fn instrument_grain_measures_agree_across_an_instruments_underlyings() {
        let batch = generate(&GeneratorConfig {
            rows: 500,
            seed: 42,
            business_dates: 1,
        });
        let s = generate_scenarios(&batch, 50, 42);
        let sl = table(&s, "spot_ladder");
        let (ci, cs) = (col(sl, "instrument_ref"), col(sl, "spot_shift"));
        let mut groups: BTreeMap<(String, String), Vec<(f64, f64)>> = BTreeMap::new();
        for r in &sl.rows {
            groups
                .entry((r.text[ci].clone(), r.text[cs].clone()))
                .or_default()
                .push((measure(sl, r, "npv"), measure(sl, r, "theta")));
        }
        for ((inst, shift), values) in &groups {
            assert!(
                values.iter().all(|v| *v == values[0]),
                "npv/theta disagree within ({inst}, {shift}): {values:?}"
            );
        }
        // Not vacuous: some covered instrument has two or more underlyings,
        // and its group at a nonzero shift has a row per underlying.
        let multi = groups
            .iter()
            .find(|((_, shift), v)| shift == "5" && v.len() >= 2);
        assert!(multi.is_some(), "no covered instrument with 2+ underlyings");
    }

    #[test]
    fn spot_ladder_base_row_sums_match_the_risk_batch() {
        let batch = generate(&GeneratorConfig {
            rows: 500,
            seed: 42,
            business_dates: 1,
        });
        let s = generate_scenarios(&batch, 50, 42);
        let mut first_row: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        for i in 0..batch.len() {
            first_row
                .entry((
                    batch.instrument_ref[i].as_str(),
                    batch.underlying_ref[i].as_str(),
                ))
                .or_insert(i);
        }
        let sl = table(&s, "spot_ladder");
        let (ci, cu, cs) = (
            col(sl, "instrument_ref"),
            col(sl, "underlying_ref"),
            col(sl, "spot_shift"),
        );
        let mut base_rows = 0;
        for r in sl.rows.iter().filter(|r| r.text[cs] == "0") {
            let i = first_row[&(r.text[ci].as_str(), r.text[cu].as_str())];
            assert_eq!(measure(sl, r, "npv"), batch.npv[i], "{}", r.text[ci]);
            assert_eq!(
                measure(sl, r, "theta"),
                batch.clean_theta_business_day[i],
                "{}",
                r.text[ci]
            );
            base_rows += 1;
        }
        assert_eq!(base_rows, 50);

        let rho = table(&s, "rho_curves");
        let (ri, ru, rt) = (
            col(rho, "instrument_ref"),
            col(rho, "underlying_ref"),
            col(rho, "curve_type"),
        );
        let mut sums: BTreeMap<(String, String, String), f64> = BTreeMap::new();
        for r in &rho.rows {
            *sums
                .entry((r.text[ri].clone(), r.text[ru].clone(), r.text[rt].clone()))
                .or_default() += measure(rho, r, "rho");
        }
        assert_eq!(sums.len(), 50 * CURVE_TYPES.len());
        for ((inst, und, curve), total) in &sums {
            let i = first_row[&(inst.as_str(), und.as_str())];
            let expected = if curve == CURVE_TYPES[0] {
                batch.rho_ois010[i]
            } else {
                batch.rho_rfr010[i]
            };
            assert!(
                (total - expected).abs() < 1e-6,
                "{inst}/{und}/{curve}: {total} vs {expected}"
            );
        }
    }

    #[test]
    fn emitting_writes_one_file_per_dataset_and_book_with_a_sentinel() {
        let s = set(1_000, 50);
        let dir = tempfile::tempdir().unwrap();
        let files = emit_scenario_directory(&s, dir.path()).unwrap();
        assert!(files.iter().all(|f| f.sentinel_path.is_some()));
        let names: Vec<String> = files
            .iter()
            .map(|f| {
                f.csv_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("vega_buckets_2026-08-24_BK")),
            "{names:?}"
        );
        let f = files
            .iter()
            .find(|f| {
                f.csv_path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("spot_ladder_")
            })
            .unwrap();
        let text = std::fs::read_to_string(&f.csv_path).unwrap();
        let header = text.lines().next().unwrap();
        assert!(
            header.contains("SpotShift") && header.contains("NPV") && header.contains("NPV_USD"),
            "{header}"
        );
        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(f.sentinel_path.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(doc["dataset"], "spot_ladder");
        assert_eq!(
            doc["columns"].as_array().unwrap().len(),
            header.split(',').count()
        );

        // Each term report: its own files, book before date, into
        // `term_sensitivities`, with only the identity, tenor and its
        // measure's headers.
        for (r, measure) in TERM_REPORTS
            .iter()
            .zip(["Repo", "ATMVol", "Skew", "Curvature", "Diva"])
        {
            let f = files
                .iter()
                .find(|f| {
                    f.csv_path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(&format!("{r}_BK"))
                })
                .unwrap_or_else(|| panic!("a {r} file"));
            let name = f.csv_path.file_name().unwrap().to_string_lossy();
            assert!(name.ends_with("_2026-08-24.csv"), "{name}");
            let header = std::fs::read_to_string(&f.csv_path).unwrap();
            let header: Vec<&str> = header.lines().next().unwrap().split(',').collect();
            let usd = format!("{measure}_USD");
            assert_eq!(header[header.len() - 3..], ["Tenor", measure, usd.as_str()]);
            assert_eq!(header.len(), IDENTITY.len() + 3, "{r}: {header:?}");
            let doc: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(f.sentinel_path.as_ref().unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(doc["dataset"], "term_sensitivities");
        }
        assert!(
            !names.iter().any(|n| n.starts_with("term_sensitivities_")),
            "no combined term file"
        );
    }
}
