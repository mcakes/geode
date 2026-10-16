//! Split flat source rows by declared grain and detect conflicting repeated
//! values. Coarse measures must contribute once per grain group rather than
//! once per atomic row. All grouping and conflict checks run in DuckDB,
//! avoiding per-row transfers and allocations in Rust.

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

/// A column whose repeated values disagree within a grain group. This can
/// indicate inconsistent upstream data or an incorrect declared grain.
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

/// Key expressions for a grain: its key columns, then the axes that extend
/// the key where the grain holds measures. The pair grain canonicalizes its
/// two underlyings so both source orderings collapse to one row.
fn key_exprs(ds: &DatasetSpec, grain: Grain) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = grain
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
        .collect();
    out.extend(
        ds.axes_at(grain)
            .iter()
            .map(|a| (a.clone(), format!("\"{a}\""))),
    );
    out
}

/// Measures and attributes declared at this grain, plus the carried
/// dimensions it carries. A carried dimension goes through
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
        let keys = key_exprs(req.dataset, grain);
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
    use geode_core::schema::{ColumnSpec, ColumnType, Grain};

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

    /// The document family's own roles, and a document-level attribute,
    /// must never be treated as payload at a measure grain — even though
    /// `parse_column` cannot itself produce this mix today (`Axis`/`Value`
    /// only come from a document dataset). The `_ => false` this replaces
    /// would flip to `true` unnoticed: no fixture in this file ever puts
    /// one of these roles on a measure dataset's columns.
    #[test]
    fn payload_columns_never_selects_a_document_shaped_column() {
        let mut ds = dataset();
        ds.columns.push(ColumnSpec {
            name: "term".into(),
            source_name: None,
            ty: ColumnType::Date,
            required: false,
            textual: false,
            categorical: false,
            order: geode_core::schema::TextOrder::Lexical,
            role: ColumnRole::document_axis(),
        });
        ds.columns.push(ColumnSpec {
            name: "param".into(),
            source_name: None,
            ty: ColumnType::F64,
            required: false,
            textual: false,
            categorical: false,
            order: geode_core::schema::TextOrder::Lexical,
            role: ColumnRole::Value,
        });
        ds.columns.push(ColumnSpec {
            name: "spot_ref".into(),
            source_name: None,
            ty: ColumnType::F64,
            required: false,
            textual: false,
            categorical: false,
            order: geode_core::schema::TextOrder::Lexical,
            role: ColumnRole::Attribute { grain: None },
        });
        let payload = payload_columns(&ds, Grain::Position);
        assert!(!payload.contains(&"term"), "{payload:?}");
        assert!(!payload.contains(&"param"), "{payload:?}");
        assert!(!payload.contains(&"spot_ref"), "{payload:?}");
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
        // disagrees or the grain assignment is wrong.
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

    #[test]
    fn the_split_groups_by_key_and_axes_and_dedups_attributes_once() {
        let ds = crate::store::ddl::tests_support::axis_dataset();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "create table raw as select * from (values
               ('BK0','L0','P1','C','I1','SPX','2026-08-24','USD','1M', 100.0, 10.0),
               ('BK0','L0','P1','C','I1','SPX','2026-08-24','USD','1M', 105.0, 20.0),
               ('BK0','L0','P1','C','I1','SPX','2026-08-24','USD','3M', 100.0, 30.0),
               ('BK0','L0','P1','C','I1','RUT','2026-08-24','USD','1M', 100.0, 40.0))
             t(book, lhu, position_ref, counterparty, instrument_ref, underlying_ref,
               business_date, currency, tenor, moneyness, vega)",
        )
        .unwrap();
        let out = split_by_grain(
            &conn,
            &SplitRequest {
                dataset: &ds,
                raw_table: "raw",
                batch: "b",
                file_id: 1,
            },
        )
        .unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        let under: i64 = conn
            .query_row(
                "select count(*) from staging_measures_underlying",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(under, 4, "one row per (key, tenor, moneyness)");
        let pos: i64 = conn
            .query_row("select count(*) from staging_measures_position", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            pos, 1,
            "the attribute grain is not multiplied by the buckets"
        );
        let v: f64 = conn
            .query_row(
                "select vega from staging_measures_underlying
                 where tenor = '1M' and moneyness = 105.0 and underlying_ref = 'SPX'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, 20.0);
    }

    /// The demo spot ladder's shape: NPV and theta are per instrument and
    /// repeat identically on each underlying row of a shift, delta is per
    /// underlying. A generator that derived the instrument-grain NPV from a
    /// per-underlying input would make the repeats disagree, and the split
    /// would keep one underlying's value under a conflict.
    #[test]
    fn an_instrument_grain_measure_repeated_per_underlying_splits_without_conflict() {
        use geode_core::config::{LayerDoc, merge_docs};
        let text = r#"
[spot_ladder]
axes = ["spot_shift"]
[spot_ladder.columns.book]
type = "utf8"
role = "dimension"
[spot_ladder.columns.lhu]
type = "utf8"
role = "dimension"
[spot_ladder.columns.position_ref]
type = "utf8"
role = "key"
[spot_ladder.columns.counterparty]
type = "utf8"
role = "dimension"
[spot_ladder.columns.instrument_ref]
type = "utf8"
role = "key"
[spot_ladder.columns.underlying_ref]
type = "utf8"
role = "dimension"
[spot_ladder.columns.spot_shift]
type = "f64"
role = "axis"
axis = "scenario"
[spot_ladder.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
[spot_ladder.columns.theta]
type = "f64"
role = "measure"
grain = "instrument"
[spot_ladder.columns.delta]
type = "f64"
role = "measure"
grain = "underlying"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        let (schema, diags) = geode_core::schema::SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let ds = schema.dataset("spot_ladder").unwrap().clone();
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "create table raw as select * from (values
               ('BK0','L0','P1','C','I1','SPX', 0.0, 100.0, -3.0, 10.0),
               ('BK0','L0','P1','C','I1','RUT', 0.0, 100.0, -3.0, 20.0),
               ('BK0','L0','P1','C','I1','SPX', 5.0, 250.0, -2.88, 12.0),
               ('BK0','L0','P1','C','I1','RUT', 5.0, 250.0, -2.88, 22.0))
             t(book, lhu, position_ref, counterparty, instrument_ref, underlying_ref,
               spot_shift, npv, theta, delta)",
        )
        .unwrap();
        let out = split_by_grain(
            &conn,
            &SplitRequest {
                dataset: &ds,
                raw_table: "raw",
                batch: "b",
                file_id: 1,
            },
        )
        .unwrap();
        assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        let per_shift = |table: &str| -> Vec<(f64, i64)> {
            let mut stmt = conn
                .prepare(&format!(
                    "select spot_shift, count(*) from {table} group by spot_shift order by spot_shift"
                ))
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(
            per_shift(&staging_table(Grain::Instrument)),
            [(0.0, 1), (5.0, 1)],
            "one instrument row per (instrument, shift)"
        );
        assert_eq!(
            per_shift(&staging_table(Grain::Underlying)),
            [(0.0, 2), (5.0, 2)],
            "one underlying row per (underlying, shift)"
        );
        let npv: f64 = conn
            .query_row(
                &format!(
                    "select npv from {} where spot_shift = 5.0",
                    staging_table(Grain::Instrument)
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(npv, 250.0);
    }
}
