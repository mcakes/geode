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
    use arrow::array::ArrayRef;
    use arrow::datatypes::{DataType, UInt8Type, UInt16Type};

    let schema = batches[0].schema();
    if batches.len() == 1 {
        return Ok(batches[0].clone());
    }

    let mut columns: Vec<ArrayRef> = Vec::with_capacity(schema.fields().len());
    for (i, field) in schema.fields().iter().enumerate() {
        let slices: Vec<&dyn Array> = batches.iter().map(|b| b.column(i).as_ref()).collect();

        // Both key widths, because the width follows the vocabulary size
        // rather than the schema — see `dict_column`.
        let shared = match field.data_type() {
            DataType::Dictionary(k, _) if **k == DataType::UInt8 => {
                concat_shared_dictionary::<UInt8Type>(&slices)
            }
            DataType::Dictionary(k, _) if **k == DataType::UInt16 => {
                concat_shared_dictionary::<UInt16Type>(&slices)
            }
            _ => None,
        };
        if let Some(merged) = shared {
            columns.push(merged?);
            continue;
        }
        columns.push(arrow::compute::concat(&slices)?);
    }

    RecordBatch::try_new(schema, columns)
}

/// Concatenate dictionary arrays that all share one dictionary, by
/// concatenating their keys against it.
///
/// `None` when the slices are not all dictionaries of this key width, or
/// do not share a dictionary — the caller then falls back to Arrow's own
/// `concat`, which is correct but merges the dictionaries.
fn concat_shared_dictionary<K: arrow::datatypes::ArrowDictionaryKeyType>(
    slices: &[&dyn Array],
) -> Option<Result<arrow::array::ArrayRef, arrow::error::ArrowError>> {
    use arrow::array::{ArrayRef, DictionaryArray, PrimitiveArray};

    let dicts: Vec<&DictionaryArray<K>> = slices
        .iter()
        .filter_map(|a| a.as_any().downcast_ref::<DictionaryArray<K>>())
        .collect();
    if dicts.len() != slices.len()
        || !dicts
            .iter()
            .all(|d| d.values().as_ref() == dicts[0].values().as_ref())
    {
        return None;
    }

    let keys: Vec<&dyn Array> = dicts.iter().map(|d| d.keys() as &dyn Array).collect();
    let merged = match arrow::compute::concat(&keys) {
        Ok(m) => m,
        Err(e) => return Some(Err(e)),
    };
    // Concatenating keys of one width yields that width; anything else
    // would be an Arrow bug rather than a data condition.
    let merged = merged.as_any().downcast_ref::<PrimitiveArray<K>>()?.clone();
    Some(
        DictionaryArray::<K>::try_new(merged, dicts[0].values().clone())
            .map(|d| std::sync::Arc::new(d) as ArrayRef),
    )
}

/// Per-row dictionary codes, at whichever width the data uses.
///
/// The width is not a schema decision: DuckDB sizes an ENUM's key to its
/// vocabulary, so the same logical dimension is UInt8 in a small fixture
/// and UInt16 once its value list passes 255. Callers that only compare
/// and group can use [`Self::code`] and stay width-agnostic; the raw
/// slices are exposed for bulk work that wants the narrower type.
#[derive(Debug, Clone, Copy)]
pub enum DictCodes<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
}

impl DictCodes<'_> {
    pub fn len(&self) -> usize {
        match self {
            DictCodes::U8(c) => c.len(),
            DictCodes::U16(c) => c.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The code at `row`, widened. `None` past the end.
    ///
    /// A code is present for every row including NULL ones, where it is
    /// arbitrary; only [`Snapshot::dict_value`] consults the null bitmap.
    pub fn code(&self, row: usize) -> Option<usize> {
        match self {
            DictCodes::U8(c) => c.get(row).map(|v| *v as usize),
            DictCodes::U16(c) => c.get(row).map(|v| *v as usize),
        }
    }
}

/// One cell of a dictionary array, resolved to its string, honouring both
/// the key null bitmap and the dictionary's own.
fn dictionary_cell<K: arrow::datatypes::ArrowDictionaryKeyType>(
    d: &arrow::array::DictionaryArray<K>,
    row: usize,
) -> Option<&str> {
    use arrow::datatypes::ArrowNativeType;
    if row >= d.len() || d.is_null(row) {
        return None;
    }
    let values = d.values().as_any().downcast_ref::<StringArray>()?;
    let code = d.keys().value(row).as_usize();
    (code < values.len() && !values.is_null(code)).then(|| values.value(code))
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
    ///
    /// **This is the raw value buffer and cannot express a NULL.** Arrow
    /// stores a blanked cell as an arbitrary value (in practice 0.0) plus a
    /// cleared bit in a separate null bitmap, which this slice does not
    /// carry. Use it for bulk numeric work where the caller has already
    /// established the column has no nulls; anything rendering a cell must
    /// go through [`Self::f64_value`], or a cell the compiler deliberately
    /// blanked will read back as a real zero.
    pub fn f64_column(&self, name: &str) -> Option<&[f64]> {
        Some(
            self.column(name)?
                .as_any()
                .downcast_ref::<Float64Array>()?
                .values(),
        )
    }

    /// One numeric cell. `None` when the column is absent or not f64, the
    /// row is past the end, **or the value is NULL**.
    ///
    /// NULL and 0.0 are different answers and the difference is the whole
    /// of §6.3. A measure is blanked outright where it is
    /// `NonAttributable` — cross gamma at an underlying-level grouping
    /// belongs to no single underlying — and a renderer that cannot tell
    /// the two apart prints a confident zero where the honest answer is
    /// "this number does not belong to this row".
    pub fn f64_value(&self, name: &str, row: usize) -> Option<f64> {
        let values = self.column(name)?.as_any().downcast_ref::<Float64Array>()?;
        (row < values.len() && !values.is_null(row)).then(|| values.value(row))
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
    ///
    /// Both key widths are matched, because the width is a property of the
    /// data rather than the schema: DuckDB sizes an ENUM's key to its
    /// vocabulary, UInt8 up to 255 values and UInt16 above (verified: 200
    /// → UInt8, 300 → UInt16). Matching only UInt8 made every dimension
    /// with a real underlying list fall through this *and* `str_column`
    /// and render blank.
    ///
    /// The codes carry no null bitmap, so a rolled-up cell is an arbitrary
    /// code here. Use [`Self::dict_value`] to read one cell for display.
    pub fn dict_column(&self, name: &str) -> Option<(DictCodes<'_>, &StringArray)> {
        use arrow::array::DictionaryArray;
        use arrow::datatypes::{UInt8Type, UInt16Type};
        let arr = self.column(name)?;
        if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<UInt8Type>>() {
            let values = d.values().as_any().downcast_ref::<StringArray>()?;
            return Some((DictCodes::U8(d.keys().values()), values));
        }
        let d = arr.as_any().downcast_ref::<DictionaryArray<UInt16Type>>()?;
        let values = d.values().as_any().downcast_ref::<StringArray>()?;
        Some((DictCodes::U16(d.keys().values()), values))
    }

    /// One string cell, or `None` when the column is absent, the value is
    /// NULL, or the row is past the end.
    ///
    /// A renderer walks rows, and [`Self::str_column`] hands back the Arrow
    /// array — which would make every caller name an Arrow type and bring
    /// its traits into scope, exactly what this module exists to prevent.
    pub fn str_value(&self, name: &str, row: usize) -> Option<&str> {
        let values = self.str_column(name)?;
        (row < values.len() && !values.is_null(row)).then(|| values.value(row))
    }

    /// One dictionary-encoded cell, resolved to its string. The codes are
    /// what comparisons and grouping should use (spec §7.2); this is for
    /// display.
    ///
    /// `None` for a NULL cell. The key array's null bitmap is the only
    /// thing distinguishing "this row has no value for this dimension"
    /// from dictionary entry 0 — read through the raw codes instead and
    /// the grand-total row, which belongs to no book, displays a real book
    /// name.
    pub fn dict_value(&self, name: &str, row: usize) -> Option<&str> {
        use arrow::array::DictionaryArray;
        use arrow::datatypes::{UInt8Type, UInt16Type};
        let arr = self.column(name)?;
        if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<UInt8Type>>() {
            return dictionary_cell(d, row);
        }
        dictionary_cell(
            arr.as_any().downcast_ref::<DictionaryArray<UInt16Type>>()?,
            row,
        )
    }

    /// One dimension cell as text, whatever encoding it arrived in.
    ///
    /// A column's encoding depends on the *era*, not only on the schema:
    /// the live path interns dimensions as DuckDB ENUMs and gets
    /// dictionary-encoded columns back, while an as-of read skips the
    /// interning and produces plain strings (§6.5). `ColumnMeta` does not
    /// record which, so a caller that picks an accessor by column name
    /// reads a value in one era and `None` in the other, with no signal
    /// that anything changed. Anything rendering a dimension should come
    /// through here rather than choose for itself.
    pub fn text_value(&self, name: &str, row: usize) -> Option<&str> {
        self.dict_value(name, row)
            .or_else(|| self.str_value(name, row))
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

/// One column of fixture data, in the shapes [`Snapshot`] hands back.
#[cfg(any(test, feature = "test-support"))]
pub enum TestColumn {
    F64(Vec<Option<f64>>),
    I64(Vec<i64>),
    Str(Vec<Option<&'static str>>),
    /// Dictionary-encoded, the shape a live ENUM column arrives in. The
    /// key width follows DuckDB's own rule — UInt8 up to 255 distinct
    /// values, UInt16 above — so a fixture that crosses the cliff
    /// exercises what a real underlying list does.
    Dict(Vec<Option<String>>),
}

/// Build a dictionary column the way DuckDB would: distinct values in
/// first-seen order, keys sized to the vocabulary, NULL cells carrying a
/// cleared key bit rather than a code.
#[cfg(any(test, feature = "test-support"))]
fn dictionary_fixture(
    cells: &[Option<String>],
) -> (arrow::datatypes::DataType, arrow::array::ArrayRef) {
    use arrow::array::{ArrayRef, DictionaryArray, UInt8Array, UInt16Array};
    use arrow::datatypes::{DataType, UInt8Type, UInt16Type};
    use std::sync::Arc;

    let mut distinct: Vec<&str> = Vec::new();
    for c in cells.iter().flatten() {
        if !distinct.contains(&c.as_str()) {
            distinct.push(c.as_str());
        }
    }
    let values = Arc::new(StringArray::from(distinct.clone()));
    let code_of = |s: &str| {
        distinct
            .iter()
            .position(|d| *d == s)
            .expect("every non-null cell is interned above")
    };

    if distinct.len() <= u8::MAX as usize {
        let keys: UInt8Array = cells
            .iter()
            .map(|c| c.as_deref().map(|s| code_of(s) as u8))
            .collect();
        (
            DataType::Dictionary(Box::new(DataType::UInt8), Box::new(DataType::Utf8)),
            Arc::new(
                DictionaryArray::<UInt8Type>::try_new(keys, values).expect("fixture dictionary"),
            ) as ArrayRef,
        )
    } else {
        let keys: UInt16Array = cells
            .iter()
            .map(|c| c.as_deref().map(|s| code_of(s) as u16))
            .collect();
        (
            DataType::Dictionary(Box::new(DataType::UInt16), Box::new(DataType::Utf8)),
            Arc::new(
                DictionaryArray::<UInt16Type>::try_new(keys, values).expect("fixture dictionary"),
            ) as ArrayRef,
        )
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Snapshot {
    /// Build a snapshot from plain Rust values.
    ///
    /// Downstream crates need `Snapshot` fixtures, and this module's whole
    /// premise is that nothing outside it names an Arrow type — so the
    /// fixture builder lives here rather than making every test crate
    /// reach for `arrow` and pin its version to match duckdb's.
    pub fn for_tests(columns: Vec<(ColumnMeta, TestColumn)>, grouping_len: usize) -> Snapshot {
        use arrow::array::{ArrayRef, Float64Array, StringArray};
        use arrow::datatypes::{DataType, Field, Schema};
        use std::sync::Arc;

        let (fields, arrays): (Vec<Field>, Vec<ArrayRef>) = columns
            .iter()
            .map(|(meta, values)| {
                let (ty, array): (DataType, ArrayRef) = match values {
                    TestColumn::F64(v) => {
                        (DataType::Float64, Arc::new(Float64Array::from(v.clone())))
                    }
                    TestColumn::I64(v) => (DataType::Int64, Arc::new(Int64Array::from(v.clone()))),
                    TestColumn::Str(v) => (DataType::Utf8, Arc::new(StringArray::from(v.clone()))),
                    TestColumn::Dict(v) => dictionary_fixture(v),
                };
                (Field::new(&meta.name, ty, true), array)
            })
            .unzip();
        let meta = columns.into_iter().map(|(m, _)| m).collect();
        let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)
            .expect("fixture columns must be the same length");
        Snapshot::from_batches(vec![batch], meta, grouping_len, Provenance::default())
            .expect("fixture snapshot")
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
        assert!(s.f64_value("nonesuch", 0).is_none());
        assert!(
            s.f64_value("book", 0).is_none(),
            "wrong type must not panic"
        );
    }

    #[test]
    fn a_null_measure_reads_as_none_not_zero() {
        // The compiler blanks a measure where it is NonAttributable
        // (§6.3). Arrow stores that as a cleared null bit over an
        // arbitrary payload, so the distinction lives in the bitmap and
        // nowhere else.
        let s = Snapshot::for_tests(
            vec![(
                ColumnMeta {
                    name: "cross_gamma".into(),
                    attribution_by_depth: vec![Attribution::NonAttributable, Attribution::Additive],
                    scope_semantics: ScopeSemantics::Direct,
                },
                TestColumn::F64(vec![None, Some(0.0), Some(2.5)]),
            )],
            1,
        );
        assert_eq!(s.f64_value("cross_gamma", 0), None, "NULL is not 0.0");
        assert_eq!(
            s.f64_value("cross_gamma", 1),
            Some(0.0),
            "a real zero is still a number"
        );
        assert_eq!(s.f64_value("cross_gamma", 2), Some(2.5));
        assert_eq!(s.f64_value("cross_gamma", 99), None, "past the end");
    }

    #[test]
    fn the_raw_value_buffer_cannot_express_the_null_that_f64_value_reports() {
        // Pins *why* f64_value has to exist: delete it and route a
        // renderer back through f64_column, and this is the value it sees
        // for a deliberately blanked cell.
        let s = Snapshot::for_tests(
            vec![(
                ColumnMeta {
                    name: "cross_gamma".into(),
                    attribution_by_depth: vec![Attribution::NonAttributable],
                    scope_semantics: ScopeSemantics::Direct,
                },
                TestColumn::F64(vec![None]),
            )],
            1,
        );
        assert_eq!(s.f64_column("cross_gamma").unwrap()[0], 0.0);
        assert_eq!(s.f64_value("cross_gamma", 0), None);
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

    fn dim(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            attribution_by_depth: vec![Attribution::Additive; 2],
            scope_semantics: ScopeSemantics::Direct,
        }
    }

    #[test]
    fn a_rolled_up_dimension_cell_is_none_not_the_first_dictionary_entry() {
        // The grand total belongs to no book. The key array's null bitmap
        // is the only thing saying so — the code underneath it is
        // arbitrary, and reading it directly names a real book.
        let s = Snapshot::for_tests(
            vec![(
                dim("book"),
                TestColumn::Dict(vec![Some("BK000".into()), None]),
            )],
            1,
        );
        assert_eq!(s.dict_value("book", 0), Some("BK000"));
        assert_eq!(s.dict_value("book", 1), None, "the total carries no book");
        assert_eq!(s.dict_value("book", 99), None, "past the end");
    }

    #[test]
    fn a_dimension_past_the_255_value_cliff_reads_at_either_width() {
        // Above 255 distinct values the keys widen to UInt16. Matching
        // only UInt8 made such a column fall through dict_column *and*
        // str_column and render blank.
        let wide: Vec<Option<String>> = (0..300).map(|i| Some(format!("U{i:04}"))).collect();
        let narrow: Vec<Option<String>> = (0..200).map(|i| Some(format!("U{i:04}"))).collect();

        let s = Snapshot::for_tests(vec![(dim("underlying_ref"), TestColumn::Dict(wide))], 1);
        assert_eq!(s.dict_value("underlying_ref", 299), Some("U0299"));
        assert!(matches!(
            s.dict_column("underlying_ref").unwrap().0,
            DictCodes::U16(_)
        ));

        let s = Snapshot::for_tests(vec![(dim("underlying_ref"), TestColumn::Dict(narrow))], 1);
        assert_eq!(s.dict_value("underlying_ref", 199), Some("U0199"));
        assert!(matches!(
            s.dict_column("underlying_ref").unwrap().0,
            DictCodes::U8(_)
        ));
    }

    #[test]
    fn a_dimension_reads_the_same_under_either_era_encoding() {
        // Live interns dimensions as ENUMs; an as-of read does not. The
        // column type therefore depends on the era while ColumnMeta does
        // not record it, so a renderer must not choose an accessor itself.
        let live = Snapshot::for_tests(
            vec![(
                dim("book"),
                TestColumn::Dict(vec![Some("BK000".into()), None]),
            )],
            1,
        );
        let historical = Snapshot::for_tests(
            vec![(dim("book"), TestColumn::Str(vec![Some("BK000"), None]))],
            1,
        );
        for (era, s) in [("live", &live), ("as-of", &historical)] {
            assert_eq!(s.text_value("book", 0), Some("BK000"), "{era}");
            assert_eq!(s.text_value("book", 1), None, "{era}: rolled up");
            assert_eq!(s.text_value("book", 99), None, "{era}: past the end");
        }
    }

    #[test]
    fn dictionary_batches_concatenate_at_either_key_width() {
        // The concat path special-cased UInt8. A wide dimension arriving
        // as several 2048-row batches has to survive it too.
        for distinct in [200usize, 300] {
            let cells: Vec<Option<String>> =
                (0..distinct).map(|i| Some(format!("U{i:04}"))).collect();
            let (ty, array) = dictionary_fixture(&cells);
            let schema = Arc::new(Schema::new(vec![Field::new("underlying_ref", ty, true)]));
            let batch = RecordBatch::try_new(schema, vec![array]).unwrap();
            let s = Snapshot::from_batches(
                vec![batch.clone(), batch],
                vec![dim("underlying_ref")],
                1,
                Provenance::default(),
            )
            .unwrap();
            assert_eq!(s.rows(), distinct * 2, "{distinct}: both batches");
            let last = distinct * 2 - 1;
            assert_eq!(
                s.dict_value("underlying_ref", last),
                Some(format!("U{:04}", distinct - 1).as_str()),
                "{distinct}: the second batch's last value"
            );
        }
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
