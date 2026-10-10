//! The structural half of the `scenarios` reader: the kind name, required
//! keys, exactly one `columns` form, the `value`/`diff`/`paint` rules and
//! the formats, judged from each entry alone. Every problem refuses the
//! entry: a misread key would otherwise paint a plausible wrong grid.

use super::{Columns, Diff, DiffBase, MeasureColumn, Paint, ShapeSpec, refusal};
use crate::config::{Diagnostic, MergedDoc};
use crate::panel::PLACEMENT_SUFFIXES;
use crate::panel::read::{
    Refusal, is_kind_name, join, known_keys, opt_array, req, req_str, req_table,
};
use crate::view::{ColumnFormat, ColumnPresentation};
use std::cell::RefCell;
use toml::{Table, Value};

/// Every shape `doc` defines that reads cleanly, in document order, plus an
/// Error per refused shape. Checks against the schema are `load_scenarios`'s.
pub fn read_scenarios(doc: &MergedDoc) -> (Vec<ShapeSpec>, Vec<Diagnostic>) {
    let mut shapes = Vec::new();
    let mut diags = Vec::new();
    for (name, value) in &doc.value {
        if name == "config_version" {
            continue;
        }
        match read_shape(name, value) {
            Ok(shape) => shapes.push(shape),
            Err((suffix, message)) => diags.push(refusal(doc, name, &suffix, &message)),
        }
    }
    (shapes, diags)
}

const KEYS: &[&str] = &[
    "title", "dataset", "rows", "columns", "value", "format", "unit", "diff", "paint",
];

fn read_shape(name: &str, value: &Value) -> Result<ShapeSpec, Refusal> {
    if !is_kind_name(name) {
        return Err((
            String::new(),
            format!(
                "'{name}' is not a usable tile kind: use lower-case letters, digits and '_', starting with a letter"
            ),
        ));
    }
    if let Some(suffix) = PLACEMENT_SUFFIXES.iter().find(|s| name.ends_with(*s)) {
        return Err((
            String::new(),
            format!(
                "'{name}' ends in '{suffix}', a suffix reserved for add-tile placement ids: 'tile::add_{name}' would name another kind's placement"
            ),
        ));
    }
    let t = value
        .as_table()
        .ok_or_else(|| (String::new(), "a shape must be a table".to_string()))?;
    known_keys(t, "", KEYS)?;
    let title = req_str(t, "", "title")?;
    let dataset = req_str(t, "", "dataset")?;
    let rows = req_str(t, "", "rows")?;
    let columns = read_columns(req_table(t, "", "columns")?, &rows)?;
    let value = match (&columns, t.get("value")) {
        (Columns::Axes(_), None) => {
            return Err((
                "value".into(),
                "a matrix (an axis 'columns') needs 'value': the measure it paints".into(),
            ));
        }
        (Columns::Axes(_), Some(_)) => Some(MeasureColumn {
            column: req_str(t, "", "value")?,
            label: req_str(t, "", "value")?,
            format: read_format(t, "")?,
            unit: opt_str(t, "", "unit")?,
            usd_twin: None,
        }),
        (Columns::Measures(_), Some(_)) => {
            return Err((
                "value".into(),
                "a ladder (a measures 'columns') paints each listed measure; 'value' names none"
                    .into(),
            ));
        }
        (Columns::Measures(_), None) => {
            for key in ["format", "unit"] {
                if t.contains_key(key) {
                    return Err((
                        key.into(),
                        format!("'{key}' belongs on each entry of 'columns.measures' in a ladder"),
                    ));
                }
            }
            None
        }
    };
    let diff = match t.get("diff") {
        None => None,
        Some(v) => Some(read_diff(v, &columns)?),
    };
    let paint_text = req_str(t, "", "paint")?;
    let Some(paint) = Paint::parse(&paint_text) else {
        return Err((
            "paint".into(),
            format!("'paint' must be \"heat\" or \"plain\" (got \"{paint_text}\")"),
        ));
    };
    Ok(ShapeSpec {
        kind: name.to_string(),
        title,
        dataset,
        rows,
        columns,
        value,
        diff,
        paint,
        axes: Vec::new(),
    })
}

fn opt_str(t: &Table, at: &str, key: &str) -> Result<Option<String>, Refusal> {
    match t.get(key) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) if !s.trim().is_empty() => Ok(Some(s.to_string())),
            _ => Err((
                join(at, key),
                format!("'{key}' must be a non-empty string (got {v})"),
            )),
        },
    }
}

/// `ColumnFormat::MEASURE` with the table's `format` keys applied: the view
/// format grammar (`precision`, `thousands`, `negative`, `scale`, `color`).
fn read_format(t: &Table, at: &str) -> Result<ColumnFormat, Refusal> {
    let at_format = join(at, "format");
    let Some(v) = t.get("format") else {
        return Ok(ColumnFormat::MEASURE);
    };
    let f = v
        .as_table()
        .ok_or_else(|| (at_format.clone(), "'format' must be a table".to_string()))?;
    known_keys(
        f,
        &at_format,
        &["precision", "thousands", "negative", "scale", "color"],
    )?;
    let first = RefCell::new(None::<Refusal>);
    let mut presentation = ColumnPresentation::default();
    presentation.parse_format_keys(f, &|key, message| {
        first
            .borrow_mut()
            .get_or_insert((join(&at_format, key), message));
    });
    match first.into_inner() {
        Some(r) => Err(r),
        None => Ok(ColumnFormat::MEASURE.with(&presentation)),
    }
}

fn read_columns(t: &Table, rows: &str) -> Result<Columns, Refusal> {
    match (t.get("axis"), t.get("measures")) {
        (Some(axis), None) => {
            known_keys(t, "columns", &["axis"])?;
            let axes: Vec<String> = match axis {
                Value::String(s) => vec![s.clone()],
                Value::Array(items) => items
                    .iter()
                    .map(|v| {
                        v.as_str().map(str::to_string).ok_or_else(|| {
                            (
                                "columns.axis".to_string(),
                                format!("'axis' must be a string or an array of strings (got {v})"),
                            )
                        })
                    })
                    .collect::<Result<_, _>>()?,
                other => {
                    return Err((
                        "columns.axis".into(),
                        format!("'axis' must be a string or an array of strings (got {other})"),
                    ));
                }
            };
            if axes.is_empty() || axes.len() > 2 {
                return Err((
                    "columns.axis".into(),
                    format!(
                        "'axis' names one axis, or two for a two-level header (got {})",
                        axes.len()
                    ),
                ));
            }
            if axes.len() == 2 && axes[0] == axes[1] {
                return Err((
                    "columns.axis".into(),
                    format!("'axis' names '{}' twice", axes[0]),
                ));
            }
            if let Some(a) = axes.iter().find(|a| *a == rows) {
                return Err((
                    "columns.axis".into(),
                    format!("'{a}' is the rows axis; a columns axis must be another"),
                ));
            }
            Ok(Columns::Axes(axes))
        }
        (None, Some(_)) => {
            known_keys(t, "columns", &["measures"])?;
            let items = opt_array(t, "columns", "measures")?;
            if items.is_empty() {
                return Err((
                    "columns.measures".into(),
                    "a ladder needs at least one measure".into(),
                ));
            }
            let cols = items
                .iter()
                .enumerate()
                .map(|(i, v)| read_measure(i, v))
                .collect::<Result<Vec<_>, _>>()?;
            for (i, c) in cols.iter().enumerate() {
                if cols[..i].iter().any(|p| p.column == c.column) {
                    return Err((
                        format!("columns.measures.{i}.column"),
                        format!("'{}' is listed twice", c.column),
                    ));
                }
                if cols[..i].iter().any(|p| p.label == c.label) {
                    return Err((
                        format!("columns.measures.{i}.label"),
                        format!("two columns share the label '{}'", c.label),
                    ));
                }
            }
            Ok(Columns::Measures(cols))
        }
        _ => Err((
            "columns".into(),
            "give exactly one of 'axis' (a matrix) or 'measures' (a ladder)".into(),
        )),
    }
}

fn read_measure(i: usize, v: &Value) -> Result<MeasureColumn, Refusal> {
    let at = format!("columns.measures.{i}");
    let t = v
        .as_table()
        .ok_or_else(|| (at.clone(), format!("a measure must be a table (got {v})")))?;
    known_keys(t, &at, &["column", "label", "format", "unit"])?;
    let column = req_str(t, &at, "column")?;
    let label = opt_str(t, &at, "label")?.unwrap_or_else(|| column.clone());
    Ok(MeasureColumn {
        column,
        label,
        format: read_format(t, &at)?,
        unit: opt_str(t, &at, "unit")?,
        usd_twin: None,
    })
}

fn read_diff(v: &Value, columns: &Columns) -> Result<Diff, Refusal> {
    let t = v.as_table().ok_or_else(|| {
        (
            "diff".to_string(),
            format!("'diff' must be a table (got {v})"),
        )
    })?;
    known_keys(t, "diff", &["base", "measures"])?;
    let Columns::Measures(cols) = columns else {
        return Err((
            "diff".into(),
            "'diff' subtracts a base row from listed measure columns: it needs a measures 'columns'".into(),
        ));
    };
    let base = match req(t, "diff", "base")? {
        Value::Integer(n) => DiffBase::Number(*n as f64),
        Value::Float(x) => DiffBase::Number(*x),
        Value::String(s) => DiffBase::Text(s.clone()),
        other => {
            return Err((
                "diff.base".into(),
                format!("'base' must be a number or a string (got {other})"),
            ));
        }
    };
    let measures = opt_array(t, "diff", "measures")?;
    if measures.is_empty() {
        return Err((
            "diff.measures".into(),
            "'measures' lists at least one column the diff applies to".into(),
        ));
    }
    let measures = measures
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let name = m.as_str().ok_or_else(|| {
                (
                    format!("diff.measures.{i}"),
                    format!("a diff measure is a column name (got {m})"),
                )
            })?;
            if !cols.iter().any(|c| c.column == name) {
                return Err((
                    format!("diff.measures.{i}"),
                    format!("'{name}' is not one of the shape's measure columns"),
                ));
            }
            Ok(name.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Diff { base, measures })
}
