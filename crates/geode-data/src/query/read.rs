//! Database-dependent planning, freshness and execution on one worker snapshot.
//! Requests retain their configuration revision while waiting in the pool.

use std::sync::Arc;

use duckdb::Connection;
use geode_core::dimensions::DerivedDimensions;
use geode_core::document::join_key;
use geode_core::query::{AsOf, DistinctParams, DocumentParams};
use geode_core::schema::SchemaSpec;
use geode_core::snapshot::{Freshness, Provenance, Snapshot};
use geode_core::view::ViewSpec;

use super::document::compile_document;
use super::{compile_distinct, compile_view};
use crate::store::{Catalog, StoreError, begin_transaction, commit_transaction};
use geode_core::scope::Scope;

#[derive(Debug)]
pub(crate) struct ReadConfig {
    pub schema: Arc<SchemaSpec>,
    pub dimensions: DerivedDimensions,
}

#[derive(Debug)]
enum Kind {
    View(Box<ViewSpec>, Scope, AsOf, usize),
    Distinct(DistinctParams),
    Document(DocumentParams),
}

#[derive(Debug)]
pub struct ReadQuery {
    config: Arc<ReadConfig>,
    kind: Kind,
}

impl ReadQuery {
    pub(crate) fn view(
        config: Arc<ReadConfig>,
        view: ViewSpec,
        scope: Scope,
        as_of: AsOf,
        max_depth: usize,
    ) -> Self {
        Self {
            config,
            kind: Kind::View(Box::new(view), scope, as_of, max_depth),
        }
    }

    pub(crate) fn distinct(config: Arc<ReadConfig>, params: DistinctParams) -> Self {
        Self {
            config,
            kind: Kind::Distinct(params),
        }
    }

    pub(crate) fn document(config: Arc<ReadConfig>, params: DocumentParams) -> Self {
        Self {
            config,
            kind: Kind::Document(params),
        }
    }

    pub(crate) fn run(&self, conn: &Connection) -> Result<Snapshot, StoreError> {
        self.run_with(conn, || {})
    }

    // The hook lets concurrency tests commit a writer after planning, without
    // sleeps or races. Production monomorphizes it to a no-op.
    fn run_with(
        &self,
        conn: &Connection,
        after_plan: impl FnOnce(),
    ) -> Result<Snapshot, StoreError> {
        let tx = begin_transaction(conn)?;
        let catalog = Catalog::new(&tx);
        let (compiled, provenance) = match &self.kind {
            Kind::View(view, scope, as_of, max_depth) => {
                let compiled = compile_view(
                    &tx,
                    view,
                    &self.config.schema,
                    scope,
                    &self.config.dimensions,
                    as_of,
                    *max_depth,
                )?;
                let mut provenance = provenance(as_of);
                for dataset in &compiled.stalest_input {
                    let freshness = match as_of {
                        AsOf::Live => Freshness {
                            dataset: dataset.clone(),
                            as_of: catalog.dataset_as_of(dataset, &[])?.map(|t| t.to_rfc3339()),
                            generation: catalog.dataset_generation(dataset)?,
                        },
                        // Historical views resolve each partition independently;
                        // no single generation ID identifies the whole result.
                        AsOf::At(_) => Freshness {
                            dataset: dataset.clone(),
                            as_of: compiled.resolved_as_of.get(dataset).map(|t| t.to_rfc3339()),
                            generation: None,
                        },
                    };
                    provenance.datasets.push(freshness);
                }
                (compiled, provenance)
            }
            Kind::Distinct(params) => (
                compile_distinct(&tx, &self.config.schema, &self.config.dimensions, params)?,
                Provenance::default(),
            ),
            Kind::Document(params) => {
                let compiled = compile_document(&tx, &self.config.schema, params)?;
                let freshness = match &params.as_of {
                    AsOf::Live => Freshness {
                        dataset: params.dataset.clone(),
                        as_of: catalog
                            .live_source_time(
                                &params.dataset,
                                &join_key(&params.document_key),
                                None,
                            )?
                            .map(|t| t.to_rfc3339()),
                        generation: catalog.live_generation(
                            &params.dataset,
                            &join_key(&params.document_key),
                            None,
                        )?,
                    },
                    AsOf::At(_) => Freshness {
                        dataset: params.dataset.clone(),
                        as_of: compiled
                            .resolved_as_of
                            .get(&params.dataset)
                            .map(|t| t.to_rfc3339()),
                        // The document compiler pinned exactly one
                        // generation; report the one it read.
                        generation: compiled.resolved_generation,
                    },
                };
                let mut provenance = provenance(&params.as_of);
                provenance.datasets.push(freshness);
                (compiled, provenance)
            }
        };
        after_plan();
        let snapshot = super::pool::run_snapshot(&tx, &compiled, &compiled.grouping, provenance)
            .map_err(|source| StoreError::Sql {
                statement: compiled.sql,
                source,
            })?;
        commit_transaction(tx)?;
        Ok(snapshot)
    }
}

fn provenance(as_of: &AsOf) -> Provenance {
    Provenance {
        as_of_request: match as_of {
            AsOf::Live => None,
            AsOf::At(t) => Some(t.to_rfc3339()),
        },
        ..Provenance::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::store::ddl::tests_support::{cvi_dataset, cvi_doc, ts};
    use crate::store::document::{DocumentPublishRequest, publish_document};
    use geode_core::query::QueryKey;
    use std::time::Instant;

    #[test]
    fn document_rows_and_freshness_share_the_planning_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("read.duckdb")).unwrap();
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let publish = |value, at| {
            publish_document(
                &store,
                &DocumentPublishRequest {
                    dataset: &ds,
                    source: "test",
                    rows: &cvi_doc("SPX.Z", [value; 6]),
                    source_time: ts(at),
                    received_at: ts(at),
                    bytes: 0,
                },
            )
            .unwrap();
        };
        publish(1., "2026-09-12T14:00:00Z");
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds.clone());
        let query = ReadQuery::document(
            Arc::new(ReadConfig {
                schema: Arc::new(schema),
                dimensions: DerivedDimensions::default(),
            }),
            DocumentParams {
                key: QueryKey(1),
                tag: 1,
                submitted: Instant::now(),
                dataset: ds.name.clone(),
                document_key: vec!["SPX.Z".into()],
                as_of: AsOf::Live,
            },
        );
        let reader = store.reader().unwrap();
        let first = query
            .run_with(&reader, || publish(2., "2026-09-12T14:05:00Z"))
            .unwrap();
        let col = first.column_index("param").unwrap();
        assert_eq!(first.f64_at(col, 0), Some(1.));
        assert_eq!(
            first.provenance().datasets[0].as_of.as_deref(),
            Some("2026-09-12T14:00:00+00:00")
        );
        let next = query.run(&reader).unwrap();
        assert_eq!(next.f64_at(col, 0), Some(2.));
        assert_eq!(
            next.provenance().datasets[0].as_of.as_deref(),
            Some("2026-09-12T14:05:00+00:00")
        );
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = query.run_with(&reader, || panic!("planning hook failed"));
        }));
        assert!(panic.is_err());
        assert!(
            query.run(&reader).is_ok(),
            "an unwind releases the read transaction"
        );

        let Kind::Document(mut params) = query.kind else {
            unreachable!()
        };
        params.as_of = AsOf::At(ts("2026-09-12T14:01:00Z"));
        let historical = ReadQuery::document(query.config, params);
        let before_eviction = historical.run_with(&reader, || {
            store.writer().execute_batch("begin; delete from cvi_params_document_archive; delete from generations where source_time < '2026-09-12T14:05:00Z'; commit;").unwrap();
        }).unwrap();
        assert_eq!(before_eviction.f64_at(col, 0), Some(1.));
        assert_eq!(historical.run(&reader).unwrap().rows(), 0);
    }

    /// A corrected republish keeps its source time and takes a new
    /// generation. A reader holding unsent work over the older one can only
    /// tell them apart if provenance says so.
    #[test]
    fn a_republish_at_the_same_source_time_reports_a_different_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("gen.duckdb")).unwrap();
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let publish = |value: f64| {
            publish_document(
                &store,
                &DocumentPublishRequest {
                    dataset: &ds,
                    source: "test",
                    rows: &cvi_doc("SPX.Z", [value; 6]),
                    source_time: ts("2026-09-12T14:00:00Z"),
                    received_at: ts("2026-09-12T14:00:00Z"),
                    bytes: 0,
                },
            )
            .unwrap();
        };
        publish(1.);
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds.clone());
        let config = Arc::new(ReadConfig {
            schema: Arc::new(schema),
            dimensions: DerivedDimensions::default(),
        });
        let params = DocumentParams {
            key: QueryKey(1),
            tag: 1,
            submitted: Instant::now(),
            dataset: ds.name.clone(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        };
        let live = ReadQuery::document(Arc::clone(&config), params.clone());
        let reader = store.reader().unwrap();
        let first = live.run(&reader).unwrap();
        publish(2.);
        let second = live.run(&reader).unwrap();

        let (a, b) = (
            &first.provenance().datasets[0],
            &second.provenance().datasets[0],
        );
        assert_eq!(a.as_of, b.as_of, "the republish kept its source time");
        assert!(
            a.generation.is_some() && b.generation.is_some(),
            "a live document read names the generation it read"
        );
        assert_ne!(
            a.generation, b.generation,
            "a same-time republish must be distinguishable from its predecessor"
        );

        // Historical provenance identifies the selected document generation.
        let mut at = params;
        at.as_of = AsOf::At(ts("2026-09-12T14:30:00Z"));
        let historical = ReadQuery::document(config, at);
        assert_eq!(
            historical.run(&reader).unwrap().provenance().datasets[0].generation,
            b.generation,
            "as-of at a time after both publishes resolves the newer generation"
        );
    }
}
