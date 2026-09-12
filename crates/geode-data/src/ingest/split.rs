//! The grain split (spec §3.2) and conflict detection (spec §3.5).
//!
//! The source file is flat at the atomic grain, so a measure belonging to a
//! coarser grain is repeated across a row group and any `SUM` over it
//! double-counts. Splitting by grain makes that impossible by construction
//! rather than by discipline.
//!
//! Everything here is SQL executed inside DuckDB: no rows cross into Rust.
//! Deduplicating a million rows through a Rust HashMap would violate both
//! the allocation discipline (PHILOSOPHY §6) and the ingest-invisibility
//! budget (spec §7.1).

use crate::store::StoreError;
use crate::store::catalog::FileId;
use duckdb::Connection;
use geode_core::schema::{ColumnRole, DatasetSpec, Grain};

pub struct SplitRequest<'a> {
    pub dataset: &'a DatasetSpec,
    /// The table `read_csv` landed in, already column-mapped.
    pub raw_table: &'a str,
    pub batch: &'a str,
    pub file_id: FileId,
}

/// A column whose repeated values disagreed within one grain group. Either
/// upstream is inconsistent or the column's declared grain is wrong — which
/// is exactly the signal spec §3.5 wants surfaced rather than averaged away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub grain: Grain,
    pub column: String,
    /// How many grain groups disagreed.
    pub groups: usize,
}

#[derive(Debug, Default)]
pub struct SplitResult {
    /// `(grain, staging table name)` for each grain the dataset declares.
    pub staged: Vec<(Grain, String)>,
    pub conflicts: Vec<Conflict>,
}

pub fn staging_table(grain: Grain) -> String {
    format!("staging_{}", grain.table())
}

fn sql_err(statement: &str) -> impl FnOnce(duckdb::Error) -> StoreError + '_ {
    move |source| StoreError::Sql {
        statement: statement.to_string(),
        source,
    }
}

/// Key expressions for a grain. The pair grain canonicalizes its two
/// underlyings so both source orderings collapse to one row (spec §3.3).
fn key_exprs(grain: Grain) -> Vec<(String, String)> {
    grain
        .key_columns()
        .iter()
        .map(|&col| match (grain, col) {
            (Grain::UnderlyingPair, "underlying_ref") => (
                col.to_string(),
                "least(underlying_ref, underlying2_ref)".to_string(),
            ),
            (Grain::UnderlyingPair, "underlying2_ref") => (
                col.to_string(),
                "greatest(underlying_ref, underlying2_ref)".to_string(),
            ),
            _ => (col.to_string(), format!("\"{col}\"")),
        })
        .collect()
}

/// Measures and attributes declared at this grain, plus the carried
/// dimensions it carries (spec §3.3). A carried dimension goes through
/// `any_value` like an attribute and — because the dependency on the
/// key is a claim the schema makes — through the conflict check below
/// like one too.
fn payload_columns(ds: &DatasetSpec, grain: Grain) -> Vec<&str> {
    let mut out: Vec<&str> = ds
        .columns
        .iter()
        .filter(|c| match c.role {
            ColumnRole::Measure { grain: g, .. } => g == grain,
            ColumnRole::Attribute { grain: Some(g) } => g == grain,
            _ => false,
        })
        .map(|c| c.name.as_str())
        .collect();
    out.extend(
        ds.carried_dimensions_at(grain)
            .into_iter()
            .map(|c| c.name.as_str()),
    );
    out
}

pub fn split_by_grain(conn: &Connection, req: &SplitRequest) -> Result<SplitResult, StoreError> {
    let mut out = SplitResult::default();

    for grain in req.dataset.grains() {
        let keys = key_exprs(grain);
        let payload = payload_columns(req.dataset, grain);
        let table = staging_table(grain);

        let key_select: Vec<String> = keys
            .iter()
            .map(|(name, expr)| format!("{expr} as \"{name}\""))
            .collect();
        let key_group: Vec<String> = keys.iter().map(|(_, expr)| expr.clone()).collect();
        // any_value is correct because the repeated values are identical;
        // where they are not, the conflict query below says so.
        let payload_select: Vec<String> = payload
            .iter()
            .map(|c| format!("any_value(\"{c}\") as \"{c}\""))
            .collect();

        let mut projection = key_select.clone();
        projection.extend(payload_select);

        let sql = format!(
            "create or replace table {table} as
             select {projection}, '{batch}' as batch, {file_id} as source_file_id
             from {raw}
             group by {group}",
            projection = projection.join(", "),
            batch = req.batch.replace('\'', "''"),
            file_id = req.file_id,
            raw = req.raw_table,
            group = key_group.join(", "),
        );
        conn.execute_batch(&sql).map_err(sql_err(&sql))?;
        out.staged.push((grain, table));

        // Conflict detection: within each grain group, did a repeated value
        // disagree with itself? One query per grain, all columns at once.
        if payload.is_empty() {
            continue;
        }
        let inner: Vec<String> = payload
            .iter()
            .map(|c| format!("min(\"{c}\") as \"{c}_lo\", max(\"{c}\") as \"{c}_hi\""))
            .collect();
        let outer: Vec<String> = payload
            .iter()
            .map(|c| {
                format!("count(*) filter (where \"{c}_lo\" is distinct from \"{c}_hi\") as \"{c}\"")
            })
            .collect();
        let sql = format!(
            "select {outer} from (select {keys}, {inner} from {raw} group by {group})",
            outer = outer.join(", "),
            keys = key_select.join(", "),
            inner = inner.join(", "),
            raw = req.raw_table,
            group = key_group.join(", "),
        );
        let mut stmt = conn.prepare(&sql).map_err(sql_err(&sql))?;
        let mut rows = stmt.query([]).map_err(sql_err(&sql))?;
        if let Some(row) = rows.next().map_err(sql_err(&sql))? {
            for (i, column) in payload.iter().enumerate() {
                let groups: i64 = row.get(i).unwrap_or(0);
                if groups > 0 {
                    out.conflicts.push(Conflict {
                        grain,
                        column: (*column).to_string(),
                        groups: groups as usize,
                    });
                }
            }
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use geode_core::schema::Grain;

    /// A raw staging table shaped like one instrument over three
    /// underlyings: six ordered pair rows, coarse measures repeated,
    /// both pair orderings present.
    fn fixture() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        store
            .writer()
            .execute_batch(
                "create table staging_raw(
                     book varchar, lhu varchar, position_ref varchar,
                     counterparty varchar, instrument_ref varchar,
                     underlying_ref varchar, underlying2_ref varchar,
                     delta01 double, cross_gamma02 double,
                     npv double, daily_trading_pnl double, model_code varchar,
                     currency varchar);
                 insert into staging_raw values
                   ('BK0','L0','P1','C','P1a','NDX','RUT', 10, 1, 100, 7, 'EURP', 'USD'),
                   ('BK0','L0','P1','C','P1a','NDX','SPX', 10, 2, 100, 7, 'EURP', 'USD'),
                   ('BK0','L0','P1','C','P1a','RUT','NDX', 20, 1, 100, 7, 'EURP', 'USD'),
                   ('BK0','L0','P1','C','P1a','RUT','SPX', 20, 3, 100, 7, 'EURP', 'USD'),
                   ('BK0','L0','P1','C','P1a','SPX','NDX', 30, 2, 100, 7, 'EURP', 'USD'),
                   ('BK0','L0','P1','C','P1a','SPX','RUT', 30, 3, 100, 7, 'EURP', 'USD');",
            )
            .unwrap();
        (dir, store)
    }

    fn dataset() -> geode_core::schema::DatasetSpec {
        use geode_core::config::{LayerDoc, merge_docs};
        let text = r#"
[risk.columns.book]
type = "utf8"
role = "dimension"
[risk.columns.lhu]
type = "utf8"
role = "dimension"
[risk.columns.position_ref]
type = "utf8"
role = "key"
[risk.columns.counterparty]
type = "utf8"
role = "dimension"
[risk.columns.instrument_ref]
type = "utf8"
role = "key"
[risk.columns.underlying_ref]
type = "utf8"
role = "dimension"
[risk.columns.underlying2_ref]
type = "utf8"
role = "dimension"
[risk.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk.columns.cross_gamma02]
type = "f64"
role = "measure"
grain = "underlying_pair"
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
[risk.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"
[risk.columns.model_code]
type = "utf8"
role = "attribute"
grain = "instrument"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        geode_core::schema::SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk")
            .unwrap()
            .clone()
    }

    fn req(ds: &geode_core::schema::DatasetSpec) -> SplitRequest<'_> {
        SplitRequest {
            dataset: ds,
            raw_table: "staging_raw",
            batch: "BK0",
            file_id: 1,
        }
    }

    fn count(store: &Store, table: &str) -> i64 {
        store
            .writer()
            .query_row(&format!("select count(*) from {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn sum(store: &Store, table: &str, col: &str) -> f64 {
        store
            .writer()
            .query_row(&format!("select sum({col}) from {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn each_grain_collapses_to_its_own_key_cardinality() {
        let (_d, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();

        assert_eq!(
            count(&store, "staging_measures_position"),
            1,
            "one position"
        );
        assert_eq!(
            count(&store, "staging_measures_instrument"),
            1,
            "one instrument"
        );
        assert_eq!(
            count(&store, "staging_measures_underlying"),
            3,
            "three underlyings"
        );
        assert_eq!(
            count(&store, "staging_measures_underlying_pair"),
            3,
            "three canonical pairs, not six ordered ones"
        );
    }

    #[test]
    fn coarse_measures_stop_double_counting() {
        let (_d, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();

        // In the raw file these sum to 6x and 6x their true value.
        assert_eq!(sum(&store, "staging_raw", "daily_trading_pnl"), 42.0);
        assert_eq!(
            sum(&store, "staging_measures_position", "daily_trading_pnl"),
            7.0
        );
        assert_eq!(sum(&store, "staging_raw", "npv"), 600.0);
        assert_eq!(sum(&store, "staging_measures_instrument", "npv"), 100.0);
        // Underlying greeks are repeated twice each (once per partner).
        assert_eq!(sum(&store, "staging_raw", "delta01"), 120.0);
        assert_eq!(sum(&store, "staging_measures_underlying", "delta01"), 60.0);
    }

    #[test]
    fn cross_gamma_is_canonicalized_so_a_sum_counts_each_pair_once() {
        let (_d, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();
        assert_eq!(sum(&store, "staging_raw", "cross_gamma02"), 12.0);
        assert_eq!(
            sum(&store, "staging_measures_underlying_pair", "cross_gamma02"),
            6.0
        );

        // Canonical ordering: underlying_ref <= underlying2_ref on every row.
        let bad: i64 = store
            .writer()
            .query_row(
                "select count(*) from staging_measures_underlying_pair
                 where underlying_ref > underlying2_ref",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bad, 0);
    }

    #[test]
    fn attributes_travel_with_their_grain() {
        let (_d, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();
        let code: String = store
            .writer()
            .query_row(
                "select model_code from staging_measures_instrument",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(code, "EURP");
    }

    #[test]
    fn batch_and_file_id_are_carried_onto_every_grain() {
        let (_d, store) = fixture();
        let ds = dataset();
        let out = split_by_grain(store.writer(), &req(&ds)).unwrap();
        for (_, table) in &out.staged {
            let n: i64 = store
                .writer()
                .query_row(
                    &format!(
                        "select count(*) from {table} where batch = 'BK0' and source_file_id = 1"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(n > 0, "{table} lost its partition columns");
        }
    }

    #[test]
    fn disagreeing_repeated_values_are_reported_as_conflicts() {
        let (_d, store) = fixture();
        // Same instrument, different NPV on one row: either upstream
        // disagrees or the grain assignment is wrong (spec §3.5).
        store
            .writer()
            .execute_batch("update staging_raw set npv = 999 where underlying_ref = 'SPX'")
            .unwrap();
        let ds = dataset();
        let out = split_by_grain(store.writer(), &req(&ds)).unwrap();

        let npv = out
            .conflicts
            .iter()
            .find(|c| c.column == "npv")
            .expect("npv conflict");
        assert_eq!(npv.grain, Grain::Instrument);
        assert_eq!(npv.groups, 1);
        assert!(
            !out.conflicts
                .iter()
                .any(|c| c.column == "daily_trading_pnl"),
            "agreeing columns must not be reported"
        );
    }

    #[test]
    fn clean_input_reports_no_conflicts() {
        let (_d, store) = fixture();
        let ds = dataset();
        let out = split_by_grain(store.writer(), &req(&ds)).unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    }

    #[test]
    fn a_carried_dimension_lands_in_its_grain_and_every_finer_one_but_not_position() {
        let (_dir, store) = fixture();
        let ds = dataset();
        split_by_grain(store.writer(), &req(&ds)).unwrap();
        let has = |table: &str| -> bool {
            store
                .writer()
                .query_row(
                    &format!(
                        "select count(*) from (describe {table}) where column_name = 'currency'"
                    ),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 1
        };
        assert!(!has(&staging_table(Grain::Position)));
        assert!(has(&staging_table(Grain::Instrument)));
        assert!(has(&staging_table(Grain::Underlying)));
        // and the value is the one the instrument carries
        let n: i64 = store
            .writer()
            .query_row(
                &format!(
                    "select count(*) from {} where currency is null",
                    staging_table(Grain::Instrument)
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_carried_dimension_that_varies_within_its_key_is_a_conflict() {
        let (_dir, store) = fixture();
        let ds = dataset();
        // Break the dependency for one row in the raw table: five of the
        // six rows share instrument P1a and agree on currency, one does
        // not, so the instrument-grain group disagrees with itself.
        store
            .writer()
            .execute_batch(
                "update staging_raw set currency = 'JPY'
                 where rowid = (select min(rowid) from staging_raw)",
            )
            .unwrap();
        let out = split_by_grain(store.writer(), &req(&ds)).unwrap();
        assert!(
            out.conflicts
                .iter()
                .any(|c| c.column == "currency" && c.grain == Grain::Instrument && c.groups == 1),
            "{:?}",
            out.conflicts
        );
    }
}
