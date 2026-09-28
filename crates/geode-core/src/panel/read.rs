//! The structural half of the `panels` reader: required keys, value shapes,
//! formats, labels and kind-action ids, judged from each panel alone.
//! Every problem refuses the panel: a misread key would otherwise paint a
//! plausible wrong grid.

use super::{
    Columns, HeaderAttr, KindAction, KindActionRegistry, PanelSpec, RowAxis, RowIdentity, RowLabel,
    SliceValue, ValueColumn, refusal,
};
use crate::config::{Diagnostic, MergedDoc};
use crate::schema::ColumnType;
use crate::view::{ColumnFormat, ColumnPresentation};
use std::cell::RefCell;
use std::sync::Arc;
use toml::{Table, Value};

/// A refusal before it becomes a diagnostic: the path under `panels.<name>`
/// and what is wrong there.
pub(super) type Refusal = (String, String);

const NEEDS_PRECISION: &str =
    "an f64 needs 'format.precision': the panel will not guess how many places a value carries";

/// Every panel `doc` defines that reads cleanly, in document order, plus an
/// Error per refused panel. Checks against the schema and the registered
/// document kinds are `load_panels`'s.
pub fn read_panels(
    doc: &MergedDoc,
    actions: &KindActionRegistry,
) -> (Vec<PanelSpec>, Vec<Diagnostic>) {
    let mut panels = Vec::new();
    let mut diags = Vec::new();
    for (name, value) in &doc.value {
        if name == "config_version" {
            continue;
        }
        match read_panel(name, value, actions) {
            Ok(panel) => panels.push(panel),
            Err((suffix, message)) => diags.push(refusal(doc, name, &suffix, &message)),
        }
    }
    (panels, diags)
}

/// A usable tile kind: the name becomes the roster kind, the
/// `tile::add_<kind>` action ids and the session's `module` value.
fn is_kind_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn read_panel(
    name: &str,
    value: &Value,
    actions: &KindActionRegistry,
) -> Result<PanelSpec, Refusal> {
    if !is_kind_name(name) {
        return Err((
            String::new(),
            format!(
                "'{name}' is not a usable tile kind: use lower-case letters, digits and '_', starting with a letter"
            ),
        ));
    }
    let t = value
        .as_table()
        .ok_or_else(|| (String::new(), "a panel must be a table".to_string()))?;
    known_keys(
        t,
        "",
        &[
            "title", "dataset", "document", "value", "rows", "columns", "header", "slice",
            "actions",
        ],
    )?;
    let title = req_str(t, "", "title")?;
    let dataset = req_str(t, "", "dataset")?;
    let document = req_str(t, "", "document")?;
    let value = req_table(t, "", "value")?;
    known_keys(value, "value", &["type", "format"])?;
    let value_type = req_type(value, "value", "type")?;
    if !is_number(value_type) {
        return Err((
            "value.type".into(),
            "a panel's value type must be f64 or i64: the grid paints and steps its values as numbers"
                .into(),
        ));
    }
    let format = read_format(
        value,
        "value",
        ColumnFormat::TEXT,
        value_type == ColumnType::F64,
    )?;
    let rows = read_rows(req_table(t, "", "rows")?)?;
    let columns = read_columns(req_table(t, "", "columns")?)?;
    let header = read_header(t)?;
    let slice_values = read_slices(t, &format)?;
    if matches!(columns, Columns::Values(_)) && !slice_values.is_empty() {
        return Err((
            "slice".into(),
            "a flat panel has no slices: slice values need 'columns.axis'".into(),
        ));
    }
    let actions = read_actions(t, actions)?;
    let spec = PanelSpec {
        kind: name.to_string(),
        title,
        dataset,
        document,
        rows,
        columns,
        header,
        slice_values,
        format,
        value_type,
        actions,
    };
    unique_labels(&spec)?;
    columns_named_once(&spec)?;
    Ok(spec)
}

fn join(at: &str, key: &str) -> String {
    if at.is_empty() {
        key.to_string()
    } else {
        format!("{at}.{key}")
    }
}

fn known_keys(t: &Table, at: &str, keys: &[&str]) -> Result<(), Refusal> {
    match t.keys().find(|k| !keys.contains(&k.as_str())) {
        None => Ok(()),
        Some(k) => Err((
            join(at, k),
            format!("unknown key '{k}' (expected one of: {})", keys.join(", ")),
        )),
    }
}

fn req<'a>(t: &'a Table, at: &str, key: &str) -> Result<&'a Value, Refusal> {
    t.get(key)
        .ok_or_else(|| (join(at, key), format!("missing required key '{key}'")))
}

fn req_str(t: &Table, at: &str, key: &str) -> Result<String, Refusal> {
    let v = req(t, at, key)?;
    match v.as_str() {
        Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
        _ => Err((
            join(at, key),
            format!("'{key}' must be a non-empty string (got {v})"),
        )),
    }
}

fn req_bool(t: &Table, at: &str, key: &str) -> Result<bool, Refusal> {
    let v = req(t, at, key)?;
    v.as_bool().ok_or_else(|| {
        (
            join(at, key),
            format!("'{key}' must be true or false (got {v})"),
        )
    })
}

fn req_table<'a>(t: &'a Table, at: &str, key: &str) -> Result<&'a Table, Refusal> {
    req(t, at, key)?
        .as_table()
        .ok_or_else(|| (join(at, key), format!("'{key}' must be a table")))
}

fn table_at<'a>(v: &'a Value, at: &str) -> Result<&'a Table, Refusal> {
    v.as_table()
        .ok_or_else(|| (at.to_string(), format!("must be a table (got {v})")))
}

fn opt_array<'a>(t: &'a Table, at: &str, key: &str) -> Result<&'a [Value], Refusal> {
    match t.get(key) {
        None => Ok(&[]),
        Some(v) => v
            .as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| (join(at, key), format!("'{key}' must be an array"))),
    }
}

/// The types a panel's typed cells, editors and upload cover.
fn edit_type(s: &str) -> Option<ColumnType> {
    match ColumnType::parse(s)? {
        ty @ (ColumnType::F64 | ColumnType::I64 | ColumnType::Date | ColumnType::Utf8) => Some(ty),
        _ => None,
    }
}

fn req_type(t: &Table, at: &str, key: &str) -> Result<ColumnType, Refusal> {
    let s = req_str(t, at, key)?;
    edit_type(&s).ok_or_else(|| {
        (
            join(at, key),
            format!("'{key}' must be f64, i64, date or utf8 (got \"{s}\")"),
        )
    })
}

fn is_number(ty: ColumnType) -> bool {
    matches!(ty, ColumnType::F64 | ColumnType::I64)
}

/// `base` with the table's `format` keys applied. `needs_precision`: an f64
/// whose base does not already carry a chosen precision must say how many
/// places it paints. `color` is not a key here: a panel grid paints every
/// value in the foreground, so it would be silently ignored.
fn read_format(
    t: &Table,
    at: &str,
    base: ColumnFormat,
    needs_precision: bool,
) -> Result<ColumnFormat, Refusal> {
    let at_format = join(at, "format");
    let Some(v) = t.get("format") else {
        return if needs_precision {
            Err((at_format, NEEDS_PRECISION.into()))
        } else {
            Ok(base)
        };
    };
    let f = v
        .as_table()
        .ok_or_else(|| (at_format.clone(), "'format' must be a table".to_string()))?;
    known_keys(
        f,
        &at_format,
        &["precision", "thousands", "negative", "scale"],
    )?;
    if needs_precision && !f.contains_key("precision") {
        return Err((join(&at_format, "precision"), NEEDS_PRECISION.into()));
    }
    let first = RefCell::new(None::<Refusal>);
    let mut presentation = ColumnPresentation::default();
    presentation.parse_format_keys(f, &|key, message| {
        first
            .borrow_mut()
            .get_or_insert((join(&at_format, key), message));
    });
    match first.into_inner() {
        Some(r) => Err(r),
        None => Ok(base.with(&presentation)),
    }
}

fn read_rows(t: &Table) -> Result<RowAxis, Refusal> {
    known_keys(t, "rows", &["column", "identity", "label"])?;
    let column = req_str(t, "rows", "column")?;
    let identity = match req_str(t, "rows", "identity")?.as_str() {
        "minted" => RowIdentity::Minted,
        other => RowIdentity::Typed(edit_type(other).ok_or_else(|| {
            (
                "rows.identity".to_string(),
                format!(
                    "'identity' must be \"minted\" or f64, i64, date or utf8 (got \"{other}\")"
                ),
            )
        })?),
    };
    let label = match req_str(t, "rows", "label")?.as_str() {
        "shown" => RowLabel::Shown,
        "hidden" => RowLabel::Hidden,
        other => {
            return Err((
                "rows.label".into(),
                format!("'label' must be \"shown\" or \"hidden\" (got \"{other}\")"),
            ));
        }
    };
    if label == RowLabel::Hidden && matches!(identity, RowIdentity::Typed(_)) {
        return Err((
            "rows.label".into(),
            "a typed row identity needs a shown label: a new row is named in the row-label column"
                .into(),
        ));
    }
    Ok(RowAxis {
        column,
        identity,
        label,
    })
}

fn read_columns(t: &Table) -> Result<Columns, Refusal> {
    match (t.get("axis"), t.get("values")) {
        (Some(_), None) => {
            known_keys(t, "columns", &["axis"])?;
            Ok(Columns::Axis(req_str(t, "columns", "axis")?))
        }
        (None, Some(_)) => {
            known_keys(t, "columns", &["values"])?;
            let values = opt_array(t, "columns", "values")?;
            if values.is_empty() {
                return Err((
                    "columns.values".into(),
                    "a flat panel needs at least one value column".into(),
                ));
            }
            values
                .iter()
                .enumerate()
                .map(|(i, v)| read_value_column(i, v))
                .collect::<Result<Vec<_>, _>>()
                .map(Columns::Values)
        }
        _ => Err((
            "columns".into(),
            "give exactly one of 'axis' (a pivot) or 'values' (a flat table)".into(),
        )),
    }
}

fn read_value_column(i: usize, v: &Value) -> Result<ValueColumn, Refusal> {
    let at = format!("columns.values.{i}");
    let t = table_at(v, &at)?;
    known_keys(
        t,
        &at,
        &["column", "label", "type", "format", "choices", "required"],
    )?;
    let column = req_str(t, &at, "column")?;
    let label = req_str(t, &at, "label")?;
    let ty = req_type(t, &at, "type")?;
    let required = req_bool(t, &at, "required")?;
    let format = if is_number(ty) {
        read_format(t, &at, ColumnFormat::TEXT, ty == ColumnType::F64)?
    } else if t.contains_key("format") {
        return Err((
            join(&at, "format"),
            "'format' applies to f64 and i64 columns only".into(),
        ));
    } else {
        ColumnFormat::TEXT
    };
    let choices = match t.get("choices") {
        None => None,
        Some(v) => Some(read_choices(v, &join(&at, "choices"), ty)?),
    };
    Ok(ValueColumn {
        column,
        label,
        ty,
        format,
        choices,
        required,
    })
}

fn read_choices(v: &Value, at: &str, ty: ColumnType) -> Result<Arc<[String]>, Refusal> {
    if ty != ColumnType::Utf8 {
        return Err((at.into(), "'choices' applies to utf8 columns only".into()));
    }
    let items = v.as_array().ok_or_else(|| {
        (
            at.to_string(),
            "'choices' must be an array of strings".to_string(),
        )
    })?;
    let mut out: Vec<String> = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let Some(s) = item.as_str().filter(|s| !s.trim().is_empty()) else {
            return Err((
                format!("{at}.{i}"),
                format!("a choice must be a non-empty string (got {item})"),
            ));
        };
        if out.iter().any(|o| o == s) {
            return Err((format!("{at}.{i}"), format!("choice '{s}' is listed twice")));
        }
        out.push(s.to_string());
    }
    if out.is_empty() {
        return Err((at.into(), "'choices' must list at least one value".into()));
    }
    Ok(out.into())
}

fn read_header(t: &Table) -> Result<Vec<HeaderAttr>, Refusal> {
    opt_array(t, "", "header")?
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let at = format!("header.{i}");
            let t = table_at(v, &at)?;
            known_keys(t, &at, &["column", "label", "type"])?;
            Ok(HeaderAttr {
                column: req_str(t, &at, "column")?,
                label: req_str(t, &at, "label")?,
                ty: req_type(t, &at, "type")?,
            })
        })
        .collect()
}

/// A slice's `format` overlays the panel's value format; with none it takes
/// that format whole.
fn read_slices(t: &Table, value_format: &ColumnFormat) -> Result<Vec<SliceValue>, Refusal> {
    opt_array(t, "", "slice")?
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let at = format!("slice.{i}");
            let t = table_at(v, &at)?;
            known_keys(t, &at, &["column", "label", "format"])?;
            Ok(SliceValue {
                column: req_str(t, &at, "column")?,
                label: req_str(t, &at, "label")?,
                format: read_format(t, &at, value_format.clone(), false)?,
            })
        })
        .collect()
}

fn read_actions(t: &Table, registry: &KindActionRegistry) -> Result<Vec<KindAction>, Refusal> {
    let mut out: Vec<KindAction> = Vec::new();
    for (i, v) in opt_array(t, "", "actions")?.iter().enumerate() {
        let at = format!("actions.{i}");
        let Some(id) = v.as_str() else {
            return Err((at, format!("an action must be an id string (got {v})")));
        };
        let Some(action) = registry.get(id) else {
            return Err((
                at,
                format!(
                    "'{id}' is not a registered kind action (registered: {})",
                    registry.ids().join(", ")
                ),
            ));
        };
        if out.iter().any(|a| a.id == action.id) {
            return Err((at, format!("'{id}' is listed twice")));
        }
        out.push(action);
    }
    Ok(out)
}

/// Labels name columns: a draft resolves an edit by (row label, column
/// label) and the model indexes its columns by label, so two columns one
/// label would send an edit to the wrong one.
fn unique_labels(spec: &PanelSpec) -> Result<(), Refusal> {
    let labels: Vec<(String, &str)> = match &spec.columns {
        Columns::Values(cols) => cols
            .iter()
            .enumerate()
            .map(|(i, c)| (format!("columns.values.{i}.label"), c.label.as_str()))
            .collect(),
        Columns::Axis(_) => spec
            .slice_values
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("slice.{i}.label"), s.label.as_str()))
            .collect(),
    };
    for (i, (path, label)) in labels.iter().enumerate() {
        if labels[..i].iter().any(|(_, l)| l == label) {
            return Err((path.clone(), format!("label '{label}' names two columns")));
        }
    }
    Ok(())
}

/// Each dataset column plays one part: a column read as both a header
/// attribute and a value would be edited in two places and uploaded once.
fn columns_named_once(spec: &PanelSpec) -> Result<(), Refusal> {
    let mut named: Vec<(String, &str)> = vec![("rows.column".into(), spec.rows.column.as_str())];
    match &spec.columns {
        Columns::Axis(a) => named.push(("columns.axis".into(), a.as_str())),
        Columns::Values(cols) => named.extend(
            cols.iter()
                .enumerate()
                .map(|(i, c)| (format!("columns.values.{i}.column"), c.column.as_str())),
        ),
    }
    named.extend(
        spec.header
            .iter()
            .enumerate()
            .map(|(i, h)| (format!("header.{i}.column"), h.column.as_str())),
    );
    named.extend(
        spec.slice_values
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("slice.{i}.column"), s.column.as_str())),
    );
    for (i, (path, column)) in named.iter().enumerate() {
        if named[..i].iter().any(|(_, c)| c == column) {
            return Err((path.clone(), format!("column '{column}' is named twice")));
        }
    }
    Ok(())
}
