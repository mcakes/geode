//! The storage row shape (line-pricer spec §7.2): a sheet as one document
//! of the `pricer_sheets` dataset — one row per line or package row, the
//! sheet-wide settings as document attributes. Only definitions are
//! stored (spec §1.2): a reopened sheet reprices.
//!
//! Values have no NULL, so every optional is a flag plus a value or a
//! kind plus a value. Part 4 wires the dataset into the builtin layer and
//! the `SheetStore` around `DataHandle`; this module is the pure pair.

use crate::core::sheet::{LineId, LineState, OwnShifts, Refresh, RowKind, RowRecord, Sheet};
use crate::core::template::Template;
use chrono::NaiveDate;
use geode_core::document::{Column, DocumentRows, Value};
use geode_core::pricing::{
    Barrier, BarrierKind, Expiry, Instrument, MarketOverrides, OptionKind, Strike, Vanilla,
};
use geode_core::source_config::parse_duration;

pub const PRICER_SHEETS_DATASET: &str = "pricer_sheets";
pub const SHEET_KEY: &str = "sheet";
pub const LINE_AXIS: &str = "line";

/// The datasets-doc declaration (spec §7.2), one `[pricer_sheets.columns.<name>]`
/// table per column. Part 4 pushes it into `ConfigSources.builtin`.
pub const PRICER_SHEETS_DECLARATION: &str = r#"[pricer_sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]

[pricer_sheets.columns.sheet]
type = "utf8"
role = "dimension"
textual = true
[pricer_sheets.columns.line]
type = "i64"
role = "axis"

[pricer_sheets.columns.order]
type = "i64"
role = "value"
[pricer_sheets.columns.kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.template]
type = "utf8"
role = "value"
[pricer_sheets.columns.parent]
type = "i64"
role = "value"
[pricer_sheets.columns.qty]
type = "i64"
role = "value"
[pricer_sheets.columns.underlying]
type = "utf8"
role = "value"
[pricer_sheets.columns.expiry_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.expiry]
type = "utf8"
role = "value"
[pricer_sheets.columns.strike_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.strike]
type = "f64"
role = "value"
[pricer_sheets.columns.option_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.barrier_kind]
type = "utf8"
role = "value"
[pricer_sheets.columns.barrier]
type = "f64"
role = "value"
[pricer_sheets.columns.spot_shift_own]
type = "i64"
role = "value"
[pricer_sheets.columns.spot_shift]
type = "f64"
role = "value"
[pricer_sheets.columns.vol_shift_own]
type = "i64"
role = "value"
[pricer_sheets.columns.vol_shift]
type = "f64"
role = "value"

[pricer_sheets.columns.view]
type = "utf8"
role = "attribute"
[pricer_sheets.columns.sheet_spot_shift_own]
type = "i64"
role = "attribute"
[pricer_sheets.columns.sheet_spot_shift]
type = "f64"
role = "attribute"
[pricer_sheets.columns.sheet_vol_shift_own]
type = "i64"
role = "attribute"
[pricer_sheets.columns.sheet_vol_shift]
type = "f64"
role = "attribute"
[pricer_sheets.columns.refresh]
type = "utf8"
role = "attribute"
[pricer_sheets.columns.spot_overrides]
type = "utf8"
role = "attribute"
"#;

/// `NDX=20000.5;SPX=5100` in `BTreeMap` order; `""` when empty
/// (planning decision 6). Plain data, no arithmetic. Keys are
/// upper-case, as `Edit::SetSpotOverride` stores them.
pub fn encode_overrides(overrides: &MarketOverrides) -> String {
    overrides
        .spot
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
}

pub fn parse_overrides(text: &str) -> Result<MarketOverrides, String> {
    let mut out = MarketOverrides::default();
    if text.is_empty() {
        return Ok(out);
    }
    for pair in text.split(';') {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| format!("spot_overrides: '{pair}' is not UNDERLYING=LEVEL"))?;
        if k.is_empty() {
            return Err(format!("spot_overrides: '{pair}' has no underlying"));
        }
        let level: f64 = v
            .parse()
            .map_err(|_| format!("spot_overrides: '{v}' is not a number"))?;
        // `SetSpotOverride` upper-cases its underlying; a document
        // hand-edited in lower case must key the same way or the
        // override would never match an instrument.
        out.spot.insert(k.to_ascii_uppercase(), level);
    }
    Ok(out)
}

/// `""` | `off` | `<secs>s`, or `<millis>ms` where the interval has a
/// sub-second remainder (spec §7.2): `{}s` alone would spell every
/// sub-second refresh `0s`, and `parse_duration` reads both units.
pub fn encode_refresh(refresh: Refresh) -> String {
    match refresh {
        Refresh::Default => String::new(),
        Refresh::Off => "off".to_string(),
        Refresh::Every(d) if d.subsec_nanos() != 0 => format!("{}ms", d.as_millis()),
        Refresh::Every(d) => format!("{}s", d.as_secs()),
    }
}

pub fn parse_refresh(text: &str) -> Result<Refresh, String> {
    match text {
        "" => Ok(Refresh::Default),
        "off" => Ok(Refresh::Off),
        other => parse_duration(other)
            .map(Refresh::Every)
            .ok_or_else(|| format!("refresh: '{other}' is not a duration or 'off'")),
    }
}

fn expiry_parts(e: &Expiry) -> (&'static str, String) {
    match e {
        Expiry::Date(d) => ("date", d.format("%Y-%m-%d").to_string()),
        Expiry::Tenor(t) => ("tenor", t.clone()),
    }
}

fn barrier_name(k: BarrierKind) -> &'static str {
    match k {
        BarrierKind::UpIn => "ui",
        BarrierKind::UpOut => "uo",
        BarrierKind::DownIn => "di",
        BarrierKind::DownOut => "do",
    }
}

fn own_pair(v: Option<f64>) -> (i64, f64) {
    match v {
        Some(x) => (1, x),
        None => (0, 0.0),
    }
}

/// The document for a sheet, or `None` when it has no rows: a zero-row
/// document is refused by the store, and the last non-empty generation
/// stays as history (spec §7.2).
pub fn to_rows(sheet: &Sheet) -> Option<DocumentRows> {
    if sheet.is_empty() {
        return None;
    }
    let n = sheet.len();
    let mut line = Vec::with_capacity(n);
    let mut order = Vec::with_capacity(n);
    let mut kind = Vec::with_capacity(n);
    let mut template = Vec::with_capacity(n);
    let mut parent = Vec::with_capacity(n);
    let mut qty = Vec::with_capacity(n);
    let mut underlying = Vec::with_capacity(n);
    let mut expiry_kind = Vec::with_capacity(n);
    let mut expiry = Vec::with_capacity(n);
    let mut strike_kind = Vec::with_capacity(n);
    let mut strike = Vec::with_capacity(n);
    let mut option_kind = Vec::with_capacity(n);
    let mut barrier_kind = Vec::with_capacity(n);
    let mut barrier = Vec::with_capacity(n);
    let mut spot_own = Vec::with_capacity(n);
    let mut spot = Vec::with_capacity(n);
    let mut vol_own = Vec::with_capacity(n);
    let mut vol = Vec::with_capacity(n);
    for row in 0..n {
        line.push(sheet.id(row).0 as i64);
        order.push(row as i64);
        let (k, t) = match sheet.kind(row) {
            RowKind::Line => ("line", ""),
            RowKind::Package { template } => ("package", template.storage_name()),
            RowKind::Underlying => ("underlying", ""),
        };
        kind.push(k.to_string());
        template.push(t.to_string());
        parent.push(sheet.parent(row).map_or(-1, |p| sheet.id(p).0 as i64));
        qty.push(sheet.qty(row));
        match sheet.instrument(row) {
            Some(i) => {
                let v = i.vanilla();
                underlying.push(v.underlying.clone());
                let (ek, e) = expiry_parts(&v.expiry);
                expiry_kind.push(ek.to_string());
                expiry.push(e);
                let (sk, s) = match v.strike {
                    Strike::Absolute(k) => ("abs", k),
                    Strike::Percent(p) => ("pct", p),
                };
                strike_kind.push(sk.to_string());
                strike.push(s);
                option_kind.push(
                    match v.kind {
                        OptionKind::Call => "call",
                        OptionKind::Put => "put",
                    }
                    .to_string(),
                );
                match i {
                    Instrument::Barrier(b) => {
                        barrier_kind.push(barrier_name(b.barrier).to_string());
                        barrier.push(b.level);
                    }
                    Instrument::Vanilla(_) => {
                        barrier_kind.push(String::new());
                        barrier.push(0.0);
                    }
                }
            }
            None => {
                underlying.push(String::new());
                expiry_kind.push(String::new());
                expiry.push(String::new());
                strike_kind.push(String::new());
                strike.push(0.0);
                option_kind.push(String::new());
                barrier_kind.push(String::new());
                barrier.push(0.0);
            }
        }
        let sh = sheet.shift(row);
        let (so, s) = own_pair(sh.spot_pct);
        let (vo, v) = own_pair(sh.vol_pts);
        spot_own.push(so);
        spot.push(s);
        vol_own.push(vo);
        vol.push(v);
    }
    let (sso, ss) = own_pair(sheet.sheet_shift().spot_pct);
    let (svo, sv) = own_pair(sheet.sheet_shift().vol_pts);
    Some(DocumentRows {
        key: vec![sheet.name.clone()],
        attributes: vec![
            ("view".into(), Value::Utf8(sheet.view.clone())),
            ("sheet_spot_shift_own".into(), Value::I64(sso)),
            ("sheet_spot_shift".into(), Value::F64(ss)),
            ("sheet_vol_shift_own".into(), Value::I64(svo)),
            ("sheet_vol_shift".into(), Value::F64(sv)),
            ("refresh".into(), Value::Utf8(encode_refresh(sheet.refresh))),
            (
                "spot_overrides".into(),
                Value::Utf8(encode_overrides(sheet.overrides())),
            ),
        ],
        axes: vec![(LINE_AXIS.into(), Column::I64(line))],
        values: vec![
            ("order".into(), Column::I64(order)),
            ("kind".into(), Column::Utf8(kind)),
            ("template".into(), Column::Utf8(template)),
            ("parent".into(), Column::I64(parent)),
            ("qty".into(), Column::I64(qty)),
            ("underlying".into(), Column::Utf8(underlying)),
            ("expiry_kind".into(), Column::Utf8(expiry_kind)),
            ("expiry".into(), Column::Utf8(expiry)),
            ("strike_kind".into(), Column::Utf8(strike_kind)),
            ("strike".into(), Column::F64(strike)),
            ("option_kind".into(), Column::Utf8(option_kind)),
            ("barrier_kind".into(), Column::Utf8(barrier_kind)),
            ("barrier".into(), Column::F64(barrier)),
            ("spot_shift_own".into(), Column::I64(spot_own)),
            ("spot_shift".into(), Column::F64(spot)),
            ("vol_shift_own".into(), Column::I64(vol_own)),
            ("vol_shift".into(), Column::F64(vol)),
        ],
    })
}

fn utf8<'a>(rows: &'a DocumentRows, name: &str) -> Result<&'a [String], String> {
    match rows.values.iter().find(|(n, _)| n == name) {
        Some((_, Column::Utf8(v))) => Ok(v),
        Some(_) => Err(format!("value '{name}' is not utf8")),
        None => Err(format!("value '{name}' is missing")),
    }
}

fn i64s<'a>(rows: &'a DocumentRows, name: &str) -> Result<&'a [i64], String> {
    match rows.values.iter().find(|(n, _)| n == name) {
        Some((_, Column::I64(v))) => Ok(v),
        Some(_) => Err(format!("value '{name}' is not i64")),
        None => Err(format!("value '{name}' is missing")),
    }
}

fn f64s<'a>(rows: &'a DocumentRows, name: &str) -> Result<&'a [f64], String> {
    match rows.values.iter().find(|(n, _)| n == name) {
        Some((_, Column::F64(v))) => Ok(v),
        Some(_) => Err(format!("value '{name}' is not f64")),
        None => Err(format!("value '{name}' is missing")),
    }
}

fn attr<'a>(rows: &'a DocumentRows, name: &str) -> Result<&'a Value, String> {
    rows.attributes
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
        .ok_or_else(|| format!("attribute '{name}' is missing"))
}

fn attr_utf8(rows: &DocumentRows, name: &str) -> Result<String, String> {
    match attr(rows, name)? {
        Value::Utf8(s) => Ok(s.clone()),
        _ => Err(format!("attribute '{name}' is not utf8")),
    }
}

fn attr_own(rows: &DocumentRows, flag: &str, value: &str) -> Result<Option<f64>, String> {
    let f = match attr(rows, flag)? {
        Value::I64(i) => *i,
        _ => return Err(format!("attribute '{flag}' is not i64")),
    };
    let v = match attr(rows, value)? {
        Value::F64(x) => *x,
        _ => return Err(format!("attribute '{value}' is not f64")),
    };
    Ok((f != 0).then_some(v))
}

fn own(flag: i64, value: f64) -> Option<f64> {
    (flag != 0).then_some(value)
}

/// A sheet from its document. Rows are taken in `order`; a leg's parent
/// is resolved by id, must be a package earlier in that order, and the
/// legs of one package must follow it CONTIGUOUSLY (the sheet's own
/// invariant — `children` is a scan of the following rows). Every line
/// comes back `Stale`
/// at revision 1 with no result (results are not stored, spec §1.2), and
/// `next_id` continues past the highest stored id.
pub fn from_rows(name: &str, rows: &DocumentRows) -> Result<Sheet, String> {
    let ids = match rows.axes.iter().find(|(n, _)| n == LINE_AXIS) {
        Some((_, Column::I64(v))) => v,
        Some(_) => return Err(format!("axis '{LINE_AXIS}' is not i64")),
        None => return Err(format!("axis '{LINE_AXIS}' is missing")),
    };
    let order = i64s(rows, "order")?;
    let kind = utf8(rows, "kind")?;
    let template = utf8(rows, "template")?;
    let parent = i64s(rows, "parent")?;
    let qty = i64s(rows, "qty")?;
    let underlying = utf8(rows, "underlying")?;
    let expiry_kind = utf8(rows, "expiry_kind")?;
    let expiry = utf8(rows, "expiry")?;
    let strike_kind = utf8(rows, "strike_kind")?;
    let strike = f64s(rows, "strike")?;
    let option_kind = utf8(rows, "option_kind")?;
    let barrier_kind = utf8(rows, "barrier_kind")?;
    let barrier = f64s(rows, "barrier")?;
    let spot_own = i64s(rows, "spot_shift_own")?;
    let spot = f64s(rows, "spot_shift")?;
    let vol_own = i64s(rows, "vol_shift_own")?;
    let vol = f64s(rows, "vol_shift")?;
    let n = rows.rows();
    for (label, len) in [
        ("order", order.len()),
        ("kind", kind.len()),
        ("template", template.len()),
        ("parent", parent.len()),
        ("qty", qty.len()),
        ("underlying", underlying.len()),
        ("expiry_kind", expiry_kind.len()),
        ("expiry", expiry.len()),
        ("strike_kind", strike_kind.len()),
        ("strike", strike.len()),
        ("option_kind", option_kind.len()),
        ("barrier_kind", barrier_kind.len()),
        ("barrier", barrier.len()),
        ("spot_shift_own", spot_own.len()),
        ("spot_shift", spot.len()),
        ("vol_shift_own", vol_own.len()),
        ("vol_shift", vol.len()),
    ] {
        if len != n {
            return Err(format!("value '{label}' has {len} rows, the axis has {n}"));
        }
    }

    let mut sheet = Sheet::new(name);
    sheet.view = attr_utf8(rows, "view")?;
    sheet.sheet_shift = OwnShifts {
        spot_pct: attr_own(rows, "sheet_spot_shift_own", "sheet_spot_shift")?,
        vol_pts: attr_own(rows, "sheet_vol_shift_own", "sheet_vol_shift")?,
    };
    sheet.refresh = parse_refresh(&attr_utf8(rows, "refresh")?)?;
    sheet.overrides = parse_overrides(&attr_utf8(rows, "spot_overrides")?)?;

    let mut by_order: Vec<usize> = (0..n).collect();
    by_order.sort_by_key(|i| order[*i]);
    let mut seen: Vec<LineId> = Vec::with_capacity(n);
    let mut records: Vec<RowRecord> = Vec::with_capacity(n);
    for i in by_order {
        let id = u64::try_from(ids[i]).map_err(|_| format!("line id {} is negative", ids[i]))?;
        let id = LineId(id);
        if seen.contains(&id) {
            return Err(format!("line id {} appears twice", id.0));
        }
        seen.push(id);
        let parent_id = if parent[i] < 0 {
            None
        } else {
            let pid = LineId(parent[i] as u64);
            if !records
                .iter()
                .any(|r| r.id == pid && matches!(r.kind, RowKind::Package { .. }))
            {
                return Err(format!(
                    "line {} names parent {} which is not a package before it",
                    id.0, pid.0
                ));
            }
            // The sheet's invariant is that a package's legs are the
            // CONTIGUOUS run after it (`Sheet::children` is a scan, not
            // an index): a leg whose predecessor is neither its package
            // nor another of its legs would land in neither `roots()`
            // nor `children(package)` once `reindex_parents` ran.
            let follows = records
                .last()
                .is_some_and(|prev| prev.id == pid || prev.parent == Some(pid));
            if !follows {
                return Err(format!(
                    "line {}: legs of package {} must follow it contiguously",
                    id.0, pid.0
                ));
            }
            Some(pid)
        };
        let row_kind = match kind[i].as_str() {
            "line" => RowKind::Line,
            "package" => RowKind::Package {
                template: Template::parse(&template[i])
                    .ok_or_else(|| format!("line {}: unknown template '{}'", id.0, template[i]))?,
            },
            "underlying" => {
                return Err(format!(
                    "line {}: kind 'underlying' is reserved and not readable by this build",
                    id.0
                ));
            }
            other => return Err(format!("line {}: unknown kind '{}'", id.0, other)),
        };
        let instrument = match row_kind {
            RowKind::Line => Some(
                read_instrument(
                    &underlying[i],
                    &expiry_kind[i],
                    &expiry[i],
                    &strike_kind[i],
                    strike[i],
                    &option_kind[i],
                    &barrier_kind[i],
                    barrier[i],
                )
                .map_err(|m| format!("line {}: {m}", id.0))?,
            ),
            _ => None,
        };
        if qty[i] == 0 && row_kind == RowKind::Line {
            return Err(format!("line {}: quantity is zero", id.0));
        }
        records.push(RowRecord {
            id,
            kind: row_kind,
            parent: parent_id,
            instrument,
            qty: qty[i],
            shift: OwnShifts {
                spot_pct: own(spot_own[i], spot[i]),
                vol_pts: own(vol_own[i], vol[i]),
            },
            revision: 1,
            result: None,
            state: if row_kind == RowKind::Line {
                LineState::Stale
            } else {
                LineState::Fresh
            },
            priced_at: None,
        });
    }
    if !records.is_empty() {
        sheet
            .apply(crate::core::edit::Edit::Restore {
                at: 0,
                rows: records,
            })
            .map_err(|e| format!("could not rebuild the sheet: {e}"))?;
    }
    Ok(sheet)
}

#[allow(clippy::too_many_arguments)]
fn read_instrument(
    underlying: &str,
    expiry_kind: &str,
    expiry: &str,
    strike_kind: &str,
    strike: f64,
    option_kind: &str,
    barrier_kind: &str,
    barrier: f64,
) -> Result<Instrument, String> {
    if underlying.is_empty() {
        return Err("a line needs an underlying".into());
    }
    let expiry = match expiry_kind {
        "date" => Expiry::Date(
            NaiveDate::parse_from_str(expiry, "%Y-%m-%d")
                .map_err(|_| format!("expiry '{expiry}' is not a date"))?,
        ),
        "tenor" => Expiry::tenor(expiry).map_err(|m| format!("expiry: {m}"))?,
        other => return Err(format!("expiry_kind '{other}' is not date or tenor")),
    };
    let strike = match strike_kind {
        "abs" => Strike::Absolute(strike),
        "pct" => Strike::Percent(strike),
        other => return Err(format!("strike_kind '{other}' is not abs or pct")),
    };
    let kind = match option_kind {
        "call" => OptionKind::Call,
        "put" => OptionKind::Put,
        other => return Err(format!("option_kind '{other}' is not call or put")),
    };
    let vanilla = Vanilla {
        underlying: underlying.to_string(),
        expiry,
        strike,
        kind,
    };
    let barrier_kind = match barrier_kind {
        "" => None,
        "ui" => Some(BarrierKind::UpIn),
        "uo" => Some(BarrierKind::UpOut),
        "di" => Some(BarrierKind::DownIn),
        "do" => Some(BarrierKind::DownOut),
        other => return Err(format!("barrier_kind '{other}' is not one of ui uo di do")),
    };
    Ok(match barrier_kind {
        None => Instrument::Vanilla(vanilla),
        Some(b) => Instrument::Barrier(Barrier {
            vanilla,
            level: barrier,
            barrier: b,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::sheet::{LineState, OwnShifts, Refresh, Sheet};
    use crate::core::shorthand::parse;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::pricing::OptionKind;
    use geode_core::schema::SchemaSpec;
    use std::time::Duration;

    fn dataset() -> geode_core::schema::DatasetSpec {
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", PRICER_SHEETS_DECLARATION).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema
            .dataset(PRICER_SHEETS_DATASET)
            .expect("declared")
            .clone()
    }

    /// Every template, both variants, every shift state, a sheet-wide
    /// shift, an override, a refresh — the round-trip fixture.
    fn full_sheet() -> Sheet {
        let mut s = Sheet::new("book-1");
        s.view = "barrier".into();
        let mut rows = vec![
            line(spx(5000.0, OptionKind::Call), 2),
            parse("-3 SPX 20DEC26 5000 P DO 4200").unwrap(),
            parse("NDX 3m 95% C UI 110").unwrap(),
        ];
        for text in [
            "-5 SPX Z26 95%/105% CS",
            "SPX Z26 4800/5200 PS",
            "2 SPX Z26 5000 STRD",
            "SPX Z26 4800/5200 STRG",
            "SPX Z26 4800/5200 RR",
            "3 SPX Z26 4800/5000/5200 FLY",
            "SPX Z26/H27 5000 CAL",
        ] {
            rows.push(parse(text).unwrap());
        }
        push(&mut s, rows);
        s.apply(Edit::SetShift {
            row: 0,
            shift: OwnShifts {
                spot_pct: Some(1.5),
                vol_pts: None,
            },
        })
        .unwrap();
        s.apply(Edit::SetShift {
            row: 1,
            shift: OwnShifts {
                spot_pct: None,
                vol_pts: Some(-2.0),
            },
        })
        .unwrap();
        s.apply(Edit::SetShift {
            row: 2,
            shift: OwnShifts {
                spot_pct: Some(0.0),
                vol_pts: Some(0.0),
            },
        })
        .unwrap();
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: None,
            vol_pts: Some(1.0),
        }))
        .unwrap();
        s.apply(Edit::SetSpotOverride {
            underlying: "SPX".into(),
            level: Some(5100.0),
        })
        .unwrap();
        s.apply(Edit::SetSpotOverride {
            underlying: "NDX".into(),
            level: Some(20000.5),
        })
        .unwrap();
        s.refresh = Refresh::Every(Duration::from_secs(45));
        // An empty custom package too.
        push(&mut s, vec![callspread(1)]);
        let last = s.len() - 1;
        s.apply(Edit::Remove { at: last }).unwrap();
        s.apply(Edit::Remove { at: last - 1 }).unwrap();
        s
    }

    #[allow(clippy::type_complexity)]
    fn definition(
        s: &Sheet,
    ) -> Vec<(
        u64,
        crate::core::sheet::RowKind,
        Option<u64>,
        Option<geode_core::pricing::Instrument>,
        i64,
        OwnShifts,
    )> {
        (0..s.len())
            .map(|r| {
                (
                    s.id(r).0,
                    s.kind(r),
                    s.parent(r).map(|p| s.id(p).0),
                    s.instrument(r).cloned(),
                    s.qty(r),
                    s.shift(r),
                )
            })
            .collect()
    }

    #[test]
    fn the_declaration_parses_and_a_full_sheet_validates_against_it() {
        let ds = dataset();
        assert!(ds.local, "a local dataset (spec §7.2)");
        assert!(ds.is_document());
        assert_eq!(ds.key, vec![SHEET_KEY.to_string()]);
        assert_eq!(ds.axes, vec![LINE_AXIS.to_string()]);
        let rows = to_rows(&full_sheet()).expect("a sheet with rows");
        rows.validate(&ds).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(rows.key, vec!["book-1".to_string()]);
        assert_eq!(rows.rows(), full_sheet().len());
    }

    #[test]
    fn to_rows_then_from_rows_is_the_same_definition() {
        let s = full_sheet();
        let rows = to_rows(&s).unwrap();
        let back = from_rows("book-1", &rows).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.name, "book-1");
        assert_eq!(back.view, "barrier");
        assert_eq!(back.sheet_shift(), s.sheet_shift());
        assert_eq!(back.overrides(), s.overrides());
        assert_eq!(back.refresh, Refresh::Every(Duration::from_secs(45)));
        assert_eq!(definition(&back), definition(&s));
        // Results are not persisted (spec §1.2): every line is stale, rev 1, unpriced.
        for r in 0..back.len() {
            assert_eq!(back.revision(r), 1);
            assert_eq!(back.result(r), None);
            if back.is_line(r) {
                assert_eq!(back.state(r), &LineState::Stale);
            }
        }
        // The next id continues past the highest stored one.
        let mut back = back;
        let n = back.len();
        push(&mut back, vec![line(spx(1.0, OptionKind::Call), 1)]);
        assert!(back.id(n).0 > s.id(s.len() - 1).0);
        // And a second round trip is stable.
        assert_eq!(to_rows(&back).unwrap().rows(), n + 1);
    }

    #[test]
    fn an_empty_sheet_has_no_document() {
        let s = Sheet::new("empty");
        assert_eq!(to_rows(&s), None);
        // A sheet whose last line was removed publishes nothing either.
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(1.0, OptionKind::Call), 1)]);
        assert!(to_rows(&s).is_some());
        s.apply(Edit::Remove { at: 0 }).unwrap();
        assert_eq!(to_rows(&s), None);
    }

    #[test]
    fn overrides_and_refresh_encode_both_ways() {
        let mut o = geode_core::pricing::MarketOverrides::default();
        assert_eq!(encode_overrides(&o), "");
        assert_eq!(parse_overrides("").unwrap(), o);
        o.spot.insert("SPX".into(), 5100.0);
        o.spot.insert("NDX".into(), 20000.5);
        assert_eq!(encode_overrides(&o), "NDX=20000.5;SPX=5100");
        assert_eq!(parse_overrides("NDX=20000.5;SPX=5100").unwrap(), o);
        assert!(parse_overrides("SPX").is_err());
        assert!(parse_overrides("SPX=abc").is_err());
        assert!(parse_overrides("=5").is_err());
        // The key is upper-cased on the way in, as SetSpotOverride does.
        assert_eq!(
            parse_overrides("spx=5100").unwrap().spot.get("SPX"),
            Some(&5100.0)
        );
        assert_eq!(encode_refresh(Refresh::Default), "");
        assert_eq!(encode_refresh(Refresh::Off), "off");
        assert_eq!(
            encode_refresh(Refresh::Every(Duration::from_secs(30))),
            "30s"
        );
        assert_eq!(
            encode_refresh(Refresh::Every(Duration::from_secs(90))),
            "90s"
        );
        // A sub-second interval keeps its unit rather than spelling 0s.
        assert_eq!(
            encode_refresh(Refresh::Every(Duration::from_millis(500))),
            "500ms"
        );
        assert_eq!(
            parse_refresh("500ms").unwrap(),
            Refresh::Every(Duration::from_millis(500))
        );
        assert_eq!(parse_refresh("").unwrap(), Refresh::Default);
        assert_eq!(parse_refresh("off").unwrap(), Refresh::Off);
        assert_eq!(
            parse_refresh("30s").unwrap(),
            Refresh::Every(Duration::from_secs(30))
        );
        assert_eq!(
            parse_refresh("2m").unwrap(),
            Refresh::Every(Duration::from_secs(120))
        );
        assert!(parse_refresh("soon").is_err());
    }

    #[test]
    fn a_hostile_document_is_refused_with_a_reason() {
        let s = full_sheet();
        let good = to_rows(&s).unwrap();
        // A missing value column.
        let mut rows = good.clone();
        rows.values.retain(|(n, _)| n != "qty");
        assert!(from_rows("book-1", &rows).unwrap_err().contains("qty"));
        // A leg whose parent is not in the document.
        let mut rows = good.clone();
        if let Some((_, geode_core::document::Column::I64(parents))) =
            rows.values.iter_mut().find(|(n, _)| n == "parent")
        {
            parents[4] = 9999;
        }
        assert!(from_rows("book-1", &rows).unwrap_err().contains("parent"));
        // The reserved kind.
        let mut rows = good.clone();
        if let Some((_, geode_core::document::Column::Utf8(kinds))) =
            rows.values.iter_mut().find(|(n, _)| n == "kind")
        {
            kinds[0] = "underlying".into();
        }
        assert!(
            from_rows("book-1", &rows)
                .unwrap_err()
                .contains("underlying")
        );
        // A bad date.
        let mut rows = good.clone();
        if let Some((_, geode_core::document::Column::Utf8(exp))) =
            rows.values.iter_mut().find(|(n, _)| n == "expiry")
        {
            exp[0] = "2026-13-40".into();
        }
        assert!(from_rows("book-1", &rows).unwrap_err().contains("expiry"));
        // A repeated line id.
        let mut rows = good.clone();
        if let Some((_, geode_core::document::Column::I64(ids))) =
            rows.axes.iter_mut().find(|(n, _)| n == LINE_AXIS)
        {
            ids[1] = ids[0];
        }
        assert!(from_rows("book-1", &rows).unwrap_err().contains("line"));
        // Legs that are not contiguous after their package: row 2 is a
        // root line and row 5 the second leg of the callspread at row 3,
        // so this `order` reads package, leg, root line, leg — every id
        // resolves, and the sheet would still be malformed.
        let mut rows = good.clone();
        if let Some((_, geode_core::document::Column::I64(order))) =
            rows.values.iter_mut().find(|(n, _)| n == "order")
        {
            assert_eq!((order[2], order[3], order[5]), (2, 3, 5), "the fixture");
            order[2] = 100;
            order[5] = 101;
        }
        let e = from_rows("book-1", &rows).unwrap_err();
        assert!(e.contains("contiguously"), "{e}");
        // The good one still loads.
        assert!(from_rows("book-1", &good).is_ok());
    }
}
