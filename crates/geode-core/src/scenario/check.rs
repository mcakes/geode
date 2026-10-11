//! The cross-document half of the `scenarios` reader: each shape checked
//! against the schema and completed with what only the schema knows. Every
//! problem refuses the shape: a shape that disagrees with its dataset would
//! paint a plausible wrong grid, and an axis laid out nowhere would be
//! summed across silently.

use super::{AxisInfo, Columns, DiffBase, MeasureColumn, ShapeSpec, read_scenarios, refusal};
use crate::config::{Diagnostic, Layer, MergedDoc};
use crate::panel::read::Refusal;
use crate::schema::{ColumnRole, DatasetSpec, Grain, SchemaSpec};

/// Every shape `doc` defines that both halves accept, in document order,
/// plus an Error per refused shape. An accepted shape carries its dataset's
/// axes and each measure's `_usd` twin. A refused shape is absent: it never
/// becomes a tile kind, and a refused desk or user entry does not fall back
/// to the builtin it replaced.
pub fn load_scenarios(doc: &MergedDoc, schema: &SchemaSpec) -> (Vec<ShapeSpec>, Vec<Diagnostic>) {
    let (read, mut diags) = read_scenarios(doc);
    let mut accepted = Vec::with_capacity(read.len());
    for mut shape in read {
        match check_shape(&mut shape, schema) {
            Ok(()) => accepted.push(shape),
            Err((suffix, message)) => diags.push(refusal(doc, &shape.kind, &suffix, &message)),
        }
    }
    (accepted, diags)
}

/// The shapes the builtin layer supplies (no desk or user entry replaced
/// them) whose dataset `schema` does not declare, as `(kind, dataset)`, in
/// document order. The builtin shapes read demo-layer datasets, so a launch
/// without that layer would otherwise refuse each with an Error; the app
/// withholds these instead. A desk or user shape over an undeclared dataset
/// is not listed: its refusal is the author's to see.
pub fn builtin_without_dataset(doc: &MergedDoc, schema: &SchemaSpec) -> Vec<(String, String)> {
    read_scenarios(doc)
        .0
        .into_iter()
        .filter(|s| doc.provenance.get(&s.kind) == Some(&Layer::Builtin))
        .filter(|s| schema.dataset(&s.dataset).is_none())
        .map(|s| (s.kind, s.dataset))
        .collect()
}

fn check_shape(spec: &mut ShapeSpec, schema: &SchemaSpec) -> Result<(), Refusal> {
    let Some(ds) = schema.dataset(&spec.dataset) else {
        return Err((
            "dataset".into(),
            format!("dataset '{}' is not declared", spec.dataset),
        ));
    };
    if !ds.has_axes() {
        return Err((
            "dataset".into(),
            format!(
                "dataset '{}' is not a measures dataset with 'axes'; a shape pivots an axis dataset",
                ds.name
            ),
        ));
    }
    spec.axes = ds
        .axes
        .iter()
        .map(|name| {
            let col = ds
                .column(name)
                .expect("a declared axis is a declared column");
            AxisInfo {
                name: name.clone(),
                kind: col.axis_kind().expect("a measures axis has a kind"),
                order: col.axis_order(),
                ty: col.ty,
            }
        })
        .collect();
    if spec.axis(&spec.rows).is_none() {
        return Err((
            "rows".into(),
            format!(
                "'{}' is not an axis of '{}' (its axes are [{}])",
                spec.rows,
                ds.name,
                ds.axes.join(", ")
            ),
        ));
    }
    for (i, c) in spec.column_axes().iter().enumerate() {
        if spec.axis(c).is_none() {
            return Err((
                format!("columns.axis.{i}"),
                format!(
                    "'{c}' is not an axis of '{}' (its axes are [{}])",
                    ds.name,
                    ds.axes.join(", ")
                ),
            ));
        }
    }
    let laid: Vec<&str> = std::iter::once(spec.rows.as_str())
        .chain(spec.column_axes().iter().map(String::as_str))
        .collect();
    if let Some(unlaid) = ds.axes.iter().find(|a| !laid.contains(&a.as_str())) {
        return Err((
            "columns".into(),
            format!(
                "axis '{unlaid}' of '{}' is neither 'rows' nor in 'columns': it would be summed across silently",
                ds.name
            ),
        ));
    }
    if let Some(value) = spec.value.as_mut() {
        check_measure(value, ds, "value")?;
    }
    if let Columns::Measures(cols) = &mut spec.columns {
        for (i, c) in cols.iter_mut().enumerate() {
            check_measure(c, ds, &format!("columns.measures.{i}.column"))?;
        }
    }
    if let Some(diff) = &spec.diff {
        let rows = spec.axis(&spec.rows).expect("checked above");
        let fits = match &diff.base {
            DiffBase::Number(_) => rows.is_numeric(),
            DiffBase::Text(_) => !rows.is_numeric(),
        };
        if !fits {
            return Err((
                "diff.base".into(),
                format!(
                    "'base' must be a {} like the '{}' axis ({})",
                    if rows.is_numeric() {
                        "number"
                    } else {
                        "string"
                    },
                    rows.name,
                    rows.ty.sql().to_ascii_lowercase()
                ),
            ));
        }
    }
    Ok(())
}

/// `m.column` is a measure of `ds`; its `_usd` twin is recorded when `ds`
/// declares one as a measure too.
fn check_measure(m: &mut MeasureColumn, ds: &DatasetSpec, at: &str) -> Result<(), Refusal> {
    match ds.column(&m.column) {
        Some(col) if matches!(col.role, ColumnRole::Measure { .. }) => {}
        Some(_) => {
            return Err((
                at.into(),
                format!("'{}' is not a measure of '{}'", m.column, ds.name),
            ));
        }
        None => {
            return Err((
                at.into(),
                format!("'{}' is not a column of '{}'", m.column, ds.name),
            ));
        }
    }
    // A twin at another grain would sum the converted measure over a
    // different row set than the measure itself: the toggle would swap in a
    // plausible wrong total.
    let twin = format!("{}_usd", m.column);
    let base_grain = ds.column(&m.column).and_then(|c| c.grain());
    m.usd_twin = match ds.column(&twin) {
        Some(c) if matches!(c.role, ColumnRole::Measure { .. }) => {
            if c.grain() != base_grain {
                return Err((
                    at.into(),
                    format!(
                        "'{twin}' is at {} grain while '{}' is at {}; a twin converts the same measure at the same grain",
                        grain_name(c.grain()),
                        m.column,
                        grain_name(base_grain),
                    ),
                ));
            }
            Some(twin)
        }
        _ => None,
    };
    Ok(())
}

/// A measure always carries a grain; "no" only spells a schema that broke
/// that invariant, rather than panicking over a message.
fn grain_name(grain: Option<Grain>) -> &'static str {
    grain.map_or("no", Grain::short)
}
