//! The immutable columnar result handed to the UI (spec §6.6).
//!
//! Arrow is an implementation detail: modules see `Snapshot` and typed
//! accessors returning plain slices, and nothing outside this file names
//! an Arrow type. Snapshots are `Arc`-shared, so handoff is a pointer
//! swap (§7.2) and no row objects are materialized anywhere.
//!
//! `query_arrow` returns 2048-row batches, so a column-wide slice needs
//! concatenation. That happens once here, at construction: blotter
//! results are aggregates — one row per visible group — while the million
//! rows are scanned inside DuckDB and never cross this boundary.

use crate::attribution::{Attribution, ScopeSemantics};
use arrow::array::{Array, Float64Array, Int64Array, StringArray};
use arrow::record_batch::RecordBatch;

#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub name: String,
    /// Indexed by depth; see [`Snapshot::depth_of_row`].
    pub attribution_by_depth: Vec<Attribution>,
    pub scope_semantics: ScopeSemantics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freshness {
    pub dataset: String,
    /// RFC 3339, or `None` when the dataset has never loaded.
    pub as_of: Option<String>,
    pub generation: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Provenance {
    pub datasets: Vec<Freshness>,
    /// Set when the result came from the archive rather than live.
    pub as_of_request: Option<String>,
}

impl Provenance {
    /// The stalest input. A joined view is as stale as this (spec §5.4),
    /// and a tile mixing cadences shows per-dataset freshness rather than
    /// one misleading timestamp.
    pub fn stalest(&self) -> Option<&Freshness> {
        self.datasets
            .iter()
            .filter(|f| f.as_of.is_some())
            .min_by(|a, b| a.as_of.cmp(&b.as_of))
    }
}

/// Concatenate batches, keeping a shared dictionary shared.
///
/// Arrow's `concat` *appends* dictionaries rather than noticing that two
/// batches carry the same one, so after a handful of 2048-row batches the
/// merged dictionary outgrows the 8-bit key space and the keys overflow.
/// Every batch of one query carries the same derived ENUM, so its keys
/// can simply be concatenated against the one dictionary — correct, and
/// cheaper than merging.
fn concat_preserving_dictionaries(
    batches: &[RecordBatch],
) -> Result<RecordBatch, arrow::error::ArrowError> {
    use arrow::array::{ArrayRef, DictionaryArray, UInt8Array};
    use arrow::datatypes::{DataType, UInt8Type};

    let schema = batches[0].schema();
    if batches.len() == 1 {
        return Ok(batches[0].clone());
    }

    let mut columns: Vec<ArrayRef> = Vec::with_capacity(schema.fields().len());
    for (i, field) in schema.fields().iter().enumerate() {
        let slices: Vec<&dyn Array> = batches.iter().map(|b| b.column(i).as_ref()).collect();

        let dictionary_of_u8 = matches!(
            field.data_type(),
            DataType::Dictionary(k, _) if **k == DataType::UInt8
        );
        if dictionary_of_u8 {
            let dicts: Vec<&DictionaryArray<UInt8Type>> = slices
                .iter()
                .filter_map(|a| a.as_any().downcast_ref::<DictionaryArray<UInt8Type>>())
                .collect();
            if dicts.len() == slices.len()
                && dicts
                    .iter()
                    .all(|d| d.values().as_ref() == dicts[0].values().as_ref())
            {
                let keys: Vec<&dyn Array> = dicts.iter().map(|d| d.keys() as &dyn Array).collect();
                let merged = arrow::compute::concat(&keys)?;
                let merged = merged
                    .as_any()
                    .downcast_ref::<UInt8Array>()
                    .expect("concat of UInt8 keys")
                    .clone();
                columns.push(std::sync::Arc::new(DictionaryArray::<UInt8Type>::try_new(
                    merged,
                    dicts[0].values().clone(),
                )?));
                continue;
            }
        }
        columns.push(arrow::compute::concat(&slices)?);
    }

    RecordBatch::try_new(schema, columns)
}

#[derive(Debug)]
pub struct Snapshot {
    batch: Option<RecordBatch>,
    meta: Vec<ColumnMeta>,
    /// Number of grouping columns — the deepest level a row can carry.
    grouping_len: usize,
    provenance: Provenance,
}

impl Snapshot {
    pub fn from_batches(
        batches: Vec<RecordBatch>,
        meta: Vec<ColumnMeta>,
        grouping_len: usize,
        provenance: Provenance,
    ) -> Result<Snapshot, arrow::error::ArrowError> {
        let batch = match batches.first() {
            None => None,
            Some(_) => Some(concat_preserving_dictionaries(&batches)?),
        };
        Ok(Snapshot {
            batch,
            meta,
            grouping_len,
            provenance,
        })
    }

    pub fn rows(&self) -> usize {
        self.batch.as_ref().map_or(0, |b| b.num_rows())
    }

    pub fn column_names(&self) -> Vec<&str> {
        self.meta.iter().map(|m| m.name.as_str()).collect()
    }

    pub fn meta(&self, name: &str) -> Option<&ColumnMeta> {
        self.meta.iter().find(|m| m.name == name)
    }

    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    fn column(&self, name: &str) -> Option<&dyn Array> {
        let batch = self.batch.as_ref()?;
        let idx = batch.schema().index_of(name).ok()?;
        Some(batch.column(idx).as_ref())
    }

    /// Zero-copy over the whole column. `None` when absent or not f64 —
    /// never a panic, because a view can name a column the data lacks.
    pub fn f64_column(&self, name: &str) -> Option<&[f64]> {
        Some(
            self.column(name)?
                .as_any()
                .downcast_ref::<Float64Array>()?
                .values(),
        )
    }

    pub fn i64_column(&self, name: &str) -> Option<&[i64]> {
        Some(
            self.column(name)?
                .as_any()
                .downcast_ref::<Int64Array>()?
                .values(),
        )
    }

    /// Strings are returned as the Arrow array: offsets make a `&[&str]`
    /// impossible without allocating, and the renderer reads by row.
    pub fn str_column(&self, name: &str) -> Option<&StringArray> {
        self.column(name)?.as_any().downcast_ref::<StringArray>()
    }

    /// A dictionary-encoded dimension column: per-row codes plus the
    /// shared value dictionary. The renderer compares and formats on the
    /// codes rather than the strings (spec §7.2).
    pub fn dict_column(&self, name: &str) -> Option<(&[u8], &StringArray)> {
        use arrow::array::DictionaryArray;
        use arrow::datatypes::UInt8Type;
        let arr = self
            .column(name)?
            .as_any()
            .downcast_ref::<DictionaryArray<UInt8Type>>()?;
        let values = arr.values().as_any().downcast_ref::<StringArray>()?;
        Some((arr.keys().values(), values))
    }

    /// How many grouping columns are present on this row — 0 is the grand
    /// total, `grouping_len` a leaf. The compiler emits it directly rather
    /// than as a `GROUPING()` bitmask, whose width would otherwise change
    /// with how deep the query was told to materialize.
    pub fn depth_of_row(&self, row: usize) -> Option<usize> {
        let depth = *self.i64_column("row_depth")?.get(row)?;
        usize::try_from(depth)
            .ok()
            .filter(|d| *d <= self.grouping_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{Attribution, ScopeSemantics};
    use arrow::array::{Float64Array, Int64Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use std::sync::Arc;

    /// Two batches, so concatenation is actually exercised — this is the
    /// shape query_arrow really returns.
    fn batches() -> Vec<RecordBatch> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("book", DataType::Utf8, true),
            Field::new("row_depth", DataType::Int64, true),
            Field::new("delta01", DataType::Float64, true),
        ]));
        let one = RecordBatch::try_new(
            schema.clone(),
            vec![
                // A leaf carries its book; the grand total does not.
                Arc::new(StringArray::from(vec![Some("BK000"), None])),
                Arc::new(Int64Array::from(vec![1, 0])),
                Arc::new(Float64Array::from(vec![Some(10.0), Some(30.0)])),
            ],
        )
        .unwrap();
        let two = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec![Some("BK001")])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Float64Array::from(vec![Some(20.0)])),
            ],
        )
        .unwrap();
        vec![one, two]
    }

    fn meta() -> Vec<ColumnMeta> {
        ["book", "row_depth", "delta01"]
            .into_iter()
            .map(|name| ColumnMeta {
                name: name.into(),
                attribution_by_depth: vec![Attribution::Additive; 2],
                scope_semantics: ScopeSemantics::Direct,
            })
            .collect()
    }

    fn snapshot() -> Snapshot {
        Snapshot::from_batches(batches(), meta(), 1, Provenance::default()).unwrap()
    }

    #[test]
    fn concatenates_batches_into_one_addressable_column() {
        let s = snapshot();
        assert_eq!(s.rows(), 3, "both batches, not just the first");
        assert_eq!(s.f64_column("delta01").unwrap(), &[10.0, 30.0, 20.0]);
    }

    #[test]
    fn string_columns_read_by_row_including_nulls() {
        let s = snapshot();
        assert_eq!(s.str_column("book").unwrap().value(0), "BK000");
        assert!(s.str_column("book").unwrap().is_null(1));
    }

    #[test]
    fn an_unknown_or_mistyped_column_is_none_not_a_panic() {
        let s = snapshot();
        assert!(s.f64_column("nonesuch").is_none());
        assert!(s.f64_column("book").is_none(), "wrong type must not panic");
    }

    #[test]
    fn depth_is_read_off_the_row() {
        // n = 1: row 0 is a leaf, row 1 the grand total.
        let s = snapshot();
        assert_eq!(s.depth_of_row(0), Some(1));
        assert_eq!(s.depth_of_row(1), Some(0));
        assert_eq!(s.depth_of_row(99), None, "past the end is not a panic");
    }

    #[test]
    fn column_metadata_is_addressable_by_name() {
        let s = snapshot();
        assert_eq!(
            s.meta("delta01").unwrap().attribution_by_depth[1],
            Attribution::Additive
        );
        assert!(s.meta("nonesuch").is_none());
    }

    #[test]
    fn freshness_reports_the_stalest_input() {
        // A joined view is as stale as its stalest input (spec §5.4).
        let mut p = Provenance::default();
        p.datasets.push(Freshness {
            dataset: "risk_snapshot".into(),
            as_of: Some("2026-08-30T14:32:00Z".into()),
            generation: 47,
        });
        p.datasets.push(Freshness {
            dataset: "implied_vol_summary".into(),
            as_of: Some("2026-08-30T07:00:00Z".into()),
            generation: 3,
        });
        let s = Snapshot::from_batches(batches(), meta(), 1, p).unwrap();
        assert_eq!(
            s.provenance().stalest().map(|f| f.dataset.as_str()),
            Some("implied_vol_summary")
        );
    }

    #[test]
    fn an_empty_result_is_a_valid_snapshot() {
        let s = Snapshot::from_batches(Vec::new(), meta(), 1, Provenance::default()).unwrap();
        assert_eq!(s.rows(), 0);
        assert!(s.f64_column("delta01").is_none_or(|c| c.is_empty()));
    }
}
