//! The two documents a slice viewer reads, out of the snapshots the query
//! tier answers with. A CVI document goes to the vol door as
//! `DocumentRows`, so its layout here is the kind's (a test pins it); a
//! chain never does: its strikes and mid vols are sent to the door to be
//! placed (`MapRequest`), and its bid and ask vols are painted as given.

use chrono::{DateTime, NaiveDate};
use geode_core::document::{Column, DocumentRows, Value};
use geode_core::snapshot::Snapshot;

pub const CVI: &str = "cvi_params";
pub const CHAIN: &str = "option_chain";
pub const CVI_AXES: [&str; 2] = ["term", "node"];
pub const CVI_VALUES: [&str; 4] = ["param", "forward", "atm", "skew"];
pub const CVI_ATTRIBUTES: [&str; 2] = ["anchor_date", "spot_ref"];
const DATE: &str = "%Y-%m-%d";

/// One expiry's chain: ascending strikes, `bid`/`ask` NaN where the quote
/// is one-sided, `as_of` the quote's own date (time to expiry runs from it).
#[derive(Debug, Clone, PartialEq)]
pub struct ChainExpiry {
    pub expiry: NaiveDate,
    pub as_of: NaiveDate,
    pub forward: f64,
    pub strikes: Vec<f64>,
    pub bid: Vec<f64>,
    pub mid: Vec<f64>,
    pub ask: Vec<f64>,
}

fn f64_at(s: &Snapshot, column: &str, row: usize) -> Result<f64, String> {
    s.f64_value(column, row)
        .ok_or_else(|| format!("{column} is missing at row {row}"))
}

/// A date cell whether it arrived typed (`Date32`) or as text.
fn date_at(s: &Snapshot, column: &str, row: usize) -> Result<NaiveDate, String> {
    let text = s
        .display_value(column, row)
        .ok_or_else(|| format!("{column} is missing at row {row}"))?;
    NaiveDate::parse_from_str(&text, DATE).map_err(|_| format!("{column} '{text}' is not a date"))
}

/// The CVI document a full-key snapshot carries; `None` when it carries
/// no rows (no document under the key).
pub fn cvi_rows(s: &Snapshot) -> Result<Option<DocumentRows>, String> {
    let n = s.rows();
    if n == 0 {
        return Ok(None);
    }
    let underlying = s
        .text_value(geode_core::link::UNDERLYING, 0)
        .ok_or("underlying_ref is missing")?
        .to_string();
    let attributes = vec![
        (
            "anchor_date".to_string(),
            Value::Date(date_at(s, "anchor_date", 0)?),
        ),
        (
            "spot_ref".to_string(),
            Value::F64(f64_at(s, "spot_ref", 0)?),
        ),
    ];
    let terms = (0..n)
        .map(|r| date_at(s, "term", r))
        .collect::<Result<Vec<_>, _>>()?;
    let nodes = (0..n)
        .map(|r| f64_at(s, "node", r))
        .collect::<Result<Vec<_>, _>>()?;
    let values = CVI_VALUES
        .iter()
        .map(|c| {
            (0..n)
                .map(|r| f64_at(s, c, r))
                .collect::<Result<Vec<_>, _>>()
                .map(|v| (c.to_string(), Column::F64(v)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(DocumentRows {
        key: vec![underlying],
        attributes,
        axes: vec![
            ("term".to_string(), Column::Date(terms)),
            ("node".to_string(), Column::F64(nodes)),
        ],
        values,
    }))
}

/// A CVI document's terms, ascending and distinct.
pub fn terms(doc: &DocumentRows) -> Vec<NaiveDate> {
    let Some((_, Column::Date(t))) = doc.axes.iter().find(|(n, _)| n == "term") else {
        return Vec::new();
    };
    let mut t = t.clone();
    t.sort();
    t.dedup();
    t
}

/// Every chain a prefix snapshot carries, one per expiry, ascending, none
/// before `today`. The query orders rows by the open key part, then
/// strike, so an expiry's rows are one run; a second run is refused rather
/// than merged, since it would mean two documents under one key.
pub fn chain_expiries(s: &Snapshot, today: NaiveDate) -> Result<Vec<ChainExpiry>, String> {
    let mut out: Vec<ChainExpiry> = Vec::new();
    for r in 0..s.rows() {
        let text = s.text_value("expiry", r).ok_or("expiry is missing")?;
        let expiry = NaiveDate::parse_from_str(text, DATE)
            .map_err(|_| format!("expiry '{text}' is not a date"))?;
        if out.last().is_none_or(|c| c.expiry != expiry) {
            if out.iter().any(|c| c.expiry == expiry) {
                return Err(format!("option_chain rows for {expiry} are not contiguous"));
            }
            let as_of = s
                .text_value("quote_time", r)
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map_or(today, |t| t.date_naive());
            out.push(ChainExpiry {
                expiry,
                as_of,
                forward: f64_at(s, "forward", r)?,
                strikes: Vec::new(),
                bid: Vec::new(),
                mid: Vec::new(),
                ask: Vec::new(),
            });
        }
        let c = out.last_mut().expect("pushed above");
        c.strikes.push(f64_at(s, "strike", r)?);
        c.mid.push(f64_at(s, "mid_vol", r)?);
        c.bid.push(s.f64_value("bid_vol", r).unwrap_or(f64::NAN));
        c.ask.push(s.f64_value("ask_vol", r).unwrap_or(f64::NAN));
    }
    out.retain(|c| c.expiry >= today);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn meta(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        }
    }

    fn leak(v: Vec<String>) -> Vec<Option<&'static str>> {
        v.into_iter()
            .map(|s| Some(&*Box::leak(s.into_boxed_str())))
            .collect()
    }

    /// Two terms, two nodes each.
    fn cvi_snapshot() -> Snapshot {
        let n = 4;
        Snapshot::for_tests(
            vec![
                (
                    meta("underlying_ref"),
                    TestColumn::Str(vec![Some("SPX.Z"); n]),
                ),
                (
                    meta("term"),
                    TestColumn::Date(vec![
                        Some(d("2026-11-20")),
                        Some(d("2026-11-20")),
                        Some(d("2026-10-16")),
                        Some(d("2026-10-16")),
                    ]),
                ),
                (
                    meta("node"),
                    TestColumn::F64(vec![Some(-10.0), Some(10.0), Some(-10.0), Some(10.0)]),
                ),
                (meta("param"), TestColumn::F64(vec![Some(0.0); n])),
                (meta("forward"), TestColumn::F64(vec![Some(100.0); n])),
                (meta("atm"), TestColumn::F64(vec![Some(0.2); n])),
                (meta("skew"), TestColumn::F64(vec![Some(-0.1); n])),
                (
                    meta("anchor_date"),
                    TestColumn::Date(vec![Some(d("2026-10-01")); n]),
                ),
                (meta("spot_ref"), TestColumn::F64(vec![Some(99.0); n])),
            ],
            0,
        )
    }

    #[test]
    fn a_cvi_snapshot_becomes_the_document_the_model_reads() {
        let rows = cvi_rows(&cvi_snapshot()).unwrap().unwrap();
        assert_eq!(rows.key, vec!["SPX.Z".to_string()]);
        assert_eq!(rows.rows(), 4);
        assert_eq!(rows.axes[0].0, "term");
        assert_eq!(
            rows.values
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            CVI_VALUES
        );
        assert_eq!(
            rows.attributes[0],
            ("anchor_date".to_string(), Value::Date(d("2026-10-01")))
        );
        assert_eq!(
            terms(&rows),
            vec![d("2026-10-16"), d("2026-11-20")],
            "sorted, deduplicated"
        );
    }

    #[test]
    fn an_empty_cvi_snapshot_is_no_document() {
        let empty = Snapshot::for_tests(vec![(meta("underlying_ref"), TestColumn::Str(vec![]))], 0);
        assert_eq!(cvi_rows(&empty).unwrap(), None);
    }

    #[test]
    fn the_cvi_layout_is_the_kinds() {
        use geode_core::document::DocumentKind;
        let kind: Vec<&str> = geode_documents::cvi::CviKind
            .columns()
            .iter()
            .map(|(n, _)| *n)
            .collect();
        let ours: Vec<&str> = ["underlying_ref"]
            .into_iter()
            .chain(CVI_AXES)
            .chain(CVI_VALUES)
            .chain(CVI_ATTRIBUTES)
            .collect();
        assert_eq!(
            ours, kind,
            "document_columns order: key, axes, values, attributes"
        );
    }

    fn chain_snapshot(
        expiries: &[(&str, &[f64])],
        quote_time: &str,
        one_sided_first: bool,
    ) -> Snapshot {
        let mut exp = Vec::new();
        let mut strike = Vec::new();
        let (mut bid, mut mid, mut ask) = (Vec::new(), Vec::new(), Vec::new());
        for (e, ks) in expiries {
            for (i, k) in ks.iter().enumerate() {
                exp.push(e.to_string());
                strike.push(Some(*k));
                bid.push(if one_sided_first && i == 0 {
                    Some(f64::NAN)
                } else {
                    Some(0.19)
                });
                mid.push(Some(0.20));
                ask.push(Some(0.21));
            }
        }
        let n = exp.len();
        Snapshot::for_tests(
            vec![
                (
                    meta("underlying_ref"),
                    TestColumn::Str(vec![Some("SPX.Z"); n]),
                ),
                (meta("expiry"), TestColumn::Str(leak(exp))),
                (meta("strike"), TestColumn::F64(strike)),
                (meta("bid_vol"), TestColumn::F64(bid)),
                (meta("ask_vol"), TestColumn::F64(ask)),
                (meta("mid_vol"), TestColumn::F64(mid)),
                (meta("forward"), TestColumn::F64(vec![Some(100.0); n])),
                (
                    meta("quote_time"),
                    TestColumn::Str(leak(vec![quote_time.to_string(); n])),
                ),
            ],
            0,
        )
    }

    #[test]
    fn a_prefix_snapshot_splits_into_one_chain_per_expiry_and_drops_the_past() {
        let s = chain_snapshot(
            &[
                ("2026-09-18", &[90.0, 100.0]),
                ("2026-10-16", &[90.0, 100.0, 110.0]),
                ("2026-11-20", &[100.0]),
            ],
            "2026-10-02T15:30:00-04:00",
            true,
        );
        let c = chain_expiries(&s, d("2026-10-02")).unwrap();
        assert_eq!(
            c.iter().map(|c| c.expiry).collect::<Vec<_>>(),
            vec![d("2026-10-16"), d("2026-11-20")]
        );
        assert_eq!(c[0].strikes, vec![90.0, 100.0, 110.0]);
        assert_eq!(c[0].as_of, d("2026-10-02"), "the quote's own date");
        assert!(
            c[0].bid[0].is_nan() && c[0].mid[0] == 0.20,
            "a one-sided quote keeps its NaN side"
        );
    }

    #[test]
    fn an_unparseable_quote_time_measures_from_today() {
        let s = chain_snapshot(&[("2026-10-16", &[100.0])], "garbage", false);
        assert_eq!(
            chain_expiries(&s, d("2026-10-02")).unwrap()[0].as_of,
            d("2026-10-02")
        );
    }

    #[test]
    fn an_expiry_split_across_runs_is_refused() {
        let s = chain_snapshot(
            &[
                ("2026-10-16", &[90.0]),
                ("2026-11-20", &[90.0]),
                ("2026-10-16", &[100.0]),
            ],
            "2026-10-02T15:30:00Z",
            false,
        );
        assert_eq!(
            chain_expiries(&s, d("2026-10-02")).unwrap_err(),
            "option_chain rows for 2026-10-16 are not contiguous"
        );
    }
}
