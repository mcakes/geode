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
use geode_core::document::{KEY_SEPARATOR, is_key_prefix, join_key};
use geode_core::query::{AsOf, DocumentParams};
use geode_core::schema::{ColumnRole, SchemaSpec};
use std::collections::BTreeMap;

fn invalid(msg: String) -> StoreError {
    StoreError::Document(msg)
}

/// Compile a document request against its dataset's document tables.
///
/// Reads the document named by `document_key`, or every document under it
/// when the key is a shorter prefix, ordered by the open key parts then
/// axes. Historical reads resolve each matched document's generation and pin
/// that set (`gen_id = N` for one, `gen_id in (…)` for several) across the
/// live/archive union. An instant before any matched document's first
/// retained generation produces an empty result.
///
/// For historical reads, the oldest matched source time travels with the
/// compiled query for provenance, and a generation ID only when exactly one
/// document matched. Documents outside the key do not affect either value.
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
    let arity = ds.key.len();
    let given = params.document_key.len();
    if given == 0 {
        return Err(invalid(
            "a document key needs at least one part".to_string(),
        ));
    }
    if given > arity {
        return Err(invalid(format!(
            "key has {given} parts, dataset '{}' declares {arity}",
            ds.name
        )));
    }
    // A part holding the separator would join to another key's batch, so
    // freshness and the as-of match would describe a document the column
    // predicate does not select.
    if let Some(part) = params
        .document_key
        .iter()
        .find(|p| p.contains(KEY_SEPARATOR))
    {
        return Err(invalid(format!(
            "key part {part:?} contains the reserved separator"
        )));
    }

    let columns = ds.document_columns();
    let projection = columns
        .iter()
        .map(|c| format!("\"{}\"", c.name))
        .collect::<Vec<_>>()
        .join(", ");
    // Order by the key parts the request leaves open, then the axes, so a
    // multi-document result is grouped per document. The given parts are
    // constant across the result, so a full key orders by the axes alone.
    let order = ds.key[given..]
        .iter()
        .chain(ds.axes.iter())
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let key_predicate = ds.key[..given]
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
            let prefix = join_key(&params.document_key);
            let matched: Vec<_> = resolve_generations(conn, &ds.name, *t)?
                .into_iter()
                .filter(|g| is_key_prefix(&prefix, &g.batch))
                .collect();
            // The selected generation may still be live: publication archives only
            // the outgoing generation. Read both tables so historical requests can
            // select the latest retained document too.
            let relation = format!(
                "(select * from {} union all select * from {})",
                tables.live, tables.archive
            );
            if matched.is_empty() {
                // No document under this key existed by `t`: a request that
                // compiles and returns nothing, not an error — the same shape a
                // live query answers an unknown key with.
                (relation, " and false".to_string())
            } else {
                // A multi-document result reports its oldest source time, as a
                // historical view does; a generation ID only when one document
                // matched, since no single ID identifies several.
                let oldest = matched
                    .iter()
                    .map(|g| g.source_time)
                    .min()
                    .expect("matched is not empty");
                resolved_as_of.insert(ds.name.clone(), oldest);
                if let [only] = matched.as_slice() {
                    resolved_generation = Some(only.gen_id);
                }
                // Document publication allocates each generation from the store
                // sequence and writes one key per publish, so the ID set together
                // with the key predicate pins exactly the resolved documents.
                let pin = match matched.as_slice() {
                    [only] => format!(" and gen_id = {}", only.gen_id),
                    _ => {
                        let ids = matched
                            .iter()
                            .map(|g| g.gen_id.to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(" and gen_id in ({ids})")
                    }
                };
                (relation, pin)
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
            mixed_flag: None,
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
                compare_live: false,
                topic: None,
            },
        )
        .unwrap()
        .published()
        .expect("an uncompared document publishes");
        publish_document(
            &store,
            &DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("NDX.Z", [100., 200., 300., 400., 500., 600.]),
                source_time: ts("2026-09-12T14:03:00Z"),
                received_at: ts("2026-09-12T14:03:00Z"),
                bytes: 0,
                compare_live: false,
                topic: None,
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
                compare_live: false,
                topic: None,
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

    /// SPX has two expiries published at 14:00 and 14:05; SPXW (a string
    /// extension of SPX) has one at 14:02; SPX's October expiry is
    /// republished at 14:10; SPXW's is republished at 14:12, so SPXW holds
    /// both the newest source time and the greatest generation.
    fn chain_fixture() -> (tempfile::TempDir, Store, SchemaSpec) {
        use crate::store::ddl::tests_support::{chain_dataset, chain_doc};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = chain_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        for (u, e, mids, at) in [
            ("SPX", "2026-10-16", [0.20, 0.18], "2026-09-12T14:00:00Z"),
            ("SPXW", "2026-10-16", [0.50, 0.50], "2026-09-12T14:02:00Z"),
            ("SPX", "2026-11-20", [0.22, 0.19], "2026-09-12T14:05:00Z"),
            ("SPX", "2026-10-16", [0.21, 0.17], "2026-09-12T14:10:00Z"),
            ("SPXW", "2026-10-16", [0.51, 0.51], "2026-09-12T14:12:00Z"),
        ] {
            publish_document(
                &store,
                &DocumentPublishRequest {
                    dataset: &ds,
                    source: "opra_sim",
                    rows: &chain_doc(u, e, mids),
                    source_time: ts(at),
                    received_at: ts(at),
                    bytes: 0,
                    compare_live: false,
                    topic: None,
                },
            )
            .unwrap();
        }
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        (dir, store, schema)
    }

    /// `(expiry, strike, mid_vol)` in result order.
    fn run_chain(store: &Store, compiled: &CompiledQuery) -> Vec<(String, f64, f64)> {
        let mut stmt = store.writer().prepare(&compiled.sql).unwrap();
        stmt.query_map(duckdb::params_from_iter(compiled.params.iter()), |r| {
            Ok((
                r.get::<_, String>("expiry")?,
                r.get::<_, f64>("strike")?,
                r.get::<_, f64>("mid_vol")?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    #[test]
    fn a_prefix_query_returns_every_document_under_it_in_key_then_axis_order() {
        let (_d, store, schema) = chain_fixture();
        let compiled = compile_document(
            store.writer(),
            &schema,
            &params("option_chain", &["SPX"], AsOf::Live),
        )
        .unwrap();
        assert_eq!(
            run_chain(&store, &compiled),
            vec![
                ("2026-10-16".into(), 90.0, 0.21),
                ("2026-10-16".into(), 110.0, 0.17),
                ("2026-11-20".into(), 90.0, 0.22),
                ("2026-11-20".into(), 110.0, 0.19),
            ]
        );
    }

    #[test]
    fn a_full_key_on_a_two_part_dataset_still_reads_one_document() {
        let (_d, store, schema) = chain_fixture();
        let compiled = compile_document(
            store.writer(),
            &schema,
            &params("option_chain", &["SPX", "2026-11-20"], AsOf::Live),
        )
        .unwrap();
        assert_eq!(
            run_chain(&store, &compiled),
            vec![
                ("2026-11-20".into(), 90.0, 0.22),
                ("2026-11-20".into(), 110.0, 0.19)
            ]
        );
    }

    #[test]
    fn a_prefix_never_matches_a_longer_underlying() {
        let (_d, store, schema) = chain_fixture();
        for as_of in [AsOf::Live, AsOf::At(ts("2026-09-12T14:30:00Z"))] {
            let is_at = matches!(as_of, AsOf::At(_));
            let compiled = compile_document(
                store.writer(),
                &schema,
                &params("option_chain", &["SPX"], as_of),
            )
            .unwrap();
            assert!(
                run_chain(&store, &compiled)
                    .iter()
                    .all(|(_, _, mid)| *mid < 0.5),
                "SPXW's 0.5 rows leaked into an SPX read"
            );
            if is_at {
                // SPX October at 14:10 and November at 14:05: the oldest.
                assert_eq!(
                    compiled.resolved_as_of.get("option_chain"),
                    Some(&ts("2026-09-12T14:05:00Z"))
                );
            }
        }
        let cat = Catalog::new(store.writer());
        let spx = join_key(&["SPX".to_string()]);
        // SPX October's newest is 14:10, November's 14:05: the stalest.
        assert_eq!(
            cat.live_source_time_under("option_chain", &spx).unwrap(),
            Some(ts("2026-09-12T14:05:00Z")),
            "SPX's freshness is its stalest expiry's, and SPXW is not under SPX"
        );
        let spxw = join_key(&["SPXW".to_string()]);
        assert_eq!(
            cat.live_source_time_under("option_chain", &spxw).unwrap(),
            Some(ts("2026-09-12T14:12:00Z"))
        );
        let spx_gen = cat.live_generation_under("option_chain", &spx).unwrap();
        let spxw_gen = cat.live_generation_under("option_chain", &spxw).unwrap();
        assert!(
            spx_gen < spxw_gen,
            "SPXW's greater generation leaked into SPX's: {spx_gen:?} vs {spxw_gen:?}"
        );
    }

    #[test]
    fn an_as_of_prefix_read_returns_each_document_that_existed_then() {
        let (_d, store, schema) = chain_fixture();
        // 14:03: SPX October's first generation exists; November does not yet.
        let compiled = compile_document(
            store.writer(),
            &schema,
            &params(
                "option_chain",
                &["SPX"],
                AsOf::At(ts("2026-09-12T14:03:00Z")),
            ),
        )
        .unwrap();
        assert_eq!(
            run_chain(&store, &compiled),
            vec![
                ("2026-10-16".into(), 90.0, 0.20),
                ("2026-10-16".into(), 110.0, 0.18)
            ]
        );
        assert!(
            compiled.resolved_generation.is_some(),
            "exactly one document matched"
        );
        // 14:07: both exist, October still at its first generation.
        let compiled = compile_document(
            store.writer(),
            &schema,
            &params(
                "option_chain",
                &["SPX"],
                AsOf::At(ts("2026-09-12T14:07:00Z")),
            ),
        )
        .unwrap();
        assert_eq!(
            run_chain(&store, &compiled),
            vec![
                ("2026-10-16".into(), 90.0, 0.20),
                ("2026-10-16".into(), 110.0, 0.18),
                ("2026-11-20".into(), 90.0, 0.22),
                ("2026-11-20".into(), 110.0, 0.19),
            ]
        );
        assert_eq!(
            compiled.resolved_generation, None,
            "two documents: no single generation"
        );
        assert_eq!(
            compiled.resolved_as_of.get("option_chain"),
            Some(&ts("2026-09-12T14:00:00Z")),
            "the oldest resolved source time"
        );
    }

    #[test]
    fn prefix_freshness_is_the_stalest_source_time_and_greatest_generation_under_it() {
        let (_d, store, _schema) = chain_fixture();
        let cat = Catalog::new(store.writer());
        let spx = join_key(&["SPX".to_string()]);
        let oct = join_key(&["SPX".to_string(), "2026-10-16".to_string()]);
        let nov = join_key(&["SPX".to_string(), "2026-11-20".to_string()]);
        let greatest = [&oct, &nov]
            .iter()
            .filter_map(|b| cat.live_generation("option_chain", b, None).unwrap())
            .max();
        assert_eq!(
            cat.live_generation_under("option_chain", &spx).unwrap(),
            greatest
        );
        // Each expiry's own newest source time; the prefix reports the older.
        let stalest = [&oct, &nov]
            .iter()
            .filter_map(|b| cat.live_source_time("option_chain", b, None).unwrap())
            .min();
        assert!(stalest.is_some());
        assert_eq!(
            cat.live_source_time_under("option_chain", &spx).unwrap(),
            stalest
        );
        // A full key's `_under` answer equals the exact-batch answer.
        assert_eq!(
            cat.live_generation_under("option_chain", &nov).unwrap(),
            cat.live_generation("option_chain", &nov, None).unwrap()
        );
        assert_eq!(
            cat.live_source_time_under("option_chain", &oct).unwrap(),
            cat.live_source_time("option_chain", &oct, None).unwrap()
        );
    }

    #[test]
    fn an_empty_key_is_refused() {
        let (_d, store, schema) = chain_fixture();
        let err = compile_document(
            store.writer(),
            &schema,
            &params("option_chain", &[], AsOf::Live),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("a document key needs at least one part"),
            "{err}"
        );
    }

    /// A part holding the separator would join to another key's batch:
    /// `["SPX␟2026-10-16"]` matches no rows by column yet names SPX
    /// October's partition for freshness and the as-of prefix match.
    #[test]
    fn a_key_part_holding_the_separator_is_refused() {
        let (_d, store, schema) = chain_fixture();
        let joined = format!("SPX{KEY_SEPARATOR}2026-10-16");
        for key in [vec![joined.as_str()], vec!["SPX", joined.as_str()]] {
            let err = compile_document(
                store.writer(),
                &schema,
                &params("option_chain", &key, AsOf::Live),
            )
            .unwrap_err()
            .to_string();
            assert!(
                err.contains(&format!(
                    "key part {joined:?} contains the reserved separator"
                )),
                "{err}"
            );
        }
    }
}
