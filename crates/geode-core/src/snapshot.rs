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
/// Every batch of one query carries the same derived ENUM, so its keys can
/// simply be concatenated against that one dictionary rather than
/// unified — cheaper, and it keeps the codes stable, which is what §7.2
/// lets a renderer compare and group on.
///
/// **It also guards §7.2's code identity, which the fallback does not.**
/// This comment has been wrong twice, so the measurements are recorded
/// rather than the conclusions.
///
/// The original claim — that arrow's `concat` appends dictionaries and
/// overflows the key space — is false at arrow 58.4.0: 40 batches of a
/// 200-value dictionary concatenate to 200 entries at UInt8, 300 stays
/// 300 at UInt16, and a union that genuinely cannot fit the key type
/// (disjoint 200-value dictionaries at UInt8) returns
/// `Err("Dictionary key bigger than the key type")` rather than
/// corrupting.
///
/// The correction that replaced it — "the fallback is safe; this path
/// costs speed only" — was measured without nulls, and that is the case
/// that matters. With a NULL present the fallback emits a *duplicate*
/// dictionary entry:
///
/// ```text
/// no nulls      dict=["A","B","C"]      codes=[0,1,2,0]
/// null present  dict=["A","B","C","A"]  codes=[0,1,0,2,3]
/// ```
///
/// `"A"` then has both code 0 and code 3. `dict_value` still returns the
/// right string, so nothing visibly breaks — but §7.2's whole premise is
/// that the renderer compares and groups on codes, and after a fallback
/// concat code equality no longer implies string equality. A renderer
/// grouping by code would split one book in two. A NULL in a dimension
/// column is not exotic: it *is* the rolled-up row, present in
/// essentially every result.
///
/// The fast path is unreached in production today (every batch of one
/// query carries the same derived ENUM), so this is a latent property of
/// the fallback rather than a live defect. Re-measure before trusting any
/// of it across an arrow upgrade.
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

        // Both key widths, because the width follows the vocabulary size
        // rather than the schema — see `dict_column`.
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

    /// Whether this row has no value — the rolled-up level, not a code.
    ///
    /// §7.2 tells the renderer to compare and group on codes, so this has
    /// to be askable here. Without it a code-based renderer reads the
    /// arbitrary code sitting under a NULL — in practice 0 — and puts the
    /// grand-total row under a real book, which is exactly the defect
    /// [`Snapshot::dict_value`] consults the bitmap to avoid.
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
    // A declared `i64` measure does not come back as an integer.
    // `ColumnType::I64` is BIGINT, the default aggregate is `Sum`, and
    // DuckDB's `sum(BIGINT)` is HUGEINT — exported as
    // `Decimal128(38, 0)`. Nothing forbids such a measure, so without
    // this arm every one of its cells read blank, and under §6.3 a
    // blank cell is a positive claim: "this number does not belong to
    // this row". Turning "I cannot read this type" into that claim is
    // the worst failure available here.
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
    /// query worker rather than per frame (Phase 3 §5.5).
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

    /// Resolve a column's index once; the blotter reads by index for the
    /// rest of the snapshot's life rather than searching by name per cell
    /// (spec §5.5).
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
        f64_in(self.column(name)?, row)
    }

    /// The by-index twin of [`Self::f64_value`]. The blotter resolves a
    /// column once via [`Self::column_index`] and reads every subsequent
    /// row through this, never by name (spec §5.5).
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

    /// One integer cell, at whatever width it arrived in, widened to i64.
    /// `None` when the column is absent or not an integer, the row is past
    /// the end, or the value is NULL.
    ///
    /// The width is DuckDB's choice, not the schema's: it emits the
    /// narrowest type that fits, so `row_depth` — a small computed
    /// integer — comes back `Int32` while a declared `i64` column comes
    /// back `Int64`. Matching only `Int64` made every real result's depth
    /// unreadable while every fixture built on `Int64Array` passed.
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

    /// A dictionary-encoded dimension column: per-row codes plus the
    /// shared value dictionary. The renderer compares and formats on the
    /// codes rather than the strings (spec §7.2).
    ///
    /// The key width is a property of the data, not the schema: DuckDB
    /// sizes an ENUM's key to its vocabulary. Measured at the boundaries:
    /// 255 → UInt8, 256 → UInt16, 65535 → UInt16, 65536 → UInt32. All
    /// three are matched.
    ///
    /// An earlier version matched UInt8 only, and every dimension with a
    /// real underlying list fell through this *and* `str_column` and
    /// rendered blank. The version after it matched UInt8 and UInt16 and
    /// called that exhaustive — it was not, and the comment saying so
    /// would have stopped the next reader re-checking. If a further width
    /// ever appears, this is the third place to find out about it.
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

    /// One string cell, or `None` when the column is absent, the value is
    /// NULL, or the row is past the end.
    ///
    /// A renderer walks rows, and [`Self::str_column`] hands back the Arrow
    /// array — which would make every caller name an Arrow type and bring
    /// its traits into scope, exactly what this module exists to prevent.
    pub fn str_value(&self, name: &str, row: usize) -> Option<&str> {
        str_in(self.column(name)?, row)
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
        dict_cell_in(self.column(name)?, row)
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
        text_in(self.column(name)?, row)
    }

    /// The by-index twin of [`Self::text_value`].
    pub fn text_at(&self, idx: usize, row: usize) -> Option<&str> {
        text_in(self.column_at(idx)?, row)
    }

    /// One cell as display text, for the types no other accessor reads.
    ///
    /// `ColumnType` accepts `date`, `timestamp` and `bool`, and DuckDB
    /// emits them as `Date32`, `Timestamp(Micros)` and `Boolean` — none of
    /// which any typed accessor here matched, so a legal declaration
    /// produced a column of blank cells. Under §6.3 a blank cell asserts
    /// "this number does not belong to this row", so an unreadable type
    /// silently became a claim about the data. `business_date` is the
    /// standing example: `store/ddl.rs` names it as the attribute worth
    /// displaying, and the sample config declares it `utf8`, which was a
    /// workaround for this gap rather than a preference.
    ///
    /// Returns owned text because a formatted date has nowhere to borrow
    /// from. Numbers are deliberately not formatted here — precision is
    /// the renderer's decision, so it should ask `f64_value` first.
    pub fn display_value(&self, name: &str, row: usize) -> Option<String> {
        display_in(self.column(name)?, row)
    }

    /// The by-index twin of [`Self::display_value`].
    pub fn display_at(&self, idx: usize, row: usize) -> Option<String> {
        display_in(self.column_at(idx)?, row)
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

    /// The parent/child structure, built once here (Phase 3 §5.5).
    pub fn tree(&self) -> &TreeIndex {
        &self.tree
    }
}

/// One column of fixture data, in the shapes [`Snapshot`] hands back.
#[cfg(any(test, feature = "test-support"))]
pub enum TestColumn {
    F64(Vec<Option<f64>>),
    I64(Vec<i64>),
    /// A narrower integer, as DuckDB actually emits `row_depth`. A
    /// fixture that only builds `Int64` cannot see the width defect that
    /// made `depth_of_row` return `None` for every real result.
    I32(Vec<i32>),
    Str(Vec<Option<&'static str>>),
    /// Dictionary-encoded, the shape a live ENUM column arrives in. The
    /// key width follows DuckDB's own rule — UInt8 up to 255 distinct
    /// values, UInt16 above — so a fixture that crosses the cliff
    /// exercises what a real underlying list does.
    Dict(Vec<Option<String>>),
    /// A real `Date32` column — DuckDB's own encoding for a `date`
    /// column (spec §3.6), which `display_in` reads through
    /// `Date32Array::value_as_date`. Most fixtures spell a date as ISO
    /// text through `Dict`/`Str` instead, since what a reader gets off an
    /// axis is its label either way — this variant exists for a fixture
    /// that must exercise the real column type, such as a typed
    /// `Value::Date` cell.
    Date(Vec<Option<chrono::NaiveDate>>),
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

    // A `Vec::contains`/`position` scan made this O(cells × distinct
    // values); a benchmark fixture with a large vocabulary made building
    // the fixture itself the bottleneck rather than the code under test.
    // The map interns each distinct value once, in first-seen order.
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
    /// Downstream crates need `Snapshot` fixtures, and this module's whole
    /// premise is that nothing outside it names an Arrow type — so the
    /// fixture builder lives here rather than making every test crate
    /// reach for `arrow` and pin its version to match duckdb's.
    ///
    /// The first `grouping_len` columns are the grouping columns, which is
    /// the order the compiler emits.
    pub fn for_tests(columns: Vec<(ColumnMeta, TestColumn)>, grouping_len: usize) -> Snapshot {
        Snapshot::for_tests_with_provenance(columns, grouping_len, Provenance::default())
    }

    /// The same builder with a provenance of the caller's choosing.
    ///
    /// A document panel reads its generation's source time off
    /// `Provenance.datasets[0].as_of` (market-data spec §8.4 — the source
    /// time, not a `gen_id`, is the identity a draft compares), so a
    /// fixture with the default empty provenance cannot exercise
    /// anything that depends on it: the freshness line, the draft's
    /// `base`, or `Behind`. `for_tests` keeps its two-argument spelling
    /// because most fixtures do not care.
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
                    summable: false,
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
        }
    }

    #[test]
    fn depth_is_read_at_whatever_integer_width_it_arrives_in() {
        // DuckDB emits the narrowest integer that fits, and `row_depth` is
        // small, so a real result carries Int32. Every fixture here built
        // it as Int64, so `depth_of_row` returned None for every row of
        // every real query and nothing noticed — a renderer treats that as
        // depth 0 and reads the wrong attribution for the whole tree.
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
        // Above 255 distinct values the keys widen to UInt16. Matching
        // only UInt8 made such a column fall through dict_column *and*
        // str_column and render blank.
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
        // §7.2 tells the renderer to compare and group on codes, so the
        // code path has to answer the same question `dict_value` does. It
        // did not: the arbitrary code under a NULL is 0, so a code-based
        // renderer put the grand-total row under a real book — the same
        // defect, reached through the API the spec points at.
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
        // DuckDB sizes an ENUM's key to its vocabulary, and the widths do
        // not stop at UInt16: measured, 255 -> UInt8, 256 -> UInt16,
        // 65535 -> UInt16, 65536 -> UInt32. Matching only the first two
        // left the same silent blank one cliff further out.
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
        // `ColumnType::I64` is BIGINT, the default aggregate is Sum, and
        // DuckDB's `sum(BIGINT)` is HUGEINT — exported as
        // `Decimal128(38, 0)`. Nothing forbids declaring such a measure,
        // and without an arm for it every cell read blank. Under §6.3 a
        // blank cell is a positive claim about the data, so an unreadable
        // type silently became "this number does not belong to this row".
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
        // `ColumnType` accepts date/timestamp/bool and DuckDB emits
        // Date32/Timestamp/Boolean, none of which any typed accessor
        // matched — so a legal declaration produced a column of blank
        // cells, which §6.3 reads as a claim about the data.
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
        // rest of the snapshot's life (§5.5). Every typed accessor here
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

    #[test]
    fn depth_is_read_through_the_cached_column() {
        let s = snapshot();
        assert_eq!(s.depth_of_row(0), Some(1));
        assert_eq!(s.depth_of_row(1), Some(0));
        let flat = Snapshot::for_tests(vec![(dim("delta01"), TestColumn::F64(vec![Some(1.0)]))], 0);
        assert_eq!(flat.depth_of_row(0), None, "no depth column, no depth");
    }
}
