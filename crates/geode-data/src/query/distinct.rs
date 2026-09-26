//! The picker's distinct-values query (Phase 4 spec §3.4): per value,
//! how many rows the current scope would leave, over every dataset
//! carrying the column, under the same era routing every query uses.

use crate::query::as_of::{generation_predicate, resolve_generations};
use crate::query::compile::{CompiledColumn, CompiledQuery, era_for};
use crate::query::scope_sql::{
    DictionaryCache, compile_scope_cached, conjuncts, like_pattern, render_expr, selection_clause,
    text_column_term,
};
use crate::store::StoreError;
use crate::store::ddl::TablePair;
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::{AsOf, DistinctParams};
use geode_core::schema::{ColumnRole, DatasetSpec, SchemaSpec};

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
        // A document dataset has no grain, so the grain search below can
        // never find it a table: it took its own arm or the picker showed
        // a document-only dimension nothing at all (market-data spec
        // §3.3).
        if ds.is_document() {
            if let Some((select, select_params)) =
                document_select(conn, ds, dims, params, base, cache)?
            {
                selects.push(select);
                all_params.extend(select_params);
            }
            continue;
        }
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
            // `base` is the alias `membership`'s probes correlate on
            // (`probe.k is not distinct from base.k`), the same one
            // `compile_view` gives its own `from`; a scope term routed to
            // another grain fails to bind without it.
            "select {value_expr} as value, count(*) as n from {} base where {} group by 1",
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
        resolved_generation: None,
    })
}

/// One document dataset's contribution to the distinct-values union, or
/// `None` if it does not carry `base` as a dimension at all.
///
/// Shaped exactly like the measure arms' contribution — `select <value> as
/// value, count(*) as n from <relation> where <predicate> group by 1` —
/// because the outer query unions them and every branch of a union has to
/// agree on its column set.
///
/// Only a `Dimension` column counts. A document dataset's dimensions are
/// its identity key (`schema::validate_document` refuses an unkeyed one),
/// and an axis is row identity *within* one document rather than
/// something the frame groups or scopes by — the same rule
/// `groupable_columns` states (market-data spec §3.3). An axis or a value
/// is therefore not offered here, even though the picker's own list is
/// categorical-and-any-role: a categorical *attribute* no grain can group
/// by is likewise not offered by the measure arms.
///
/// **The whole scope is applied, lowered grain-free** (market-data spec
/// §4.5, Part 2 Task 1): the dimension selections `applicable_to` keeps,
/// the text filter, and the expression filter. `compile_scope` is still the
/// only place a *grain* routes one of them — `route`, `evaluable_at`,
/// `membership` and `Era::relation` all speak of a `Grain` this family does
/// not have — but routing is only ever the question of *where* a term is
/// evaluated, and a document dataset is one table, so every one of those
/// steps is a no-op here. What is left is the term itself, and the term is
/// shared rather than restated: `selection_clause` for a selection,
/// `scope_sql::text_column_term` for a textual column, `scope_sql::
/// render_expr` for an expression conjunct.
///
/// Two rules carry over from the measure path because the direction of the
/// error matters more than the rule:
///
/// * A text filter with no term surviving — every categorical column's
///   dictionary dropped the needle, or the dataset declares no textual
///   column at all — compiles to a literal `false`. A needle over a dataset
///   that cannot be searched matches **nothing**, never everything;
///   widening there is exactly the over-count Part 1 disclosed, and it
///   widens hardest at the moment the trader has narrowed hardest.
/// * A selection or expression conjunct naming a column this dataset has no
///   storage for is **dropped**, not compiled and not collapsed to `false`
///   (`Scope::applicable_to` for selections, the `stored` check below for
///   conjuncts). A frame-wide `:filter book = 'BK001'` must not fail the
///   whole picker query with a binder error on the one dataset that has no
///   `book`, and `false` would claim the document dataset holds no such rows
///   rather than that the question never reaches it. The widening that
///   leaves is the disclosed, deliberate one `ScopeSemantics::
///   NotApplicable` exists to report once Part 2's panel produces it.
fn document_select(
    conn: &Connection,
    ds: &DatasetSpec,
    dims: &DerivedDimensions,
    params: &DistinctParams,
    base: &str,
    cache: &mut DictionaryCache,
) -> Result<Option<(String, Vec<Value>)>, StoreError> {
    if !ds
        .column(base)
        .is_some_and(|c| matches!(c.role, ColumnRole::Dimension { .. }))
    {
        return Ok(None);
    }

    // Dataset-wide, not per document: distinct spans every key by
    // definition, so every partition's newest generation as of `t` is in
    // scope — the same resolution `era_for` performs for a measure
    // dataset. (`query::document::compile_document` narrows to one batch
    // because it answers for one document; this does not.)
    let pair = TablePair::for_document(&ds.name);
    let relation = match &params.as_of {
        AsOf::Live => pair.live.clone(),
        AsOf::At(t) => {
            let predicate = generation_predicate(&resolve_generations(conn, &ds.name, *t)?);
            // Both sides, each filtered: the generation a partition holds
            // now is in live and nowhere else, so an as-of read aimed at
            // the archive alone answers with the previous generation or
            // with nothing — the same rule `Era::relation` follows for a
            // grain table, and the same one `compile_document` follows.
            format!(
                "(select * from {} where {predicate} \
                 union all select * from {} where {predicate})",
                pair.live, pair.archive
            )
        }
    };

    // `applicable_to` drops the selections this dataset has no column for
    // and names them (market-data spec §3.4). Dropping them is what keeps
    // a scope aimed at the measure datasets — a book selection, say —
    // from compiling into a predicate on a column the document table does
    // not have.
    let (scope, _dropped) = params.scope.applicable_to(ds, dims);
    let mut clauses: Vec<String> = Vec::new();
    let mut sql_params: Vec<Value> = Vec::new();
    // Whether anything past the dimension selections is worth compiling.
    // Correctness does not need it — `false and <anything>` is still false —
    // but the text filter's dictionary rewrite costs real catalog
    // round-trips, and buying them for a predicate already known to select
    // nothing is a cost with no answer attached.
    let mut nothing_matches = scope.impossible;
    // A contradiction selects nothing and must say so in SQL, exactly as
    // `compile_scope` does: the contradicted dimension has already been
    // dropped from `dimensions`, so compiling the rest would produce a
    // predicate *wider* than either layer asked for.
    if scope.impossible {
        clauses.push("false".to_string());
    } else {
        for sel in &scope.dimensions {
            if sel.values.is_empty() {
                continue;
            }
            match selection_clause(sel, dims) {
                Some((clause, clause_params)) => {
                    clauses.push(clause);
                    sql_params.extend(clause_params);
                }
                // A derived value the map does not produce: nothing can
                // match, and the whole contribution is that constant.
                None => {
                    clauses.clear();
                    sql_params.clear();
                    clauses.push("false".to_string());
                    nothing_matches = true;
                    break;
                }
            }
        }
    }

    // The text filter, lowered grain-free. Every per-column decision is
    // `scope_sql::text_column_term`'s — the dictionary `IN` for a
    // categorical column whose ENUM type exists, a row-scanning `ILIKE`
    // otherwise — so the two paths cannot drift into searching different
    // things. `textual_columns()` is safe to read straight through here:
    // `validate_document` clears `textual` on anything but a dimension, and
    // a document dataset's dimensions are all key columns, so every column
    // this yields really is in the table `document_columns()` builds.
    //
    // No term surviving means a literal `false`, the measure path's own
    // rule: a needle over a dataset that cannot be searched matches
    // nothing, never everything.
    if !nothing_matches && let Some(text) = &scope.text {
        let pattern_text = like_pattern(text);
        // Resolved once and owned, not held as the cache's borrow: the loop
        // below needs `&mut cache` for the dictionary itself.
        let enum_types: Vec<String> = cache.enum_types(conn, &ds.name)?.to_vec();
        let mut terms: Vec<String> = Vec::new();
        let mut term_params: Vec<Value> = Vec::new();
        for col in ds.textual_columns() {
            if let Some((test, bound)) =
                text_column_term(conn, ds, col, &enum_types, &pattern_text, cache)?
            {
                terms.push(test);
                term_params.push(bound);
            }
        }
        if terms.is_empty() {
            clauses.push("false".to_string());
            nothing_matches = true;
        } else {
            clauses.push(format!("({})", terms.join(" or ")));
            sql_params.extend(term_params);
        }
    }

    // The expression filter, one top-level `and` term at a time — the same
    // split the measure path makes so each term can route on its own, kept
    // here because it is also the unit a document dataset can *drop*.
    // `render_expr` is the same lowering, derived dimensions translated back
    // to their source column and every literal bound.
    if !nothing_matches && let Some(expr) = &scope.expression {
        let stored: Vec<&str> = ds
            .document_columns()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        for term in conjuncts(expr) {
            // `document_columns()`, not `ds.column()`: it is the projection
            // the DDL was built from, so it is the exact set a predicate can
            // name. A conjunct over anything else is this dataset's
            // not-applicable, dropped for the reason in the doc comment
            // above.
            if term
                .columns()
                .iter()
                .any(|c| !stored.contains(&dims.base_column(c)))
            {
                continue;
            }
            let mut expr_params = Vec::new();
            let sql = render_expr(term, &mut expr_params, dims)?;
            clauses.push(sql);
            sql_params.extend(expr_params);
        }
    }

    let predicate = if clauses.is_empty() {
        "true".to_string()
    } else {
        clauses.join(" and ")
    };

    let value_expr = match dims.get(&params.column) {
        None => format!("\"{base}\"::varchar"),
        Some(d) => crate::query::compile::derived_case(d),
    };
    Ok(Some((
        format!(
            "select {value_expr} as value, count(*) as n from {relation} where {predicate} group by 1"
        ),
        sql_params,
    )))
}

fn meta(name: &str) -> CompiledColumn {
    CompiledColumn {
        name: name.into(),
        grain: None,
        attribution_by_depth: vec![Attribution::Additive],
        scope_semantics: ScopeSemantics::Direct,
        summable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::pool::{Payload, QueryRequest, RequestKind, ViewId, Work, run_one};
    use crate::store::ddl::tests_support::ts;
    use chrono::{DateTime, Utc};
    use geode_core::config::{LayerDoc, merge_docs};
    use geode_core::query::{AsOf, QueryKey};
    use geode_core::scope::{DimensionSelection, Scope};
    use geode_core::snapshot::Provenance;

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
                work: Work::Query(compiled.clone()),
                grouping: Vec::new(),
                provenance: Provenance::default(),
                kind: RequestKind::Distinct {
                    column: String::new(),
                },
            };
            let snap = match run_one(self.conn(), &req).unwrap() {
                Payload::Snapshot(s) => s,
                Payload::Series(_) => panic!("a distinct query answered with a series"),
            };
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

    /// A scope term on a column the picked dimension's own grain does not
    /// carry is routed through `membership`'s `exists (… probe where
    /// probe.k is not distinct from base.k …)`, which names the outer
    /// relation `base` — the alias `compile_view` gives its `from`.
    /// `compile_distinct` shipped without it (2026-09-19, seen on the
    /// scope chip: `Referenced table "base" not found`): picking `book`
    /// while scoped on an instrument-grain `currency` probed the
    /// instrument table from an unaliased position table.
    #[test]
    fn distinct_under_a_scope_probing_a_finer_grain_aliases_its_relation_as_base() {
        let f = two_dataset_fixture();
        f.conn()
            .execute_batch(
                "insert into risk_position_live values
                   ('BK000','L','P1','C', 1.0, 'b', 1, 1, now()),
                   ('BK000','L','P2','C', 1.0, 'b', 1, 1, now()),
                   ('BK000','L','P3','C', 1.0, 'b', 1, 1, now()),
                   ('BK000','L','P4','C', 1.0, 'b', 1, 1, now()),
                   ('BK001','L','P5','C', 1.0, 'b', 1, 1, now()),
                   ('BK001','L','P6','C', 1.0, 'b', 1, 1, now());",
            )
            .unwrap();
        let params = DistinctParams {
            key: QueryKey(1),
            tag: 1,
            column: "book".into(),
            scope: Scope {
                dimensions: vec![DimensionSelection {
                    column: "currency".into(),
                    values: vec!["EUR".into()],
                }],
                ..Scope::default()
            },
            as_of: AsOf::Live,
        };
        let compiled = compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap();
        assert!(
            compiled.sql.contains("risk_position_live base where exists (select 1 from risk_instrument_live probe where"),
            "{}",
            compiled.sql
        );
        // risk: BK000's P3 and P4 hold EUR instruments, BK001's none.
        // ref has no position grain, so its `book` comes straight off
        // `ref_instrument_live`, where BK000 has one EUR row (J4).
        assert_eq!(f.run(&compiled), vec![("BK000".to_string(), 3)]);
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

    /// A measure dataset and a document dataset sharing one dimension
    /// column, with different spellings on each side, plus a dimension
    /// only the document dataset has (market-data spec §3.3).
    ///
    /// `risk` carries `underlying_ref` as an underlying-grain dimension
    /// key; `cvi_params` is a document dataset keyed on `(underlying_ref,
    /// curve_id)`, so `underlying_ref` is shared and `curve_id` is the
    /// document family's alone. `pickable_columns` offers both — every
    /// categorical column of every dataset — so `compile_distinct` has to
    /// answer for both or the picker paints a short list for one and an
    /// error for the other.
    const DOC_SCHEMA: &str = r#"
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
[risk.columns.delta01]
type = "f64"
role = "measure"
grain = "underlying"
[risk.columns.spot_ref]
type = "f64"
role = "attribute"
grain = "underlying"

[cvi_params]
family = "document"
key = ["underlying_ref", "curve_id"]
axes = ["term", "node"]
[cvi_params.columns.underlying_ref]
type = "utf8"
role = "dimension"
textual = true
[cvi_params.columns.curve_id]
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
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
"#;

    /// Six rows (two terms x three nodes) for one `(underlying, curve)`
    /// document, carrying `spot_ref` as its one document-level attribute.
    ///
    /// The attribute exists so an expression filter has something to
    /// narrow *by* that is not the key itself: a document dataset's
    /// dimensions are all key columns (`validate_document`), so without an
    /// attribute the only expression this fixture could express would
    /// duplicate a dimension selection. Each document gets its own value,
    /// so `spot_ref > 7000` really separates SPX.Z from NDX.Z.
    fn cvi_doc(underlying: &str, curve: &str, spot: f64) -> geode_core::document::DocumentRows {
        use geode_core::document::{Column, DocumentRows, Value as DocValue};
        let d = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        DocumentRows {
            key: vec![underlying.into(), curve.into()],
            attributes: vec![("spot_ref".into(), DocValue::F64(spot))],
            axes: vec![
                (
                    "term".into(),
                    Column::Date(vec![
                        d("2026-09-18"),
                        d("2026-09-18"),
                        d("2026-09-18"),
                        d("2026-10-16"),
                        d("2026-10-16"),
                        d("2026-10-16"),
                    ]),
                ),
                (
                    "node".into(),
                    Column::F64(vec![-20.0, -1.0, 3.5, -20.0, -1.0, 3.5]),
                ),
            ],
            values: vec![("param".into(), Column::F64(vec![1.0; 6]))],
        }
    }

    /// `risk` holds 2 SPX.Z + 1 NDX.Z underlying rows in BK000 and 1
    /// RTY.Z row in BK001. `cvi_params` holds one six-row document per
    /// `(SPX.Z, EQ1)` and `(NDX.Z, EQ1)`, published at 14:00 and 14:01 —
    /// `between` sits after the first and before the second.
    ///
    /// `spot_ref` is declared on both sides — a document-level attribute on
    /// `cvi_params`, an underlying-grain attribute on `risk` — and carries
    /// 7100 for SPX.Z, 5200 for NDX.Z and 2400 for RTY.Z, so one expression
    /// (`spot_ref > 7000`) narrows both datasets and separates the two
    /// documents. It is declared on `risk` as well because `route` refuses a
    /// column no grain carries: an expression naming a document-only column
    /// would fail the measure arm, and with it the whole union, before the
    /// document arm was ever reached.
    fn document_fixture() -> Fixture {
        let doc = merge_docs(
            "datasets",
            &[LayerDoc::builtin("datasets", DOC_SCHEMA).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path().join("g.duckdb")).unwrap();
        store.apply_schema(schema.dataset("risk").unwrap()).unwrap();
        store
            .apply_schema(schema.dataset("cvi_params").unwrap())
            .unwrap();
        crate::store::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        let f = Fixture {
            _dir: dir,
            store,
            schema,
            dims: DerivedDimensions::default(),
            between: ts("2026-09-12T14:00:30Z"),
        };
        f.conn()
            .execute_batch(
                "insert into risk_underlying_live
                   (book, lhu, position_ref, counterparty, instrument_ref, underlying_ref,
                    delta01, spot_ref, batch, source_file_id, gen_id, source_time) values
                   ('BK000','L','P1','C','I1','SPX.Z', 1.0, 7100.0, 'b', 1, 1, now()),
                   ('BK000','L','P2','C','I2','SPX.Z', 1.0, 7100.0, 'b', 1, 1, now()),
                   ('BK000','L','P3','C','I3','NDX.Z', 1.0, 5200.0, 'b', 1, 1, now()),
                   ('BK001','L','P4','C','I4','RTY.Z', 1.0, 2400.0, 'b', 1, 1, now());",
            )
            .unwrap();
        let ds = f.schema.dataset("cvi_params").unwrap().clone();
        for (underlying, spot, at) in [
            ("SPX.Z", 7100.0, "2026-09-12T14:00:00Z"),
            ("NDX.Z", 5200.0, "2026-09-12T14:01:00Z"),
        ] {
            crate::store::document::publish_document(
                &f.store,
                &crate::store::document::DocumentPublishRequest {
                    dataset: &ds,
                    source: "cvi",
                    rows: &cvi_doc(underlying, "EQ1", spot),
                    source_time: ts(at),
                    received_at: ts(at),
                    bytes: 0,
                },
            )
            .unwrap();
        }
        f
    }

    /// Important 3 (final fix wave): `compile_distinct` located a column
    /// through `ds.grains()` alone, so a document dataset — which has no
    /// grain — contributed nothing. A shared column showed only the
    /// measure dataset's values.
    #[test]
    fn distinct_unions_a_document_datasets_values_with_a_measure_datasets() {
        let f = document_fixture();
        let params = DistinctParams {
            column: "underlying_ref".into(),
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert_eq!(
            rows,
            vec![
                // 1 risk row + one six-row document
                ("NDX.Z".to_string(), 7),
                ("RTY.Z".to_string(), 1),
                // 2 risk rows + one six-row document
                ("SPX.Z".to_string(), 8),
            ]
        );
    }

    /// The other half of Important 3: a dimension only a document dataset
    /// declares used to fail the whole query with "no dataset carries",
    /// because no grain carried it anywhere.
    #[test]
    fn distinct_over_a_document_only_dimension_returns_its_values() {
        let f = document_fixture();
        let params = DistinctParams {
            column: "curve_id".into(),
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert_eq!(
            rows,
            vec![("EQ1".to_string(), 12)],
            "both documents' rows, and no error from the measure dataset that lacks the column"
        );
    }

    /// An axis is row identity *within* one document, never a frame
    /// dimension (market-data spec §3.3, `groupable_columns`), so it is
    /// not a column the picker can offer values for — the same answer a
    /// measure dataset gives for a measure. Unknown to every dataset, the
    /// request is an error rather than an empty list.
    #[test]
    fn distinct_over_a_document_dataset_offers_no_axis_values() {
        let f = document_fixture();
        for column in ["term", "node", "param"] {
            let params = DistinctParams {
                column: column.into(),
                ..base_params()
            };
            let e = compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap_err();
            assert!(
                e.to_string().contains(column),
                "{column} must not be offered: {e}"
            );
        }
    }

    /// As-of over a document dataset resolves generations dataset-wide,
    /// not per document: distinct spans every key, so every partition's
    /// newest generation at `t` is in scope — and a document published
    /// after `t` is not.
    #[test]
    fn distinct_over_a_document_dataset_under_as_of_reads_the_resolved_generation() {
        let f = document_fixture();
        let params = DistinctParams {
            column: "curve_id".into(),
            as_of: AsOf::At(f.between),
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert_eq!(
            rows,
            vec![("EQ1".to_string(), 6)],
            "only the document published before `between`: {rows:?}"
        );

        let params = DistinctParams {
            column: "underlying_ref".into(),
            as_of: AsOf::At(f.between),
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert!(
            rows.iter().any(|(v, n)| v == "SPX.Z" && *n == 6),
            "the document that existed by then: {rows:?}"
        );
        assert!(
            !rows.iter().any(|(v, n)| v == "NDX.Z" && *n >= 6),
            "the later document must not leak into an as-of read: {rows:?}"
        );
    }

    /// A dimension selection the document dataset does have really binds:
    /// narrowing to one underlying leaves that document's rows alone.
    #[test]
    fn a_selection_on_a_document_dimension_narrows_its_contribution() {
        let f = document_fixture();
        let params = DistinctParams {
            column: "curve_id".into(),
            scope: Scope {
                dimensions: vec![DimensionSelection {
                    column: "underlying_ref".into(),
                    values: vec!["SPX.Z".into()],
                }],
                ..Scope::default()
            },
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert_eq!(rows, vec![("EQ1".to_string(), 6)]);
    }

    /// A selection on a column the document dataset has no column for is
    /// dropped (`Scope::applicable_to`, market-data spec §3.4) rather than
    /// compiled into a predicate on a column that does not exist — the
    /// dataset contributes its values unnarrowed, and the measure dataset
    /// beside it is narrowed as always.
    #[test]
    fn a_selection_the_document_dataset_lacks_is_dropped_not_a_binder_error() {
        let f = document_fixture();
        let params = DistinctParams {
            column: "underlying_ref".into(),
            scope: book_scope("BK001"),
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert_eq!(
            rows,
            vec![
                ("NDX.Z".to_string(), 6),
                ("RTY.Z".to_string(), 1),
                ("SPX.Z".to_string(), 6),
            ],
            "risk narrowed to BK001's one row; both documents unnarrowed: {rows:?}"
        );
    }

    /// Part 2 Task 1 (the item Part 1 parked): the text filter is lowered
    /// grain-free over the document table, so a text-filtered picker no
    /// longer counts every document regardless of the needle.
    ///
    /// `cvi_params.underlying_ref` is textual and categorical, so the
    /// needle goes through its ENUM dictionary rather than a row scan —
    /// the same rewrite the measure path uses. `risk` declares no textual
    /// column at all, so it contributes nothing under the same
    /// `false`-if-nothing-searchable rule `compile_scope` has always
    /// applied: the whole answer is the one document the needle keeps.
    #[test]
    fn a_text_filter_narrows_a_document_datasets_contribution() {
        let f = document_fixture();
        let params = DistinctParams {
            column: "underlying_ref".into(),
            scope: Scope {
                text: Some("spx".into()),
                ..Scope::default()
            },
            ..base_params()
        };
        let compiled = compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap();
        // The needle really went through the ENUM dictionary rather than a
        // row scan: `publish_document` refreshes the type for every
        // categorical column, so the rewrite is available here and taking it
        // is the §7.1 half of this change (Phase 4a measured the row-scan
        // form at 63ms where the dictionary form is 20.6ms). Both forms
        // return the same rows, so only the statement text can tell them
        // apart.
        assert!(
            compiled.sql.contains("string_split"),
            "the document arm's text term must be the dictionary IN, not an ILIKE row scan: {}",
            compiled.sql
        );
        assert!(
            !compiled.sql.contains("ilike"),
            "no row-scanning term should survive: {}",
            compiled.sql
        );
        let rows = f.run(&compiled);
        assert_eq!(
            rows,
            vec![("SPX.Z".to_string(), 6)],
            "NDX.Z is dropped by the needle, and `risk` has nothing to search: {rows:?}"
        );
    }

    /// The other half of the rule, and the direction that matters: a
    /// needle over a dataset that cannot be searched matches NOTHING, never
    /// everything. Without it a document dataset would widen to its full
    /// row count exactly when the trader has narrowed hardest — the
    /// over-count Part 1 disclosed.
    #[test]
    fn a_text_filter_on_a_document_dataset_with_no_textual_column_contributes_nothing() {
        let mut f = document_fixture();
        let ds = f
            .schema
            .datasets
            .iter_mut()
            .find(|d| d.name == "cvi_params")
            .expect("the fixture declares cvi_params");
        for c in ds.columns.iter_mut() {
            c.textual = false;
        }
        let params = DistinctParams {
            column: "underlying_ref".into(),
            scope: Scope {
                text: Some("spx".into()),
                ..Scope::default()
            },
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert!(
            rows.is_empty(),
            "neither dataset has a searchable column, so neither contributes: {rows:?}"
        );
    }

    /// The expression filter is lowered over the same relation, with no
    /// grain to route it through. `spot_ref` is the document's own
    /// attribute — one value per document — so `spot_ref > 7000` keeps
    /// SPX.Z's six rows and drops NDX.Z's, and narrows `risk` to its two
    /// SPX.Z rows by the same predicate.
    #[test]
    fn an_expression_filter_narrows_a_document_datasets_contribution() {
        let f = document_fixture();
        let params = DistinctParams {
            column: "underlying_ref".into(),
            scope: Scope {
                expression: Some(geode_core::scope::parse_expr("spot_ref > 7000").unwrap()),
                ..Scope::default()
            },
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert_eq!(
            rows,
            vec![("SPX.Z".to_string(), 8)],
            "2 risk rows + SPX.Z's six document rows; the rest are below 7000: {rows:?}"
        );
    }

    /// An expression conjunct naming a column the document dataset has no
    /// storage for is DROPPED, exactly as `applicable_to` drops such a
    /// dimension selection (market-data spec §3.4) — not compiled into a
    /// binder error that would fail the whole picker query, and not
    /// collapsed to `false`, which would claim the document dataset holds
    /// no such rows rather than that the question does not reach it.
    ///
    /// `:filter book = 'BK001'` is the standing case: a frame-wide filter
    /// on a measure-side column, with the picker open on a column the
    /// document dataset shares.
    #[test]
    fn an_expression_the_document_dataset_lacks_a_column_for_is_dropped_not_an_error() {
        let f = document_fixture();
        let params = DistinctParams {
            column: "underlying_ref".into(),
            scope: Scope {
                expression: Some(geode_core::scope::parse_expr("book = 'BK001'").unwrap()),
                ..Scope::default()
            },
            ..base_params()
        };
        let rows = f.run(&compile_distinct(f.conn(), &f.schema, &f.dims, &params).unwrap());
        assert_eq!(
            rows,
            vec![
                ("NDX.Z".to_string(), 6),
                ("RTY.Z".to_string(), 1),
                ("SPX.Z".to_string(), 6),
            ],
            "risk narrowed to BK001; both documents unnarrowed, having no `book`: {rows:?}"
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
