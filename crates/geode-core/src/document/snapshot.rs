//! A document answer (`DataHandle::document`, a full key or a key prefix)
//! back into the [`DocumentRows`] it was published from: one per distinct
//! key tuple, in the answer's row order, columns decoded by the dataset's
//! declared roles and types. The data tier stores every document of a
//! dataset in one table with the key and the attributes repeated on each
//! row, and a prefix read returns every document under the prefix in one
//! snapshot, so the split by key tuple is this module's to make.
//!
//! The decoder walks the dataset's declaration, never the answer's columns:
//! a declared column the answer lacks, or carries at another type or with a
//! NULL, is an error naming it, never a partial document, because a
//! pipeline fed a plausible half-document would fit it without complaint.

use super::{Column, DocumentRows, Value, cell_text};
use crate::schema::{ColumnRole, ColumnType, DatasetSpec};
use crate::snapshot::Snapshot;

fn column_present(snapshot: &Snapshot, name: &str) -> Result<(), String> {
    snapshot
        .column_index(name)
        .map(|_| ())
        .ok_or_else(|| format!("column '{name}' is missing from the answer"))
}

/// One declared column over every row of the answer. The type check is
/// the strict one (`i64_column`, `f64_column` and `date_column` match
/// exactly Int64, Float64 and Date32); every cell then goes through the
/// per-row accessor, which answers `None` for a NULL — a NULL is refused,
/// never read as a zero or the epoch.
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
        ColumnType::Date => {
            if snapshot.date_column(name).is_none() {
                return Err(format!("column '{name}' is not date"));
            }
            (0..n)
                .map(|r| snapshot.date_value(name, r).ok_or_else(|| null(r)))
                .collect::<Result<_, _>>()
                .map(Column::Date)
        }
        other => Err(format!(
            "column '{name}' is declared {other:?}, which a document never stores"
        )),
    }
}

/// The rows `lo..hi` of a decoded column.
fn slice(col: &Column, lo: usize, hi: usize) -> Column {
    match col {
        Column::F64(v) => Column::F64(v[lo..hi].to_vec()),
        Column::I64(v) => Column::I64(v[lo..hi].to_vec()),
        Column::Utf8(v) => Column::Utf8(v[lo..hi].to_vec()),
        Column::Date(v) => Column::Date(v[lo..hi].to_vec()),
    }
}

/// An attribute is repeated on every row of its document; it is read from
/// the document's first row only after every row of the document agrees.
/// Rows that disagree are a store fault, and picking one would be a
/// plausible wrong document.
fn attribute_of(col: &Column, name: &str, lo: usize, hi: usize) -> Result<Value, String> {
    let differs = || format!("attribute '{name}' differs between rows");
    Ok(match col {
        Column::Utf8(v) => {
            if v[lo..hi].iter().any(|x| *x != v[lo]) {
                return Err(differs());
            }
            Value::Utf8(v[lo].clone())
        }
        Column::I64(v) => {
            if v[lo..hi].iter().any(|x| *x != v[lo]) {
                return Err(differs());
            }
            Value::I64(v[lo])
        }
        Column::F64(v) => {
            if v[lo..hi].iter().any(|x| x.to_bits() != v[lo].to_bits()) {
                return Err(differs());
            }
            Value::F64(v[lo])
        }
        Column::Date(v) => {
            if v[lo..hi].iter().any(|x| *x != v[lo]) {
                return Err(differs());
            }
            Value::Date(v[lo])
        }
    })
}

/// Every document a (possibly prefix-keyed) snapshot carries, one per
/// distinct key tuple in row order, columns decoded by the dataset's
/// roles. `Ok(empty)` for zero rows (no document under the key); `Err`
/// names the first bad column or row. The answer orders rows by the open
/// key parts, then the axes, so a document's rows are one run; a key
/// tuple that recurs after another's rows is refused rather than merged,
/// since it would mean two documents under one key.
pub fn documents_from_snapshot(
    ds: &DatasetSpec,
    snapshot: &Snapshot,
) -> Result<Vec<DocumentRows>, String> {
    if !ds.is_document() {
        return Err(format!("dataset '{}' is not a document dataset", ds.name));
    }
    let n = snapshot.rows();
    if n == 0 {
        return Ok(Vec::new());
    }
    let mut keys: Vec<Column> = Vec::with_capacity(ds.key.len());
    let mut axes: Vec<(&str, Column)> = Vec::new();
    let mut values: Vec<(&str, Column)> = Vec::new();
    let mut attributes: Vec<(&str, Column)> = Vec::new();
    for spec in ds.document_columns() {
        let col = decode_column(snapshot, &spec.name, spec.ty)?;
        if ds.key.contains(&spec.name) {
            keys.push(col);
            continue;
        }
        match spec.role {
            ColumnRole::Attribute { .. } => attributes.push((&spec.name, col)),
            ColumnRole::Axis { .. } => axes.push((&spec.name, col)),
            ColumnRole::Value => values.push((&spec.name, col)),
            _ => {
                return Err(format!(
                    "column '{}' has a role a document never declares",
                    spec.name
                ));
            }
        }
    }
    let key_at = |row: usize| -> Vec<String> { keys.iter().map(|k| cell_text(k, row)).collect() };
    // Each document's row range, in first-seen order.
    let mut runs: Vec<(Vec<String>, usize, usize)> = Vec::new();
    for row in 0..n {
        let key = key_at(row);
        match runs.last_mut() {
            Some((last, _, hi)) if *last == key => *hi = row + 1,
            _ => {
                if runs.iter().any(|(k, _, _)| *k == key) {
                    return Err(format!(
                        "rows for key {} are not contiguous (row {row})",
                        key.join("/")
                    ));
                }
                runs.push((key, row, row + 1));
            }
        }
    }
    runs.into_iter()
        .map(|(key, lo, hi)| {
            Ok(DocumentRows {
                key,
                attributes: attributes
                    .iter()
                    .map(|(name, col)| Ok((name.to_string(), attribute_of(col, name, lo, hi)?)))
                    .collect::<Result<_, String>>()?,
                axes: axes
                    .iter()
                    .map(|(name, col)| (name.to_string(), slice(col, lo, hi)))
                    .collect(),
                values: values
                    .iter()
                    .map(|(name, col)| (name.to_string(), slice(col, lo, hi)))
                    .collect(),
            })
        })
        .collect()
}

/// Fixture builders for downstream tests: a snapshot in the shape a
/// document answer arrives in.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use super::*;
    use crate::attribution::{Attribution, ScopeSemantics};
    use crate::snapshot::{ColumnMeta, TestColumn};

    fn meta(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::DeterminedNonAdditive],
            scope_semantics: ScopeSemantics::Direct,
            summable: false,
            mixed_flag: None,
        }
    }

    fn cells(col: &Column) -> TestColumn {
        match col {
            Column::F64(v) => TestColumn::F64(v.iter().map(|x| Some(*x)).collect()),
            Column::I64(v) => TestColumn::I64(v.clone()),
            Column::Utf8(v) => TestColumn::Dict(v.iter().map(|s| Some(s.clone())).collect()),
            Column::Date(v) => TestColumn::Date(v.iter().map(|d| Some(*d)).collect()),
        }
    }

    fn repeated(value: &Value, n: usize) -> TestColumn {
        match value {
            Value::F64(x) => TestColumn::F64(vec![Some(*x); n]),
            Value::I64(x) => TestColumn::I64(vec![*x; n]),
            Value::Utf8(s) => TestColumn::Dict(vec![Some(s.clone()); n]),
            Value::Date(d) => TestColumn::Date(vec![Some(*d); n]),
        }
    }

    fn append(into: &mut TestColumn, more: TestColumn) {
        match (into, more) {
            (TestColumn::F64(a), TestColumn::F64(b)) => a.extend(b),
            (TestColumn::I64(a), TestColumn::I64(b)) => a.extend(b),
            (TestColumn::Dict(a), TestColumn::Dict(b)) => a.extend(b),
            (TestColumn::Date(a), TestColumn::Date(b)) => a.extend(b),
            _ => panic!("the documents of one dataset share their column types"),
        }
    }

    /// `docs` as the data tier answers a read of `ds`: the declared columns
    /// in `document_columns()` order, every document's rows contiguous in
    /// the given order, its key and attributes repeated on each row. Utf8
    /// columns are dictionary-encoded, as a live read returns them. `edit`
    /// may drop, retype or reorder a column before the snapshot is built.
    pub fn snapshot_of_documents(
        ds: &DatasetSpec,
        docs: &[&DocumentRows],
        edit: impl FnOnce(&mut Vec<(ColumnMeta, TestColumn)>),
    ) -> Snapshot {
        let mut columns: Vec<(ColumnMeta, TestColumn)> = Vec::new();
        for spec in ds.document_columns() {
            let mut col: Option<TestColumn> = None;
            for doc in docs {
                let n = doc.rows();
                let part = if let Some(i) = ds.key.iter().position(|k| *k == spec.name) {
                    TestColumn::Dict(vec![Some(doc.key[i].clone()); n])
                } else if let Some((_, c)) = doc
                    .axes
                    .iter()
                    .chain(doc.values.iter())
                    .find(|(name, _)| *name == spec.name)
                {
                    cells(c)
                } else if let Some((_, v)) =
                    doc.attributes.iter().find(|(name, _)| *name == spec.name)
                {
                    repeated(v, n)
                } else {
                    panic!("document lacks the declared column '{}'", spec.name)
                };
                match &mut col {
                    None => col = Some(part),
                    Some(c) => append(c, part),
                }
            }
            columns.push((
                meta(&spec.name),
                col.unwrap_or_else(|| TestColumn::F64(Vec::new())),
            ));
        }
        edit(&mut columns);
        Snapshot::for_tests(columns, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::snapshot_of_documents;
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;
    use crate::snapshot::{ColumnMeta, TestColumn};
    use chrono::NaiveDate;

    /// An option chain: a two-part key, one axis, two values and two
    /// attributes, one typed as a date to exercise Date32 decoding.
    const CHAIN: &str = r#"
[option_chain]
family = "document"
key = ["underlying_ref", "expiry"]
axes = ["strike"]
[option_chain.columns.underlying_ref]
type = "utf8"
role = "dimension"
[option_chain.columns.expiry]
type = "utf8"
role = "dimension"
[option_chain.columns.strike]
type = "f64"
role = "axis"
[option_chain.columns.bid]
type = "f64"
role = "value"
[option_chain.columns.lots]
type = "i64"
role = "value"
[option_chain.columns.spot_ref]
type = "f64"
role = "attribute"
[option_chain.columns.quote_date]
type = "date"
role = "attribute"
"#;

    type Columns = Vec<(ColumnMeta, TestColumn)>;

    fn chain_dataset() -> DatasetSpec {
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", CHAIN).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema.dataset("option_chain").unwrap().clone()
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn expiry(expiry: &str, strikes: &[f64], spot: f64) -> DocumentRows {
        DocumentRows {
            key: vec!["SPX".into(), expiry.into()],
            attributes: vec![
                ("spot_ref".into(), Value::F64(spot)),
                ("quote_date".into(), Value::Date(d("2026-11-09"))),
            ],
            axes: vec![("strike".into(), Column::F64(strikes.to_vec()))],
            values: vec![
                (
                    "bid".into(),
                    Column::F64(strikes.iter().map(|k| k / 100.0).collect()),
                ),
                (
                    "lots".into(),
                    Column::I64((0..strikes.len() as i64).collect()),
                ),
            ],
        }
    }

    #[test]
    fn documents_from_snapshot_splits_a_prefix_snapshot_by_key_tuple() {
        let ds = chain_dataset();
        let dec = expiry("2026-12-18", &[4800.0, 5000.0, 5200.0], 5010.0);
        let jan = expiry("2027-01-15", &[4900.0, 5100.0], 5010.0);
        let snapshot = snapshot_of_documents(&ds, &[&dec, &jan], |_| {});
        assert_eq!(snapshot.rows(), 5);
        let docs = documents_from_snapshot(&ds, &snapshot).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(docs.len(), 2);
        assert_eq!(
            docs[0].key,
            vec!["SPX".to_string(), "2026-12-18".to_string()]
        );
        assert_eq!(
            docs[1].key,
            vec!["SPX".to_string(), "2027-01-15".to_string()]
        );
        assert_eq!(docs[0].rows(), 3);
        assert_eq!(docs[1].rows(), 2);
        // Every column is split at the document boundary, axes and values
        // alike, and the whole round trip is exact.
        assert_eq!(docs[0], dec);
        assert_eq!(docs[1], jan);
        assert_eq!(docs[1].values[1].1, Column::I64(vec![0, 1]));
    }

    #[test]
    fn documents_from_snapshot_decodes_attributes_once_per_document_and_refuses_a_disagreeing_attribute()
     {
        let ds = chain_dataset();
        // Two documents with different attribute values: each is read once,
        // from its own rows, not from row 0 of the answer.
        let dec = expiry("2026-12-18", &[4800.0, 5000.0], 5010.0);
        let jan = expiry("2027-01-15", &[4900.0, 5100.0], 5020.0);
        let docs = documents_from_snapshot(&ds, &snapshot_of_documents(&ds, &[&dec, &jan], |_| {}))
            .unwrap();
        assert_eq!(
            docs[0].attributes[0],
            ("spot_ref".into(), Value::F64(5010.0))
        );
        assert_eq!(
            docs[1].attributes[0],
            ("spot_ref".into(), Value::F64(5020.0))
        );
        assert_eq!(
            docs[1].attributes[1],
            ("quote_date".into(), Value::Date(d("2026-11-09")))
        );
        // A value that changes within one document's rows is a store fault
        // named by column, never a choice of row.
        let snapshot = snapshot_of_documents(&ds, &[&dec, &jan], |cols| {
            let (_, col) = cols.iter_mut().find(|(m, _)| m.name == "spot_ref").unwrap();
            let TestColumn::F64(v) = col else {
                panic!("spot_ref is f64")
            };
            v[3] = Some(5021.0);
        });
        let e = documents_from_snapshot(&ds, &snapshot).unwrap_err();
        assert!(e.contains("attribute 'spot_ref' differs"), "{e}");
        let snapshot = snapshot_of_documents(&ds, &[&dec, &jan], |cols| {
            let (_, col) = cols
                .iter_mut()
                .find(|(m, _)| m.name == "quote_date")
                .unwrap();
            let TestColumn::Date(v) = col else {
                panic!("quote_date is a date")
            };
            v[1] = Some(d("2026-11-10"));
        });
        let e = documents_from_snapshot(&ds, &snapshot).unwrap_err();
        assert!(e.contains("attribute 'quote_date' differs"), "{e}");
    }

    #[test]
    fn an_empty_snapshot_is_no_documents() {
        let ds = chain_dataset();
        let empty = snapshot_of_documents(&ds, &[], |_| {});
        assert_eq!(empty.rows(), 0);
        assert_eq!(documents_from_snapshot(&ds, &empty), Ok(Vec::new()));
    }

    #[test]
    fn a_missing_wrong_typed_or_null_column_is_refused_by_name() {
        let ds = chain_dataset();
        let dec = expiry("2026-12-18", &[4800.0, 5000.0], 5010.0);
        let refused = |edit: &dyn Fn(&mut Columns)| {
            documents_from_snapshot(&ds, &snapshot_of_documents(&ds, &[&dec], |c| edit(c)))
                .expect_err("refused, never a half-document")
        };
        for name in ["expiry", "strike", "bid", "lots", "spot_ref", "quote_date"] {
            let e = refused(&|c| c.retain(|(m, _)| m.name != name));
            assert!(e.contains(&format!("'{name}' is missing")), "{name}: {e}");
        }
        // Columns in `document_columns()` order: underlying_ref, expiry,
        // strike, bid, lots, spot_ref, quote_date.
        let e = refused(&|c| c[2].1 = TestColumn::I64(vec![1, 2]));
        assert!(e.contains("'strike' is not f64"), "{e}");
        let e = refused(&|c| c[4].1 = TestColumn::F64(vec![Some(1.0); 2]));
        assert!(e.contains("'lots' is not i64"), "{e}");
        let e = refused(&|c| c[6].1 = TestColumn::Dict(vec![Some("2026-11-09".into()); 2]));
        assert!(e.contains("'quote_date' is not date"), "{e}");
        let e = refused(&|c| c[1].1 = TestColumn::F64(vec![Some(1.0); 2]));
        assert!(e.contains("'expiry' is not utf8"), "{e}");
        let e = refused(&|c| c[3].1 = TestColumn::F64(vec![Some(1.0), None]));
        assert!(e.contains("'bid' is NULL at row 1"), "{e}");
        let e = refused(&|c| c[6].1 = TestColumn::Date(vec![None, Some(d("2026-11-09"))]));
        assert!(e.contains("'quote_date' is NULL at row 0"), "{e}");
    }

    /// Rows of one key split by another key's rows would mean two
    /// documents under one key; refused, not merged.
    #[test]
    fn a_key_tuple_that_recurs_after_another_is_refused() {
        let ds = chain_dataset();
        let dec = expiry("2026-12-18", &[4800.0], 5010.0);
        let jan = expiry("2027-01-15", &[4900.0], 5010.0);
        let snapshot = snapshot_of_documents(&ds, &[&dec, &jan, &dec], |_| {});
        let e = documents_from_snapshot(&ds, &snapshot).unwrap_err();
        assert!(e.contains("not contiguous"), "{e}");
        assert!(e.contains("2026-12-18"), "{e}");
    }

    #[test]
    fn a_measure_dataset_is_refused() {
        let mut ds = chain_dataset();
        ds.family = crate::schema::Family::Measures;
        let dec = expiry("2026-12-18", &[4800.0], 5010.0);
        let snapshot = snapshot_of_documents(&chain_dataset(), &[&dec], |_| {});
        assert!(
            documents_from_snapshot(&ds, &snapshot)
                .unwrap_err()
                .contains("not a document dataset")
        );
    }
}
