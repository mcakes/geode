//! Whole-table snapshots for reference datasets. An adapter answers a
//! snapshot query with [`TableRows`]; [`TableRows::conform`] checks it
//! against the declaration and returns the rows in storage order, sorted by
//! key, before anything is written. On the read side, [`ReferenceData`]
//! holds the live tables consumers look cells up in. Pure: no I/O.

use std::collections::BTreeMap;

use chrono::NaiveDate;

use crate::query::ReferenceTable;
use crate::schema::{ColumnType, DatasetSpec};

/// One nullable column. SQL sources return NULLs, so every cell is optional;
/// key columns are checked non-null by [`TableRows::conform`].
#[derive(Debug, Clone, PartialEq)]
pub enum RefColumn {
    Utf8(Vec<Option<String>>),
    F64(Vec<Option<f64>>),
    I64(Vec<Option<i64>>),
    Date(Vec<Option<NaiveDate>>),
    Bool(Vec<Option<bool>>),
}

impl RefColumn {
    pub fn len(&self) -> usize {
        match self {
            RefColumn::Utf8(v) => v.len(),
            RefColumn::F64(v) => v.len(),
            RefColumn::I64(v) => v.len(),
            RefColumn::Date(v) => v.len(),
            RefColumn::Bool(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn column_type(&self) -> ColumnType {
        match self {
            RefColumn::Utf8(_) => ColumnType::Utf8,
            RefColumn::F64(_) => ColumnType::F64,
            RefColumn::I64(_) => ColumnType::I64,
            RefColumn::Date(_) => ColumnType::Date,
            RefColumn::Bool(_) => ColumnType::Bool,
        }
    }

    /// An all-NULL column of a declared type, for a column the source omitted.
    /// `Timestamp` never reaches here: the schema refuses it on this family.
    pub fn nulls(ty: ColumnType, n: usize) -> RefColumn {
        match ty {
            ColumnType::Utf8 | ColumnType::Timestamp => RefColumn::Utf8(vec![None; n]),
            ColumnType::F64 => RefColumn::F64(vec![None; n]),
            ColumnType::I64 => RefColumn::I64(vec![None; n]),
            ColumnType::Date => RefColumn::Date(vec![None; n]),
            ColumnType::Bool => RefColumn::Bool(vec![None; n]),
        }
    }

    fn is_null(&self, i: usize) -> bool {
        match self {
            RefColumn::Utf8(v) => v[i].is_none(),
            RefColumn::F64(v) => v[i].is_none(),
            RefColumn::I64(v) => v[i].is_none(),
            RefColumn::Date(v) => v[i].is_none(),
            RefColumn::Bool(v) => v[i].is_none(),
        }
    }

    fn permuted(&self, order: &[usize]) -> RefColumn {
        fn p<T: Clone>(v: &[T], order: &[usize]) -> Vec<T> {
            order.iter().map(|&i| v[i].clone()).collect()
        }
        match self {
            RefColumn::Utf8(v) => RefColumn::Utf8(p(v, order)),
            RefColumn::F64(v) => RefColumn::F64(p(v, order)),
            RefColumn::I64(v) => RefColumn::I64(p(v, order)),
            RefColumn::Date(v) => RefColumn::Date(p(v, order)),
            RefColumn::Bool(v) => RefColumn::Bool(p(v, order)),
        }
    }
}

/// A snapshot as the adapter returned it: named columns in any order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TableRows {
    pub columns: Vec<(String, RefColumn)>,
}

/// Rows ready to stage: one column per `DatasetSpec::document_columns()`
/// entry, in that order, rows sorted by key. `extra` and `missing` name the
/// undeclared columns ignored and the optional columns read as NULL, for
/// the once-per-combination warning.
#[derive(Debug, Clone, PartialEq)]
pub struct ConformedRows {
    pub columns: Vec<RefColumn>,
    pub rows: usize,
    pub extra: Vec<String>,
    pub missing: Vec<String>,
}

fn type_name(t: ColumnType) -> &'static str {
    match t {
        ColumnType::Utf8 => "utf8",
        ColumnType::F64 => "f64",
        ColumnType::I64 => "i64",
        ColumnType::Date => "date",
        ColumnType::Timestamp => "timestamp",
        ColumnType::Bool => "bool",
    }
}

impl TableRows {
    /// Check against the declaration and return storage-ordered rows.
    /// Refuses a repeated column, unequal lengths, no rows, a missing key
    /// column, a type mismatch, a non-finite number, a NULL key and a
    /// repeated key. Every refusal leaves the live table as it was, so the
    /// message says what the source must fix.
    pub fn conform(&self, ds: &DatasetSpec) -> Result<ConformedRows, String> {
        for (i, (name, _)) in self.columns.iter().enumerate() {
            if self.columns[..i].iter().any(|(n, _)| n == name) {
                return Err(format!("column '{name}' appears twice"));
            }
        }
        let rows = self.columns.first().map_or(0, |(_, c)| c.len());
        if let Some((name, c)) = self.columns.iter().find(|(_, c)| c.len() != rows) {
            return Err(format!(
                "column '{name}' has {} rows; the first column has {rows}",
                c.len()
            ));
        }
        if rows == 0 {
            return Err("the query returned no rows; the live table is kept".to_string());
        }
        let declared = ds.document_columns();
        let mut columns = Vec::with_capacity(declared.len());
        let mut missing = Vec::new();
        for spec in &declared {
            match self.columns.iter().find(|(n, _)| *n == spec.name) {
                None if ds.key.contains(&spec.name) => {
                    return Err(format!("key column '{}' is missing", spec.name));
                }
                None => {
                    missing.push(spec.name.clone());
                    columns.push(RefColumn::nulls(spec.ty, rows));
                }
                Some((_, col)) => {
                    if col.column_type() != spec.ty {
                        return Err(format!(
                            "column '{}' is {}, declared {}",
                            spec.name,
                            type_name(col.column_type()),
                            type_name(spec.ty)
                        ));
                    }
                    if let RefColumn::F64(v) = col
                        && let Some(i) = v.iter().position(|x| x.is_some_and(|x| !x.is_finite()))
                    {
                        return Err(format!("column '{}' row {i} is not finite", spec.name));
                    }
                    columns.push(col.clone());
                }
            }
        }
        let mut extra: Vec<String> = self
            .columns
            .iter()
            .map(|(n, _)| n.clone())
            .filter(|n| !declared.iter().any(|c| &c.name == n))
            .collect();
        extra.sort();

        // `document_columns` puts the key first, so the leading columns are
        // the key in declared order.
        let key_columns = &columns[..ds.key.len()];
        for (k, col) in key_columns.iter().enumerate() {
            if let Some(i) = (0..rows).find(|&i| col.is_null(i)) {
                return Err(format!("row {i}: key column '{}' is NULL", ds.key[k]));
            }
        }
        let keys: Vec<Vec<&str>> = (0..rows)
            .map(|i| {
                key_columns
                    .iter()
                    .map(|c| match c {
                        RefColumn::Utf8(v) => v[i].as_deref().unwrap_or(""),
                        _ => unreachable!("the schema declares reference keys utf8"),
                    })
                    .collect()
            })
            .collect();
        let mut order: Vec<usize> = (0..rows).collect();
        order.sort_by(|&a, &b| keys[a].cmp(&keys[b]));
        if let Some(w) = order.windows(2).find(|w| keys[w[0]] == keys[w[1]]) {
            return Err(format!(
                "key '{}' appears more than once",
                keys[w[0]].join("/")
            ));
        }
        let columns = columns.iter().map(|c| c.permuted(&order)).collect();
        Ok(ConformedRows {
            columns,
            rows,
            extra,
            missing,
        })
    }
}

/// Live reference tables keyed by dataset, then by joined key text.
///
/// Live only: consumers never see reference data at the frame's as-of.
/// Equality covers columns and cells, never the generation or source time, so
/// a republish of unchanged rows compares equal and wakes no observer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReferenceData {
    tables: BTreeMap<String, Table>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Table {
    columns: Vec<String>,
    /// Whole rows, key cells included, so a column's index addresses its cell.
    rows: BTreeMap<String, Vec<Option<String>>>,
}

impl Table {
    /// The key is the first `key_columns` cells joined with `/`; the
    /// reference schema allows only utf8 keys. A row with a NULL key cell is
    /// dropped rather than keyed by empty text, which could answer a lookup
    /// for some other key; `TableRows::conform` refuses such rows on ingest.
    fn from_answer(table: &ReferenceTable, key_columns: usize) -> Table {
        let rows = table
            .rows
            .iter()
            .filter_map(|row| {
                let key = row
                    .iter()
                    .take(key_columns)
                    .map(|cell| cell.as_deref())
                    .collect::<Option<Vec<&str>>>()?
                    .join("/");
                Some((key, row.clone()))
            })
            .collect();
        Table {
            columns: table.columns.clone(),
            rows,
        }
    }
}

impl ReferenceData {
    /// Replace one dataset's table from a read answer. `None` when the stored
    /// table already equals the new one, so the caller can skip republishing.
    pub fn with_table(
        &self,
        dataset: &str,
        table: &ReferenceTable,
        key_columns: usize,
    ) -> Option<ReferenceData> {
        let table = Table::from_answer(table, key_columns);
        if self.tables.get(dataset) == Some(&table) {
            return None;
        }
        let mut next = self.clone();
        next.tables.insert(dataset.to_string(), table);
        Some(next)
    }

    /// Remove a dataset (nothing published). `None` when it was already absent.
    pub fn without(&self, dataset: &str) -> Option<ReferenceData> {
        if !self.tables.contains_key(dataset) {
            return None;
        }
        let mut next = self.clone();
        next.tables.remove(dataset);
        Some(next)
    }

    /// The cell at `column` of the row keyed `key`. `None` for an unknown
    /// dataset, key or column, and for a NULL cell.
    pub fn lookup(&self, dataset: &str, key: &str, column: &str) -> Option<&str> {
        let table = self.tables.get(dataset)?;
        let index = table.columns.iter().position(|c| c == column)?;
        table.rows.get(key)?.get(index)?.as_deref()
    }

    pub fn has(&self, dataset: &str) -> bool {
        self.tables.contains_key(dataset)
    }
}

#[cfg(test)]
mod read_tests {
    use super::*;
    use crate::query::ReferenceTable;

    fn table(rows: &[(&str, Option<&str>)]) -> ReferenceTable {
        ReferenceTable {
            columns: vec!["underlying_ref".into(), "currency".into()],
            rows: rows
                .iter()
                .map(|(k, c)| vec![Some(k.to_string()), c.map(str::to_string)])
                .collect(),
            gen_id: 1,
            source_time: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        }
    }

    #[test]
    fn lookup_finds_a_cell_by_dataset_key_and_column() {
        let r = ReferenceData::default()
            .with_table("underlyings", &table(&[("SPX", Some("USD"))]), 1)
            .unwrap();
        assert_eq!(r.lookup("underlyings", "SPX", "currency"), Some("USD"));
    }

    #[test]
    fn lookup_misses_read_none() {
        let r = ReferenceData::default()
            .with_table("underlyings", &table(&[("SPX", None)]), 1)
            .unwrap();
        assert_eq!(
            r.lookup("underlyings", "SPX", "currency"),
            None,
            "NULL cell"
        );
        assert_eq!(
            r.lookup("underlyings", "AAPL", "currency"),
            None,
            "unknown key"
        );
        assert_eq!(
            r.lookup("underlyings", "SPX", "isin"),
            None,
            "unknown column"
        );
        assert_eq!(
            r.lookup("calendars", "SPX", "currency"),
            None,
            "unknown dataset"
        );
    }

    #[test]
    fn an_identical_table_changes_nothing() {
        let t = table(&[("SPX", Some("USD"))]);
        let r = ReferenceData::default()
            .with_table("underlyings", &t, 1)
            .unwrap();
        assert_eq!(r.with_table("underlyings", &t, 1), None);
        let mut moved = t.clone();
        moved.gen_id = 2;
        assert_eq!(
            r.with_table("underlyings", &moved, 1),
            None,
            "a new generation with equal rows is not a change"
        );
    }

    #[test]
    fn a_changed_cell_is_a_change_and_without_removes() {
        let r = ReferenceData::default()
            .with_table("underlyings", &table(&[("SPX", Some("USD"))]), 1)
            .unwrap();
        let r2 = r
            .with_table("underlyings", &table(&[("SPX", Some("EUR"))]), 1)
            .unwrap();
        assert_eq!(r2.lookup("underlyings", "SPX", "currency"), Some("EUR"));
        let r3 = r2.without("underlyings").unwrap();
        assert!(!r3.has("underlyings"));
        assert_eq!(r3.without("underlyings"), None);
    }

    #[test]
    fn a_composite_key_joins_with_a_slash_and_a_null_key_row_is_dropped() {
        let t = ReferenceTable {
            columns: vec!["market".into(), "ref".into(), "currency".into()],
            rows: vec![
                vec![Some("XNYS".into()), Some("SPX".into()), Some("USD".into())],
                vec![Some("XEUR".into()), None, Some("EUR".into())],
            ],
            gen_id: 1,
            source_time: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        };
        let r = ReferenceData::default().with_table("u", &t, 2).unwrap();
        assert_eq!(r.lookup("u", "XNYS/SPX", "currency"), Some("USD"));
        assert_eq!(r.lookup("u", "XEUR/", "currency"), None, "NULL key cell");
        assert_eq!(r.lookup("u", "XEUR", "currency"), None, "NULL key cell");
    }
}

/// Reference dataset fixtures for downstream tests.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::{DatasetSpec, SchemaSpec};

    /// The reference dataset `u`, parsed through the real schema reader:
    /// key `underlying_ref`, then `currency` (utf8) and `multiplier` (f64).
    pub fn reference_dataset() -> DatasetSpec {
        let text = r#"
[u]
family = "reference"
key = ["underlying_ref"]
[u.columns.underlying_ref]
type = "utf8"
role = "dimension"
[u.columns.currency]
type = "utf8"
role = "attribute"
[u.columns.multiplier]
type = "f64"
role = "attribute"
"#;
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", text).expect("well-formed test TOML")],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema.dataset("u").expect("the fixture declares u").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ColumnType;

    fn ds() -> DatasetSpec {
        test_support::reference_dataset()
    }

    fn utf8(v: &[Option<&str>]) -> RefColumn {
        RefColumn::Utf8(v.iter().map(|s| s.map(str::to_string)).collect())
    }

    fn rows() -> TableRows {
        TableRows {
            columns: vec![
                (
                    "multiplier".into(),
                    RefColumn::F64(vec![Some(10.0), Some(100.0)]),
                ),
                ("underlying_ref".into(), utf8(&[Some("SX5E"), Some("SPX")])),
                ("currency".into(), utf8(&[Some("EUR"), None])),
            ],
        }
    }

    #[test]
    fn conform_orders_columns_by_storage_and_rows_by_key() {
        let c = rows().conform(&ds()).unwrap();
        assert_eq!(c.rows, 2);
        assert_eq!(c.columns[0], utf8(&[Some("SPX"), Some("SX5E")]));
        assert_eq!(
            c.columns[1],
            utf8(&[None, Some("EUR")]),
            "NULL kept, moved with its row"
        );
        assert_eq!(c.columns[2], RefColumn::F64(vec![Some(100.0), Some(10.0)]));
        assert!(c.extra.is_empty() && c.missing.is_empty());
    }

    #[test]
    fn a_missing_optional_column_reads_null_and_an_extra_is_named() {
        let mut t = rows();
        t.columns.retain(|(n, _)| n != "currency");
        t.columns
            .push(("isin".into(), utf8(&[Some("x"), Some("y")])));
        let c = t.conform(&ds()).unwrap();
        assert_eq!(c.columns[1], RefColumn::nulls(ColumnType::Utf8, 2));
        assert_eq!(c.missing, vec!["currency".to_string()]);
        assert_eq!(c.extra, vec!["isin".to_string()]);
    }

    #[test]
    fn a_missing_key_column_is_refused() {
        let mut t = rows();
        t.columns.retain(|(n, _)| n != "underlying_ref");
        assert!(
            t.conform(&ds())
                .unwrap_err()
                .contains("key column 'underlying_ref' is missing")
        );
    }

    #[test]
    fn a_type_mismatch_is_refused() {
        let mut t = rows();
        t.columns[0].1 = RefColumn::I64(vec![Some(10), Some(100)]);
        assert!(
            t.conform(&ds())
                .unwrap_err()
                .contains("column 'multiplier' is i64, declared f64")
        );
    }

    #[test]
    fn a_null_key_is_refused() {
        let mut t = rows();
        t.columns[1].1 = utf8(&[Some("SX5E"), None]);
        assert!(
            t.conform(&ds())
                .unwrap_err()
                .contains("key column 'underlying_ref' is NULL")
        );
    }

    #[test]
    fn a_duplicate_key_is_refused() {
        let mut t = rows();
        t.columns[1].1 = utf8(&[Some("SPX"), Some("SPX")]);
        assert!(
            t.conform(&ds())
                .unwrap_err()
                .contains("key 'SPX' appears more than once")
        );
    }

    #[test]
    fn an_empty_result_is_refused() {
        let t = TableRows {
            columns: vec![("underlying_ref".into(), utf8(&[]))],
        };
        assert!(t.conform(&ds()).unwrap_err().contains("no rows"));
    }

    #[test]
    fn unequal_column_lengths_are_refused() {
        let mut t = rows();
        t.columns[2].1 = utf8(&[Some("EUR")]);
        let name = t.columns[2].0.clone();
        assert_eq!(
            t.conform(&ds()).unwrap_err(),
            format!("column '{name}' has 1 rows; the first column has 2")
        );
    }

    #[test]
    fn a_non_finite_number_is_refused() {
        let mut t = rows();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            t.columns[0].1 = RefColumn::F64(vec![Some(bad), Some(1.0)]);
            assert!(
                t.conform(&ds()).unwrap_err().contains("not finite"),
                "{bad} conformed"
            );
        }
    }

    #[test]
    fn a_repeated_column_name_is_refused() {
        let mut t = rows();
        t.columns.push(("currency".into(), utf8(&[None, None])));
        assert!(
            t.conform(&ds())
                .unwrap_err()
                .contains("'currency' appears twice")
        );
    }
}
