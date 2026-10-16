//! Writes a realistic source directory: per-book CSVs in the source's own
//! column spelling, with `.done` JSON sentinels marking files ready to ingest.
//!
//! No quoting or escaping: every string column draws from fixed, comma-free
//! vocabularies. Revisit if a vocabulary ever grows free-form values.

use crate::model::RiskBatch;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Canonical name -> the spelling the source file uses.
const SOURCE_NAMES: &[(&str, &str)] = &[
    ("business_date", "BusinessDate"),
    ("book", "Book"),
    ("lhu", "LHU"),
    ("position_ref", "PositionRef"),
    ("instrument_ref", "InstrumentRef"),
    ("underlying_ref", "Underlying1Ref"),
    ("underlying2_ref", "Underlying2Ref"),
    ("counterparty", "Counterparty"),
    ("strike", "Strike"),
    ("expiry", "Expiry"),
    ("currency", "Currency"),
    ("model_code", "ModelCode"),
    ("delta01", "Delta01"),
    ("delta02", "Delta02"),
    ("delta05", "Delta05"),
    ("gamma01", "Gamma01"),
    ("gamma02", "Gamma02"),
    ("gamma05", "Gamma05"),
    ("vega01", "Vega01"),
    ("normalized_vega01", "NormalizedVega01"),
    ("skew01", "Skew01"),
    ("rho010", "Rho010"),
    ("rho_rfr010", "RhoRFR010"),
    ("rho_ois010", "RhoOIS010"),
    ("cross_gamma02", "CrossGamma02"),
    ("cross_gamma05", "CrossGamma05"),
    ("npv", "NPV"),
    ("daily_pnl", "DailyPNL"),
    ("daily_m2m_pnl", "DailyM2MPNL"),
    ("daily_fx_pnl", "DailyFXPNL"),
    ("clean_theta_business_day", "CleanThetaBusinessDay"),
    ("realized_theta", "RealizedTheta"),
    ("daily_trading_pnl", "DailyTradingPNL"),
    ("sc", "SC"),
];

/// Deterministic FX factor for every `_USD` twin, so twins are reproducible.
pub const USD_FACTOR: f64 = 1.08;

/// Measures emitted with a `_USD` twin using the fixed `USD_FACTOR`.
const USD_TWINS: &[&str] = &[
    "delta01",
    "delta02",
    "delta05",
    "gamma01",
    "gamma02",
    "gamma05",
    "vega01",
    "normalized_vega01",
    "skew01",
    "rho010",
    "rho_rfr010",
    "rho_ois010",
    "cross_gamma02",
    "cross_gamma05",
    "clean_theta_business_day",
    "realized_theta",
];

/// Columns omitted from every third file, together with their USD twins,
/// to exercise missing-column ingestion and health reporting.
const OMITTED_FROM_SOME_FILES: &[&str] = &["skew01", "rho_ois010"];

pub(crate) fn source_name(canonical: &str) -> &'static str {
    SOURCE_NAMES
        .iter()
        .find(|(c, _)| *c == canonical)
        .map(|(_, s)| *s)
        .unwrap_or_else(|| panic!("no source spelling for '{canonical}'"))
}

pub struct EmitOptions {
    pub root: PathBuf,
    /// Withhold the final file's sentinel so discovery leaves it pending.
    /// An existing sentinel at that path is not removed.
    pub leave_one_pending: bool,
}

impl EmitOptions {
    pub fn new(root: impl Into<PathBuf>) -> EmitOptions {
        EmitOptions {
            root: root.into(),
            leave_one_pending: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmittedFile {
    pub csv_path: PathBuf,
    /// `None` when the sentinel was deliberately withheld.
    pub sentinel_path: Option<PathBuf>,
    pub books: Vec<String>,
    pub rows: usize,
    /// Source-spelled column headers, in file order.
    pub columns: Vec<String>,
    pub as_of: String,
}

#[derive(Debug, Clone)]
pub struct EmittedDirectory {
    pub files: Vec<EmittedFile>,
    /// Instruments with deliberately conflicting model codes among their
    /// rows in one file, exercising within-grain conflict detection.
    pub conflicting_instruments: Vec<String>,
}

/// Group row indices into files: most books get one file, `BK000` is split
/// across two, and `BK001`+`BK002` share one.
///
/// Each date has separate files. Split books keep every position in one
/// file: ingestion deduplicates within a file, so spreading a position
/// across partitions would double-count its trading PnL.
fn file_assignments(batch: &RiskBatch) -> BTreeMap<String, Vec<usize>> {
    let mut by_file: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut part_of_position: BTreeMap<&str, u8> = BTreeMap::new();
    let mut next_part: u8 = 1;
    for i in 0..batch.len() {
        let book = &batch.book[i];
        let date = &batch.business_date[i];
        let key = match book.as_str() {
            "BK000" => {
                let part = *part_of_position
                    .entry(batch.position_ref[i].as_str())
                    .or_insert_with(|| {
                        let p = next_part;
                        next_part = if next_part == 1 { 2 } else { 1 };
                        p
                    });
                format!("risk_{date}_BK000_part{part}")
            }
            "BK001" | "BK002" => format!("risk_{date}_BK001_BK002"),
            other => format!("risk_{date}_{other}"),
        };
        by_file.entry(key).or_default().push(i);
    }
    by_file
}

pub fn emit_directory(batch: &RiskBatch, opts: &EmitOptions) -> std::io::Result<EmittedDirectory> {
    std::fs::create_dir_all(&opts.root)?;
    let assignments = file_assignments(batch);
    let mut files = Vec::new();
    let mut conflicting_instruments = Vec::new();

    for (idx, (stem, rows)) in assignments.iter().enumerate() {
        // Every third file omits the optional columns.
        let omit: &[&str] = if idx % 3 == 2 {
            OMITTED_FROM_SOME_FILES
        } else {
            &[]
        };
        let columns = header_columns(omit);

        // Change alternate rows of one instrument in the second file.
        // Within-grain detection needs disagreement inside the group;
        // changing every row would leave the group consistent.
        let conflict_instrument: Option<&str> = if idx == 1 {
            rows.first().map(|&i| batch.instrument_ref[i].as_str())
        } else {
            None
        };

        let csv_path = opts.root.join(format!("{stem}.csv"));
        let mut out = std::io::BufWriter::new(std::fs::File::create(&csv_path)?);
        writeln!(out, "{}", columns.join(","))?;

        let canonical = canonical_columns(omit);
        let mut conflict_row = 0usize;
        for &i in rows {
            // Alternate rows of the chosen instrument carry a wrong model
            // code, so min != max within its instrument-grain group.
            let plant = conflict_instrument.is_some_and(|target| {
                batch.instrument_ref[i] == target && {
                    conflict_row += 1;
                    conflict_row.is_multiple_of(2)
                }
            });
            let mut fields: Vec<String> = Vec::with_capacity(canonical.len());
            for name in &canonical {
                fields.push(field_value(batch, i, name, plant));
            }
            if plant && !conflicting_instruments.contains(&batch.instrument_ref[i]) {
                conflicting_instruments.push(batch.instrument_ref[i].clone());
            }
            writeln!(out, "{}", fields.join(","))?;
        }
        out.flush()?;

        let mut books: Vec<String> = rows.iter().map(|&i| batch.book[i].clone()).collect();
        books.sort_unstable();
        books.dedup();

        // Stagger source times so per-book freshness differs.
        let as_of = format!(
            "2026-08-30T{:02}:{:02}:00Z",
            7 + (idx as u32 % 8),
            (idx as u32 * 7) % 60
        );

        let withhold = opts.leave_one_pending && idx == assignments.len() - 1;
        let sentinel_path = if withhold {
            None
        } else {
            let p = opts.root.join(format!("{stem}.csv.done"));
            let doc = serde_json::json!({
                "dataset": "risk_snapshot",
                "as_of": as_of,
                "business_date": batch.business_date[rows[0]],
                "books": books,
                "row_count": rows.len(),
                "columns": columns,
            });
            std::fs::write(&p, serde_json::to_string_pretty(&doc)?)?;
            Some(p)
        };

        files.push(EmittedFile {
            csv_path,
            sentinel_path,
            books,
            rows: rows.len(),
            columns,
            as_of,
        });
    }

    Ok(EmittedDirectory {
        files,
        conflicting_instruments,
    })
}

fn canonical_columns(omit: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in RiskBatch::IDENTITY {
        out.push((*name).to_string());
    }
    for group in [
        RiskBatch::UNDERLYING_MEASURES,
        RiskBatch::PAIR_MEASURES,
        RiskBatch::INSTRUMENT_MEASURES,
        RiskBatch::POSITION_MEASURES,
    ] {
        for name in group {
            if omit.contains(name) {
                continue;
            }
            out.push((*name).to_string());
            if USD_TWINS.contains(name) {
                out.push(format!("{name}__usd"));
            }
        }
    }
    out
}

fn header_columns(omit: &[&str]) -> Vec<String> {
    canonical_columns(omit)
        .iter()
        .map(|c| match c.strip_suffix("__usd") {
            Some(base) => format!("{}_USD", source_name(base)),
            None => source_name(c).to_string(),
        })
        .collect()
}

fn field_value(batch: &RiskBatch, i: usize, canonical: &str, plant_conflict: bool) -> String {
    if let Some(base) = canonical.strip_suffix("__usd") {
        return format!("{:.6}", batch.measure(base)[i] * USD_FACTOR);
    }
    match canonical {
        "strike" => format!("{:.2}", batch.strike[i]),
        "model_code" if plant_conflict => "CONFLICT".to_string(),
        name if RiskBatch::IDENTITY.contains(&name) => batch.identity(name)[i].clone(),
        name => format!("{:.6}", batch.measure(name)[i]),
    }
}

/// Header spellings for scenario axis and measure columns; identity columns
/// reuse `SOURCE_NAMES`.
const SCENARIO_SOURCE_NAMES: &[(&str, &str)] = &[
    ("tenor", "Tenor"),
    ("moneyness", "Moneyness"),
    ("spot_shift", "SpotShift"),
    ("curve_ccy", "CurveCcy"),
    ("curve_type", "CurveType"),
    ("vega", "Vega"),
    ("npv", "NPV"),
    ("delta", "Delta"),
    ("gamma", "Gamma"),
    ("theta", "Theta"),
    ("repo", "Repo"),
    ("atm_vol", "ATMVol"),
    ("skew", "Skew"),
    ("curvature", "Curvature"),
    ("diva", "Diva"),
    ("rho", "Rho"),
];

fn scenario_header(canonical: &str) -> &'static str {
    SCENARIO_SOURCE_NAMES
        .iter()
        .find(|(c, _)| *c == canonical)
        .map(|(_, s)| *s)
        .unwrap_or_else(|| source_name(canonical))
}

/// One CSV and sentinel per (table, business date, book) under `root`:
/// `<dataset>_<date>_<book>.csv` for a dataset fed by one report, and
/// `<report>_<book>_<date>.csv` for each of several reports into one
/// dataset, so a batch pattern can capture `<report>_<book>` whole: a
/// partition is (dataset, batch, book), and two reports sharing a batch
/// would replace each other's rows on every load. Every measure is followed by its `_USD`
/// twin at the risk emitter's fixed factor. Source times are staggered per
/// file as the risk emitter does.
pub fn emit_scenario_directory(
    set: &crate::ScenarioSet,
    root: &Path,
) -> std::io::Result<Vec<EmittedFile>> {
    std::fs::create_dir_all(root)?;
    let mut files = Vec::new();
    let mut idx = 0u32;
    for table in &set.tables {
        let n_text = table.text_columns();
        let mut headers: Vec<String> = table.columns[..n_text]
            .iter()
            .map(|c| scenario_header(c).to_string())
            .collect();
        for m in &table.columns[n_text..] {
            headers.push(scenario_header(m).to_string());
            headers.push(format!("{}_USD", scenario_header(m)));
        }
        // Group rows by (date, book); the text layout is identity first, so
        // business_date is column 0 and book column 1.
        let mut by_file: BTreeMap<String, Vec<&crate::ScenarioRow>> = BTreeMap::new();
        for r in &table.rows {
            let stem = match table.report {
                None => format!("{}_{}_{}", table.dataset, r.text[0], r.text[1]),
                Some(report) => format!("{report}_{}_{}", r.text[1], r.text[0]),
            };
            by_file.entry(stem).or_default().push(r);
        }
        for (stem, rows) in &by_file {
            let csv_path = root.join(format!("{stem}.csv"));
            let mut out = std::io::BufWriter::new(std::fs::File::create(&csv_path)?);
            writeln!(out, "{}", headers.join(","))?;
            for r in rows {
                let mut fields: Vec<String> = r.text.clone();
                for v in &r.measures {
                    fields.push(format!("{v:.6}"));
                    fields.push(format!("{:.6}", v * USD_FACTOR));
                }
                writeln!(out, "{}", fields.join(","))?;
            }
            out.flush()?;
            let as_of = format!("2026-08-30T{:02}:{:02}:00Z", 9 + (idx % 6), (idx * 11) % 60);
            let book = rows[0].text[1].clone();
            let sentinel_path = root.join(format!("{stem}.csv.done"));
            let doc = serde_json::json!({
                "dataset": table.dataset,
                "as_of": as_of,
                "business_date": rows[0].text[0],
                "books": [book],
                "row_count": rows.len(),
                "columns": headers,
            });
            std::fs::write(&sentinel_path, serde_json::to_string_pretty(&doc)?)?;
            files.push(EmittedFile {
                csv_path,
                sentinel_path: Some(sentinel_path),
                books: vec![book],
                rows: rows.len(),
                columns: headers.clone(),
                as_of,
            });
            idx += 1;
        }
    }
    Ok(files)
}
