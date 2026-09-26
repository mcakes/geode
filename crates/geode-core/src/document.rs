//! Shared columnar document values, parser/writer traits, and schema checks.
//! The data service, document kinds, and demo generators use these types without
//! depending on each other. Columns hold row data; document keys and attributes
//! hold one value for the whole document.

use crate::schema::{ColumnRole, ColumnType, DatasetSpec};
use chrono::NaiveDate;

/// Separator between parts of a stored document key. Joining and splitting
/// preserves a nonempty key whose parts exclude this character. Validation
/// checks the separator and key arity; loaded schemas require a nonempty key.
pub const KEY_SEPARATOR: char = '\u{1f}';

pub fn join_key(parts: &[String]) -> String {
    parts.join(&KEY_SEPARATOR.to_string())
}

pub fn split_key(batch: &str) -> Vec<String> {
    batch.split(KEY_SEPARATOR).map(str::to_string).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Column {
    F64(Vec<f64>),
    I64(Vec<i64>),
    Utf8(Vec<String>),
    Date(Vec<NaiveDate>),
}

impl Column {
    pub fn len(&self) -> usize {
        match self {
            Column::F64(v) => v.len(),
            Column::I64(v) => v.len(),
            Column::Utf8(v) => v.len(),
            Column::Date(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn column_type(&self) -> ColumnType {
        match self {
            Column::F64(_) => ColumnType::F64,
            Column::I64(_) => ColumnType::I64,
            Column::Utf8(_) => ColumnType::Utf8,
            Column::Date(_) => ColumnType::Date,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    F64(f64),
    I64(i64),
    Utf8(String),
    Date(NaiveDate),
}

impl Value {
    pub fn column_type(&self) -> ColumnType {
        match self {
            Value::F64(_) => ColumnType::F64,
            Value::I64(_) => ColumnType::I64,
            Value::Utf8(_) => ColumnType::Utf8,
            Value::Date(_) => ColumnType::Date,
        }
    }
}

/// One parsed document with columnar row data and document-level values.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentRows {
    pub key: Vec<String>,                 // in the dataset's `key` order
    pub attributes: Vec<(String, Value)>, // document-level, one value each
    pub axes: Vec<(String, Column)>,      // one per axis, all the same length
    pub values: Vec<(String, Column)>,    // one per value column, same length
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

impl DocumentRows {
    pub fn rows(&self) -> usize {
        self.axes.first().map_or(0, |(_, c)| c.len())
    }

    /// Validate the document family, key arity and reserved separator, nonempty
    /// row count, axis order/types/uniqueness, value and attribute names/types,
    /// required columns, and equal column lengths before publication.
    ///
    /// Axes are matched positionally against `ds.axes`; values and attributes are
    /// matched by name. These checks do not reject duplicate value or attribute
    /// names, and do not enforce kind-specific rules such as finite numbers.
    pub fn validate(&self, ds: &DatasetSpec) -> Result<(), String> {
        if !ds.is_document() {
            return Err(format!("dataset '{}' is not a document dataset", ds.name));
        }
        if self.key.len() != ds.key.len() {
            return Err(format!(
                "key has {} parts, dataset declares {}",
                self.key.len(),
                ds.key.len()
            ));
        }
        if let Some(part) = self.key.iter().find(|p| p.contains(KEY_SEPARATOR)) {
            return Err(format!("key part {part:?} contains the reserved separator"));
        }
        if self.axes.len() != ds.axes.len() {
            return Err(format!(
                "document has {} axes, dataset declares {}",
                self.axes.len(),
                ds.axes.len()
            ));
        }
        // An empty generation would remove live rows while leaving no payload row
        // that identifies the new generation. Readers could not distinguish it from
        // a missing document, and retention would discard its catalog entry.
        // Document schemas require an axis, so `rows()` reads that axis's length.
        if self.rows() == 0 {
            return Err("document has no rows".to_string());
        }
        for (i, ((name, col), declared)) in self.axes.iter().zip(&ds.axes).enumerate() {
            if name != declared {
                return Err(format!(
                    "axis {i} is '{name}', dataset declares '{declared}'"
                ));
            }
            // Hand-built schemas can name an undeclared axis even though the schema
            // reader rejects that shape. Return a column-specific error instead of
            // panicking on the ingest thread.
            let Some(spec) = ds.column(declared) else {
                return Err(format!("axis '{name}' is not a declared column"));
            };
            if col.column_type() != spec.ty {
                return Err(format!(
                    "axis '{name}' is {}, dataset declares {}",
                    type_name(col.column_type()),
                    type_name(spec.ty)
                ));
            }
        }
        let rows = self.rows();
        for (name, col) in &self.axes {
            if col.len() != rows {
                return Err(format!(
                    "axis '{name}' has {} rows, first axis has {rows}",
                    col.len()
                ));
            }
        }
        // The panel addresses cells by axis tuple, while storage has no uniqueness
        // constraint over those axes. Reject duplicates before they can publish as
        // ambiguous or double-counted cells.
        //
        // Sorting row indices uses one buffer and O(n log n) comparisons without
        // allocating a tuple per row. The preceding length check makes indexing safe.
        if let Some((a, b)) = self.first_duplicate_rows() {
            let tuple = self
                .axes
                .iter()
                .map(|(name, col)| format!("{name}={}", cell_text(col, a)))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "axis tuple ({tuple}) appears twice (rows {a} and {b})"
            ));
        }
        for (name, col) in &self.values {
            let Some(spec) = ds.column(name).filter(|c| c.role == ColumnRole::Value) else {
                return Err(format!("value '{name}' is not declared"));
            };
            if col.column_type() != spec.ty {
                return Err(format!(
                    "value '{name}' is {}, dataset declares {}",
                    type_name(col.column_type()),
                    type_name(spec.ty)
                ));
            }
            if col.len() != rows {
                return Err(format!(
                    "value '{name}' has {} rows, axes have {rows}",
                    col.len()
                ));
            }
        }
        for spec in ds.columns.iter().filter(|c| c.role == ColumnRole::Value) {
            if !self.values.iter().any(|(n, _)| n == &spec.name) {
                return Err(format!("value '{}' is missing", spec.name));
            }
        }
        for spec in ds
            .columns
            .iter()
            .filter(|c| matches!(c.role, ColumnRole::Attribute { grain: None }))
        {
            let Some((_, v)) = self.attributes.iter().find(|(n, _)| n == &spec.name) else {
                return Err(format!("attribute '{}' is missing", spec.name));
            };
            if v.column_type() != spec.ty {
                return Err(format!(
                    "attribute '{}' is {}, dataset declares {}",
                    spec.name,
                    type_name(v.column_type()),
                    type_name(spec.ty)
                ));
            }
        }
        for (name, _) in &self.attributes {
            if !ds
                .columns
                .iter()
                .any(|c| &c.name == name && matches!(c.role, ColumnRole::Attribute { grain: None }))
            {
                return Err(format!("attribute '{name}' is not declared"));
            }
        }
        Ok(())
    }

    /// The first pair of rows sharing an axis tuple, as `(earlier,
    /// later)` row indices, or `None` when every tuple is distinct.
    ///
    /// "First" is by the EARLIER row of the pair: the indices are sorted
    /// by axis value with the row index as the final tie-break, so the
    /// duplicate reported is stable for a given document and reads as the
    /// first row a human scanning the grid would notice twice.
    ///
    /// Callers may index `self.axes` freely with the answer: it only ever
    /// names rows that exist.
    fn first_duplicate_rows(&self) -> Option<(usize, usize)> {
        let rows = self.rows();
        if rows < 2 || self.axes.is_empty() {
            return None;
        }
        let mut order: Vec<usize> = (0..rows).collect();
        order.sort_unstable_by(|&a, &b| {
            self.axes
                .iter()
                .map(|(_, col)| cmp_cell(col, a, b))
                .find(|o| o.is_ne())
                .unwrap_or_else(|| a.cmp(&b))
        });
        let mut found: Option<(usize, usize)> = None;
        for pair in order.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if self.axes.iter().all(|(_, col)| cmp_cell(col, a, b).is_eq()) {
                let pair = (a.min(b), a.max(b));
                if found.is_none_or(|(first, _)| pair.0 < first) {
                    found = Some(pair);
                }
            }
        }
        found
    }
}

/// One cell of an axis or value column, as a diagnostic spells it.
/// Only ever called on an error path, so the `String` per cell costs
/// nothing on the hot one.
fn cell_text(col: &Column, i: usize) -> String {
    match col {
        Column::F64(v) => v[i].to_string(),
        Column::I64(v) => v[i].to_string(),
        Column::Utf8(v) => v[i].clone(),
        Column::Date(v) => v[i].to_string(),
    }
}

/// Orders two rows of one column. `total_cmp` for `f64` because it is a
/// total order over every bit pattern including NaN — an axis carrying
/// one is a document `CviKind::parse` already refuses, and a partial
/// comparison here would make the sort's own ordering undefined rather
/// than merely report the NaN late.
fn cmp_cell(col: &Column, a: usize, b: usize) -> std::cmp::Ordering {
    match col {
        Column::F64(v) => v[a].total_cmp(&v[b]),
        Column::I64(v) => v[a].cmp(&v[b]),
        Column::Utf8(v) => v[a].cmp(&v[b]),
        Column::Date(v) => v[a].cmp(&v[b]),
    }
}

/// Parsed rows and paths of unrecognized elements. The subscription receiver
/// deduplicates these paths per source before logging, so repeated extensions
/// do not flood the log.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDocument {
    pub rows: DocumentRows,
    pub unknown_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteError {
    pub message: String,
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// A document format's parser, writer, and column vocabulary. The data service
/// registers this trait without depending on a concrete parser crate. Source
/// startup checks the kind's columns against its configured dataset; individual
/// parsed documents still require shape validation before publication.
pub trait DocumentKind: Send + Sync {
    fn name(&self) -> &'static str;
    /// Columns produced by this kind: keys, axes, values, and attributes.
    /// Source startup checks their names and types against the dataset.
    fn columns(&self) -> &[(&'static str, ColumnType)];
    fn parse(&self, bytes: &[u8]) -> Result<ParsedDocument, ParseError>;
    fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, WriteError>;
}

/// Check that a document dataset and kind declare the same column names and
/// types in both directions. Source startup reports a mismatch as a configuration
/// diagnostic. This check does not compare column order or validate individual
/// rows; publication uses the dataset's order and validates each document.
pub fn check_kind_against(kind: &dyn DocumentKind, ds: &DatasetSpec) -> Result<(), String> {
    if !ds.is_document() {
        return Err(format!("dataset '{}' is not a document dataset", ds.name));
    }
    let declared = ds.document_columns();
    for (name, _) in kind.columns() {
        if !declared.iter().any(|c| &c.name == name) {
            return Err(format!(
                "kind produces '{name}', which dataset '{}' does not declare",
                ds.name
            ));
        }
    }
    for spec in &declared {
        if !kind.columns().iter().any(|(name, _)| name == &spec.name) {
            return Err(format!(
                "dataset '{}' declares '{}', which kind '{}' does not produce",
                ds.name,
                spec.name,
                kind.name()
            ));
        }
    }
    for (name, ty) in kind.columns() {
        if let Some(spec) = declared.iter().find(|c| &c.name == name)
            && *ty != spec.ty
        {
            return Err(format!(
                "'{name}' is {} in the kind, {} in the dataset",
                type_name(*ty),
                type_name(spec.ty)
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerDoc, merge_docs};
    use crate::schema::SchemaSpec;
    use chrono::NaiveDate;

    const CVI: &str = r#"
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
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
"#;

    fn cvi() -> crate::schema::DatasetSpec {
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", CVI).unwrap()]);
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema.dataset("cvi_params").unwrap().clone()
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// Two terms × three nodes, term-major.
    pub(crate) fn sample() -> DocumentRows {
        DocumentRows {
            key: vec!["SPX.Z".into()],
            attributes: vec![
                ("anchor_date".into(), Value::Date(d("2026-09-12"))),
                ("spot_ref".into(), Value::F64(7650.0)),
            ],
            axes: vec![
                (
                    "term".into(),
                    Column::Date(
                        vec![d("2026-09-18"); 3]
                            .into_iter()
                            .chain(vec![d("2026-10-16"); 3])
                            .collect(),
                    ),
                ),
                (
                    "node".into(),
                    Column::F64(vec![-20.0, -1.0, 3.5, -20.0, -1.0, 3.5]),
                ),
            ],
            values: vec![(
                "param".into(),
                Column::F64(vec![-0.34, 0.1, 1.3, -0.3, 0.12, 1.25]),
            )],
        }
    }

    #[test]
    fn join_and_split_round_trip_and_a_single_part_needs_no_separator() {
        let parts = vec!["SPX.Z".to_string(), "NDX.Z".to_string()];
        let joined = join_key(&parts);
        assert!(joined.contains(KEY_SEPARATOR));
        assert_eq!(split_key(&joined), parts);
        assert_eq!(join_key(&["SPX.Z".to_string()]), "SPX.Z");
        assert_eq!(split_key("SPX.Z"), vec!["SPX.Z".to_string()]);
    }

    #[test]
    fn a_well_formed_document_validates_against_its_dataset() {
        assert_eq!(sample().validate(&cvi()), Ok(()));
        assert_eq!(sample().rows(), 6);
    }

    #[test]
    fn a_repeated_axis_tuple_is_refused_and_the_message_names_it() {
        // Two rows carrying the same (term, node) pair. Nothing
        // downstream can tell them apart — the panel indexes its grid by
        // the axis tuple, and the store has no uniqueness constraint — so
        // a document that merged two copies of an element (a feed sending
        // `<cviParams>` twice, say) would publish a healthy-looking
        // generation with one cell silently doubled.
        let mut r = sample();
        let Column::F64(nodes) = &mut r.axes[1].1 else {
            panic!("node is the f64 axis")
        };
        nodes[2] = nodes[1];
        let e = r.validate(&cvi()).expect_err("a repeated axis tuple");
        assert!(
            e.contains("term=2026-09-18") && e.contains("node=-1"),
            "the message names the duplicated tuple: {e}"
        );
        assert!(e.contains("twice"), "{e}");
    }

    #[test]
    fn validate_refuses_each_mismatch_with_a_message_naming_it() {
        let ds = cvi();
        let mut r = sample();
        r.key.push("extra".into());
        assert!(
            r.validate(&ds)
                .unwrap_err()
                .contains("key has 2 parts, dataset declares 1")
        );

        let mut r = sample();
        r.axes.swap(0, 1);
        assert!(
            r.validate(&ds)
                .unwrap_err()
                .contains("axis 0 is 'node', dataset declares 'term'")
        );

        let mut r = sample();
        r.axes[1].1 = Column::Utf8(vec!["x".into(); 6]);
        assert!(
            r.validate(&ds)
                .unwrap_err()
                .contains("axis 'node' is utf8, dataset declares f64")
        );

        let mut r = sample();
        r.values[0].1 = Column::F64(vec![1.0; 5]);
        assert!(
            r.validate(&ds)
                .unwrap_err()
                .contains("value 'param' has 5 rows, axes have 6")
        );

        let mut r = sample();
        r.values[0].0 = "nonesuch".into();
        assert!(
            r.validate(&ds)
                .unwrap_err()
                .contains("value 'nonesuch' is not declared")
        );

        let mut r = sample();
        r.attributes.retain(|(n, _)| n != "spot_ref");
        assert!(
            r.validate(&ds)
                .unwrap_err()
                .contains("attribute 'spot_ref' is missing")
        );

        let mut r = sample();
        r.attributes[1].1 = Value::Utf8("7650".into());
        assert!(
            r.validate(&ds)
                .unwrap_err()
                .contains("attribute 'spot_ref' is utf8, dataset declares f64")
        );

        let mut r = sample();
        r.key[0] = format!("SPX{KEY_SEPARATOR}Z");
        assert!(r.validate(&ds).unwrap_err().contains("reserved separator"));
    }

    /// An empty document is refused at the source, so nothing downstream
    /// has to decide what an empty generation means: publishing one would
    /// delete the batch's live rows, insert none, and record a generation
    /// no table holds.
    #[test]
    fn validate_refuses_a_document_with_no_rows() {
        let mut r = sample();
        for (_, col) in r.axes.iter_mut().chain(r.values.iter_mut()) {
            *col = match col {
                Column::F64(_) => Column::F64(Vec::new()),
                Column::I64(_) => Column::I64(Vec::new()),
                Column::Utf8(_) => Column::Utf8(Vec::new()),
                Column::Date(_) => Column::Date(Vec::new()),
            };
        }
        assert_eq!(r.rows(), 0);
        let err = r.validate(&cvi()).unwrap_err();
        assert!(err.contains("document has no rows"), "{err}");
    }

    /// Ledger (ii): the undeclared-attribute branch — the mirror of
    /// "attribute 'x' is missing". A document carrying an attribute the
    /// dataset never declared has nowhere to put it: `document_columns()`
    /// would omit it, so the value would be dropped silently rather than
    /// stored.
    #[test]
    fn validate_refuses_an_attribute_the_dataset_does_not_declare() {
        let mut r = sample();
        r.attributes.push(("nonesuch".into(), Value::F64(1.0)));
        let err = r.validate(&cvi()).unwrap_err();
        assert!(
            err.contains("attribute 'nonesuch' is not declared"),
            "{err}"
        );
    }

    /// An undeclared axis in a hand-built schema returns a diagnostic instead of
    /// panicking. Schema loading normally rejects this shape before publication.
    #[test]
    fn validate_refuses_an_axis_the_dataset_does_not_declare_as_a_column() {
        let mut ds = cvi();
        ds.columns.retain(|c| c.name != "term");
        let err = sample().validate(&ds).unwrap_err();
        assert!(
            err.contains("axis 'term' is not a declared column"),
            "{err}"
        );
    }

    #[test]
    fn validate_refuses_a_measure_dataset() {
        let mut ds = cvi();
        ds.family = crate::schema::Family::Measures;
        assert!(
            sample()
                .validate(&ds)
                .unwrap_err()
                .contains("not a document dataset")
        );
    }

    /// The CVI fixture's six columns, in `document_columns()` order
    /// (key, axes, values, attributes) — the vocabulary `FakeKind`
    /// claims to produce by default, matching `cvi()` exactly.
    fn fake_columns() -> Vec<(&'static str, ColumnType)> {
        vec![
            ("underlying_ref", ColumnType::Utf8),
            ("term", ColumnType::Date),
            ("node", ColumnType::F64),
            ("param", ColumnType::F64),
            ("anchor_date", ColumnType::Date),
            ("spot_ref", ColumnType::F64),
        ]
    }

    fn with_extra(extra: (&'static str, ColumnType)) -> Vec<(&'static str, ColumnType)> {
        let mut v = fake_columns();
        v.push(extra);
        v
    }

    fn without(name: &str) -> Vec<(&'static str, ColumnType)> {
        fake_columns()
            .into_iter()
            .filter(|(n, _)| *n != name)
            .collect()
    }

    fn retyped(name: &str, ty: ColumnType) -> Vec<(&'static str, ColumnType)> {
        fake_columns()
            .into_iter()
            .map(|(n, t)| if n == name { (n, ty) } else { (n, t) })
            .collect()
    }

    struct FakeKind {
        columns: Vec<(&'static str, ColumnType)>,
    }

    impl Default for FakeKind {
        fn default() -> Self {
            FakeKind {
                columns: fake_columns(),
            }
        }
    }

    impl DocumentKind for FakeKind {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn columns(&self) -> &[(&'static str, ColumnType)] {
            &self.columns
        }
        fn parse(&self, _bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
            Ok(ParsedDocument {
                rows: sample(),
                unknown_paths: Vec::new(),
            })
        }
        fn write(&self, _rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
            Ok(b"fake".to_vec())
        }
    }

    #[test]
    fn check_kind_against_accepts_a_matching_dataset_and_names_each_mismatch() {
        let ds = cvi();
        assert_eq!(check_kind_against(&FakeKind::default(), &ds), Ok(()));
        // The kind produces a column the dataset lacks.
        let extra = FakeKind {
            columns: with_extra(("vol", ColumnType::F64)),
        };
        assert!(
            check_kind_against(&extra, &ds)
                .unwrap_err()
                .contains("kind produces 'vol', which dataset 'cvi_params' does not declare")
        );
        // The dataset declares a column the kind does not produce.
        let missing = FakeKind {
            columns: without("spot_ref"),
        };
        assert!(check_kind_against(&missing, &ds).unwrap_err().contains(
            "dataset 'cvi_params' declares 'spot_ref', which kind 'fake' does not produce"
        ));
        // Same name, different type.
        let wrong = FakeKind {
            columns: retyped("node", ColumnType::I64),
        };
        assert!(
            check_kind_against(&wrong, &ds)
                .unwrap_err()
                .contains("'node' is i64 in the kind, f64 in the dataset")
        );
        // A measure dataset is refused outright.
        let mut m = ds.clone();
        m.family = crate::schema::Family::Measures;
        assert!(
            check_kind_against(&FakeKind::default(), &m)
                .unwrap_err()
                .contains("not a document dataset")
        );
    }
}
