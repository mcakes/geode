//! What an upload sends, and whether an echo confirms it (egress spec §6
//! "Assembly", §7 "The echo", amendment 3).
//!
//! Pure: no element, entity or I/O. The tile calls [`assemble`] on
//! `:upload`'s `y` and keeps the result as `sent`; when a later generation
//! is delivered it calls [`echo_differs`] with that generation's rows.
//!
//! Every value is written at its DECLARED type — the spec's `ty` for a
//! flat column, `value_type` for a pivot's ladder, `f64` for a slice
//! value, `HeaderAttr::ty` for an attribute — and a value of any other
//! tag is refused naming its row and column rather than coerced: an
//! upload that quietly turned a typo into a number would publish a
//! plausible wrong value upstream.

use crate::core::draft::{Draft, parse_attr};
use crate::core::matrix::{MatrixModel, RowModel, RowState, read_flat_value};
use crate::core::spec::{Columns, HeaderAttr, PanelSpec, RowIdentity, ValueColumn};
use geode_core::document::{Column, DocumentRows, Value};
use geode_core::schema::ColumnType;
use geode_core::snapshot::Snapshot;

/// The document an upload sends: the base generation with the draft
/// applied, in painted order. `model` is the PAINTED model (base + draft);
/// `snapshot` is the base generation it was built from (attributes are
/// read typed from it, then overridden by `draft.attrs`).
///
/// `Deleted` rows are dropped, `Inserted` rows sit where the model spliced
/// them, and a `Minted` row axis carries the painted labels (`new-<n>`
/// included) — the kind ignores that axis on write, but `validate` and
/// the kind's vocabulary check both require it. Under `Columns::Axis` the
/// result is the kind's long form: one row per (painted row × ladder
/// column), the ladder's value then every slice value repeated across the
/// row's nodes.
///
/// `Err` names the first row and column that cannot be written: an empty
/// cell (an inserted row not yet filled), a value whose tag is not the
/// declared type, a typed row label that does not parse, or a document
/// with nothing left to send.
pub fn assemble(
    snapshot: &Snapshot,
    spec: &PanelSpec,
    model: &MatrixModel,
    draft: &Draft,
) -> Result<DocumentRows, String> {
    let attributes = spec
        .header
        .iter()
        .map(|attr| attribute(snapshot, attr, draft))
        .collect::<Result<Vec<_>, _>>()?;
    let rows: Vec<&RowModel> = model
        .rows
        .iter()
        .filter(|r| r.state != RowState::Deleted)
        .collect();
    let (axes, values) = match &spec.columns {
        Columns::Values(cols) => flat(spec, cols, &rows)?,
        Columns::Axis(axis) => long(snapshot, spec, axis, model, &rows)?,
    };
    if axes.first().is_none_or(|(_, c)| c.is_empty()) {
        return Err("nothing to upload: every row is deleted".to_string());
    }
    Ok(DocumentRows {
        key: model.key.clone(),
        attributes,
        axes,
        values,
    })
}

/// A document's named axes and named values, as `DocumentRows` holds them.
type Laid = (Vec<(String, Column)>, Vec<(String, Column)>);

/// The flat shape: one document row per painted row, one value column per
/// [`ValueColumn`] in spec order (the model's cells are in that order).
fn flat(spec: &PanelSpec, cols: &[ValueColumn], rows: &[&RowModel]) -> Result<Laid, String> {
    let mut axis = empty_column(spec.rows.column, row_axis_type(spec))?;
    let mut values = cols
        .iter()
        .map(|vc| empty_column(vc.column, vc.ty).map(|c| (vc.column.to_string(), c)))
        .collect::<Result<Vec<_>, _>>()?;
    for row in rows {
        let label = row.label.as_ref();
        put(
            &mut axis,
            label,
            spec.rows.column,
            Some(&row_axis(spec, label)?),
        )?;
        for (ci, (name, column)) in values.iter_mut().enumerate() {
            put(column, label, name, cell(row, ci))?;
        }
    }
    Ok((vec![(spec.rows.column.to_string(), axis)], values))
}

/// The pivot's long form: axes `[row axis, column axis]`, values the
/// ladder's own column then each slice value in spec order — the kind's
/// own order (CVI `param, forward, atm, skew`).
fn long(
    snapshot: &Snapshot,
    spec: &PanelSpec,
    axis: &str,
    model: &MatrixModel,
    rows: &[&RowModel],
) -> Result<Laid, String> {
    // A slice value the document did not carry is not in the model, so
    // it has nothing to write — and an upload without it is a different
    // document from the one the kind declares.
    if let Some(missing) = spec.slice_values.iter().find(|sv| {
        !model.columns[..model.slice_columns]
            .iter()
            .any(|c| c.as_ref() == sv.label)
    }) {
        return Err(format!(
            "the document carries no '{}' slice value to upload",
            missing.column
        ));
    }
    let ladder = &model.columns[model.slice_columns..];
    if model.column_values.len() != ladder.len() {
        return Err(format!(
            "the model has {} '{axis}' values for {} columns",
            model.column_values.len(),
            ladder.len()
        ));
    }
    let value_name = model
        .pivot_index
        .as_ref()
        .and_then(|index| snapshot.meta_at(index.value_idx()))
        .map(|m| m.name.clone())
        .ok_or_else(|| "the model was not built as a pivot".to_string())?;

    let mut row_axis_col = empty_column(spec.rows.column, row_axis_type(spec))?;
    let axis_type = model
        .column_values
        .first()
        .map_or(ColumnType::F64, Value::column_type);
    let mut column_axis = empty_column(axis, axis_type)?;
    let mut value = empty_column(&value_name, spec.value_type)?;
    let mut slices = spec
        .slice_values
        .iter()
        .map(|sv| empty_column(sv.column, ColumnType::F64))
        .collect::<Result<Vec<_>, _>>()?;
    for row in rows {
        let label = row.label.as_ref();
        let row_value = row_axis(spec, label)?;
        for (ci, (column_label, column_value)) in
            ladder.iter().zip(&model.column_values).enumerate()
        {
            put(&mut row_axis_col, label, spec.rows.column, Some(&row_value))?;
            put(&mut column_axis, label, axis, Some(column_value))?;
            put(
                &mut value,
                label,
                &format!("{value_name} at {axis}={column_label}"),
                cell(row, model.slice_columns + ci),
            )?;
            for (si, (sv, column)) in spec.slice_values.iter().zip(&mut slices).enumerate() {
                put(column, label, sv.column, cell(row, si))?;
            }
        }
    }
    let axes = vec![
        (spec.rows.column.to_string(), row_axis_col),
        (axis.to_string(), column_axis),
    ];
    let values = std::iter::once((value_name, value))
        .chain(
            spec.slice_values
                .iter()
                .zip(slices)
                .map(|(sv, c)| (sv.column.to_string(), c)),
        )
        .collect();
    Ok((axes, values))
}

/// One document-level attribute at its declared type: the draft's edit,
/// else the snapshot's value on its first row (an attribute is constant
/// within a document).
fn attribute(
    snapshot: &Snapshot,
    attr: &HeaderAttr,
    draft: &Draft,
) -> Result<(String, Value), String> {
    let value = match draft.attrs.get(attr.column) {
        Some(value) => value.clone(),
        None => snapshot
            .column_index(attr.column)
            .and_then(|idx| read_flat_value(snapshot, idx, 0, attr.ty))
            .ok_or_else(|| format!("the document has no '{}' attribute", attr.column))?,
    };
    if value.column_type() != attr.ty {
        return Err(format!(
            "attribute '{}' is {}, declared {}",
            attr.column,
            tag(value.column_type()),
            tag(attr.ty)
        ));
    }
    Ok((attr.column.to_string(), value))
}

fn cell(row: &RowModel, col: usize) -> Option<&Value> {
    row.cells.get(col).and_then(|c| c.value.as_ref())
}

/// The row axis's declared type: a minted label is text.
fn row_axis_type(spec: &PanelSpec) -> ColumnType {
    match spec.rows.identity {
        RowIdentity::Minted => ColumnType::Utf8,
        RowIdentity::Typed(ty) => ty,
    }
}

/// A row label as its axis's value. A typed label is parsed, never sent
/// as the text it is painted as.
fn row_axis(spec: &PanelSpec, label: &str) -> Result<Value, String> {
    match row_axis_type(spec) {
        ColumnType::Utf8 => Ok(Value::Utf8(label.to_string())),
        ColumnType::Date => chrono::NaiveDate::parse_from_str(label, "%Y-%m-%d")
            .map(Value::Date)
            .map_err(|_| format!("row '{label}': not a date")),
        ty => parse_attr(label, ty).map_err(|e| format!("row '{label}': {e}")),
    }
}

/// An empty column of a declared type. `Timestamp`/`Bool` have no
/// document column to write into.
fn empty_column(name: &str, ty: ColumnType) -> Result<Column, String> {
    match ty {
        ColumnType::F64 => Ok(Column::F64(Vec::new())),
        ColumnType::I64 => Ok(Column::I64(Vec::new())),
        ColumnType::Utf8 => Ok(Column::Utf8(Vec::new())),
        ColumnType::Date => Ok(Column::Date(Vec::new())),
        ColumnType::Timestamp | ColumnType::Bool => Err(format!(
            "'{name}' is declared {}, which a document cannot carry",
            tag(ty)
        )),
    }
}

/// Append one value to a column of its declared type: an empty cell or a
/// different tag is refused naming the row and column.
fn put(column: &mut Column, label: &str, name: &str, value: Option<&Value>) -> Result<(), String> {
    let Some(value) = value else {
        return Err(format!("row '{label}': {name} is empty"));
    };
    if !push(column, value) {
        return Err(format!(
            "row '{label}': {name} is {}, declared {}",
            tag(value.column_type()),
            tag(column.column_type())
        ));
    }
    Ok(())
}

/// `false`, writing nothing, when `value`'s tag is not `column`'s.
fn push(column: &mut Column, value: &Value) -> bool {
    match (column, value) {
        (Column::F64(c), Value::F64(v)) => c.push(*v),
        (Column::I64(c), Value::I64(v)) => c.push(*v),
        (Column::Utf8(c), Value::Utf8(v)) => c.push(v.clone()),
        (Column::Date(c), Value::Date(v)) => c.push(*v),
        _ => return false,
    }
    true
}

fn tag(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Utf8 => "utf8",
        ColumnType::F64 => "f64",
        ColumnType::I64 => "i64",
        ColumnType::Date => "date",
        ColumnType::Timestamp => "timestamp",
        ColumnType::Bool => "bool",
    }
}

/// How many rows differ between what was sent and a delivered document,
/// comparing every column except a `Minted` row axis's label; `0` means
/// the echo confirms the upload. `f64` within one ULP, everything else exact.
/// Attributes that differ count as one extra row.
///
/// The minted label is Geode's, not the wire's: the kind writes no id and
/// the echo arrives re-minted, so comparing it would make every upload
/// that inserted a row "differ". Rows are compared as a MULTISET: both
/// sides are sorted by the full tuple of compared columns and matched in
/// one merge walk. The sent rows are in painted order while the store
/// hands a document back sorted by its axes, so an out-of-order insert,
/// an ex-date edited past a neighbour or a term inserted out of date order
/// would otherwise read as differing on every successful upload. The
/// count is the larger side's unmatched rows — one changed value is one
/// row, a row only one side has counts once. The cost: an upstream that
/// only reorders rows reads as confirmed. A column one side carries and
/// the other lacks makes every row differ.
pub fn echo_differs(spec: &PanelSpec, sent: &DocumentRows, delivered: &DocumentRows) -> usize {
    let minted = spec.rows.identity == RowIdentity::Minted;
    let skipped = minted.then_some(spec.rows.column);
    let (ours, all_theirs) = (compared(sent, skipped), compared(delivered, skipped));
    // `theirs` in `ours`' column order, so one index names one column on
    // both sides; `None` when the two carry different columns.
    let theirs: Option<Vec<&Column>> = (ours.len() == all_theirs.len())
        .then(|| {
            ours.iter()
                .map(|(name, _)| all_theirs.iter().find(|(n, _)| n == name).map(|(_, c)| c))
                .collect()
        })
        .flatten();
    let ours: Vec<&Column> = ours.into_iter().map(|(_, c)| c).collect();
    let mut differs = match theirs {
        Some(theirs) => unmatched_rows(&ours, sent.rows(), &theirs, delivered.rows()),
        None => sent.rows().max(delivered.rows()),
    };
    let attrs_equal = sent.attributes.len() == delivered.attributes.len()
        && sent.attributes.iter().all(|(name, a)| {
            delivered
                .attributes
                .iter()
                .find(|(n, _)| n == name)
                .is_some_and(|(_, b)| value_eq(a, b))
        });
    if !attrs_equal {
        differs += 1;
    }
    differs
}

/// The multiset difference of two row sets over the same columns: each
/// side's row indices sorted by the exact compared tuple, then one merge
/// walk pairing rows equal within [`f64_close`]. Answers the larger side's
/// count of rows left unpaired.
fn unmatched_rows(ours: &[&Column], n_ours: usize, theirs: &[&Column], n_theirs: usize) -> usize {
    use std::cmp::Ordering;
    // The sort takes the EXACT order: the one-ULP tolerance is not
    // transitive, and a comparator that is not a total order may panic
    // in the standard sort.
    let sorted = |cols: &[&Column], n: usize| {
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by(|&i, &j| {
            cols.iter()
                .map(|c| cell_cmp(c, i, c, j))
                .find(|o| o.is_ne())
                .unwrap_or(Ordering::Equal)
        });
        idx
    };
    let (a, b) = (sorted(ours, n_ours), sorted(theirs, n_theirs));
    let (mut i, mut j) = (0, 0);
    let (mut only_ours, mut only_theirs) = (0, 0);
    while i < a.len() && j < b.len() {
        match merge_order(ours, a[i], theirs, b[j]) {
            Ordering::Equal => {
                i += 1;
                j += 1;
            }
            Ordering::Less => {
                only_ours += 1;
                i += 1;
            }
            Ordering::Greater => {
                only_theirs += 1;
                j += 1;
            }
        }
    }
    only_ours += a.len() - i;
    only_theirs += b.len() - j;
    only_ours.max(only_theirs)
}

/// Row `i` of `a` against row `j` of `b` for the merge walk: `Equal` when
/// every cell is the same value (`f64` within one ULP), otherwise the
/// exact order of the first cell that is not.
fn merge_order(a: &[&Column], i: usize, b: &[&Column], j: usize) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b) {
        if cell_eq(x, i, y, j) {
            continue;
        }
        // Not the same value, so never `Equal` — a missing cell included.
        return cell_cmp(x, i, y, j).then(std::cmp::Ordering::Less);
    }
    std::cmp::Ordering::Equal
}

/// The exact total order of two cells (`f64` by `total_cmp`); a different
/// tag orders by tag, a missing cell after a present one.
fn cell_cmp(a: &Column, i: usize, b: &Column, j: usize) -> std::cmp::Ordering {
    let rank = |c: &Column| match c {
        Column::F64(_) => 0,
        Column::I64(_) => 1,
        Column::Utf8(_) => 2,
        Column::Date(_) => 3,
    };
    fn by<T>(
        a: &[T],
        i: usize,
        b: &[T],
        j: usize,
        f: impl Fn(&T, &T) -> std::cmp::Ordering,
    ) -> std::cmp::Ordering {
        match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) => f(x, y),
            (x, y) => y.is_some().cmp(&x.is_some()),
        }
    }
    match (a, b) {
        (Column::F64(a), Column::F64(b)) => by(a, i, b, j, |x, y| x.total_cmp(y)),
        (Column::I64(a), Column::I64(b)) => by(a, i, b, j, Ord::cmp),
        (Column::Utf8(a), Column::Utf8(b)) => by(a, i, b, j, Ord::cmp),
        (Column::Date(a), Column::Date(b)) => by(a, i, b, j, Ord::cmp),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// A document's axes then values, less the column `skipped` names.
fn compared<'a>(doc: &'a DocumentRows, skipped: Option<&str>) -> Vec<&'a (String, Column)> {
    doc.axes
        .iter()
        .chain(&doc.values)
        .filter(|(name, _)| Some(name.as_str()) != skipped)
        .collect()
}

/// How far apart two `f64`s may be and still be the same number: one
/// unit in the last place, the most a text round trip through a wire
/// format can move a correctly printed and parsed double.
const ULPS: u64 = 1;

fn f64_close(a: f64, b: f64) -> bool {
    let same_sign = a.is_sign_negative() == b.is_sign_negative();
    a == b || (same_sign && a.to_bits().abs_diff(b.to_bits()) <= ULPS)
}

fn cell_eq(a: &Column, i: usize, b: &Column, j: usize) -> bool {
    match (a, b) {
        (Column::F64(a), Column::F64(b)) => a
            .get(i)
            .zip(b.get(j))
            .is_some_and(|(x, y)| f64_close(*x, *y)),
        (Column::I64(a), Column::I64(b)) => a.get(i).is_some() && a.get(i) == b.get(j),
        (Column::Utf8(a), Column::Utf8(b)) => a.get(i).is_some() && a.get(i) == b.get(j),
        (Column::Date(a), Column::Date(b)) => a.get(i).is_some() && a.get(i) == b.get(j),
        _ => false,
    }
}

fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F64(x), Value::F64(y)) => f64_close(*x, *y),
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::spec::{CVI, DIVIDEND};
    use crate::core::test_fixtures::{
        BASE, CVI_NODES, CVI_TERMS, date, fixture_cvi_rows, fixture_dividend_rows, snapshot_of,
    };
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::document::{Column, Value};
    use geode_core::schema::SchemaSpec;

    /// The two shipped datasets as `geode-documents` declares them — the
    /// shape `DocumentRows::validate` holds an assembled upload to. Copied
    /// rather than imported: this crate must not name `geode-documents`.
    const DATASETS: &str = r#"
[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]
[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
[cvi_params.columns.term]
type = "date"
role = "axis"
[cvi_params.columns.node]
type = "f64"
role = "axis"
[cvi_params.columns.param]
type = "f64"
role = "value"
[cvi_params.columns.forward]
type = "f64"
role = "value"
[cvi_params.columns.atm]
type = "f64"
role = "value"
[cvi_params.columns.skew]
type = "f64"
role = "value"
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"

[dividend_schedule]
family = "document"
key = ["underlying_ref"]
axes = ["dividend_id"]
[dividend_schedule.columns.underlying_ref]
type = "utf8"
role = "dimension"
[dividend_schedule.columns.dividend_id]
type = "utf8"
role = "axis"
[dividend_schedule.columns.ex_date]
type = "date"
role = "value"
[dividend_schedule.columns.announced_date]
type = "date"
role = "value"
[dividend_schedule.columns.pay_date]
type = "date"
role = "value"
[dividend_schedule.columns.amount]
type = "f64"
role = "value"
[dividend_schedule.columns.status]
type = "utf8"
role = "value"
[dividend_schedule.columns.currency]
type = "utf8"
role = "attribute"
[dividend_schedule.columns.schedule_date]
type = "date"
role = "attribute"
"#;

    fn assert_valid(spec: &PanelSpec, doc: &DocumentRows) {
        let merged = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", DATASETS).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&merged);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset(spec.dataset).expect("declared above");
        assert_eq!(doc.validate(ds), Ok(()), "{}", spec.kind);
    }

    fn built(spec: &PanelSpec, doc: &DocumentRows, draft: &Draft) -> (Snapshot, MatrixModel) {
        let snapshot = snapshot_of(spec, doc);
        let model = MatrixModel::build(&snapshot, spec, draft).unwrap();
        (snapshot, model)
    }

    fn utf8(labels: &[&str]) -> Column {
        Column::Utf8(labels.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn an_empty_draft_assembles_the_base_document_exactly() {
        for (spec, doc) in [
            (&CVI, fixture_cvi_rows()),
            (&DIVIDEND, fixture_dividend_rows()),
        ] {
            let snapshot = snapshot_of(spec, &doc);
            let model = MatrixModel::build(&snapshot, spec, &Draft::default()).unwrap();
            let assembled = assemble(&snapshot, spec, &model, &Draft::default()).unwrap();
            assert_eq!(assembled, doc, "{}", spec.kind);
            assert_valid(spec, &assembled);
        }
    }

    /// Base rows A, B, C: B's amount edited, C deleted, `new-1` inserted
    /// under A with every cell set. The upload is the painted order with
    /// the deleted row gone, the minted label written into the axis
    /// (amendment 3), and the attributes read typed off the snapshot.
    #[test]
    fn a_dividend_draft_assembles_edits_deletes_and_inserts_in_painted_order() {
        let base = fixture_dividend_rows();
        let mut draft = Draft::default();
        draft.set((1, 3), ("B".into(), "amount".into()), Value::F64(1.5), BASE);
        draft.delete_row("C", BASE);
        draft.insert_row("new-1".into(), Some("A".into()), BASE);
        for (column, value) in [
            ("ex", Value::Date(date(2026, 11, 20))),
            ("announced", Value::Date(date(2026, 10, 15))),
            ("pay", Value::Date(date(2026, 12, 1))),
            ("amount", Value::F64(0.75)),
            ("status", Value::Utf8("estimated".into())),
        ] {
            assert!(draft.set_row_cell("new-1", column, value));
        }
        let (snapshot, model) = built(&DIVIDEND, &base, &draft);
        let sent = assemble(&snapshot, &DIVIDEND, &model, &draft).unwrap();

        let expected = DocumentRows {
            key: vec!["SPX.Z".into()],
            attributes: base.attributes.clone(),
            axes: vec![("dividend_id".into(), utf8(&["A", "new-1", "B"]))],
            values: vec![
                (
                    "ex_date".into(),
                    Column::Date(vec![
                        date(2026, 9, 18),
                        date(2026, 11, 20),
                        date(2026, 12, 18),
                    ]),
                ),
                (
                    "announced_date".into(),
                    Column::Date(vec![
                        date(2026, 8, 1),
                        date(2026, 10, 15),
                        date(2026, 11, 1),
                    ]),
                ),
                (
                    "pay_date".into(),
                    Column::Date(vec![date(2026, 10, 1), date(2026, 12, 1), date(2027, 1, 4)]),
                ),
                ("amount".into(), Column::F64(vec![1.25, 0.75, 1.5])),
                ("status".into(), utf8(&["paid", "estimated", "declared"])),
            ],
        };
        assert_eq!(sent, expected);
        assert_valid(&DIVIDEND, &sent);
    }

    /// An attribute edit overrides the snapshot's value, typed.
    #[test]
    fn an_attribute_edit_replaces_the_snapshots_value() {
        let base = fixture_dividend_rows();
        let mut draft = Draft::default();
        draft.set_attr("currency", Value::Utf8("EUR".into()), BASE);
        let (snapshot, model) = built(&DIVIDEND, &base, &draft);
        let sent = assemble(&snapshot, &DIVIDEND, &model, &draft).unwrap();
        assert_eq!(
            sent.attributes,
            vec![
                ("currency".to_string(), Value::Utf8("EUR".into())),
                ("schedule_date".to_string(), Value::Date(date(2026, 9, 1))),
            ]
        );
        assert_eq!(sent.values, base.values);
    }

    /// CVI's long form: one row per (term × node), the node's TYPED value
    /// in the axis, and the slice values repeated on every node row of
    /// their term — an inserted term included, spliced where it was put.
    #[test]
    fn a_cvi_draft_assembles_the_long_form_with_an_inserted_term() {
        let base = fixture_cvi_rows();
        let mut draft = Draft::default();
        // The first term's node -1 (grid column 3 + 1).
        draft.set(
            (0, 4),
            ("2026-10-16".into(), "-1".into()),
            Value::F64(0.25),
            BASE,
        );
        draft.insert_row("2026-12-18".into(), Some("2026-11-20".into()), BASE);
        for (column, value) in [
            ("fwd", 4600.0),
            ("atm", 0.2),
            ("skew", -1.0),
            ("-20", 0.7),
            ("-1", 0.8),
            ("3.5", 0.9),
        ] {
            assert!(draft.set_row_cell("2026-12-18", column, Value::F64(value)));
        }
        let (snapshot, model) = built(&CVI, &base, &draft);
        let sent = assemble(&snapshot, &CVI, &model, &draft).unwrap();

        let terms: Vec<_> = CVI_TERMS
            .iter()
            .chain(["2026-12-18"].iter())
            .flat_map(|t| {
                std::iter::repeat_n(chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d").unwrap(), 3)
            })
            .collect();
        let nodes: Vec<f64> = (0..3).flat_map(|_| CVI_NODES).collect();
        assert_eq!(sent.axes[0], ("term".to_string(), Column::Date(terms)));
        assert_eq!(sent.axes[1], ("node".to_string(), Column::F64(nodes)));
        assert_eq!(
            sent.values,
            vec![
                (
                    "param".to_string(),
                    Column::F64(vec![0.1, 0.25, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9]),
                ),
                (
                    "forward".to_string(),
                    Column::F64(vec![
                        4512.3, 4512.3, 4512.3, 4530.75, 4530.75, 4530.75, 4600.0, 4600.0, 4600.0,
                    ]),
                ),
                (
                    "atm".to_string(),
                    Column::F64(vec![0.182, 0.182, 0.182, 0.19, 0.19, 0.19, 0.2, 0.2, 0.2]),
                ),
                (
                    "skew".to_string(),
                    Column::F64(vec![
                        -1.1, -1.1, -1.1, -0.95, -0.95, -0.95, -1.0, -1.0, -1.0
                    ]),
                ),
            ]
        );
        assert_eq!(sent.attributes, base.attributes);
        assert_valid(&CVI, &sent);
    }

    #[test]
    fn assembly_refuses_an_empty_cell_and_a_wrong_tag_naming_the_row() {
        let base = fixture_dividend_rows();

        // An inserted row with only its amount filled: the first empty
        // column in the dataset's order is named.
        let mut draft = Draft::default();
        draft.insert_row("new-1".into(), Some("A".into()), BASE);
        assert!(draft.set_row_cell("new-1", "amount", Value::F64(0.5)));
        let (snapshot, model) = built(&DIVIDEND, &base, &draft);
        assert_eq!(
            assemble(&snapshot, &DIVIDEND, &model, &draft),
            Err("row 'new-1': ex_date is empty".to_string())
        );

        // A text value in an f64 column is refused, never coerced.
        let mut draft = Draft::default();
        draft.set(
            (1, 3),
            ("B".into(), "amount".into()),
            Value::Utf8("lots".into()),
            BASE,
        );
        let (snapshot, model) = built(&DIVIDEND, &base, &draft);
        assert_eq!(
            assemble(&snapshot, &DIVIDEND, &model, &draft),
            Err("row 'B': amount is utf8, declared f64".to_string())
        );

        // Nothing left once every row is deleted: refused, never an
        // empty document.
        let mut draft = Draft::default();
        for label in ["A", "B", "C"] {
            draft.delete_row(label, BASE);
        }
        let (snapshot, model) = built(&DIVIDEND, &base, &draft);
        assert_eq!(
            assemble(&snapshot, &DIVIDEND, &model, &draft),
            Err("nothing to upload: every row is deleted".to_string())
        );

        // A wrong-typed attribute edit likewise.
        let mut draft = Draft::default();
        draft.set_attr("schedule_date", Value::Utf8("soon".into()), BASE);
        let (snapshot, model) = built(&DIVIDEND, &base, &draft);
        assert_eq!(
            assemble(&snapshot, &DIVIDEND, &model, &draft),
            Err("attribute 'schedule_date' is utf8, declared date".to_string())
        );
    }

    /// A `Typed(Date)` row label that does not parse is refused naming it
    /// — the term axis is a date on the wire, not a string.
    #[test]
    fn assembly_refuses_a_typed_row_label_that_is_not_a_date() {
        let base = fixture_cvi_rows();
        let mut draft = Draft::default();
        draft.insert_row("soon".into(), Some("2026-11-20".into()), BASE);
        for column in ["fwd", "atm", "skew", "-20", "-1", "3.5"] {
            assert!(draft.set_row_cell("soon", column, Value::F64(1.0)));
        }
        let (snapshot, model) = built(&CVI, &base, &draft);
        assert_eq!(
            assemble(&snapshot, &CVI, &model, &draft),
            Err("row 'soon': not a date".to_string())
        );
    }

    #[test]
    fn echo_ignores_the_minted_label_and_counts_differing_rows() {
        let sent = fixture_dividend_rows();
        let mut echoed = sent.clone();
        echoed.axes[0].1 = Column::Utf8(vec!["x".into(); sent.rows()]);
        assert_eq!(echo_differs(&DIVIDEND, &sent, &echoed), 0);
        if let Column::F64(v) = &mut echoed.values[3].1 {
            v[0] += 1.0;
        }
        assert_eq!(echo_differs(&DIVIDEND, &sent, &echoed), 1);
        if let Column::Utf8(v) = &mut echoed.values[4].1 {
            v[2] = "cancelled".into();
        }
        assert_eq!(echo_differs(&DIVIDEND, &sent, &echoed), 2);
    }

    /// A differing attribute is one extra row; each row one side lacks
    /// counts once.
    #[test]
    fn echo_counts_a_differing_attribute_once_and_each_unmatched_row() {
        let sent = fixture_dividend_rows();
        let mut echoed = sent.clone();
        echoed.attributes[0].1 = Value::Utf8("EUR".into());
        assert_eq!(echo_differs(&DIVIDEND, &sent, &echoed), 1);

        let mut shorter = sent.clone();
        for (_, col) in shorter.axes.iter_mut().chain(shorter.values.iter_mut()) {
            match col {
                Column::F64(v) => v.truncate(1),
                Column::I64(v) => v.truncate(1),
                Column::Utf8(v) => v.truncate(1),
                Column::Date(v) => v.truncate(1),
            }
        }
        assert_eq!(echo_differs(&DIVIDEND, &sent, &shorter), 2);
        assert_eq!(echo_differs(&DIVIDEND, &shorter, &sent), 2);
    }

    #[test]
    fn echo_accepts_one_ulp_and_refuses_two() {
        let sent = fixture_dividend_rows();
        let Column::F64(amounts) = &sent.values[3].1 else {
            panic!("amount is f64");
        };
        let bits = amounts[1].to_bits();
        for (step, differs) in [(1, 0), (2, 1)] {
            let mut echoed = sent.clone();
            if let Column::F64(v) = &mut echoed.values[3].1 {
                v[1] = f64::from_bits(bits + step);
            }
            assert_eq!(
                echo_differs(&DIVIDEND, &sent, &echoed),
                differs,
                "{step} ulp"
            );
        }
    }

    /// The store hands a document back sorted by its axes; the sent rows
    /// are in painted order. The same rows in another order confirm, and a
    /// changed value among reordered rows is still one row.
    #[test]
    fn echo_compares_rows_as_a_multiset_not_by_position() {
        let sent = fixture_dividend_rows();
        let n = sent.rows();
        assert!(n >= 3, "the fixture has rows to reorder");
        let order: Vec<usize> = (0..n).rev().collect();
        let mut reordered = sent.clone();
        for (_, col) in reordered.axes.iter_mut().chain(reordered.values.iter_mut()) {
            *col = match col {
                Column::F64(v) => Column::F64(order.iter().map(|&i| v[i]).collect()),
                Column::I64(v) => Column::I64(order.iter().map(|&i| v[i]).collect()),
                Column::Utf8(v) => Column::Utf8(order.iter().map(|&i| v[i].clone()).collect()),
                Column::Date(v) => Column::Date(order.iter().map(|&i| v[i]).collect()),
            };
        }
        assert_eq!(echo_differs(&DIVIDEND, &sent, &reordered), 0);
        assert_eq!(echo_differs(&DIVIDEND, &reordered, &sent), 0);
        // CVI's typed axis too: the long form reversed.
        let cvi = fixture_cvi_rows();
        let m = cvi.rows();
        let back: Vec<usize> = (0..m).rev().collect();
        let mut cvi_rev = cvi.clone();
        for (_, col) in cvi_rev.axes.iter_mut().chain(cvi_rev.values.iter_mut()) {
            *col = match col {
                Column::F64(v) => Column::F64(back.iter().map(|&i| v[i]).collect()),
                Column::I64(v) => Column::I64(back.iter().map(|&i| v[i]).collect()),
                Column::Utf8(v) => Column::Utf8(back.iter().map(|&i| v[i].clone()).collect()),
                Column::Date(v) => Column::Date(back.iter().map(|&i| v[i]).collect()),
            };
        }
        assert_eq!(echo_differs(&CVI, &cvi, &cvi_rev), 0);

        if let Column::F64(v) = &mut reordered.values[3].1 {
            v[0] += 1.0;
        }
        assert_eq!(echo_differs(&DIVIDEND, &sent, &reordered), 1);
    }

    /// CVI's row axis is typed, not minted: a term is the wire's own
    /// value, so a changed one is a differing row.
    #[test]
    fn echo_compares_a_typed_label_on_cvi() {
        let sent = fixture_cvi_rows();
        assert_eq!(echo_differs(&CVI, &sent, &sent.clone()), 0);
        let mut echoed = sent.clone();
        if let Column::Date(v) = &mut echoed.axes[0].1 {
            v[0] = date(2026, 10, 17);
        }
        assert_eq!(echo_differs(&CVI, &sent, &echoed), 1);
    }
}
