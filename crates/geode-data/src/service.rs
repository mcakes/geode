//! `DataService` — the only door to data (spec §5).
//!
//! Modules ask this and nothing else: no module opens a file, holds a
//! connection, or names a table. Everything below is an implementation
//! detail, which is what makes the future sidecar-process split an
//! evolution rather than a rewrite (§2).

use crate::query::as_of::AsOf;
use crate::query::compile::compile_view;
use crate::query::pool::{QueryId, QueryPool, QueryRequest, QueryResult, ViewId};
use crate::store::{Catalog, Store, StoreError};
use chrono::{DateTime, Utc};
use geode_core::dimensions::DerivedDimensions;
use geode_core::schema::SchemaSpec;
use geode_core::scope::Scope;
use geode_core::snapshot::{Freshness, Provenance};
use geode_core::view::ViewSpec;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

pub struct DataServiceConfig {
    pub db_path: PathBuf,
    pub schema: SchemaSpec,
    pub views: Vec<ViewSpec>,
    pub dimensions: DerivedDimensions,
    pub query_workers: usize,
}

pub struct DataService {
    config: DataServiceConfig,
    /// The store stays owned here so the database outlives the pool's
    /// connections. Never used to run a view query.
    _store: Store,
    /// A dedicated connection for compilation and catalog reads.
    conn: duckdb::Connection,
    pool: QueryPool,
    results: Receiver<QueryResult>,
}

impl DataService {
    pub fn open(config: DataServiceConfig) -> Result<DataService, StoreError> {
        let store = Store::open(&config.db_path)?;
        for ds in &config.schema.datasets {
            store.apply_schema(ds)?;
        }
        Catalog::new(store.writer()).ensure_tables()?;
        let conn = store.reader()?;
        let (pool, results) = QueryPool::spawn(&store, config.query_workers.max(1))?;
        Ok(DataService {
            config,
            _store: store,
            conn,
            pool,
            results,
        })
    }

    /// Compile and submit. Results arrive on [`Self::query_results`];
    /// a newer query for the same view supersedes an older one.
    /// `max_depth` is the deepest grouping level to materialize. Pass one
    /// more than what the tree has expanded: a single-step expand is then
    /// already in the snapshot, and only a deeper one costs a requery.
    /// Materializing everything makes the caller.s flatten walk
    /// proportional to the whole tree rather than to what is on screen.
    pub fn query(
        &self,
        view: &str,
        scope: &Scope,
        as_of: AsOf,
        max_depth: usize,
    ) -> Result<QueryId, StoreError> {
        let spec = self
            .config
            .views
            .iter()
            .find(|v| v.name == view)
            .ok_or_else(|| StoreError::Sql {
                statement: format!("query view '{view}'"),
                source: duckdb::Error::InvalidParameterName(format!("unknown view '{view}'")),
            })?;

        let compiled = compile_view(
            &self.conn,
            spec,
            &self.config.schema,
            scope,
            &self.config.dimensions,
            &as_of,
            max_depth,
        )?;

        // Freshness travels with the result, so §5.4's stalest-input rule
        // reaches the UI without every module reimplementing it.
        let mut provenance = Provenance {
            as_of_request: match &as_of {
                AsOf::Live => None,
                AsOf::At(t) => Some(t.to_rfc3339()),
            },
            ..Provenance::default()
        };
        let catalog = Catalog::new(&self.conn);
        for dataset in &compiled.stalest_input {
            // A historical result must not be labelled with today's
            // freshness. `dataset_as_of` reads the live catalog and
            // `next_gen_id` is the newest generation in the database, so
            // both describe *now* — reporting them beside an as-of result
            // inverts the very rule §5.4 exists for.
            let freshness = match &as_of {
                AsOf::Live => Freshness {
                    dataset: dataset.clone(),
                    as_of: catalog.dataset_as_of(dataset, &[])?.map(|t| t.to_rfc3339()),
                    generation: catalog.next_gen_id()?.saturating_sub(1),
                },
                AsOf::At(_) => Freshness {
                    dataset: dataset.clone(),
                    // The newest generation actually resolved, not the
                    // instant requested. Labelling every dataset with the
                    // request makes them all equal, and `stalest()` then
                    // cannot show that one side of a join is a month
                    // behind the other — which is all §5.4 is for.
                    as_of: compiled.resolved_as_of.get(dataset).map(|t| t.to_rfc3339()),
                    // Per-partition, so no single number describes it.
                    generation: 0,
                },
            };
            provenance.datasets.push(freshness);
        }

        let grouping_len = compiled.grouping.len();
        Ok(self.pool.submit(QueryRequest {
            view: ViewId(view.to_string()),
            compiled,
            grouping_len,
            provenance,
        }))
    }

    pub fn cancel(&self, view: &str) {
        self.pool.cancel(&ViewId(view.to_string()));
    }

    pub fn query_results(&self) -> &Receiver<QueryResult> {
        &self.results
    }

    /// Per-book freshness for a dataset (spec §4.5).
    pub fn freshness(&self, dataset: &str) -> Result<Vec<(String, DateTime<Utc>)>, StoreError> {
        Catalog::new(&self.conn).book_freshness(dataset)
    }

    /// How far back time travel can go, or `None` when nothing has ever
    /// been published — never a fabricated time (spec §4.6).
    ///
    /// Over the archive *and* live: a partition published once has its
    /// only generation in live, and as-of to any instant since then reads
    /// it (`Era::relation`), so the bound starts at the oldest generation
    /// anywhere rather than at the oldest one that has been superseded.
    pub fn as_of_bounds(&self, dataset: &str) -> Result<Option<DateTime<Utc>>, StoreError> {
        let Some(ds) = self.config.schema.dataset(dataset) else {
            return Ok(None);
        };
        let mut oldest: Option<DateTime<Utc>> = None;
        let tables = ds.grains().into_iter().flat_map(|grain| {
            [
                crate::store::ddl::TableKind::Archive,
                crate::store::ddl::TableKind::Live,
            ]
            .map(|kind| crate::store::ddl::table_name(dataset, grain, kind))
        });
        for table in tables {
            let sql = format!("select min(source_time) from {table}");
            let found: Option<DateTime<Utc>> = self
                .conn
                .query_row(&sql, [], |r| r.get(0))
                .map_err(|source| StoreError::Sql {
                    statement: sql,
                    source,
                })?;
            oldest = match (oldest, found) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        Ok(oldest)
    }

    pub fn shutdown(&self) {
        self.pool.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::scope::{DimensionSelection, Scope};
    use std::time::Duration;

    fn service() -> (tempfile::TempDir, tempfile::TempDir, DataService) {
        let (db, src, store, ds, emitted) = crate::ingest::load::tests_support::fixture();
        for file in emitted.files.iter().filter(|f| f.sentinel_path.is_some()) {
            let text = std::fs::read_to_string(file.sentinel_path.as_ref().unwrap()).unwrap();
            let sentinel = crate::source::parse_sentinel(&text).unwrap();
            let batch = crate::ingest::load::tests_support::batch_of(&file.csv_path);
            let _ = crate::ingest::load_file(
                &store,
                &crate::ingest::LoadRequest {
                    dataset: &ds,
                    dataset_name: "risk_snapshot",
                    csv_path: &file.csv_path,
                    sentinel: &sentinel,
                    batch: &batch,
                },
            );
        }
        drop(store);

        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let service = DataService::open(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![crate::ingest::load::tests_support::tree_view()],
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
        })
        .unwrap();
        (db, src, service)
    }

    fn next(svc: &DataService) -> QueryResult {
        svc.query_results()
            .recv_timeout(Duration::from_secs(60))
            .unwrap()
    }

    #[test]
    fn a_query_by_view_name_returns_a_snapshot() {
        let (_db, _src, svc) = service();
        svc.query("tree", &Scope::default(), AsOf::Live, usize::MAX)
            .unwrap();
        let r = next(&svc);
        let snap = r.snapshot.expect("query failed");
        assert!(snap.rows() > 0);
        svc.shutdown();
    }

    #[test]
    fn an_unknown_view_is_an_error_not_a_panic() {
        let (_db, _src, svc) = service();
        assert!(
            svc.query("nonesuch", &Scope::default(), AsOf::Live, usize::MAX)
                .is_err()
        );
        svc.shutdown();
    }

    #[test]
    fn a_scope_narrows_the_result() {
        let (_db, _src, svc) = service();
        svc.query("tree", &Scope::default(), AsOf::Live, usize::MAX)
            .unwrap();
        let all = next(&svc).snapshot.unwrap().rows();

        let scoped = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        svc.query("tree", &scoped, AsOf::Live, usize::MAX).unwrap();
        let narrowed = next(&svc).snapshot.unwrap().rows();
        assert!(narrowed < all, "{narrowed} should be fewer than {all}");
        svc.shutdown();
    }

    #[test]
    fn the_snapshot_carries_per_dataset_freshness() {
        let (_db, _src, svc) = service();
        svc.query("tree", &Scope::default(), AsOf::Live, usize::MAX)
            .unwrap();
        let snap = next(&svc).snapshot.unwrap();
        let p = snap.provenance();
        assert!(!p.datasets.is_empty(), "freshness must reach the snapshot");
        assert!(p.stalest().is_some());
        svc.shutdown();
    }

    #[test]
    fn freshness_is_reported_per_book() {
        let (_db, _src, svc) = service();
        let books = svc.freshness("risk_snapshot").unwrap();
        assert!(!books.is_empty(), "books must have freshness recorded");
        svc.shutdown();
    }

    #[test]
    fn a_historical_result_is_labelled_with_the_data_it_actually_read() {
        // Not the instant requested. Asking for today against data last
        // published a month ago must report the month-old time, or every
        // dataset carries the same value and `stalest()` — the whole
        // point of §5.4 — can no longer tell one input from another.
        let (_db, _src, svc) = service();
        // A month-old generation of a partition nothing else covers,
        // written through the service's own store. An earlier version of
        // this test opened a second `Connection` on the file — a separate
        // database instance whose writes the service never saw — and
        // `.ok()`ed an insert that failed anyway on its column count. It
        // then passed against an empty archive, vacuously.
        svc._store
            .writer()
            .execute_batch(
                "insert into risk_snapshot_position_archive
                   select * replace ('ghost' as batch, 99 as gen_id,
                                     TIMESTAMPTZ '2026-07-01 00:00:00Z' as source_time)
                   from risk_snapshot_position_live limit 1;",
            )
            .unwrap();

        let requested = chrono::DateTime::parse_from_rfc3339("2026-08-30T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        svc.query("tree", &Scope::default(), AsOf::At(requested), usize::MAX)
            .unwrap();
        let snap = next(&svc).snapshot.expect("query failed");
        let p = snap.provenance();
        assert_eq!(
            p.as_of_request.as_deref(),
            Some("2026-08-30T00:00:00+00:00"),
            "the request is recorded as the request"
        );
        assert_eq!(
            p.datasets.len(),
            1,
            "one dataset, so the stalest input is unambiguous"
        );
        assert_eq!(
            p.datasets[0].as_of.as_deref(),
            Some("2026-07-01T00:00:00+00:00"),
            "the freshness is the stalest partition read, not what was asked"
        );
        svc.shutdown();
    }

    #[test]
    fn as_of_bounds_start_at_the_oldest_generation_anywhere() {
        // A first load writes only to live — and that generation *is*
        // readable as-of any instant since it was published, so the bound
        // starts there rather than at the first superseded generation.
        let (_db, _src, svc) = service();
        let oldest_live = svc
            .freshness("risk_snapshot")
            .unwrap()
            .into_iter()
            .map(|(_, t)| t)
            .min()
            .expect("the fixture loaded something");
        assert_eq!(
            svc.as_of_bounds("risk_snapshot").unwrap(),
            Some(oldest_live)
        );
        assert!(svc.as_of_bounds("nonesuch").unwrap().is_none());
        svc.shutdown();
    }

    #[test]
    fn shutdown_is_idempotent() {
        let (_db, _src, svc) = service();
        svc.shutdown();
        svc.shutdown();
    }
}
