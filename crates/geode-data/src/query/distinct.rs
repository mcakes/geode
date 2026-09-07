//! The picker's distinct-values query (Phase 4 spec §3.4): per value,
//! how many rows the current scope would leave, over every dataset
//! carrying the column, under the same era routing every query uses.

use crate::query::compile::{CompiledColumn, CompiledQuery, era_for};
use crate::query::scope_sql::compile_scope;
use crate::store::StoreError;
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::DistinctParams;
use geode_core::schema::SchemaSpec;

pub fn compile_distinct(
    conn: &Connection,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
    params: &DistinctParams,
) -> Result<CompiledQuery, StoreError> {
    let base = dims.base_column(&params.column);
    let mut selects: Vec<String> = Vec::new();
    let mut all_params: Vec<Value> = Vec::new();
    for ds in &schema.datasets {
        // The coarsest grain carrying the column: the smallest table
        // that sees every value.
        let Some(grain) = ds
            .grains()
            .into_iter()
            .find(|g| ds.carries(*g, base) || ds.column(base).and_then(|c| c.grain()) == Some(*g))
        else {
            continue;
        };
        let era = era_for(conn, &ds.name, ds, &params.as_of)?;
        let scope = compile_scope(conn, &params.scope, ds, grain, dims, era.era())?;
        let derived = dims.get(&params.column);
        let value_expr = match derived {
            None => format!("\"{base}\"::varchar"),
            Some(d) => crate::query::compile::derived_case(d),
        };
        selects.push(format!(
            "select {value_expr} as value, count(*) as n from {} where {} group by 1",
            era.era().relation(&ds.name, grain),
            match era.era().generations {
                Some(g) => format!("({}) and ({g})", scope.predicate),
                None => scope.predicate.clone(),
            }
        ));
        all_params.extend(scope.params);
    }
    if selects.is_empty() {
        return Err(StoreError::Sql {
            statement: format!("distinct '{}'", params.column),
            source: duckdb::Error::InvalidParameterName(format!(
                "no dataset carries '{}'",
                params.column
            )),
        });
    }
    let sql = format!(
        "select value, sum(n)::bigint as n from ({}) u where value is not null group by 1 order by 1",
        selects.join(" union all ")
    );
    Ok(CompiledQuery {
        sql,
        params: all_params,
        grouping: Vec::new(),
        columns: vec![meta("value"), meta("n")],
        stalest_input: Vec::new(),
        resolved_as_of: Default::default(),
    })
}

fn meta(name: &str) -> CompiledColumn {
    CompiledColumn {
        name: name.into(),
        grain: None,
        attribution_by_depth: vec![Attribution::Additive],
        scope_semantics: ScopeSemantics::Direct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::pool::{QueryRequest, RequestKind, ViewId, run_one};
    use chrono::{DateTime, Utc};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::query::{AsOf, QueryKey};
    use geode_core::scope::{DimensionSelection, Scope};
    use geode_core::snapshot::Provenance;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Two datasets both carrying `currency` at instrument grain: `risk`
    /// (which also has a position-grain measure, so `carries` at Position
    /// is exercised and correctly fails) and `ref` (instrument-only).
    fn schema() -> SchemaSpec {
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
[risk.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[risk.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
[risk.columns.daily_trading_pnl]
type = "f64"
role = "measure"
grain = "position"

[ref.columns.book]
type = "utf8"
role = "dimension"
[ref.columns.lhu]
type = "utf8"
role = "dimension"
[ref.columns.position_ref]
type = "utf8"
role = "key"
[ref.columns.counterparty]
type = "utf8"
role = "dimension"
[ref.columns.instrument_ref]
type = "utf8"
role = "key"
[ref.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
[ref.columns.notional]
type = "f64"
role = "measure"
grain = "instrument"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc).0
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: crate::store::Store,
        schema: SchemaSpec,
        dims: DerivedDimensions,
        between: DateTime<Utc>,
    }

    impl Fixture {
        fn conn(&self) -> &Connection {
            self.store.writer()
        }

        fn run(&self, compiled: &CompiledQuery) -> Vec<(String, u64)> {
            let req = QueryRequest {
                key: QueryKey(0),
                tag: 0,
                submitted: std::time::Instant::now(),
                view: ViewId("distinct".into()),
                compiled: compiled.clone(),
                grouping: Vec::new(),
                provenance: Provenance::default(),
                kind: RequestKind::Distinct {
                    column: String::new(),
                },
            };
            let snap = run_one(self.conn(), &req).unwrap();
            let v = snap.column_index("value").expect("distinct selects value");
            let n = snap.column_index("n").expect("distinct selects n");
            (0..snap.rows())
                .filter_map(|row| {
                    Some((
                        snap.text_at(v, row)?.to_string(),
                        snap.i64_at(n, row)? as u64,
                    ))
                })
                .collect()
        }
    }

    fn build_fixture(between: DateTime<Utc>) -> Fixture {
        let schema = schema();
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store.apply_schema(schema.dataset("risk").unwrap()).unwrap();
        store.apply_schema(schema.dataset("ref").unwrap()).unwrap();
        Fixture {
            _dir: dir,
            store,
            schema,
            dims: DerivedDimensions::default(),
            between,
        }
    }

    /// `risk` and `ref` both carry `currency` at instrument grain.
    /// BK000: risk has 2 USD + 2 EUR instrument rows, ref has 3 USD + 1
    /// EUR — combined 5 USD, 3 EUR. BK001 adds risk's 1 USD + 1 JPY and
    /// ref's 1 GBP, so the unscoped total (11) exceeds BK000's alone (8).
    fn two_dataset_fixture() -> Fixture {
        let f = build_fixture(Utc::now());
        f.conn()
            .execute_batch(
                "insert into risk_instrument_live values
                   ('BK000','L','P1','C','I1', 1.0, 'USD', 'b', 1, 1, now()),
                   ('BK000','L','P2','C','I2', 1.0, 'USD', 'b', 1, 1, now()),
                   ('BK000','L','P3','C','I3', 1.0, 'EUR', 'b', 1, 1, now()),
                   ('BK000','L','P4','C','I4', 1.0, 'EUR', 'b', 1, 1, now()),
                   ('BK001','L','P5','C','I5', 1.0, 'USD', 'b', 1, 1, now()),
                   ('BK001','L','P6','C','I6', 1.0, 'JPY', 'b', 1, 1, now());
                 insert into ref_instrument_live values
                   ('BK000','L','P7','C','J1', 10.0, 'USD', 'b', 1, 1, now()),
                   ('BK000','L','P8','C','J2', 10.0, 'USD', 'b', 1, 1, now()),
                   ('BK000','L','P9','C','J3', 10.0, 'USD', 'b', 1, 1, now()),
                   ('BK000','L','P10','C','J4', 10.0, 'EUR', 'b', 1, 1, now()),
                   ('BK001','L','P11','C','J5', 10.0, 'GBP', 'b', 1, 1, now());",
            )
            .unwrap();
        f
    }

    /// `risk`'s BK000 partition has two generations: the archived one
    /// (2026-08-01, `currency = 'GBP'`) and the live, current one
    /// (2026-08-10, `currency = 'USD'`). `between` sits strictly after the
    /// first and strictly before the second.
    fn two_dataset_fixture_with_history() -> Fixture {
        let f = build_fixture(ts("2026-08-05T00:00:00Z"));
        f.conn()
            .execute_batch(
                "insert into risk_instrument_archive values
                   ('BK000','L','P1','C','I1', 1.0, 'GBP', 'b1', 1, 1,
                    TIMESTAMPTZ '2026-08-01T00:00:00Z');
                 insert into risk_instrument_live values
                   ('BK000','L','P1','C','I1', 1.0, 'USD', 'b1', 1, 2,
                    TIMESTAMPTZ '2026-08-10T00:00:00Z');",
            )
            .unwrap();
        f
    }

    /// `two_dataset_fixture` with a `[dimensions]` doc mapping `book` to
    /// a derived `desk` dimension (Phase 4a §3.4's `compile_distinct`
    /// derived-dimension branch): BK000 -> NORTH, BK001 -> SOUTH.
    fn two_dataset_fixture_with_desk_dims() -> Fixture {
        let mut f = two_dataset_fixture();
        let doc = merge_docs(
            "dimensions",
            &[LayerDoc::builtin(
                "dimensions",
                "[desk]\nfrom = \"book\"\n[desk.values]\nNORTH = [\"BK000\"]\nSOUTH = [\"BK001\"]\n",
            )
            .unwrap()],
        );
        f.dims = DerivedDimensions::from_doc(&doc).0;
        f
    }

    fn book_scope(book: &str) -> Scope {
        Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec![book.to_string()],
            }],
            ..Scope::default()
        }
    }

    fn base_params() -> DistinctParams {
        DistinctParams {
            key: QueryKey(1),
            tag: 1,
            column: "currency".into(),
            scope: Scope::default(),
            as_of: AsOf::Live,
        }
    }

    #[test]
    fn distinct_counts_values_under_the_given_scope_and_unions_datasets() {
        let f = two_dataset_fixture(); // risk (book, currency at instrument) + ref (currency at instrument)
        let params = DistinctParams {
            key: QueryKey(1),
            tag: 1,
            column: "currency".into(),
            scope: book_scope("BK000"),
            as_of: AsOf::Live,
        };
        let compiled = compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap();
        let rows = f.run(&compiled); // Vec<(String, u64)> via run_one + text_at/i64_at
        assert_eq!(rows, vec![("EUR".to_string(), 3), ("USD".to_string(), 5)]);
        // A book selection narrowed the counts; unscoped they are larger.
        let unscoped = DistinctParams {
            scope: Scope::default(),
            ..params.clone()
        };
        let all = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &unscoped).unwrap());
        assert!(all.iter().map(|(_, n)| n).sum::<u64>() > 8);
    }

    #[test]
    fn distinct_over_an_unknown_column_is_an_error_not_a_binder_failure() {
        let f = two_dataset_fixture();
        let params = DistinctParams {
            column: "nope".into(),
            ..base_params()
        };
        let e = compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap_err();
        assert!(e.to_string().contains("nope"));
    }

    #[test]
    fn distinct_under_as_of_reads_the_archive_era() {
        let f = two_dataset_fixture_with_history(); // two generations, currency changed between them
        let params = DistinctParams {
            as_of: AsOf::At(f.between),
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert!(
            rows.iter().any(|(v, _)| v == "GBP"),
            "the older generation's value: {rows:?}"
        );
        assert!(
            !rows.iter().any(|(v, _)| v == "USD"),
            "the live generation must not leak into an as-of read: {rows:?}"
        );
    }

    /// D2 (final fix wave, T2 deferred): `DistinctParams.column` naming a
    /// derived dimension must take `compile_distinct`'s `derived_case`
    /// branch — the values returned are the derived labels (`NORTH`/
    /// `SOUTH`), with counts summed across every source value each label
    /// covers, not the source `book` values themselves. Compared against
    /// the same fixture's own `book` query rather than hand-computed
    /// counts, so this doesn't also have to pin `compile_distinct`'s
    /// per-dataset grain selection (which `book`, carried at every
    /// grain, is subject to regardless of whether it's requested
    /// directly or through a derived dimension): whatever `book` itself
    /// returns, `desk` must return the same total, just relabeled.
    #[test]
    fn compile_distinct_over_a_derived_dimension_groups_by_its_labels() {
        let f = two_dataset_fixture_with_desk_dims();
        let base = DistinctParams {
            key: QueryKey(1),
            tag: 1,
            column: "book".into(),
            scope: Scope::default(),
            as_of: AsOf::Live,
        };
        let book_rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &base).unwrap());
        let book_total: u64 = book_rows.iter().map(|(_, n)| n).sum();
        assert_eq!(
            book_rows
                .iter()
                .map(|(v, _)| v.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["BK000", "BK001"]),
            "sanity: the fixture's own book values, unrelabeled: {book_rows:?}"
        );

        let desk_params = DistinctParams {
            column: "desk".into(),
            ..base
        };
        let desk_rows =
            f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &desk_params).unwrap());
        let desk_total: u64 = desk_rows.iter().map(|(_, n)| n).sum();
        assert_eq!(
            desk_rows
                .iter()
                .map(|(v, _)| v.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["NORTH", "SOUTH"]),
            "the derived dimension's own labels, not the source `book` values: {desk_rows:?}"
        );
        assert_eq!(
            desk_total, book_total,
            "the derived query must sum the same rows the base column's own query does, just \
             relabeled — not drop or double-count any: book {book_rows:?} vs desk {desk_rows:?}"
        );
    }
}
