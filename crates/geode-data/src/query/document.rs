//! Read one document by key, live or at a historical instant, in axis order.
//! The key supplies the predicate; document reads have no scope, aggregation,
//! or rollup tree.

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

/// Compile a document request against its dataset's document tables.
///
/// Live reads select by key from the live table. Historical reads resolve the
/// generation summary, select the partition named by [`join_key`], and pin
/// its generation with `gen_id = N` across the live/archive union. An instant
/// before that document's first retained generation produces an empty result.
///
/// For historical reads, the selected source time and generation ID travel
/// with the compiled query for provenance. Other documents in the dataset do
/// not affect either value.
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
    let mut resolved_generation: Option<i64> = None;
    let (table, era) = match &params.as_of {
        AsOf::Live => (tables.live.clone(), String::new()),
        AsOf::At(t) => {
            let batch = join_key(&params.document_key);
            let gens = resolve_generations(conn, &ds.name, *t)?;
            let resolved = gens.into_iter().find(|g| g.batch == batch);
            // The selected generation may still be live: publication archives only
            // the outgoing generation. Read both tables so historical requests can
            // select the latest retained document too.
            let relation = format!(
                "(select * from {} union all select * from {})",
                tables.live, tables.archive
            );
            match resolved {
                Some(g) => {
                    resolved_as_of.insert(ds.name.clone(), g.source_time);
                    resolved_generation = Some(g.gen_id);
                    // Document publication allocates each generation from the
                    // store sequence and writes one key per publish. Together
                    // with the key predicate, this pins the selected document.
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
            summable: false,
        })
        .collect();

    Ok(CompiledQuery {
        sql,
        params: sql_params,
        // Document snapshots have no rollup tree and contain only depth 0.
        grouping: Vec::new(),
        columns: compiled_columns,
        stalest_input: vec![ds.name.clone()],
        resolved_as_of,
        resolved_generation,
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

    /// SPX.Z has generations at 14:00 and 14:05; NDX.Z has one at 14:03.
    /// Different keys and times expose resolution that selects another
    /// document's generation or reports dataset-wide freshness.
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

    /// At 14:04, each document resolves to its own generation. SPX.Z still
    /// selects 14:00 even though NDX.Z has a newer generation at 14:03.
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

    /// A request before the first publish returns no rows; the same key at
    /// a later instant resolves normally.
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
