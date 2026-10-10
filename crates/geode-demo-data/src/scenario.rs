//! Scenario batches derived from the risk batch: bucketed vega, a vega spot
//! ladder, a spot ladder, term sensitivities and rho by curve, for the first
//! `coverage` distinct (instrument, underlying) rows. Each bucket is a share
//! of the position's risk measure so Σ over a bucket axis agrees with the
//! blotter; curves exist only in the instrument's own currency so a scope's
//! untouched curves stay blank.

use crate::model::RiskBatch;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::BTreeSet;

/// Distinct (instrument, underlying) rows the demo derives scenarios for.
/// Each row fans out to as many as 110 scenario rows per table, so this
/// bound, not the risk batch size, keeps the scenario store small.
pub const SCENARIO_COVERAGE: usize = 5_000;
pub const TENORS: [&str; 10] = ["1M", "2M", "3M", "6M", "9M", "1Y", "18M", "2Y", "3Y", "5Y"];
pub const MONEYNESS: [f64; 9] = [70.0, 80.0, 90.0, 95.0, 100.0, 105.0, 110.0, 120.0, 130.0];
pub const SPOT_SHIFTS: [f64; 11] = [
    -20.0, -15.0, -10.0, -5.0, -2.0, 0.0, 2.0, 5.0, 10.0, 15.0, 20.0,
];
pub const CURVE_TYPES: [&str; 2] = ["OIS", "RFR"];
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

/// Derives the five scenario tables from `batch`, seeded and deterministic.
///
/// Vega buckets, rho curves and the term table's repo, ATM vol and skew are
/// unjittered shares of the position's own measure, so Σ over a bucket axis
/// reproduces the blotter's value; jitter shapes only the vega spot ladder
/// and the curvature column, which have no blotter total to agree with.
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
    let mut term = table(
        "term_sensitivities",
        &["tenor"],
        &["repo", "atm_vol", "skew", "curvature"],
    );
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
            let mut text = id.clone();
            text.push(tenor.to_string());
            term.rows.push(ScenarioRow {
                text,
                measures: vec![
                    batch.rho010[i] * 0.1 * tw[t],
                    vega * tw[t],
                    batch.skew01[i] * tw[t],
                    batch.normalized_vega01[i] * 0.2 * tw[t] * jitter[t],
                ],
            });
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
            let npv = batch.npv[i] + batch.delta01[i] * s + 0.5 * batch.gamma01[i] * s * s;
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
    ScenarioSet {
        tables: vec![vega_buckets, vega_spot, spot, term, rho],
    }
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

    fn table<'a>(s: &'a ScenarioSet, name: &str) -> &'a ScenarioTable {
        s.tables.iter().find(|t| t.dataset == name).unwrap()
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
        assert_eq!(
            table(&s, "term_sensitivities").rows.len(),
            keys.len() * TENORS.len()
        );
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
    }
}
