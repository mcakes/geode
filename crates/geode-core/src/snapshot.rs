//! Immutable columnar query results shared with the UI through `Arc`.
//! Cell accessors expose Rust values and slices so feature modules need no Arrow
//! dependency. Construction and raw array accessors also expose Arrow types for
//! callers that need them.
//!
//! Construction concatenates query batches once, validates metadata against the
//! result columns, and builds the tree index on the query worker. Delivery shares
//! the prepared snapshot without materializing row objects.

use crate::attribution::{Attribution, ScopeSemantics};
use crate::tree::TreeIndex;
use arrow::array::{Array, Float64Array, Int64Array, StringArray};
use arrow::buffer::NullBuffer;
use arrow::record_batch::RecordBatch;

#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub name: String,
    /// Indexed by depth; see [`Snapshot::depth_of_row`].
    pub attribution_by_depth: Vec<Attribution>,
    pub scope_semantics: ScopeSemantics,
    /// Whether values in this column add up across sibling rows, as the
    /// query compiler decided it: true only for a plain `sum` measure.
    /// Attribution says whether a value belongs to its row, not whether
    /// the column totals — a `max` measure is additive in attribution and
    /// still must not be summed. Anything unmarked is not summable.
    pub summable: bool,
    /// For an ungrouped dimension column (the compiler's unanimity rule),
    /// the index of its boolean companion column: true on a row whose
    /// underlying rows disagree. Such a cell is NULL in this column and must
    /// not be read as blank — blank means no row had a value. `None` for
    /// every other column; [`Snapshot::from_batches`] refuses an index that
    /// is out of range, names the column itself, or is not boolean.
    pub mixed_flag: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freshness {
    pub dataset: String,
    /// Selected source time in RFC 3339, or `None` when no matching source
    /// time is known. Live views report dataset-wide freshness; historical
    /// views report the oldest selected source time per dataset. Documents
    /// report the source time of their own partition.
    pub as_of: Option<String>,
    /// Publication identity for a document, or the dataset's newest live
    /// generation for a live view. The live-view value is a dataset-wide
    /// change marker; its partitions can contain different generations.
    /// Historical views report `None`, as do reads with no matching generation.
    ///
    /// A corrected republish can keep its source time while taking a new
    /// generation ID. Compare known generation IDs as well as source times
    /// when checking for changes. `None` means unknown, not unchanged.
    pub generation: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct Provenance {
    pub datasets: Vec<Freshness>,
    /// Requested historical instant in RFC 3339, or `None` for a live read.
    /// Historical reads can select rows from either live or archive storage.
    pub as_of_request: Option<String>,
}

impl Provenance {
    /// The earliest known input timestamp. Inputs without a timestamp are skipped;
    /// `None` means none has one. Joined views can use this for overall freshness
    /// while retaining each dataset's timestamp for mixed-cadence displays.
    pub fn stalest(&self) -> Option<&Freshness> {
        self.datasets
            .iter()
            .filter(|f| f.as_of.is_some())
            .min_by(|a, b| a.as_of.cmp(&b.as_of))
    }
}

/// Concatenate batches while preserving a shared dictionary's codes.
/// When dictionary contents match across batches, concatenate only the keys and
/// reuse the dictionary. This preserves one code per value when the input
/// dictionary is canonical, allowing the tree and renderer to compare codes.
///
/// Other arrays use Arrow's `concat`. That fallback can merge dictionaries; this
/// function does not verify that its output retains one code per value. Duplicate
/// entries would give equal strings different codes and split code-based groups.
/// Production ENUM batches share a dictionary and use the preserving path.
/// Recheck dictionary and NULL behavior when upgrading Arrow.
fn concat_preserving_dictionaries(
    batches: &[RecordBatch],
) -> Result<RecordBatch, arrow::error::ArrowError> {
    use arrow::array::ArrayRef;
    use arrow::datatypes::{DataType, UInt8Type, UInt16Type, UInt32Type};

    let schema = batches[0].schema();
    if batches.len() == 1 {
        return Ok(batches[0].clone());
    }

    let mut columns: Vec<ArrayRef> = Vec::with_capacity(schema.fields().len());
    for (i, field) in schema.fields().iter().enumerate() {
        let slices: Vec<&dyn Array> = batches.iter().map(|b| b.column(i).as_ref()).collect();

        // ENUM key width follows vocabulary size; preserve shared dictionaries
        // for all three supported widths.
        let shared = match field.data_type() {
            DataType::Dictionary(k, _) if **k == DataType::UInt8 => {
                concat_shared_dictionary::<UInt8Type>(&slices)
            }
            DataType::Dictionary(k, _) if **k == DataType::UInt16 => {
                concat_shared_dictionary::<UInt16Type>(&slices)
            }
            DataType::Dictionary(k, _) if **k == DataType::UInt32 => {
                concat_shared_dictionary::<UInt32Type>(&slices)
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
/// `concat`, which can merge dictionaries without preserving canonical codes.
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
    U8(&'a [u8], Option<&'a NullBuffer>),
    U16(&'a [u16], Option<&'a NullBuffer>),
    U32(&'a [u32], Option<&'a NullBuffer>),
}

impl DictCodes<'_> {
    pub fn len(&self) -> usize {
        match self {
            DictCodes::U8(c, _) => c.len(),
            DictCodes::U16(c, _) => c.len(),
            DictCodes::U32(c, _) => c.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether this row has no value, such as a rolled-up dimension cell.
    /// Raw codes under a NULL are arbitrary and must not be used for grouping.
    /// Returns true for an out-of-range row or a cleared key validity bit.
    pub fn is_null(&self, row: usize) -> bool {
        let nulls = match self {
            DictCodes::U8(_, n) | DictCodes::U16(_, n) | DictCodes::U32(_, n) => *n,
        };
        row >= self.len() || nulls.is_some_and(|n| n.is_null(row))
    }

    /// The code at `row`. `None` past the end **or for a NULL row**, so a
    /// caller that groups on the result cannot silently merge rolled-up
    /// rows into a real dimension value. Use [`Self::raw`] for bulk work
    /// that has already established there are no nulls.
    pub fn code(&self, row: usize) -> Option<usize> {
        if self.is_null(row) {
            return None;
        }
        match self {
            DictCodes::U8(c, _) => c.get(row).map(|v| *v as usize),
            DictCodes::U16(c, _) => c.get(row).map(|v| *v as usize),
            DictCodes::U32(c, _) => c.get(row).map(|v| *v as usize),
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

/// One f64 cell out of whatever numeric shape the column arrived in.
/// Shared by [`Snapshot::f64_value`] and [`Snapshot::f64_at`] so the two
/// cannot drift into disagreeing about a type.
fn f64_in(arr: &dyn Array, row: usize) -> Option<f64> {
    if let Some(values) = arr.as_any().downcast_ref::<Float64Array>() {
        return (row < values.len() && !values.is_null(row)).then(|| values.value(row));
    }
    // DuckDB exports `sum(BIGINT)` as `Decimal128(38, 0)`. Read decimals
    // as numeric values so a valid integer aggregate does not display as a
    // blank cell. Conversion to f64 follows the declared decimal scale.
    use arrow::array::Decimal128Array;
    use arrow::datatypes::DataType;
    if let Some(values) = arr.as_any().downcast_ref::<Decimal128Array>() {
        if row >= values.len() || values.is_null(row) {
            return None;
        }
        let scale = match arr.data_type() {
            DataType::Decimal128(_, s) => *s,
            _ => 0,
        };
        return Some(values.value(row) as f64 / 10f64.powi(scale as i32));
    }
    // A narrower float, or an integer measure that was not summed.
    if let Some(v) = i64_in(arr, row) {
        return Some(v as f64);
    }
    let values = arr.as_any().downcast_ref::<arrow::array::Float32Array>()?;
    (row < values.len() && !values.is_null(row)).then(|| values.value(row) as f64)
}

/// One i64 cell, at any integer width DuckDB might have chosen. Shared by
/// [`Snapshot::i64_value`] and [`Snapshot::i64_at`].
fn i64_in(arr: &dyn Array, row: usize) -> Option<i64> {
    use arrow::array::PrimitiveArray;
    use arrow::datatypes::{
        Int8Type, Int16Type, Int32Type, Int64Type, UInt8Type, UInt16Type, UInt32Type,
    };

    macro_rules! read_at_width {
        ($t:ty) => {
            if let Some(a) = arr.as_any().downcast_ref::<PrimitiveArray<$t>>() {
                if row >= a.len() || a.is_null(row) {
                    return None;
                }
                return i64::try_from(a.value(row)).ok();
            }
        };
    }
    read_at_width!(Int64Type);
    read_at_width!(Int32Type);
    read_at_width!(Int16Type);
    read_at_width!(Int8Type);
    read_at_width!(UInt32Type);
    read_at_width!(UInt16Type);
    read_at_width!(UInt8Type);
    None
}

/// The codes and dictionary of a dictionary-encoded column, at any key
/// width. Shared by [`Snapshot::dict_column`] and
/// [`Snapshot::dict_codes_at`].
fn dict_column_in(arr: &dyn Array) -> Option<(DictCodes<'_>, &StringArray)> {
    use arrow::array::DictionaryArray;
    use arrow::datatypes::{UInt8Type, UInt16Type, UInt32Type};
    if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<UInt8Type>>() {
        let values = d.values().as_any().downcast_ref::<StringArray>()?;
        return Some((DictCodes::U8(d.keys().values(), d.nulls()), values));
    }
    if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<UInt16Type>>() {
        let values = d.values().as_any().downcast_ref::<StringArray>()?;
        return Some((DictCodes::U16(d.keys().values(), d.nulls()), values));
    }
    let d = arr.as_any().downcast_ref::<DictionaryArray<UInt32Type>>()?;
    let values = d.values().as_any().downcast_ref::<StringArray>()?;
    Some((DictCodes::U32(d.keys().values(), d.nulls()), values))
}

/// One dictionary-encoded cell, resolved to its string. Shared by
/// [`Snapshot::dict_value`] and [`text_in`].
fn dict_cell_in(arr: &dyn Array, row: usize) -> Option<&str> {
    use arrow::array::DictionaryArray;
    use arrow::datatypes::{UInt8Type, UInt16Type, UInt32Type};
    if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<UInt8Type>>() {
        return dictionary_cell(d, row);
    }
    if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<UInt16Type>>() {
        return dictionary_cell(d, row);
    }
    dictionary_cell(
        arr.as_any().downcast_ref::<DictionaryArray<UInt32Type>>()?,
        row,
    )
}

/// One plain-`StringArray` cell. Shared by [`Snapshot::str_value`] and
/// [`text_in`].
fn str_in(arr: &dyn Array, row: usize) -> Option<&str> {
    let values = arr.as_any().downcast_ref::<StringArray>()?;
    (row < values.len() && !values.is_null(row)).then(|| values.value(row))
}

/// One dimension cell as text, whatever encoding it arrived in. Shared by
/// [`Snapshot::text_value`] and [`Snapshot::text_at`].
fn text_in(arr: &dyn Array, row: usize) -> Option<&str> {
    dict_cell_in(arr, row).or_else(|| str_in(arr, row))
}

/// One cell as display text, for the types no other accessor reads.
/// Shared by [`Snapshot::display_value`] and [`Snapshot::display_at`].
fn display_in(arr: &dyn Array, row: usize) -> Option<String> {
    use arrow::array::{BooleanArray, Date32Array, Date64Array, TimestampMicrosecondArray};

    if let Some(s) = text_in(arr, row) {
        return Some(s.to_string());
    }
    let present = |a: &dyn Array| row < a.len() && !a.is_null(row);

    if let Some(v) = arr.as_any().downcast_ref::<BooleanArray>() {
        return present(v).then(|| v.value(row).to_string());
    }
    if let Some(v) = arr.as_any().downcast_ref::<Date32Array>() {
        return present(v)
            .then(|| v.value_as_date(row).map(|d| d.to_string()))
            .flatten();
    }
    if let Some(v) = arr.as_any().downcast_ref::<Date64Array>() {
        return present(v)
            .then(|| v.value_as_date(row).map(|d| d.to_string()))
            .flatten();
    }
    if let Some(v) = arr.as_any().downcast_ref::<TimestampMicrosecondArray>() {
        return present(v)
            .then(|| v.value_as_datetime(row).map(|d| d.to_string()))
            .flatten();
    }
    None
}

#[derive(Debug)]
pub struct Snapshot {
    batch: Option<RecordBatch>,
    meta: Vec<ColumnMeta>,
    /// The grouping columns in order; each prefix is one tree level.
    grouping: Vec<String>,
    /// Index of `row_depth`, resolved once. `None` for a flat result.
    depth_col: Option<usize>,
    provenance: Provenance,
    /// The parent/child structure, built once at construction on the
    /// query worker rather than per frame.
    tree: TreeIndex,
}

impl Snapshot {
    pub fn from_batches(
        batches: Vec<RecordBatch>,
        meta: Vec<ColumnMeta>,
        grouping: Vec<String>,
        provenance: Provenance,
    ) -> Result<Snapshot, arrow::error::ArrowError> {
        let batch = match batches.first() {
            None => None,
            Some(_) => Some(concat_preserving_dictionaries(&batches)?),
        };
        // `meta[i]` must describe batch column `i`: the index accessors
        // rely on it, and the by-name ones are implemented over them.
        if let Some(b) = &batch {
            let names: Vec<&str> = b
                .schema_ref()
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();
            let described: Vec<&str> = meta.iter().map(|m| m.name.as_str()).collect();
            if names != described {
                return Err(arrow::error::ArrowError::SchemaError(format!(
                    "snapshot meta {described:?} does not match batch columns {names:?}"
                )));
            }
        }
        // A mixed flag the cell renderer consults must be a boolean column
        // of this batch other than the value's own; anything else would read
        // an arbitrary column's truthiness as "the rows disagree".
        for (i, m) in meta.iter().enumerate() {
            let Some(flag) = m.mixed_flag else { continue };
            let ok = flag != i
                && flag < meta.len()
                && batch.as_ref().is_none_or(|b| {
                    b.column(flag).data_type() == &arrow::datatypes::DataType::Boolean
                });
            if !ok {
                return Err(arrow::error::ArrowError::SchemaError(format!(
                    "column '{}' names mixed flag {flag}, which is not a boolean \
                     companion column of this snapshot",
                    m.name
                )));
            }
        }
        let depth_col = batch
            .as_ref()
            .and_then(|b| b.schema().index_of("row_depth").ok());
        let mut snapshot = Snapshot {
            batch,
            meta,
            grouping,
            depth_col,
            provenance,
            tree: TreeIndex::default(),
        };
        snapshot.tree = TreeIndex::build(&snapshot);
        Ok(snapshot)
    }

    pub fn rows(&self) -> usize {
        self.batch.as_ref().map_or(0, |b| b.num_rows())
    }

    pub fn column_names(&self) -> Vec<&str> {
        self.meta.iter().map(|m| m.name.as_str()).collect()
    }

    /// The grouping columns in order; each prefix is one tree level.
    pub fn grouping(&self) -> &[String] {
        &self.grouping
    }

    pub fn grouping_len(&self) -> usize {
        self.grouping.len()
    }

    /// How many columns the snapshot carries — the valid range for the
    /// `_at` accessors is `0..columns()` when a batch is present. An
    /// empty result carries no batch at all, in which case every `_at`
    /// accessor returns `None` regardless of index.
    pub fn columns(&self) -> usize {
        self.meta.len()
    }

    /// Resolve a column index once for repeated cell reads. Index accessors
    /// avoid a name lookup for every cell during the snapshot's lifetime.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.meta.iter().position(|m| m.name == name)
    }

    pub fn meta_at(&self, idx: usize) -> Option<&ColumnMeta> {
        self.meta.get(idx)
    }

    pub fn meta(&self, name: &str) -> Option<&ColumnMeta> {
        self.meta.iter().find(|m| m.name == name)
    }

    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    fn column_at(&self, idx: usize) -> Option<&dyn Array> {
        let batch = self.batch.as_ref()?;
        (idx < batch.num_columns()).then(|| batch.column(idx).as_ref())
    }

    fn column(&self, name: &str) -> Option<&dyn Array> {
        self.column_at(self.column_index(name)?)
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

    /// One numeric cell converted to f64. Supports float, integer, and decimal
    /// arrays; large integers or decimals can lose precision in the conversion.
    /// Returns `None` for an absent or unsupported column, an out-of-range row,
    /// or NULL. NULL must remain distinct from zero: non-attributable measures
    /// are deliberately blanked at grouping levels they do not belong to.
    pub fn f64_value(&self, name: &str, row: usize) -> Option<f64> {
        f64_in(self.column(name)?, row)
    }

    /// The by-index twin of [`Self::f64_value`]. The blotter resolves a
    /// column once via [`Self::column_index`] and reads every subsequent
    /// row through this, without a name lookup.
    pub fn f64_at(&self, idx: usize, row: usize) -> Option<f64> {
        f64_in(self.column_at(idx)?, row)
    }

    /// Zero-copy over the whole column, **only when it is exactly Int64**.
    ///
    /// DuckDB picks the narrowest integer that fits, so a computed column
    /// often arrives narrower: `row_depth` is `Int32`. Use
    /// [`Self::i64_value`] unless the width is known, or this returns
    /// `None` for a column that is plainly an integer. Carries no null
    /// bitmap, for the same reason [`Self::f64_column`] does not.
    pub fn i64_column(&self, name: &str) -> Option<&[i64]> {
        Some(
            self.column(name)?
                .as_any()
                .downcast_ref::<Int64Array>()?
                .values(),
        )
    }

    /// One integer cell converted to i64: signed 8/16/32/64-bit and unsigned
    /// 8/16/32-bit arrays are supported. Returns `None` for other types, an
    /// absent column, an out-of-range row, or NULL. Computed columns can be narrow:
    /// `row_depth` arrives as Int32 while a declared i64 column arrives as Int64.
    pub fn i64_value(&self, name: &str, row: usize) -> Option<i64> {
        i64_in(self.column(name)?, row)
    }

    /// The by-index twin of [`Self::i64_value`].
    pub fn i64_at(&self, idx: usize, row: usize) -> Option<i64> {
        i64_in(self.column_at(idx)?, row)
    }

    /// Strings are returned as the Arrow array: offsets make a `&[&str]`
    /// impossible without allocating, and the renderer reads by row.
    pub fn str_column(&self, name: &str) -> Option<&StringArray> {
        self.column(name)?.as_any().downcast_ref::<StringArray>()
    }

    /// Dictionary codes and their shared string dictionary. Code comparisons
    /// are local to this column and require a canonical dictionary; use text
    /// accessors when comparing across snapshots.
    ///
    /// DuckDB sizes ENUM keys by vocabulary: up to 255 values use UInt8, up to
    /// 65,535 use UInt16, and larger vocabularies use UInt32. All three widths
    /// are supported. Check the key null bitmap before reading a raw code.
    pub fn dict_column(&self, name: &str) -> Option<(DictCodes<'_>, &StringArray)> {
        dict_column_in(self.column(name)?)
    }

    /// The by-index twin of [`Self::dict_column`]. `None` both when the
    /// index is out of range and when the column at it is not
    /// dictionary-encoded — the same "absent or wrong shape" answer every
    /// other `_at` accessor gives.
    pub fn dict_codes_at(&self, idx: usize) -> Option<(DictCodes<'_>, &StringArray)> {
        dict_column_in(self.column_at(idx)?)
    }

    /// One plain-string cell without exposing an Arrow array to the caller.
    /// Returns `None` for an absent or non-string column, NULL, or an out-of-range
    /// row. Use `text_value` to accept dictionary encoding too.
    pub fn str_value(&self, name: &str, row: usize) -> Option<&str> {
        str_in(self.column(name)?, row)
    }

    /// One dictionary-encoded cell, resolved to its string. The codes are
    /// available for comparisons and grouping within the column; this is for
    /// display.
    ///
    /// `None` for a NULL cell. The key array's null bitmap is the only
    /// thing distinguishing "this row has no value for this dimension"
    /// from dictionary entry 0 — read through the raw codes instead and
    /// the grand-total row, which belongs to no book, displays a real book
    /// name.
    pub fn dict_value(&self, name: &str, row: usize) -> Option<&str> {
        dict_cell_in(self.column(name)?, row)
    }

    /// One dimension cell as text, whatever encoding it arrived in.
    ///
    /// A column's encoding depends on the *era*, not only on the schema:
    /// the live path interns dimensions as DuckDB ENUMs and gets
    /// dictionary-encoded columns back, while an as-of read skips the
    /// interning and produces plain strings. `ColumnMeta` does not
    /// record which, so a caller that picks an accessor by column name
    /// reads a value in one era and `None` in the other, with no signal
    /// that anything changed. Anything rendering a dimension should come
    /// through here rather than choose for itself.
    pub fn text_value(&self, name: &str, row: usize) -> Option<&str> {
        text_in(self.column(name)?, row)
    }

    /// The by-index twin of [`Self::text_value`].
    pub fn text_at(&self, idx: usize, row: usize) -> Option<&str> {
        text_in(self.column_at(idx)?, row)
    }

    /// One cell as display text for strings, booleans, dates, and microsecond
    /// timestamps. Returns `None` for unsupported types, missing columns, NULL,
    /// or out-of-range rows. Text is owned because formatted dates and times
    /// cannot borrow from the array. Numeric formatting belongs to the caller;
    /// use `f64_value` for numbers.
    pub fn display_value(&self, name: &str, row: usize) -> Option<String> {
        display_in(self.column(name)?, row)
    }

    /// The by-index twin of [`Self::display_value`].
    pub fn display_at(&self, idx: usize, row: usize) -> Option<String> {
        display_in(self.column_at(idx)?, row)
    }

    /// Whether column `idx` is mixed at `row`: an ungrouped dimension whose
    /// rows under this tree row disagree (see [`ColumnMeta::mixed_flag`]).
    /// False for a column with no flag, past the end, and for a NULL flag —
    /// a spine row the column's grain has no rows for is blank, not mixed.
    pub fn is_mixed_at(&self, idx: usize, row: usize) -> bool {
        let Some(flag) = self.meta.get(idx).and_then(|m| m.mixed_flag) else {
            return false;
        };
        self.column_at(flag)
            .and_then(|a| a.as_any().downcast_ref::<arrow::array::BooleanArray>())
            .is_some_and(|a| row < a.len() && a.is_valid(row) && a.value(row))
    }

    /// How many grouping columns are present on this row — 0 is the grand
    /// total, `grouping_len` a leaf. The compiler emits it directly rather
    /// than as a `GROUPING()` bitmask, whose width would otherwise change
    /// with how deep the query was told to materialize.
    ///
    /// `row_depth`'s column index is resolved once at construction
    /// ([`Self::from_batches`]) rather than searched by name here, so a
    /// per-cell read costs one array access, not a name lookup too.
    pub fn depth_of_row(&self, row: usize) -> Option<usize> {
        let depth = self.i64_at(self.depth_col?, row)?;
        usize::try_from(depth)
            .ok()
            .filter(|d| *d <= self.grouping.len())
    }

    /// Whether the result carries `row_depth` at all. A result without it
    /// is flat — every row a root — and asking [`Self::depth_of_row`] row
    /// by row cannot distinguish that from a column of NULLs.
    pub fn has_depth_column(&self) -> bool {
        self.depth_col.is_some()
    }

    /// The parent/child structure prepared during snapshot construction.
    pub fn tree(&self) -> &TreeIndex {
        &self.tree
    }
}

/// One column of fixture data, in the shapes [`Snapshot`] hands back.
#[cfg(any(test, feature = "test-support"))]
pub enum TestColumn {
    F64(Vec<Option<f64>>),
    I64(Vec<i64>),
    /// Int32, matching the encoding of computed columns such as `row_depth`.
    I32(Vec<i32>),
    Str(Vec<Option<&'static str>>),
    /// Dictionary-encoded, matching live ENUM columns. Keys use UInt8 up to
    /// 255 distinct values, UInt16 up to 65,535, and UInt32 above that, so
    /// fixtures can exercise every supported vocabulary width.
    Dict(Vec<Option<String>>),
    /// Date32, matching a declared date column. Use this to exercise typed date
    /// cells; `Dict` and `Str` fixtures exercise date labels stored as text.
    Date(Vec<Option<chrono::NaiveDate>>),
    /// Boolean, matching the compiler's mixed-flag companion columns.
    Bool(Vec<Option<bool>>),
}

/// Build a dictionary column the way DuckDB would: distinct values in
/// first-seen order, keys sized to the vocabulary, NULL cells carrying a
/// cleared key bit rather than a code.
#[cfg(any(test, feature = "test-support"))]
fn dictionary_fixture(
    cells: &[Option<String>],
) -> (arrow::datatypes::DataType, arrow::array::ArrayRef) {
    use arrow::array::{ArrayRef, DictionaryArray, UInt8Array, UInt16Array, UInt32Array};
    use arrow::datatypes::{DataType, UInt8Type, UInt16Type, UInt32Type};
    use std::collections::HashMap;
    use std::sync::Arc;

    // Intern each value once in first-seen order. Hash lookup keeps fixture
    // construction from scaling with cells times vocabulary size.
    let mut distinct: Vec<&str> = Vec::new();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for c in cells.iter().flatten() {
        seen.entry(c.as_str()).or_insert_with(|| {
            distinct.push(c.as_str());
            distinct.len() - 1
        });
    }
    let values = Arc::new(StringArray::from(distinct.clone()));
    let code_of = |s: &str| *seen.get(s).expect("every non-null cell is interned above");

    if distinct.len() > u16::MAX as usize {
        let keys: UInt32Array = cells
            .iter()
            .map(|c| c.as_deref().map(|s| code_of(s) as u32))
            .collect();
        return (
            DataType::Dictionary(Box::new(DataType::UInt32), Box::new(DataType::Utf8)),
            Arc::new(
                DictionaryArray::<UInt32Type>::try_new(keys, values).expect("fixture dictionary"),
            ) as ArrayRef,
        );
    }
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
    /// Downstream tests can build results without an Arrow dependency or
    /// matching its version to the data layer's DuckDB dependency.
    ///
    /// The first `grouping_len` columns are the grouping columns, which is
    /// the order the compiler emits.
    pub fn for_tests(columns: Vec<(ColumnMeta, TestColumn)>, grouping_len: usize) -> Snapshot {
        Snapshot::for_tests_with_provenance(columns, grouping_len, Provenance::default())
    }

    /// Build a snapshot with caller-supplied provenance. Document-panel fixtures
    /// use `Provenance.datasets[0]`'s source time and generation to exercise
    /// freshness and draft-base comparisons; the ordinary builder supplies empty
    /// provenance.
    ///
    /// Supply a generation ID to exercise identity comparisons. `None`
    /// exercises the document panel's source-time fallback, which cannot
    /// distinguish corrected republishes at the same source time.
    pub fn for_tests_with_provenance(
        columns: Vec<(ColumnMeta, TestColumn)>,
        grouping_len: usize,
        provenance: Provenance,
    ) -> Snapshot {
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
                    TestColumn::I32(v) => (
                        DataType::Int32,
                        Arc::new(arrow::array::Int32Array::from(v.clone())),
                    ),
                    TestColumn::Str(v) => (DataType::Utf8, Arc::new(StringArray::from(v.clone()))),
                    TestColumn::Dict(v) => dictionary_fixture(v),
                    TestColumn::Date(v) => (
                        DataType::Date32,
                        Arc::new(arrow::array::Date32Array::from(
                            v.iter()
                                .map(|d| d.map(arrow::datatypes::Date32Type::from_naive_date))
                                .collect::<Vec<_>>(),
                        )),
                    ),
                    TestColumn::Bool(v) => (
                        DataType::Boolean,
                        Arc::new(arrow::array::BooleanArray::from(v.clone())),
                    ),
                };
                (Field::new(&meta.name, ty, true), array)
            })
            .unzip();
        let grouping = columns
            .iter()
            .take(grouping_len)
            .map(|(m, _)| m.name.clone())
            .collect();
        let meta = columns.into_iter().map(|(m, _)| m).collect();
        let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)
            .expect("fixture columns must be the same length");
        Snapshot::from_batches(vec![batch], meta, grouping, provenance).expect("fixture snapshot")
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
                summable: false,
                mixed_flag: None,
            })
            .collect()
    }

    fn snapshot() -> Snapshot {
        Snapshot::from_batches(
            batches(),
            meta(),
            vec!["book".into()],
            Provenance::default(),
        )
        .unwrap()
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
        // Non-attributable cells are NULL. Arrow keeps an arbitrary payload under
        // the cleared null bit, so only the bitmap distinguishes them from values.
        let s = Snapshot::for_tests(
            vec![(
                ColumnMeta {
                    name: "cross_gamma".into(),
                    attribution_by_depth: vec![Attribution::NonAttributable, Attribution::Additive],
                    scope_semantics: ScopeSemantics::Direct,
                    summable: false,
                    mixed_flag: None,
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
                    summable: false,
                    mixed_flag: None,
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
            summable: false,
            mixed_flag: None,
        }
    }

    #[test]
    fn depth_is_read_at_whatever_integer_width_it_arrives_in() {
        // Use Int32 for `row_depth`, matching computed query output. Reading only
        // Int64 would lose every row's depth and apply the wrong attribution.
        let schema = Arc::new(Schema::new(vec![
            Field::new("row_depth", DataType::Int32, true),
            Field::new("wide", DataType::Int64, true),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(arrow::array::Int32Array::from(vec![Some(0), Some(2), None])),
                Arc::new(Int64Array::from(vec![Some(7), Some(8), Some(9)])),
            ],
        )
        .unwrap();
        let s = Snapshot::from_batches(
            vec![batch],
            vec![dim("row_depth"), dim("wide")],
            vec!["wide".into(), "x".into()],
            Provenance::default(),
        )
        .unwrap();

        assert_eq!(s.depth_of_row(0), Some(0));
        assert_eq!(s.depth_of_row(1), Some(2));
        assert_eq!(s.depth_of_row(2), None, "a NULL depth is not depth 0");
        assert_eq!(s.i64_value("row_depth", 1), Some(2), "Int32 is readable");
        assert_eq!(s.i64_value("wide", 0), Some(7), "Int64 still is");
        assert_eq!(
            s.i64_column("row_depth"),
            None,
            "the zero-copy slice is Int64-only, which is why i64_value exists"
        );
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
        // A vocabulary above 255 values requires UInt16 keys. Both dictionary
        // and text cell access must support this width.
        let wide: Vec<Option<String>> = (0..300).map(|i| Some(format!("U{i:04}"))).collect();
        let narrow: Vec<Option<String>> = (0..200).map(|i| Some(format!("U{i:04}"))).collect();

        let s = Snapshot::for_tests(vec![(dim("underlying_ref"), TestColumn::Dict(wide))], 1);
        assert_eq!(s.dict_value("underlying_ref", 299), Some("U0299"));
        assert!(matches!(
            s.dict_column("underlying_ref").unwrap().0,
            DictCodes::U16(..)
        ));

        let s = Snapshot::for_tests(vec![(dim("underlying_ref"), TestColumn::Dict(narrow))], 1);
        assert_eq!(s.dict_value("underlying_ref", 199), Some("U0199"));
        assert!(matches!(
            s.dict_column("underlying_ref").unwrap().0,
            DictCodes::U8(..)
        ));
    }

    #[test]
    fn dimension_codes_report_a_rolled_up_row_as_having_none() {
        // Code reads must honor NULL just as text reads do. A raw zero under a
        // NULL key must not identify the grand total as a real book.
        let s = Snapshot::for_tests(
            vec![(
                dim("book"),
                TestColumn::Dict(vec![Some("BK000".into()), None, Some("BK001".into())]),
            )],
            1,
        );
        let (codes, values) = s.dict_column("book").unwrap();
        assert_eq!(values.len(), 2, "only the real books are in the dictionary");
        assert!(!codes.is_null(0));
        assert!(codes.is_null(1), "row 1 is the rolled-up level");
        assert!(!codes.is_null(2));
        assert_eq!(codes.code(1), None, "and has no code to group on");
        assert_ne!(codes.code(0), codes.code(2), "distinct books stay distinct");
        assert!(codes.is_null(99), "past the end is not a value either");
    }

    #[test]
    fn a_dimension_past_the_65535_value_cliff_still_reads() {
        // Vocabulary boundaries require three key widths: 255 -> UInt8,
        // 256 -> UInt16, 65,535 -> UInt16, and 65,536 -> UInt32.
        let wide: Vec<Option<String>> = (0..65_536).map(|i| Some(format!("U{i:06}"))).collect();
        let s = Snapshot::for_tests(vec![(dim("underlying_ref"), TestColumn::Dict(wide))], 1);
        assert!(matches!(
            s.dict_column("underlying_ref").unwrap().0,
            DictCodes::U32(..)
        ));
        assert_eq!(s.dict_value("underlying_ref", 65_535), Some("U065535"));
        assert_eq!(s.text_value("underlying_ref", 0), Some("U000000"));
    }

    #[test]
    fn a_summed_integer_measure_is_readable_as_a_number() {
        // An integer sum arrives as Decimal128(38, 0). Verify that numeric access
        // reads the aggregate and preserves NULL instead of blanking the column.
        use arrow::array::Decimal128Array;

        let values = Decimal128Array::from(vec![Some(1_234i128), None, Some(-7i128)])
            .with_precision_and_scale(38, 0)
            .unwrap();
        let schema = Arc::new(Schema::new(vec![Field::new(
            "qty",
            values.data_type().clone(),
            true,
        )]));
        let batch = RecordBatch::try_new(schema, vec![Arc::new(values)]).unwrap();
        let s = Snapshot::from_batches(
            vec![batch],
            vec![dim("qty")],
            vec!["qty".into()],
            Provenance::default(),
        )
        .unwrap();

        assert_eq!(s.f64_value("qty", 0), Some(1234.0));
        assert_eq!(s.f64_value("qty", 1), None, "a NULL is still a NULL");
        assert_eq!(s.f64_value("qty", 2), Some(-7.0));
        assert_eq!(s.f64_value("qty", 99), None, "past the end");
    }

    #[test]
    fn a_date_or_boolean_column_is_displayable() {
        // Date, timestamp, and bool declarations produce typed Arrow arrays.
        // The display accessor must render their values while preserving NULL.
        use arrow::array::{BooleanArray, Date32Array};

        let schema = Arc::new(Schema::new(vec![
            Field::new("business_date", DataType::Date32, true),
            Field::new("is_live", DataType::Boolean, true),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                // 2026-08-30 is 20695 days after the epoch.
                Arc::new(Date32Array::from(vec![Some(20_695), None])),
                Arc::new(BooleanArray::from(vec![Some(true), None])),
            ],
        )
        .unwrap();
        let s = Snapshot::from_batches(
            vec![batch],
            vec![dim("business_date"), dim("is_live")],
            vec!["business_date".into()],
            Provenance::default(),
        )
        .unwrap();

        assert_eq!(
            s.display_value("business_date", 0).as_deref(),
            Some("2026-08-30")
        );
        assert_eq!(s.display_value("is_live", 0).as_deref(), Some("true"));
        assert_eq!(s.display_value("business_date", 1), None, "NULL stays NULL");
        assert_eq!(s.display_value("is_live", 1), None);
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
        // A wide dictionary split across query batches must preserve values and
        // codes during concatenation.
        for distinct in [200usize, 300] {
            let cells: Vec<Option<String>> =
                (0..distinct).map(|i| Some(format!("U{i:04}"))).collect();
            let (ty, array) = dictionary_fixture(&cells);
            let schema = Arc::new(Schema::new(vec![Field::new("underlying_ref", ty, true)]));
            let batch = RecordBatch::try_new(schema, vec![array]).unwrap();
            let s = Snapshot::from_batches(
                vec![batch.clone(), batch],
                vec![dim("underlying_ref")],
                vec!["underlying_ref".into()],
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
        // A joined view reports the earliest known input timestamp.
        let mut p = Provenance::default();
        p.datasets.push(Freshness {
            dataset: "risk_snapshot".into(),
            as_of: Some("2026-08-30T14:32:00Z".into()),
            generation: Some(47),
        });
        p.datasets.push(Freshness {
            dataset: "implied_vol_summary".into(),
            as_of: Some("2026-08-30T07:00:00Z".into()),
            generation: Some(3),
        });
        let s = Snapshot::from_batches(batches(), meta(), vec!["book".into()], p).unwrap();
        assert_eq!(
            s.provenance().stalest().map(|f| f.dataset.as_str()),
            Some("implied_vol_summary")
        );
    }

    #[test]
    fn an_empty_result_is_a_valid_snapshot() {
        let s = Snapshot::from_batches(
            Vec::new(),
            meta(),
            vec!["book".into()],
            Provenance::default(),
        )
        .unwrap();
        assert_eq!(s.rows(), 0);
        assert!(s.f64_column("delta01").is_none_or(|c| c.is_empty()));
    }

    #[test]
    fn index_accessors_agree_with_their_by_name_twins_under_every_type() {
        // The blotter resolves a column once and reads by index for the
        // rest of the snapshot's life. Every typed accessor here
        // must answer exactly what its by-name twin answers, including
        // NULL, past-the-end, and the narrow-integer and dictionary
        // shapes DuckDB actually emits.
        let s = Snapshot::for_tests(
            vec![
                (
                    dim("book"),
                    TestColumn::Dict(vec![Some("BK000".into()), None]),
                ),
                (dim("lhu"), TestColumn::Str(vec![Some("L1"), None])),
                (dim("row_depth"), TestColumn::I32(vec![1, 0])),
                (dim("delta01"), TestColumn::F64(vec![Some(1.5), None])),
            ],
            2,
        );
        assert_eq!(s.columns(), 4);
        assert_eq!(s.column_index("book"), Some(0));
        assert_eq!(s.column_index("delta01"), Some(3));
        assert_eq!(s.column_index("nonesuch"), None);
        assert_eq!(s.meta_at(3).map(|m| m.name.as_str()), Some("delta01"));
        assert!(s.meta_at(4).is_none());

        // Absolute values, not just by-name/by-index agreement: a shared
        // off-by-one in both paths would agree with itself and still be
        // wrong.
        assert_eq!(s.text_at(0, 0), Some("BK000"));
        assert_eq!(s.f64_at(3, 0), Some(1.5));
        assert_eq!(s.i64_at(2, 1), Some(0));

        for row in 0..3 {
            assert_eq!(
                s.text_at(0, row),
                s.text_value("book", row),
                "book row {row}"
            );
            assert_eq!(s.text_at(1, row), s.text_value("lhu", row), "lhu row {row}");
            assert_eq!(
                s.i64_at(2, row),
                s.i64_value("row_depth", row),
                "depth row {row}"
            );
            assert_eq!(
                s.f64_at(3, row),
                s.f64_value("delta01", row),
                "delta row {row}"
            );
            assert_eq!(
                s.display_at(0, row),
                s.display_value("book", row),
                "display row {row}"
            );
        }
        assert_eq!(s.f64_at(3, 1), None, "NULL is still NULL by index");
        assert_eq!(
            s.f64_at(9, 0),
            None,
            "an index past the end is None, not a panic"
        );
        assert!(s.dict_codes_at(0).is_some());
        assert!(
            s.dict_codes_at(1).is_none(),
            "a plain string column has no codes"
        );
    }

    #[test]
    fn a_meta_list_that_disagrees_with_the_batch_is_refused() {
        // Index accessors assume meta[i] describes batch column i. The
        // compiler keeps them aligned; a fixture or a future refactor
        // that does not must fail here, loudly, not read the wrong
        // attribution for every cell.
        let mut wrong = meta();
        wrong.swap(0, 2);
        let err =
            Snapshot::from_batches(batches(), wrong, vec!["book".into()], Provenance::default());
        assert!(
            matches!(err, Err(arrow::error::ArrowError::SchemaError(_))),
            "misaligned meta must not build a snapshot: {err:?}"
        );
    }

    /// A mixed flag must name a boolean companion column: pointing it at a
    /// value column would read that column's truthiness as "the rows
    /// disagree". A NULL flag is treated as false; the query compiler
    /// coalesces unmatched flags to false before snapshot construction.
    #[test]
    fn a_mixed_flag_must_name_a_boolean_companion_and_a_null_flag_is_not_mixed() {
        let mut wrong = meta();
        wrong[0].mixed_flag = Some(2);
        let err =
            Snapshot::from_batches(batches(), wrong, vec!["book".into()], Provenance::default());
        assert!(
            matches!(err, Err(arrow::error::ArrowError::SchemaError(_))),
            "a float column is not a flag: {err:?}"
        );
        let mut own = meta();
        own[0].mixed_flag = Some(0);
        assert!(
            Snapshot::from_batches(batches(), own, vec!["book".into()], Provenance::default())
                .is_err(),
            "a column is not its own flag"
        );

        let s = Snapshot::for_tests(
            vec![
                (
                    ColumnMeta {
                        mixed_flag: Some(1),
                        ..dim("strike")
                    },
                    TestColumn::Str(vec![None, None, Some("1")]),
                ),
                (
                    dim("strike#mixed"),
                    TestColumn::Bool(vec![Some(true), None, Some(false)]),
                ),
            ],
            0,
        );
        assert!(s.is_mixed_at(0, 0));
        assert!(!s.is_mixed_at(0, 1), "a NULL flag is blank, not mixed");
        assert!(!s.is_mixed_at(0, 2));
        assert!(!s.is_mixed_at(0, 9), "past the end");
        assert!(
            !s.is_mixed_at(1, 0),
            "the flag column has no flag of its own"
        );
    }

    #[test]
    fn depth_is_read_through_the_cached_column() {
        let s = snapshot();
        assert_eq!(s.depth_of_row(0), Some(1));
        assert_eq!(s.depth_of_row(1), Some(0));
        let flat = Snapshot::for_tests(vec![(dim("delta01"), TestColumn::F64(vec![Some(1.0)]))], 0);
        assert_eq!(flat.depth_of_row(0), None, "no depth column, no depth");
    }
}
