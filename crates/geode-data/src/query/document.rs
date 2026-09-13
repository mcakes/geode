//! The document request (market-data spec §7): one document by key, live
//! or as-of, as a plain select in axis order. Deliberately not a view:
//! there is no grouping, no aggregation and no scope — the key IS the
//! predicate — so the tree compiler has nothing to add and everything to
//! get wrong.

use crate::query::as_of::resolve_generations;
use crate::query::compile::{CompiledColumn, CompiledQuery};
use crate::store::StoreError;
use crate::store::ddl::TablePair;
use duckdb::Connection;
use duckdb::types::Value;
use geode_core::attribution::{Attribution, ScopeSemantics};
use geode_core::document::join_key;
use geode_core::query::{AsOf, DocumentParams};
use geode_core::schema::{ColumnRole, SchemaSpec};
use std::collections::BTreeMap;

fn invalid(msg: String) -> StoreError {
    StoreError::Document(msg)
}

/// Compile a document request into one `select` against the dataset's
/// document pair (`store::ddl::TablePair::for_document`).
///
/// Live reads the live table with no generation predicate at all, the
/// same rule every other live read follows (`ddl`'s own doc comment: no
/// `gen_id` column is even consulted). As-of resolves against the
/// `generations` summary the same way a view query does
/// (`query::as_of::resolve_generations`) — but only for *this request's
/// own document*, not the whole dataset's.
///
/// That narrowing is deliberate, not an optimisation of the general
/// multi-partition machinery `generation_predicate` builds for a view
/// spanning every book: a document dataset can hold many unrelated
/// documents (one partition per distinct key), and a request for one of
/// them must never be affected by, or report freshness for, another. So
/// this resolves every partition `resolve_generations` can see as of
/// `t` (a summary-table read, cheap regardless of how many documents
/// exist) and then picks out the one whose `batch` is this request's own
/// key — the exact same `join_key` call `publish_document` made to name
/// the partition on the way in, recomputed here on the way out, so the
/// two can never name it differently.
/// The result is a plain `gen_id = N` equality, not an `IN`-list: with
/// exactly one partition in scope there is exactly one generation to
/// pin, and a bare equality is both the cheapest predicate DuckDB can
/// push down and the most honest reading of "this one document, as it
/// stood at T".
pub fn compile_document(
    conn: &Connection,
    schema: &SchemaSpec,
    params: &DocumentParams,
) -> Result<CompiledQuery, StoreError> {
    let ds = schema
        .dataset(&params.dataset)
        .ok_or_else(|| invalid(format!("unknown dataset '{}'", params.dataset)))?;
    if !ds.is_document() {
        return Err(invalid(format!(
            "dataset '{}' is not a document dataset",
            ds.name
        )));
    }
    if params.document_key.len() != ds.key.len() {
        return Err(invalid(format!(
            "key has {} parts, dataset '{}' declares {}",
            params.document_key.len(),
            ds.name,
            ds.key.len()
        )));
    }

    let columns = ds.document_columns();
    let projection = columns
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let order = ds
        .axes
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let key_predicate = ds
        .key
        .iter()
        .map(|k| format!("\"{k}\" = ?"))
        .collect::<Vec<_>>()
        .join(" and ");
    let sql_params: Vec<Value> = params
        .document_key
        .iter()
        .map(|v| Value::Text(v.clone()))
        .collect();

    let tables = TablePair::for_document(&ds.name);
    let mut resolved_as_of = BTreeMap::new();
    let (table, era) = match &params.as_of {
        AsOf::Live => (tables.live.clone(), String::new()),
        AsOf::At(t) => {
            let batch = join_key(&params.document_key);
            let gens = resolve_generations(conn, &ds.name, *t)?;
            let resolved = gens.into_iter().find(|g| g.batch == batch);
            // Read from the union of both tables regardless of which side
            // the resolved generation lives on: as-of's newest generation
            // may still be live (`publish_file` only ever moves the
            // *outgoing* generation to the archive), so a query aimed at
            // the archive alone would silently answer "no such document"
            // for the common case of asking about the current state a
            // moment in the past.
            let relation = format!(
                "(select * from {} union all select * from {})",
                tables.live, tables.archive
            );
            match resolved {
                Some(g) => {
                    resolved_as_of.insert(ds.name.clone(), g.source_time);
                    // `generation_predicate`'s own doc comment warns that
                    // `gen_id` alone can collide on a database loaded by
                    // a build that predates the id sequence. That does
                    // not apply here: the document family's tables are
                    // new on this branch, so every `gen_id` a document
                    // dataset ever holds was reserved from the sequence,
                    // and one publish writes exactly one batch — so a
                    // `gen_id` can never name two different partitions.
                    (relation, format!(" and gen_id = {}", g.gen_id))
                }
                // No generation of this document existed by `t`: a
                // request that compiles and returns nothing, not an
                // error — the same "unknown key" shape a live query
                // answers with an empty result rather than a refusal.
                None => (relation, " and false".to_string()),
            }
        }
    };

    let sql =
        format!("select {projection} from {table} where {key_predicate}{era} order by {order}");

    let compiled_columns = columns
        .iter()
        .map(|c| CompiledColumn {
            name: c.name.clone(),
            grain: None,
            attribution_by_depth: vec![match c.role {
                // A document value is real on its row and must never be
                // totalled — the exact meaning of DeterminedNonAdditive.
                // Every other column here (the key, the axes, the
                // grainless attributes) is a label, and the blotter's
                // marker only ever looks at a measure's own attribution.
                ColumnRole::Value => Attribution::DeterminedNonAdditive,
                _ => Attribution::Additive,
            }],
            scope_semantics: ScopeSemantics::Direct,
        })
        .collect();

    Ok(CompiledQuery {
        sql,
        params: sql_params,
        // No grouping: a document has no tree, spec §7 — the snapshot is
        // depth 0 only.
        grouping: Vec::new(),
        columns: compiled_columns,
        stalest_input: vec![ds.name.clone()],
        resolved_as_of,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::ddl::tests_support::{cvi_dataset, cvi_doc, ts};
    use crate::store::document::{DocumentPublishRequest, publish_document};
    use geode_core::query::{AsOf, QueryKey};

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Store,
        schema: SchemaSpec,
        /// SPX.Z's first generation (2026-09-12T14:00:00Z).
        spx_first_gen: i64,
    }

    /// SPX.Z published twice (14:00, then republished at 14:05) with a
    /// second key, NDX.Z, published once in between (14:03).
    ///
    /// The second key exists so the as-of arm's per-document resolution
    /// has more than one partition to choose from at once. With only one
    /// document ever published, `resolve_generations`'s result always
    /// has exactly one entry, so `.find(|g| g.batch == batch)` and a bug
    /// that took whichever generation the resolve happened to list first
    /// (`.next()`, `min_by_key`, …) read identically — the gap the Task
    /// 8 review's Important #1 named. Two documents at different
    /// source times makes the two implementations diverge.
    fn fixture_with_two_generations() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let first = publish_document(
            &store,
            &DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("SPX.Z", [1., 2., 3., 4., 5., 6.]),
                source_time: ts("2026-09-12T14:00:00Z"),
                received_at: ts("2026-09-12T14:00:00Z"),
                bytes: 0,
            },
        )
        .unwrap();
        publish_document(
            &store,
            &DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("NDX.Z", [100., 200., 300., 400., 500., 600.]),
                source_time: ts("2026-09-12T14:03:00Z"),
                received_at: ts("2026-09-12T14:03:00Z"),
                bytes: 0,
            },
        )
        .unwrap();
        publish_document(
            &store,
            &DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("SPX.Z", [10., 20., 30., 40., 50., 60.]),
                source_time: ts("2026-09-12T14:05:00Z"),
                received_at: ts("2026-09-12T14:05:00Z"),
                bytes: 0,
            },
        )
        .unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        Fixture {
            _dir: dir,
            store,
            schema,
            spx_first_gen: first.gen_id,
        }
    }

    fn params(dataset: &str, key: &[&str], as_of: AsOf) -> DocumentParams {
        DocumentParams {
            key: QueryKey(7),
            tag: 1,
            submitted: std::time::Instant::now(),
            dataset: dataset.into(),
            document_key: key.iter().map(|s| s.to_string()).collect(),
            as_of,
        }
    }

    fn run(store: &Store, compiled: &CompiledQuery) -> Vec<(String, f64)> {
        // term is the first axis; read (node, param) in result order.
        let mut stmt = store.writer().prepare(&compiled.sql).unwrap();
        stmt.query_map(duckdb::params_from_iter(compiled.params.iter()), |r| {
            Ok((r.get::<_, String>("term")?, r.get::<_, f64>("param")?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    #[test]
    fn a_live_document_query_selects_one_key_in_axis_order() {
        let f = fixture_with_two_generations();
        let compiled = compile_document(
            f.store.writer(),
            &f.schema,
            &params("cvi_params", &["SPX.Z"], AsOf::Live),
        )
        .unwrap();
        let names: Vec<&str> = compiled.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "underlying_ref",
                "term",
                "node",
                "param",
                "anchor_date",
                "spot_ref"
            ]
        );
        assert!(
            compiled.sql.contains("cvi_params_document_live"),
            "{}",
            compiled.sql
        );
        assert!(
            compiled
                .sql
                .to_lowercase()
                .contains("order by \"term\", \"node\""),
            "{}",
            compiled.sql
        );
        let rows = run(&f.store, &compiled);
        assert_eq!(
            rows.iter().map(|(_, p)| *p).collect::<Vec<_>>(),
            vec![10., 20., 30., 40., 50., 60.]
        );
        assert_eq!(compiled.stalest_input, vec!["cvi_params".to_string()]);
        assert!(compiled.grouping.is_empty());
        let param = compiled.columns.iter().find(|c| c.name == "param").unwrap();
        assert_eq!(
            param.attribution_by_depth,
            vec![Attribution::DeterminedNonAdditive]
        );
        assert_eq!(param.scope_semantics, ScopeSemantics::Direct);
    }

    #[test]
    fn an_as_of_document_query_reads_the_resolved_generation_from_the_archive() {
        let f = fixture_with_two_generations();
        let compiled = compile_document(
            f.store.writer(),
            &f.schema,
            &params(
                "cvi_params",
                &["SPX.Z"],
                AsOf::At(ts("2026-09-12T14:02:00Z")),
            ),
        )
        .unwrap();
        assert!(
            compiled.sql.contains("cvi_params_document_archive"),
            "{}",
            compiled.sql
        );
        assert!(
            compiled
                .sql
                .contains(&format!("gen_id = {}", f.spx_first_gen)),
            "{}",
            compiled.sql
        );
        let rows = run(&f.store, &compiled);
        assert_eq!(
            rows.iter().map(|(_, p)| *p).collect::<Vec<_>>(),
            vec![1., 2., 3., 4., 5., 6.]
        );
        assert_eq!(
            compiled.resolved_as_of.get("cvi_params").copied(),
            Some(ts("2026-09-12T14:00:00Z"))
        );
    }

    /// Task 8 review, Important #1(a): two documents exist by 14:04, at
    /// different source times, so a bug that resolved the wrong batch —
    /// or simply took the first generation `resolve_generations` handed
    /// back regardless of which document it belonged to — reads
    /// differently from the correct per-document resolution. SPX.Z's
    /// 14:05 republish has not happened yet at 14:04, so it must still
    /// resolve to its *first* generation even though NDX.Z's only
    /// generation (14:03) is newer and sorts later in `resolve_generations`'
    /// result.
    #[test]
    fn an_as_of_document_query_resolves_each_key_to_its_own_generation() {
        let f = fixture_with_two_generations();
        let at = ts("2026-09-12T14:04:00Z");

        let spx = compile_document(
            f.store.writer(),
            &f.schema,
            &params("cvi_params", &["SPX.Z"], AsOf::At(at)),
        )
        .unwrap();
        assert!(
            spx.sql.contains(&format!("gen_id = {}", f.spx_first_gen)),
            "{}",
            spx.sql
        );
        assert_eq!(
            run(&f.store, &spx)
                .iter()
                .map(|(_, p)| *p)
                .collect::<Vec<_>>(),
            vec![1., 2., 3., 4., 5., 6.],
            "SPX.Z's 14:05 republish has not happened yet at 14:04"
        );
        assert_eq!(
            spx.resolved_as_of.get("cvi_params").copied(),
            Some(ts("2026-09-12T14:00:00Z"))
        );

        let ndx = compile_document(
            f.store.writer(),
            &f.schema,
            &params("cvi_params", &["NDX.Z"], AsOf::At(at)),
        )
        .unwrap();
        assert_eq!(
            run(&f.store, &ndx)
                .iter()
                .map(|(_, p)| *p)
                .collect::<Vec<_>>(),
            vec![100., 200., 300., 400., 500., 600.],
            "NDX.Z resolves to its own generation, not SPX.Z's"
        );
        assert_eq!(
            ndx.resolved_as_of.get("cvi_params").copied(),
            Some(ts("2026-09-12T14:03:00Z"))
        );
    }

    /// Task 8 review, Important #1(b): before SPX.Z's first publish, the
    /// request must compile and return nothing (the `and false` branch)
    /// rather than error or somehow resolve NDX.Z's generation instead —
    /// and the very same key at a later instant must resolve normally,
    /// so the empty result is honestly "nothing existed yet", not a
    /// compiler stuck on the first instant it was ever asked about.
    #[test]
    fn an_as_of_before_the_first_publish_compiles_and_returns_no_rows() {
        let f = fixture_with_two_generations();
        let before = compile_document(
            f.store.writer(),
            &f.schema,
            &params(
                "cvi_params",
                &["SPX.Z"],
                AsOf::At(ts("2026-09-12T13:59:00Z")),
            ),
        )
        .unwrap();
        assert!(run(&f.store, &before).is_empty());
        assert!(!before.resolved_as_of.contains_key("cvi_params"));

        let after = compile_document(
            f.store.writer(),
            &f.schema,
            &params(
                "cvi_params",
                &["SPX.Z"],
                AsOf::At(ts("2026-09-12T14:04:00Z")),
            ),
        )
        .unwrap();
        assert!(!run(&f.store, &after).is_empty());
    }

    #[test]
    fn an_unknown_key_compiles_and_returns_no_rows() {
        let f = fixture_with_two_generations();
        let compiled = compile_document(
            f.store.writer(),
            &f.schema,
            &params("cvi_params", &["RUT.Z"], AsOf::Live),
        )
        .unwrap();
        assert!(run(&f.store, &compiled).is_empty());
    }

    #[test]
    fn the_wrong_family_or_arity_or_dataset_is_a_compile_error() {
        let mut f = fixture_with_two_generations();
        assert!(
            compile_document(
                f.store.writer(),
                &f.schema,
                &params("nonesuch", &["SPX.Z"], AsOf::Live)
            )
            .unwrap_err()
            .to_string()
            .contains("unknown dataset")
        );
        assert!(
            compile_document(
                f.store.writer(),
                &f.schema,
                &params("cvi_params", &["SPX.Z", "NDX.Z"], AsOf::Live)
            )
            .unwrap_err()
            .to_string()
            .contains("key has 2 parts")
        );
        f.schema.datasets[0].family = geode_core::schema::Family::Measures;
        assert!(
            compile_document(
                f.store.writer(),
                &f.schema,
                &params("cvi_params", &["SPX.Z"], AsOf::Live)
            )
            .unwrap_err()
            .to_string()
            .contains("not a document dataset")
        );
    }
}
