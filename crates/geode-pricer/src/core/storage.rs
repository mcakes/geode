//! Pure conversion between a sheet and one document in the `pricer_sheets` dataset.
//! Each line or package occupies a row; document attributes hold sheet-wide settings.
//! Results are omitted, so reopened sheets reprice.
//!
//! Document values are non-NULL: optional values use flags or kind tags. `geode-app`
//! installs the dataset declaration, and `DuckSheetStore` performs I/O through
//! `DataHandle`.

use crate::core::sheet::{LineId, LineState, OwnShifts, Refresh, RowKind, RowRecord, Sheet};
use crate::core::template::Template;
use chrono::NaiveDate;
use geode_core::config::{LayerDoc, merge_docs};
use geode_core::document::{Column, DocumentRows, Value};
use geode_core::pricing::{
    Barrier, BarrierKind, Expiry, Instrument, MarketOverrides, OptionKind, Strike, Vanilla,
};
use geode_core::schema::{ColumnRole, ColumnType, DatasetSpec, SchemaSpec};
use geode_core::snapshot::Snapshot;
use geode_core::source_config::parse_duration;
use std::sync::OnceLock;

pub const PRICER_SHEETS_DATASET: &str = "pricer_sheets";
pub const SHEET_KEY: &str = "sheet";
pub const LINE_AXIS: &str = "line";

/// The datasets-doc declaration, one `[pricer_sheets.columns.<name>]`
/// table per column. `geode-app` pushes it into the builtin config layer.
///
/// **Frozen.** The store creates the tables with `CREATE TABLE IF NOT
/// EXISTS` and publishes insert positionally, so once a database holds
/// `pricer_sheets` its column list and order cannot change without a
/// migration, which does not exist: a changed declaration would write
/// values into the wrong columns of an existing database. Add, remove or
/// reorder a column only together with a migration.
///
/// `sheet` is `categorical = false`: a text dimension is categorical by
/// default, which would offer sheet names in the frame picker and the
/// groupings and rebuild an ENUM on every autosave. A sheet name is not a
/// scope dimension.
pub const PRICER_SHEETS_DECLARATION: &str = r#"[pricer_sheets]
family = "document"
local = true
key = ["sheet"]
axes = ["line"]

[pricer_sheets.columns.sheet]
type = "utf8"
role = "dimension"
textual = true
categorical = false
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

/// Encode overrides as `NDX=20000.5;SPX=5100` in `BTreeMap` order, or an empty string.
/// `Edit::SetSpotOverride` stores uppercase keys.
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
/// sub-second remainder: `{}s` alone would spell every
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
/// stays as history.
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
            RowKind::Line => ("line", String::new()),
            RowKind::Package { template } => ("package", template.storage_name()),
            RowKind::Underlying => ("underlying", String::new()),
        };
        kind.push(k.to_string());
        template.push(t);
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

/// Decode a sheet in stored `order`. Parent IDs must identify earlier packages, and
/// each package's legs must follow it contiguously. Loaded lines are stale at revision
/// 1 with no result; fresh ID allocation continues above the highest stored ID.
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
            // Legs must immediately follow their package or another of its legs.
            // Reindexing derives parents from row order, so accepting a separated
            // leg could hide it from its package or attach it to a different one.
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
            "package" => {
                let name = template[i].trim();
                if name.is_empty() {
                    return Err(format!("line {}: a package has no template", id.0));
                }
                // An unresolved template name still loads because its legs are
                // stored independently. Repricing uses those instruments; shorthand
                // prints each leg until a table of that name matches them.
                RowKind::Package {
                    template: Template::named(name),
                }
            }
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

/// The declaration, parsed once. The decoder walks THIS column list,
/// never the answer's: a column the answer lacks is an error naming it,
/// and a column the answer adds is ignored.
fn declared() -> Result<&'static DatasetSpec, String> {
    static DECLARED: OnceLock<Result<DatasetSpec, String>> = OnceLock::new();
    DECLARED
        .get_or_init(|| {
            let layer = LayerDoc::builtin("datasets", PRICER_SHEETS_DECLARATION)
                .map_err(|e| format!("the {PRICER_SHEETS_DATASET} declaration: {e:?}"))?;
            let (schema, diags) = SchemaSpec::from_doc(&merge_docs("datasets", &[layer]));
            if let Some(d) = diags.first() {
                return Err(format!("the {PRICER_SHEETS_DATASET} declaration: {d:?}"));
            }
            schema
                .dataset(PRICER_SHEETS_DATASET)
                .cloned()
                .ok_or_else(|| format!("{PRICER_SHEETS_DATASET} is not declared"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn column_present(snapshot: &Snapshot, name: &str) -> Result<(), String> {
    snapshot
        .column_index(name)
        .map(|_| ())
        .ok_or_else(|| format!("column '{name}' is missing from the answer"))
}

/// One declared column over every row of the answer. The type check is
/// the strict one (`i64_column`/`f64_column` match exactly Int64 and
/// Float64); every cell then goes through the per-row accessor, which
/// answers `None` for a NULL — a NULL is refused, never read as a zero.
fn decode_column(snapshot: &Snapshot, name: &str, ty: ColumnType) -> Result<Column, String> {
    column_present(snapshot, name)?;
    let n = snapshot.rows();
    let null = |row: usize| format!("column '{name}' is NULL at row {row}");
    match ty {
        ColumnType::Utf8 => {
            if snapshot.str_column(name).is_none() && snapshot.dict_column(name).is_none() {
                return Err(format!("column '{name}' is not utf8"));
            }
            (0..n)
                .map(|r| {
                    snapshot
                        .text_value(name, r)
                        .map(str::to_string)
                        .ok_or_else(|| null(r))
                })
                .collect::<Result<_, _>>()
                .map(Column::Utf8)
        }
        ColumnType::I64 => {
            if snapshot.i64_column(name).is_none() {
                return Err(format!("column '{name}' is not i64"));
            }
            (0..n)
                .map(|r| snapshot.i64_value(name, r).ok_or_else(|| null(r)))
                .collect::<Result<_, _>>()
                .map(Column::I64)
        }
        ColumnType::F64 => {
            if snapshot.f64_column(name).is_none() {
                return Err(format!("column '{name}' is not f64"));
            }
            (0..n)
                .map(|r| snapshot.f64_value(name, r).ok_or_else(|| null(r)))
                .collect::<Result<_, _>>()
                .map(Column::F64)
        }
        other => Err(format!(
            "column '{name}' is declared {other:?}, which a sheet never stores"
        )),
    }
}

/// An attribute is repeated on every row of the answer; it is read from
/// row 0 only after every row agrees. Rows that disagree are a store
/// fault, and picking one would be a plausible wrong sheet.
fn decode_attribute(snapshot: &Snapshot, name: &str, ty: ColumnType) -> Result<Value, String> {
    let differs = || format!("attribute '{name}' differs between rows");
    Ok(match decode_column(snapshot, name, ty)? {
        Column::Utf8(v) => {
            if v.iter().any(|x| *x != v[0]) {
                return Err(differs());
            }
            Value::Utf8(v[0].clone())
        }
        Column::I64(v) => {
            if v.iter().any(|x| *x != v[0]) {
                return Err(differs());
            }
            Value::I64(v[0])
        }
        Column::F64(v) => {
            if v.iter().any(|x| x.to_bits() != v[0].to_bits()) {
                return Err(differs());
            }
            Value::F64(v[0])
        }
        Column::Date(_) => unreachable!("decode_column never yields a date"),
    })
}

/// A document answer (`DataHandle::document` over `pricer_sheets`) back
/// into the rows `to_rows` produced — the inverse the store's read path
/// needs before `from_rows`.
///
/// `Ok(None)` for a zero-row answer: no document under the name (a live
/// read of an unknown key answers empty, not an error). Otherwise every
/// declared column must be present, of its declared type and NULL-free,
/// and the key column must name `name` on every row; anything else is an
/// `Err` naming the column, never a partial document. Rows keep the
/// answer's order (by `line`); `from_rows` orders them by `order`.
pub fn rows_from_snapshot(name: &str, snapshot: &Snapshot) -> Result<Option<DocumentRows>, String> {
    let ds = declared()?;
    if snapshot.rows() == 0 {
        return Ok(None);
    }
    let mut out = DocumentRows {
        key: vec![name.to_string()],
        attributes: Vec::new(),
        axes: Vec::new(),
        values: Vec::new(),
    };
    for spec in ds.document_columns() {
        if ds.key.contains(&spec.name) {
            // A misrouted answer must not install as this sheet.
            match decode_column(snapshot, &spec.name, spec.ty)? {
                Column::Utf8(v) if v.iter().all(|k| k == name) => continue,
                _ => {
                    return Err(format!(
                        "column '{}' does not name sheet '{name}' on every row",
                        spec.name
                    ));
                }
            }
        }
        match spec.role {
            ColumnRole::Attribute { .. } => out.attributes.push((
                spec.name.clone(),
                decode_attribute(snapshot, &spec.name, spec.ty)?,
            )),
            ColumnRole::Axis => out.axes.push((
                spec.name.clone(),
                decode_column(snapshot, &spec.name, spec.ty)?,
            )),
            ColumnRole::Value => out.values.push((
                spec.name.clone(),
                decode_column(snapshot, &spec.name, spec.ty)?,
            )),
            _ => {
                return Err(format!(
                    "column '{}' has a role a sheet document never declares",
                    spec.name
                ));
            }
        }
    }
    Ok(Some(out))
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
pub(crate) mod tests {
    use super::*;
    use crate::core::edit::Edit;
    use crate::core::sheet::tests::{callspread, line, push, spx};
    use crate::core::sheet::{LineState, OwnShifts, Refresh, Sheet};
    use crate::core::shorthand::parse_builtin;
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::pricing::OptionKind;
    use geode_core::schema::SchemaSpec;
    use std::sync::Arc;
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
            parse_builtin("-3 SPX 20DEC26 5000 P DO 4200").unwrap(),
            parse_builtin("NDX 3m 95% C UI 110").unwrap(),
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
            rows.push(parse_builtin(text).unwrap());
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
        assert!(ds.local, "a local dataset");
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
        // Results are not persisted: every line is stale, rev 1, unpriced.
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

    // ---- the snapshot decoder -------------------------------------------

    use geode_core::attribution::{Attribution, ScopeSemantics};
    use geode_core::snapshot::{ColumnMeta, Snapshot, TestColumn};

    fn meta(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::DeterminedNonAdditive],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        }
    }

    fn test_column(col: &Column) -> TestColumn {
        match col {
            Column::F64(v) => TestColumn::F64(v.iter().map(|x| Some(*x)).collect()),
            Column::I64(v) => TestColumn::I64(v.clone()),
            Column::Utf8(v) => TestColumn::Dict(v.iter().map(|s| Some(s.clone())).collect()),
            Column::Date(_) => unreachable!("the declaration has no date column"),
        }
    }

    /// `doc` in the shape a document answer arrives in —
    /// `document_columns()` order, every attribute repeated on every row
    /// — with `edit` free to drop or retype a column before it is built.
    /// The tile's tests answer a load's `Delivery::Query` with it.
    pub(crate) fn snapshot_of(
        doc: &DocumentRows,
        edit: impl FnOnce(&mut Vec<(ColumnMeta, TestColumn)>),
    ) -> Snapshot {
        let n = doc.rows();
        let mut columns = vec![(
            meta(SHEET_KEY),
            TestColumn::Dict(vec![Some(doc.key.join("/")); n]),
        )];
        for (name, col) in doc.axes.iter().chain(doc.values.iter()) {
            columns.push((meta(name), test_column(col)));
        }
        for (name, value) in &doc.attributes {
            let col = match value {
                Value::F64(x) => TestColumn::F64(vec![Some(*x); n]),
                Value::I64(x) => TestColumn::I64(vec![*x; n]),
                Value::Utf8(s) => TestColumn::Dict(vec![Some(s.clone()); n]),
                Value::Date(_) => unreachable!("the declaration has no date attribute"),
            };
            columns.push((meta(name), col));
        }
        edit(&mut columns);
        Snapshot::for_tests(columns, 0)
    }

    type Columns = Vec<(ColumnMeta, TestColumn)>;

    fn replace(columns: &mut [(ColumnMeta, TestColumn)], name: &str, col: TestColumn) {
        let slot = columns
            .iter_mut()
            .find(|(m, _)| m.name == name)
            .expect("the fixture has the column");
        slot.1 = col;
    }

    #[test]
    fn the_declaration_is_clean_and_sheet_is_not_a_scope_dimension() {
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", PRICER_SHEETS_DECLARATION).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset(PRICER_SHEETS_DATASET).unwrap();
        // A categorical `sheet` would put sheet names into the frame
        // picker and groupings and rebuild an ENUM on every autosave.
        assert!(
            !ds.categorical_columns().contains(&SHEET_KEY),
            "{:?}",
            ds.categorical_columns()
        );
    }

    #[test]
    fn a_decoded_snapshot_is_the_document_it_was_built_from() {
        let s = full_sheet();
        let rows = to_rows(&s).unwrap();
        let decoded = rows_from_snapshot("book-1", &snapshot_of(&rows, |_| {}))
            .unwrap_or_else(|e| panic!("{e}"))
            .expect("rows");
        assert_eq!(decoded, rows);
    }

    #[test]
    fn a_zero_row_answer_is_no_document() {
        let rows = to_rows(&full_sheet()).unwrap();
        let empty = snapshot_of(&rows, |cols| {
            for (_, c) in cols.iter_mut() {
                *c = match c {
                    TestColumn::F64(_) => TestColumn::F64(Vec::new()),
                    TestColumn::I64(_) => TestColumn::I64(Vec::new()),
                    _ => TestColumn::Dict(Vec::new()),
                };
            }
        });
        assert_eq!(empty.rows(), 0);
        assert_eq!(rows_from_snapshot("book-1", &empty), Ok(None));
    }

    #[test]
    fn a_missing_or_wrong_typed_column_is_refused_by_name() {
        let rows = to_rows(&full_sheet()).unwrap();
        let n = rows.rows();
        let refused = |edit: &dyn Fn(&mut Columns)| {
            rows_from_snapshot("book-1", &snapshot_of(&rows, |c| edit(c)))
                .expect_err("refused, never a half-sheet")
        };
        // Missing, in each role.
        for name in ["qty", "line", "spot_overrides", "kind"] {
            let e = refused(&|c| c.retain(|(m, _)| m.name != name));
            assert!(e.contains(&format!("'{name}' is missing")), "{name}: {e}");
        }
        // An f64 where an i64 is declared (a value and an attribute).
        for name in ["qty", "sheet_vol_shift_own"] {
            let e = refused(&|c| replace(c, name, TestColumn::F64(vec![Some(1.0); n])));
            assert!(e.contains(&format!("'{name}' is not i64")), "{name}: {e}");
        }
        // An i64 where an f64 is declared.
        for name in ["strike", "sheet_spot_shift"] {
            let e = refused(&|c| replace(c, name, TestColumn::I64(vec![1; n])));
            assert!(e.contains(&format!("'{name}' is not f64")), "{name}: {e}");
        }
        // Numbers where text is declared.
        for name in ["kind", "refresh"] {
            let e = refused(&|c| replace(c, name, TestColumn::I64(vec![1; n])));
            assert!(e.contains(&format!("'{name}' is not utf8")), "{name}: {e}");
        }
        // A NULL cell is not a zero.
        let e = refused(&|c| {
            let mut v: Vec<Option<f64>> = vec![Some(1.0); n];
            v[1] = None;
            replace(c, "strike", TestColumn::F64(v))
        });
        assert!(e.contains("'strike'"), "{e}");
        // An attribute that differs between rows is a store fault, not a
        // choice of row 0.
        let e = refused(&|c| {
            let mut v = vec![Some("off".to_string()); n];
            v[n - 1] = Some("30s".into());
            replace(c, "refresh", TestColumn::Dict(v))
        });
        assert!(e.contains("'refresh'"), "{e}");
        // An answer for another sheet.
        let e = rows_from_snapshot("book-2", &snapshot_of(&rows, |_| {})).unwrap_err();
        assert!(e.contains("'sheet'"), "{e}");
    }

    /// A second sheet on the other side of every sheet-wide setting:
    /// default refresh, no override, no sheet shift, the default view.
    fn plain_sheet() -> Sheet {
        let mut s = Sheet::new("plain");
        push(
            &mut s,
            vec![
                parse_builtin("SPX 1m 100% P").unwrap(),
                parse_builtin("SPX Z26 4800/5200 RR").unwrap(),
            ],
        );
        s
    }

    fn off_sheet() -> Sheet {
        let mut s = Sheet::new("off sheet/with odd; name");
        push(
            &mut s,
            vec![parse_builtin("-7 NDX 20DEC26 20000 C UO 23000").unwrap()],
        );
        s.refresh = Refresh::Off;
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(-2.5),
            vol_pts: None,
        }))
        .unwrap();
        s
    }

    #[test]
    fn a_sheet_survives_the_real_store() {
        use geode_core::dimensions::DerivedDimensions;
        use geode_core::pricing::LocalPublish;
        use geode_core::query::{AsOf, DocumentParams, QueryKey};
        use geode_data::{DataEvent, DataService, DataServiceConfig, PricerConfig};
        use std::time::Instant;

        let dir = tempfile::tempdir().unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(dataset());
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            pricer: PricerConfig::missing("none"),
        })
        .unwrap();
        let until = |pick: &mut dyn FnMut(DataEvent) -> Option<Arc<Snapshot>>| loop {
            let e = rx.recv_timeout(Duration::from_secs(30)).expect("an event");
            if let Some(t) = pick(e) {
                return t;
            }
        };
        let read = |key: u64, name: &str| {
            assert!(
                service
                    .document(&DocumentParams {
                        key: QueryKey(key),
                        tag: 1,
                        submitted: Instant::now(),
                        dataset: PRICER_SHEETS_DATASET.into(),
                        document_key: vec![name.into()],
                        as_of: AsOf::Live,
                    })
                    .is_ok()
            );
            until(&mut |e| match e {
                DataEvent::Query(o) if o.key == QueryKey(key) => {
                    Some(o.snapshot.unwrap_or_else(|e| panic!("{e}")))
                }
                _ => None,
            })
        };

        // No document under the name: an empty answer, read as "Missing".
        let nothing = read(1, "book-1");
        assert_eq!(rows_from_snapshot("book-1", &nothing), Ok(None));

        let sheets = [full_sheet(), plain_sheet(), off_sheet()];
        for s in &sheets {
            service.publish(LocalPublish {
                dataset: PRICER_SHEETS_DATASET.into(),
                rows: to_rows(s).unwrap(),
            });
        }
        let mut stored = 0;
        while stored < sheets.len() {
            match rx.recv_timeout(Duration::from_secs(30)).expect("an event") {
                DataEvent::LocalPublished { .. } => stored += 1,
                DataEvent::LocalPublishFailed { reason, .. } => panic!("{reason}"),
                _ => {}
            }
        }

        for (i, s) in sheets.iter().enumerate() {
            let snapshot = read(10 + i as u64, &s.name);
            let rows = rows_from_snapshot(&s.name, &snapshot)
                .unwrap_or_else(|e| panic!("{}: {e}", s.name))
                .expect("a stored document");
            let back = from_rows(&s.name, &rows).unwrap_or_else(|e| panic!("{e}"));
            let expected = from_rows(&s.name, &to_rows(s).unwrap()).unwrap();
            assert_eq!(definition(&back), definition(&expected), "{}", s.name);
            assert_eq!(definition(&back), definition(s), "{}", s.name);
            assert_eq!(back.view, s.view);
            assert_eq!(back.sheet_shift(), s.sheet_shift());
            assert_eq!(back.overrides(), s.overrides());
            assert_eq!(back.refresh, s.refresh);
            // The whole document, not only what `definition` looks at.
            assert_eq!(to_rows(&back), to_rows(&expected), "{}", s.name);
        }
        service.shutdown();
    }

    #[test]
    fn a_package_whose_template_is_unknown_loads_and_prints_its_legs() {
        let mut s = Sheet::new("t");
        s.apply(Edit::Insert {
            place: crate::core::sheet::Place::Root { at: 0 },
            rows: vec![parse_builtin("SPX Z26 4800/5200 CS").unwrap()],
        })
        .unwrap();
        let mut rows = to_rows(&s).unwrap();
        // Rename the stored template to one no set defines.
        if let Some((_, Column::Utf8(t))) = rows.values.iter_mut().find(|(n, _)| n == "template") {
            for v in t.iter_mut().filter(|v| v.as_str() == "cs") {
                *v = "gone".into();
            }
        }
        let back = from_rows("t", &rows).expect("an unknown template no longer refuses the load");
        assert_eq!(
            back.kind(0),
            RowKind::Package {
                template: Template::named("GONE")
            }
        );
        assert!(back.shorthand(0).contains('\n'), "legs one per line");
    }
}
