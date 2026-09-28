//! The cross-document half of the `panels` reader: each panel checked
//! against the schema and the registered document kinds. Every problem
//! refuses the panel: a panel that disagrees with its dataset would paint a
//! plausible wrong grid or upload a document the store refuses.

use super::read::Refusal;
use super::{Columns, KindActionRegistry, PanelSpec, RowIdentity, read_panels, refusal};
use crate::config::{Diagnostic, MergedDoc};
use crate::document::{DocumentKind, check_kind_against};
use crate::schema::{ColumnRole, ColumnSpec, ColumnType, DatasetSpec, SchemaSpec};
use std::sync::Arc;

/// Every panel `doc` defines that both halves accept, in document order,
/// plus an Error per refused panel. A refused panel is absent: it never
/// becomes a tile kind, so it cannot open blank or with guessed columns.
/// A panel over an undeclared dataset is refused whichever layer supplied
/// it, the builtin one included.
pub fn load_panels(
    doc: &MergedDoc,
    actions: &KindActionRegistry,
    schema: &SchemaSpec,
    documents: &[Arc<dyn DocumentKind>],
) -> (Vec<PanelSpec>, Vec<Diagnostic>) {
    let (read, mut diags) = read_panels(doc, actions);
    let mut accepted = Vec::with_capacity(read.len());
    for panel in read {
        match check_panel(&panel, schema, documents) {
            Ok(()) => accepted.push(panel),
            Err((suffix, message)) => diags.push(refusal(doc, &panel.kind, &suffix, &message)),
        }
    }
    (accepted, diags)
}

fn check_panel(
    spec: &PanelSpec,
    schema: &SchemaSpec,
    documents: &[Arc<dyn DocumentKind>],
) -> Result<(), Refusal> {
    let Some(ds) = schema.dataset(&spec.dataset) else {
        return Err((
            "dataset".into(),
            format!("dataset '{}' is not declared", spec.dataset),
        ));
    };
    if !ds.is_document() {
        return Err((
            "dataset".into(),
            format!("dataset '{}' is not a document dataset", ds.name),
        ));
    }
    let Some(kind) = documents.iter().find(|k| k.name() == spec.document) else {
        let names: Vec<&str> = documents.iter().map(|k| k.name()).collect();
        return Err((
            "document".into(),
            format!(
                "'{}' is not a registered document kind (registered: {})",
                spec.document,
                names.join(", ")
            ),
        ));
    };
    let fits = check_kind_against(kind.as_ref(), ds);
    if let Err(e) = fits {
        return Err((
            "document".into(),
            format!(
                "document kind '{}' does not fit dataset '{}': {e}",
                spec.document, spec.dataset
            ),
        ));
    }
    check_axes(spec, ds)?;
    check_rows(spec, ds)?;
    for (i, h) in spec.header.iter().enumerate() {
        let col = column(ds, &h.column, &format!("header.{i}.column"))?;
        if col.role != (ColumnRole::Attribute { grain: None }) {
            return Err((
                format!("header.{i}.column"),
                format!(
                    "'{}' is not a document attribute of '{}'",
                    h.column, ds.name
                ),
            ));
        }
        same_type(col, h.ty, &format!("header.{i}.type"))?;
    }
    let ladder = match &spec.columns {
        Columns::Values(cols) => {
            for (i, c) in cols.iter().enumerate() {
                let at = format!("columns.values.{i}.column");
                let col = column(ds, &c.column, &at)?;
                is_value(col, ds, &at)?;
                same_type(col, c.ty, &format!("columns.values.{i}.type"))?;
            }
            None
        }
        Columns::Axis(axis) => Some(check_pivot(spec, ds, axis)?),
    };
    check_coverage(spec, ds, kind.as_ref(), ladder)
}

/// The model reads its document key from the columns before the row axis,
/// with the dataset's first axis as rows: an axis the panel does not lay out
/// would fold into the key and split one document into several.
fn check_axes(spec: &PanelSpec, ds: &DatasetSpec) -> Result<(), Refusal> {
    if ds.axes.first().map(String::as_str) != Some(spec.rows.column.as_str()) {
        return Err((
            "rows.column".into(),
            format!(
                "the row axis must be dataset '{}''s first axis (its axes are [{}])",
                ds.name,
                ds.axes.join(", ")
            ),
        ));
    }
    let (laid, at): (Vec<&str>, &str) = match &spec.columns {
        Columns::Axis(a) => (vec![spec.rows.column.as_str(), a.as_str()], "columns.axis"),
        Columns::Values(_) => (vec![spec.rows.column.as_str()], "rows.column"),
    };
    if !ds.axes.iter().map(String::as_str).eq(laid.iter().copied()) {
        return Err((
            at.into(),
            format!(
                "the panel lays out axes [{}] but dataset '{}' has [{}]",
                laid.join(", "),
                ds.name,
                ds.axes.join(", ")
            ),
        ));
    }
    Ok(())
}

fn check_rows(spec: &PanelSpec, ds: &DatasetSpec) -> Result<(), Refusal> {
    let col = column(ds, &spec.rows.column, "rows.column")?;
    match spec.rows.identity {
        RowIdentity::Typed(ty) => same_type(col, ty, "rows.identity"),
        RowIdentity::Minted if col.ty == ColumnType::Utf8 => Ok(()),
        RowIdentity::Minted => Err((
            "rows.identity".into(),
            format!(
                "minted row labels are text, but '{}' is {}",
                col.name,
                spelled(col.ty)
            ),
        )),
    }
}

/// One unnamed value column is the ladder; every other value must be a
/// slice. Two unnamed values make a pivot the model refuses on every
/// delivery. Answers the ladder column's name.
fn check_pivot<'a>(spec: &PanelSpec, ds: &'a DatasetSpec, axis: &str) -> Result<&'a str, Refusal> {
    for (i, s) in spec.slice_values.iter().enumerate() {
        let at = format!("slice.{i}.column");
        let col = column(ds, &s.column, &at)?;
        is_value(col, ds, &at)?;
        if col.ty != ColumnType::F64 {
            return Err((
                at,
                format!(
                    "slice values are read and uploaded as f64, but '{}' is {}",
                    s.column,
                    spelled(col.ty)
                ),
            ));
        }
    }
    let ladder: Vec<&ColumnSpec> = ds
        .columns
        .iter()
        .filter(|c| c.role == ColumnRole::Value && spec.slice_value(&c.name).is_none())
        .collect();
    let [value] = ladder.as_slice() else {
        let names: Vec<&str> = ladder.iter().map(|c| c.name.as_str()).collect();
        return Err((
            "slice".into(),
            format!(
                "dataset '{}' must leave exactly one value column for the grid once the slices are named; it leaves [{}]",
                ds.name,
                names.join(", ")
            ),
        ));
    };
    same_type(value, spec.value_type, "value.type")?;
    // A slice editor parses by the panel's value type: an i64 panel would
    // refuse a fractional forward, and its format would round one.
    for (i, s) in spec.slice_values.iter().enumerate() {
        let col = column(ds, &s.column, &format!("slice.{i}.column"))?;
        if col.ty != spec.value_type {
            return Err((
                format!("slice.{i}.column"),
                format!(
                    "slice '{}' is {} but the panel's value.type is {}: a slice is edited as the panel's value type",
                    s.column,
                    spelled(col.ty),
                    spelled(spec.value_type)
                ),
            ));
        }
    }
    let axis_ty = column(ds, axis, "columns.axis")?.ty;
    for (i, s) in spec.slice_values.iter().enumerate() {
        // Numbers paint through their plain spelling and dates as ISO text,
        // so a label that parses as one could also be an axis label. Any
        // other axis paints labels straight from the data: no label can be
        // proven distinct at load.
        let collides = match axis_ty {
            ColumnType::F64 | ColumnType::I64 => s.label.trim().parse::<f64>().is_ok(),
            ColumnType::Date => chrono::NaiveDate::parse_from_str(&s.label, "%Y-%m-%d").is_ok(),
            _ => return Err(data_labelled_axis(i, axis, axis_ty, &s.label)),
        };
        if collides {
            return Err((
                format!("slice.{i}.label"),
                format!(
                    "slice label '{}' could also be a '{axis}' label; a column label must name one column",
                    s.label
                ),
            ));
        }
    }
    Ok(value.name.as_str())
}

/// Slice `i` over an axis whose column labels come from the data: a draft
/// resolves an edit by column label, so a collision would misdirect edits,
/// and none can be ruled out before the data arrives.
fn data_labelled_axis(i: usize, axis: &str, ty: ColumnType, label: &str) -> Refusal {
    (
        format!("slice.{i}.label"),
        format!(
            "slices are refused over the {} axis '{axis}': its column labels come from the data, so slice label '{label}' could collide with one",
            spelled(ty)
        ),
    )
}

/// Every column the document kind writes, less the document key the tile
/// supplies, must be laid out by the panel: an unnamed column could never be
/// edited, so every upload would write it empty or be refused. The pivot's
/// ladder is named by being the one unnamed value.
fn check_coverage(
    spec: &PanelSpec,
    ds: &DatasetSpec,
    kind: &dyn DocumentKind,
    ladder: Option<&str>,
) -> Result<(), Refusal> {
    let missing: Vec<&str> = kind
        .columns()
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| {
            !ds.key.iter().any(|k| k == name) && !spec.names(name) && ladder != Some(*name)
        })
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let values_missing = missing
        .iter()
        .any(|name| ds.column(name).is_some_and(|c| c.role == ColumnRole::Value));
    let at = if values_missing {
        "columns.values"
    } else {
        "header"
    };
    Err((
        at.into(),
        format!(
            "document kind '{}' writes [{}], which the panel does not name; a panel must name every column it uploads",
            kind.name(),
            missing.join(", ")
        ),
    ))
}

fn column<'a>(ds: &'a DatasetSpec, name: &str, at: &str) -> Result<&'a ColumnSpec, Refusal> {
    ds.column(name).ok_or_else(|| {
        (
            at.to_string(),
            format!("dataset '{}' has no column '{name}'", ds.name),
        )
    })
}

fn is_value(col: &ColumnSpec, ds: &DatasetSpec, at: &str) -> Result<(), Refusal> {
    if col.role != ColumnRole::Value {
        return Err((
            at.to_string(),
            format!("'{}' is not a value column of '{}'", col.name, ds.name),
        ));
    }
    Ok(())
}

fn same_type(col: &ColumnSpec, ty: ColumnType, at: &str) -> Result<(), Refusal> {
    if col.ty != ty {
        return Err((
            at.to_string(),
            format!(
                "'{}' is {} in the dataset, {} here",
                col.name,
                spelled(col.ty),
                spelled(ty)
            ),
        ));
    }
    Ok(())
}

fn spelled(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Utf8 => "utf8",
        ColumnType::F64 => "f64",
        ColumnType::I64 => "i64",
        ColumnType::Date => "date",
        ColumnType::Timestamp => "timestamp",
        ColumnType::Bool => "bool",
    }
}
