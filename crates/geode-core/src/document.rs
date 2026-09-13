//! The rows a parsed document becomes (market-data spec §6.2). Lives in
//! `geode-core` because both the data crate (which publishes them) and
//! the demo generator (which produces them) need the type, and the data
//! crate dev-depends on the generator — a trait or type in either would
//! be a cycle. Struct-of-arrays, per PHILOSOPHY §6: nothing here is a row.

use crate::schema::{ColumnRole, ColumnType, DatasetSpec};
use chrono::NaiveDate;

/// Joins the parts of a multi-column document key into the one string the
/// store's `batch` column holds. ASCII unit separator: no dimension value
/// may contain it (`DocumentRows::validate` refuses one that does), so the
/// join is unambiguous and `split_key` is its exact inverse.
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

/// One parsed document, struct-of-arrays (market-data spec §6.2).
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

    /// Every check `publish_document` needs before it touches the store:
    /// key arity, axis names/order/types, value names/types, attribute
    /// names/types against `document_columns()`, and equal lengths.
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
        for (i, ((name, col), declared)) in self.axes.iter().zip(&ds.axes).enumerate() {
            if name != declared {
                return Err(format!(
                    "axis {i} is '{name}', dataset declares '{declared}'"
                ));
            }
            let spec = ds.column(declared).expect("validated by the schema");
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
}
