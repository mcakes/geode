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
use arrow::compute::concat_batches;
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

#[derive(Debug)]
pub struct Snapshot {
    batch: Option<RecordBatch>,
    meta: Vec<ColumnMeta>,
    /// Number of grouping columns, for decoding the depth bitmask.
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
            Some(first) => Some(concat_batches(&first.schema(), &batches)?),
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

    /// How many grouping columns are present on this row. The compiler
    /// emits `grouping(...)` as `depth_mask`; under ROLLUP a level with
    /// `d` of `n` columns present has mask `2^(n-d) - 1`.
    pub fn depth_of_row(&self, row: usize) -> Option<usize> {
        let mask = *self.i64_column("depth_mask")?.get(row)?;
        (0..=self.grouping_len).find(|d| ((1i64 << (self.grouping_len - d)) - 1).max(0) == mask)
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
            Field::new("depth_mask", DataType::Int64, true),
            Field::new("delta01", DataType::Float64, true),
        ]));
        let one = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![Some("BK000"), None])),
                Arc::new(Int64Array::from(vec![0, 1])),
                Arc::new(Float64Array::from(vec![Some(10.0), Some(30.0)])),
            ],
        )
        .unwrap();
        let two = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec![Some("BK001")])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Float64Array::from(vec![Some(20.0)])),
            ],
        )
        .unwrap();
        vec![one, two]
    }

    fn meta() -> Vec<ColumnMeta> {
        ["book", "depth_mask", "delta01"]
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
    fn depth_is_decoded_from_the_grouping_bitmask() {
        // n = 1: mask 0 is the leaf, mask 1 the grand total.
        let s = snapshot();
        assert_eq!(s.depth_of_row(0), Some(1));
        assert_eq!(s.depth_of_row(1), Some(0));
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
