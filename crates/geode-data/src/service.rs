//! `DataService` — the only door to data (spec §5).
//!
//! Modules ask this and nothing else: no module opens a file, holds a
//! connection, or names a table. Everything below is an implementation
//! detail, which is what makes the future sidecar-process split an
//! evolution rather than a rewrite (§2).

use crate::health::{Health, severity_rank};
use crate::ingest::scheduler::{Scheduler, SchedulerEvent, SchedulerSink};
use crate::ingest::{IngestEvent, IngestHandle, IngestRunner, IngestSink};
use crate::query::as_of::AsOf;
use crate::query::catalog::build_catalog;
use crate::query::compile::compile_view;
use crate::query::distinct::compile_distinct;
use crate::query::document::compile_document;
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
    CatalogOutcome, CatalogParams, DistinctOutcome, DistinctParams, DocumentParams, QueryKey,
    QueryOutcome,
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

/// Where events go. `false` means "this event was not delivered" — the
/// caller's channel was full, or its receiver is gone. The two are the
/// same answer here on purpose, because the rule is the same for both:
/// **no producer inside the service may stop on a refusal** (Phase 4b
/// follow-up, Task 1). The ingest runner carries on to its next item, the
/// discovery scheduler re-arms its poll, and a query worker takes its next
/// request; each logs the refusal once and drops the event. Nothing is
/// retried — a health transition the caller missed is re-offered by
/// `HealthTracker` on the next report, and a missed query result is
/// requeried by the tile that wanted it. Shutdown is `IngestHandle::
/// shutdown`, the scheduler's stop condvar, and the pool's `shutdown`,
/// never a `false` from here.
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

/// One source's health along two independent lanes (final review round
/// 3, NEW-4) — round 2's fix (`HealthTracker` as a single shared
/// last-value map, keyed only by source) closed MAJ-2's original latch
/// but opened a worse one: a routine, CONTENT-BLIND discovery poll
/// (`Health::Ok` whenever nothing is currently stuck or malformed on
/// disk) could overwrite a real, unfixed `Degraded`/`Failed` a PUBLISH
/// had set, within about one poll interval, with nothing actually
/// corrected. `CandidateState::Unchanged` is assigned to an
/// already-loaded file regardless of whether that load degraded
/// (`source::discovery::is_unchanged`'s own doc comment says so), and
/// `worst_health` skips `Unchanged`/`Ready`/`Pending` candidates
/// entirely — so "nothing looks stuck on the file system" is not
/// evidence "the last publish was clean", and a single shared map could
/// not tell the two apart. A false-`Ok` on the one surface whose job is
/// to be right about state — worse than round 2's own defect (a real
/// problem that stayed visibly flagged, if permanently).
///
/// Two lanes fix it: `discovery`, written only by the scheduler sink
/// (content-blind — "is anything currently stuck or malformed on
/// disk"), and `load`, written only by the ingest sink (content-aware —
/// "did the last publish or load attempt succeed cleanly"). Neither
/// lane can be written by the other producer. The value this module
/// actually reports is always the WORSE of the two, by
/// [`severity_rank`], so a clean discovery poll can never override a
/// load-set `Degraded`/`Failed`: only writing the `load` lane back to
/// `Ok` (a corrected republish of that batch) can, because that is the
/// only way to bring the combined worst back down.
///
/// Round 4 (NEW-5, NEW-6) changed two more things about that combine,
/// both of which were reported states that were simply wrong:
///
/// - Each slot carries its own `detail`, and `report_*` hands back the
///   DECIDING slot's `(health, detail)` pair for its caller to forward
///   verbatim. Before, each sink forwarded its OWN detail alongside
///   whatever health the tracker returned — so an ingest sink handed
///   back discovery's `PendingTooLong` attached a just-published
///   batch's name to it and dropped the stuck file's, and a scheduler
///   sink handed back a load's `Degraded` attached a clean poll's EMPTY
///   detail, surfacing "degraded" with no reason at all.
/// - Comparison is by [`severity_rank`], never `Health`'s derived
///   `Ord`, and an equal rank is decided by which slot changed most
///   recently (the ruling's `decided_by`, generalised — see
///   [`LaneValue::changed`]). Two simultaneous `Degraded`s are ordinary
///   (a malformed sentinel from discovery, a carried-dimension
///   violation from a publish); derived `Ord` fell through to comparing
///   their reason STRINGS, so which one a trader saw was decided by the
///   alphabet and the other was never reported at all.
///
/// The `load` lane is keyed by BATCH, not by source (NEW-6). A
/// `Degraded` generation stays LIVE AND QUERYABLE — that is the whole
/// difference between `Degraded` and `Failed` — so batch `BK1`
/// publishing cleanly says nothing whatever about the degraded rows
/// batch `BK0` is still serving, and must not clear them. The source's
/// load value is the worst across its batches, and only a batch's own
/// next publish replaces its entry. The map grows one entry per batch
/// name ever published for the source (books, or one per file for a
/// source with no `batch_pattern`) and never shrinks: bounded by the
/// source's own batch vocabulary, tens of entries, not a leak.
#[derive(Debug, Clone, Default)]
struct Lanes {
    /// What discovery alone currently believes. `None` until the first
    /// discovery report for this source.
    discovery: Option<LaneValue>,
    /// What each BATCH's last publish (or load failure) reported, keyed
    /// by batch. Seeded at `DataService::open` from the health the
    /// catalog persisted for the generations live at open — no time
    /// bound, exactly what `AsOf::Live` serves (`Catalog::live_health`) —
    /// so a restart does not forget a still-live degraded generation;
    /// empty after that only for a source
    /// whose live generations are all clean, until its first load
    /// report.
    load: std::collections::HashMap<String, LaneValue>,
    /// Monotonic within this source. Bumped only when a slot's value
    /// actually changes, and stamped onto that slot — see
    /// [`LaneValue::changed`].
    seq: u64,
    /// The combined value last actually forwarded — distinct from any
    /// slot on its own, so `report_discovery`/`report_load` can tell a
    /// real change in the COMBINED (worst-of-all-slots) value apart
    /// from a slot merely being overwritten with an equally-severe one.
    last_reported: Option<(Health, String)>,
}

/// One slot's current value, its producer's own explanation of it, and
/// when it last changed.
#[derive(Debug, Clone)]
struct LaneValue {
    health: Health,
    /// The detail line this slot's own producer wrote with it (which
    /// file, which batch, what is wrong). It travels WITH the health so
    /// that whoever forwards the combined value forwards the deciding
    /// slot's explanation rather than its own caller's.
    detail: String,
    /// [`Lanes::seq`] at the moment this value last actually CHANGED.
    ///
    /// This is the ruling's `decided_by` in its general form: on an
    /// equal rank, the slot that changed most recently decides. Because
    /// the stamp is per SLOT rather than per lane, the same rule also
    /// orders the `load` lane's batches against each other, which a
    /// single "which lane wrote last" flag could not do.
    changed: u64,
}

/// `Some(stamp)` when `slot` already holds exactly this value — a
/// re-report that changes nothing and so keeps its old stamp.
///
/// This is what stops an equal-rank decision from flapping. With both
/// lanes `Degraded`, every scheduler poll rewrites the discovery slot
/// with the value it already held; were that counted as a change, the
/// caller would win every tie and the reported reason would alternate
/// between the two lanes for as long as both problems stood.
fn unchanged_stamp(slot: Option<&LaneValue>, health: &Health, detail: &str) -> Option<u64> {
    slot.filter(|v| v.health == *health && v.detail == detail)
        .map(|v| v.changed)
}

/// The worse of two slots: higher [`severity_rank`] wins; on an equal
/// rank, the one that changed more recently. Total and deterministic —
/// no two live slots within one [`Lanes`] share a `changed` stamp.
fn worse_of<'a>(a: Option<&'a LaneValue>, b: Option<&'a LaneValue>) -> Option<&'a LaneValue> {
    match (a, b) {
        (None, None) => None,
        (Some(v), None) | (None, Some(v)) => Some(v),
        (Some(incumbent), Some(candidate)) => Some(if displaces(candidate, incumbent) {
            candidate
        } else {
            incumbent
        }),
    }
}

/// Whether `candidate` displaces `incumbent` — the one and only
/// tie-break site.
fn displaces(candidate: &LaneValue, incumbent: &LaneValue) -> bool {
    (severity_rank(&candidate.health), candidate.changed)
        > (severity_rank(&incumbent.health), incumbent.changed)
}

impl Lanes {
    /// The stamp to write onto the slot being reported: the one it
    /// already had when nothing changed, otherwise the next one.
    fn stamp(&mut self, kept: Option<u64>) -> u64 {
        kept.unwrap_or_else(|| {
            self.seq += 1;
            self.seq
        })
    }

    /// The worst slot across both lanes, as its own `(health, detail)`
    /// pair. Pure — it decides nothing about what has been reported.
    fn combined(&self) -> Option<(Health, String)> {
        let load = self
            .load
            .values()
            .fold(None, |worst, v| worse_of(worst, Some(v)));
        worse_of(self.discovery.as_ref(), load).map(|v| match &v.health {
            // An `Ok` has no problem to explain, and its slot's detail
            // is decoration ("BK1: " from a publish, "" from a poll).
            // Normalised away so that WHICH clean slot happens to be
            // deciding cannot re-fire an `Ok` the surface already
            // shows — with the load lane keyed per batch, ten batches
            // publishing cleanly would otherwise send ten `Ok`s.
            Health::Ok => (Health::Ok, String::new()),
            _ => (v.health.clone(), v.detail.clone()),
        })
    }

    /// Offer the combined value to `emit` — `Some(pair)` when it
    /// differs from the one last forwarded, `None` when it does not —
    /// and record it as reported ONLY if `emit` says it was delivered.
    ///
    /// The conditional commit is round 5, re-review finding 2. Round 4
    /// removed the `delivered &&` short-circuit that could skip the
    /// send outright, on the reasoning that a transition recorded as
    /// reported but never sent is lost for good: nothing re-reports it,
    /// because an identical later report keeps its stamp and combines
    /// to the same pair, so `combined == last_reported` from then on
    /// and the entity keeps the PRE-transition value. A refused
    /// `try_send` — the event channel momentarily full — loses it the
    /// same way, so the commit waits on delivery too. `emit`'s verdict
    /// must be the HEALTH send's alone: the ingest sink's own return
    /// value also carries whether the paired `Published` event landed,
    /// and conflating the two would withhold a health transition that
    /// did arrive.
    fn offer(&mut self, emit: impl FnOnce(Option<(Health, String)>) -> bool) -> bool {
        let combined = self.combined();
        if combined == self.last_reported {
            return emit(None);
        }
        // `combined` is always `Some` here: every door writes a slot
        // before offering, so `combined()` can only be `None` while
        // both lanes are empty, which is also the only time
        // `last_reported` is `None` — the two are equal in that case
        // and returned above.
        let delivered = emit(combined.clone());
        if delivered {
            self.last_reported = combined;
        }
        delivered
    }
}

#[derive(Default)]
struct HealthTracker {
    sources: std::sync::Mutex<std::collections::HashMap<String, Lanes>>,
}

impl HealthTracker {
    /// The scheduler sink's door: `Ok` on a clean poll, the worst
    /// pending/orphaned state discovery found otherwise. `Some(pair)`
    /// exactly when the COMBINED (worst-of-all-slots) value changes —
    /// which is not the same as this lane's own value changing — and
    /// the pair is the DECIDING slot's, which may well be the load
    /// lane's, so the caller must forward it verbatim rather than its
    /// own `(worst, detail)`.
    ///
    /// Test-only, because production must not be able to hold a
    /// decision without emitting it — see
    /// [`HealthTracker::report_discovery_and_emit`], which this is the
    /// always-delivered case of, so every deciding line below is the
    /// same code either door runs.
    #[cfg(test)]
    fn report_discovery(
        &self,
        source: &str,
        health: Health,
        detail: String,
    ) -> Option<(Health, String)> {
        let mut reported = None;
        self.report_discovery_and_emit(source, health, detail, |pair| {
            reported = pair;
            true
        });
        reported
    }

    /// The scheduler sink's real door: decide, then emit inside `emit`,
    /// as ONE step.
    ///
    /// Round 4's adversarial pass. Deciding and emitting must not be
    /// separable: the scheduler thread and the ingest runner thread
    /// report independently (the runner drains its queue while the
    /// scheduler polls on, so a publish concurrent with a poll is
    /// ordinary, not exotic), and if the lock were dropped between the
    /// two, their two decisions could reach the entity in the OPPOSITE
    /// order to the one they were made in. `Diagnostics::note_health` is
    /// last-write-wins, so the entity would then latch the OLDER value
    /// while this tracker believed the newer one had been reported —
    /// and nothing re-reports, so it would stay wrong until the next
    /// real transition, in either direction including false-clean.
    ///
    /// `emit` runs with the tracker's own lock held, and returns
    /// whether its `DataEvent::Health` was DELIVERED — the commit waits
    /// on that (see [`Lanes::offer`]).
    ///
    /// Holding the lock across `emit` is safe because an [`EventSink`]
    /// may not call back into `DataService` (see that type's doc), so
    /// nothing reachable from the callback can take this lock. Note
    /// that this is a NON-REENTRANCY argument, not a non-blocking one
    /// (round 5, re-review finding 5): the callback's `try_send` is
    /// indeed non-blocking, but it also calls `log_health_event`, and
    /// production installs a synchronous stderr writer, so a slow log
    /// write can serialise the two reporter threads here. Accepted —
    /// neither of them is the render thread, and the alternative
    /// (releasing the lock to log) is the reordering this door exists
    /// to prevent.
    fn report_discovery_and_emit(
        &self,
        source: &str,
        health: Health,
        detail: String,
        emit: impl FnOnce(Option<(Health, String)>) -> bool,
    ) -> bool {
        let mut sources = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        let lanes = sources.entry(source.to_string()).or_default();
        let kept = unchanged_stamp(lanes.discovery.as_ref(), &health, &detail);
        let changed = lanes.stamp(kept);
        lanes.discovery = Some(LaneValue {
            health,
            detail,
            changed,
        });
        lanes.offer(emit)
    }

    /// The ingest sink's door: every publish's health (`Ok` included)
    /// and every load failure, under the BATCH it belongs to (NEW-6 —
    /// one batch's clean publish clears only that batch). Returns the
    /// deciding slot's pair on a real transition, on the same terms as
    /// [`HealthTracker::report_discovery`] — and test-only for the same
    /// reason.
    #[cfg(test)]
    fn report_load(
        &self,
        source: &str,
        batch: &str,
        health: Health,
        detail: String,
    ) -> Option<(Health, String)> {
        let mut reported = None;
        self.report_load_and_emit(source, batch, health, detail, |pair| {
            reported = pair;
            true
        });
        reported
    }

    /// The ingest sink's real door — decide and emit as one step, for
    /// the reason [`HealthTracker::report_discovery_and_emit`] gives.
    fn report_load_and_emit(
        &self,
        source: &str,
        batch: &str,
        health: Health,
        detail: String,
        emit: impl FnOnce(Option<(Health, String)>) -> bool,
    ) -> bool {
        let mut sources = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        let lanes = sources.entry(source.to_string()).or_default();
        let kept = unchanged_stamp(lanes.load.get(batch), &health, &detail);
        let changed = lanes.stamp(kept);
        lanes.load.insert(
            batch.to_string(),
            LaneValue {
                health,
                detail,
                changed,
            },
        );
        lanes.offer(emit)
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

        // Phase 4b's deferred gap 2 (spec §4.4): seed the LOAD lane from
        // what the catalog persisted, before anything else can speak for
        // these sources. The lane is otherwise in-process only, so a
        // restart forgot a still-live degraded generation completely —
        // nothing republishes a file that has not changed, so the first
        // content-blind discovery poll's `Ok` was the only word on the
        // source, and it read `ok` while the blotter summed degraded
        // rows.
        //
        // Placed HERE, before `Scheduler::spawn`, for that ordering: the
        // seed must be in the tracker before the first poll reports, or
        // the poll's `Ok` becomes the last-reported value and the seed
        // that follows it is a spurious transition rather than the
        // state.
        //
        // Ruling: keyed by SOURCE (as the whole tracker is) but read per
        // DATASET, which is the only grain the catalog records. Two
        // sources on one dataset therefore both get the same seed, and a
        // dataset with no configured source gets none — there is no
        // source key to file it under. That over-reports (a source is
        // told about a sibling's degraded batch) and never false-cleans,
        // which is the direction this whole seam has been fixed in five
        // times.
        //
        // A `StoreError` here is propagated, not swallowed: the failure
        // mode of a swallowed one is a service that opens quietly and
        // reports clean.
        //
        // Each dataset is read ONCE and its result fanned out to every
        // source configured for it, rather than re-running the query per
        // source: the answer depends only on the dataset, and two sources
        // on one dataset would otherwise run the identical two-window
        // join twice at open.
        //
        // One `DataEvent::Health` per unhealthy batch reaches the sink
        // here, not one per source: with several degraded batches the
        // entity's final value is the worst of them (the tracker
        // combines before it emits), but the startup log carries a line
        // for each transition along the way.
        let datasets: std::collections::BTreeSet<&str> =
            config.sources.iter().map(|s| s.dataset.as_str()).collect();
        for dataset in datasets {
            let unhealthy = Catalog::new(&conn).live_health(dataset)?;
            for spec in config.sources.iter().filter(|s| s.dataset == dataset) {
                for (batch, health) in &unhealthy {
                    let (_, reason) = health.to_parts();
                    let detail = format!("{batch}: {}", reason.unwrap_or_default());
                    let source = spec.name.clone();
                    let sink = Arc::clone(&sink);
                    // The ingest sink's `Published` arm's emit closure,
                    // verbatim: the same door, the same log line, the same
                    // verbatim forwarding of the DECIDING slot's pair.
                    //
                    // Its `bool` is discarded for the same reason the
                    // sinks' own callers stopped acting on one (Task 1):
                    // a refused send means only "not delivered", the
                    // tracker did not commit the transition, and the next
                    // report of this source offers it again.
                    health_tracker.report_load_and_emit(
                        &spec.name,
                        batch,
                        health.clone(),
                        detail,
                        |reported| match reported {
                            Some((worst, detail)) => {
                                log_health_event(&source, &worst, &detail);
                                sink(DataEvent::Health {
                                    source: source.clone(),
                                    worst,
                                    detail,
                                })
                            }
                            None => true,
                        },
                    );
                }
            }
        }

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
                    //
                    // NEW-4 (final review round 3): the LOAD lane
                    // specifically — see `HealthTracker`'s own doc for
                    // why a publish's clean `Ok` and a discovery poll's
                    // clean `Ok` are no longer interchangeable.
                    //
                    // NEW-5/NEW-6 (round 4): under this BATCH's key, and
                    // the returned `(worst, detail)` pair is forwarded
                    // VERBATIM — it is the deciding slot's, which may be
                    // the discovery lane's (a stuck file this publish
                    // knows nothing about), and attaching this publish's
                    // own detail to it named the wrong file.
                    let reason = match &health {
                        Health::Degraded { reason } | Health::Failed { reason } => reason.clone(),
                        _ => String::new(),
                    };
                    // The closure's verdict is the HEALTH send's alone,
                    // never `delivered && …` (round 5, finding 2): the
                    // tracker commits on it, and folding in whether the
                    // paired `Published` event landed would withhold a
                    // health transition that did arrive. `&&` here also
                    // short-circuits, which would skip the send
                    // outright. The two are combined afterwards, for
                    // the runner's own "was this delivered" answer —
                    // which, since Task 1, the runner logs rather than
                    // exits on.
                    let health_delivered = health_tracker.report_load_and_emit(
                        &source,
                        &batch,
                        health,
                        format!("{batch}: {reason}"),
                        |reported| match reported {
                            Some((worst, detail)) => {
                                log_health_event(&source, &worst, &detail);
                                sink(DataEvent::Health {
                                    source: source.clone(),
                                    worst,
                                    detail,
                                })
                            }
                            None => true,
                        },
                    );
                    delivered && health_delivered
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
                    // re-send. NEW-4 (round 3): the LOAD lane — a load
                    // failure is content-aware, the same as any other
                    // publish outcome, never discovery's concern.
                    health_tracker.report_load_and_emit(
                        &source,
                        &batch,
                        Health::Failed {
                            reason: reason.clone(),
                        },
                        format!("{batch}: {reason}"),
                        |reported| match reported {
                            Some((worst, detail)) => sink(DataEvent::Health {
                                source: source.clone(),
                                worst,
                                detail,
                            }),
                            None => true,
                        },
                    )
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
                    //
                    // NEW-4 (final review round 3): the DISCOVERY lane —
                    // a clean poll here says only "nothing looks stuck or
                    // malformed on disk right now", never "the last
                    // publish was clean". Writing this lane can never by
                    // itself clear a load-set `Degraded`/`Failed`; see
                    // `HealthTracker`'s own doc comment.
                    //
                    // NEW-5 (round 4): the returned `(worst, detail)`
                    // pair is forwarded VERBATIM. It is the deciding
                    // slot's, which is often the LOAD lane's — a clean
                    // poll's own detail is the empty string, so
                    // attaching it to a load-set `Degraded` published
                    // the word "degraded" with no reason at all.
                    health_tracker.report_discovery_and_emit(&source, worst, detail, |reported| {
                        match reported {
                            Some((worst, detail)) => {
                                log_health_event(&source, &worst, &detail);
                                sink(DataEvent::Health {
                                    source: source.clone(),
                                    worst,
                                    detail,
                                })
                            }
                            None => true,
                        }
                    })
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

    /// The document request (market-data spec §7): compiled by
    /// `compile_document` and submitted like a view query, so it shares
    /// the pool's cancellation and per-key coalescing and comes back as
    /// an ordinary `DataEvent::Query` — the tile route is unchanged.
    pub fn document(&self, params: &DocumentParams) -> Result<QueryId, StoreError> {
        let compiled = compile_document(&self.conn, &self.config.schema, params)?;
        let catalog = Catalog::new(&self.conn);
        let freshness = match &params.as_of {
            AsOf::Live => Freshness {
                dataset: params.dataset.clone(),
                as_of: catalog
                    .dataset_as_of(&params.dataset, &[])?
                    .map(|t| t.to_rfc3339()),
                generation: catalog.latest_gen_id()?,
            },
            AsOf::At(_) => Freshness {
                dataset: params.dataset.clone(),
                // The generation actually resolved for *this* document,
                // never the requested instant — the same stalest-input
                // rule live freshness applies elsewhere (§5.4).
                as_of: compiled
                    .resolved_as_of
                    .get(&params.dataset)
                    .map(|t| t.to_rfc3339()),
                // Per-document, so no dataset-wide generation id applies.
                generation: 0,
            },
        };
        let provenance = Provenance {
            datasets: vec![freshness],
            as_of_request: match &params.as_of {
                AsOf::Live => None,
                AsOf::At(t) => Some(t.to_rfc3339()),
            },
        };
        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            view: ViewId(format!(
                "document:{}:{}",
                params.dataset,
                params.document_key.join("/")
            )),
            grouping: Vec::new(),
            compiled,
            provenance,
            kind: RequestKind::Query,
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
    ///
    /// The table list is `ddl::history_of`, the one place a dataset's
    /// tables are named, so both families are covered: built from
    /// `ds.grains()` here instead, a document dataset (which declares no
    /// grain) scanned nothing and reported `None` — no bound, so no time
    /// travel — however much history it held.
    pub fn as_of_bounds(&self, dataset: &str) -> Result<Option<DateTime<Utc>>, StoreError> {
        let Some(ds) = self.config.schema.dataset(dataset) else {
            return Ok(None);
        };
        let mut oldest: Option<DateTime<Utc>> = None;
        for table in crate::store::ddl::history_of(&ds.name, ds) {
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

    const CVI: &str = r#"
[cvi_params]
family = "document"
key = ["underlying_ref"]
axes = ["term", "node"]
[cvi_params.columns.underlying_ref]
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
[cvi_params.columns.anchor_date]
type = "date"
role = "attribute"
[cvi_params.columns.spot_ref]
type = "f64"
role = "attribute"
"#;

    fn cvi() -> geode_core::schema::DatasetSpec {
        let doc = geode_core::config::merge_docs(
            "datasets",
            &[geode_core::config::LayerDoc::builtin("datasets", CVI).unwrap()],
        );
        let (schema, diags) = SchemaSpec::from_doc(&doc);
        assert!(diags.is_empty(), "{diags:?}");
        schema.dataset("cvi_params").unwrap().clone()
    }

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn d(s: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// Two terms x three nodes, term-major — the same shape
    /// `store::document`'s own tests use.
    fn doc(key: &str, params: [f64; 6]) -> geode_core::document::DocumentRows {
        use geode_core::document::{Column, DocumentRows, Value};
        DocumentRows {
            key: vec![key.into()],
            attributes: vec![
                ("anchor_date".into(), Value::Date(d("2026-09-12"))),
                ("spot_ref".into(), Value::F64(7650.0)),
            ],
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
            values: vec![("param".into(), Column::F64(params.to_vec()))],
        }
    }

    /// A service opened over a document dataset, with SPX.Z published
    /// twice — the CVI fixture `query::document`'s own tests use, built
    /// through `publish_document` on a scratch `Store` (dropped before
    /// the service opens its own connection to the same file, the same
    /// two-phase shape `service()` above uses for the measure family).
    fn document_service() -> (
        tempfile::TempDir,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = cvi();
        store.apply_schema(&ds).unwrap();
        crate::store::catalog::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        crate::store::document::publish_document(
            &store,
            &crate::store::document::DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &doc("SPX.Z", [1., 2., 3., 4., 5., 6.]),
                source_time: ts("2026-09-12T14:00:00Z"),
                received_at: ts("2026-09-12T14:00:00Z"),
                bytes: 0,
            },
        )
        .unwrap();
        crate::store::document::publish_document(
            &store,
            &crate::store::document::DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &doc("SPX.Z", [10., 20., 30., 40., 50., 60.]),
                source_time: ts("2026-09-12T14:05:00Z"),
                received_at: ts("2026-09-12T14:05:00Z"),
                bytes: 0,
            },
        )
        .unwrap();
        drop(store);

        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: Vec::new(),
        })
        .unwrap();
        (dir, service, rx)
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
    fn a_document_request_returns_the_document_as_a_query_outcome() {
        let (_dir, svc, rx) = document_service();
        let p = DocumentParams {
            key: QueryKey(3),
            tag: 9,
            submitted: Instant::now(),
            dataset: "cvi_params".into(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        };
        svc.document(&p).unwrap();
        let out = next(&rx);
        assert_eq!((out.key, out.tag), (QueryKey(3), 9));
        let snap = out.snapshot.unwrap();
        assert_eq!(snap.rows(), 6);
        assert_eq!(snap.f64_value("param", 0), Some(10.0));
        assert_eq!(snap.text_value("underlying_ref", 0), Some("SPX.Z"));
        assert_eq!(snap.provenance().datasets[0].dataset, "cvi_params");
        assert!(snap.provenance().datasets[0].as_of.is_some());
        let bad = DocumentParams {
            dataset: "nonesuch".into(),
            ..p.clone()
        };
        assert!(svc.document(&bad).is_err());
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

    /// NEW-4 (final review round 3) — the specific gap the round-2
    /// re-review named: every round-2 integration test held
    /// `poll_interval` at 3600s (so only the cold-start poll ever ran)
    /// or used a stray file discovery alone could never clear, which
    /// meant none of them ever let a SECOND, genuinely clean scheduler
    /// poll fire after a degraded publish — exactly the path that let a
    /// content-blind "nothing looks stuck" poll silently overwrite a
    /// real, unfixed `Degraded` back to `Ok`. This test deliberately
    /// lets several more polls fire (a short `poll_interval`) after the
    /// degraded publish, with the bad file never replaced, and asserts
    /// the entity never reads `Ok` again.
    #[test]
    fn a_degraded_publish_survives_several_more_clean_discovery_polls() {
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
                // Short, and deliberately so — several more polls MUST
                // fire during this test's run, each one discovering the
                // same still-bad file as `Unchanged` (not `Ready`, not
                // `Pending`) and reporting a content-blind `Ok`.
                poll_interval: Duration::from_millis(30),
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
        assert!(saw_degraded, "setup: the file must degrade");

        // ~20 more poll intervals' worth of wall time: several more
        // clean discovery polls WILL fire (nothing about the bad file
        // changed, so it is `Unchanged`, not stuck) — none of them may
        // ever read as Health::Ok for this source.
        let mut saw_ok = false;
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            if let Ok(DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            }) = rx.recv_timeout(Duration::from_millis(100))
                && source == "eod_risk"
            {
                saw_ok = true;
            }
        }
        assert!(
            !saw_ok,
            "a routine, content-blind discovery poll must never clear a \
             real, unfixed degraded publish back to Ok"
        );
        svc.shutdown();
    }

    /// The `carried_schema` source, over `src`, polling at `poll`.
    fn carried_source(src: &std::path::Path, poll: Duration) -> crate::source::SourceSpec {
        crate::source::SourceSpec {
            name: "eod_risk".into(),
            dataset: "risk_snapshot".into(),
            paths: vec![format!("{}/*.csv", src.display())],
            readiness: crate::source::Readiness::Sentinel,
            priority: crate::source::Priority::LatestRisk,
            poll_interval: poll,
            pending_timeout: Duration::from_secs(3600),
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
        }
    }

    /// One file's worth of rows for `carried_schema`, with its sentinel.
    ///
    /// The CSV is written aside and renamed, then the sentinel is
    /// (re)written — the order an operator's drop has to use, because
    /// `discovery::classify` reads a sentinel OLDER than its CSV as "the
    /// file is being rewritten" and holds the file `Pending`. Calling
    /// this a second time with different rows is therefore a corrected
    /// republish: same sentinel `as_of`, so publish's strictly-older
    /// backfill guard replaces the live generation rather than filing
    /// history.
    fn write_carried_csv(src: &std::path::Path, rows: &str) {
        let tmp = src.join("risk_2026-08-24_BK0.csv.partial");
        std::fs::write(
            &tmp,
            format!("Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n{rows}"),
        )
        .unwrap();
        std::fs::rename(&tmp, src.join("risk_2026-08-24_BK0.csv")).unwrap();
        std::fs::write(
            src.join("risk_2026-08-24_BK0.csv.done"),
            r#"{"as_of":"2026-08-24T07:00:00Z","columns":["Book","LHU","PositionRef","Counterparty","InstrumentRef","Currency","NPV"],"books":["BK0"]}"#,
        )
        .unwrap();
    }

    fn carried_config(
        db_path: PathBuf,
        src: &std::path::Path,
        poll: Duration,
    ) -> DataServiceConfig {
        let mut schema = SchemaSpec::default();
        schema.datasets.push(carried_schema());
        DataServiceConfig {
            db_path,
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![carried_source(src, poll)],
        }
    }

    /// Run `svc` until a `Health` event for `eod_risk` whose worst is not
    /// `Ok` arrives, or the deadline passes.
    fn await_unhealthy(
        rx: &std::sync::mpsc::Receiver<DataEvent>,
        within: Duration,
    ) -> Option<Health> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(DataEvent::Health {
                    source,
                    worst: Health::Ok,
                    ..
                }) => assert_eq!(source, "eod_risk"),
                Ok(DataEvent::Health { source, worst, .. }) => {
                    assert_eq!(source, "eod_risk");
                    return Some(worst);
                }
                Ok(_) | Err(_) => {}
            }
        }
        None
    }

    #[test]
    fn a_restart_seeds_the_load_lane_from_a_still_live_degraded_generation() {
        // Phase 4b's deferred gap 2 (spec §4.4): the load lane was in
        // process only. `file_generations.health` persisted the
        // degradation, but nothing read it back, so after a restart the
        // first poll found the CSV `Unchanged` (nothing republishes),
        // the tracker was empty, and the scheduler's content-blind `Ok`
        // was the only word on the source — reading `ok` while the
        // blotter summed the still-live degraded rows.
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let db_path = db.path().join("geode.duckdb");
        write_carried_csv(
            src.path(),
            "BK0,L0,P1,C,I1,USD,100\nBK0,L0,P1,C,I1,EUR,100\n",
        );

        // First run: publish the degraded generation, then go away.
        {
            let (svc, rx) = DataService::open_channel(carried_config(
                db_path.clone(),
                src.path(),
                Duration::from_secs(3600),
            ))
            .unwrap();
            let worst = await_unhealthy(&rx, Duration::from_secs(30));
            assert!(
                matches!(worst, Some(Health::Degraded { .. })),
                "setup: the file must degrade, got {worst:?}"
            );
            svc.shutdown();
            // Dropped, not merely shut down: the reopen below is a real
            // second `Store::open` on the same file, which cannot happen
            // while this service still holds the database handle.
            drop(svc);
        }

        // The restart. Nothing on disk changed, so nothing republishes —
        // which is what makes the seed the only possible source of a
        // `Degraded` here.
        let (svc, rx) = DataService::open_channel(carried_config(
            db_path,
            src.path(),
            Duration::from_millis(30),
        ))
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seeded = None;
        while Instant::now() < deadline && seeded.is_none() {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(DataEvent::Published { .. }) => {
                    panic!("nothing changed on disk: a republish would make this test a tautology")
                }
                Ok(DataEvent::Health {
                    source,
                    worst: Health::Degraded { reason },
                    detail,
                }) => {
                    assert_eq!(source, "eod_risk", "keyed by the source, not the dataset");
                    assert!(reason.contains("currency"), "{reason}");
                    assert!(
                        detail.starts_with("BK0: "),
                        "the batch's own detail: {detail}"
                    );
                    seeded = Some(reason);
                }
                Ok(_) | Err(_) => {}
            }
        }
        assert!(
            seeded.is_some(),
            "a restart must re-report the still-live degraded generation"
        );

        // And the tracker HOLDS it: the polls that keep firing here are
        // content-blind `Unchanged` polls, exactly the ones NEW-4 showed
        // must never clear a load-set problem.
        let mut saw_ok = false;
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            if let Ok(DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            }) = rx.recv_timeout(Duration::from_millis(100))
                && source == "eod_risk"
            {
                saw_ok = true;
            }
        }
        assert!(
            !saw_ok,
            "the seeded load lane must outlive a clean discovery poll, \
             the same as a freshly published one"
        );
        svc.shutdown();
    }

    #[test]
    fn a_seeded_batch_is_cleared_by_that_batchs_own_corrected_republish() {
        // Fix round 1, MIN-2. The seed's key must be exactly the key a
        // publish writes (`WorkItem::batch`), or the seeded `Degraded` is
        // unclearable: the operator fixes the file, the republish clears
        // its own key, and the seeded one sits beside it reporting a
        // problem that no longer exists — for the rest of the session,
        // and again on the next restart. That is MAJ-2's stuck-forever
        // failure mode arriving through the seed.
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let db_path = db.path().join("geode.duckdb");
        write_carried_csv(
            src.path(),
            "BK0,L0,P1,C,I1,USD,100\nBK0,L0,P1,C,I1,EUR,100\n",
        );

        {
            let (svc, rx) = DataService::open_channel(carried_config(
                db_path.clone(),
                src.path(),
                Duration::from_secs(3600),
            ))
            .unwrap();
            assert!(
                matches!(
                    await_unhealthy(&rx, Duration::from_secs(30)),
                    Some(Health::Degraded { .. })
                ),
                "setup: the file must degrade"
            );
            svc.shutdown();
            drop(svc);
        }

        let (svc, rx) = DataService::open_channel(carried_config(
            db_path,
            src.path(),
            Duration::from_millis(30),
        ))
        .unwrap();
        assert!(
            matches!(
                await_unhealthy(&rx, Duration::from_secs(10)),
                Some(Health::Degraded { .. })
            ),
            "setup: the restart must seed the degraded batch"
        );

        // The correction: one currency per instrument key, dropped the
        // way an operator drops one (CSV then sentinel, same `as_of`), so
        // publish's strictly-older backfill guard replaces the live
        // generation rather than filing this as history.
        write_carried_csv(src.path(), "BK0,L0,P1,C,I1,USD,100\n");

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut cleared = false;
        while Instant::now() < deadline && !cleared {
            if let Ok(DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            }) = rx.recv_timeout(Duration::from_millis(200))
            {
                assert_eq!(source, "eod_risk");
                cleared = true;
            }
        }
        assert!(
            cleared,
            "a corrected republish of the seeded batch must clear it: the \
             seed's key is the key a publish writes"
        );
        svc.shutdown();
    }

    #[test]
    fn a_restart_after_a_clean_publish_seeds_nothing() {
        // The other half: the seed reads the health of the generation
        // that is LIVE, so a database whose live generations are all
        // clean must produce no problem report at all. Without this,
        // "seed everything the catalog ever recorded" would pass the
        // test above and cry wolf on every restart.
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let db_path = db.path().join("geode.duckdb");
        write_carried_csv(src.path(), "BK0,L0,P1,C,I1,USD,100\n");

        {
            let (svc, rx) = DataService::open_channel(carried_config(
                db_path.clone(),
                src.path(),
                Duration::from_secs(3600),
            ))
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut published = false;
            while Instant::now() < deadline && !published {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(DataEvent::Published { .. }) => published = true,
                    Ok(DataEvent::Health { worst, detail, .. }) => {
                        assert_eq!(worst, Health::Ok, "setup: a clean load — {detail}")
                    }
                    Ok(_) | Err(_) => {}
                }
            }
            assert!(published, "setup: the clean file must publish");
            svc.shutdown();
            drop(svc);
        }

        let (svc, rx) = DataService::open_channel(carried_config(
            db_path,
            src.path(),
            Duration::from_millis(30),
        ))
        .unwrap();
        let worst = await_unhealthy(&rx, Duration::from_millis(600));
        assert!(
            worst.is_none(),
            "a clean database seeds nothing, got {worst:?}"
        );
        svc.shutdown();
    }

    /// The detail line each producer writes alongside a health, in the
    /// shape the real sinks write it (`"{batch}: {reason}"` for the
    /// ingest sink, `worst_health`'s `"{label}: {files}"` for the
    /// scheduler's) — so a test that cares only about the health can go
    /// through `poll`/`publish` below and still exercise the real
    /// detail-carrying path.
    fn detail_of(who: &str, h: &Health) -> String {
        match h {
            Health::Degraded { reason } | Health::Failed { reason } => format!("{who}: {reason}"),
            _ => format!("{who}: {}", h.label()),
        }
    }

    /// A discovery poll, health only.
    fn poll(t: &HealthTracker, source: &str, h: Health) -> Option<Health> {
        let detail = detail_of("poll", &h);
        t.report_discovery(source, h, detail).map(|(h, _)| h)
    }

    /// One batch's publish outcome, health only.
    fn publish(t: &HealthTracker, source: &str, batch: &str, h: Health) -> Option<Health> {
        let detail = detail_of(batch, &h);
        t.report_load(source, batch, h, detail).map(|(h, _)| h)
    }

    #[test]
    fn health_tracker_reports_a_transition_only() {
        let t = HealthTracker::default();
        assert_eq!(
            publish(&t, "a", "BK0", Health::Ok),
            Some(Health::Ok),
            "the first report for a source is always a transition"
        );
        assert_eq!(
            publish(&t, "a", "BK0", Health::Ok),
            None,
            "a repeated identical report is not a transition"
        );
        let degraded = Health::Degraded { reason: "x".into() };
        assert_eq!(
            publish(&t, "a", "BK0", degraded.clone()),
            Some(degraded.clone()),
            "a changed report is a transition"
        );
        assert_eq!(
            publish(&t, "a", "BK0", degraded),
            None,
            "repeating the new state again is not a transition"
        );
        assert_eq!(
            publish(&t, "b", "BK0", Health::Ok),
            Some(Health::Ok),
            "a different source's first report is its own transition, \
             independent of source \"a\""
        );
    }

    /// NEW-4 (final review round 3): the six consequences the ruling
    /// asks to be pinned directly against `HealthTracker`'s two-lane
    /// combine logic — fast and precise, ahead of the slower
    /// integration-level coverage below (which still exists for the
    /// scenario the review specifically flagged as untested at the
    /// `DataService` level: a real second scheduler poll).
    mod health_tracker_lanes {
        use super::*;

        /// The two same-rank reasons round 4's NEW-5 trace uses: an
        /// `Orphaned` sentinel found by discovery and a
        /// carried-dimension violation found by a publish, both
        /// `Degraded`, both real, both needing different action. Their
        /// lexicographic order matters to these tests: `"currency…"`
        /// sorts BEFORE `"expected…"`, so `Health`'s derived `Ord` —
        /// which falls through to the reason string once the variants
        /// tie — picks the discovery one, the opposite of what the
        /// severity-rank-plus-most-recently-changed rule picks when the
        /// load lane moved last.
        fn orphan() -> Health {
            Health::Degraded {
                reason: "expected value at line 1".into(),
            }
        }
        fn carried() -> Health {
            Health::Degraded {
                reason: "currency varies within instrument key I1".into(),
            }
        }

        /// A degraded LOAD, then a CLEAN DISCOVERY poll: the combined
        /// value must stay `Degraded` — a routine, content-blind poll
        /// (`worst_health` sees no stuck/malformed file) is not evidence
        /// the last publish was corrected, and must not clear it.
        #[test]
        fn a_degraded_load_survives_a_clean_discovery_poll() {
            let t = HealthTracker::default();
            let degraded = Health::Degraded { reason: "x".into() };
            assert_eq!(publish(&t, "a", "BK0", degraded.clone()), Some(degraded));
            assert_eq!(
                poll(&t, "a", Health::Ok),
                None,
                "a clean discovery poll must not clear a load-set Degraded"
            );
        }

        /// A degraded LOAD, then a CLEAN LOAD FOR THE SAME BATCH (the
        /// corrected file, republished): only that batch's own clean
        /// publish may clear it.
        #[test]
        fn a_degraded_load_is_cleared_by_a_clean_load() {
            let t = HealthTracker::default();
            let degraded = Health::Degraded { reason: "x".into() };
            assert_eq!(
                publish(&t, "a", "BK0", degraded),
                Some(Health::Degraded { reason: "x".into() })
            );
            assert_eq!(
                publish(&t, "a", "BK0", Health::Ok),
                Some(Health::Ok),
                "a clean republish of the same batch must clear the \
                 earlier degraded load"
            );
        }

        /// Discovery reports `PendingTooLong` while the load lane is
        /// clean: the combined value is the WORSE of the two, so
        /// `PendingTooLong` (discovery's own domain) must still surface
        /// even though nothing about the last publish was wrong.
        #[test]
        fn discovery_pending_too_long_surfaces_over_a_clean_load() {
            let t = HealthTracker::default();
            assert_eq!(publish(&t, "a", "BK0", Health::Ok), Some(Health::Ok));
            assert_eq!(
                poll(&t, "a", Health::PendingTooLong),
                Some(Health::PendingTooLong),
                "PendingTooLong is discovery's own domain and must surface"
            );
        }

        /// The mirror of the first test, worded as the ruling states it:
        /// discovery clearing (a clean poll) while the load lane is
        /// Degraded must leave the combined value at Degraded.
        #[test]
        fn discovery_clearing_does_not_override_a_degraded_load() {
            let t = HealthTracker::default();
            // Discovery starts PendingTooLong (a stuck file), independent
            // of the load lane going Degraded from an unrelated earlier
            // publish for the same source.
            assert_eq!(
                poll(&t, "a", Health::PendingTooLong),
                Some(Health::PendingTooLong)
            );
            let degraded = Health::Degraded {
                reason: "carried-dimension violation".into(),
            };
            assert_eq!(
                publish(&t, "a", "BK0", degraded.clone()),
                Some(degraded),
                "Degraded outranks PendingTooLong, so the load report is \
                 itself a transition"
            );
            assert_eq!(
                poll(&t, "a", Health::Ok),
                None,
                "discovery clearing to Ok must not override the load-set \
                 Degraded — Degraded still outranks Ok"
            );
        }

        /// A FAILED load, then a clean discovery poll: `Failed` is the
        /// worst state and must survive a content-blind clean poll the
        /// same way `Degraded` does.
        #[test]
        fn a_failed_load_survives_a_clean_discovery_poll() {
            let t = HealthTracker::default();
            let failed = Health::Failed {
                reason: "bad header".into(),
            };
            assert_eq!(publish(&t, "a", "BK0", failed.clone()), Some(failed));
            assert_eq!(
                poll(&t, "a", Health::Ok),
                None,
                "a clean discovery poll must not clear a load-set Failed"
            );
        }

        /// NEW-5 (final review round 4): both lanes `Degraded` at once —
        /// a malformed sentinel (discovery's `Orphaned`) AND a
        /// carried-dimension violation from a publish. The second is a
        /// real, distinct finding a trader must act on separately: it
        /// must reach the surface, WITH ITS OWN REASON. Comparing whole
        /// `Health` values instead of severity rank decides this by the
        /// alphabet and drops one of the two findings entirely.
        #[test]
        fn a_second_degradation_at_the_same_rank_is_reported() {
            let t = HealthTracker::default();
            assert_eq!(
                t.report_discovery("a", orphan(), "degraded: bad.csv".into()),
                Some((orphan(), "degraded: bad.csv".into()))
            );
            assert_eq!(
                t.report_load("a", "BK0", carried(), "BK0: currency".into()),
                Some((carried(), "BK0: currency".to_string())),
                "at an equal rank the lane that changed most recently \
                 decides, and it reports ITS OWN reason — a \
                 carried-dimension violation is a new finding, not a \
                 duplicate of the malformed sentinel already showing"
            );
        }

        /// A same-rank change on the DECIDING lane — a second, different
        /// carried-dimension violation replacing the first — is a
        /// transition: the reason is the only thing on the surface that
        /// says which problem the trader has.
        #[test]
        fn a_new_load_reason_at_the_same_rank_is_a_transition() {
            let t = HealthTracker::default();
            t.report_discovery("a", orphan(), "degraded: bad.csv".into());
            t.report_load("a", "BK0", carried(), "BK0: currency".into());
            let renamed = Health::Degraded {
                reason: "lhu varies within instrument key I2".into(),
            };
            assert_eq!(
                t.report_load("a", "BK0", renamed.clone(), "BK0: lhu".into()),
                Some((renamed, "BK0: lhu".to_string())),
                "a different reason at the same severity is a different \
                 finding and must surface"
            );
        }

        /// The ruling's flap check: a standing degraded LOAD and a
        /// source polling cleanly forever reports nothing after the
        /// first transition.
        #[test]
        fn repeated_clean_polls_with_a_standing_degraded_load_report_nothing() {
            let t = HealthTracker::default();
            assert_eq!(
                publish(&t, "a", "BK0", carried()),
                Some(carried()),
                "setup: the degraded publish is the transition"
            );
            for _ in 0..8 {
                assert_eq!(
                    poll(&t, "a", Health::Ok),
                    None,
                    "a clean poll changes nothing and reports nothing"
                );
            }
        }

        /// The other flap check, and the one that pins WHY an identical
        /// re-report must not restamp its slot: with both lanes at the
        /// same rank, "prefer whichever lane is calling" would hand the
        /// decision back and forth on every poll, alternating the
        /// reported reason between two standing problems forever.
        #[test]
        fn repeated_identical_polls_at_the_same_rank_do_not_flap_the_decision() {
            let t = HealthTracker::default();
            t.report_discovery("a", orphan(), "degraded: bad.csv".into());
            assert_eq!(
                t.report_load("a", "BK0", carried(), "BK0: currency".into()),
                Some((carried(), "BK0: currency".to_string())),
                "setup: the load lane changed last, so it decides"
            );
            for _ in 0..8 {
                assert_eq!(
                    t.report_discovery("a", orphan(), "degraded: bad.csv".into()),
                    None,
                    "re-reporting a value the discovery lane already held \
                     changes nothing and must not take the decision back"
                );
            }
        }

        /// A discovery `Degraded` (a malformed sentinel) over a clean
        /// load lane reports DISCOVERY's reason — the deciding lane's,
        /// not the caller's.
        #[test]
        fn a_discovery_degraded_over_a_clean_load_reports_the_discovery_detail() {
            let t = HealthTracker::default();
            assert_eq!(
                t.report_load("a", "BK0", Health::Ok, "BK0: ".into()),
                Some((Health::Ok, String::new()))
            );
            assert_eq!(
                t.report_discovery("a", orphan(), "degraded: bad.csv".into()),
                Some((orphan(), "degraded: bad.csv".to_string())),
                "the malformed sentinel outranks the clean load, and its \
                 own file name is the actionable half"
            );
        }

        /// NEW-5(b): the pair handed back belongs to the DECIDING slot,
        /// which is routinely not the caller's. A stuck stray file
        /// (discovery `PendingTooLong`) stands while a batch fails and
        /// is then fixed: the ingest sink's clean publish gets back
        /// discovery's `PendingTooLong` AND discovery's detail — naming
        /// the stuck file, which is the one thing that can be acted on.
        #[test]
        fn the_reported_pair_comes_from_the_deciding_lane_not_the_caller() {
            let t = HealthTracker::default();
            t.report_discovery(
                "a",
                Health::PendingTooLong,
                "pending_too_long: stray.csv".into(),
            );
            let failed = Health::Failed {
                reason: "bad header".into(),
            };
            assert_eq!(
                t.report_load("a", "BK0", failed.clone(), "BK0: bad header".into()),
                Some((failed, "BK0: bad header".to_string()))
            );
            assert_eq!(
                t.report_load("a", "BK0", Health::Ok, "BK0: ".into()),
                Some((
                    Health::PendingTooLong,
                    "pending_too_long: stray.csv".to_string()
                )),
                "the ingest sink must forward discovery's pair verbatim — \
                 attaching the just-published batch's own detail names the \
                 wrong file and drops the stuck one"
            );
        }

        /// NEW-6 (final review round 4): batch `BK0`'s degraded
        /// generation is LIVE AND QUERYABLE, so batch `BK1` publishing
        /// cleanly says nothing about it and must not clear it.
        #[test]
        fn a_clean_publish_of_one_batch_leaves_another_batchs_degraded_standing() {
            let t = HealthTracker::default();
            assert_eq!(publish(&t, "a", "BK0", carried()), Some(carried()));
            assert_eq!(
                publish(&t, "a", "BK1", Health::Ok),
                None,
                "BK1's clean publish says nothing about BK0's still-live \
                 degraded rows"
            );
            assert_eq!(
                publish(&t, "a", "BK0", Health::Ok),
                Some(Health::Ok),
                "BK0's OWN clean republish is the only thing that clears it"
            );
        }

        /// The worst across batches, not the last one written: two
        /// degraded batches, and clearing one leaves the other showing.
        #[test]
        fn the_load_lane_reports_the_worst_batch_not_the_latest() {
            let t = HealthTracker::default();
            assert_eq!(publish(&t, "a", "BK0", carried()), Some(carried()));
            let failed = Health::Failed {
                reason: "bad header".into(),
            };
            assert_eq!(
                publish(&t, "a", "BK1", failed.clone()),
                Some(failed.clone()),
                "Failed outranks Degraded"
            );
            assert_eq!(
                publish(&t, "a", "BK1", Health::Ok),
                Some(carried()),
                "clearing the worse batch falls back to the other batch's \
                 still-standing Degraded, never to Ok"
            );
        }

        /// A `Failed` batch that later publishes cleanly clears — the
        /// load lane holds each batch's LATEST outcome, so a recovered
        /// file is genuinely recovered.
        #[test]
        fn a_failed_batch_is_cleared_by_its_own_clean_publish() {
            let t = HealthTracker::default();
            let failed = Health::Failed {
                reason: "bad header".into(),
            };
            assert_eq!(publish(&t, "a", "BK0", failed.clone()), Some(failed));
            assert_eq!(
                publish(&t, "a", "BK0", Health::Ok),
                Some(Health::Ok),
                "the same batch loading cleanly is a real recovery"
            );
        }

        /// Both lanes steadily `Ok`: exactly one `Ok` is ever reported,
        /// across many discovery polls AND publishes of several batches,
        /// in either order. A clean `Ok` carries no reason, so which
        /// clean slot happens to be deciding must never re-fire it.
        #[test]
        fn both_lanes_ok_report_exactly_one_ok() {
            let t = HealthTracker::default();
            let mut ok_count = 0;
            for i in 0..10 {
                if poll(&t, "a", Health::Ok).is_some() {
                    ok_count += 1;
                }
                let batch = format!("BK{}", i % 3);
                if i % 3 == 0 && publish(&t, "a", &batch, Health::Ok).is_some() {
                    ok_count += 1;
                }
            }
            assert_eq!(
                ok_count, 1,
                "only the very first report (whichever lane) is a \
                 transition; every later Ok from either lane repeats the \
                 same combined value"
            );
        }

        /// Round 5, re-review finding 2: a transition is recorded as
        /// reported only once the emit says it was DELIVERED. A refused
        /// `DataEvent::Health` — a momentarily full 256-slot event
        /// channel — must be offered again on the next report rather
        /// than silently counted as shown, or the entity keeps the
        /// pre-transition value for good (nothing re-reports; an
        /// identical later poll keeps its stamp and combines to the
        /// same pair). This is the other half of the reasoning that
        /// removed the `delivered &&` short-circuit in round 4: that
        /// closed one of the two ways the send can fail to happen.
        #[test]
        fn a_refused_health_event_is_offered_again_not_recorded_as_reported() {
            let t = HealthTracker::default();
            let degraded = Health::Degraded { reason: "x".into() };
            let pair = Some((degraded.clone(), "BK0: x".to_string()));

            let mut first = None;
            let delivered =
                t.report_load_and_emit("a", "BK0", degraded.clone(), "BK0: x".into(), |reported| {
                    first = reported;
                    false
                });
            assert!(!delivered, "the door reports the emit's own verdict");
            assert_eq!(first, pair, "setup: the transition is offered once");

            let mut second = None;
            let delivered =
                t.report_load_and_emit("a", "BK0", degraded.clone(), "BK0: x".into(), |reported| {
                    second = reported;
                    true
                });
            assert!(delivered);
            assert_eq!(
                second, pair,
                "a refused Health event must be offered again — the \
                 tracker may not record as reported what never reached \
                 the entity"
            );

            let mut third = Some(pair.clone());
            t.report_load_and_emit("a", "BK0", degraded, "BK0: x".into(), |reported| {
                third = Some(reported);
                true
            });
            assert_eq!(
                third,
                Some(None),
                "once it has actually landed, repeating it is not a \
                 transition again"
            );
        }

        /// Round 4's adversarial pass, pinned deterministically (round
        /// 5, on the re-review's recommendation). The scheduler thread
        /// and the ingest runner thread report independently (the
        /// runner drains its queue while the scheduler polls on), so if
        /// a decision and its emission were separable the two could
        /// reach the entity in the opposite order to the one they were
        /// decided in — and `Diagnostics::note_health` is
        /// last-write-wins, so the entity would latch the OLDER value
        /// while this tracker believed the newer one had been reported,
        /// with nothing to re-report it.
        ///
        /// What makes that impossible is the invariant asserted here
        /// directly: `emit` runs with the tracker's lock HELD, so no
        /// second reporter can decide (let alone emit) in between.
        /// `try_lock` returns `Err(WouldBlock)` for a lock held by any
        /// thread, this one included.
        ///
        /// This replaces round 4's two-thread, 50 ms-sleep version.
        /// That one could only fail honestly, but it could pass
        /// spuriously — on a loaded CI box the second thread simply not
        /// having been scheduled looks exactly like it being correctly
        /// blocked, which would hide the regression while the harness
        /// reported the mutation caught.
        #[test]
        fn emit_runs_with_the_tracker_lock_held() {
            let t = HealthTracker::default();
            let mut ran = false;
            t.report_discovery_and_emit(
                "a",
                Health::PendingTooLong,
                "pending_too_long: stray.csv".into(),
                |reported| {
                    assert!(reported.is_some(), "setup: a first report transitions");
                    assert!(
                        t.sources.try_lock().is_err(),
                        "emit must run with the tracker lock held — a second \
                         reporter that could take it here would decide and \
                         emit out of order"
                    );
                    ran = true;
                    true
                },
            );
            assert!(ran, "setup: the emit closure ran at all");

            // And the same for the ingest door, which has its own copy
            // of the lock/emit sequence.
            let mut ran = false;
            t.report_load_and_emit(
                "a",
                "BK0",
                Health::Failed {
                    reason: "bad header".into(),
                },
                "BK0: bad header".into(),
                |reported| {
                    assert!(reported.is_some(), "setup: Failed outranks PendingTooLong");
                    assert!(
                        t.sources.try_lock().is_err(),
                        "the ingest door must hold the lock across its emit too"
                    );
                    ran = true;
                    true
                },
            );
            assert!(ran, "setup: the emit closure ran at all");
        }

        /// Two sources are wholly independent: nothing either lane of
        /// one writes can be read as the other's state.
        #[test]
        fn two_sources_do_not_see_each_others_lanes() {
            let t = HealthTracker::default();
            assert_eq!(publish(&t, "a", "BK0", carried()), Some(carried()));
            assert_eq!(
                poll(&t, "b", Health::Ok),
                Some(Health::Ok),
                "source b's first report is its own, unaffected by a"
            );
            assert_eq!(
                publish(&t, "b", "BK0", Health::Ok),
                None,
                "and b stays Ok while a stays Degraded"
            );
            assert_eq!(
                poll(&t, "a", Health::Ok),
                None,
                "a's own degraded load is still standing"
            );
        }
    }

    /// NEW-6 (final review round 4), superseding round 2's version of
    /// this test (which asserted the OPPOSITE — that ANY clean publish
    /// for the source clears an earlier degraded one, whichever batch
    /// each belonged to): batch `BK0`'s degraded generation stays LIVE
    /// AND QUERYABLE, that being the whole difference between `Degraded`
    /// and `Failed`, so an unrelated batch `BK1` publishing cleanly says
    /// nothing at all about `BK0`'s rows and must not clear them. Only
    /// `BK0`'s own clean republish can
    /// (`a_clean_republish_of_the_same_batch_clears_its_degraded_health`).
    ///
    /// **Isolation from the scheduler's own signal is deliberate.** Any
    /// poll that *discovers* a `Ready` file also, in that same cycle,
    /// computes `worst_health` as `None` over it (`Ready` candidates are
    /// skipped, never counted unhealthy) — so a poll that notices a new
    /// clean file tends to send its own `Ok` before that file has even
    /// finished loading, which would let the SCHEDULER path mask a
    /// broken ingest-sink path rather than this test catching it. Both
    /// files are written and discovered together, in the ONE poll a
    /// 3600s `poll_interval` allows within this test's run — after that,
    /// the only thing that can still change the entity's health is a
    /// PUBLISH. `source_time` (the sentinel's `as_of`) makes the load
    /// order deterministic: the runner's queue sorts newest-first at the
    /// same priority, so the degraded batch (newer `as_of`) loads before
    /// the clean one.
    #[test]
    fn a_clean_publish_of_another_batch_does_not_clear_a_degraded_batch() {
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();

        // batch BK0: currency varies within its instrument key — Degraded.
        // Newer `as_of` than BK1, so it is queued (and loads) first.
        std::fs::write(
            src.path().join("risk_2026-08-25_BK0.csv"),
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK0,L0,P1,C,I1,USD,100\n\
             BK0,L0,P1,C,I1,EUR,100\n",
        )
        .unwrap();
        std::fs::write(
            src.path().join("risk_2026-08-25_BK0.csv.done"),
            r#"{"as_of":"2026-08-25T07:00:00Z","columns":["Book","LHU","PositionRef","Counterparty","InstrumentRef","Currency","NPV"],"books":["BK0"]}"#,
        )
        .unwrap();

        // batch BK1: a clean, non-violating publish for the SAME source.
        // Older `as_of` than BK0, so it loads second.
        std::fs::write(
            src.path().join("risk_2026-08-24_BK1.csv"),
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK1,L0,P2,C,I2,USD,50\n",
        )
        .unwrap();
        std::fs::write(
            src.path().join("risk_2026-08-24_BK1.csv.done"),
            r#"{"as_of":"2026-08-24T07:00:00Z","columns":["Book","LHU","PositionRef","Counterparty","InstrumentRef","Currency","NPV"],"books":["BK1"]}"#,
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
                // Long enough that the cold-start poll is the only one
                // to ever run within this test — see the doc comment
                // above for why that matters.
                poll_interval: Duration::from_secs(3600),
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

        // Wait for BK1 — the CLEAN batch — to actually publish, so this
        // test cannot pass merely by the second publish never happening,
        // and watch for a Health::Ok the whole way there and for a
        // moment after (the sink emits a publish's Health event
        // immediately after its Published event, on the same thread).
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut saw_bk1 = false;
        let mut saw_ok = false;
        let mut drain_until = None;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(DataEvent::Published { batch, .. }) if batch == "BK1" => {
                    saw_bk1 = true;
                    drain_until = Some(Instant::now() + Duration::from_millis(300));
                }
                Ok(DataEvent::Health {
                    source,
                    worst: Health::Ok,
                    ..
                }) if source == "eod_risk" => saw_ok = true,
                _ => {}
            }
            if drain_until.is_some_and(|d| Instant::now() >= d) {
                break;
            }
        }
        assert!(saw_bk1, "setup: the clean batch BK1 must publish");
        assert!(
            !saw_ok,
            "a clean publish of BK1 says nothing about BK0, whose degraded \
             generation is still live and queryable — it must not clear it"
        );
        svc.shutdown();
    }

    /// NEW-6's other half (final review round 4): the batch's OWN clean
    /// republish — the corrected file dropped back into the source
    /// directory — genuinely clears it, all the way through the real
    /// door (discovery, load, publish, tracker, entity event). Without
    /// this, "only that batch can clear it" would be satisfiable by
    /// nothing ever clearing anything.
    ///
    /// A short `poll_interval` here on purpose: a SECOND poll is the
    /// whole point, and — since round 3 — a routine poll can no longer
    /// mask anything, so there is nothing left to isolate the test from.
    #[test]
    fn a_clean_republish_of_the_same_batch_clears_its_degraded_health() {
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let csv = src.path().join("risk_2026-08-24_BK0.csv");
        let sentinel = src.path().join("risk_2026-08-24_BK0.csv.done");

        // Currency varies within the instrument key — Degraded.
        std::fs::write(
            &csv,
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK0,L0,P1,C,I1,USD,100\n\
             BK0,L0,P1,C,I1,EUR,100\n",
        )
        .unwrap();
        std::fs::write(
            &sentinel,
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
                poll_interval: Duration::from_millis(30),
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

        // The corrected file, same name and so the same BATCH, with a
        // newer `as_of` so it is a new generation rather than a stale
        // re-read. Sentinel removed first: a file whose sentinel has not
        // landed is `Pending`, which discovery skips — never a
        // half-written CSV read against its old sentinel.
        std::fs::remove_file(&sentinel).unwrap();
        std::fs::write(
            &csv,
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK0,L0,P1,C,I1,USD,100\n\
             BK0,L0,P2,C,I2,USD,100\n",
        )
        .unwrap();
        std::fs::write(
            &sentinel,
            r#"{"as_of":"2026-08-24T18:00:00Z","columns":["Book","LHU","PositionRef","Counterparty","InstrumentRef","Currency","NPV"],"books":["BK0"]}"#,
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
            "the batch's own corrected republish must clear its degraded \
             health"
        );
        svc.shutdown();
    }

    /// NEW-4 (final review round 3), superseding round 2's version of
    /// this test (which asserted the OPPOSITE — that a clean publish
    /// clears a scheduler-set `PendingTooLong` — an assertion round 3's
    /// ruling explicitly overturns): a source the SCHEDULER reports
    /// `PendingTooLong` for (a permanently stuck stray file, discovery
    /// alone, no load involved) is NOT cleared by an unrelated CLEAN
    /// PUBLISH for the same source. The stray file is still genuinely
    /// stuck — nothing about it changed — so the combined (worse-of-two)
    /// value must stay `PendingTooLong`; only discovery itself observing
    /// the stray file resolve (or vanish) could clear discovery's own
    /// lane.
    #[test]
    fn discovery_pending_too_long_is_not_cleared_by_an_unrelated_clean_publish() {
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

        // A real, ready file for the same source lands and loads cleanly
        // — but the stray file is still there, still stuck.
        let batch = geode_demo_data::generate(&geode_demo_data::GeneratorConfig {
            rows: 50,
            seed: 13,
            business_dates: 1,
        });
        let mut opts = geode_demo_data::EmitOptions::new(src.path());
        opts.leave_one_pending = false;
        geode_demo_data::emit_directory(&batch, &opts).unwrap();

        // Collect everything for several poll intervals: the clean
        // publish itself must still arrive (the load succeeds), but no
        // Health::Ok for this source may ever follow it, since the stray
        // file keeps discovery's own lane at PendingTooLong.
        //
        // Two phases rather than one 800 ms window (fix round 1): the
        // setup half is a real discovery poll, CSV read and publish, and
        // 800 ms of wall clock is not enough for it under a loaded
        // machine — it flaked in a full-suite run, passing every time in
        // isolation. Waiting for the publish on the suite's usual
        // generous bound and only then watching for a spurious `Ok`
        // makes the assertion timing-independent and strictly stronger:
        // the whole publish is now inside the observation window instead
        // of racing its end.
        let mut saw_published = false;
        let mut saw_ok = false;
        let watch = |e, saw_published: &mut bool, saw_ok: &mut bool| match e {
            Ok(DataEvent::Published { .. }) => *saw_published = true,
            Ok(DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            }) if source == "eod_risk" => *saw_ok = true,
            _ => {}
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline && !saw_published {
            watch(
                rx.recv_timeout(Duration::from_millis(100)),
                &mut saw_published,
                &mut saw_ok,
            );
        }
        assert!(saw_published, "setup: the clean file must still load");
        let deadline = Instant::now() + Duration::from_millis(800);
        while Instant::now() < deadline {
            watch(
                rx.recv_timeout(Duration::from_millis(100)),
                &mut saw_published,
                &mut saw_ok,
            );
        }
        assert!(
            !saw_ok,
            "an unrelated clean publish must not clear a stray file's \
             still-genuine PendingTooLong"
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
