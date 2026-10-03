//! `DataService` is the application's door to stored data. Modules submit
//! requests without owning connections, transports, or table names. See
//! `docs/current/data-path.md` for ownership and delivery contracts.

use crate::adapter::{AdapterRegistry, ConnectionState, HealthSink};
use crate::documents::DocumentRegistry;
use crate::egress::{EgressWorkers, UploadOutcome, UploadParams};
use crate::health::{Health, severity_rank};
use crate::ingest::fetch::{FetchOutcome, FetchOutcomeSink, FetchWork, FetchWorker};
use crate::ingest::scheduler::{Scheduler, SchedulerEvent, SchedulerSink};
use crate::ingest::snapshot::{
    PolledSink, SnapshotOutcome, SnapshotPlan, SnapshotSink, SnapshotWorker,
};
use crate::ingest::subscribe::{LoadReportSink, SubscriptionWorker};
use crate::ingest::{
    DocumentJob, ForgetJob, IngestEvent, IngestHandle, IngestRunner, IngestSink, ReferenceJob,
    SeriesJob,
};
use crate::positions::PositionWorker;
use crate::pricing::{PriceSink, PricerConfig, PricingWorker};
use crate::query::as_of::AsOf;
use crate::query::catalog::build_catalog;
use crate::query::pool::{
    Payload, QueryId, QueryPool, QueryRequest, QueryResult, RequestKind, ResultSink, ViewId, Work,
};
use crate::query::read::{ReadConfig, ReadQuery};
use crate::query::series::compile_series;
use crate::source::SourceSpec;
use crate::store::catalog::BookFreshness;
use crate::store::{Catalog, Store, StoreError};
use crate::vol::{VolConfig, VolSink, VolWorker};
use chrono::{DateTime, Utc};
use geode_core::config::{Diagnostic, Severity};
use geode_core::dimensions::DerivedDimensions;
use geode_core::document::check_kind_against;
use geode_core::egress_config::EgressSpec;
use geode_core::positions::{CommandOutcome, MoveLhuParams};
use geode_core::pricing::{LOCAL_SOURCE, LocalPublish, PriceOutcome, PriceParams};
use geode_core::query::{
    CatalogOutcome, CatalogParams, DistinctOutcome, DistinctParams, DocumentParams, QueryKey,
    QueryOutcome, ReferenceOutcome, ReferenceParams,
};
use geode_core::schema::SchemaSpec;
use geode_core::scope::Scope;
use geode_core::series::{SERIES_POINT_CAP, SeriesOutcome, SeriesParams, SlotKind, cap_message};
use geode_core::snapshot::Provenance;
use geode_core::source_config::SourceShape;
use geode_core::view::ViewSpec;
use geode_core::vol::{VolSliceOutcome, VolSliceParams};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// The host's context columns (`ViewSpec::context`), shared between the
/// `DataHandle` that sets them and the service that copies them onto each
/// query's view. A mutex rather than a request: they are set once at
/// startup, read per query, and a request would need its own arm in every
/// request match.
pub type ContextColumns = Arc<Mutex<Vec<String>>>;

pub struct DataServiceConfig {
    pub db_path: PathBuf,
    pub schema: SchemaSpec,
    pub views: Vec<ViewSpec>,
    pub dimensions: DerivedDimensions,
    pub query_workers: usize,
    /// Configured sources. Empty disables source workers; stored-data queries
    /// and local document writes remain available.
    pub sources: Vec<SourceSpec>,
    /// Transports registered by `geode-app`, looked up by each source's adapter
    /// name. An empty registry still permits directory sources; sources requiring
    /// an unavailable adapter are reported as unservable.
    pub adapters: AdapterRegistry,
    /// Document formats registered by `geode-app`. A subscribed source's
    /// `document` key selects one of these kinds.
    pub documents: DocumentRegistry,
    /// Pricing implementation registered by `geode-app`.
    pub pricer: PricerConfig,
    /// Vol model registered by geode-app.
    pub vol: VolConfig,
    /// Upload targets, already passed through `egress::resolve`. Each gets
    /// its own worker thread at open; empty means every upload answers
    /// "unknown target".
    pub egress: Vec<EgressSpec>,
    /// The position service, already passed through `positions::resolve`.
    /// `None` means every position command is refused
    /// `no position service configured`.
    pub positions: Option<geode_core::positions::PositionsSpec>,
    /// The display clock at open. Formats the times inside health reasons
    /// (`N messages dropped since HH:MM:SS`); a later `[time]` reload re-zones
    /// them on restart.
    pub clock: geode_core::clock::Clock,
}

/// Outcomes and state changes delivered through the service's event sink.
#[derive(Debug)]
pub enum DataEvent {
    Query(QueryOutcome),
    /// Series query result, addressed by the requesting tile's key.
    Series(SeriesOutcome),
    /// Picker distinct-values result.
    Distinct(DistinctOutcome),
    /// Catalog metadata for diagnostics and source identities.
    Catalog(CatalogOutcome),
    /// One reference table, live or as of a time, addressed by the
    /// requester's key and tag.
    Reference(ReferenceOutcome),
    /// Pricing result, addressed by the requesting tile's key.
    Price(PriceOutcome),
    /// Vol slice batch result, addressed by the requesting tile's key.
    VolSlices(VolSliceOutcome),
    /// File or document publication. Dataset, batch, and books identify the
    /// partitions whose subscribers need invalidation; the app coalesces bursts.
    Published {
        dataset: String,
        batch: String,
        gen_id: i64,
        books: Vec<Option<String>>,
    },
    /// Fetch completion addressed by `(source, identity)` so every tile watching
    /// the pair can requery. `Ok(0)` is still completion: the span may already
    /// have been covered or the refetch may have appended no changed rows.
    SeriesFetched {
        source: String,
        identity: String,
        result: Result<u64, String>,
    },
    /// Begins ingest progress after the file stale check, or before a document
    /// or series write. Local documents and reference snapshots omit this
    /// event. Ended by LoadEnded.
    Loading {
        source: String,
        path: String,
        queued: usize,
    },
    /// Ends the ingest progress state. Coalesces with `Loading` in the app's
    /// mailbox, so even a burst ending while the UI is busy clears the strip.
    LoadEnded,
    /// Combined source health across discovery and per-batch load outcomes,
    /// with the deciding state's explanation.
    Health {
        source: String,
        worst: Health,
        detail: String,
    },
    /// Poll completion. `next` estimates the next poll using the current
    /// interval; it falls back to `at` if timestamp addition overflows.
    Polled {
        source: String,
        ready: usize,
        at: SystemTime,
        next: SystemTime,
    },
    /// Configuration problems found at open or view reload.
    Diagnostics(Vec<Diagnostic>),
    /// Upload result, addressed by the requesting tile's key. Every
    /// admitted upload request answers exactly one.
    Upload(UploadOutcome),
    /// A position-system command's answer, addressed by the requester's tag.
    /// Every command `DataHandle::move_lhu` admits normally answers exactly
    /// one, including a refusal decided before it reached the position
    /// service; worker and event-delivery failures can prevent that.
    Command(geode_core::positions::CommandOutcome),
    /// A local publish (`DataHandle::publish`) was stored as generation
    /// `gen_id` of document `batch`. Sent beside, not instead of, that
    /// publish's `Published`: this one answers the writer (addressed by
    /// dataset and document key, since a publish carries no requester key),
    /// `Published` invalidates readers.
    LocalPublished {
        dataset: String,
        batch: String,
        gen_id: i64,
    },
    /// A local publish that stored nothing, with the reason. Sent beside the
    /// error `Diagnostics` the failure also produces, including a publish
    /// refused at the service before it reached the writer.
    LocalPublishFailed {
        dataset: String,
        batch: String,
        reason: String,
    },
    /// A local forget (`DataHandle::forget`) deleted document `batch` and its
    /// whole history. A forget of a key nothing held also answers this.
    Forgotten {
        dataset: String,
        batch: String,
    },
    /// A local forget that deleted nothing, with the reason; beside an error
    /// `Diagnostics`. A forget refused at the service (a dataset that is not
    /// local, a key of the wrong arity) answers this too, with its joined
    /// key as the batch, though it never reached the writer.
    ForgetFailed {
        dataset: String,
        batch: String,
        reason: String,
    },
    /// A data thread unwound past every containment boundary and has ended.
    /// Emitted once per thread; nothing restarts it. `thread` is the spawn
    /// name (`geode-data`, `geode-ingest`, `geode-query-2`, ...), `reason` the
    /// panic payload, or the open error for a request loop that never started.
    ThreadStopped {
        thread: String,
        reason: String,
    },
}

/// Forget one document of a `local = true` dataset: every generation, live
/// and archived, is deleted on the ingest writer, in order with publishes.
/// `key` is the document key's parts, as `DocumentRows::key` holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalForget {
    pub dataset: String,
    pub key: Vec<String>,
}

/// Nonblocking delivery into the caller's latest-state mailbox. `false` means
/// the receiver is gone or an alternate caller refused the event; producers
/// continue running after a refusal. The app retains terminal outcomes per key
/// and publication invalidations per partition until its UI drains them.
///
/// Called under the query pool queue lock, so this callback must not wait for
/// the UI or call back into `DataService`. Coalescing state plus a bounded
/// wakeup channel keeps memory proportional to recipients, not event rate.
pub type EventSink = Arc<dyn Fn(DataEvent) -> bool + Send + Sync>;

/// One query, as a module asks for it.
#[derive(Debug, Clone)]
pub struct QueryParams {
    pub key: QueryKey,
    pub tag: u64,
    pub submitted: Instant,
    pub view: String,
    /// Replaces the named view's own grouping for this query: the
    /// frame's active slot or a tile's pin. `None` keeps the
    /// view's.
    pub grouping: Option<Vec<String>>,
    pub scope: Scope,
    pub as_of: AsOf,
    pub max_depth: usize,
}

/// On-demand history request. `key` identifies the requester; completion is
/// addressed by source and identity so other tiles watching the pair also
/// learn the outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchParams {
    pub key: QueryKey,
    pub source: String,
    pub identity: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// The `Snapshot` a `Query` or `Distinct` result must carry. A series
/// payload under either kind is a routing defect, not data: reported as
/// that key's failure rather than unwrapped, so it degrades one tile and
/// leaves the pool running.
fn view_snapshot(payload: Payload) -> Result<geode_core::snapshot::Snapshot, String> {
    match payload {
        Payload::Snapshot(s) => Ok(s),
        Payload::Series(_) => Err("internal: a view query answered with a series".to_string()),
    }
}

/// The event a pool result becomes. A payload of the wrong kind, or a
/// distinct snapshot missing its `value`/`n` columns, is that key's error,
/// never a panic: this runs on a query worker.
fn result_event(r: QueryResult, health_tracker: &HealthTracker) -> DataEvent {
    match r.kind {
        RequestKind::Query => DataEvent::Query(QueryOutcome {
            key: r.key,
            tag: r.tag,
            snapshot: r.payload.and_then(view_snapshot).map(Arc::new),
            submitted: r.submitted,
        }),
        RequestKind::Distinct { column } => DataEvent::Distinct(DistinctOutcome {
            key: r.key,
            tag: r.tag,
            column,
            values: r.payload.and_then(view_snapshot).and_then(|s| {
                let (Some(v), Some(n)) = (s.column_index("value"), s.column_index("n")) else {
                    return Err(
                        "internal: a distinct answer without its value and n columns".to_string(),
                    );
                };
                Ok((0..s.rows())
                    .filter_map(|row| {
                        Some((s.text_at(v, row)?.to_string(), s.i64_at(n, row)? as u64))
                    })
                    .collect())
            }),
        }),
        // Match health to source slots by slot number. Expression slots leave gaps,
        // so positional zipping would attach health to the wrong result.
        RequestKind::Series { pairs } => {
            let result = match r.payload {
                Ok(Payload::Series(mut res)) => {
                    for (slot, source, identity) in &pairs {
                        let key = format!("{identity}@{source}");
                        if let Some(s) = res.slots.iter_mut().find(|s| s.slot == *slot) {
                            s.provenance.health = health_tracker.load_lane(source, &key);
                        }
                    }
                    Ok(res)
                }
                // Report a mismatched worker payload as an error rather than panicking.
                Ok(Payload::Snapshot(_)) => {
                    Err("internal: a series request answered with a snapshot".to_string())
                }
                Err(e) => Err(e),
            };
            DataEvent::Series(SeriesOutcome {
                key: r.key,
                tag: r.tag,
                submitted: r.submitted,
                result,
            })
        }
    }
}

/// Build `r`'s event inside a panic boundary. The pool worker sends what
/// this returns outside its own boundary, so a panic here would end the
/// worker; instead the key is answered with an error of its own kind.
fn contained_result_event(
    r: QueryResult,
    build: impl FnOnce(QueryResult) -> DataEvent,
) -> DataEvent {
    let (key, tag, submitted, kind) = (r.key, r.tag, r.submitted, r.kind.clone());
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        geode_core::panic::contained(|| build(r))
    })) {
        Ok(event) => event,
        Err(payload) => {
            let reason = format!(
                "result delivery panicked: {}",
                crate::ingest::runner::panic_payload_message(payload.as_ref())
            );
            match kind {
                RequestKind::Query => DataEvent::Query(QueryOutcome {
                    key,
                    tag,
                    snapshot: Err(reason),
                    submitted,
                }),
                RequestKind::Distinct { column } => DataEvent::Distinct(DistinctOutcome {
                    key,
                    tag,
                    column,
                    values: Err(reason),
                }),
                RequestKind::Series { .. } => DataEvent::Series(SeriesOutcome {
                    key,
                    tag,
                    submitted,
                    result: Err(reason),
                }),
            }
        }
    }
}

/// A fetch source's identity listing panicked. Nobody asked for it, so the
/// only door is an error diagnostic naming the source and the payload.
fn identity_listing_panicked(source: &str, payload: &str) -> DataEvent {
    DataEvent::Diagnostics(vec![Diagnostic {
        severity: Severity::Error,
        layer: None,
        file: None,
        message: format!("identity listing for {source} panicked: {payload}"),
        path: None,
    }])
}

/// Convert a poll result to its diagnostic event. A user-configured interval
/// can overflow SystemTime; fall back to `at` rather than panic. `next` is
/// an estimate, not a scheduling deadline.
fn polled_event(source: String, ready: usize, at: SystemTime, next_in: Duration) -> DataEvent {
    DataEvent::Polled {
        source,
        ready,
        at,
        next: at.checked_add(next_in).unwrap_or(at),
    }
}

/// Log an ingest failure with dataset, batch, and reason at error level.
fn log_ingest_failure(dataset: &str, batch: &str, reason: &str) {
    tracing::error!(target: "geode::ingest", "{dataset}/{batch}: {reason}");
}

/// Distinct (source, dataset, extra, missing) combinations remembered for
/// load-note warnings. A desk's files share a few header shapes, so the key
/// space is small; the cap bounds a feed whose headers vary without end.
const LOAD_NOTE_CAP: usize = 256;

#[derive(Debug, Default)]
struct LoadNoteLog {
    seen: std::collections::HashSet<(String, String, Vec<String>, Vec<String>)>,
    capped: bool,
}

impl LoadNoteLog {
    /// The warning one load's notes earn: one for a combination not seen
    /// this run (column order aside), none for a repeat. At the cap, one
    /// last warning names the source and file whose new combination hit the
    /// cap and says further combinations go unreported.
    fn note(
        &mut self,
        source: &str,
        dataset: &str,
        notes: &crate::ingest::load::LoadNotes,
    ) -> Option<Diagnostic> {
        let mut extra = notes.extra_columns.clone();
        extra.sort();
        extra.dedup();
        let mut missing = notes.missing_optional.clone();
        missing.sort();
        missing.dedup();
        let key = (source.to_string(), dataset.to_string(), extra, missing);
        if self.seen.contains(&key) {
            return None;
        }
        if self.seen.len() >= LOAD_NOTE_CAP {
            if self.capped {
                return None;
            }
            self.capped = true;
            return Some(load_note_warning(format!(
                "source '{source}' reached {LOAD_NOTE_CAP} distinct load-note combinations \
                 ('{}' into '{dataset}'); further ones are not reported",
                notes.file
            )));
        }
        self.seen.insert(key);
        Some(load_note_warning(load_note_message(dataset, notes)))
    }
}

/// `'<file>' loaded into '<dataset>' with extra columns [a, b] ignored;
/// optional [c] missing, read as NULL`, either half omitted when empty.
fn load_note_message(dataset: &str, notes: &crate::ingest::load::LoadNotes) -> String {
    let mut parts = Vec::new();
    if !notes.extra_columns.is_empty() {
        parts.push(format!(
            "extra columns [{}] ignored",
            notes.extra_columns.join(", ")
        ));
    }
    if !notes.missing_optional.is_empty() {
        parts.push(format!(
            "optional [{}] missing, read as NULL",
            notes.missing_optional.join(", ")
        ));
    }
    format!(
        "'{}' loaded into '{dataset}' with {}",
        notes.file,
        parts.join("; ")
    )
}

fn load_note_warning(message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Warning,
        layer: None,
        file: None,
        message,
        path: None,
    }
}

/// Log source health: Failed at error, Degraded/PendingTooLong at warn,
/// Ok at info, and Pending at debug. Callable independently so tests can
/// verify levels through a scoped subscriber.
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

/// The load-lane slot an ingest backlog report fills: `<source>:backlog`,
/// `Degraded "ingest backlog N"` while over, `Ok` once below.
fn backlog_health(source: &str, queued: usize, over: bool) -> (String, Health, String) {
    let key = crate::health::condition_key(source, crate::health::BACKLOG);
    let reason = format!("ingest backlog {queued}");
    let detail = format!("{key}: {reason}");
    let health = if over {
        Health::Degraded { reason }
    } else {
        Health::Ok
    };
    (key, health, detail)
}

/// Report adapter content outcomes through the shared load lane. Batch keys
/// are the parsed document key (or raw topic for parse failure) for subscribed
/// sources, and `identity@source` for fetches. Failures receive one operation
/// log; recovery logs through the combined health transition.
fn load_report_sink(
    spec: &SourceSpec,
    sink: &EventSink,
    health_tracker: &Arc<HealthTracker>,
) -> LoadReportSink {
    let sink = Arc::clone(sink);
    let health_tracker = Arc::clone(health_tracker);
    let source = spec.name.clone();
    let dataset_name = spec.dataset.clone();
    Arc::new(move |batch: &str, health: Health, detail: String| {
        let failed = match &health {
            Health::Degraded { reason } | Health::Failed { reason } => {
                log_ingest_failure(&dataset_name, batch, reason);
                true
            }
            _ => false,
        };
        health_tracker.report_load_and_emit(&source, batch, health, detail, |reported| {
            match reported {
                Some((worst, detail)) => {
                    if !failed {
                        log_health_event(&source, &worst, &detail);
                    }
                    sink(DataEvent::Health {
                        source: source.clone(),
                        worst,
                        detail,
                    })
                }
                None => true,
            }
        });
    })
}

/// A snapshot source's poll outcome: rows go to the runner (the one door
/// storage is entered by) and the discovery lane is clean; a failed query or
/// refused table degrades the discovery lane only. Publish outcomes report on
/// the load lane under the batch, so a clean poll never clears a failed
/// publish, nor a publish a failed poll.
fn snapshot_outcome_sink(
    spec: &SourceSpec,
    ingest: &Arc<IngestHandle>,
    sink: &EventSink,
    health_tracker: &Arc<HealthTracker>,
) -> SnapshotSink {
    let ingest = Arc::clone(ingest);
    let sink = Arc::clone(sink);
    let health_tracker = Arc::clone(health_tracker);
    let source = spec.name.clone();
    let dataset = spec.dataset.clone();
    Arc::new(move |outcome| {
        let (health, detail, note) = match outcome {
            SnapshotOutcome::Rows {
                rows,
                received_at,
                note,
            } => {
                ingest.submit_reference(ReferenceJob {
                    source: source.clone(),
                    dataset: dataset.clone(),
                    rows,
                    received_at,
                });
                (Health::Ok, String::new(), note)
            }
            // Not logged here: a database that stays down fails every
            // poll, and the discovery lane's transition to `Degraded`
            // below is the one log line, naming the source and reason.
            SnapshotOutcome::Failed { reason } => (
                Health::Degraded {
                    reason: reason.clone(),
                },
                reason,
                None,
            ),
        };
        health_tracker.report_discovery_and_emit(
            &source,
            health,
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
        // Health stays `Ok`: the rows conformed. The note is a warning, once
        // per distinct combination.
        if let Some(message) = note {
            let _ = sink(DataEvent::Diagnostics(vec![Diagnostic {
                path: Some(format!("sources.{source}")),
                ..load_note_warning(message)
            }]));
        }
    })
}

/// Per-source discovery state and load state per batch. The worst severity
/// wins; equal severities use the most recently changed value. Repeating an
/// unchanged report preserves its stamp, so clean polls cannot displace a
/// load failure or make the displayed reason alternate.
///
/// Load entries are retained for every observed batch/key. There is no fixed
/// capacity or eviction policy; memory grows with distinct names.
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
    /// Advance the stamp only when health or detail changes. Repeated polls
    /// must not win a severity tie by arrival time alone.
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

    /// Offer a changed combined state and remember it only if delivery succeeds.
    /// A refused transition is offered again on the next report for this source;
    /// there is no independent retry timer.
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

    /// Update discovery, choose the combined state, and emit under one lock.
    /// Serializing these steps prevents concurrent reporters from delivering an
    /// older transition after a newer one. The sink must not reenter the service
    /// or query pool: result delivery can already hold the pool lock.
    /// Logging here can delay a reporter while the tracker lock is held.
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

    /// Test helper: report a load outcome with delivery assumed successful.
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

    /// Read one source/batch load state without changing it. Series result
    /// delivery may hold the query-pool lock, so tracker callbacks must never
    /// acquire that lock in the opposite order.
    pub(crate) fn load_lane(&self, source: &str, batch: &str) -> Option<Health> {
        let sources = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        sources
            .get(source)?
            .load
            .get(batch)
            .map(|v| v.health.clone())
    }
}

/// Validate every view once, returning the diagnostics to publish and the
/// refusals to enforce.
///
/// Opening and reloading use the same validation rules, so a configuration's
/// acceptance does not depend on when it was loaded.
fn validate_views(
    views: &[ViewSpec],
    schema: &SchemaSpec,
    dimensions: &DerivedDimensions,
) -> (Vec<Diagnostic>, std::collections::BTreeMap<String, String>) {
    let mut diagnostics = Vec::new();
    let mut refused = std::collections::BTreeMap::new();
    for view in views {
        let diags = view.validate(schema, dimensions);
        // The FIRST error is the message the trader sees. Later ones are in
        // the diagnostics panel; repeating them all in a query refusal would
        // bury the one that has to be read.
        if let Some(first) = diags.iter().find(|d| d.severity == Severity::Error) {
            refused.insert(view.name.clone(), first.message.clone());
        }
        diagnostics.extend(diags);
    }
    (diagnostics, refused)
}

pub struct DataService {
    read_config: Arc<ReadConfig>,
    config: DataServiceConfig,
    /// Captured from `open`'s `sink` argument before it is cloned into
    /// the ingest/pool/scheduler closures, so `publish` can send a
    /// refusal diagnostic itself without a request round trip through any
    /// of them, and so `fetch` can answer the asking tile directly —
    /// every early exit of a fetch is a `SeriesFetched`, and the ones
    /// decided here never reach a worker or the runner.
    sink: EventSink,
    /// The source-health lanes, shared with every worker sink, so a fetch
    /// the request loop could not run is failed on the same load lane the
    /// fetch worker reports on.
    health: Arc<HealthTracker>,
    /// Config errors found at open. Held rather than
    /// returned so `open` keeps its signature and a caller that does not
    /// surface diagnostics still gets a working service.
    diagnostics: Vec<Diagnostic>,
    /// Views that failed validation, keyed by name with their first error.
    /// Refusing them before compilation keeps configuration failures visible
    /// even when no diagnostics panel is open.
    refused_views: std::collections::BTreeMap<String, String>,
    /// Datasets whose payload tables drifted at open, with the reason. Reads
    /// of them are refused before compilation (`refuse_drifted`).
    drifted: std::collections::BTreeMap<String, String>,
    /// One worker per upload target. They only answer the sink, so they
    /// stop first and depend on nothing below.
    egress: EgressWorkers,
    /// The position-command worker. Like the upload workers it only answers
    /// the sink, so it stops early and depends on nothing below.
    positions: PositionWorker,
    /// Workers precede their consumers in field drop order. Fetchers and
    /// subscriptions can submit to ingest; they must stop before the writer.
    /// Explicit shutdown follows the same producer-before-consumer order.
    fetchers: std::sync::Mutex<Vec<FetchWorker>>,
    /// One polling worker per snapshot source; each submits its snapshots
    /// to ingest, so it stops with the fetchers, before the writer. Joining
    /// one waits for a poll in flight, which has no deadline, so they are
    /// stopped here on the service thread, never by a UI-thread drop.
    snapshots: std::sync::Mutex<Vec<SnapshotWorker>>,
    /// Allows `shutdown(&self)` to stop and join workers whose shutdown needs
    /// mutable access.
    subscriptions: std::sync::Mutex<Vec<SubscriptionWorker>>,
    /// What each fetch source last answered `Fetch::catalogue` with,
    /// written by the workers' outcome sinks and read by `catalog`.
    /// Sorted and deduplicated on the way in, so the read is a clone.
    identities: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Vec<String>>>>,
    /// Fetch source name -> the series dataset it feeds. The one thing
    /// `fetch` needs to name a coverage table, and the membership test
    /// that makes "is this a fetch source" a lookup rather than a
    /// re-derivation from the schema.
    fetch_datasets: std::collections::HashMap<String, String>,
    /// Copied onto every query's view as `ViewSpec::context`; see
    /// [`ContextColumns`].
    context_columns: ContextColumns,
    pool: QueryPool,
    pricing: PricingWorker,
    vol: VolWorker,
    scheduler: Scheduler,
    ingest: Arc<IngestHandle>,
    /// A dedicated read connection for service-side catalog and coverage reads.
    conn: duckdb::Connection,
}

impl DataService {
    pub fn open(config: DataServiceConfig, sink: EventSink) -> Result<DataService, StoreError> {
        // Captured before every closure below clones `sink` for its own
        // use, so `publish`'s refusal path can send through it directly.
        let stored_sink = Arc::clone(&sink);
        let mut store = Store::open(&config.db_path)?;
        // A computed dataset is answered by a module in process; it owns no
        // table and so has no generation summary to rebuild.
        for ds in config.schema.datasets.iter().filter(|d| !d.computed) {
            store.apply_schema(ds)?;
            // Decided once per run: `datasets` is restart-required, so the
            // recovery (delete the table or fix the dataset) takes a restart.
            if let Some(drift) = store.check_drift(ds)? {
                store.mark_drifted(&ds.name, drift.reason());
            }
        }
        Catalog::new(store.writer()).ensure_tables()?;
        let drifted = store.drifted_all().clone();
        // One error per drifted dataset, once, at open: the diagnostics page
        // names the tables and the recovery even if no tile asks for them.
        if !drifted.is_empty() {
            let _ = sink(DataEvent::Diagnostics(
                drifted
                    .values()
                    .map(|reason| Diagnostic {
                        severity: Severity::Error,
                        layer: None,
                        file: None,
                        message: reason.clone(),
                        path: None,
                    })
                    .collect(),
            ));
        }

        // Rebuild generation summaries only for datasets with payload but no
        // summary entries. This does not validate or repair a partially populated
        // summary; repairing one requires clearing that dataset's summary before
        // reopening.
        for ds in config.schema.datasets.iter().filter(|d| !d.computed) {
            // A drifted table's columns cannot be trusted to rebuild a summary from.
            if drifted.contains_key(&ds.name) {
                continue;
            }
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

        // Create independent readers before transferring the only writer to ingest.
        let conn = store.reader()?;
        let discovery_conn = store.reader()?;
        // All source workers share one health tracker.
        let health_tracker = Arc::new(HealthTracker::default());

        // Seed load health from persisted live file generations before workers
        // start. Unchanged files may never reload, so their degradation must survive
        // restart. Query once per dataset and apply the result to each configured
        // source for it: the catalog does not retain source ownership, so sources
        // sharing a dataset can receive the same conservative degradation.
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
                    // Only acknowledge a seed if its health event was delivered. A refusal
                    // leaves it eligible for emission on the source's next report.
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
            // The tracker rides into the sink so a series result can carry
            // each pair's load-lane word without a second trip through the
            // service thread.
            let health_tracker = Arc::clone(&health_tracker);
            Arc::new(move |r: QueryResult| {
                sink(contained_result_event(r, |r| {
                    result_event(r, &health_tracker)
                }))
            })
        };
        let pool = QueryPool::spawn_with_sink(
            &store,
            config.query_workers.max(1),
            result_sink,
            Arc::clone(&sink),
        )?;

        let price_sink: PriceSink = {
            let sink = Arc::clone(&sink);
            Arc::new(move |o| sink(DataEvent::Price(o)))
        };
        let pricing = PricingWorker::spawn(config.pricer.clone(), price_sink, Arc::clone(&sink));

        let vol_sink: VolSink = {
            let sink = Arc::clone(&sink);
            Arc::new(move |o| sink(DataEvent::VolSlices(o)))
        };
        let vol = VolWorker::spawn(config.vol.clone(), vol_sink, Arc::clone(&sink));

        let ingest_sink: IngestSink = {
            let sink = Arc::clone(&sink);
            let health_tracker = Arc::clone(&health_tracker);
            let load_notes = std::sync::Mutex::new(LoadNoteLog::default());
            Arc::new(move |e: IngestEvent| match e {
                IngestEvent::Started {
                    source,
                    path,
                    queued,
                } => sink(DataEvent::Loading {
                    source,
                    path,
                    queued,
                }),
                IngestEvent::Published {
                    source,
                    dataset,
                    batch,
                    gen_id,
                    books,
                    rows,
                    health,
                    notes,
                } => {
                    // Decided before `dataset` moves into the Published event.
                    let note = notes.as_ref().and_then(|n| {
                        load_notes
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .note(&source, &dataset, n)
                    });
                    tracing::info!(
                        target: "geode::ingest",
                        "published {dataset}/{batch} gen {gen_id}: {} book(s), {rows} row(s)",
                        books.len(),
                    );
                    let local = (source == LOCAL_SOURCE).then(|| DataEvent::LocalPublished {
                        dataset: dataset.clone(),
                        batch: batch.clone(),
                        gen_id,
                    });
                    let delivered = sink(DataEvent::Published {
                        dataset,
                        batch: batch.clone(),
                        gen_id,
                        books,
                    });
                    // The writer's own answer; `Published` above stays the readers' one.
                    let delivered = match local {
                        Some(event) => sink(event) && delivered,
                        None => delivered,
                    };
                    // Local document writes have no configured source-health lane.
                    if source == LOCAL_SOURCE {
                        let _ = sink(DataEvent::LoadEnded);
                        return delivered;
                    }
                    // Report every publication, including a clean correction, against its own
                    // batch. The tracker chooses the worst discovery/load state and its detail;
                    // a clean batch cannot clear another batch's failure.
                    let reason = match &health {
                        Health::Degraded { reason } | Health::Failed { reason } => reason.clone(),
                        _ => String::new(),
                    };
                    // Attempt both events independently. Only the health event's own delivery
                    // verdict acknowledges the health transition.
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
                    // Health stays `Ok`: the data loaded correctly. The note is
                    // a warning, once per distinct combination.
                    if let Some(warning) = note {
                        let _ = sink(DataEvent::Diagnostics(vec![warning]));
                    }
                    // Unconditional, and after the health send: a
                    // failed load's `Health` may be deduplicated away by
                    // the tracker and never reach the shell, so
                    // `LoadEnded` — not `Health` — is what the status
                    // bar's progress strip relies on to know a load is
                    // over.
                    let _ = sink(DataEvent::LoadEnded);
                    delivered && health_delivered
                }
                IngestEvent::Failed {
                    source,
                    dataset,
                    batch,
                    reason,
                } => {
                    // Log each failed operation even if aggregate source health is unchanged.
                    log_ingest_failure(&dataset, &batch, &reason);
                    if source == LOCAL_SOURCE {
                        let delivered = sink(DataEvent::Diagnostics(vec![Diagnostic {
                            severity: Severity::Error,
                            layer: None,
                            file: None,
                            message: format!("local publish of {dataset}/{batch} failed: {reason}"),
                            path: None,
                        }]));
                        let answered = sink(DataEvent::LocalPublishFailed {
                            dataset,
                            batch,
                            reason,
                        });
                        let _ = sink(DataEvent::LoadEnded);
                        return delivered && answered;
                    }
                    // Key health by source name and record this failure in its load lane.
                    let health_delivered = health_tracker.report_load_and_emit(
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
                    );
                    // Unconditional, same reasoning as the `Published`
                    // arm's own `LoadEnded` send above.
                    let _ = sink(DataEvent::LoadEnded);
                    health_delivered
                }
                // A snapshot equal to live wrote nothing, so readers have
                // nothing to requery and no `Published` is sent. It is
                // still a clean load: it clears a failed publish of this
                // batch, since live now matches the source again.
                IngestEvent::Unchanged {
                    source,
                    dataset,
                    batch,
                } => {
                    tracing::debug!(
                        target: "geode::ingest",
                        "{dataset}/{batch} from {source} unchanged",
                    );
                    let health_delivered = health_tracker.report_load_and_emit(
                        &source,
                        &batch,
                        Health::Ok,
                        String::new(),
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
                    // No `LoadEnded`: a snapshot job announces no
                    // `Loading`, so there is no progress to end.
                    health_delivered
                }
                // Series completion is addressed by identity and source, including zero
                // appends. End progress after either outcome.
                IngestEvent::SeriesAppended {
                    source,
                    dataset,
                    identity,
                    appended,
                    swept,
                } => {
                    tracing::info!(
                        target: "geode::ingest",
                        "appended {identity}@{source} into {dataset}: {appended} row(s), {swept} swept",
                    );
                    let pair = format!("{identity}@{source}");
                    let health_delivered = health_tracker.report_load_and_emit(
                        &source,
                        &pair,
                        Health::Ok,
                        String::new(),
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
                    let delivered = sink(DataEvent::SeriesFetched {
                        source: source.clone(),
                        identity,
                        result: Ok(appended as u64),
                    });
                    let _ = sink(DataEvent::LoadEnded);
                    delivered && health_delivered
                }
                IngestEvent::SeriesFailed {
                    source,
                    dataset,
                    identity,
                    reason,
                } => {
                    let pair = format!("{identity}@{source}");
                    log_ingest_failure(&dataset, &pair, &reason);
                    let health_delivered = health_tracker.report_load_and_emit(
                        &source,
                        &pair,
                        Health::Failed {
                            reason: reason.clone(),
                        },
                        format!("{pair}: {reason}"),
                        |reported| match reported {
                            Some((worst, detail)) => sink(DataEvent::Health {
                                source: source.clone(),
                                worst,
                                detail,
                            }),
                            None => true,
                        },
                    );
                    let delivered = sink(DataEvent::SeriesFetched {
                        source: source.clone(),
                        identity,
                        result: Err(reason),
                    });
                    let _ = sink(DataEvent::LoadEnded);
                    delivered && health_delivered
                }
                // A forget is a local write: no health lane and no progress (it
                // announced no Started), so no LoadEnded either.
                IngestEvent::Forgotten {
                    dataset,
                    batch,
                    rows,
                } => {
                    tracing::info!(
                        target: "geode::ingest",
                        "forgot {dataset}/{batch}: {rows} row(s) deleted",
                    );
                    sink(DataEvent::Forgotten { dataset, batch })
                }
                IngestEvent::ForgetFailed {
                    dataset,
                    batch,
                    reason,
                } => {
                    log_ingest_failure(&dataset, &batch, &reason);
                    let delivered = sink(DataEvent::Diagnostics(vec![Diagnostic {
                        severity: Severity::Error,
                        layer: None,
                        file: None,
                        message: format!("forgetting {dataset}/{batch} failed: {reason}"),
                        path: None,
                    }]));
                    sink(DataEvent::ForgetFailed {
                        dataset,
                        batch,
                        reason,
                    }) && delivered
                }
                // A condition the runner reports without failing a job: a
                // stale check that could not read the catalog, a local sweep
                // that panicked.
                IngestEvent::Diagnostic(d) => sink(DataEvent::Diagnostics(vec![d])),
                // One source's queue crossed the backlog depth, or fell back
                // below it: its own `<source>:backlog` load slot.
                IngestEvent::Backlog {
                    source,
                    queued,
                    over,
                } => {
                    let (key, health, detail) = backlog_health(&source, queued, over);
                    health_tracker.report_load_and_emit(&source, &key, health, detail, |reported| {
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
                // A drained runner also ends progress. The runner does not retry refused
                // events; the app mailbox coalesces progress state.
                IngestEvent::PlanComplete => sink(DataEvent::LoadEnded),
            })
        };
        let ingest = Arc::new(IngestRunner::spawn(
            store,
            config.schema.clone(),
            ingest_sink,
            Arc::clone(&sink),
        ));

        // Resolve adapters after the ingest runner exists and before discovery
        // starts. An unservable source reports Failed and is skipped; other sources
        // and stored-data queries remain available.
        let mut subscriptions: Vec<SubscriptionWorker> = Vec::new();
        let mut directory_sources: Vec<SourceSpec> = Vec::new();
        // Fetch sources resolve through the same source configuration loop.
        let mut fetchers: Vec<FetchWorker> = Vec::new();
        let mut snapshots: Vec<SnapshotWorker> = Vec::new();
        let mut fetch_datasets: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let identities: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Vec<String>>>> =
            Default::default();
        for spec in &config.sources {
            // The scheduler sink's own emit closure, verbatim (it is the
            // discovery lane's): the deciding slot's pair forwarded as
            // it comes back, logged at the level the outcome deserves.
            let report_unservable = |reason: String| {
                let source = spec.name.clone();
                let sink = Arc::clone(&sink);
                health_tracker.report_discovery_and_emit(
                    &spec.name,
                    Health::Failed {
                        reason: reason.clone(),
                    },
                    reason,
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
            };
            // A drifted dataset's sources are not started: a running scheduler
            // or receiver would report `Ok` on the discovery lane and clear the
            // `Failed` that must stand until a restart after the fix.
            if let Some(reason) = drifted.get(&spec.dataset) {
                report_unservable(reason.clone());
                continue;
            }
            match spec.shape(&config.schema) {
                SourceShape::Directory => {
                    directory_sources.push(spec.clone());
                    continue;
                }
                SourceShape::Subscribed => {}
                SourceShape::Snapshot => {
                    let Some(adapter) = config.adapters.get(&spec.adapter) else {
                        report_unservable(format!(
                            "adapter '{}' is not in this build",
                            spec.adapter
                        ));
                        continue;
                    };
                    // Asked for per source, never cached, for the reason
                    // the subscribed arm's `subscription()` call records.
                    let Some(query) = adapter.snapshot() else {
                        report_unservable(format!(
                            "adapter '{}' has no snapshot side",
                            spec.adapter
                        ));
                        continue;
                    };
                    // `SourceSpec::from_doc` refuses a snapshot source with
                    // no table and one naming an undeclared dataset; a
                    // config built in code is still reported, not unwrapped.
                    let Some(dataset) = config.schema.dataset(&spec.dataset) else {
                        report_unservable(format!("dataset '{}' is not declared", spec.dataset));
                        continue;
                    };
                    let Some(table) = spec.table.clone() else {
                        report_unservable("a snapshot source needs a table".to_string());
                        continue;
                    };
                    let plan = SnapshotPlan {
                        source: spec.name.clone(),
                        dataset: dataset.clone(),
                        table,
                        interval: spec.poll_interval,
                    };
                    let outcome_sink = snapshot_outcome_sink(spec, &ingest, &sink, &health_tracker);
                    let polled: PolledSink = {
                        let sink = Arc::clone(&sink);
                        let source = spec.name.clone();
                        Arc::new(move |at, next| {
                            let next_in = next.duration_since(at).unwrap_or_default();
                            let _ = sink(polled_event(source.clone(), 0, at, next_in));
                        })
                    };
                    match SnapshotWorker::spawn(
                        plan,
                        query,
                        outcome_sink,
                        polled,
                        Arc::clone(&sink),
                    ) {
                        Ok(worker) => snapshots.push(worker),
                        Err(e) => report_unservable(e.message),
                    }
                    continue;
                }
                SourceShape::Fetch => {
                    let Some(adapter) = config.adapters.get(&spec.adapter) else {
                        report_unservable(format!(
                            "adapter '{}' is not in this build",
                            spec.adapter
                        ));
                        continue;
                    };
                    // Asked for per source, never cached, for the reason
                    // the subscribed arm's `subscription()` call records.
                    let Some(fetch) = adapter.fetch() else {
                        report_unservable(format!("adapter '{}' has no fetch side", spec.adapter));
                        continue;
                    };
                    // The same load-lane door the subscribed arm below
                    // uses: a fetch that failed is a content-aware
                    // outcome, keyed here by the PAIR — the same key
                    // `ingest_sink`'s two series arms use, so a later
                    // success clears this failure.
                    let report_load = load_report_sink(spec, &sink, &health_tracker);
                    // What a fetch worker's outcome becomes: rows go to
                    // the ingest runner (the one door storage is entered
                    // by), a failure is the load lane plus the asking
                    // tile's answer, and a catalogue is remembered for
                    // the next `catalog` read.
                    let outcome_sink: FetchOutcomeSink = {
                        let ingest = Arc::clone(&ingest);
                        let sink = Arc::clone(&sink);
                        let identities = Arc::clone(&identities);
                        let source = spec.name.clone();
                        let dataset = spec.dataset.clone();
                        Arc::new(move |outcome| match outcome {
                            FetchOutcome::Fetched {
                                identity,
                                rows,
                                span,
                                ..
                            } => {
                                ingest.submit_series(SeriesJob {
                                    source: source.clone(),
                                    dataset: dataset.clone(),
                                    identity,
                                    rows,
                                    span,
                                    received_at: Utc::now(),
                                });
                            }
                            FetchOutcome::Failed { identity, reason } => {
                                let pair = format!("{identity}@{source}");
                                // Health first, then the asking tile's
                                // answer — the order the runner's own
                                // `SeriesAppended`/`SeriesFailed` arms
                                // report in, so every path this lane has
                                // reads the same way.
                                report_load(
                                    &pair,
                                    Health::Failed {
                                        reason: reason.clone(),
                                    },
                                    format!("{pair}: {reason}"),
                                );
                                let _ = sink(DataEvent::SeriesFetched {
                                    source: source.clone(),
                                    identity,
                                    result: Err(reason),
                                });
                            }
                            FetchOutcome::Identities(Some(mut ids)) => {
                                ids.sort();
                                ids.dedup();
                                identities
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .insert(source.clone(), ids);
                            }
                            // A source that cannot enumerate is not a
                            // failure (`Fetch::catalogue`'s own doc): the
                            // picker simply has no typeahead for it.
                            FetchOutcome::Identities(None) => {}
                            FetchOutcome::IdentitiesPanicked(payload) => {
                                let _ = sink(identity_listing_panicked(&source, &payload));
                            }
                        })
                    };
                    match FetchWorker::spawn(&spec.name, fetch, outcome_sink, Arc::clone(&sink)) {
                        Ok(worker) => {
                            // Use the actual delivery verdict when reporting a clean connection.
                            let source = spec.name.clone();
                            let sink = Arc::clone(&sink);
                            health_tracker.report_discovery_and_emit(
                                &spec.name,
                                Health::Ok,
                                String::new(),
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
                            worker.request(FetchWork::Identities);
                            fetch_datasets.insert(spec.name.clone(), spec.dataset.clone());
                            fetchers.push(worker);
                        }
                        Err(e) => report_unservable(e.message),
                    }
                    continue;
                }
            }
            let Some(adapter) = config.adapters.get(&spec.adapter) else {
                report_unservable(format!("adapter '{}' is not in this build", spec.adapter));
                continue;
            };
            // `SourceSpec::from_doc` refuses a subscribed source with no
            // `document` key and one naming an undeclared or non-document
            // dataset, so neither this nor the dataset lookup below is
            // reachable from a config file. Both are still reported
            // rather than unwrapped: a `DataServiceConfig` can be built
            // in code (every test here does), and a panic inside `open`
            // over a misconfigured source would take the whole app down
            // for the one thing this seam exists to report.
            let Some(document) = spec.document.as_deref() else {
                report_unservable(format!("adapter '{}' needs a document kind", spec.adapter));
                continue;
            };
            let Some(kind) = config.documents.get(document) else {
                report_unservable(format!("document kind '{document}' is not registered"));
                continue;
            };
            let Some(dataset) = config.schema.dataset(&spec.dataset) else {
                report_unservable(format!("dataset '{}' is not declared", spec.dataset));
                continue;
            };
            // Validate kind and schema compatibility once when opening the source.
            if let Err(e) = check_kind_against(kind.as_ref(), dataset) {
                report_unservable(e);
                continue;
            }
            // Asked for per source, never cached as a property of the
            // adapter: `Adapter::subscription` may answer `None` at
            // runtime (see its doc), and one source's refusal says
            // nothing about the next.
            let Some(subscription) = adapter.subscription() else {
                report_unservable(format!(
                    "adapter '{}' has no subscription side",
                    spec.adapter
                ));
                continue;
            };
            // The ingest sink's own two arms, through the one load-lane
            // door (`load_report_sink`): a document that did not publish
            // is the same event as a file that did not load, so a failure
            // is logged by `log_ingest_failure` (the `Failed` arm) and
            // filed on the LOAD lane keyed by the batch the receiver
            // names — the document's key, or the raw topic when the bytes
            // never yielded one.
            //
            // The receiver reports the HEALTH, not just a reason, because
            // it also reports the `Ok` that clears a topic-keyed failure
            // (`ingest::subscribe`'s module doc: this lane's only other
            // `Ok` writer keys by the document's batch, which is a
            // different string from the topic, so nothing else could).
            // That recovery gets the ordinary transition line instead —
            // the same `log_health_event` the `Published` arm uses, and
            // never both, since a failure already had its own line above.
            let report_load = load_report_sink(spec, &sink, &health_tracker);
            let on_connection: HealthSink = {
                let sink = Arc::clone(&sink);
                let health_tracker = Arc::clone(&health_tracker);
                let source = spec.name.clone();
                Arc::new(move |state: ConnectionState| {
                    // `ConnectionState`'s own doc records this mapping:
                    // `Reconnecting` is `Pending` rather than a failure
                    // because nothing has been lost yet and a trader
                    // should read "waiting", not "broken"; `Lost` carries
                    // the adapter's reason through verbatim, since that
                    // string is the whole of what the diagnostics page
                    // can say about a vendor library's failure.
                    let (worst, detail) = match state {
                        ConnectionState::Connected => (Health::Ok, String::new()),
                        ConnectionState::Reconnecting => {
                            (Health::Pending, "reconnecting".to_string())
                        }
                        ConnectionState::Lost { reason } => (
                            Health::Failed {
                                reason: reason.clone(),
                            },
                            reason,
                        ),
                    };
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
                    });
                })
            };
            match SubscriptionWorker::spawn(
                spec,
                dataset.clone(),
                kind,
                subscription,
                Arc::clone(&ingest),
                report_load,
                on_connection,
                config.clock,
                Arc::clone(&sink),
            ) {
                Ok(worker) => subscriptions.push(worker),
                Err(e) => report_unservable(e.message),
            }
        }

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
                    // Forward every poll to the shared discovery lane. Emit the deciding lane's
                    // health and detail together, which may still describe a load failure.
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
        // Directory sources only: a subscribed source has no `paths` to
        // poll, and handing it to the scheduler would have it report
        // discovery health for a directory nobody configured — on the
        // very lane the receiver's own connection state is reported on.
        let scheduler = Scheduler::spawn(
            directory_sources,
            discovery_conn,
            Arc::clone(&ingest),
            scheduler_sink,
            Arc::clone(&sink),
        );

        // Validate views at open so diagnostics name the configuration before any
        // tile queries it. A view with an error is refused by name in `query`;
        // every other view serves normally.
        let (diagnostics, refused_views) =
            validate_views(&config.views, &config.schema, &config.dimensions);
        let egress =
            EgressWorkers::spawn(&config.egress, &config.adapters, Arc::clone(&stored_sink));
        let positions = PositionWorker::spawn(
            &config.positions,
            &config.adapters,
            Arc::clone(&stored_sink),
        );
        Ok(DataService {
            read_config: Arc::new(ReadConfig {
                schema: Arc::new(config.schema.clone()),
                dimensions: config.dimensions.clone(),
            }),
            config,
            sink: stored_sink,
            health: Arc::clone(&health_tracker),
            diagnostics,
            refused_views,
            drifted,
            egress,
            positions,
            subscriptions: std::sync::Mutex::new(subscriptions),
            fetchers: std::sync::Mutex::new(fetchers),
            snapshots: std::sync::Mutex::new(snapshots),
            identities,
            fetch_datasets,
            context_columns: Arc::default(),
            pool,
            pricing,
            vol,
            scheduler,
            ingest,
            conn,
        })
    }

    /// Replace the context columns every later query carries.
    pub fn set_context_columns(&self, columns: Vec<String>) {
        *self
            .context_columns
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = columns;
    }

    /// Read the context columns from `shared` from now on (the handle's
    /// copy, so `DataHandle::set_context_columns` reaches a running service).
    pub fn share_context_columns(&mut self, shared: ContextColumns) {
        self.context_columns = shared;
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

    /// Replace every view and revalidate, returning the diagnostics found.
    ///
    /// Nothing is filtered here: a view whose configuration cannot be honoured
    /// stays registered and is refused by name when queried, so its author can
    /// still see it in the dialogs and fix it. The refusal set is REPLACED
    /// rather than merged, or a view just corrected in the configuration would
    /// stay refused until restart. One broken view never stops the others.
    pub fn replace_views(
        &mut self,
        views: Vec<ViewSpec>,
        dimensions: DerivedDimensions,
    ) -> Vec<Diagnostic> {
        // Validate before assigning anything: if validation panics, the
        // previous views, dimensions and refusals stay in force together.
        let (diagnostics, refused_views) = validate_views(&views, &self.config.schema, &dimensions);
        self.read_config = Arc::new(ReadConfig {
            schema: Arc::clone(&self.read_config.schema),
            dimensions: dimensions.clone(),
        });
        self.config.dimensions = dimensions;
        self.config.views = views;
        self.diagnostics = diagnostics.clone();
        self.refused_views = refused_views;
        diagnostics
    }

    /// Refuse a read of a drifted dataset before compiling it: its rows may
    /// sit in the wrong columns, and an error naming the drift is the answer.
    fn refuse_drifted<'a>(
        &self,
        datasets: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), StoreError> {
        for name in datasets {
            if let Some(reason) = self.drifted.get(name) {
                return Err(StoreError::Drift(reason.clone()));
            }
        }
        Ok(())
    }

    /// Validate caller-owned scope before compilation so errors name its invalid
    /// columns rather than surfacing later as SQL binder failures.
    pub fn validate_scope(&self, dataset: &str, scope: &Scope) -> Vec<Diagnostic> {
        match self.config.schema.dataset(dataset) {
            Some(ds) => scope.validate(ds, &self.config.dimensions),
            None => Vec::new(),
        }
    }

    /// Submit for planning and execution on one worker snapshot. Compilation
    /// errors and results arrive on the sink `open` was given;
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

        // Refused before the grouping override is considered: a regrouping of a
        // view that cannot be honoured is not a way in.
        if let Some(why) = self.refused_views.get(view) {
            return Err(StoreError::Sql {
                statement: format!("query view '{view}'"),
                source: duckdb::Error::InvalidParameterName(why.clone()),
            });
        }
        self.refuse_drifted(
            std::iter::once(spec.dataset.as_str())
                .chain(spec.joins.iter().map(|j| j.dataset.as_str())),
        )?;

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
        // Every query carries the host's context columns; the configured
        // view never does (it is shared across queries and reloads).
        let contextual = ViewSpec {
            context: self
                .context_columns
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            ..spec.clone()
        };

        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            view: ViewId(view.to_string()),
            grouping: spec.grouping.clone(),
            work: Work::Read(Box::new(ReadQuery::view(
                Arc::clone(&self.read_config),
                contextual,
                params.scope.clone(),
                params.as_of.clone(),
                params.max_depth,
            ))),
            provenance: Provenance::default(),
            kind: RequestKind::Query,
        }))
    }

    /// The picker's distinct-values query: compile and submit
    /// under the caller's scope and era, unioned across every dataset
    /// that carries the column. The caller has already removed the
    /// column's own selection from `params.scope`.
    pub fn distinct(&self, params: &DistinctParams) -> Result<QueryId, StoreError> {
        // The picker unions every dataset carrying the column; one drifted
        // contributor refuses the whole answer rather than shorten it.
        let base = self.config.dimensions.base_column(&params.column);
        self.refuse_drifted(self.drifted.keys().map(String::as_str).filter(|name| {
            self.config
                .schema
                .dataset(name)
                .is_some_and(|ds| ds.column(base).is_some())
        }))?;
        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: Instant::now(),
            view: ViewId(format!("distinct:{}", params.column)),
            grouping: Vec::new(),
            work: Work::Read(Box::new(ReadQuery::distinct(
                Arc::clone(&self.read_config),
                params.clone(),
            ))),
            provenance: Provenance::default(),
            kind: RequestKind::Distinct {
                column: params.column.clone(),
            },
        }))
    }

    /// Write one document and queue it on its target's worker. Every outcome,
    /// including a refusal decided here, arrives as one `DataEvent::Upload`.
    pub fn upload(&self, params: UploadParams) {
        self.egress.upload(params, &self.config.documents);
    }

    /// Queue a position command on the position worker. Every outcome,
    /// including a refusal decided here, arrives as one `DataEvent::Command`.
    pub fn move_lhu(&self, params: MoveLhuParams) {
        let (tag, count, lhu) = (params.tag, params.positions.len(), params.lhu.clone());
        if let Err(reason) = self.positions.submit(params) {
            tracing::info!(target: "geode::ingest", "move to LHU {lhu} refused: {reason}");
            let _ = (self.sink)(DataEvent::Command(CommandOutcome {
                tag,
                count,
                lhu,
                result: Err(reason),
            }));
        }
    }

    /// Queue a document query through the shared pool, with the same per-key
    /// supersession and cancellation behavior as view queries.
    pub fn document(&self, params: &DocumentParams) -> Result<QueryId, StoreError> {
        self.refuse_drifted([params.dataset.as_str()])?;
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
            work: Work::Read(Box::new(ReadQuery::document(
                Arc::clone(&self.read_config),
                params.clone(),
            ))),
            provenance: Provenance::default(),
            kind: RequestKind::Query,
        }))
    }

    /// Queue pricing work without waiting. Refusal produces a terminal
    /// `PriceOutcome` for each requested line so callers do not wait forever.
    pub fn price(&self, params: PriceParams) {
        let key = params.key;
        let tag = params.tag;
        let submitted = params.submitted;
        let lines: Vec<(u64, u64)> = params.lines.iter().map(|l| (l.id, l.revision)).collect();
        if self.pricing.request(params) {
            return;
        }
        tracing::warn!(
            target: "geode::pricing",
            "the pricing queue is full; batch for key {} tag {tag} was refused",
            key.0
        );
        let results = lines
            .into_iter()
            .map(|(id, revision)| {
                (
                    id,
                    revision,
                    Err("the pricing queue is full; resubmit".to_string()),
                )
            })
            .collect();
        let _ = (self.sink)(DataEvent::Price(PriceOutcome {
            key,
            tag,
            submitted,
            results,
        }));
    }

    /// Queue vol work without waiting. Refusal produces a terminal
    /// `VolSliceOutcome` with an error per job so callers do not wait forever.
    pub fn vol_slices(&self, params: VolSliceParams) {
        let key = params.key;
        let tag = params.tag;
        let submitted = params.submitted;
        let jobs = params.jobs.len();
        if self.vol.request(params) {
            return;
        }
        tracing::warn!(
            target: "geode::vol",
            "the vol queue is full; batch for key {} tag {tag} was refused",
            key.0
        );
        let _ = (self.sink)(DataEvent::VolSlices(VolSliceOutcome {
            key,
            tag,
            submitted,
            results: (0..jobs)
                .map(|_| Err("the vol queue is full; resubmit".to_string()))
                .collect(),
        }));
    }

    /// Publish an app-authored document. The dataset
    /// must be declared `local = true`: anything else is refused with an
    /// error diagnostic and nothing is written. Accepted, the rows go
    /// through the ingest runner's document lane exactly as a
    /// subscribed document does — same validation, same `contained`
    /// boundary, same `Published` event — stamped `LOCAL_SOURCE`.
    ///
    /// A refusal also answers `LocalPublishFailed`: every admitted publish
    /// answers its writer exactly as one the runner failed would, so a
    /// writer waiting on the outcome is never left waiting.
    pub fn publish(&self, publish: LocalPublish) {
        let local = self
            .config
            .schema
            .dataset(&publish.dataset)
            .is_some_and(|d| d.local);
        if !local {
            tracing::warn!(
                target: "geode::ingest",
                "refused a local publish to '{}': not a local dataset",
                publish.dataset
            );
            let _ = (self.sink)(DataEvent::Diagnostics(vec![Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!(
                    "refused a local publish to '{}': not a local dataset (declare `local = true` on a document dataset)",
                    publish.dataset
                ),
                path: None,
            }]));
            let _ = (self.sink)(DataEvent::LocalPublishFailed {
                batch: geode_core::document::join_key(&publish.rows.key),
                dataset: publish.dataset,
                reason: "not a local dataset".to_string(),
            });
            return;
        }
        let now = chrono::Utc::now();
        self.ingest.submit_document(DocumentJob {
            source: LOCAL_SOURCE.to_string(),
            dataset: publish.dataset,
            rows: publish.rows,
            source_time: now,
            received_at: now,
            bytes: 0,
        });
    }

    /// Forget one document of a local dataset. Anything else — a dataset not
    /// declared `local = true`, or a key whose arity is not the dataset's —
    /// is refused with an error diagnostic and a `ForgetFailed`, and nothing
    /// is queued. Accepted, the forget joins the ingest runner's documents
    /// FIFO behind every publish already queued, and answers `Forgotten` or
    /// `ForgetFailed`. Either way every admitted forget answers its asker.
    pub fn forget(&self, forget: LocalForget) {
        let refusal = match self.config.schema.dataset(&forget.dataset) {
            Some(ds) if !ds.local => Some("not a local dataset".to_string()),
            None => Some("not a local dataset".to_string()),
            Some(ds) if forget.key.len() != ds.key.len() => Some(format!(
                "key has {} part(s), the dataset's has {}",
                forget.key.len(),
                ds.key.len()
            )),
            Some(_) => None,
        };
        if let Some(why) = refusal {
            tracing::warn!(
                target: "geode::ingest",
                "refused a forget in '{}': {why}",
                forget.dataset
            );
            let _ = (self.sink)(DataEvent::Diagnostics(vec![Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                message: format!("refused a forget in '{}': {why}", forget.dataset),
                path: None,
            }]));
            let _ = (self.sink)(DataEvent::ForgetFailed {
                batch: geode_core::document::join_key(&forget.key),
                dataset: forget.dataset,
                reason: why,
            });
            return;
        }
        self.ingest.submit_forget(ForgetJob {
            batch: geode_core::document::join_key(&forget.key),
            dataset: forget.dataset,
        });
    }

    /// Cap the series request before compilation, then submit it through the
    /// shared pool for per-key supersession and cancellation. A cap refusal names
    /// the frequency and span; results arrive as DataEvent::Series.
    pub fn series(&self, params: &SeriesParams) -> Result<QueryId, StoreError> {
        self.refuse_drifted([params.dataset.as_str()])?;
        let points = params.frequency.buckets_in(params.range.0, params.range.1);
        if points > SERIES_POINT_CAP {
            return Err(StoreError::Series(cap_message(
                params.frequency,
                params.range.0,
                params.range.1,
                points,
            )));
        }
        let plan = compile_series(&self.config.schema, params)?;
        // Only the SOURCE slots: an expression slot has no pair and so
        // no load lane of its own. Carried on the request so the result
        // sink attaches each one's health without re-reading the plan.
        let pairs = params
            .series
            .iter()
            .filter_map(|s| match &s.kind {
                SlotKind::Source {
                    source, identity, ..
                } => Some((s.slot, source.clone(), identity.clone())),
                SlotKind::Expr(_) => None,
            })
            .collect();
        Ok(self.pool.submit(QueryRequest {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            view: ViewId(format!("series:{}", params.dataset)),
            work: Work::Series(Box::new(plan)),
            // Retain provenance for each source slot; series results have no tree.
            grouping: Vec::new(),
            provenance: Provenance::default(),
            kind: RequestKind::Series { pairs },
        }))
    }

    pub fn cancel(&self, key: QueryKey) {
        self.pool.cancel(key);
        self.pricing.cancel(key);
        self.vol.cancel(key);
    }

    /// Answer a fetch the request loop could not run the way the fetch worker
    /// answers its own panic: the pair's load lane goes `Failed`, then
    /// `SeriesFetched` carries the error, so the asking tile and every other
    /// tile watching the pair hear back.
    pub(crate) fn fail_fetch(&self, source: &str, identity: &str, reason: String) {
        let pair = format!("{identity}@{source}");
        self.health.report_load_and_emit(
            source,
            &pair,
            Health::Failed {
                reason: reason.clone(),
            },
            format!("{pair}: {reason}"),
            |reported| match reported {
                Some((worst, detail)) => {
                    log_health_event(source, &worst, &detail);
                    (self.sink)(DataEvent::Health {
                        source: source.to_string(),
                        worst,
                        detail,
                    })
                }
                None => true,
            },
        );
        let _ = (self.sink)(DataEvent::SeriesFetched {
            source: source.to_string(),
            identity: identity.to_string(),
            result: Err(reason),
        });
    }

    /// The on-demand fetch: subtract what the
    /// coverage table already holds and queue one job per gap on the
    /// source's fetch worker. Every early exit is a `SeriesFetched`, so
    /// the asking tile always hears back.
    ///
    /// Coverage is read through `self.conn`, the service's own reader:
    /// `append_series` commits its rows and its coverage row in one
    /// transaction on the ingest thread, so a span this sees as covered
    /// is a span whose rows are queryable.
    pub fn fetch(&self, params: &FetchParams) {
        let answer = |result: Result<u64, String>| {
            let _ = (self.sink)(DataEvent::SeriesFetched {
                source: params.source.clone(),
                identity: params.identity.clone(),
                result,
            });
        };
        if let Some(reason) = self
            .config
            .sources
            .iter()
            .find(|s| s.name == params.source)
            .and_then(|s| self.drifted.get(&s.dataset))
        {
            answer(Err(reason.clone()));
            return;
        }
        let Some(dataset) = self.fetch_datasets.get(&params.source) else {
            answer(Err(format!(
                "source '{}' is not a fetch source",
                params.source
            )));
            return;
        };
        // Clip to the dataset's history window before subtracting coverage. Otherwise
        // append retention would immediately remove old rows and coverage, causing
        // the same expired span to be fetched repeatedly. Wholly expired requests
        // complete without adapter I/O. If conversion or subtraction overflows,
        // skip clipping rather than panic; append still applies its retention policy.
        let mut from = params.from;
        if let Some(cutoff) = self
            .config
            .schema
            .dataset(dataset)
            .and_then(|ds| ds.series_retention)
            .and_then(|r| r.history)
            .and_then(|window| chrono::Duration::from_std(window).ok())
            .and_then(|window| Utc::now().checked_sub_signed(window))
        {
            from = from.max(cutoff);
        }
        if from >= params.to {
            answer(Ok(0));
            return;
        }
        let loaded = match crate::store::series::coverage(
            &self.conn,
            dataset,
            &params.source,
            &params.identity,
        ) {
            Ok(spans) => spans,
            Err(e) => {
                answer(Err(format!("reading coverage: {e}")));
                return;
            }
        };
        let gaps = crate::store::series::missing_spans((from, params.to), &loaded);
        if gaps.is_empty() {
            answer(Ok(0));
            return;
        }
        let fetchers = self.fetchers.lock().unwrap_or_else(|e| e.into_inner());
        let Some(worker) = fetchers.iter().find(|w| w.source() == params.source) else {
            answer(Err(format!(
                "source '{}' is not a fetch source",
                params.source
            )));
            return;
        };
        for (from, to) in gaps {
            if !worker.request(FetchWork::Span {
                identity: params.identity.clone(),
                from,
                to,
            }) {
                answer(Err(format!(
                    "the fetch queue for '{}' is full",
                    params.source
                )));
                return;
            }
        }
    }

    /// Refresh a fetch source's identities. Return false for an unknown/non-fetch
    /// source or refused queue submission. Results update the next catalog snapshot.
    pub fn identities(&self, source: &str) -> bool {
        let fetchers = self.fetchers.lock().unwrap_or_else(|e| e.into_inner());
        fetchers
            .iter()
            .find(|w| w.source() == source)
            .is_some_and(|w| w.request(FetchWork::Identities))
    }

    /// Poll every snapshot source filling `dataset` now. Keyed by dataset
    /// because that is what the asking page shows; never blocks, and a
    /// poll already running absorbs the ask into one follow-up poll.
    pub fn poll(&self, dataset: &str) {
        let snapshots = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        let mut any = false;
        for worker in snapshots.iter().filter(|w| w.dataset() == dataset) {
            worker.poll_now();
            any = true;
        }
        if !any {
            tracing::warn!(target: "geode::ingest", "poll: no snapshot source fills '{dataset}'");
        }
    }

    /// Read one reference table synchronously on the service's reader
    /// connection: reference tables are small, and the answer is whole.
    pub fn reference(&self, params: &ReferenceParams) -> ReferenceOutcome {
        let table = match self.config.schema.dataset(&params.dataset) {
            Some(ds) if ds.family == geode_core::schema::Family::Reference => self
                .refuse_drifted([ds.name.as_str()])
                .and_then(|()| {
                    crate::store::reference::read_reference(&self.conn, ds, &params.as_of)
                })
                .map_err(|e| e.to_string()),
            _ => Err(format!(
                "dataset '{}' is not a declared reference dataset",
                params.dataset
            )),
        };
        if let Err(e) = &table {
            tracing::warn!(target: "geode::query", "reference request failed: {e}");
        }
        ReferenceOutcome {
            key: params.key,
            tag: params.tag,
            dataset: params.dataset.clone(),
            as_of: params.as_of.clone(),
            table,
        }
    }

    /// Read catalog metadata synchronously on the service's reader connection.
    /// These queries inspect metadata rather than scanning payload tables.
    pub fn catalog(&self, params: &CatalogParams) -> CatalogOutcome {
        let mut snapshot = build_catalog(&self.conn, &self.config.schema, &params.as_of);
        // Merge identities cached by fetch sources; these are not stored in DuckDB.
        if let Ok(snap) = &mut snapshot {
            snap.identities = self
                .identities
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|(s, ids)| (s.clone(), ids.clone()))
                .collect();
        }
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

    /// Per-book freshness in the requested era. Historical results must use the
    /// selected generations rather than today's live freshness.
    pub fn freshness(&self, dataset: &str, as_of: AsOf) -> Result<BookFreshness, StoreError> {
        let at = match as_of {
            AsOf::Live => return Catalog::new(&self.conn).book_freshness(dataset),
            AsOf::At(t) => t,
        };
        if self.config.schema.dataset(dataset).is_none() {
            return Ok(Vec::new());
        };

        // The oldest generation contributing to each book, which is the
        // same stalest-input rule live freshness applies — a book
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

    /// Oldest source time across the dataset's live and archive table pairs, or
    /// None when they contain no rows. This includes document history and a
    /// partition's first generation still in live; it does not guarantee complete
    /// history across every partition since that instant.
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
        // Upload workers first: they answer only the sink, and an upload
        // echoing onto a bus should not arrive after its subscriptions stop.
        self.egress.shutdown();
        // The position worker for the same reason: it answers only the sink.
        self.positions.shutdown();
        // Fetch workers before the subscriptions, for the same reason
        // the subscriptions come before the runner: a worker's outcome
        // sink submits series jobs into the ingest runner, so it must
        // stop delivering before anything downstream of it does.
        for worker in self
            .fetchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter_mut()
        {
            worker.shutdown();
        }
        // Snapshot workers for the same reason: each submits its polls to
        // the runner. Each waits for a poll in flight to finish.
        for worker in self
            .snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter_mut()
        {
            worker.shutdown();
        }
        // Then the subscriptions, for the same reason as the fetch
        // workers above: each one's receiver thread submits documents
        // into the ingest runner, so stopping the runner while a worker
        // is still delivering would leave work queued behind a shut-down
        // consumer.
        for worker in self
            .subscriptions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter_mut()
        {
            worker.shutdown();
        }
        self.pool.shutdown();
        self.pricing.shutdown();
        self.vol.shutdown();
        self.scheduler.shutdown();
        self.ingest.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ddl::tests_support::{
        FakeKind, cvi_dataset, cvi_doc, local_dataset, sheet_rows, ts,
    };
    use geode_core::scope::{DimensionSelection, Scope};
    use std::time::Duration;

    fn service() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        service_with(|_| {})
    }

    /// `service()` with `prepare` run on the loaded store before the service
    /// opens it, so a test can change the database open then finds.
    fn service_with(
        prepare: impl FnOnce(&Store),
    ) -> (
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
        prepare(&store);
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
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();
        (db, src, service, rx)
    }

    const COMPUTED_PRICER: &str = "[pricer]\ncomputed = true\n[pricer.columns.instrument_ref]\ntype = \"utf8\"\nrole = \"key\"\n[pricer.columns.npv]\ntype = \"f64\"\nrole = \"measure\"\ngrain = \"underlying\"\n";

    /// `service()` with the computed `pricer` dataset declared beside the
    /// stored `risk_snapshot` fixture.
    fn service_with_computed() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        let (db, src, store, ds, _emitted) = crate::ingest::load::tests_support::fixture();
        drop(store);

        let doc = geode_core::config::merge_docs(
            "datasets",
            &[geode_core::config::LayerDoc::builtin("datasets", COMPUTED_PRICER).unwrap()],
        );
        let (computed, diags) = SchemaSpec::from_doc(&doc);
        assert!(
            diags.iter().all(|d| d.severity != Severity::Error),
            "the computed fixture must parse cleanly: {diags:?}"
        );
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        schema.datasets.extend(computed.datasets);
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![crate::ingest::load::tests_support::tree_view()],
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();
        (db, src, service, rx)
    }

    #[test]
    fn a_computed_dataset_creates_no_table_and_is_absent_from_the_catalog() {
        let (_db, _src, svc, _rx) = service_with_computed();
        let tables: Vec<String> = svc
            .conn
            .prepare(
                "select table_name from information_schema.tables where table_name like 'pricer%'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            tables.is_empty(),
            "a computed dataset owns no table: {tables:?}"
        );
        let catalog = build_catalog(&svc.conn, &svc.config.schema, &AsOf::Live).unwrap();
        assert!(catalog.datasets.iter().all(|d| d.name != "pricer"));
        svc.shutdown();
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
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::catalog::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        crate::store::document::publish_document(
            &store,
            &crate::store::document::DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("SPX.Z", [1., 2., 3., 4., 5., 6.]),
                source_time: ts("2026-09-12T14:00:00Z"),
                received_at: ts("2026-09-12T14:00:00Z"),
                bytes: 0,
            },
        )
        .unwrap();
        // Publish another key between SPX.Z's two generations. The distinct live
        // times detect a dataset-wide minimum incorrectly used for both documents.
        crate::store::document::publish_document(
            &store,
            &crate::store::document::DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("NDX.Z", [100., 200., 300., 400., 500., 600.]),
                source_time: ts("2026-09-12T14:03:00Z"),
                received_at: ts("2026-09-12T14:03:00Z"),
                bytes: 0,
            },
        )
        .unwrap();
        crate::store::document::publish_document(
            &store,
            &crate::store::document::DocumentPublishRequest {
                dataset: &ds,
                source: "cvi",
                rows: &cvi_doc("SPX.Z", [10., 20., 30., 40., 50., 60.]),
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
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();
        (dir, service, rx)
    }

    /// A service over the two-part-key `option_chain` fixture. SPX has two
    /// expiries: 2026-10-16 published at 14:00 and republished at 14:10,
    /// 2026-11-20 published once at 14:05. SPXW (a string extension of
    /// SPX) publishes at 14:02 and again at 14:12, so it holds both the
    /// newest source time and the greatest generation in the dataset.
    fn chain_service() -> (
        tempfile::TempDir,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        use crate::store::ddl::tests_support::{chain_dataset, chain_doc};
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = chain_dataset();
        store.apply_schema(&ds).unwrap();
        crate::store::catalog::Catalog::new(store.writer())
            .ensure_tables()
            .unwrap();
        for (u, e, mids, at) in [
            ("SPX", "2026-10-16", [0.20, 0.18], "2026-09-12T14:00:00Z"),
            ("SPXW", "2026-10-16", [0.50, 0.50], "2026-09-12T14:02:00Z"),
            ("SPX", "2026-11-20", [0.22, 0.19], "2026-09-12T14:05:00Z"),
            ("SPX", "2026-10-16", [0.21, 0.17], "2026-09-12T14:10:00Z"),
            ("SPXW", "2026-10-16", [0.51, 0.51], "2026-09-12T14:12:00Z"),
        ] {
            crate::store::document::publish_document(
                &store,
                &crate::store::document::DocumentPublishRequest {
                    dataset: &ds,
                    source: "opra_sim",
                    rows: &chain_doc(u, e, mids),
                    source_time: ts(at),
                    received_at: ts(at),
                    bytes: 0,
                },
            )
            .unwrap();
        }
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
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();
        (dir, service, rx)
    }

    /// A service over a `local = true` document dataset (`sheets`) plus
    /// the CVI fixture dataset (not `local`), with a `FakePricer` behind
    /// the pricing worker and a `FakeVolModel` behind the vol worker —
    /// the fixture the publish, pricing and vol tests share. `delay` is
    /// each fake's own per-job delay: `Duration::ZERO` for most tests,
    /// non-zero where a test needs a batch to still be running when it
    /// submits the next one (the cancel and full-queue tests below).
    fn local_service_with_delay(
        delay: Duration,
    ) -> (
        tempfile::TempDir,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(local_dataset());
        schema.datasets.push(cvi_dataset());
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::with(Arc::new(crate::pricing::worker::tests::FakePricer {
                asked: Default::default(),
                delay,
                overrides_seen: Default::default(),
            })),
            vol: crate::vol::VolConfig::with(Arc::new(crate::vol::worker::tests::FakeVolModel {
                asked: Default::default(),
                delay,
            })),
            positions: None,
        })
        .unwrap();
        (dir, service, rx)
    }

    fn local_service() -> (
        tempfile::TempDir,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        local_service_with_delay(Duration::ZERO)
    }

    /// Drains `rx` until `pick` answers `Some`, discarding everything
    /// else — the shape every test below wants: "the event I asked for,
    /// whenever it lands among the others".
    fn until<T>(
        rx: &std::sync::mpsc::Receiver<DataEvent>,
        mut pick: impl FnMut(DataEvent) -> Option<T>,
    ) -> T {
        loop {
            let e = rx.recv_timeout(Duration::from_secs(30)).expect("an event");
            if let Some(t) = pick(e) {
                return t;
            }
        }
    }

    /// [`until`] with an overall deadline: `None` when no picked event lands
    /// within `limit`. A source that polls every interval keeps the channel
    /// busy, so `until`'s per-event timeout never fires on a missing event;
    /// a test whose broken path still emits must fail on its assertion here.
    fn until_within<T>(
        rx: &std::sync::mpsc::Receiver<DataEvent>,
        limit: Duration,
        mut pick: impl FnMut(DataEvent) -> Option<T>,
    ) -> Option<T> {
        let deadline = Instant::now() + limit;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let e = rx.recv_timeout(remaining).ok()?;
            if let Some(t) = pick(e) {
                return Some(t);
            }
        }
    }

    #[test]
    fn a_local_publish_lands_a_generation_the_document_request_reads_back() {
        let (_d, service, rx) = local_service();
        service.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: sheet_rows("untitled-1", &[1, -2, 3]),
        });
        let (dataset, batch) = until(&rx, |e| match e {
            DataEvent::Published { dataset, batch, .. } => Some((dataset, batch)),
            _ => None,
        });
        assert_eq!((dataset.as_str(), batch.as_str()), ("sheets", "untitled-1"));
        service
            .document(&DocumentParams {
                key: QueryKey(5),
                tag: 1,
                submitted: Instant::now(),
                dataset: "sheets".into(),
                document_key: vec!["untitled-1".into()],
                as_of: AsOf::Live,
            })
            .unwrap();
        let snapshot = until(&rx, |e| match e {
            DataEvent::Query(o) if o.key == QueryKey(5) => Some(o.snapshot.unwrap()),
            _ => None,
        });
        assert_eq!(snapshot.rows(), 3);
    }

    #[test]
    fn a_local_publish_emits_no_health_event_and_a_load_ended() {
        let (_d, service, rx) = local_service();
        service.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: sheet_rows("s", &[1]),
        });
        let mut saw_published = false;
        let mut saw_ended = false;
        // `local_service` opens with no `[sources]`, so the ingest
        // runner's own startup drain announces an idle `PlanComplete` —
        // a `LoadEnded` with nothing behind it — before this publish's
        // job is even queued; a bare "break on the first LoadEnded"
        // races that spurious one. The `LoadEnded` this test asserts on
        // is specifically the one that follows OUR `Published`.
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
            match e {
                DataEvent::Health { source, .. } => {
                    panic!("no health lane for a local publish, got {source}")
                }
                // Local autosave must not start ingest progress. Observe the raw Loading
                // events so an unwanted start cannot be hidden by an outcome helper.
                DataEvent::Loading { ref source, .. } if source == "local" => {
                    panic!("a local publish must not emit Loading, got {e:?}")
                }
                DataEvent::Published { .. } => saw_published = true,
                DataEvent::LoadEnded if saw_published => {
                    saw_ended = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_published && saw_ended);
    }

    #[test]
    fn a_failed_local_publish_is_a_diagnostics_error_and_a_load_ended_with_no_health() {
        let (_d, service, rx) = local_service();
        // A zero-row document (`DocumentRows::validate`'s row floor):
        // refused by the runner's own publish, never reaching the
        // store, and — like any local publish — reported with no health
        // lane at all (there is no `[sources]` entry to key one under).
        service.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: sheet_rows("s", &[]),
        });
        let mut saw_diagnostic = false;
        let mut saw_ended = false;
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
            match e {
                DataEvent::Health { source, .. } => {
                    panic!("no health lane for a local publish, got {source}")
                }
                DataEvent::Diagnostics(diags) => {
                    assert!(
                        diags.iter().any(|d| d.severity == Severity::Error
                            && d.message.contains("local publish of sheets/s failed")),
                        "{diags:?}"
                    );
                    saw_diagnostic = true;
                }
                DataEvent::LoadEnded if saw_diagnostic => {
                    saw_ended = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_diagnostic && saw_ended);
    }

    /// A local publish answers its own outcome by dataset and batch —
    /// `LocalPublished` with the generation — beside the `Published`
    /// invalidation every publish sends, so the tile that saved can clear
    /// its dirty mark without diagnostics losing its publication record.
    #[test]
    fn a_local_publish_reports_local_published_beside_published() {
        let (_d, service, rx) = local_service();
        service.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: sheet_rows("s", &[1]),
        });
        let mut published = None;
        let mut local = None;
        while published.is_none() || local.is_none() {
            match rx.recv_timeout(Duration::from_secs(30)).expect("an event") {
                DataEvent::Published {
                    dataset,
                    batch,
                    gen_id,
                    ..
                } => published = Some((dataset, batch, gen_id)),
                DataEvent::LocalPublished {
                    dataset,
                    batch,
                    gen_id,
                } => local = Some((dataset, batch, gen_id)),
                DataEvent::LocalPublishFailed { reason, .. } => panic!("{reason}"),
                _ => {}
            }
        }
        let local = local.unwrap();
        assert_eq!((local.0.as_str(), local.1.as_str()), ("sheets", "s"));
        assert_eq!(Some(local), published);
    }

    #[test]
    fn a_failed_local_publish_reports_local_publish_failed_beside_the_diagnostic() {
        let (_d, service, rx) = local_service();
        service.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: sheet_rows("s", &[]),
        });
        let mut diagnostic = false;
        let mut failed = None;
        while !diagnostic || failed.is_none() {
            match rx.recv_timeout(Duration::from_secs(30)).expect("an event") {
                DataEvent::Diagnostics(d) => {
                    diagnostic |= d.iter().any(|d| {
                        d.severity == Severity::Error
                            && d.message.contains("local publish of sheets/s failed")
                    })
                }
                DataEvent::LocalPublishFailed {
                    dataset,
                    batch,
                    reason,
                } => failed = Some((dataset, batch, reason)),
                DataEvent::LocalPublished { .. } => panic!("an empty document was stored"),
                _ => {}
            }
        }
        let (dataset, batch, reason) = failed.unwrap();
        assert_eq!((dataset.as_str(), batch.as_str()), ("sheets", "s"));
        assert!(reason.contains("document has no rows"), "{reason}");
    }

    /// A publish refused at the service still answers its writer: the
    /// writer may be waiting on this outcome (a sheet load deferred behind
    /// its save has no timeout), and a refusal that sent only a diagnostic
    /// would leave it waiting forever. The batch is the rows' joined key,
    /// as a publish that reached the runner would name it.
    #[test]
    fn a_publish_refused_at_the_service_answers_local_publish_failed() {
        let (_d, service, rx) = local_service();
        for dataset in ["cvi_params", "undeclared"] {
            service.publish(LocalPublish {
                dataset: dataset.into(),
                rows: cvi_doc("SPX.Z", [1., 2., 3., 4., 5., 6.]),
            });
            let (failed, batch, reason) = until(&rx, |e| match e {
                DataEvent::LocalPublishFailed {
                    dataset,
                    batch,
                    reason,
                } => Some((dataset, batch, reason)),
                DataEvent::LocalPublished { dataset, .. } => panic!("stored: {dataset}"),
                _ => None,
            });
            assert_eq!((failed.as_str(), batch.as_str()), (dataset, "SPX.Z"));
            assert!(reason.contains("not a local dataset"), "{reason}");
        }
    }

    #[test]
    fn a_publish_to_a_dataset_that_is_not_local_is_refused_unwritten() {
        let (_d, service, rx) = local_service();
        service.publish(LocalPublish {
            dataset: "cvi_params".into(),
            rows: cvi_doc("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        });
        let diags = until(&rx, |e| match e {
            DataEvent::Diagnostics(d) => Some(d),
            DataEvent::Published { dataset, .. } if dataset == "cvi_params" => {
                panic!("written: {dataset}")
            }
            _ => None,
        });
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("not a local dataset")),
            "{diags:?}"
        );
        // Prove nothing landed rather than merely asserting the refusal
        // fired: the runner's document queue is FIFO, so a wrongly
        // submitted `cvi_params` job — if one had slipped through —
        // would publish before this second, good `sheets` document does.
        service.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: sheet_rows("proof", &[1]),
        });
        let dataset = until(&rx, |e| match e {
            DataEvent::Published { dataset, .. } => Some(dataset),
            _ => None,
        });
        assert_eq!(dataset, "sheets");
        let catalog = service.catalog(&CatalogParams {
            key: QueryKey(1),
            tag: 1,
            as_of: AsOf::Live,
        });
        assert!(
            catalog
                .snapshot
                .unwrap()
                .datasets
                .iter()
                .all(|d| d.name != "cvi_params" || d.partitions.is_empty()),
            "nothing landed for cvi_params"
        );
    }

    #[test]
    fn a_price_request_reaches_the_sink_as_a_price_event_and_cancel_reaches_the_worker() {
        // A ~150 ms delay so key 11's batch is still running when key
        // 12's is submitted and cancelled — cancelling AFTER key 11's
        // outcome had already been delivered would make the cancel a
        // guaranteed no-op that a deleted `PricingWorker::cancel` call
        // could not fail.
        let (_d, service, rx) = local_service_with_delay(Duration::from_millis(150));
        service.price(crate::pricing::worker::tests::params(
            11,
            4,
            &["SPX", "FAIL"],
        ));
        service.price(crate::pricing::worker::tests::params(12, 1, &["NDX"]));
        service.cancel(QueryKey(12));
        let o = until(&rx, |e| match e {
            DataEvent::Price(o) if o.key == QueryKey(11) => Some(o),
            _ => None,
        });
        assert_eq!((o.key, o.tag), (QueryKey(11), 4));
        assert!(o.results[0].2.is_ok());
        assert!(o.results[1].2.is_err());
        // No outcome for key 12 within a generous window after key 11's
        // own arrived — cancel must have stopped it before it ran.
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(remaining.min(Duration::from_millis(50))) {
                Ok(DataEvent::Price(o)) if o.key == QueryKey(12) => {
                    panic!("key 12 was cancelled and must not have priced")
                }
                _ => {}
            }
        }
    }

    #[test]
    fn a_full_pricing_queue_answers_the_refused_batch_with_an_error_per_line() {
        // The first key is given a moment to actually start running —
        // freeing its slot from the WAITING queue — before the rest are
        // submitted back to back. A margin well beyond `PRICE_BOUND`
        // (not just one over it) is what makes this robust rather than
        // a race against the worker's own draining: submitting one
        // instant faster than the worker can pop-and-start the next
        // (measured: the worker can drain a handful of keys during a
        // tight submission loop even at a few milliseconds per line) is
        // not reliable, but submitting dozens more than fit is.
        let (_d, service, rx) = local_service_with_delay(Duration::from_millis(20));
        let bound = crate::pricing::PRICE_BOUND as u64;
        let first = 100u64;
        service.price(crate::pricing::worker::tests::params(first, 1, &["SPX"]));
        std::thread::sleep(Duration::from_millis(50));
        let rest: Vec<u64> = (first + 1..=first + bound + 20).collect();
        for &k in &rest {
            service.price(crate::pricing::worker::tests::params(k, 1, &["SPX"]));
        }
        let mut keys = vec![first];
        keys.extend(&rest);
        let mut seen: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        let mut saw_queue_full_error = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while seen.len() < keys.len() && std::time::Instant::now() < deadline {
            if let Ok(DataEvent::Price(o)) = rx.recv_timeout(Duration::from_secs(5)) {
                seen.insert(o.key.0);
                if o.results.iter().any(|(_, _, r)| {
                    r.as_ref()
                        .is_err_and(|e| e == "the pricing queue is full; resubmit")
                }) {
                    saw_queue_full_error = true;
                }
            }
        }
        assert_eq!(
            seen,
            keys.iter().copied().collect(),
            "every submitted key gets exactly one outcome"
        );
        assert!(saw_queue_full_error, "at least one batch was refused");
    }

    #[test]
    fn a_vol_batch_is_answered_by_the_worker_under_its_key_and_tag() {
        let (_d, service, rx) = local_service();
        service.vol_slices(crate::vol::worker::tests::params(
            11,
            1,
            &["2026-10-16", "1999-01-01"],
        ));
        let outcome = loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("an event") {
                DataEvent::VolSlices(o) => break o,
                _ => continue,
            }
        };
        assert_eq!((outcome.key, outcome.tag), (QueryKey(11), 1));
        assert!(outcome.results[0].is_ok());
        assert_eq!(outcome.results[1].as_ref().unwrap_err(), "refused");
    }

    #[test]
    fn a_vol_request_reaches_the_sink_and_cancel_reaches_the_vol_worker() {
        // The vol twin of the pricing test above. A 40 ms per-job delay
        // keeps key 21's five-job batch running when both keys are
        // cancelled, so a cancel that never reached the vol worker would
        // deliver all five results and then run key 22.
        let (_d, service, rx) = local_service_with_delay(Duration::from_millis(40));
        let expiries = [
            "2026-01-01",
            "2026-02-01",
            "2026-03-01",
            "2026-04-01",
            "2026-05-01",
        ];
        service.vol_slices(crate::vol::worker::tests::params(21, 3, &expiries));
        service.vol_slices(crate::vol::worker::tests::params(22, 1, &["2026-06-01"]));
        std::thread::sleep(Duration::from_millis(60));
        service.cancel(QueryKey(22));
        service.cancel(QueryKey(21));
        let o = until(&rx, |e| match e {
            DataEvent::VolSlices(o) if o.key == QueryKey(21) => Some(o),
            _ => None,
        });
        assert_eq!(o.tag, 3);
        assert!(
            o.results.len() < expiries.len(),
            "cancel stopped the running batch early: {}",
            o.results.len()
        );
        // Key 22 was queued when cancelled and never answers.
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(remaining.min(Duration::from_millis(50))) {
                Ok(DataEvent::VolSlices(o)) if o.key == QueryKey(22) => {
                    panic!("key 22 was cancelled and must not have run")
                }
                _ => {}
            }
        }
    }

    #[test]
    fn a_full_vol_queue_answers_the_refused_batch_with_an_error_per_job() {
        // A slow model holds the worker; fill the queue; the next batch is
        // refused from the service thread with one error per job.
        let (_d, service, rx) = local_service_with_delay(Duration::from_millis(300));
        let first = 1_000u64;
        service.vol_slices(crate::vol::worker::tests::params(first, 1, &["2026-01-01"]));
        std::thread::sleep(Duration::from_millis(20));
        let bound = crate::vol::VOL_BOUND as u64;
        for k in (first + 1)..=(first + bound) {
            service.vol_slices(crate::vol::worker::tests::params(k, 1, &["2026-01-01"]));
        }
        service.vol_slices(crate::vol::worker::tests::params(
            first + bound + 1,
            7,
            &["2026-01-01", "2026-02-01"],
        ));
        let refused = loop {
            match rx.recv_timeout(Duration::from_secs(10)).expect("an event") {
                DataEvent::VolSlices(o) if o.key == QueryKey(first + bound + 1) => break o,
                _ => continue,
            }
        };
        assert_eq!(refused.tag, 7);
        assert_eq!(refused.results.len(), 2);
        for r in &refused.results {
            assert_eq!(r.as_ref().unwrap_err(), "the vol queue is full; resubmit");
        }
    }

    /// A service with one SUBSCRIBED source on an
    /// in-process `ChannelAdapter` — the same code path a broker source
    /// takes, with only the wire faked (`ChannelAdapter`'s own doc).
    ///
    /// `adapter` is what the SOURCE declares, so a test can name one the
    /// registry does not hold; the bus itself is always registered as
    /// `demo_bus`. `kind` is registered under its own name, which the
    /// source's `document` key names. No pre-publish and no scratch
    /// `Store`: `open` applies the schema itself, and everything these
    /// tests assert on arrives through the feed.
    fn subscribed_service(
        kind: Arc<dyn geode_core::document::DocumentKind>,
        adapter: &str,
    ) -> (
        tempfile::TempDir,
        crate::adapter::ChannelFeed,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        subscribed_service_for(kind, adapter, "fake_cvi")
    }

    /// The same, with the source's `document` key spelled out — so a test
    /// can name a kind the registry does not hold. The registry itself
    /// always holds `kind` under ITS own name, which is what makes the
    /// two strings able to disagree.
    fn subscribed_service_for(
        kind: Arc<dyn geode_core::document::DocumentKind>,
        adapter: &str,
        document: &str,
    ) -> (
        tempfile::TempDir,
        crate::adapter::ChannelFeed,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        let (dir, feed, config) = subscribed_config(kind, adapter, document);
        let (service, rx) = DataService::open_channel(config).unwrap();
        (dir, feed, service, rx)
    }

    /// [`subscribed_service_for`]'s configuration, for a test that opens
    /// the service on its own sink.
    fn subscribed_config(
        kind: Arc<dyn geode_core::document::DocumentKind>,
        adapter: &str,
        document: &str,
    ) -> (
        tempfile::TempDir,
        crate::adapter::ChannelFeed,
        DataServiceConfig,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (bus, feed) = crate::adapter::ChannelAdapter::new("demo_bus");
        let mut adapters = AdapterRegistry::default();
        adapters.register(bus);
        let mut documents = DocumentRegistry::default();
        documents.register(kind);
        let ds = cvi_dataset();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let spec = crate::source::SourceSpec {
            adapter: adapter.to_string(),
            document: Some(document.to_string()),
            topics: vec!["cvi/>".into()],
            // Every message publishes: the coalescing window itself is
            // `ingest::subscribe`'s to test, and a window here would only
            // make these assertions wait.
            coalesce: Duration::ZERO,
            ..crate::source::SourceSpec::directory("cvi", "cvi_params", Vec::new())
        };
        let config = DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: vec![spec],
            adapters,
            documents,
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            positions: None,
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
        };
        (dir, feed, config)
    }

    /// The writer is held on the first document's `Loading` (the sink runs
    /// on the runner thread, outside the queue lock), so the burst queues
    /// behind it whatever the machine's speed: the depth is crossed by
    /// count, not by submissions outrunning publishes.
    #[test]
    fn a_burst_past_the_backlog_depth_degrades_the_source_until_it_drains() {
        let (_dir, _feed, config) = subscribed_config(
            Arc::new(crate::store::ddl::tests_support::FakeKind::new()),
            "demo_bus",
            "fake_cvi",
        );
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let held_tx = Mutex::new(held_tx);
        let (tx, rx) = std::sync::mpsc::channel();
        let sink_gate = Arc::clone(&gate);
        let sink: EventSink = Arc::new(move |e| {
            if matches!(e, DataEvent::Loading { .. }) {
                let _ = held_tx.lock().unwrap().send(());
                let (lock, opened) = &*sink_gate;
                let mut open = lock.lock().unwrap();
                let deadline = Instant::now() + Duration::from_secs(30);
                while !*open {
                    let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    open = opened.wait_timeout(open, left).unwrap().0;
                }
            }
            tx.send(e).is_ok()
        });
        let svc = DataService::open(config, sink).unwrap();
        let doc = |i: usize| DocumentJob {
            source: "cvi".into(),
            dataset: "cvi_params".into(),
            rows: cvi_doc(&format!("K{i}.Z"), [1., 2., 3., 4., 5., 6.]),
            source_time: Utc::now(),
            received_at: Utc::now(),
            bytes: 0,
        };
        svc.ingest.submit_document(doc(0));
        held_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the writer holds on the first document");
        for i in 1..=65 {
            svc.ingest.submit_document(doc(i));
        }
        let (worst, detail) = until(&rx, |e| match e {
            DataEvent::Health {
                source,
                worst: worst @ Health::Degraded { .. },
                detail,
            } => {
                assert_eq!(source, "cvi");
                Some((worst, detail))
            }
            _ => None,
        });
        assert_eq!(
            worst,
            Health::Degraded {
                reason: "ingest backlog 65".into()
            }
        );
        assert_eq!(detail, "cvi:backlog: ingest backlog 65");
        {
            let (lock, opened) = &*gate;
            *lock.lock().unwrap() = true;
            opened.notify_all();
        }
        until(&rx, |e| match e {
            DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            } if source == "cvi" => Some(()),
            _ => None,
        });
        svc.shutdown();
    }

    /// A real database file whose `cvi_params` live table gained a column
    /// since it was created, opened under a subscribed `cvi` source.
    #[test]
    fn a_drifted_dataset_fails_its_sources_refuses_its_reads_and_says_so_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("geode.duckdb");
        {
            let store = Store::open(&path).unwrap();
            store.apply_schema(&cvi_dataset()).unwrap();
            store
                .writer()
                .execute_batch("alter table cvi_params_document_live add column surprise VARCHAR;")
                .unwrap();
        }
        let (bus, feed) = crate::adapter::ChannelAdapter::new("demo_bus");
        let mut adapters = AdapterRegistry::default();
        adapters.register(bus);
        let mut documents = DocumentRegistry::default();
        documents.register(Arc::new(crate::store::ddl::tests_support::FakeKind::new()));
        let mut schema = SchemaSpec::default();
        schema.datasets.push(cvi_dataset());
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: path,
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                adapter: "demo_bus".into(),
                document: Some("fake_cvi".into()),
                topics: vec!["cvi/>".into()],
                coalesce: Duration::ZERO,
                ..crate::source::SourceSpec::directory("cvi", "cvi_params", Vec::new())
            }],
            adapters,
            documents,
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();
        let expected = "schema drift in 'cvi_params': 'cvi_params_document_live' column";
        let mut errors = Vec::new();
        let failed = until_within(&rx, Duration::from_secs(30), |e| match e {
            DataEvent::Diagnostics(d) => {
                errors.extend(
                    d.into_iter()
                        .filter(|d| d.severity == Severity::Error)
                        .map(|d| d.message),
                );
                None
            }
            DataEvent::Health {
                source,
                worst: Health::Failed { reason },
                ..
            } => {
                assert_eq!(source, "cvi");
                Some(reason)
            }
            _ => None,
        })
        .expect("the drifted dataset's source reports Failed");
        assert!(failed.starts_with(expected), "{failed}");
        assert!(
            failed.ends_with("; delete the table or fix the dataset"),
            "{failed}"
        );
        assert_eq!(
            errors,
            vec![failed.clone()],
            "one Error diagnostic, at open"
        );
        // The read is refused with the drift reason, not compiled.
        let refused = service
            .document(&DocumentParams {
                key: QueryKey(9),
                tag: 1,
                submitted: Instant::now(),
                dataset: "cvi_params".into(),
                document_key: vec!["SPX.Z".into()],
                as_of: AsOf::Live,
            })
            .unwrap_err();
        assert_eq!(refused.to_string(), failed);
        // Nothing subscribed: a message on the bus loads nothing.
        feed.publish(
            "cvi/SPX.Z",
            crate::store::ddl::tests_support::FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        let deadline = Instant::now() + Duration::from_millis(500);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if let Ok(DataEvent::Published { dataset, .. }) = rx.recv_timeout(left) {
                panic!("published into a drifted dataset: {dataset}");
            }
        }
        service.shutdown();
    }

    /// The drop report leaves the service as the SOURCE's health — the name
    /// the app describes with its dataset — so a tile's chip shows it.
    #[test]
    fn a_flooded_subscription_reports_its_drops_as_degraded_source_health() {
        use crate::store::ddl::tests_support::{FakeKind, GateKind};
        let (kind, gate) = GateKind::new();
        let (_dir, feed, service, rx) = subscribed_service(kind, "demo_bus");
        let body = FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]);
        let deadline = Instant::now() + Duration::from_secs(30);
        while service.subscriptions.lock().unwrap()[0].refused() == 0 {
            assert!(Instant::now() < deadline, "the queue never overflowed");
            feed.publish("cvi/SPX.Z", body.clone());
        }
        GateKind::open(&gate);
        let (worst, detail) = until_within(&rx, Duration::from_secs(30), |e| match e {
            DataEvent::Health {
                source,
                worst: worst @ Health::Degraded { .. },
                detail,
            } => {
                assert_eq!(
                    source, "cvi",
                    "filed under the source, not '<source>:queue'"
                );
                Some((worst, detail))
            }
            _ => None,
        })
        .expect("the source reports its drops as Degraded");
        let Health::Degraded { reason } = worst else {
            unreachable!()
        };
        assert!(reason.contains(" messages dropped since "), "{reason}");
        assert!(detail.starts_with("cvi:queue: "), "{detail}");
        service.shutdown();
        // Stopping the subscription ends the receiver: its open episode is
        // cleared, not left on the chip until restart. Nothing else could
        // turn the source Ok inside the 60 s quiet interval.
        let cleared = until_within(&rx, Duration::from_secs(5), |e| match e {
            DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            } if source == "cvi" => Some(()),
            _ => None,
        });
        assert!(
            cleared.is_some(),
            "the queue Degraded clears when the subscription ends"
        );
    }

    /// The blotter's read path: live and archive are unioned with `select *`
    /// by position, so a view over a drifted dataset must be refused with the
    /// drift reason rather than read values out of the wrong columns. The
    /// picker's distinct read over the same dataset is refused the same way.
    #[test]
    fn a_view_or_distinct_read_of_a_drifted_dataset_is_refused_with_the_drift() {
        let (_db, _src, service, rx) = service_with(|store| {
            store
                .writer()
                .execute_batch(
                    "alter table risk_snapshot_position_archive add column surprise VARCHAR;",
                )
                .unwrap();
        });
        let refused = service
            .query(&params(1, "tree", &Scope::default(), AsOf::Live, 3))
            .unwrap_err()
            .to_string();
        assert!(
            refused.starts_with(
                "schema drift in 'risk_snapshot': 'risk_snapshot_position_archive' column"
            ),
            "{refused}"
        );
        assert!(
            refused.ends_with("; delete the table or fix the dataset"),
            "{refused}"
        );
        let distinct = service
            .distinct(&DistinctParams {
                key: QueryKey(2),
                tag: 2,
                column: "book".into(),
                scope: Scope::default(),
                as_of: AsOf::Live,
            })
            .unwrap_err()
            .to_string();
        assert_eq!(distinct, refused);
        assert!(
            until_within(&rx, Duration::from_millis(300), |e| match e {
                DataEvent::Query(o) => Some(o.key),
                DataEvent::Distinct(o) => Some(o.key),
                _ => None,
            })
            .is_none(),
            "nothing was submitted to the pool"
        );
        service.shutdown();
    }

    /// A drifted series dataset: its fetch source reports `Failed`, a series
    /// read is refused before compiling, and a fetch is answered with the
    /// drift reason without asking the adapter.
    #[test]
    fn a_series_read_or_fetch_of_a_drifted_dataset_is_refused_with_the_drift() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
            store
                .apply_schema(&crate::store::ddl::tests_support::series_dataset())
                .unwrap();
            store
                .writer()
                .execute_batch("alter table series_series add column surprise VARCHAR;")
                .unwrap();
        }
        let (_dir, calls, service, rx) = fetch_service_over(dir, None, false);
        let refused = service
            .series(&series_params("SPX.close"))
            .unwrap_err()
            .to_string();
        assert!(
            refused.starts_with("schema drift in 'series': 'series_series' column"),
            "{refused}"
        );
        service.fetch(&fetch_params(
            "SPX.close",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let fetched = until_within(&rx, Duration::from_secs(30), |e| match e {
            DataEvent::SeriesFetched { result, .. } => Some(result),
            _ => None,
        })
        .expect("the fetch is answered");
        assert_eq!(fetched, Err(refused));
        assert!(
            calls.lock().unwrap().is_empty(),
            "the adapter was not asked"
        );
        service.shutdown();
    }

    /// A drifted local table that already holds rows and has no generation
    /// summary: open must not try to rebuild the summary from it, and a local
    /// publish into it is refused unwritten.
    #[test]
    fn a_local_publish_into_a_drifted_dataset_is_refused_unwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("geode.duckdb");
        Store::open(&path)
            .unwrap()
            .writer()
            .execute_batch(
                "create table sheets_document_live (
                     sheet VARCHAR, line BIGINT, qty BIGINT, batch VARCHAR, book VARCHAR,
                     source_file_id BIGINT, source_time TIMESTAMPTZ
                 );
                 insert into sheets_document_live values ('old', 1, 1, 'old', NULL, 0, now());",
            )
            .unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(local_dataset());
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: path,
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            positions: None,
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
        })
        .expect("a drifted table does not stop the service opening");
        service.publish(LocalPublish {
            dataset: "sheets".into(),
            rows: sheet_rows("S1", &[1, 2]),
        });
        let reason = until_within(&rx, Duration::from_secs(30), |e| match e {
            DataEvent::LocalPublishFailed { reason, .. } => Some(reason),
            DataEvent::Published { dataset, .. } => panic!("written: {dataset}"),
            _ => None,
        })
        .expect("the publish is answered");
        assert!(reason.starts_with("schema drift in 'sheets': "), "{reason}");
        let rows: i64 = service
            .conn
            .query_row("select count(*) from sheets_document_live", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1, "only the row that was already there");
        service.shutdown();
    }

    /// The next `Published`, skipping anything else.
    fn next_published(
        rx: &std::sync::mpsc::Receiver<DataEvent>,
    ) -> (String, String, Vec<Option<String>>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Published {
                    dataset,
                    batch,
                    books,
                    ..
                } => return (dataset, batch, books),
                _ => continue,
            }
        }
    }

    /// The next `Health`, skipping anything else.
    fn next_health(rx: &std::sync::mpsc::Receiver<DataEvent>) -> (String, Health, String) {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Health {
                    source,
                    worst,
                    detail,
                } => return (source, worst, detail),
                _ => continue,
            }
        }
    }

    /// Allow only the runner's startup idle LoadEnded within the timeout. These
    /// fixtures have one unservable source and submit no ingest work.
    fn assert_nothing_but_the_idle_drain_arrives(
        rx: &std::sync::mpsc::Receiver<DataEvent>,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            match rx.recv_timeout(remaining) {
                Ok(DataEvent::LoadEnded) => continue,
                Ok(other) => panic!("a skipped source subscribes to nothing: {other:?}"),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return,
                Err(e) => panic!("channel closed unexpectedly: {e:?}"),
            }
        }
    }

    /// A fetch adapter for the service tests: `FakeFetch` from
    /// `ingest::fetch`'s tests, wrapped as an `Adapter` named `fake_kdb`.
    struct FakeFetchAdapter {
        calls: Arc<std::sync::Mutex<Vec<crate::adapter::FetchRequest>>>,
        catalogue: Option<Vec<String>>,
        /// See `FakeFetch::fail_once`: "broken" recovers on its second
        /// ask, which is the only way a test can watch the load lane
        /// clear.
        fail_once: bool,
    }

    impl crate::adapter::Adapter for FakeFetchAdapter {
        fn name(&self) -> &'static str {
            "fake_kdb"
        }
        fn subscription(&self) -> Option<Box<dyn crate::adapter::Subscription>> {
            None
        }
        fn egress(&self) -> Option<Box<dyn crate::adapter::Egress>> {
            None
        }
        fn fetch(&self) -> Option<Box<dyn crate::adapter::Fetch>> {
            Some(Box::new(crate::ingest::fetch::tests::FakeFetch {
                calls: self.calls.clone(),
                n: 3,
                catalogue: self.catalogue.clone(),
                fail_once: self.fail_once,
            }))
        }
    }

    /// A service with one FETCH source over the
    /// series dataset — the shape `SourceSpec::shape` answers `Fetch`
    /// for: a subscribed source whose dataset is of the series family.
    fn fetch_service(
        catalogue: Option<Vec<String>>,
    ) -> (
        tempfile::TempDir,
        Arc<std::sync::Mutex<Vec<crate::adapter::FetchRequest>>>,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        fetch_service_with(catalogue, false)
    }

    fn fetch_service_with(
        catalogue: Option<Vec<String>>,
        fail_once: bool,
    ) -> (
        tempfile::TempDir,
        Arc<std::sync::Mutex<Vec<crate::adapter::FetchRequest>>>,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        fetch_service_over(tempfile::tempdir().unwrap(), catalogue, fail_once)
    }

    /// `fetch_service_with` over a database directory the caller prepared.
    fn fetch_service_over(
        dir: tempfile::TempDir,
        catalogue: Option<Vec<String>>,
        fail_once: bool,
    ) -> (
        tempfile::TempDir,
        Arc<std::sync::Mutex<Vec<crate::adapter::FetchRequest>>>,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(FakeFetchAdapter {
            calls: calls.clone(),
            catalogue,
            fail_once,
        }));
        let mut schema = SchemaSpec::default();
        schema
            .datasets
            .push(crate::store::ddl::tests_support::series_dataset());
        let spec = crate::source::SourceSpec {
            adapter: "fake_kdb".to_string(),
            ..crate::source::SourceSpec::directory("kdb_hist", "series", Vec::new())
        };
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: vec![spec],
            adapters,
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();
        (dir, calls, service, rx)
    }

    struct PanickingCatalogueAdapter;
    impl crate::adapter::Adapter for PanickingCatalogueAdapter {
        fn name(&self) -> &'static str {
            "fake_kdb"
        }
        fn subscription(&self) -> Option<Box<dyn crate::adapter::Subscription>> {
            None
        }
        fn egress(&self) -> Option<Box<dyn crate::adapter::Egress>> {
            None
        }
        fn fetch(&self) -> Option<Box<dyn crate::adapter::Fetch>> {
            struct Listing;
            impl crate::adapter::Fetch for Listing {
                fn fetch(
                    &mut self,
                    _: &crate::adapter::FetchRequest,
                ) -> Result<crate::adapter::SeriesRows, crate::adapter::AdapterError>
                {
                    unreachable!("only the catalogue is asked")
                }
                fn catalogue(&mut self) -> Option<Vec<String>> {
                    panic!("the listing fell over")
                }
            }
            Some(Box::new(Listing))
        }
    }

    /// Nobody asked for the listing (open asks), so the only door is an
    /// error diagnostic naming the source and the payload.
    #[test]
    fn a_panicking_identity_listing_is_an_error_diagnostic_naming_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(PanickingCatalogueAdapter));
        let mut schema = SchemaSpec::default();
        schema
            .datasets
            .push(crate::store::ddl::tests_support::series_dataset());
        let (_service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: vec![crate::source::SourceSpec {
                adapter: "fake_kdb".to_string(),
                ..crate::source::SourceSpec::directory("kdb_hist", "series", Vec::new())
            }],
            adapters,
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();
        let message = until(&rx, |e| match e {
            DataEvent::Diagnostics(d) => d
                .into_iter()
                .find(|d| d.severity == Severity::Error && d.message.contains("identity listing")),
            _ => None,
        });
        assert!(
            message
                .message
                .contains("identity listing for kdb_hist panicked: the listing fell over"),
            "{message:?}"
        );
    }

    /// The stale check fails open (a failed lookup must not discard the
    /// load) and is reported: an error naming the file reaches the sink,
    /// since a catalog row the lookup cannot read is corruption.
    #[test]
    fn a_failed_stale_check_is_an_error_diagnostic_through_the_service() {
        let (db, src, store, ds, _emitted) = crate::ingest::load::tests_support::fixture();
        let spec = crate::source::SourceSpec {
            pending_timeout: Duration::from_secs(3600),
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            ..crate::source::SourceSpec::directory(
                "risk",
                "risk_snapshot",
                vec![format!("{}/*.csv", src.path().display())],
            )
        };
        let found =
            crate::source::discover(&spec, &Catalog::new(store.writer()), SystemTime::now())
                .unwrap();
        let plan = crate::ingest::build_plan(&[(spec, found)]);
        let poisoned = plan.items[0].clone();
        // A NULL mtime makes the catalog lookup's row read panic.
        store
            .writer()
            .execute_batch(&format!(
                "insert into file_generations
                     (file_id, dataset, batch, path, size, mtime, source_time,
                      gen_id, loaded_at, row_count, health, health_reason,
                      archived_only)
                 values
                     (-1, '{}', '{}', '{}', {}, NULL, '{}'::timestamptz, -1,
                      now(), 1, 'ok', NULL, false);",
                poisoned.dataset,
                poisoned.batch,
                poisoned.candidate.csv_path.display(),
                poisoned.candidate.size,
                poisoned.source_time.to_rfc3339(),
            ))
            .unwrap();
        drop(store);
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let (service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();
        service.ingest.submit(crate::ingest::WorkPlan {
            items: vec![poisoned.clone()],
        });
        let error = until(&rx, |e| match e {
            DataEvent::Diagnostics(d) => d.into_iter().find(|d| d.severity == Severity::Error),
            _ => None,
        });
        assert!(
            error
                .message
                .contains(&poisoned.candidate.csv_path.display().to_string())
                && error.message.contains("panicked"),
            "{error:?}"
        );
        service.shutdown();
    }

    #[test]
    fn a_distinct_answer_without_its_columns_is_an_err_for_its_key() {
        let empty = geode_core::snapshot::Snapshot::from_batches(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            geode_core::snapshot::Provenance::default(),
        )
        .unwrap();
        let event = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            result_event(
                crate::query::pool::QueryResult {
                    id: 1,
                    key: QueryKey(3),
                    tag: 2,
                    submitted: Instant::now(),
                    view: crate::query::pool::ViewId("distinct".into()),
                    payload: Ok(Payload::Snapshot(empty)),
                    kind: RequestKind::Distinct {
                        column: "book".into(),
                    },
                },
                &HealthTracker::default(),
            )
        }))
        .expect("a distinct answer without its columns is an Err, not a panic");
        match event {
            DataEvent::Distinct(o) => {
                assert_eq!((o.key, o.tag, o.column.as_str()), (QueryKey(3), 2, "book"));
                assert!(
                    o.values.is_err_and(|e| e.contains("value")),
                    "an Err for the key, not a panic"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_panic_building_a_result_event_answers_its_key_with_an_error() {
        let event = std::panic::catch_unwind(|| {
            contained_result_event(
                crate::query::pool::QueryResult {
                    id: 1,
                    key: QueryKey(4),
                    tag: 7,
                    submitted: Instant::now(),
                    view: crate::query::pool::ViewId("v".into()),
                    payload: Err("unused".into()),
                    kind: RequestKind::Query,
                },
                |_| panic!("the event builder fell over"),
            )
        })
        .expect("the build's panic is contained, not propagated");
        match event {
            DataEvent::Query(o) => {
                assert_eq!((o.key, o.tag), (QueryKey(4), 7));
                let reason = o.snapshot.expect_err("a panicking build is an error");
                assert!(reason.contains("the event builder fell over"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    fn next_series_fetched(
        rx: &std::sync::mpsc::Receiver<DataEvent>,
    ) -> (String, String, Result<u64, String>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::SeriesFetched {
                    source,
                    identity,
                    result,
                } => return (source, identity, result),
                _ => continue,
            }
        }
    }

    fn fetch_params(identity: &str, from: &str, to: &str) -> FetchParams {
        FetchParams {
            key: QueryKey(7),
            source: "kdb_hist".into(),
            identity: identity.into(),
            from: ts(from),
            to: ts(to),
        }
    }

    fn next_series(rx: &std::sync::mpsc::Receiver<DataEvent>) -> geode_core::series::SeriesOutcome {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Series(o) => return o,
                _ => continue,
            }
        }
    }

    fn series_params(identity: &str) -> geode_core::series::SeriesParams {
        use geode_core::series::*;
        SeriesParams {
            key: QueryKey(7),
            tag: 5,
            submitted: Instant::now(),
            dataset: "series".into(),
            range: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            window: (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
            as_of: AsOf::Live,
            frequency: Frequency::D1,
            series: vec![SeriesSpec {
                slot: 1,
                kind: SlotKind::Source {
                    source: "kdb_hist".into(),
                    identity: identity.into(),
                    rule: BucketRule::Last,
                },
            }],
            percentiles: vec![0.5],
            bins: None,
        }
    }

    /// Verify the pool result reaches the service sink with its request key/tag
    /// and the load health for each source slot.
    #[test]
    fn a_series_request_answers_with_the_bucketed_values_and_the_pairs_health() {
        let (_d, _calls, service, rx) = fetch_service(None);
        service.fetch(&fetch_params(
            "SPX.close",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let _ = next_series_fetched(&rx);
        service.series(&series_params("SPX.close")).unwrap();
        let o = next_series(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(7), 5));
        let r = o.result.unwrap();
        assert_eq!(r.buckets.len(), 1);
        // The `pairs` round trip: one slot came back, under the number
        // the request gave it, and its health was filed against that
        // number rather than a positional guess.
        assert_eq!(r.slots.len(), 1);
        assert_eq!(r.slots[0].slot, 1);
        assert_eq!(
            r.slots[0].values,
            vec![2.0],
            "the FakeFetch's three bars are 0, NaN (dropped), 2; last wins"
        );
        assert_eq!(r.slots[0].percentiles, vec![(0.5, 2.0)]);
        assert_eq!(
            r.slots[0].provenance.health,
            Some(Health::Ok),
            "the load lane's word rides the outcome"
        );
        assert!(r.slots[0].provenance.loaded.is_some());
    }

    /// The same door carries a failure: the pair's load lane is what a
    /// slot reports, so a fetch that failed marks the slot it belongs to
    /// rather than the whole request.
    #[test]
    fn a_failed_pairs_health_rides_its_slot() {
        let (_d, _calls, service, rx) = fetch_service(None);
        service.fetch(&fetch_params(
            "broken",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let _ = next_series_fetched(&rx);
        service.series(&series_params("broken")).unwrap();
        let o = next_series(&rx);
        let r = o.result.unwrap();
        assert!(r.buckets.is_empty());
        assert!(
            matches!(r.slots[0].provenance.health, Some(Health::Failed { .. })),
            "{:?}",
            r.slots[0].provenance
        );
    }

    /// Place an expression between two source slots. Matching source health by
    /// position would mark the expression failed and leave the second source
    /// clean; matching by slot number preserves both assignments.
    #[test]
    fn health_is_attached_by_slot_number_not_position() {
        use geode_core::series::expr::{Ast, Op};
        use geode_core::series::{BucketRule, SeriesSpec, SlotKind};

        let (_d, _calls, service, rx) = fetch_service(None);
        service.fetch(&fetch_params(
            "SPX.close",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let _ = next_series_fetched(&rx);
        service.fetch(&fetch_params(
            "broken",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let _ = next_series_fetched(&rx);

        let source_slot = |slot: u8, identity: &str| SeriesSpec {
            slot,
            kind: SlotKind::Source {
                source: "kdb_hist".into(),
                identity: identity.into(),
                rule: BucketRule::Last,
            },
        };
        let mut p = series_params("SPX.close");
        p.series = vec![
            source_slot(1, "SPX.close"),
            SeriesSpec {
                slot: 3,
                kind: SlotKind::Expr(Ast::Bin(
                    Op::Mul,
                    Box::new(Ast::Ref(1)),
                    Box::new(Ast::Num(2.0)),
                )),
            },
            source_slot(2, "broken"),
        ];
        service.series(&p).unwrap();

        let r = next_series(&rx).result.unwrap();
        assert_eq!(
            r.slots.iter().map(|s| s.slot).collect::<Vec<_>>(),
            vec![1, 3, 2],
            "slots come back in REQUEST order, not sorted"
        );
        assert_eq!(
            r.slots[0].provenance.health,
            Some(Health::Ok),
            "slot 1 is the pair that fetched cleanly"
        );
        assert_eq!(
            r.slots[1].provenance.health, None,
            "slot 3 is an expression: no pair, so no lane to read"
        );
        assert!(
            matches!(r.slots[2].provenance.health, Some(Health::Failed { .. })),
            "slot 2 is the pair that failed: {:?}",
            r.slots[2].provenance
        );
    }

    /// Use an oversized request with an unknown dataset. The cap error must win
    /// over compilation's unknown-dataset error, proving validation order.
    #[test]
    fn a_capped_request_is_refused_before_compilation() {
        let (_d, _calls, service, _rx) = fetch_service(None);
        let mut p = series_params("SPX.close");
        p.frequency = geode_core::series::Frequency::M1;
        p.range = (ts("2026-01-05T00:00:00Z"), ts("2029-01-05T00:00:00Z"));
        p.window = p.range;
        p.dataset = "nope".into();
        let e = service.series(&p).unwrap_err().to_string();
        assert!(
            e.contains("1m over 3y is ") && e.contains("; the cap is 500,000"),
            "{e}"
        );
        assert!(
            !e.contains("unknown dataset"),
            "the compiler never ran: {e}"
        );
    }

    /// Through `DataService::spawn` so the serve loop's error arm is what
    /// answers: a compile failure is this key's own outcome, never a lost
    /// request.
    #[test]
    fn a_compile_error_is_the_requests_own_outcome_through_the_handle() {
        let dir = tempfile::tempdir().unwrap();
        let mut schema = SchemaSpec::default();
        schema
            .datasets
            .push(crate::store::ddl::tests_support::series_dataset());
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn(
            DataServiceConfig {
                db_path: dir.path().join("geode.duckdb"),
                schema,
                views: Vec::new(),
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: Vec::new(),
                adapters: Default::default(),
                documents: Default::default(),
                egress: Vec::new(),
                clock: geode_core::clock::Clock::utc(),
                pricer: PricerConfig::default(),
                vol: crate::vol::VolConfig::default(),
                positions: None,
            },
            sink,
        );
        let mut p = series_params("X");
        p.dataset = "nope".into();
        assert!(handle.series(p).is_ok());
        let o = next_series(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(7), 5));
        assert!(o.result.unwrap_err().contains("unknown dataset 'nope'"));
        handle.shutdown();
    }

    #[test]
    fn a_fetch_lands_rows_and_announces_the_pair() {
        let (_d, calls, service, rx) = fetch_service(None);
        service.fetch(&fetch_params(
            "SPX.close",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let (source, identity, result) = next_series_fetched(&rx);
        assert_eq!(
            (source.as_str(), identity.as_str()),
            ("kdb_hist", "SPX.close")
        );
        assert_eq!(result, Ok(2), "three bars, one NaN dropped");
        assert_eq!(calls.lock().unwrap().len(), 1);
        let n: i64 = service
            .conn
            .query_row("select count(*) from series_series", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn a_covered_span_is_answered_without_asking_the_source() {
        let (_d, calls, service, rx) = fetch_service(None);
        service.fetch(&fetch_params(
            "SPX.close",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let _ = next_series_fetched(&rx);
        service.fetch(&fetch_params(
            "SPX.close",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let (_, _, result) = next_series_fetched(&rx);
        assert_eq!(result, Ok(0));
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "no second call reached the adapter"
        );
    }

    #[test]
    fn widening_the_range_fetches_only_the_gaps() {
        let (_d, calls, service, rx) = fetch_service(None);
        service.fetch(&fetch_params(
            "SPX.close",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let _ = next_series_fetched(&rx);
        service.fetch(&fetch_params(
            "SPX.close",
            "2026-01-04T00:00:00Z",
            "2026-01-07T00:00:00Z",
        ));
        let _ = next_series_fetched(&rx);
        let _ = next_series_fetched(&rx);
        let calls = calls.lock().unwrap();
        let spans: Vec<(DateTime<Utc>, DateTime<Utc>)> =
            calls.iter().map(|c| (c.from, c.to)).collect();
        assert_eq!(
            spans,
            vec![
                (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z")),
                (ts("2026-01-04T00:00:00Z"), ts("2026-01-05T00:00:00Z")),
                (ts("2026-01-06T00:00:00Z"), ts("2026-01-07T00:00:00Z")),
            ]
        );
    }

    /// A fetch wholly outside the dataset history window must complete without
    /// calling the source; append retention would retain none of its rows.
    #[test]
    fn a_fetch_older_than_the_history_window_is_answered_without_asking_the_source() {
        let (_d, calls, service, rx) = fetch_service(None);
        service.fetch(&fetch_params(
            "SPX.close",
            "2015-01-01T00:00:00Z",
            "2015-01-02T00:00:00Z",
        ));
        let (_, _, result) = next_series_fetched(&rx);
        assert_eq!(result, Ok(0));
        assert_eq!(calls.lock().unwrap().len(), 0);
    }

    /// Opening a servable fetch source must deliver its clean discovery state,
    /// not merely mark it as reported inside the tracker.
    #[test]
    fn a_servable_fetch_source_reports_ok_on_the_discovery_lane_at_open() {
        let (_d, _calls, service, rx) = fetch_service(None);
        let (source, worst, _) = next_health(&rx);
        assert_eq!(source, "kdb_hist");
        assert_eq!(worst, Health::Ok);
        service.shutdown();
    }

    #[test]
    fn a_failed_fetch_is_a_load_lane_failure_keyed_by_the_pair_and_clears_on_success() {
        // `fail_once`: "broken" fails the first ask and answers bars on
        // the second, so the SAME pair can be watched failing and then
        // recovering — the only shape in which the load lane's key is
        // observable, since a key that differs between the failing path
        // (the fetch worker's own `Failed`) and the succeeding one (the
        // runner's `SeriesAppended`) leaves the failure standing forever
        // while every assertion about the failure itself still passes.
        let (_d, _calls, service, rx) = fetch_service_with(None, true);
        service.fetch(&fetch_params(
            "broken",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        // Collect both outcomes in one drain regardless of arrival order. Waiting
        // for one while discarding the other would impose an unnecessary ordering.
        let mut fetched = None;
        let mut health = None;
        while fetched.is_none() || health.is_none() {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::SeriesFetched {
                    source,
                    identity,
                    result,
                } => fetched = Some((source, identity, result)),
                DataEvent::Health {
                    source,
                    worst,
                    detail,
                } if worst != Health::Ok => health = Some((source, worst, detail)),
                _ => continue,
            }
        }
        let (_, identity, result) = fetched.unwrap();
        assert_eq!(identity, "broken");
        assert_eq!(result, Err("no such symbol".to_string()));
        let health = health.unwrap();
        assert_eq!(health.0, "kdb_hist");
        assert!(matches!(health.1, Health::Failed { .. }));
        assert!(health.2.starts_with("broken@kdb_hist:"), "{}", health.2);

        // The clear-on-success half. The retry of the same pair succeeds
        // and lands rows, so the runner's `SeriesAppended` arm reports
        // `Ok` under `"broken@kdb_hist"` — the key the failure above was
        // filed under — and the lane transitions back to `Ok`. Under a
        // key that differs between the two paths the failure is never
        // cleared, no `Ok` is ever emitted, and this loop runs out its
        // deadline.
        service.fetch(&fetch_params(
            "broken",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut cleared = false;
        let mut refetched = None;
        while !cleared || refetched.is_none() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let never_cleared = "the load lane never cleared to Ok after the retry succeeded";
            assert!(!left.is_zero(), "{never_cleared}");
            match rx.recv_timeout(left) {
                Ok(DataEvent::Health { source, worst, .. }) if source == "kdb_hist" => {
                    assert_eq!(worst, Health::Ok, "the retry clears the pair's failure");
                    cleared = true;
                }
                Ok(DataEvent::SeriesFetched {
                    identity, result, ..
                }) => refetched = Some((identity, result)),
                Ok(_) => continue,
                Err(_) => panic!("{never_cleared}"),
            }
        }
        assert_eq!(
            refetched.unwrap(),
            ("broken".to_string(), Ok(2)),
            "three bars, one NaN dropped"
        );
        drop(rx);
        drop(service);
    }

    #[test]
    fn an_unknown_source_or_a_non_fetch_source_fails_at_once() {
        let (_d, _calls, service, rx) = fetch_service(None);
        service.fetch(&FetchParams {
            source: "nope".into(),
            ..fetch_params("SPX.close", "2026-01-05T00:00:00Z", "2026-01-06T00:00:00Z")
        });
        let (_, _, result) = next_series_fetched(&rx);
        assert_eq!(
            result,
            Err("source 'nope' is not a fetch source".to_string())
        );
    }

    #[test]
    fn a_fetch_source_whose_adapter_has_no_fetch_side_is_failed_on_the_discovery_lane_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let (bus, _feed) = crate::adapter::ChannelAdapter::new("demo_bus");
        let mut adapters = AdapterRegistry::default();
        adapters.register(bus);
        let mut schema = SchemaSpec::default();
        schema
            .datasets
            .push(crate::store::ddl::tests_support::series_dataset());
        let spec = crate::source::SourceSpec {
            adapter: "demo_bus".into(),
            ..crate::source::SourceSpec::directory("kdb_hist", "series", Vec::new())
        };
        let (_service, rx) = DataService::open_channel(DataServiceConfig {
            db_path: dir.path().join("geode.duckdb"),
            schema,
            views: Vec::new(),
            dimensions: DerivedDimensions::default(),
            query_workers: 2,
            sources: vec![spec],
            adapters,
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();
        let health = loop {
            match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                DataEvent::Health {
                    source,
                    worst,
                    detail,
                } => break (source, worst, detail),
                _ => continue,
            }
        };
        assert_eq!(health.0, "kdb_hist");
        assert_eq!(
            health.1,
            Health::Failed {
                reason: "adapter 'demo_bus' has no fetch side".into()
            }
        );
    }

    #[test]
    fn the_catalog_lists_series_spans_and_source_identities() {
        let (_d, _calls, service, rx) = fetch_service(Some(vec!["VIX".into(), "SPX.close".into()]));
        service.fetch(&fetch_params(
            "SPX.close",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
        ));
        let _ = next_series_fetched(&rx);
        // Identities were requested at open; wait for them to land before
        // reading.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let snap = loop {
            let out = service.catalog(&CatalogParams {
                key: QueryKey(1),
                tag: 1,
                as_of: AsOf::Live,
            });
            let snap = out.snapshot.unwrap();
            if !snap.identities.is_empty() || std::time::Instant::now() > deadline {
                break snap;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            snap.identities,
            vec![(
                "kdb_hist".to_string(),
                vec!["SPX.close".to_string(), "VIX".to_string()]
            )],
            "sorted"
        );
        let ds = snap.datasets.iter().find(|d| d.name == "series").unwrap();
        assert_eq!(ds.series.len(), 1);
        let s = &ds.series[0];
        assert_eq!(
            (s.source.as_str(), s.identity.as_str(), s.fetches),
            ("kdb_hist", "SPX.close", 1)
        );
        assert_eq!(
            (s.from, s.to),
            (ts("2026-01-05T00:00:00Z"), ts("2026-01-06T00:00:00Z"))
        );
    }

    #[test]
    fn a_subscribed_source_publishes_what_the_feed_sends_and_serves_it_back() {
        let (_dir, feed, svc, rx) = subscribed_service(Arc::new(FakeKind::new()), "demo_bus");
        assert!(feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.])
        ));
        let (dataset, batch, books) = next_published(&rx);
        assert_eq!((dataset.as_str(), batch.as_str()), ("cvi_params", "SPX.Z"));
        // The bookless partition, as a document publish always writes —
        // an empty list would say nothing was written at all.
        assert_eq!(books, vec![None]);

        // And it is queryable through the ordinary document request: the
        // receiver's rows went through the real publish, so nothing about
        // this path knows the source was subscribed rather than a file.
        svc.document(&DocumentParams {
            key: QueryKey(1),
            tag: 1,
            submitted: Instant::now(),
            dataset: "cvi_params".into(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        })
        .unwrap();
        let snap = next(&rx).snapshot.unwrap();
        assert_eq!(snap.rows(), 6);
        assert_eq!(snap.f64_value("param", 0), Some(1.0));
        assert_eq!(snap.text_value("underlying_ref", 0), Some("SPX.Z"));
        svc.shutdown();
    }

    #[test]
    fn a_source_naming_an_adapter_this_build_lacks_is_reported_and_skipped() {
        // An unavailable adapter must fail only its configured source while the
        // service continues answering stored-data requests.
        let (_dir, _feed, svc, rx) = subscribed_service(Arc::new(FakeKind::new()), "solace");
        let (source, worst, detail) = next_health(&rx);
        assert_eq!(source, "cvi");
        match &worst {
            Health::Failed { reason } => {
                assert!(reason.contains("solace"), "{reason}");
                assert!(reason.contains("not in this build"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert!(detail.contains("not in this build"), "{detail}");
        // Still a working service.
        let outcome = svc.catalog(&CatalogParams {
            key: QueryKey(2),
            tag: 2,
            as_of: AsOf::Live,
        });
        let snap = outcome.snapshot.expect("the catalog still answers");
        assert_eq!(snap.datasets[0].name, "cvi_params");
        svc.shutdown();
    }

    /// A registered adapter with an unknown document kind is unservable. The
    /// diagnostic must name the kind while other service operations remain usable.
    #[test]
    fn a_source_naming_a_document_kind_this_build_lacks_is_reported_and_skipped() {
        let (_dir, feed, svc, rx) =
            subscribed_service_for(Arc::new(FakeKind::new()), "demo_bus", "nonesuch_cvi");
        let (source, worst, detail) = next_health(&rx);
        assert_eq!(source, "cvi");
        match &worst {
            Health::Failed { reason } => {
                assert!(reason.contains("nonesuch_cvi"), "{reason}");
                assert!(reason.contains("not registered"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
        assert!(detail.contains("nonesuch_cvi"), "{detail}");
        // Nothing subscribed, so a message on the bus reaches nobody —
        // the source was skipped, not half-started.
        feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        assert_nothing_but_the_idle_drain_arrives(&rx, Duration::from_millis(300));
        svc.shutdown();
    }

    #[test]
    fn a_kind_that_disagrees_with_its_dataset_is_reported_and_skipped() {
        // Reject kind/schema mismatch once at source open, before receiving messages.
        let (_dir, feed, svc, rx) =
            subscribed_service(Arc::new(FakeKind::with_extra_column()), "demo_bus");
        let (source, worst, _) = next_health(&rx);
        assert_eq!(source, "cvi");
        match &worst {
            Health::Failed { reason } => assert!(reason.contains("surface_id"), "{reason}"),
            other => panic!("{other:?}"),
        }
        // Nothing is subscribed, so a message on the bus reaches nobody.
        feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        assert_nothing_but_the_idle_drain_arrives(&rx, Duration::from_millis(300));
        svc.shutdown();
    }

    #[test]
    fn a_lost_connection_is_discovery_health_and_a_parse_failure_outlives_a_reconnect() {
        let (_dir, feed, svc, rx) = subscribed_service(Arc::new(FakeKind::new()), "demo_bus");
        // Subscribing reported `Connected`, which is the discovery lane's
        // `Ok` — the first event this service ever sends.
        assert_eq!(next_health(&rx).1, Health::Ok);

        feed.set_state(ConnectionState::Lost {
            reason: "broker gone".into(),
        });
        let (_, worst, _) = next_health(&rx);
        assert_eq!(
            worst,
            Health::Failed {
                reason: "broker gone".into()
            },
            "a lost connection is the discovery lane's Failed, reason verbatim"
        );

        // A clean document may publish while connection health remains Failed.
        // Its load outcome must not clear the independent discovery lane.
        feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        assert_eq!(next_published(&rx).1, "SPX.Z");

        // A parse failure is the LOAD lane, keyed by the topic (the bytes
        // yielded no key) — and it is what the combined value reports from
        // here on.
        feed.publish("cvi/SPX.Z", b"rubbish".to_vec());
        let (_, worst, detail) = next_health(&rx);
        assert!(
            matches!(&worst, Health::Failed { reason } if reason.starts_with("parse: ")),
            "{worst:?}"
        );
        assert!(detail.starts_with("cvi/SPX.Z: parse: "), "{detail}");

        // Reconnecting cannot clear it: a content-blind connection report
        // says nothing about the documents that did arrive.
        feed.set_state(ConnectionState::Connected);
        let deadline = Instant::now() + Duration::from_millis(300);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(DataEvent::Health { worst, .. }) => {
                    panic!("a reconnect must not re-report health at all here: {worst:?}")
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        svc.shutdown();
    }

    /// A parse failure is keyed by raw topic, while publication uses the parsed
    /// document key. Recovery must clear the raw-topic entry explicitly.
    #[test]
    fn a_parse_failure_sets_the_load_lane_and_a_later_clean_document_clears_it() {
        let (_dir, feed, svc, rx) = subscribed_service(Arc::new(FakeKind::new()), "demo_bus");
        // Subscribing reported `Connected` — the discovery lane's `Ok`.
        assert_eq!(next_health(&rx).1, Health::Ok);

        feed.publish("cvi/SPX.Z", b"rubbish".to_vec());
        let (source, worst, detail) = next_health(&rx);
        assert_eq!(source, "cvi");
        assert!(
            matches!(&worst, Health::Failed { reason } if reason.starts_with("parse: ")),
            "{worst:?}"
        );
        assert!(detail.starts_with("cvi/SPX.Z: parse: "), "{detail}");

        // The same topic, parseable this time.
        feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        let (source, worst, detail) = next_health(&rx);
        assert_eq!((source.as_str(), &worst), ("cvi", &Health::Ok));
        // `Lanes::combined` normalises a clean slot's detail away, so the
        // recovery carries no reason — deliberately: with the lane keyed
        // per batch, whichever clean slot happened to decide would
        // otherwise re-fire an `Ok` the surface already shows.
        assert_eq!(detail, "");
        // And the document itself published, so the clear rides the
        // ordinary path rather than replacing it.
        assert_eq!(next_published(&rx).1, "SPX.Z");
        svc.shutdown();
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
        // The frame's active slot is applied per query, not
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
    fn context_columns_reach_the_query() {
        let (_db, _src, svc, rx) = service();
        svc.set_context_columns(vec!["position_ref".into()]);
        let mut p = params(1, "tree", &Scope::default(), AsOf::Live, 1);
        p.grouping = Some(vec!["lhu".into()]);
        svc.query(&p).unwrap();
        let snap = next(&rx).snapshot.unwrap();
        let ix = snap
            .column_index("position_ref")
            .expect("a hidden context column");
        assert!(
            snap.meta_at(ix).unwrap().mixed_flag.is_some(),
            "linked to its flag"
        );
        svc.shutdown();
    }

    #[test]
    fn a_query_without_context_columns_compiles_as_before() {
        let (_db, _src, svc, rx) = service();
        let mut p = params(1, "tree", &Scope::default(), AsOf::Live, 1);
        p.grouping = Some(vec!["lhu".into()]);
        svc.query(&p).unwrap();
        let snap = next(&rx).snapshot.unwrap();
        assert_eq!(snap.column_index("position_ref"), None);
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
        // Check the synchronous metadata return directly. The handle tests exercise
        // request-loop delivery as DataEvent::Catalog.
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
        svc.document(&bad).unwrap();
        assert!(next(&rx).snapshot.unwrap_err().contains("unknown dataset"));
        svc.shutdown();
    }

    /// Query two live documents with different source times. Each must report
    /// its own selected generation rather than the dataset's minimum time —
    /// and, since the fixture publishes SPX.Z again after NDX.Z, each must
    /// report its own generation ID rather than the dataset's maximum.
    #[test]
    fn a_live_document_request_reports_its_own_documents_freshness() {
        let (_dir, svc, rx) = document_service();

        svc.document(&DocumentParams {
            key: QueryKey(4),
            tag: 1,
            submitted: Instant::now(),
            dataset: "cvi_params".into(),
            document_key: vec!["SPX.Z".into()],
            as_of: AsOf::Live,
        })
        .unwrap();
        let spx = next(&rx).snapshot.unwrap();
        assert_eq!(
            spx.provenance().datasets[0].as_of.as_deref(),
            Some(ts("2026-09-12T14:05:00Z").to_rfc3339().as_str()),
            "SPX.Z's own live generation, not NDX.Z's staler one"
        );
        let spx_generation = spx.provenance().datasets[0].generation;

        svc.document(&DocumentParams {
            key: QueryKey(5),
            tag: 1,
            submitted: Instant::now(),
            dataset: "cvi_params".into(),
            document_key: vec!["NDX.Z".into()],
            as_of: AsOf::Live,
        })
        .unwrap();
        let ndx = next(&rx).snapshot.unwrap();
        assert_eq!(
            ndx.provenance().datasets[0].as_of.as_deref(),
            Some(ts("2026-09-12T14:03:00Z").to_rfc3339().as_str()),
            "NDX.Z's own generation, not SPX.Z's newer one"
        );
        let ndx_generation = ndx.provenance().datasets[0].generation;

        // NDX.Z predates SPX.Z's second publish. A dataset-wide maximum
        // would incorrectly report SPX.Z's generation for both documents.
        assert!(spx_generation.is_some() && ndx_generation.is_some());
        assert_ne!(spx_generation, ndx_generation);
        assert!(
            ndx_generation < spx_generation,
            "NDX.Z's own generation, not the dataset's newest"
        );
        svc.shutdown();
    }

    /// A live request by a key prefix reads every document under it
    /// through the service and reports the matched set's freshness: the
    /// STALEST matched document's newest source time (SPX 2026-11-20 at
    /// 14:05, not SPX 2026-10-16's 14:10 republish) and the greatest
    /// matched generation (SPX 2026-10-16's republish, not SPXW's newer
    /// one). An exact-batch freshness lookup finds no batch named `SPX`
    /// and reports neither.
    #[test]
    fn the_service_reports_a_live_prefix_requests_freshness() {
        let (_dir, svc, rx) = chain_service();
        let request = |key: u64, parts: &[&str]| DocumentParams {
            key: QueryKey(key),
            tag: 1,
            submitted: Instant::now(),
            dataset: "option_chain".into(),
            document_key: parts.iter().map(|p| p.to_string()).collect(),
            as_of: AsOf::Live,
        };

        svc.document(&request(6, &["SPX"])).unwrap();
        let spx = next(&rx).snapshot.unwrap();
        let rows: Vec<(String, String, f64)> = (0..spx.rows())
            .map(|r| {
                (
                    spx.text_value("underlying_ref", r).unwrap().to_string(),
                    spx.text_value("expiry", r).unwrap().to_string(),
                    spx.f64_value("mid_vol", r).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("SPX".into(), "2026-10-16".into(), 0.21),
                ("SPX".into(), "2026-10-16".into(), 0.17),
                ("SPX".into(), "2026-11-20".into(), 0.22),
                ("SPX".into(), "2026-11-20".into(), 0.19),
            ],
            "both SPX expiries, no SPXW rows"
        );
        let freshness = &spx.provenance().datasets[0];
        assert_eq!(freshness.dataset, "option_chain");
        assert_eq!(
            freshness.as_of.as_deref(),
            Some(ts("2026-09-12T14:05:00Z").to_rfc3339().as_str()),
            "the stalest SPX expiry's newest time"
        );

        // The greatest SPX generation is SPX 2026-10-16's republish.
        svc.document(&request(7, &["SPX", "2026-10-16"])).unwrap();
        let october = next(&rx).snapshot.unwrap();
        let october_generation = october.provenance().datasets[0].generation;
        svc.document(&request(8, &["SPXW"])).unwrap();
        let spxw = next(&rx).snapshot.unwrap();
        let spxw_generation = spxw.provenance().datasets[0].generation;
        assert!(october_generation.is_some());
        assert_eq!(
            freshness.generation, october_generation,
            "the greatest SPX generation"
        );
        assert!(
            spxw_generation > october_generation,
            "SPXW's newer generation must not leak into SPX's"
        );
        svc.shutdown();
    }

    #[test]
    fn polled_event_next_is_at_plus_next_in() {
        // Check poll-time arithmetic without depending on scheduler timing.
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
        // An interval too large for timestamp addition must not panic.
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
                adapters: Default::default(),
                documents: Default::default(),
                egress: Vec::new(),
                clock: geode_core::clock::Clock::utc(),
                pricer: PricerConfig::default(),
                vol: crate::vol::VolConfig::default(),
                positions: None,
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
        // Opening the service must diagnose the view by name before its first query,
        // including views no tile opens.
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
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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
    fn a_view_with_an_error_diagnostic_is_refused_by_name_not_compiled() {
        // Invalid views must produce a query refusal naming the problem,
        // even when the caller does not inspect configuration diagnostics.
        let (db, _src, _svc, _rx) = service();
        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);

        let good = crate::ingest::load::tests_support::tree_view();
        let mut broken = good.clone();
        broken.name = "broken".into();
        broken.grouping.push("nosuchcolumn".into());

        let (svc, _rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![good, broken],
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .expect("a broken view must not stop the service opening");

        let err = svc
            .query(&params(1, "broken", &Scope::default(), AsOf::Live, 1))
            .expect_err("a view that cannot be honoured must not compile");
        let message = format!("{err}");
        assert!(
            message.contains("nosuchcolumn"),
            "the refusal must name the column — it is the whole remedy: {message}"
        );

        // One broken view must not take the service down with it.
        assert!(
            svc.query(&params(2, "tree", &Scope::default(), AsOf::Live, 1))
                .is_ok(),
            "a healthy view over the same schema still serves"
        );

        // And a grouping override is not a way past the refusal.
        let mut regrouped = params(3, "broken", &Scope::default(), AsOf::Live, 1);
        regrouped.grouping = Some(vec!["book".into()]);
        assert!(
            svc.query(&regrouped).is_err(),
            "an override cannot bypass it"
        );
    }

    #[test]
    fn a_reload_replaces_the_refusals_rather_than_keeping_the_ones_from_open() {
        // Both directions of the same fact. If a reload merged into the
        // refusals instead of replacing them, a view the author has just
        // corrected would stay refused until the desk restarted the app, and a
        // view the author has just broken would keep serving the blank columns
        // this refusal exists to stop.
        let (db, _src, _svc, _rx) = service();
        let ds = crate::ingest::load::tests_support::fixture().3;
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);

        let good = crate::ingest::load::tests_support::tree_view();
        let mut broken = good.clone();
        broken.name = "broken".into();
        broken.grouping.push("nosuchcolumn".into());

        let (mut svc, _rx) = DataService::open_channel(DataServiceConfig {
            db_path: db.path().join("geode.duckdb"),
            schema,
            views: vec![good.clone(), broken],
            dimensions: DerivedDimensions::default(),
            query_workers: 1,
            sources: Vec::new(),
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .expect("a broken view must not stop the service opening");
        assert!(
            svc.query(&params(1, "broken", &Scope::default(), AsOf::Live, 1))
                .is_err(),
            "the view is refused at open"
        );

        // The author fixes it: same name, honourable grouping.
        let mut fixed = good.clone();
        fixed.name = "broken".into();
        svc.replace_views(
            vec![good.clone(), fixed.clone()],
            DerivedDimensions::default(),
        );
        assert!(
            svc.query(&params(2, "broken", &Scope::default(), AsOf::Live, 1))
                .is_ok(),
            "a view corrected in the configuration serves without a restart"
        );

        // And the other way: a reload that breaks a view that was serving.
        let mut now_broken = good.clone();
        now_broken.grouping.push("nosuchcolumn".into());
        svc.replace_views(vec![now_broken, fixed], DerivedDimensions::default());
        assert!(
            svc.query(&params(3, "tree", &Scope::default(), AsOf::Live, 1))
                .is_err(),
            "a view broken by the reload stops serving at once"
        );

        svc.shutdown();
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
        // Freshness must name the actual selected generation. Reporting the requested
        // instant would hide differences between stale inputs.
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
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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
                // A first clean discovery report emits Ok.
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
    fn a_load_is_bracketed_by_loading_and_load_ended() {
        // One file emits Loading and LoadEnded; draining the queue emits another
        // LoadEnded. Assert both boundaries.
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let csv_path = src.path().join("risk_2026-08-24_BK0.csv");
        std::fs::write(
            &csv_path,
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK0,L0,P1,C,I1,USD,100\n",
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
                // Keep discovery to its initial poll while checking this event sequence.
                poll_interval: Duration::from_secs(3600),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();

        let mut kinds = Vec::new();
        let mut ended = 0;
        while ended < 2 {
            let e = rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("loading, published and two LoadEnded events");
            match e {
                DataEvent::Loading {
                    ref source,
                    ref path,
                    queued,
                } => {
                    assert_eq!(source, "risk");
                    assert!(path.ends_with(".csv"), "{path}");
                    assert_eq!(queued, 0);
                    kinds.push("loading");
                }
                DataEvent::Published { .. } => kinds.push("published"),
                DataEvent::LoadEnded if kinds.is_empty() => {
                    // The runner's own startup drain (queue empty before
                    // discovery ever submits this file) can win the race
                    // and announce first — a real `LoadEnded` with
                    // nothing recorded yet, harmless by construction
                    // (`note_load_ended` is a no-op when idle) and not
                    // part of the sequence this test asserts.
                }
                DataEvent::LoadEnded => {
                    kinds.push("ended");
                    ended += 1;
                }
                _ => {}
            }
        }
        // Exactly two `LoadEnded`: the `Published` arm's own send, then
        // the queue-drain's (`IngestEvent::PlanComplete`) — deterministic
        // because one runner thread sends both in order with nothing else
        // queued behind this file (the pinned `poll_interval` above rules
        // out a second poll landing a third pair).
        assert_eq!(kinds, ["loading", "published", "ended", "ended"]);
        svc.shutdown();
    }

    #[test]
    fn a_failed_load_still_ends_the_strip() {
        // Queue a malformed newer file before a valid older one. The valid file
        // keeps the queue nonempty after failure, isolating the failed operation's
        // LoadEnded from the separate queue-drained announcement.
        let db = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        std::fs::write(
            src.path().join("risk_2026-08-24_BK0.csv"),
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency\n\
             BK0,L0,P1,C,I1,USD\n",
        )
        .unwrap();
        std::fs::write(
            src.path().join("risk_2026-08-24_BK0.csv.done"),
            r#"{"as_of":"2026-08-24T07:00:00Z","columns":["Book","LHU","PositionRef","Counterparty","InstrumentRef","Currency","NPV"],"books":["BK0"]}"#,
        )
        .unwrap();
        std::fs::write(
            src.path().join("risk_2026-08-23_BK1.csv"),
            "Book,LHU,PositionRef,Counterparty,InstrumentRef,Currency,NPV\n\
             BK1,L0,P2,C,I2,USD,50\n",
        )
        .unwrap();
        std::fs::write(
            src.path().join("risk_2026-08-23_BK1.csv.done"),
            r#"{"as_of":"2026-08-23T07:00:00Z","columns":["Book","LHU","PositionRef","Counterparty","InstrumentRef","Currency","NPV"],"books":["BK1"]}"#,
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
                // Keep discovery to its initial poll while checking failure progress.
                poll_interval: Duration::from_secs(3600),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();

        // `Health`, `Polled` and `Diagnostics` events are the failure's
        // own report; this test cares only about the `Loading`/
        // `Published`/`LoadEnded` sequence. The runner's own startup
        // drain can win the race and send a `LoadEnded` before either
        // file is even submitted (harmless — `note_load_ended` is a
        // no-op when idle) — ignored the same way the sibling test above
        // ignores it. Collected until the queue-drain's own trailing
        // `LoadEnded` (the third) arrives.
        let mut kinds = Vec::new();
        let mut ended = 0;
        while ended < 3 {
            let e = rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("loading, ended, loading, published and two more LoadEnded events");
            match e {
                DataEvent::Loading { ref source, .. } => {
                    assert_eq!(source, "risk");
                    kinds.push("loading");
                }
                DataEvent::Published { .. } => kinds.push("published"),
                DataEvent::LoadEnded if kinds.is_empty() => {}
                DataEvent::LoadEnded => {
                    kinds.push("ended");
                    ended += 1;
                }
                _ => {}
            }
        }
        // BK0 (malformed) fails and ends the strip on the Failed arm's
        // own send BEFORE BK1 even starts loading — the queue is not yet
        // empty, so the drain cannot have supplied that first "ended"
        // instead. BK1 then loads cleanly, and the drain supplies the
        // trailing "ended" once both are done.
        assert_eq!(
            kinds,
            ["loading", "ended", "loading", "published", "ended", "ended"]
        );
        svc.shutdown();
    }

    #[test]
    fn a_load_failure_reports_health_under_the_source_name_not_the_dataset_name() {
        // Use different source and dataset names to detect health keyed incorrectly.
        // Leave the dataset undeclared for a deterministic runner failure.
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
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "eod_risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut seen = None;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(5)) {
                // Skip initial clean discovery health while awaiting the ingest failure
                // from the undeclared dataset.
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
    /// instrument grain — same shape as
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
        // A real directory load with conflicting carried currency must publish its
        // degradation through DataEvent::Health as well as publish the generation.
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
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "eod_risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
        })
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(30);
        let mut seen = None;
        let mut published = false;
        while Instant::now() < deadline && (!published || seen.is_none()) {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(DataEvent::Published { .. }) => published = true,
                // Discovery runs independently; its clean report must not be mistaken for
                // the degraded publication this test awaits.
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
        assert_eq!(source, "eod_risk", "keyed by the source name");
        assert!(
            matches!(worst, Health::Degraded { .. }),
            "expected Degraded, got {worst:?}"
        );
        assert!(detail.contains("currency"), "{detail}");
        svc.shutdown();
    }

    /// Let several clean polls run after a degraded publication without replacing
    /// the bad file. Content-blind discovery must not clear its load degradation.
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
                // Short, and deliberately so — several more polls MUST
                // fire during this test's run, each one discovering the
                // same still-bad file as `Unchanged` (not `Ready`, not
                // `Pending`) and reporting a content-blind `Ok`.
                poll_interval: Duration::from_millis(30),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "eod_risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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
            poll_interval: poll,
            pending_timeout: Duration::from_secs(3600),
            batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
            ..crate::source::SourceSpec::directory(
                "eod_risk",
                "risk_snapshot",
                vec![format!("{}/*.csv", src.display())],
            )
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
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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
        // An unchanged unhealthy file is skipped on restart. Its persisted live
        // generation must seed health without requiring another load.
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

        // Repeated Unchanged polls must preserve the seeded load degradation.
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
        // The seed must use the same batch key as publication. A corrected republish
        // can then clear it instead of leaving an unreachable unhealthy entry.
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

    /// Unit tests for lane combination and delivery acknowledgement.
    mod health_tracker_lanes {
        use super::*;

        /// Choose reasons whose alphabetical order opposes change order, so the
        /// test distinguishes last-changed tie breaking from string ordering.
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

        /// Clearing discovery must preserve an unresolved degraded load.
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

        /// Two different degraded reasons have equal severity. The latest changed
        /// lane must surface with its own reason, independent of alphabetical order.
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

        /// Repeated clean polls must not re-emit a standing load degradation.
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

        /// Forward the deciding lane's detail with its health, even when the caller
        /// updated another lane. A clean publish can still leave a stuck-file warning.
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

        /// A clean second batch must not clear the first batch's degraded live data.
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

        /// Refusal must not acknowledge the transition: the next unchanged report
        /// must offer the same health event again.
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

        /// Use try_lock inside the sink to prove emission holds the tracker lock.
        /// This tests ordering directly without a timing race.
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

    /// One poll queues an unhealthy batch before a clean unrelated batch.
    /// A long interval prevents another discovery report from masking whether
    /// the clean publication incorrectly clears the first batch's failure.
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
                // Long enough that the cold-start poll is the only one
                // to ever run within this test — see the doc comment
                // above for why that matters.
                poll_interval: Duration::from_secs(3600),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "eod_risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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

    /// Republish the same batch cleanly through the real pipeline. Repeated
    /// polling is needed to discover the corrected file.
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
                poll_interval: Duration::from_millis(30),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "eod_risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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

    /// A clean publish cannot clear discovery's PendingTooLong for a stuck file.
    /// Only discovery observing that file resolve or disappear can clear its lane.
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
                poll_interval: Duration::from_millis(50),
                pending_timeout: Duration::ZERO,
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "eod_risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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

        // Wait for publication before observing several further polls. Separate the
        // I/O completion timeout from the observation window so slow setup cannot
        // shorten the check for an incorrect Health::Ok.
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

    fn notes(file: &str, extra: &[&str], missing: &[&str]) -> crate::ingest::load::LoadNotes {
        crate::ingest::load::LoadNotes {
            file: file.into(),
            extra_columns: extra.iter().map(|s| s.to_string()).collect(),
            missing_optional: missing.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_load_note_names_the_file_dataset_and_both_lists() {
        let mut log = LoadNoteLog::default();
        let d = log
            .note(
                "eod_risk",
                "risk_snapshot",
                &notes("risk_a.csv", &["A", "B"], &["skew01"]),
            )
            .unwrap();
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(
            d.message,
            "'risk_a.csv' loaded into 'risk_snapshot' with extra columns [A, B] ignored; \
             optional [skew01] missing, read as NULL"
        );
        let only_missing = log
            .note(
                "eod_risk",
                "risk_snapshot",
                &notes("risk_b.csv", &[], &["skew01"]),
            )
            .unwrap();
        assert_eq!(
            only_missing.message,
            "'risk_b.csv' loaded into 'risk_snapshot' with optional [skew01] missing, read as NULL"
        );
    }

    #[test]
    fn a_repeated_load_note_combination_warns_once() {
        let mut log = LoadNoteLog::default();
        assert!(
            log.note("s", "d", &notes("a.csv", &["X", "Y"], &["z"]))
                .is_some()
        );
        // Another file, same combination (column order aside): nothing.
        assert!(
            log.note("s", "d", &notes("b.csv", &["Y", "X"], &["z"]))
                .is_none()
        );
        // A different combination, or another source, warns again.
        assert!(
            log.note("s", "d", &notes("c.csv", &["X"], &["z"]))
                .is_some()
        );
        assert!(
            log.note("t", "d", &notes("d.csv", &["X", "Y"], &["z"]))
                .is_some()
        );
    }

    #[test]
    fn load_notes_stop_growing_at_the_cap_with_one_last_warning() {
        let mut log = LoadNoteLog::default();
        for i in 0..LOAD_NOTE_CAP {
            assert!(
                log.note("s", "d", &notes("f.csv", &[&format!("C{i}")], &[]))
                    .is_some()
            );
        }
        let last = log
            .note("t", "d", &notes("g.csv", &["one more"], &[]))
            .expect("the cap is announced once");
        assert!(last.message.contains("further"), "{}", last.message);
        assert!(
            last.message.contains("source 't'") && last.message.contains("'g.csv'"),
            "the cap warning names the source and file that hit it: {}",
            last.message
        );
        assert!(
            log.note("s", "d", &notes("f.csv", &["and another"], &[]))
                .is_none()
        );
        assert_eq!(
            log.seen.len(),
            LOAD_NOTE_CAP,
            "nothing remembered past the cap"
        );
    }

    /// Real files through a directory source: a file with undeclared columns
    /// and a missing optional column warns once; an identical second file
    /// warns nothing; a file with a different combination warns again.
    #[test]
    fn extra_and_missing_optional_columns_are_one_warning_per_combination() {
        let (_db, src, _store, _ds, emitted) = crate::ingest::load::tests_support::fixture();
        let ready = |has_skew: bool| {
            emitted
                .files
                .iter()
                .find(|f| {
                    f.sentinel_path.is_some() && f.columns.iter().any(|c| c == "Skew01") == has_skew
                })
                .expect("the generator emits both shapes")
        };
        let drops = tempfile::tempdir().unwrap();
        let (_sdb, svc, rx) = directory_service(format!("{}/*.csv", drops.path().display()));
        let land = |from: &geode_demo_data::EmittedFile, batch: &str| -> Vec<String> {
            let csv = drops.path().join(format!("risk_2026-08-24_{batch}.csv"));
            std::fs::write(&csv, std::fs::read(&from.csv_path).unwrap()).unwrap();
            let done = drops
                .path()
                .join(format!("risk_2026-08-24_{batch}.csv.done"));
            std::fs::write(
                &done,
                std::fs::read(from.sentinel_path.as_ref().unwrap()).unwrap(),
            )
            .unwrap();
            let mut warnings = Vec::new();
            let mut published = false;
            loop {
                match rx.recv_timeout(Duration::from_secs(60)).expect("an event") {
                    DataEvent::Published { batch: b, .. } if b == batch => published = true,
                    DataEvent::Diagnostics(d) => warnings.extend(
                        d.into_iter()
                            .filter(|d| d.severity == Severity::Warning)
                            .map(|d| d.message),
                    ),
                    DataEvent::LoadEnded if published => return warnings,
                    _ => {}
                }
            }
        };
        let first = land(ready(false), "a1");
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(
            first[0].starts_with(
                "'risk_2026-08-24_a1.csv' loaded into 'risk_snapshot' with extra columns ["
            ) && first[0].ends_with("; optional [skew01] missing, read as NULL"),
            "{first:?}"
        );
        assert_eq!(
            land(ready(false), "a2"),
            Vec::<String>::new(),
            "the same combination again"
        );
        let other = land(ready(true), "b1");
        assert_eq!(other.len(), 1, "{other:?}");
        assert!(!other[0].contains("optional ["), "{other:?}");
        drop(src);
        svc.shutdown();
    }

    /// A service with one directory source, `eod_risk` over `risk_snapshot`,
    /// polling `pattern` every 50 ms.
    fn directory_service(
        pattern: String,
    ) -> (
        tempfile::TempDir,
        DataService,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        let db = tempfile::tempdir().unwrap();
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
                poll_interval: Duration::from_millis(50),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory("eod_risk", "risk_snapshot", vec![pattern])
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            positions: None,
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
        })
        .unwrap();
        (db, svc, rx)
    }

    /// A drop directory that is not there is a degraded source, named by its
    /// path, not a green one with no rows; it clears once the directory exists.
    #[test]
    fn a_missing_source_directory_is_degraded_and_clears_when_it_appears() {
        let root = tempfile::tempdir().unwrap();
        let drops = root.path().join("drops");
        let (_db, svc, rx) = directory_service(format!("{}/*.csv", drops.display()));
        let reason = until_within(&rx, Duration::from_secs(10), |e| match e {
            DataEvent::Health {
                source,
                worst: Health::Degraded { reason },
                ..
            } => {
                assert_eq!(source, "eod_risk");
                Some(reason)
            }
            _ => None,
        });
        assert_eq!(
            reason,
            Some(format!("path '{}' not found", drops.display())),
            "a missing drop directory must degrade its source"
        );
        std::fs::create_dir(&drops).unwrap();
        let cleared = until_within(&rx, Duration::from_secs(10), |e| match e {
            DataEvent::Health {
                source,
                worst: Health::Ok,
                ..
            } if source == "eod_risk" => Some(()),
            _ => None,
        });
        assert_eq!(cleared, Some(()), "the directory appearing must clear it");
        svc.shutdown();
    }

    #[test]
    fn an_invalid_source_pattern_is_degraded_not_skipped() {
        let root = tempfile::tempdir().unwrap();
        let pattern = format!("{}/[.csv", root.path().display());
        let (_db, svc, rx) = directory_service(pattern.clone());
        let reason = until_within(&rx, Duration::from_secs(10), |e| match e {
            DataEvent::Health {
                source,
                worst: Health::Degraded { reason },
                ..
            } if source == "eod_risk" => Some(reason),
            _ => None,
        });
        assert!(
            reason
                .as_deref()
                .is_some_and(|r| r.starts_with(&format!("invalid pattern '{pattern}': "))),
            "{reason:?}"
        );
        svc.shutdown();
    }

    /// Clean polls and a clean publication for one source must combine into one
    /// Ok transition at the service boundary.
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
                poll_interval: Duration::from_millis(30),
                pending_timeout: Duration::from_secs(3600),
                batch_pattern: Some(r"^risk_\d{4}-\d{2}-\d{2}_(?<batch>.+)$".into()),
                ..crate::source::SourceSpec::directory(
                    "eod_risk",
                    "risk_snapshot",
                    vec![format!("{}/*.csv", src.path().display())],
                )
            }],
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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
            adapters: Default::default(),
            documents: Default::default(),
            egress: Vec::new(),
            clock: geode_core::clock::Clock::utc(),
            pricer: PricerConfig::default(),
            vol: crate::vol::VolConfig::default(),
            positions: None,
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

    // Verify health log levels through a scoped ring subscriber.

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

    /// A reference database that stays down fails every poll. The source's
    /// discovery lane degrades once, and that transition is the one log
    /// line: a warning naming the source and the reason, not an error per
    /// poll.
    #[test]
    fn a_snapshot_source_that_keeps_failing_logs_one_warning_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let (ingest, _rx) = IngestRunner::spawn_channel(store, SchemaSpec::default());
        let ingest = Arc::new(ingest);
        let sink: EventSink = Arc::new(|_| true);
        let health_tracker = Arc::new(HealthTracker::default());
        let spec = crate::source::SourceSpec {
            adapter: "fake".to_string(),
            table: Some("t".to_string()),
            ..crate::source::SourceSpec::directory("refdb", "u", Vec::new())
        };
        let outcome = snapshot_outcome_sink(&spec, &ingest, &sink, &health_tracker);
        let records = logged(|| {
            for _ in 0..3 {
                outcome(SnapshotOutcome::Failed {
                    reason: "db down".into(),
                });
            }
        });
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].level, tracing::Level::WARN);
        assert_eq!(records[0].target, "geode::ingest");
        assert!(
            records[0].message.contains("refdb"),
            "{}",
            records[0].message
        );
        assert!(
            records[0].message.contains("db down"),
            "{}",
            records[0].message
        );
        ingest.shutdown();
    }

    /// A series dataset whose coverage row holds a timestamp past chrono's
    /// range: `from_micros` panics reading it, in the catalog and in a fetch.
    fn out_of_range_coverage_service(
        fail_once: bool,
    ) -> (
        tempfile::TempDir,
        crate::handle::DataHandle,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        const PAST_CHRONO: i64 = 9_000_000_000_000_000_000;
        assert!(chrono::DateTime::<Utc>::from_timestamp_micros(PAST_CHRONO).is_none());
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = crate::store::ddl::tests_support::series_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        store
            .writer()
            .execute_batch(&format!(
                "insert into series_series_coverage values \
                 ('kdb_hist', 'SPX', make_timestamp({PAST_CHRONO}::BIGINT), \
                  make_timestamp({PAST_CHRONO}::BIGINT), now()::timestamp);"
            ))
            .unwrap();
        drop(store);
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds);
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(FakeFetchAdapter {
            calls: Default::default(),
            catalogue: None,
            fail_once,
        }));
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn(
            DataServiceConfig {
                db_path: dir.path().join("geode.duckdb"),
                schema,
                views: Vec::new(),
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: vec![crate::source::SourceSpec {
                    adapter: "fake_kdb".to_string(),
                    ..crate::source::SourceSpec::directory("kdb_hist", "series", Vec::new())
                }],
                adapters,
                documents: Default::default(),
                egress: Vec::new(),
                clock: geode_core::clock::Clock::utc(),
                pricer: PricerConfig::default(),
                vol: crate::vol::VolConfig::default(),
                positions: None,
            },
            sink,
        );
        (dir, handle, rx)
    }

    /// An unknown view answers at once: proof the loop is still serving.
    fn still_serves(handle: &crate::handle::DataHandle, rx: &std::sync::mpsc::Receiver<DataEvent>) {
        handle
            .query(params(4242, "nonesuch", &Scope::default(), AsOf::Live, 1))
            .unwrap();
        until(rx, |e| match e {
            DataEvent::Query(o) if o.key == QueryKey(4242) => Some(()),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
    }

    #[test]
    fn a_catalog_over_an_out_of_range_timestamp_answers_err_and_the_loop_serves_on() {
        let (_d, handle, rx) = out_of_range_coverage_service(false);
        handle
            .catalog(CatalogParams {
                key: QueryKey(3),
                tag: 9,
                as_of: AsOf::Live,
            })
            .unwrap();
        let answer = until(&rx, |e| match e {
            DataEvent::Catalog(o) if o.tag == 9 => Some(o),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
        let reason = answer
            .snapshot
            .expect_err("a panicking catalog read is an error");
        assert!(reason.contains("catalog request panicked"), "{reason}");
        assert!(
            reason.contains("a stored timestamp is in range"),
            "{reason}"
        );
        still_serves(&handle, &rx);
        handle.shutdown();
    }

    #[test]
    fn a_fetch_over_an_out_of_range_timestamp_fails_the_pair_and_the_loop_serves_on() {
        let (_d, handle, rx) = out_of_range_coverage_service(false);
        let now = Utc::now();
        handle
            .fetch(FetchParams {
                key: QueryKey(3),
                source: "kdb_hist".into(),
                identity: "SPX".into(),
                from: now - chrono::Duration::days(1),
                to: now,
            })
            .unwrap();
        let health = until(&rx, |e| match e {
            DataEvent::Health {
                source,
                worst: Health::Failed { reason },
                detail,
            } if source == "kdb_hist" => Some((reason, detail)),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
        assert!(health.0.contains("fetch request panicked"), "{health:?}");
        assert!(health.1.starts_with("SPX@kdb_hist"), "{health:?}");
        let fetched = until(&rx, |e| match e {
            DataEvent::SeriesFetched {
                source,
                identity,
                result,
            } if source == "kdb_hist" && identity == "SPX" => Some(result),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
        assert!(
            fetched
                .as_ref()
                .is_err_and(|r| r.contains("a stored timestamp is in range")),
            "{fetched:?}"
        );
        still_serves(&handle, &rx);
        handle.shutdown();
    }

    /// The failure lands on the source's own lanes, the ones the fetch
    /// worker and the runner report on: another pair failing and then
    /// recovering must leave the source failed on the pair the loop could
    /// not fetch, not report it clean.
    #[test]
    fn a_fetch_the_loop_could_not_run_keeps_its_source_failed_past_another_pairs_recovery() {
        let (_d, handle, rx) = out_of_range_coverage_service(true);
        let now = Utc::now();
        let fetch = |identity: &str| {
            handle
                .fetch(FetchParams {
                    key: QueryKey(3),
                    source: "kdb_hist".into(),
                    identity: identity.into(),
                    from: now - chrono::Duration::days(1),
                    to: now,
                })
                .unwrap()
        };
        let answered = |identity: &str| {
            until(&rx, |e| match e {
                DataEvent::SeriesFetched {
                    source,
                    identity: i,
                    result,
                } if source == "kdb_hist" && i == identity => Some(result),
                DataEvent::ThreadStopped { thread, reason } => {
                    panic!("{thread} stopped: {reason}")
                }
                _ => None,
            })
        };
        fetch("SPX");
        assert!(answered("SPX").is_err());
        fetch("broken");
        assert!(answered("broken").is_err());
        // "broken" recovers on its second ask.
        fetch("broken");
        let mut last_health = None;
        let recovered = until(&rx, |e| match e {
            DataEvent::Health {
                source,
                worst,
                detail,
            } if source == "kdb_hist" => {
                last_health = Some((worst, detail));
                None
            }
            DataEvent::SeriesFetched {
                source,
                identity,
                result,
            } if source == "kdb_hist" && identity == "broken" => Some(result),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
        assert!(recovered.is_ok(), "{recovered:?}");
        let (worst, detail) = last_health.expect("the recovery changed the source's health");
        assert!(
            matches!(&worst, Health::Failed { reason } if reason.contains("fetch request panicked")),
            "{worst:?}"
        );
        assert!(detail.starts_with("SPX@kdb_hist"), "{detail}");
        handle.shutdown();
    }

    type SnapshotAnswers = Arc<
        std::sync::Mutex<
            std::collections::VecDeque<
                Result<geode_core::reference::TableRows, crate::adapter::AdapterError>,
            >,
        >,
    >;

    /// A snapshot adapter named `fake`: each query takes the next queued
    /// answer, or repeats the last one when the queue is empty. `None`
    /// answers model an adapter with no snapshot side.
    struct FakeSnapshotAdapter {
        answers: Option<SnapshotAnswers>,
    }

    struct FakeSnapshot {
        answers: SnapshotAnswers,
        last: Option<Result<geode_core::reference::TableRows, crate::adapter::AdapterError>>,
    }

    impl crate::adapter::SnapshotQuery for FakeSnapshot {
        fn query(
            &mut self,
            table: &str,
        ) -> Result<geode_core::reference::TableRows, crate::adapter::AdapterError> {
            assert_eq!(table, "t", "the configured table is the one read");
            if let Some(next) = self.answers.lock().unwrap().pop_front() {
                self.last = Some(next);
            }
            self.last.clone().expect("a queued first answer")
        }
    }

    impl crate::adapter::Adapter for FakeSnapshotAdapter {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn subscription(&self) -> Option<Box<dyn crate::adapter::Subscription>> {
            None
        }
        fn egress(&self) -> Option<Box<dyn crate::adapter::Egress>> {
            None
        }
        fn snapshot(&self) -> Option<Box<dyn crate::adapter::SnapshotQuery>> {
            let answers = self.answers.clone()?;
            Some(Box::new(FakeSnapshot {
                answers,
                last: None,
            }))
        }
    }

    /// `u` rows of `(underlying_ref, currency, multiplier)`.
    fn ref_rows(rows: &[(&str, &str, f64)]) -> geode_core::reference::TableRows {
        use geode_core::reference::RefColumn;
        geode_core::reference::TableRows {
            columns: vec![
                (
                    "underlying_ref".into(),
                    RefColumn::Utf8(rows.iter().map(|r| Some(r.0.to_string())).collect()),
                ),
                (
                    "currency".into(),
                    RefColumn::Utf8(rows.iter().map(|r| Some(r.1.to_string())).collect()),
                ),
                (
                    "multiplier".into(),
                    RefColumn::F64(rows.iter().map(|r| Some(r.2)).collect()),
                ),
            ],
        }
    }

    /// The underlying refs a reference read answered, in order.
    fn refs_of(table: &geode_core::query::ReferenceTable) -> Vec<String> {
        table
            .rows
            .iter()
            .map(|r| r[0].clone().expect("a key is never NULL"))
            .collect()
    }

    const ROWS_A: &[(&str, &str, f64)] = &[("NDX", "USD", 20.0), ("SPX", "USD", 100.0)];
    const ROWS_B: &[(&str, &str, f64)] = &[("SPX", "USD", 50.0), ("SX5E", "EUR", 10.0)];

    /// A service, through its request loop, with one snapshot source
    /// `refdb` reading table `t` into the reference dataset `u` every hour:
    /// after the poll at start, only `DataHandle::poll` polls it.
    fn snapshot_service(
        answers: Option<SnapshotAnswers>,
    ) -> (
        tempfile::TempDir,
        crate::handle::DataHandle,
        std::sync::mpsc::Receiver<DataEvent>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut adapters = AdapterRegistry::default();
        adapters.register(Arc::new(FakeSnapshotAdapter { answers }));
        let mut schema = SchemaSpec::default();
        schema
            .datasets
            .push(geode_core::reference::test_support::reference_dataset());
        let spec = crate::source::SourceSpec {
            adapter: "fake".to_string(),
            table: Some("t".to_string()),
            poll_interval: Duration::from_secs(3600),
            ..crate::source::SourceSpec::directory("refdb", "u", Vec::new())
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let sink: EventSink = Arc::new(move |e| tx.send(e).is_ok());
        let handle = DataService::spawn(
            DataServiceConfig {
                db_path: dir.path().join("geode.duckdb"),
                schema,
                views: Vec::new(),
                dimensions: DerivedDimensions::default(),
                query_workers: 1,
                sources: vec![spec],
                adapters,
                documents: Default::default(),
                egress: Vec::new(),
                clock: geode_core::clock::Clock::utc(),
                pricer: PricerConfig::default(),
                vol: crate::vol::VolConfig::default(),
                positions: None,
            },
            sink,
        );
        (dir, handle, rx)
    }

    fn answers(first: &[(&str, &str, f64)]) -> SnapshotAnswers {
        Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from([
            Ok(ref_rows(first)),
        ])))
    }

    /// The next `Published` of `u`, as its generation.
    fn next_reference_publish(rx: &std::sync::mpsc::Receiver<DataEvent>) -> i64 {
        until(rx, |e| match e {
            DataEvent::Published {
                dataset,
                batch,
                gen_id,
                books,
            } if dataset == "u" => {
                assert_eq!(batch, "u", "a reference dataset is one batch, its name");
                assert_eq!(books, vec![None], "and one bookless partition");
                Some(gen_id)
            }
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        })
    }

    /// Ask for `dataset` as of `as_of` through the handle and wait for its
    /// answer.
    fn read_reference(
        handle: &crate::handle::DataHandle,
        rx: &std::sync::mpsc::Receiver<DataEvent>,
        dataset: &str,
        as_of: AsOf,
        tag: u64,
    ) -> Result<Option<geode_core::query::ReferenceTable>, String> {
        handle
            .reference(geode_core::query::ReferenceParams {
                key: QueryKey(77),
                tag,
                dataset: dataset.into(),
                as_of: as_of.clone(),
            })
            .unwrap();
        until(rx, |e| match e {
            DataEvent::Reference(o) if o.tag == tag => {
                assert_eq!(o.key, QueryKey(77));
                assert_eq!(o.dataset, dataset);
                assert_eq!(o.as_of, as_of);
                Some(o.table)
            }
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        })
    }

    #[test]
    fn a_snapshot_source_publishes_then_stays_quiet_until_the_table_changes() {
        let queued = answers(ROWS_A);
        let (_d, handle, rx) = snapshot_service(Some(queued.clone()));
        let mut polled = None;
        let first = until(&rx, |e| match e {
            DataEvent::Polled {
                source, at, next, ..
            } if source == "refdb" => {
                polled = Some((at, next));
                None
            }
            DataEvent::Published {
                dataset, gen_id, ..
            } if dataset == "u" => Some(gen_id),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
        let (at, next) = polled.unwrap_or_else(|| {
            until(&rx, |e| match e {
                DataEvent::Polled {
                    source, at, next, ..
                } if source == "refdb" => Some((at, next)),
                _ => None,
            })
        });
        assert_eq!(
            next.duration_since(at).unwrap(),
            Duration::from_secs(3600),
            "the Sources section's next poll is one interval on"
        );

        // The same rows again: the runner is handed them, publishes
        // nothing, and the source stays clean. A poll starts no load
        // progress, so the status bar's strip does not flash every
        // interval.
        handle.poll("u".into()).unwrap();
        let deadline = Instant::now() + Duration::from_millis(500);
        while let Ok(e) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            match e {
                DataEvent::Loading { source, .. } if source == "refdb" => {
                    panic!("an unchanged poll started load progress")
                }
                DataEvent::Published { dataset, .. } if dataset == "u" => {
                    panic!("an unchanged snapshot published a generation")
                }
                DataEvent::Health { source, worst, .. } if source == "refdb" => {
                    assert_eq!(worst, Health::Ok, "an unchanged snapshot is clean")
                }
                DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
                _ => {}
            }
        }

        // The runner takes snapshots in order, so this publish proves the
        // unchanged one above reached it; a changed table starts no
        // progress either.
        queued.lock().unwrap().push_back(Ok(ref_rows(ROWS_B)));
        handle.poll("u".into()).unwrap();
        let second = until(&rx, |e| match e {
            DataEvent::Loading { source, .. } if source == "refdb" => {
                panic!("a snapshot publish started load progress")
            }
            DataEvent::Published {
                dataset, gen_id, ..
            } if dataset == "u" => Some(gen_id),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
        assert!(second > first, "{second} after {first}");
        handle.shutdown();
    }

    #[test]
    fn a_failed_snapshot_degrades_the_source_and_keeps_live_rows() {
        let queued = answers(ROWS_A);
        let (_d, handle, rx) = snapshot_service(Some(queued.clone()));
        next_reference_publish(&rx);

        queued
            .lock()
            .unwrap()
            .push_back(Err(crate::adapter::AdapterError {
                message: "db down".into(),
            }));
        handle.poll("u".into()).unwrap();
        let (worst, detail) = until(&rx, |e| match e {
            DataEvent::Health {
                source,
                worst: worst @ Health::Degraded { .. },
                detail,
            } if source == "refdb" => Some((worst, detail)),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
        assert!(
            matches!(&worst, Health::Degraded { reason } if reason.contains("db down")),
            "{worst:?}"
        );
        assert!(detail.contains("db down"), "{detail}");

        let live = read_reference(&handle, &rx, "u", AsOf::Live, 1)
            .unwrap()
            .expect("the good rows stay live");
        assert_eq!(refs_of(&live), ["NDX", "SPX"]);

        // Good again, and equal to live: the discovery lane clears and no
        // generation is spent.
        queued.lock().unwrap().push_back(Ok(ref_rows(ROWS_A)));
        handle.poll("u".into()).unwrap();
        until(&rx, |e| match e {
            DataEvent::Health { source, worst, .. } if source == "refdb" => {
                (worst == Health::Ok).then_some(())
            }
            DataEvent::Published { dataset, .. } if dataset == "u" => {
                panic!("an unchanged snapshot published a generation")
            }
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
        assert!(
            until_within(&rx, Duration::from_millis(500), |e| match e {
                DataEvent::Published { dataset, .. } if dataset == "u" => Some(()),
                _ => None,
            })
            .is_none(),
            "an unchanged snapshot published a generation"
        );
        handle.shutdown();
    }

    #[test]
    fn reference_answers_live_and_as_of() {
        let queued = answers(ROWS_A);
        let (_d, handle, rx) = snapshot_service(Some(queued.clone()));
        next_reference_publish(&rx);
        let a = read_reference(&handle, &rx, "u", AsOf::Live, 1)
            .unwrap()
            .expect("A is live");
        assert_eq!(refs_of(&a), ["NDX", "SPX"]);
        assert_eq!(a.columns, ["underlying_ref", "currency", "multiplier"]);

        queued.lock().unwrap().push_back(Ok(ref_rows(ROWS_B)));
        handle.poll("u".into()).unwrap();
        next_reference_publish(&rx);
        let b = read_reference(&handle, &rx, "u", AsOf::Live, 2)
            .unwrap()
            .expect("B is live");
        assert_eq!(refs_of(&b), ["SPX", "SX5E"]);
        assert!(b.source_time > a.source_time);
        assert!(b.gen_id > a.gen_id);

        let between = a.source_time + (b.source_time - a.source_time) / 2;
        let then = read_reference(&handle, &rx, "u", AsOf::At(between), 3)
            .unwrap()
            .expect("A as of between the two");
        assert_eq!(then, a, "as of between them, A whole");

        let before = a.source_time - chrono::Duration::seconds(1);
        assert_eq!(
            read_reference(&handle, &rx, "u", AsOf::At(before), 4),
            Ok(None),
            "nothing was published before the first snapshot"
        );
        handle.shutdown();
    }

    #[test]
    fn an_adapter_without_a_snapshot_side_is_unservable() {
        let (_d, handle, rx) = snapshot_service(None);
        let (source, worst, _) = next_health(&rx);
        assert_eq!(source, "refdb");
        assert!(
            matches!(&worst, Health::Failed { reason }
                if reason == "adapter 'fake' has no snapshot side"),
            "{worst:?}"
        );
        handle.shutdown();
    }

    #[test]
    fn reference_for_an_undeclared_dataset_answers_an_error() {
        let (_d, handle, rx) = snapshot_service(Some(answers(ROWS_A)));
        let answer = read_reference(&handle, &rx, "nope", AsOf::Live, 1);
        assert!(
            answer.as_ref().is_err_and(|e| e.contains("nope")),
            "{answer:?}"
        );
        handle.shutdown();
    }

    #[test]
    fn a_snapshot_with_an_extra_column_publishes_and_warns_against_its_source() {
        let mut rows = ref_rows(ROWS_A);
        rows.columns.push((
            "isin".into(),
            geode_core::reference::RefColumn::Utf8(vec![None, None]),
        ));
        let queued = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from([
            Ok(rows),
        ])));
        let (_d, handle, rx) = snapshot_service(Some(queued));
        let warning = until(&rx, |e| match e {
            DataEvent::Diagnostics(d) => d
                .into_iter()
                .find(|d| d.severity == Severity::Warning && d.message.contains("isin")),
            DataEvent::ThreadStopped { thread, reason } => panic!("{thread} stopped: {reason}"),
            _ => None,
        });
        assert_eq!(warning.path.as_deref(), Some("sources.refdb"));
        next_reference_publish(&rx);
        handle.shutdown();
    }

    #[test]
    fn a_poll_for_a_dataset_no_snapshot_source_fills_is_harmless() {
        let (_d, handle, rx) = snapshot_service(Some(answers(ROWS_A)));
        next_reference_publish(&rx);
        handle.poll("nope".into()).unwrap();
        assert!(
            read_reference(&handle, &rx, "u", AsOf::Live, 1)
                .unwrap()
                .is_some(),
            "the loop serves on"
        );
        handle.shutdown();
    }
}
