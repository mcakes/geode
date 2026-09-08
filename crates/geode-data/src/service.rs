//! `DataService` — the only door to data (spec §5).
//!
//! Modules ask this and nothing else: no module opens a file, holds a
//! connection, or names a table. Everything below is an implementation
//! detail, which is what makes the future sidecar-process split an
//! evolution rather than a rewrite (§2).

use crate::health::Health;
use crate::ingest::scheduler::{Scheduler, SchedulerEvent, SchedulerSink};
use crate::ingest::{IngestEvent, IngestHandle, IngestRunner, IngestSink};
use crate::query::as_of::AsOf;
use crate::query::catalog::build_catalog;
use crate::query::compile::compile_view;
use crate::query::distinct::compile_distinct;
use crate::query::pool::{
    QueryId, QueryPool, QueryRequest, QueryResult, RequestKind, ResultSink, ViewId,
};
use crate::source::SourceSpec;
use crate::store::catalog::BookFreshness;
use crate::store::{Catalog, Store, StoreError};
use chrono::{DateTime, Utc};
use geode_core::config::Diagnostic;
use geode_core::dimensions::DerivedDimensions;
use geode_core::query::{
    CatalogOutcome, CatalogParams, DistinctOutcome, DistinctParams, QueryKey, QueryOutcome,
};
use geode_core::schema::SchemaSpec;
use geode_core::scope::Scope;
use geode_core::snapshot::{Freshness, Provenance};
use geode_core::view::ViewSpec;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime};

pub struct DataServiceConfig {
    pub db_path: PathBuf,
    pub schema: SchemaSpec,
    pub views: Vec<ViewSpec>,
    pub dimensions: DerivedDimensions,
    pub query_workers: usize,
    /// Configured sources (spec §5.2). Empty means nothing is ever
    /// ingested — a warm database is queried as it stands.
    pub sources: Vec<SourceSpec>,
}

/// Everything the service produces, on one channel (spec §5.1).
#[derive(Debug)]
pub enum DataEvent {
    Query(QueryOutcome),
    /// The picker's distinct-values result (spec §3.4).
    Distinct(DistinctOutcome),
    /// The diagnostics tile's "what does the database hold" result
    /// (spec §4.5).
    Catalog(CatalogOutcome),
    /// A file was published: the frame bumps its data generation and every
    /// visible tile requeries. A burst coalesces there.
    Published {
        dataset: String,
        batch: String,
        gen_id: i64,
        books: Vec<Option<String>>,
    },
    /// The worst state discovery found for a source on its last poll.
    Health {
        source: String,
        worst: Health,
        detail: String,
    },
    /// One source's poll finished — Phase 4b §4.4's "last and next poll"
    /// diagnostic. `next` is `at + spec.poll_interval` at the moment of
    /// this poll, not a live countdown; a later poll's own `Polled`
    /// supersedes it.
    Polled {
        source: String,
        ready: usize,
        at: SystemTime,
        next: SystemTime,
    },
    /// Config problems found at open or on a view reload (§10.1).
    Diagnostics(Vec<Diagnostic>),
}

/// Where events go. `false` means nobody is listening.
///
/// Called synchronously from inside the query pool's worker delivery
/// site, which holds the pool's queue lock (see `pool::ResultSink`), so
/// an `EventSink` must not block and must not call back into
/// `DataService`: a channel `send`/`try_send` is fine, a call into
/// `DataService::query` or `cancel` from inside the sink is not.
///
/// The caller owns the outbound channel this closes over, and that
/// channel must be bounded (spec §7.3) and fed with `try_send` — never
/// `send` — so a slow or gone receiver cannot block the query worker
/// that calls this. A refused event is counted and surfaced as a
/// diagnostic on the caller's side; the service itself never blocks on
/// delivery and never retries one.
pub type EventSink = Arc<dyn Fn(DataEvent) -> bool + Send + Sync>;

/// One query, as a module asks for it.
#[derive(Debug, Clone)]
pub struct QueryParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub view: String,
    /// Replaces the named view's own grouping for this query: the
    /// frame's active slot or a tile's pin (spec §5.1). `None` keeps the
    /// view's.
    pub grouping: Option<Vec<String>>,
    pub scope: Scope,
    pub as_of: AsOf,
    pub max_depth: usize,
}

/// `SchedulerEvent::Polled` -> `DataEvent::Polled` (Phase 4b §4.4's
/// last/next-poll diagnostic), extracted as a pure free function
/// (review round 1 MAJ-4) so the `next = at + next_in` arithmetic is
/// unit-testable without spinning up a real scheduler thread — nothing
/// in the suite otherwise ever observes `DataEvent::Polled` at all,
/// since `service()`'s test fixture runs with `sources: Vec::new()`.
///
/// `checked_add` with a saturating fallback (MIN-7), not `at + next_in`
/// directly: `poll_interval` is user-configured
/// (`source::config::parse_duration`) and unbounded in magnitude, so a
/// pathological config value must not panic the request loop over a
/// diagnostic nobody asked to see fail.
fn polled_event(source: String, ready: usize, at: SystemTime, next_in: Duration) -> DataEvent {
    DataEvent::Polled {
        source,
        ready,
        at,
        next: at.checked_add(next_in).unwrap_or(at),
    }
}

/// Logs one `IngestEvent::Failed` at `geode::ingest` `error` (MIN-4): a
/// file that did not load, named by dataset and batch, with the reason.
/// A free function for the same testability reason as
/// [`log_health_event`] just below.
fn log_ingest_failure(dataset: &str, batch: &str, reason: &str) {
    tracing::error!(target: "geode::ingest", "{dataset}/{batch}: {reason}");
}

/// Logs one `Health` outcome at `geode::ingest`, leveled by what actually
/// happened (Phase 4b Task 2 fix round 1, MAJ-1): `Failed` lost data or a
/// working source, so it's `error`; `Degraded`/`PendingTooLong` are
/// notable but not a loss, so `warn`; `Ok`/`Pending` are routine, so
/// `info`/`debug`. The one choke point every `SchedulerEvent::Health`
/// passes through (`scheduler_sink` below) — kept as a free function so
/// a test can drive it directly against a scoped ring subscriber without
/// standing up a real `Scheduler`.
fn log_health_event(source: &str, worst: &Health, detail: &str) {
    match worst {
        Health::Failed { .. } => {
            tracing::error!(target: "geode::ingest", "{source}: {} — {detail}", worst.label());
        }
        Health::Degraded { .. } | Health::PendingTooLong => {
            tracing::warn!(target: "geode::ingest", "{source}: {} — {detail}", worst.label());
        }
        Health::Ok => {
            tracing::info!(target: "geode::ingest", "{source}: {} — {detail}", worst.label());
        }
        Health::Pending => {
            tracing::debug!(target: "geode::ingest", "{source}: {} — {detail}", worst.label());
        }
    }
}

/// One source's last REPORTED health, shared between the ingest sink and
/// the scheduler sink (final review round 2, NEW-1) — MAJ-3 added a
/// second writer of source health (a publish) that MAJ-2's
/// scheduler-only transition tracker knew nothing about: a degraded
/// publish set `SourceState.health` to `Degraded`, but the scheduler's
/// own `last_reported` (keyed and consulted only inside `ingest::
/// scheduler::run`) had no idea a publish had happened, so its next
/// clean poll saw its own state as already `Some(Ok)` and suppressed the
/// event that would have cleared it — and the publish path's own guard
/// (`health == Health::Ok` never sent) meant a later clean republish
/// couldn't clear it either. `Health::Ok` was reachable in general
/// (MAJ-2), but a source that had ever had one degraded publish stayed
/// latched at `degraded` for the rest of the session — MAJ-2's exact
/// failure mode, through the path MAJ-3 opened.
///
/// One tracker, owned by the service and shared by both sinks, closes
/// the gap: every report from EITHER path goes through the same
/// `report`, so whichever next reports `Ok` — a scheduler poll or a
/// publish — is the one that clears it, and a report that repeats the
/// last one recorded (from either path) is suppressed regardless of
/// which path sent it.
#[derive(Default)]
struct HealthTracker {
    last: std::sync::Mutex<std::collections::HashMap<String, Health>>,
}

impl HealthTracker {
    /// `Some(health)` exactly on a transition for `source` — the first
    /// report ever, or one that differs from the last reported (`Health`
    /// derives `PartialEq` down to `Degraded`/`Failed`'s own `reason`, so
    /// a changed reason at the same severity still counts as a
    /// transition). `None` when it repeats the last reported state,
    /// meaning the caller must not forward it.
    fn report(&self, source: &str, health: Health) -> Option<Health> {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.get(source) == Some(&health) {
            return None;
        }
        last.insert(source.to_string(), health.clone());
        Some(health)
    }
}

pub struct DataService {
    config: DataServiceConfig,
    /// Config errors found at open (spec §10.1). Held rather than
    /// returned so `open` keeps its signature and a caller that does not
    /// surface diagnostics still gets a working service.
    diagnostics: Vec<Diagnostic>,
    /// Field order is drop order. The pool joins its workers first; the
    /// scheduler stops submitting next; the runner drains its queue and
    /// drops the `Store` before `conn` — the field listed last — drops
    /// after everything else.
    ///
    /// `conn` dropping last is harmless, not accidental correctness:
    /// duckdb-rs holds the database as `Arc<Mutex<DatabaseHandle>>`, and
    /// `conn` is a `try_clone` of that same handle, so `duckdb_close`
    /// only runs when the *last* reference goes, whichever field that
    /// happens to be — dropping `conn` before the `Store` would just
    /// issue one `duckdb_disconnect` and close nothing. An earlier
    /// version of this comment wrongly called a different drop order a
    /// live bug on the strength of this same detail.
    pool: QueryPool,
    scheduler: Scheduler,
    ingest: Arc<IngestHandle>,
    /// A dedicated read connection for compilation and catalog reads.
    conn: duckdb::Connection,
}

impl DataService {
    pub fn open(config: DataServiceConfig, sink: EventSink) -> Result<DataService, StoreError> {
        let store = Store::open(&config.db_path)?;
        for ds in &config.schema.datasets {
            store.apply_schema(ds)?;
        }
        Catalog::new(store.writer()).ensure_tables()?;

        // Migration: a database written before `generations` existed has
        // real rows in its data tables and nothing in the summary. Rebuild
        // it once, here, per dataset -- but only when the summary is
        // actually empty for that dataset *and* at least one of its
        // tables holds data, so a dataset that has simply never been
        // published to is not given a spurious rebuild, and a database
        // whose summary is already maintained is never rebuilt merely
        // because it was opened (`rebuild_generations` is destructive: it
        // deletes the dataset's rows before reinserting, so running it
        // unconditionally would make the summary just a cache of the last
        // open rather than a maintained record).
        //
        // This can only ever notice an *absent* summary, never a
        // *wrong* one (review round 1, MIN-2) -- a database from a build
        // that had the table and a maintenance bug, or a hand-edited row,
        // looks "already populated" and is left alone, by design: a full
        // comparison at open is exactly the scan this table exists to
        // remove. There is no palette command or flag for this today; the
        // manual repair is `delete from generations where dataset = ?`
        // for the affected dataset, then reopen -- the next `open` call
        // finds an empty summary and rebuilds it from the tables, same as
        // the first-time migration below.
        for ds in &config.schema.datasets {
            let tables = crate::store::ddl::history_of(&ds.name, ds);
            let summarised: i64 = {
                let sql = "select count(*) from generations where dataset = ?";
                store
                    .writer()
                    .query_row(sql, duckdb::params![&ds.name], |r| r.get(0))
                    .map_err(|source| StoreError::Sql {
                        statement: sql.to_string(),
                        source,
                    })?
            };
            if summarised > 0 {
                continue;
            }
            let mut has_data = false;
            for t in &tables {
                let sql = format!("select exists(select 1 from {t})");
                let exists: bool =
                    store
                        .writer()
                        .query_row(&sql, [], |r| r.get(0))
                        .map_err(|source| StoreError::Sql {
                            statement: sql,
                            source,
                        })?;
                if exists {
                    has_data = true;
                    break;
                }
            }
            if has_data {
                crate::store::ddl::rebuild_generations(store.writer(), &ds.name, &tables)?;
            }
        }

        // Every reader the service will ever need is cloned before the
        // store moves onto the ingest thread (Phase 3 §2.5).
        let conn = store.reader()?;
        let discovery_conn = store.reader()?;
        // NEW-1 (final review round 2): one `HealthTracker`, shared by
        // both sinks built below — see that type's own doc for why a
        // tracker scoped to just one of the two producers cannot close
        // the latch MAJ-3 reopened.
        let health_tracker = Arc::new(HealthTracker::default());
        let result_sink: ResultSink = {
            let sink = Arc::clone(&sink);
            Arc::new(move |r: QueryResult| match r.kind {
                RequestKind::Query => sink(DataEvent::Query(QueryOutcome {
                    key: r.key,
                    tag: r.tag,
                    snapshot: r.snapshot.map(Arc::new),
                    submitted: r.submitted,
                })),
                RequestKind::Distinct { column } => sink(DataEvent::Distinct(DistinctOutcome {
                    key: r.key,
                    tag: r.tag,
                    column,
                    values: r.snapshot.map(|s| {
                        let v = s.column_index("value").expect("distinct selects value");
                        let n = s.column_index("n").expect("distinct selects n");
                        (0..s.rows())
                            .filter_map(|row| {
                                Some((s.text_at(v, row)?.to_string(), s.i64_at(n, row)? as u64))
                            })
                            .collect()
                    }),
                })),
            })
        };
        let pool = QueryPool::spawn_with_sink(&store, config.query_workers.max(1), result_sink)?;

        let ingest_sink: IngestSink = {
            let sink = Arc::clone(&sink);
            let health_tracker = Arc::clone(&health_tracker);
            Arc::new(move |e: IngestEvent| match e {
                IngestEvent::Published {
                    source,
                    dataset,
                    batch,
                    gen_id,
                    books,
                    rows,
                    health,
                } => {
                    tracing::info!(
                        target: "geode::ingest",
                        "published {dataset}/{batch} gen {gen_id}: {} book(s), {rows} row(s)",
                        books.len(),
                    );
                    let delivered = sink(DataEvent::Published {
                        dataset,
                        batch: batch.clone(),
                        gen_id,
                        books,
                    });
                    // MAJ-3 (final review): a degraded *publish* — the
                    // exact carried-dimension violation Phase 4a's grain
                    // rules exist to catch — used to reach nowhere but a
                    // DuckDB column nothing reads. Paired `Health` event
                    // under the source key (MAJ-1), same shape a load
                    // failure reports, whenever the load itself wasn't
                    // clean.
                    //
                    // NEW-1 (final review round 2): EVERY publish's
                    // health is reported to the shared tracker now, `Ok`
                    // included — not gated on `health != Health::Ok`
                    // here. That old guard was the other half of the
                    // latch: a clean republish's `Ok` never even reached
                    // the entity, so nothing could ever clear a source a
                    // degraded publish had marked. The tracker decides
                    // whether this is a real transition; only then is it
                    // forwarded.
                    let reason = match &health {
                        Health::Degraded { reason } | Health::Failed { reason } => reason.clone(),
                        _ => String::new(),
                    };
                    match health_tracker.report(&source, health) {
                        Some(reported) => {
                            log_health_event(&source, &reported, &reason);
                            delivered
                                && sink(DataEvent::Health {
                                    source,
                                    worst: reported,
                                    detail: format!("{batch}: {reason}"),
                                })
                        }
                        None => delivered,
                    }
                }
                IngestEvent::Failed {
                    source,
                    dataset,
                    batch,
                    reason,
                } => {
                    // MIN-4: a file that did not load is exactly the
                    // "lost data or a feature" case `log_health_event`
                    // maps to `error` — logged directly (dataset, batch,
                    // reason) rather than through that helper, since the
                    // message shape a load failure wants (which file,
                    // which batch) differs from a discovery-level
                    // `Health` line's (which source, what's wrong with
                    // it). Unconditional — a load failure is always worth
                    // this line, whether or not the AGGREGATE health
                    // (below) changed.
                    log_ingest_failure(&dataset, &batch, &reason);
                    // MAJ-1 (final review): keyed by the SOURCE name
                    // (`WorkItem::source`, threaded onto `IngestEvent`),
                    // never the dataset — a `[sources.<name>]` block's
                    // `name` and `dataset` are two separate fields, and
                    // keying by `dataset` created a phantom `sources`
                    // entry while the real source kept reading "no
                    // report yet".
                    //
                    // NEW-1 (final review round 2): routed through the
                    // shared tracker like every other health report, so
                    // a repeated identical failure (a permanently
                    // unreachable share, polled forever) does not
                    // re-send.
                    let Some(reported) = health_tracker.report(
                        &source,
                        Health::Failed {
                            reason: reason.clone(),
                        },
                    ) else {
                        return true;
                    };
                    sink(DataEvent::Health {
                        source,
                        worst: reported,
                        detail: format!("{batch}: {reason}"),
                    })
                }
                IngestEvent::PlanComplete => true,
            })
        };
        let ingest = Arc::new(IngestRunner::spawn(
            store,
            config.schema.clone(),
            ingest_sink,
        ));

        let scheduler_sink: SchedulerSink = {
            let sink = Arc::clone(&sink);
            let health_tracker = Arc::clone(&health_tracker);
            Arc::new(move |e: SchedulerEvent| match e {
                SchedulerEvent::Polled {
                    source,
                    ready,
                    next_in,
                } => {
                    tracing::debug!(target: "geode::ingest", "polled {source}: {ready} ready");
                    sink(polled_event(source, ready, SystemTime::now(), next_in))
                }
                SchedulerEvent::Health {
                    source,
                    worst,
                    detail,
                } => {
                    // NEW-1 (final review round 2): the scheduler now
                    // sends its poll result on every poll, unconditionally
                    // (see `ingest::scheduler::run`'s own comment) — this
                    // is the one place that decides whether it is a real
                    // transition, through the SAME tracker the ingest
                    // sink above reports through.
                    match health_tracker.report(&source, worst) {
                        Some(reported) => {
                            log_health_event(&source, &reported, &detail);
                            sink(DataEvent::Health {
                                source,
                                worst: reported,
                                detail,
                            })
                        }
                        None => true,
                    }
                }
            })
        };
        let scheduler = Scheduler::spawn(
            config.sources.clone(),
            discovery_conn,
            Arc::clone(&ingest),
            scheduler_sink,
        );

        // Validate here, not at first query: a misconfigured view otherwise
        // surfaces as a DuckDB binder error from inside a pool worker,
        // attributed to whichever tile happened to submit it, with the
        // config that caused it nowhere in the message. §10.1 wants a
        // diagnostic naming the view.
        //
        // Reported, never fatal — the same rule the shell follows for bad
        // config. One broken view must not stop the service the other
        // views need.
        let diagnostics = config
            .views
            .iter()
            .flat_map(|v| v.validate(&config.schema, &config.dimensions))
            .collect();
        Ok(DataService {
            config,
            diagnostics,
            pool,
            scheduler,
            ingest,
            conn,
        })
    }

    /// A service delivering into a channel, for callers that block on
    /// events — tests, benches, and the probe.
    pub fn open_channel(
        config: DataServiceConfig,
    ) -> Result<(DataService, Receiver<DataEvent>), StoreError> {
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        Ok((Self::open(config, sink)?, rx))
    }

    /// What validation found at open: config errors that would otherwise
    /// have surfaced as binder errors inside the query pool. Empty when
    /// every view checks out.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Swap the view set (a safe hot reload, foundation §8). Returns what
    /// validation found; a broken view is reported and skipped, the rest
    /// take effect.
    pub fn replace_views(
        &mut self,
        views: Vec<ViewSpec>,
        dimensions: DerivedDimensions,
    ) -> Vec<Diagnostic> {
        self.config.dimensions = dimensions;
        let diagnostics: Vec<Diagnostic> = views
            .iter()
            .flat_map(|v| v.validate(&self.config.schema, &self.config.dimensions))
            .collect();
        self.config.views = views;
        self.diagnostics = diagnostics.clone();
        diagnostics
    }

    /// Check a scope before it is compiled, so a bad column is reported
    /// against the scope rather than as a binder error (§10.1). The
    /// caller owns scope state, so this cannot be done at open.
    pub fn validate_scope(&self, dataset: &str, scope: &Scope) -> Vec<Diagnostic> {
        match self.config.schema.dataset(dataset) {
            Some(ds) => scope.validate(ds, &self.config.dimensions),
            None => Vec::new(),
        }
    }

    /// Compile and submit. Results arrive on the sink `open` was given;
    /// a newer query for the same key supersedes an older one.
    /// `max_depth` is the deepest grouping level to materialize. Pass one
    /// more than what the tree has expanded: a single-step expand is then
    /// already in the snapshot, and only a deeper one costs a requery.
    /// Materializing everything makes the caller.s flatten walk
    /// proportional to the whole tree rather than to what is on screen.
    pub fn query(&self, params: &QueryParams) -> Result<QueryId, StoreError> {
        let view = params.view.as_str();
        let spec = self
            .config
            .views
            .iter()
            .find(|v| v.name == view)
            .ok_or_else(|| StoreError::Sql {
                statement: format!("query view '{view}'"),
                source: duckdb::Error::InvalidParameterName(format!("unknown view '{view}'")),
            })?;

        // A grouping override is a per-query copy of the spec with its
        // grouping replaced; validation runs on the copy so an undeclared
        // column is this query's error, named, not a binder error later.
        let regrouped;
        let spec = match &params.grouping {
            None => spec,
            Some(grouping) => {
                regrouped = ViewSpec {
                    grouping: grouping.clone(),
                    ..spec.clone()
                };
                let diags = regrouped.validate(&self.config.schema, &self.config.dimensions);
                if let Some(d) = diags
                    .iter()
                    .find(|d| d.severity == geode_core::config::Severity::Error)
                {
                    return Err(StoreError::Sql {
                        statement: format!("query view '{view}' grouped by {grouping:?}"),
                        source: duckdb::Error::InvalidParameterName(d.message.clone()),
                    });
                }
                &regrouped
            }
        };

        let compiled = compile_view(
            &self.conn,
            spec,
            &self.config.schema,
            &params.scope,
            &self.config.dimensions,
            &params.as_of,
            params.max_depth,
        )?;

        // Freshness travels with the result, so §5.4's stalest-input rule
        // reaches the UI without every module reimplementing it.
        let mut provenance = Provenance {
            as_of_request: match &params.as_of {
                AsOf::Live => None,
                AsOf::At(t) => Some(t.to_rfc3339()),
            },
            ..Provenance::default()
        };
        let catalog = Catalog::new(&self.conn);
        for dataset in &compiled.stalest_input {
            // A historical result must not be labelled with today's
            // freshness. `dataset_as_of` reads the live catalog and
            // `latest_gen_id` is the newest generation in the database, so
            // both describe *now* — reporting them beside an as-of result
            // inverts the very rule §5.4 exists for.
            let freshness = match &params.as_of {
                AsOf::Live => Freshness {
                    dataset: dataset.clone(),
                    as_of: catalog.dataset_as_of(dataset, &[])?.map(|t| t.to_rfc3339()),
                    generation: catalog.latest_gen_id()?,
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

        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            view: ViewId(view.to_string()),
            // Cloned ahead of `compiled` below, which moves it: the field
            // order here is why this line precedes `compiled` rather than
            // sitting next to its other fields.
            grouping: compiled.grouping.clone(),
            compiled,
            provenance,
            kind: RequestKind::Query,
        }))
    }

    /// The picker's distinct-values query (spec §3.4): compile and submit
    /// under the caller's scope and era, unioned across every dataset
    /// that carries the column. The caller has already removed the
    /// column's own selection from `params.scope`.
    pub fn distinct(&self, params: &DistinctParams) -> Result<QueryId, StoreError> {
        let compiled = compile_distinct(
            &self.conn,
            &self.config.schema,
            &self.config.dimensions,
            params,
        )?;
        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: Instant::now(),
            view: ViewId(format!("distinct:{}", params.column)),
            grouping: Vec::new(),
            compiled,
            provenance: Provenance::default(),
            kind: RequestKind::Distinct {
                column: params.column.clone(),
            },
        }))
    }

    pub fn cancel(&self, key: QueryKey) {
        self.pool.cancel(key);
    }

    /// The diagnostics tile's "what does the database hold" request
    /// (spec §4.5), answered directly here on the service thread rather
    /// than submitted to the query pool — the plan's ruling: every query
    /// `build_catalog` runs is catalog-sized (`generations`,
    /// `file_generations`, DuckDB's own introspection functions), none
    /// of them touch a data table's rows, and the pool exists to bound
    /// concurrent *data* scans, not to serialize a synchronous,
    /// millisecond-scale read.
    pub fn catalog(&self, params: &CatalogParams) -> CatalogOutcome {
        let snapshot = build_catalog(&self.conn, &self.config.schema, &params.as_of);
        match &snapshot {
            Ok(snap) => {
                tracing::debug!(
                    target: "geode::query",
                    "catalog served: {} dataset(s)",
                    snap.datasets.len(),
                );
            }
            Err(e) => {
                tracing::warn!(target: "geode::query", "catalog request failed: {e}");
            }
        }
        CatalogOutcome {
            key: params.key,
            tag: params.tag,
            snapshot: snapshot.map_err(|e| e.to_string()),
        }
    }

    /// Per-book freshness for a dataset (spec §4.5).
    ///
    /// Takes the era rather than assuming live. Reporting today's
    /// freshness beside a historical result inverts the rule §5.4 exists
    /// for — the same defect class that was fixed at five other sites in
    /// phase 2b, and this was the one place left holding it. The parameter
    /// is what stops the next caller reintroducing it by omission.
    pub fn freshness(&self, dataset: &str, as_of: AsOf) -> Result<BookFreshness, StoreError> {
        let at = match as_of {
            AsOf::Live => return Catalog::new(&self.conn).book_freshness(dataset),
            AsOf::At(t) => t,
        };
        if self.config.schema.dataset(dataset).is_none() {
            return Ok(Vec::new());
        };

        // The oldest generation contributing to each book, which is the
        // same stalest-input rule live freshness applies (§4.5) — a book
        // is as fresh as the stalest file behind it, not the newest.
        let mut by_book: std::collections::BTreeMap<Option<String>, DateTime<Utc>> =
            std::collections::BTreeMap::new();
        for g in crate::query::as_of::resolve_generations(&self.conn, dataset, at)? {
            let book = g.book.clone();
            by_book
                .entry(book)
                .and_modify(|t| {
                    if g.source_time < *t {
                        *t = g.source_time;
                    }
                })
                .or_insert(g.source_time);
        }
        Ok(by_book.into_iter().collect())
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
        self.scheduler.shutdown();
        self.ingest.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::scope::{DimensionSelection, Scope};
    use std::time::Duration;

    fn service() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
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
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![crate::ingest::load::tests_support::tree_view()],
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: Vec::new(),
        })
        .unwrap();
        (db, src, service, rx)
    }

    /// The next query outcome, skipping any other event.
    fn next(rx: &std::sync::mpsc::Receiver<DataEvent>) -> QueryOutcome {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Query(o) => return o,
                _ => continue,
            }
        }
    }

    fn params(key: u64, view: &str, scope: &Scope, as_of: AsOf, max_depth: usize) -> QueryParams {
        QueryParams {
            key: QueryKey(key),
            tag: key,
            submitted: Instant::now(),
            view: view.to_string(),
            grouping: None,
            scope: scope.clone(),
            as_of,
            max_depth,
        }
    }

    #[test]
    fn a_grouping_override_regroups_the_named_view() {
        // The frame's active slot is applied per query (spec §5.1), not
        // by registering a view per slot. Grouped by book alone the tree
        // has 1 + books rows at depth ≤ 1; the view's own three-level
        // grouping has many more.
        let (_db, _src, svc, rx) = service();
        let mut p = params(1, "tree", &Scope::default(), AsOf::Live, 1);
        p.grouping = Some(vec!["book".into()]);
        svc.query(&p).unwrap();
        let by_book = next(&rx).snapshot.unwrap();
        // Checked two ways: the compiler's own contract (compile.rs:
        // grouping columns are pushed first, in order) against the column
        // order, and `Snapshot::grouping()` directly — which is what the
        // override actually has to change.
        assert_eq!(by_book.column_names().first().copied(), Some("book"));
        assert_eq!(by_book.grouping(), &["book".to_string()]);
        let mut wrong = params(2, "tree", &Scope::default(), AsOf::Live, 1);
        wrong.grouping = Some(vec!["nonesuch".into()]);
        assert!(
            svc.query(&wrong).is_err(),
            "an undeclared column fails at compile time"
        );
        svc.shutdown();
    }

    #[test]
    fn an_outcome_is_addressed_to_the_key_that_asked() {
        let (_db, _src, svc, rx) = service();
        svc.query(&params(42, "tree", &Scope::default(), AsOf::Live, 1))
            .unwrap();
        let o = next(&rx);
        assert_eq!(o.key, QueryKey(42));
        assert_eq!(o.tag, 42);
        assert!(o.snapshot.unwrap().rows() > 0);
        svc.shutdown();
    }

    #[test]
    fn a_distinct_query_returns_value_counts_on_the_distinct_event() {
        let (_db, _src, svc, rx) = service();
        let params = DistinctParams {
            key: QueryKey(3),
            tag: 3,
            column: "book".into(),
            scope: Scope::default(),
            as_of: AsOf::Live,
        };
        svc.distinct(&params).unwrap();
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Distinct(o) => {
                    assert_eq!(o.key, QueryKey(3));
                    assert_eq!(o.tag, 3);
                    assert_eq!(o.column, "book");
                    let values = o.values.expect("distinct query failed");
                    assert!(!values.is_empty(), "the fixture has books");
                    assert!(values.iter().all(|(_, n)| *n > 0));
                    break;
                }
                _ => continue,
            }
        }
        svc.shutdown();
    }

    #[test]
    fn a_catalog_request_echoes_the_tag_and_lists_the_one_dataset() {
        // `DataService::catalog` answers directly (the plan's ruling:
        // catalog-sized queries run on the service thread, not the
        // pool), so this checks the return value straight — the request
        // loop that puts it on the sink as `DataEvent::Catalog` is
        // `handle.rs`'s job, exercised end to end there.
        let (_db, _src, svc, _rx) = service();
        let params = CatalogParams {
            key: QueryKey(11),
            tag: 11,
            as_of: AsOf::Live,
        };
        let outcome = svc.catalog(&params);
        assert_eq!(outcome.key, QueryKey(11));
        assert_eq!(outcome.tag, 11);
        let snap = outcome.snapshot.expect("catalog request failed");
        assert_eq!(snap.datasets.len(), 1);
        assert_eq!(snap.datasets[0].name, "risk_snapshot");
        svc.shutdown();
    }

    #[test]
    fn polled_event_next_is_at_plus_next_in() {
        // Review round 1 MAJ-4, restoring the brief's own Step 8 entry
        // ("Polled.next = at"): the one line the plan's "Rulings taken
        // while planning" section specifically called for, now unit-
        // tested directly rather than only through a scheduler.
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let next_in = Duration::from_secs(30);
        match polled_event("risk".into(), 3, at, next_in) {
            DataEvent::Polled {
                source,
                ready,
                at: got_at,
                next,
            } => {
                assert_eq!(source, "risk");
                assert_eq!(ready, 3);
                assert_eq!(got_at, at);
                assert_eq!(next, at + next_in);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn polled_event_saturates_rather_than_panics_on_a_huge_poll_interval() {
        // MIN-7: `poll_interval` is user-configured and unbounded in
        // magnitude (`source::config::parse_duration`); `at + next_in`
        // must not panic the request loop over a diagnostic value.
        let at = SystemTime::now();
        match polled_event("risk".into(), 0, at, Duration::MAX) {
            DataEvent::Polled { next, .. } => assert!(next >= at),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn replacing_views_makes_a_new_view_queryable_and_reports_a_bad_one() {
        let (_db, _src, mut svc, rx) = service();
        let mut renamed = crate::ingest::load::tests_support::tree_view();
        renamed.name = "tree2".into();
        let mut broken = crate::ingest::load::tests_support::tree_view();
        broken.name = "broken".into();
        broken.grouping.push("nosuchcolumn".into());

        let diags = svc.replace_views(vec![renamed, broken], DerivedDimensions::default());
        assert!(
            diags.iter().any(|d| d.message.contains("broken")),
            "{diags:?}"
        );
        assert!(
            svc.query(&params(1, "tree", &Scope::default(), AsOf::Live, 1))
                .is_err(),
            "the old view name is gone"
        );
        svc.query(&params(2, "tree2", &Scope::default(), AsOf::Live, 1))
            .unwrap();
        assert!(next(&rx).snapshot.is_ok());
        svc.shutdown();
    }

    #[test]
    fn a_sink_that_reports_nobody_listening_does_not_wedge_the_service() {
        // A closed sink is how the UI goes away. The service must keep
        // accepting requests without panicking; results simply have
        // nowhere to go.
        let (db, _src, svc, rx) = service();
        svc.shutdown();
        drop(rx);
        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let sink: EventSink = Arc::new(|_| false);
        let svc = DataService::open(
            DataServiceConfig {
                db_path: db.path().join("geode.duckdb"),
                schema,
                views: vec![crate::ingest::load::tests_support::tree_view()],
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
            },
            sink,
        )
        .unwrap();
        svc.query(&params(1, "tree", &Scope::default(), AsOf::Live, 1))
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        svc.shutdown();
    }

    #[test]
    fn a_misconfigured_view_is_a_diagnostic_at_open_not_a_binder_error_later() {
        // Unwired, this view compiled fine and failed inside a pool worker
        // as `Binder Error: ... nosuchcolumn`, attributed to whichever tile
        // submitted it, with nothing naming the view or the config that
        // caused it (§10.1). And it failed at first query, not at load, so
        // a view nobody opened looked healthy.
        let (db, _src, _svc, _rx) = service();
        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);

        let mut broken = crate::ingest::load::tests_support::tree_view();
        broken.name = "broken".into();
        broken.grouping.push("nosuchcolumn".into());

        let diags = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![broken],
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
        })
        .expect("a broken view must not stop the service opening")
        .0
        .diagnostics()
        .to_vec();
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("broken") && d.message.contains("nosuchcolumn")),
            "the diagnostic must name the view and the column: {diags:?}"
        );
    }

    #[test]
    fn a_scope_naming_an_unknown_column_is_reported_against_the_scope() {
        let (_db, _src, svc, _rx) = service();
        let scope = Scope {
            dimensions: vec![geode_core::scope::DimensionSelection {
                column: "nosuchcolumn".into(),
                values: vec!["x".into()],
            }],
            ..Scope::default()
        };
        let diags = svc.validate_scope("risk_snapshot", &scope);
        assert!(
            diags.iter().any(|d| d.message.contains("nosuchcolumn")),
            "{diags:?}"
        );
        assert!(
            svc.validate_scope("risk_snapshot", &Scope::default())
                .is_empty(),
            "an empty scope is valid"
        );
    }

    #[test]
    fn a_query_by_view_name_returns_a_snapshot() {
        let (_db, _src, svc, rx) = service();
        svc.query(&params(
            1,
            "tree",
            &Scope::default(),
            AsOf::Live,
            usize::MAX,
        ))
        .unwrap();
        let o = next(&rx);
        let snap = o.snapshot.expect("query failed");
        assert!(snap.rows() > 0);
        svc.shutdown();
    }

    #[test]
    fn an_unknown_view_is_an_error_not_a_panic() {
        let (_db, _src, svc, _rx) = service();
        assert!(
            svc.query(&params(
                1,
                "nonesuch",
                &Scope::default(),
                AsOf::Live,
                usize::MAX
            ))
            .is_err()
        );
        svc.shutdown();
    }

    #[test]
    fn a_scope_narrows_the_result() {
        let (_db, _src, svc, rx) = service();
        svc.query(&params(
            1,
            "tree",
            &Scope::default(),
            AsOf::Live,
            usize::MAX,
        ))
        .unwrap();
        let all = next(&rx).snapshot.unwrap().rows();

        let scoped = Scope {
            dimensions: vec![DimensionSelection {
                column: "book".into(),
                values: vec!["BK000".into()],
            }],
            ..Scope::default()
        };
        svc.query(&params(2, "tree", &scoped, AsOf::Live, usize::MAX))
            .unwrap();
        let narrowed = next(&rx).snapshot.unwrap().rows();
        assert!(narrowed < all, "{narrowed} should be fewer than {all}");
        svc.shutdown();
    }

    #[test]
    fn the_snapshot_carries_per_dataset_freshness() {
        let (_db, _src, svc, rx) = service();
        svc.query(&params(
            1,
            "tree",
            &Scope::default(),
            AsOf::Live,
            usize::MAX,
        ))
        .unwrap();
        let snap = next(&rx).snapshot.unwrap();
        let p = snap.provenance();
        assert!(!p.datasets.is_empty(), "freshness must reach the snapshot");
        assert!(p.stalest().is_some());
        svc.shutdown();
    }

    #[test]
    fn freshness_is_reported_per_book() {
        let (_db, _src, svc, _rx) = service();
        let books = svc.freshness("risk_snapshot", AsOf::Live).unwrap();
        assert!(!books.is_empty(), "books must have freshness recorded");
        svc.shutdown();
    }

    #[test]
    fn a_historical_result_is_labelled_with_the_data_it_actually_read() {
        // Not the instant requested. Asking for today against data last
        // published a month ago must report the month-old time, or every
        // dataset carries the same value and `stalest()` — the whole
        // point of §5.4 — can no longer tell one input from another.
        let (_db, _src, svc, rx) = service();
        // A month-old generation of a partition nothing else covers,
        // written through a clone of the service's own connection. A
        // `try_clone` shares the same database handle — it is not a
        // separate database instance, which is the trap `Connection::open`
        // on the path would be. An earlier version of this test did that:
        // it opened a second `Connection` on the file — a separate
        // database instance whose writes the service never saw — and
        // `.ok()`ed an insert that failed anyway on its column count. It
        // then passed against an empty archive, vacuously.
        let writer = svc.conn.try_clone().unwrap();
        writer
            .execute_batch(
                "insert into risk_snapshot_position_archive
                   select * replace ('ghost' as batch, 99 as gen_id,
                                     TIMESTAMPTZ '2026-07-01 00:00:00Z' as source_time)
                   from risk_snapshot_position_live limit 1;",
            )
            .unwrap();
        // The insert above went straight to the archive, bypassing
        // `publish_file`'s own generations-summary maintenance -- rebuild
        // it so `era_for`'s resolve (which now reads the summary, not the
        // tables directly) can see the ghost generation.
        let ds = svc.config.schema.dataset("risk_snapshot").unwrap();
        crate::store::ddl::rebuild_generations(
            &writer,
            "risk_snapshot",
            &crate::store::ddl::history_of("risk_snapshot", ds),
        )
        .unwrap();

        let requested = chrono::DateTime::parse_from_rfc3339("2026-08-30T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        svc.query(&params(
            1,
            "tree",
            &Scope::default(),
            AsOf::At(requested),
            usize::MAX,
        ))
        .unwrap();
        let snap = next(&rx).snapshot.expect("query failed");
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
        let (_db, _src, svc, _rx) = service();
        let oldest_live = svc
            .freshness("risk_snapshot", AsOf::Live)
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
        let (_db, _src, svc, _rx) = service();
        svc.shutdown();
        svc.shutdown();
    }

    #[test]
    fn a_configured_source_is_discovered_loaded_and_announced() {
        // Cold start through the real door: a service opened over an
        // empty database with one source, and nothing else.
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 500,
            seed: 3,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(src.path());
        opts.leave_one_pending = false;
        let emitted = geode_demo_data::emit_directory(&batch, &opts).unwrap();

        let (svc, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![crate::ingest::load::tests_support::tree_view()],
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                name: "risk".into(),
                dataset: "risk_snapshot".into(),
                paths: vec![format!("{}/*.csv", src.path().display())],
                readiness: crate::source::Readiness::Sentinel,
                priority: crate::source::Priority::LatestRisk,
                poll_interval: Duration::from_secs(3600),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            }],
        })
        .unwrap();

        let mut published = 0;
        let deadline = Instant::now() + Duration::from_secs(120);
        while published < emitted.files.len() && Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(DataEvent::Published { dataset, .. }) => {
                    assert_eq!(dataset, "risk_snapshot");
                    published += 1;
                }
                // MAJ-2 (final review): a clean source's first poll is
                // now itself an `Ok` transition — expected, not a failure.
                Ok(DataEvent::Health { worst, detail, .. }) if worst != Health::Ok => {
                    panic!("{detail}")
                }
                _ => {}
            }
        }
        assert_eq!(published, emitted.files.len());

        svc.query(&params(1, "tree", &Scope::default(), AsOf::Live, 1))
            .unwrap();
        assert!(next(&rx).snapshot.unwrap().rows() > 1, "data is queryable");
        svc.shutdown();
    }

    #[test]
    fn a_load_failure_reports_health_under_the_source_name_not_the_dataset_name() {
        // MAJ-1 (final review): `[sources.eod_risk] dataset = "risk_snapshot"`
        // — the source's own name differs from the dataset it feeds, which
        // is exactly the review's failure scenario. The schema is left
        // without `risk_snapshot` declared on purpose, so the runner's
        // "dataset is not declared" failure fires deterministically without
        // depending on a real load succeeding or failing.
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 50,
            seed: 11,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(src.path());
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts).unwrap();

        let (svc, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema: SchemaSpec::default(),
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                name: "eod_risk".into(),
                dataset: "risk_snapshot".into(),
                paths: vec![format!("{}/*.csv", src.path().display())],
                readiness: crate::source::Readiness::Sentinel,
                priority: crate::source::Priority::LatestRisk,
                poll_interval: Duration::from_secs(3600),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            }],
        })
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut seen = None;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(5)) {
                // The discovery-level `Ok` transition (MAJ-2, final
                // review) is expected on the first clean poll — the
                // failure this test cares about is the *ingest* failure
                // that follows once the runner tries to load the
                // undeclared dataset.
                Ok(DataEvent::Health {
                    worst: Health::Ok, ..
                }) => {}
                Ok(DataEvent::Health { source, detail, .. }) => {
                    seen = Some((source, detail));
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let (source, detail) = seen.expect("a Health event for the failed load");
        assert_eq!(
            source, "eod_risk",
            "keyed by the SOURCE name, never the dataset name"
        );
        assert!(
            detail.contains("risk_snapshot"),
            "dataset/batch still named in the detail: {detail}"
        );
        svc.shutdown();
    }

    /// A minimal schema with `currency` a dimension carried by the
    /// instrument grain (spec §3.3) — same shape as
    /// `ingest::load::tests::carried_schema` (not reused directly: that
    /// one is private to its own test module), needed here to drive a
    /// `Degraded` publish end to end through a real source directory.
    fn carried_schema() -> geode_core::schema::DatasetSpec {
        use geode_core::config::{LayerDoc, merge_docs};
        let text = r#"
[risk_snapshot.columns.book]
type = "utf8"
role = "dimension"
source_name = "Book"
[risk_snapshot.columns.lhu]
type = "utf8"
role = "dimension"
source_name = "LHU"
[risk_snapshot.columns.position_ref]
type = "utf8"
role = "key"
source_name = "PositionRef"
[risk_snapshot.columns.counterparty]
type = "utf8"
role = "dimension"
source_name = "Counterparty"
[risk_snapshot.columns.instrument_ref]
type = "utf8"
role = "key"
source_name = "InstrumentRef"
[risk_snapshot.columns.currency]
type = "utf8"
role = "dimension"
grain = "instrument"
source_name = "Currency"
[risk_snapshot.columns.npv]
type = "f64"
role = "measure"
grain = "instrument"
source_name = "NPV"
"#;
        let doc = merge_docs("datasets", &[LayerDoc::builtin("datasets", text).unwrap()]);
        SchemaSpec::from_doc(&doc)
            .0
            .dataset("risk_snapshot")
            .unwrap()
            .clone()
    }

    #[test]
    fn a_degraded_publish_reaches_the_entity_as_degraded_health() {
        // MAJ-3 (final review): `IngestEvent::Published.health` used to be
        // swallowed by the service's `..` — a load with a carried-dimension
        // violation (spec §3.3, the exact silent-wrong-data condition
        // Phase 4a's grain rules exist to catch) went live with no
        // `DataEvent::Health` at all. Through the real door: a source
        // directory with one file whose `currency` disagrees within its
        // instrument key.
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let csv_path = src.path().join("risk_2026-08-24_BK0.csv");
        std::fs::write(
            &csv_path,
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK0,L0,P1,C,I1,USD,100\n\
             BK0,L0,P1,C,I1,EUR,100\n",
        )
        .unwrap();
        std::fs::write(
            src.path().join("risk_2026-08-24_BK0.csv.done"),
            r#"{"as_of":"2026-08-24T07:00:00Z","columns":["Book","LHU","PositionRef","Counterparty","InstrumentRef","Currency","NPV"],"books":["BK0"]}"#,
        )
        .unwrap();

        let mut schema = SchemaSpec::default();
        schema.datasets.push(carried_schema());

        let (svc, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                name: "eod_risk".into(),
                dataset: "risk_snapshot".into(),
                paths: vec![format!("{}/*.csv", src.path().display())],
                readiness: crate::source::Readiness::Sentinel,
                priority: crate::source::Priority::LatestRisk,
                poll_interval: Duration::from_secs(3600),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            }],
        })
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut seen = None;
        let mut published = false;
        while Instant::now() < deadline && (!published || seen.is_none()) {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(DataEvent::Published { .. }) => published = true,
                // The discovery scheduler's own `Ok` transition (MAJ-2,
                // final review) runs on a separate thread and can
                // interleave with the ingest runner's `Published`/
                // `Health{Degraded}` pair in either order — it must not
                // be mistaken for the degraded-publish event this test is
                // watching for.
                Ok(DataEvent::Health {
                    worst: Health::Ok, ..
                }) => {}
                Ok(DataEvent::Health {
                    source,
                    worst,
                    detail,
                }) => {
                    seen = Some((source, worst, detail));
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(published, "the file still publishes — Degraded, not Failed");
        let (source, worst, detail) = seen.expect("a Health event for the degraded publish");
        assert_eq!(source, "eod_risk", "keyed by the source name (MAJ-1)");
        assert!(
            matches!(worst, Health::Degraded { .. }),
            "expected Degraded, got {worst:?}"
        );
        assert!(detail.contains("currency"), "{detail}");
        svc.shutdown();
    }

    #[test]
    fn health_tracker_reports_a_transition_only() {
        let t = HealthTracker::default();
        assert_eq!(
            t.report("a", Health::Ok),
            Some(Health::Ok),
            "the first report for a source is always a transition"
        );
        assert_eq!(
            t.report("a", Health::Ok),
            None,
            "a repeated identical report is not a transition"
        );
        let degraded = Health::Degraded { reason: "x".into() };
        assert_eq!(
            t.report("a", degraded.clone()),
            Some(degraded.clone()),
            "a changed report is a transition"
        );
        assert_eq!(
            t.report("a", degraded),
            None,
            "repeating the new state again is not a transition"
        );
        assert_eq!(
            t.report("b", Health::Ok),
            Some(Health::Ok),
            "a different source's first report is its own transition, \
             independent of source \"a\""
        );
    }

    /// NEW-1 (final review round 2): a degraded PUBLISH is cleared by a
    /// later, CLEAN publish for the same source — the shared
    /// `HealthTracker` closes the gap MAJ-3 opened (a second writer of
    /// source health the scheduler's old, service-local dedup knew
    /// nothing about). Two publishes, same source, different batches: the
    /// first violates the carried-dimension rule, the second does not.
    #[test]
    fn a_clean_publish_clears_source_health_left_degraded_by_an_earlier_publish() {
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();

        // batch BK0: currency varies within its instrument key — Degraded.
        std::fs::write(
            src.path().join("risk_2026-08-24_BK0.csv"),
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK0,L0,P1,C,I1,USD,100\n\
             BK0,L0,P1,C,I1,EUR,100\n",
        )
        .unwrap();
        std::fs::write(
            src.path().join("risk_2026-08-24_BK0.csv.done"),
            r#"{"as_of":"2026-08-24T07:00:00Z","columns":["Book","LHU","PositionRef","Counterparty","InstrumentRef","Currency","NPV"],"books":["BK0"]}"#,
        )
        .unwrap();

        let mut schema = SchemaSpec::default();
        schema.datasets.push(carried_schema());

        let (svc, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                name: "eod_risk".into(),
                dataset: "risk_snapshot".into(),
                paths: vec![format!("{}/*.csv", src.path().display())],
                readiness: crate::source::Readiness::Sentinel,
                priority: crate::source::Priority::LatestRisk,
                poll_interval: Duration::from_millis(50),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            }],
        })
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_degraded = false;
        while Instant::now() < deadline && !saw_degraded {
            if let Ok(DataEvent::Health {
                source,
                worst: Health::Degraded { .. },
                ..
            }) = rx.recv_timeout(Duration::from_secs(5))
            {
                assert_eq!(source, "eod_risk");
                saw_degraded = true;
            }
        }
        assert!(saw_degraded, "setup: the first publish must degrade");

        // batch BK1: a clean, non-violating publish for the SAME source.
        std::fs::write(
            src.path().join("risk_2026-08-25_BK1.csv"),
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK1,L0,P2,C,I2,USD,50\n",
        )
        .unwrap();
        std::fs::write(
            src.path().join("risk_2026-08-25_BK1.csv.done"),
            r#"{"as_of":"2026-08-25T07:00:00Z","columns":["Book","LHU","PositionRef","Counterparty","InstrumentRef","Currency","NPV"],"books":["BK1"]}"#,
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_ok = false;
        while Instant::now() < deadline && !saw_ok {
            if let Ok(DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            }) = rx.recv_timeout(Duration::from_secs(5))
            {
                assert_eq!(source, "eod_risk");
                saw_ok = true;
            }
        }
        assert!(
            saw_ok,
            "a later clean publish for the same source must clear the \
             earlier degraded health"
        );
        svc.shutdown();
    }

    /// NEW-1's other direction: a source the SCHEDULER reported
    /// `PendingTooLong` (discovery alone, no load involved) is cleared by
    /// a later CLEAN PUBLISH for the same source — proving the tracker is
    /// genuinely shared, not just consulted one-way.
    #[test]
    fn a_clean_publish_clears_source_health_left_pending_too_long_by_the_scheduler() {
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();

        // A stray CSV with no sentinel at all: discovery alone reports it
        // PendingTooLong forever (pending_timeout = 0), independent of
        // whatever else is in the directory.
        std::fs::write(src.path().join("risk_2026-09-03_STRAY.csv"), "Book\nX\n").unwrap();

        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);

        let (svc, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                name: "eod_risk".into(),
                dataset: "risk_snapshot".into(),
                paths: vec![format!("{}/*.csv", src.path().display())],
                readiness: crate::source::Readiness::Sentinel,
                priority: crate::source::Priority::LatestRisk,
                poll_interval: Duration::from_millis(50),
                pending_timeout: Duration::ZERO,
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            }],
        })
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_pending = false;
        while Instant::now() < deadline && !saw_pending {
            if let Ok(DataEvent::Health {
                source,
                worst: Health::PendingTooLong,
                ..
            }) = rx.recv_timeout(Duration::from_secs(5))
            {
                assert_eq!(source, "eod_risk");
                saw_pending = true;
            }
        }
        assert!(
            saw_pending,
            "setup: the stray file must report PendingTooLong"
        );

        // A real, ready file for the same source lands and loads cleanly.
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 50,
            seed: 13,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(src.path());
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts).unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_ok = false;
        while Instant::now() < deadline && !saw_ok {
            if let Ok(DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            }) = rx.recv_timeout(Duration::from_secs(5))
            {
                assert_eq!(source, "eod_risk");
                saw_ok = true;
            }
        }
        assert!(
            saw_ok,
            "a clean publish must clear health the scheduler alone set"
        );
        svc.shutdown();
    }

    /// NEW-1's dedup half: the scheduler's own clean polls (now emitted
    /// unconditionally, every poll — MAJ-2's dedup moved here, to the
    /// shared tracker) and one clean publish for the same source must
    /// still add up to exactly ONE `DataEvent::Health { worst: Ok, .. }`
    /// reaching the outer channel, not one per producer.
    #[test]
    fn a_clean_scheduler_poll_and_a_clean_publish_together_send_exactly_one_ok() {
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 50,
            seed: 17,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(src.path());
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts).unwrap();

        let (svc, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                name: "eod_risk".into(),
                dataset: "risk_snapshot".into(),
                paths: vec![format!("{}/*.csv", src.path().display())],
                readiness: crate::source::Readiness::Sentinel,
                priority: crate::source::Priority::LatestRisk,
                poll_interval: Duration::from_millis(30),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            }],
        })
        .unwrap();

        let mut ok_count = 0;
        // ~20 poll intervals' worth of wall time: comfortably enough for
        // several scheduler polls AND the one publish to both report,
        // without the test itself taking long.
        let deadline = Instant::now() + Duration::from_millis(900);
        while Instant::now() < deadline {
            if let Ok(DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            }) = rx.recv_timeout(Duration::from_millis(100))
                && source == "eod_risk"
            {
                ok_count += 1;
            }
        }
        assert_eq!(
            ok_count, 1,
            "the scheduler's own repeated clean polls and the one clean \
             publish must not double-send Ok"
        );
        svc.shutdown();
    }

    #[test]
    fn open_rebuilds_the_summary_when_it_is_absent_and_data_exists() {
        // The migration path: a database written before `generations`
        // existed has real rows in its data tables and nothing in the
        // summary. `open` must notice and rebuild it, or every as-of
        // query against such a database silently resolves nothing.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("geode.duckdb");
        let ds = crate::store::ddl::tests_support::sample_dataset();
        {
            let store = crate::store::Store::open(&db_path).unwrap();
            store.apply_schema(&ds).unwrap();
            Catalog::new(store.writer()).ensure_tables().unwrap();
            store
                .writer()
                .execute_batch(
                    "insert into risk_snapshot_position_live
                         (book, lhu, position_ref, counterparty, daily_trading_pnl,
                          batch, source_file_id, gen_id, source_time)
                     values ('BK0', 'L0', 'P1', 'C', 7, 'b', 1, 1, now());",
                )
                .unwrap();
            let before: i64 = store
                .writer()
                .query_row(
                    "select count(*) from generations where dataset = 'risk_snapshot'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(before, 0, "sanity: the summary starts absent");
        }

        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let (svc, _rx) = DataService::open_channel(DataServiceConfig {
            db_path,
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
        })
        .unwrap();

        let after: i64 = svc
            .conn
            .query_row(
                "select count(*) from generations where dataset = 'risk_snapshot'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, 1, "open must rebuild the summary from the tables");
        svc.shutdown();
    }

    #[test]
    fn open_leaves_an_already_populated_summary_untouched() {
        // The other half of the migration guard: a database that already
        // has summary rows for a dataset must not be rebuilt on open --
        // rebuilding unconditionally would make the summary just a cache
        // of the last open, not a maintained record. A row that matches
        // nothing in the tables proves it: a rebuild would drop it.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("geode.duckdb");
        let ds = crate::store::ddl::tests_support::sample_dataset();
        {
            let store = crate::store::Store::open(&db_path).unwrap();
            store.apply_schema(&ds).unwrap();
            Catalog::new(store.writer()).ensure_tables().unwrap();
            store
                .writer()
                .execute_batch(
                    "insert into risk_snapshot_position_live
                         (book, lhu, position_ref, counterparty, daily_trading_pnl,
                          batch, source_file_id, gen_id, source_time)
                     values ('BK0', 'L0', 'P1', 'C', 7, 'b', 1, 1,
                             TIMESTAMPTZ '2026-08-30T07:00:00Z');
                     insert into generations values
                       ('risk_snapshot', 'b', 'BK0', 1, TIMESTAMPTZ '2026-08-30T07:00:00Z'),
                       ('risk_snapshot', 'stale', 'BKX', 99, TIMESTAMPTZ '2020-01-01T00:00:00Z');",
                )
                .unwrap();
        }

        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let (svc, _rx) = DataService::open_channel(DataServiceConfig {
            db_path,
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
        })
        .unwrap();

        let count: i64 = svc
            .conn
            .query_row(
                "select count(*) from generations where dataset = 'risk_snapshot'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "row count unchanged by open");
        let stale: i64 = svc
            .conn
            .query_row(
                "select count(*) from generations
                 where dataset = 'risk_snapshot' and gen_id = 99",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stale, 1,
            "the planted extra row survives -- a rebuild would have dropped it"
        );
        svc.shutdown();
    }

    // ---- Phase 4b Task 2 fix round 1 (MAJ-1): log_health_event's level
    // split, proved against a scoped ring subscriber rather than by
    // reading the match arms. ------------------------------------------

    use tracing_subscriber::layer::SubscriberExt;

    /// Runs `f` under a subscriber that feeds only a fresh [`geode_core::
    /// log::Ring`], and returns what landed in it. Scoped
    /// (`tracing::subscriber::with_default`), not global — this doesn't
    /// touch the process-wide subscriber `main.rs` installs, so it's
    /// safe to run alongside every other test in this crate.
    fn logged(f: impl FnOnce()) -> Vec<geode_core::log::Record> {
        let ring = std::sync::Arc::new(geode_core::log::Ring::new(8));
        let sub =
            tracing_subscriber::registry().with(geode_core::log::RingLayer::new(ring.clone()));
        tracing::subscriber::with_default(sub, f);
        let mut out = Vec::new();
        ring.drain_since(0, &mut out);
        out
    }

    #[test]
    fn a_failed_health_logs_at_error_through_the_service_sink() {
        let records = logged(|| {
            log_health_event(
                "risk",
                &Health::Failed {
                    reason: "discovery panicked".into(),
                },
                "discovery panicked",
            );
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level, tracing::Level::ERROR);
        assert_eq!(records[0].target, "geode::ingest");
        assert!(records[0].message.contains("risk"));
    }

    #[test]
    fn degraded_and_pending_too_long_log_at_warn() {
        for worst in [
            Health::Degraded { reason: "r".into() },
            Health::PendingTooLong,
        ] {
            let records = logged(|| log_health_event("risk", &worst, "d"));
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].level, tracing::Level::WARN, "{worst:?}");
        }
    }

    #[test]
    fn ok_and_pending_log_below_warn() {
        let records = logged(|| log_health_event("risk", &Health::Ok, "d"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level, tracing::Level::INFO);

        // `debug` is below the ring test's own default max-level hint
        // only if something raised it; a scoped `with_default` subscriber
        // has no `Targets` filter layer, so every level is enabled here
        // regardless of what `main.rs`'s process-wide filter would do —
        // this test is about which *macro* `log_health_event` calls, not
        // about runtime filtering (that's `LogLevels::to_targets`'s own
        // test).
        let records = logged(|| log_health_event("risk", &Health::Pending, "d"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level, tracing::Level::DEBUG);
    }

    #[test]
    fn a_load_failure_logs_dataset_batch_and_reason_at_error() {
        let records = logged(|| log_ingest_failure("risk_snapshot", "b1", "bad header"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level, tracing::Level::ERROR);
        assert_eq!(records[0].target, "geode::ingest");
        assert!(records[0].message.contains("risk_snapshot/b1"));
        assert!(records[0].message.contains("bad header"));
    }
}
