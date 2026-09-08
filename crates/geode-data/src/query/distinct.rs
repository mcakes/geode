//! The picker's distinct-values query (Phase 4 spec §3.4): per value,
//! how many rows the current scope would leave, over every dataset
//! carrying the column, under the same era routing every query uses.

use crate::query::compile::{CompiledColumn, CompiledQuery, era_for};
use crate::query::scope_sql::{DictionaryCache, compile_scope_cached};
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
    compile_distinct_with_cache(conn, schema, dims, params, &mut DictionaryCache::default())
}

/// [`compile_distinct`], but resolving the text filter's catalog facts
/// through a `DictionaryCache` the caller supplies. `pub(crate)` only —
/// exists so a test can pass its own cache and read `DictionaryCache::
/// lookups`; `compile_distinct`'s own signature is the public contract.
pub(crate) fn compile_distinct_with_cache(
    conn: &Connection,
    schema: &SchemaSpec,
    dims: &DerivedDimensions,
    params: &DistinctParams,
    cache: &mut DictionaryCache,
) -> Result<CompiledQuery, StoreError> {
    let base = dims.base_column(&params.column);
    let mut selects: Vec<String> = Vec::new();
    let mut all_params: Vec<Value> = Vec::new();
    // One cache across the loop below, for the same reason
    // `compile_view` holds one across its grains. It happens to save
    // nothing *here*: `enum_type_name` is dataset-qualified
    // ("{dataset}_{column}_enum"), so two different datasets never share
    // a key and every iteration is a fresh miss. It costs nothing either
    // — one `HashMap` lookup before each real query — so the call site
    // stays uniform with `compile_view` rather than special-casing the
    // one place a cache is not sharing anything.
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
        let era = era_for(conn, &ds.name, &params.as_of)?;
        let scope = compile_scope_cached(conn, &params.scope, ds, grain, dims, era.era(), cache)?;
        let derived = dims.get(&params.column);
        let value_expr = match derived {
            None => format!("\"{base}\"::varchar"),
            Some(d) => crate::query::compile::derived_case(d),
        };
        selects.push(format!(
            "select {value_expr} as value, count(*) as n from {} where {} group by 1",
            // `era.relation` already applies the generation predicate to
            // both sides it reads (Phase 4a's as-of baseline fix); the
            // scope predicate alone is left for the caller to apply.
            era.era().relation(&ds.name, grain),
            scope.predicate,
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
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
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

    /// `schema()` with `book` also declared textual in both datasets
    /// (categorical stays on by default for a dimension column).
    fn schema_with_textual_book() -> SchemaSpec {
        let mut s = schema();
        for ds in s.datasets.iter_mut() {
            if let Some(c) = ds.columns.iter_mut().find(|c| c.name == "book") {
                c.textual = true;
            }
        }
        s
    }

    /// Review round 1, Minor 5: `compile_distinct` is the only call site
    /// where one `DictionaryCache` spans more than one dataset, and it
    /// had no coverage of that shape with a text scope at all —
    /// `two_dataset_fixture`'s schema declares no textual column, so the
    /// text block never touched the cache in any existing `distinct`
    /// test. Same data as `two_dataset_fixture`, but `book` is textual
    /// (and its ENUM built) in both `risk` and `ref`.
    fn two_dataset_fixture_with_textual_book() -> Fixture {
        let schema = schema_with_textual_book();
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store.apply_schema(schema.dataset("risk").unwrap()).unwrap();
        store.apply_schema(schema.dataset("ref").unwrap()).unwrap();
        let f = Fixture {
            _dir: dir,
            store,
            schema,
            dims: DerivedDimensions::default(),
            between: Utc::now(),
        };
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
        crate::store::ddl::refresh_enum(
            f.conn(),
            "risk",
            "book",
            "risk_instrument_live",
            "risk_instrument_archive",
        )
        .unwrap();
        crate::store::ddl::refresh_enum(
            f.conn(),
            "ref",
            "book",
            "ref_instrument_live",
            "ref_instrument_archive",
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
        // `compile_distinct` resolves each dataset's era through
        // `era_for`, which now reads the `generations` summary rather
        // than scanning tables directly -- rebuild it from this raw
        // fixture's tables ("ref" has none, so its summary stays empty).
        for ds in &f.schema.datasets {
            crate::store::ddl::rebuild_generations(
                f.conn(),
                &ds.name,
                &crate::store::ddl::history_of(&ds.name, ds),
            )
            .unwrap();
        }
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
    fn distinct_with_a_text_scope_over_two_datasets_resolves_each_dictionary_once() {
        // Review round 1, Minor 5. Both `risk` and `ref` declare `book`
        // textual, so this is the one place in the crate a
        // `DictionaryCache` genuinely spans two datasets' own dictionary
        // resolves in a single call (it saves nothing between them --
        // `enum_type_name` is dataset-qualified -- but the shape was
        // untested until now).
        let f = two_dataset_fixture_with_textual_book();
        let params = DistinctParams {
            key: QueryKey(1),
            tag: 1,
            column: "currency".into(),
            scope: Scope {
                text: Some("bk000".into()),
                ..Scope::default()
            },
            as_of: AsOf::Live,
        };

        // The cache must be invisible to the result: what `compile_
        // distinct` actually runs (one shared, internally-created cache)
        // must agree with an explicitly cache-mediated compile using a
        // cache this test controls -- and both must land on the counts
        // an un-cached compile would produce (the same BK000-only totals
        // `distinct_counts_values_under_the_given_scope_and_unions_
        // datasets` above gets from a dimension selection on "BK000",
        // since the text needle "bk000" narrows to exactly that book).
        let via_public = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert_eq!(
            via_public,
            vec![("EUR".to_string(), 3), ("USD".to_string(), 5)]
        );

        let mut cache = DictionaryCache::default();
        let via_explicit_cache =
            compile_distinct_with_cache(f.conn(), &f.schema, &f.dims, &params, &mut cache).unwrap();
        assert_eq!(
            f.run(&via_explicit_cache),
            via_public,
            "the cache must be invisible to the result"
        );
        assert_eq!(
            cache.lookups, 4,
            "one enum_types lookup plus one dictionary match per dataset -- 2 \
             datasets, one categorical textual column each -- since the cache \
             never shares a hit across datasets here (dataset-qualified ENUM \
             type names), but must not cost more than that either"
        );
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
