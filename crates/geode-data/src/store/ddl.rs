//! Table DDL generated from the declared schema (spec §4.2). One live and
//! one archive table per grain present in the dataset.
//!
//! Live carries exactly the current rows for every file partition: no
//! generation column, no history predicate, size independent of retention.
//! That is what keeps the requery budget reachable by construction, so the
//! absence of `gen_id` from live is load-bearing, not an oversight.
//!
//! A grain's table carries its key columns plus the measures and attributes
//! declared *at that grain*. A `Dimension` column outside every grain key
//! therefore appears in no table at all — so anything worth displaying that
//! is not itself a key (`business_date`, for one) must be declared as an
//! `attribute` at the grain that owns it, not as a bare dimension.

use geode_core::schema::{ColumnRole, DatasetSpec, Grain};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Live,
    Archive,
}

impl TableKind {
    pub fn suffix(self) -> &'static str {
        match self {
            TableKind::Live => "_live",
            TableKind::Archive => "_archive",
        }
    }
}

pub fn table_name(grain: Grain, kind: TableKind) -> String {
    format!("{}{}", grain.table(), kind.suffix())
}

pub fn create_table_sql(ds: &DatasetSpec, grain: Grain, kind: TableKind) -> String {
    let mut cols: Vec<String> = Vec::new();

    for key in grain.key_columns() {
        let ty = ds.column(key).map(|c| c.ty.sql()).unwrap_or("VARCHAR");
        cols.push(format!("  \"{key}\" {ty}"));
    }

    for c in ds.columns.iter() {
        let keep = match c.role {
            ColumnRole::Measure { grain: g, .. } | ColumnRole::Attribute { grain: g } => g == grain,
            ColumnRole::Key | ColumnRole::Dimension => false,
        };
        if keep {
            cols.push(format!("  \"{}\" {}", c.name, c.ty.sql()));
        }
    }

    // Partition key completion: `book` is already in the grain key, `slot`
    // is what replacement matches on, `source_file_id` is provenance only
    // (spec §4.3 — filenames carry dates, so file id is not partition id).
    cols.push("  \"slot\" VARCHAR".to_string());
    cols.push("  \"source_file_id\" BIGINT".to_string());
    if kind == TableKind::Archive {
        cols.push("  \"gen_id\" BIGINT".to_string());
        cols.push("  \"source_time\" TIMESTAMP WITH TIME ZONE".to_string());
    }

    format!(
        "CREATE TABLE IF NOT EXISTS {} (\n{}\n);",
        table_name(grain, kind),
        cols.join(",\n")
    )
}

#[cfg(test)]
pub(crate) mod tests_support {
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::schema::{DatasetSpec, SchemaSpec};

    pub(crate) fn sample_dataset() -> DatasetSpec {
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
[risk_snapshot.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk_snapshot.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk_snapshot.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk_snapshot")
            .unwrap()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::sample_dataset;
    use super::*;

    #[test]
    fn live_table_carries_the_grain_key_and_its_measures_only() {
        let sql = create_table_sql(&sample_dataset(), Grain::Position, TableKind::Live);
        assert!(sql.contains("measures_position_live"), "{sql}");
        assert!(sql.contains("\"book\" VARCHAR"), "{sql}");
        assert!(sql.contains("\"daily_trading_pnl\" DOUBLE"), "{sql}");
        // A finer grain's key column must not appear at position grain.
        assert!(!sql.contains("underlying_ref"), "{sql}");
        // Nor a measure declared at another grain.
        assert!(!sql.contains("delta01"), "{sql}");
        // Live carries no generation column (spec §4.2).
        assert!(!sql.contains("gen_id"), "{sql}");
    }

    #[test]
    fn live_carries_slot_for_replacement_and_file_id_for_provenance() {
        let sql = create_table_sql(&sample_dataset(), Grain::Underlying, TableKind::Live);
        // `slot` is what the publish transaction matches on: filenames carry
        // dates, so file identity is not partition identity (spec §4.3).
        assert!(sql.contains("\"slot\" VARCHAR"), "{sql}");
        assert!(sql.contains("\"source_file_id\" BIGINT"), "{sql}");
        // `book` is part of the grain key at every grain, completing the
        // partition key (dataset, slot, book).
        assert!(sql.contains("\"book\" VARCHAR"), "{sql}");
    }

    #[test]
    fn archive_adds_gen_id_and_source_time() {
        let sql = create_table_sql(&sample_dataset(), Grain::Underlying, TableKind::Archive);
        assert!(sql.contains("measures_underlying_archive"), "{sql}");
        assert!(sql.contains("\"gen_id\" BIGINT"), "{sql}");
        assert!(
            sql.contains("\"source_time\" TIMESTAMP WITH TIME ZONE"),
            "{sql}"
        );
    }
}
