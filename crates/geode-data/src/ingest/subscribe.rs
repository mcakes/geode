//! The receiver pipeline for a subscribed source (market-data spec §5.4):
//! one thread per source, between the adapter tier and the ingest runner.
//!
//! Its whole shape follows from where it sits. Upstream is a bus whose
//! producer must never be blocked ([`MessageSink::push`] is a `try_send`
//! and a counter), downstream is a writer thread that publishes one
//! generation at a time, and the two run at completely different speeds: a
//! market-data feed republishes a key every few milliseconds, while a
//! publish is a transaction. So this thread is where the rate is reconciled
//! — parse, validate, stamp, then hand to [`Coalescer`], which keeps at
//! most ONE pending document per key and releases it no more often than the
//! source's `coalesce` window. Nothing is queued to be caught up on later:
//! market data is a stream of latest-value snapshots, so the newest
//! document for a key is the only one worth publishing.
//!
//! **The timer is here and nowhere else.** `Coalescer` is pure — every
//! method takes `now` — so this loop's `recv_timeout` is the only clock on
//! the path, and its wait is `min(next release deadline, MAX_WAIT)`: a held
//! document is released within a millisecond or two of its deadline even
//! when the feed has gone quiet, and a thread with nothing pending still
//! wakes four times a second to notice the stop flag.
//!
//! **Failures split by lane, and this module takes no position on either.**
//! Two closures are passed in: `on_parse_failure` for a document this
//! source sent that could not be published (the LOAD lane, keyed by batch —
//! by the topic when the bytes never yielded a key), and the adapter's own
//! `HealthSink` for connection state (the DISCOVERY lane). Both are built
//! by `DataService::open`, because the health-lane rules are the service's
//! (CLAUDE.md, Phase 4b: a clean discovery report must never clear a
//! load-lane problem) and a receiver thread that reported health itself
//! would be a second place those rules live.
//!
//! Nothing here allocates per row: the parse allocates the columns and
//! those exact columns are moved into the [`DocumentJob`] the runner
//! publishes (PHILOSOPHY §6). What is allocated per message is one key
//! `String` (the coalescer's map key, which is also the `batch` the publish
//! records) and, first time only, one `String` per unrecognised element
//! path.

use crate::adapter::{AdapterError, HealthSink, MESSAGE_BOUND, Message, MessageSink, Subscription};
use crate::ingest::coalesce::Coalescer;
use crate::ingest::runner::{DocumentJob, IngestHandle, panic_payload_message};
use chrono::{DateTime, NaiveTime, Utc};
use geode_core::document::{DocumentKind, DocumentRows, Value, join_key};
use geode_core::schema::{ColumnType, DatasetSpec};
use geode_core::source_config::{SourceSpec, SourceTime};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The longest one `recv_timeout` will wait when the coalescer has nothing
/// due sooner.
///
/// It is a stop-flag granularity, not a data latency: a message wakes the
/// thread immediately and a held document wakes it on its own deadline, so
/// the only thing this bounds is how long `shutdown` could wait if the
/// channel did not disconnect first (it does — `shutdown` unsubscribes
/// before it joins, which drops the adapter's sink clone). A quarter of a
/// second was chosen to cost nothing per idle source while keeping that
/// backstop short.
const MAX_WAIT: Duration = Duration::from_millis(250);

/// What a receiver thread does with a document it could not publish: the
/// batch it is filed under (the document's joined key, or the topic when
/// the bytes never yielded one) and the reason.
///
/// A closure rather than a channel because the service already owns the
/// one door these have to go through — `HealthTracker`'s load lane — and
/// the shape of a `DataEvent` is not this module's business. It must not
/// block: it is called on the receiver thread, between two messages.
pub type ParseFailureSink = Arc<dyn Fn(&str, String) + Send + Sync>;

/// One parsed document waiting for its release window, as the coalescer
/// holds it.
///
/// `source_time` is resolved when the message arrives rather than when the
/// document is released, for two reasons: its failure is a report about
/// THIS message (`on_parse_failure` names the key), and resolving it late
/// would mean either doing the work twice or having a fallible step on the
/// release path with nowhere sensible to report from.
struct Pending {
    rows: DocumentRows,
    received: DateTime<Utc>,
    source_time: DateTime<Utc>,
    bytes: u64,
}

/// One subscribed source's live subscription and the thread draining it.
///
/// The subscription is held HERE rather than moved onto the receiver
/// thread, so that `shutdown` can stop delivery before it stops reading:
/// unsubscribing first means the dispatcher is no longer cloning messages
/// into a queue nobody will drain, and it also disconnects the channel,
/// which is what makes the join immediate instead of up to [`MAX_WAIT`].
/// One thread ever touches it either way — `&mut self` on both methods is
/// the whole of the discipline [`Subscription`] asks for.
pub struct SubscriptionWorker {
    subscription: Box<dyn Subscription>,
    stop: Arc<AtomicBool>,
    /// A clone of the sink handed to the adapter, kept for one reason:
    /// [`MessageSink::refused`] is the only record of a message this
    /// source DROPPED, and the counter lives on the sink. Moving the sole
    /// copy into `subscribe` made a refusal — a receiver that fell behind
    /// its feed, which is data silently missing from every query — a
    /// number nothing in the process could read. Clones share the counter,
    /// so this one reads the subscription's own total (fix round 1).
    sink: MessageSink,
    /// `None` once joined, so `shutdown` is idempotent and `Drop` can call
    /// it again with nothing to do.
    thread: Option<JoinHandle<()>>,
}

impl SubscriptionWorker {
    /// Subscribes and starts the receiver thread.
    ///
    /// `subscribe` runs on the CALLER's thread, before the thread is
    /// spawned, so its `Err` is this function's `Err` — a source whose
    /// adapter refuses (a closed bus, no topics) is reported as unservable
    /// at open time rather than by a thread that quietly ends. It is also
    /// what reports the adapter's current connection state through
    /// `on_connection`, so a caller never polls for "am I connected".
    ///
    /// `dataset` is taken by value: the receiver validates every document
    /// against it on its own thread, and a `DatasetSpec` is cloned once
    /// per source here rather than shared behind a lock that a validate
    /// would then take per message.
    pub fn spawn(
        spec: &SourceSpec,
        dataset: DatasetSpec,
        kind: Arc<dyn DocumentKind>,
        mut subscription: Box<dyn Subscription>,
        ingest: Arc<IngestHandle>,
        on_parse_failure: ParseFailureSink,
        on_connection: HealthSink,
    ) -> Result<SubscriptionWorker, AdapterError> {
        let (sink, rx) = MessageSink::bounded(MESSAGE_BOUND);
        subscription.subscribe(&spec.topics, sink.clone(), on_connection)?;
        let stop = Arc::new(AtomicBool::new(false));
        let mut receiving = Receiving {
            source: spec.name.clone(),
            dataset,
            kind,
            policy: spec.source_time.clone(),
            ingest,
            on_parse_failure,
            stop: Arc::clone(&stop),
            unknown: HashSet::new(),
        };
        let window = spec.coalesce;
        let thread = std::thread::Builder::new()
            .name(format!("geode-subscribe-{}", spec.name))
            .spawn(move || receiving.run(rx, window));
        match thread {
            Ok(thread) => Ok(SubscriptionWorker {
                subscription,
                stop,
                sink,
                thread: Some(thread),
            }),
            Err(e) => {
                // Nothing may stay registered behind a failed spawn: the
                // dispatcher would clone every matching message into a
                // queue with no reader for the life of the process.
                subscription.unsubscribe();
                Err(AdapterError {
                    message: format!(
                        "source '{}': could not start the receiver thread: {e}",
                        spec.name
                    ),
                })
            }
        }
    }

    /// How many messages this source's feed sent that the receiver could
    /// not take, over the subscription's whole life.
    ///
    /// Non-zero means data was DROPPED: the receiver fell far enough
    /// behind that the bounded queue in front of it overflowed, and the
    /// dropped snapshots are simply gone (the coalescer's rule — newest
    /// per key wins — makes that survivable, not invisible). Exposed
    /// rather than merely counted because a source whose numbers are
    /// stale for this reason looks identical to a healthy one from a
    /// query, and the count is the only way to tell the two apart.
    pub fn refused(&self) -> u64 {
        self.sink.refused()
    }

    /// Stops delivery, then the thread. Idempotent.
    ///
    /// Order matters: `unsubscribe` first so no further message is queued
    /// and the channel disconnects (which the loop treats as a stop),
    /// then the flag for the case where an adapter's unsubscribe leaves
    /// the sink alive, then the join. A document the coalescer is still
    /// holding is deliberately dropped rather than flushed — it is one
    /// superseded snapshot per key, and the runner it would be submitted
    /// to is being shut down in the same breath.
    pub fn shutdown(&mut self) {
        self.subscription.unsubscribe();
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SubscriptionWorker {
    /// A dropped worker is a stopped one — the same rule
    /// `ChannelSubscription` and `IngestHandle` follow, and the reason a
    /// `DataService` dropped without `shutdown` still leaves no receiver
    /// thread behind.
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Everything the receiver thread owns. A struct rather than eight
/// parameters threaded through three functions.
struct Receiving {
    source: String,
    dataset: DatasetSpec,
    kind: Arc<dyn DocumentKind>,
    policy: SourceTime,
    ingest: Arc<IngestHandle>,
    on_parse_failure: ParseFailureSink,
    stop: Arc<AtomicBool>,
    /// Element paths this source's parser has already complained about.
    /// One set for the life of the thread, so a feed sending one stray
    /// element on every message logs one line, not one per message
    /// (spec §6.3).
    unknown: HashSet<String>,
}

impl Receiving {
    /// The thread's whole life.
    fn run(&mut self, rx: Receiver<Message>, window: Duration) {
        let mut coalescer: Coalescer<Pending> = Coalescer::new(window);
        while !self.stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            let wait = coalescer.next_deadline().map_or(MAX_WAIT, |due| {
                due.saturating_duration_since(now).min(MAX_WAIT)
            });
            match rx.recv_timeout(wait) {
                Ok(message) => self.handle_message(&message, &mut coalescer),
                Err(RecvTimeoutError::Timeout) => {}
                // Every clone of our sink is gone, so no message can ever
                // arrive again: the subscription was dropped or
                // unsubscribed (`shutdown`, or an adapter that ended it).
                // Nothing to wait for.
                Err(RecvTimeoutError::Disconnected) => return,
            }
            // After a message AND after a timeout: an offer that was held
            // back may be due by now, and a quiet feed is exactly when a
            // release must not wait for the next message to trigger it.
            for (_key, pending) in coalescer.due(Instant::now()) {
                self.submit(pending);
            }
        }
    }

    /// One message, inside a panic boundary — the same one every other
    /// background boundary in this crate uses (spec §5.7: an ingest load,
    /// a pop-time catalog recheck, a discovery poll, a query worker, a
    /// document publish).
    ///
    /// It is needed HERE more than at any of those, because the foreign
    /// code is a parser over bytes a broker sent: an index into a
    /// truncated body or an `unwrap` on an element the feed stopped
    /// sending is a panic, not an `Err`, and there is no version of a
    /// vendor parser this process can promise never panics. Without the
    /// boundary one such message ends the receiver thread for the session
    /// — the source then latches at whatever health it last reported,
    /// nothing ever arrives again, and `contained` being false makes the
    /// process-wide hook write a `crash-<ts>.log` for a failure that cost
    /// one document.
    ///
    /// With it, the panic is reported exactly as an `Err` from the same
    /// parse would be (the load lane, keyed by topic, with the payload as
    /// the reason) and the thread takes the next message. `contained` is
    /// what tells the panic hook this one is handled, so it logs at
    /// `error` and writes no crash file.
    ///
    /// The whole of `on_message` is inside, not just the parse: a panic
    /// anywhere on the path is the same failure of the same message, and a
    /// boundary drawn around one step would leave the others uncovered
    /// for no reason. `AssertUnwindSafe` is the same assertion the runner
    /// makes and rests on the same fact — nothing here holds a lock, and
    /// the two pieces of state a panic could leave mid-update (the
    /// coalescer's map, the unknown-path set) are at worst a stale entry
    /// that the next message for that key supersedes.
    ///
    /// `message` is borrowed rather than moved so the topic is still
    /// readable after a panic unwound out of `on_message`, without
    /// cloning one `String` per message to have it.
    fn handle_message(&mut self, message: &Message, coalescer: &mut Coalescer<Pending>) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            geode_core::panic::contained(|| self.on_message(message, coalescer))
        }));
        if let Err(payload) = outcome {
            // Named "parse panicked" because the parser is the only
            // foreign code inside and every panic seen here in practice is
            // its; a panic in the steps after it reads the same way, which
            // is a mild imprecision rather than a wrong report — the
            // payload and its location name the real site.
            let panicked = panic_payload_message(payload.as_ref());
            (self.on_parse_failure)(&message.topic, format!("parse panicked: {panicked}"));
        }
    }

    /// Parse, validate, stamp, offer. Every failure is reported and
    /// dropped: a broken message must not stop a source, and there is
    /// nothing to retry — the feed will send the key again.
    fn on_message(&mut self, message: &Message, coalescer: &mut Coalescer<Pending>) {
        let parsed = match self.kind.parse(&message.bytes) {
            Ok(parsed) => parsed,
            // Keyed by TOPIC: bytes that did not parse yielded no key, and
            // a failure filed under no batch at all could not be shown
            // against anything.
            Err(e) => {
                (self.on_parse_failure)(&message.topic, format!("parse: {e}"));
                return;
            }
        };
        for path in &parsed.unknown_paths {
            if !self.unknown.contains(path) {
                tracing::warn!(
                    target: "geode::ingest",
                    "source {}: unknown element {path} in {} document; skipped",
                    self.source,
                    self.kind.name(),
                );
                self.unknown.insert(path.clone());
            }
        }
        let rows = parsed.rows;
        // The key is needed by both failure reports below and by the
        // coalescer, and it is the `batch` the publish will record.
        let key = join_key(&rows.key);
        if let Err(e) = rows.validate(&self.dataset) {
            (self.on_parse_failure)(&key, e);
            return;
        }
        let source_time = match source_time_of(&self.policy, &rows, message.received) {
            Ok(t) => t,
            Err(e) => {
                (self.on_parse_failure)(&key, e);
                return;
            }
        };
        let pending = Pending {
            rows,
            received: message.received,
            source_time,
            bytes: message.bytes.len() as u64,
        };
        if let Some((_key, released)) = coalescer.offer(Instant::now(), key, pending) {
            self.submit(released);
        }
    }

    /// Hands one released document to the runner. Never blocks on it:
    /// `submit_document` takes the queue lock, pushes and notifies.
    fn submit(&self, pending: Pending) {
        self.ingest.submit_document(DocumentJob {
            source: self.source.clone(),
            dataset: self.dataset.name.clone(),
            rows: pending.rows,
            source_time: pending.source_time,
            received_at: pending.received,
            bytes: pending.bytes,
        });
    }
}

/// The instant a publish of `rows` is stamped with, under this source's
/// `source_time` policy (`geode_core::source_config::SourceTime`).
///
/// Pure, and the reason it is: as-of routing, the backfill guard and
/// retention all order by this one value, so what it resolves to has to be
/// testable for every policy without a feed, a thread or a clock.
///
/// `Receive` is the moment the message arrived. `Document(field)` reads a
/// document-level attribute instead: a `Date` (a business date, which has
/// no time of day) becomes that date's MIDNIGHT UTC, and a `Utf8` is read
/// as RFC 3339 — which carries its own offset, so a feed stamping local
/// time with an offset is honoured rather than silently read as UTC. Any
/// other type is an error rather than a guess: a number could be epoch
/// seconds, millis or days, and picking one would stamp a generation
/// wrongly with nothing to notice it by.
///
/// A missing or wrongly-typed field is reported per document even though
/// `SourceSpec::from_doc` already refused the configuration at load
/// (Task 5: the field must be a `date`/`utf8` document-level attribute).
/// That check is about the SCHEMA; this one is about the document actually
/// sent, and a feed that omits an optional attribute is a feed problem,
/// not a config one.
pub fn source_time_of(
    policy: &SourceTime,
    rows: &DocumentRows,
    received: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let field = match policy {
        SourceTime::Receive => return Ok(received),
        SourceTime::Document(field) => field,
    };
    let Some((_, value)) = rows.attributes.iter().find(|(name, _)| name == field) else {
        return Err(format!(
            "source_time field '{field}' is not among the document's attributes"
        ));
    };
    match value {
        Value::Date(date) => Ok(date.and_time(NaiveTime::MIN).and_utc()),
        Value::Utf8(text) => DateTime::parse_from_rfc3339(text)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|e| {
                format!("source_time field '{field}' is {text:?}, not an RFC 3339 time: {e}")
            }),
        other => Err(format!(
            "source_time field '{field}' is {}, which is neither a date nor an RFC 3339 text",
            type_label(other.column_type())
        )),
    }
}

/// The spelling a diagnostic uses for a column type. `geode-core`'s own
/// `type_name` is private to its document module, and the two strings a
/// reader sees must match the `type = "…"` key they would edit.
fn type_label(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Utf8 => "utf8",
        ColumnType::F64 => "f64",
        ColumnType::I64 => "i64",
        ColumnType::Date => "date",
        ColumnType::Timestamp => "timestamp",
        ColumnType::Bool => "bool",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Adapter, ChannelAdapter, ChannelFeed, ConnectionState, HealthSink};
    use crate::ingest::runner::{IngestEvent, IngestRunner};
    use crate::store::Store;
    use crate::store::catalog::Catalog;
    use crate::store::ddl::tests_support::{
        FakeKind, PARSE_PANIC, PanickingKind, cvi_dataset, cvi_doc, d, ts,
    };
    use geode_core::document::{
        DocumentKind, DocumentRows, ParseError, ParsedDocument, Value, WriteError,
    };
    use geode_core::schema::{ColumnType, SchemaSpec};
    use geode_core::source_config::{SourceSpec, SourceTime};
    use std::sync::mpsc::Receiver;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    // ---- source_time_of (pure) -----------------------------------------

    fn spx() -> DocumentRows {
        cvi_doc("SPX.Z", [1., 2., 3., 4., 5., 6.])
    }

    #[test]
    fn the_receive_policy_stamps_the_moment_the_message_arrived() {
        let received = ts("2026-09-12T14:05:06Z");
        assert_eq!(
            source_time_of(&SourceTime::Receive, &spx(), received),
            Ok(received)
        );
    }

    #[test]
    fn a_document_date_attribute_stamps_midnight_utc_of_that_date() {
        // `anchor_date` is a `Date` attribute: a business date, which has
        // no time of day, so the instant is that date's midnight UTC.
        let rows = spx();
        assert_eq!(rows.attributes[0].0, "anchor_date");
        assert_eq!(rows.attributes[0].1, Value::Date(d("2026-09-12")));
        assert_eq!(
            source_time_of(
                &SourceTime::Document("anchor_date".into()),
                &rows,
                ts("2026-09-13T09:00:00Z")
            ),
            Ok(ts("2026-09-12T00:00:00Z")),
            "the document's own date, not the moment it arrived"
        );
    }

    #[test]
    fn a_document_text_attribute_is_read_as_rfc_3339() {
        let mut rows = spx();
        rows.attributes.push((
            "stamped".into(),
            Value::Utf8("2026-09-12T16:05:00+02:00".into()),
        ));
        assert_eq!(
            source_time_of(
                &SourceTime::Document("stamped".into()),
                &rows,
                ts("2026-09-13T09:00:00Z")
            ),
            Ok(ts("2026-09-12T14:05:00Z")),
            "an offset is honoured rather than assumed to be UTC"
        );
        // Text that is not a timestamp names the field AND the text, so a
        // diagnostics row says which attribute of whose document is wrong.
        let mut bad = spx();
        bad.attributes
            .push(("stamped".into(), Value::Utf8("yesterday".into())));
        let e = source_time_of(
            &SourceTime::Document("stamped".into()),
            &bad,
            ts("2026-09-13T09:00:00Z"),
        )
        .expect_err("not a timestamp");
        assert!(e.contains("stamped") && e.contains("yesterday"), "{e}");
    }

    #[test]
    fn a_missing_or_wrongly_typed_source_time_field_is_an_error() {
        let received = ts("2026-09-13T09:00:00Z");
        let missing = source_time_of(&SourceTime::Document("nonesuch".into()), &spx(), received)
            .expect_err("no such attribute");
        assert!(missing.contains("nonesuch"), "{missing}");
        // `spot_ref` exists but is an `f64`: a number is not a time, and
        // guessing (epoch seconds? days?) is exactly the silent wrong
        // answer as-of routing must not be given.
        let wrong = source_time_of(&SourceTime::Document("spot_ref".into()), &spx(), received)
            .expect_err("an f64 is not a time");
        assert!(
            wrong.contains("spot_ref") && wrong.contains("f64"),
            "{wrong}"
        );
    }

    // ---- the receiver thread, over a real ChannelAdapter ---------------

    struct Harness {
        _dir: tempfile::TempDir,
        /// Opened before the store moved onto the ingest thread, so a test
        /// can read back what was actually published.
        conn: duckdb::Connection,
        events: Receiver<IngestEvent>,
        /// Kept alive: dropping it shuts the runner down.
        _ingest: Arc<IngestHandle>,
        feed: ChannelFeed,
        worker: SubscriptionWorker,
        failures: Arc<Mutex<Vec<(String, String)>>>,
        states: Arc<Mutex<Vec<ConnectionState>>>,
    }

    fn harness(
        coalesce: Duration,
        kind: Arc<dyn DocumentKind>,
        source_time: SourceTime,
    ) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let conn = store.reader().unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds.clone());
        let (handle, events) = IngestRunner::spawn_channel(store, schema);
        let ingest = Arc::new(handle);

        let (bus, feed) = ChannelAdapter::new("test_bus");
        let spec = SourceSpec {
            adapter: "test_bus".into(),
            document: Some("fake_cvi".into()),
            topics: vec!["cvi/>".into()],
            coalesce,
            source_time,
            ..SourceSpec::directory("cvi", "cvi_params", Vec::new())
        };
        let failures: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let states: Arc<Mutex<Vec<ConnectionState>>> = Arc::new(Mutex::new(Vec::new()));
        let on_parse_failure: ParseFailureSink = {
            let failures = Arc::clone(&failures);
            Arc::new(move |batch: &str, reason: String| {
                failures.lock().unwrap().push((batch.to_string(), reason));
            })
        };
        let on_connection: HealthSink = {
            let states = Arc::clone(&states);
            Arc::new(move |state: ConnectionState| states.lock().unwrap().push(state))
        };
        let worker = SubscriptionWorker::spawn(
            &spec,
            ds,
            kind,
            bus.subscription().expect("the channel adapter subscribes"),
            Arc::clone(&ingest),
            on_parse_failure,
            on_connection,
        )
        .expect("the bus is open");
        Harness {
            _dir: dir,
            conn,
            events,
            _ingest: ingest,
            feed,
            worker,
            failures,
            states,
        }
    }

    /// The default harness: every message publishes (a zero coalescing
    /// window) and the source time is the moment of receipt.
    fn plain_harness() -> Harness {
        harness(
            Duration::ZERO,
            Arc::new(FakeKind::new()),
            SourceTime::Receive,
        )
    }

    /// The next publish, skipping the `PlanComplete` the runner emits
    /// whenever its queue empties (one rides behind every document).
    fn published(rx: &Receiver<IngestEvent>) -> (String, usize) {
        loop {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(IngestEvent::Published { batch, rows, .. }) => return (batch, rows),
                Ok(IngestEvent::PlanComplete) => continue,
                Ok(other) => panic!("expected a publish: {other:?}"),
                Err(e) => panic!("no publish: {e}"),
            }
        }
    }

    /// Asserts no publish and no failure arrives for `within` — the
    /// coalescer's held message must be dropped, not published late.
    fn nothing_more(rx: &Receiver<IngestEvent>, within: Duration) {
        let deadline = Instant::now() + within;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(IngestEvent::PlanComplete) => continue,
                Ok(other) => panic!("an extra outcome: {other:?}"),
                Err(_) => return,
            }
        }
    }

    fn live_params(conn: &duckdb::Connection, key: &str) -> Vec<f64> {
        let mut stmt = conn
            .prepare(
                "select param from cvi_params_document_live \
                 where underlying_ref = ? order by term, node",
            )
            .unwrap();
        stmt.query_map(duckdb::params![key], |r| r.get::<_, f64>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Spins (with a yield, not a fixed sleep) until `f` or 30s.
    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::yield_now();
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn a_received_message_parses_and_publishes_through_the_runner() {
        let mut h = plain_harness();
        assert!(h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.])
        ));
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        assert_eq!(live_params(&h.conn, "SPX.Z"), vec![1., 2., 3., 4., 5., 6.]);
        // Unknown elements are skipped, not fatal: the document still
        // publishes (the receiver logs each path once per source).
        assert!(
            h.feed
                .publish("cvi/SPX.Z", b"SPX.Z:7,7,7,7,7,7:a/b".to_vec())
        );
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        assert_eq!(live_params(&h.conn, "SPX.Z"), vec![7., 7., 7., 7., 7., 7.]);
        assert!(h.failures.lock().unwrap().is_empty());
        h.worker.shutdown();
    }

    #[test]
    fn two_messages_for_one_key_inside_the_window_publish_once_with_the_latest() {
        // A one-second window: long enough that the two follow-up
        // messages below cannot fall outside it on a loaded machine, and
        // the whole test still finishes in about that second.
        let mut h = harness(
            Duration::from_secs(1),
            Arc::new(FakeKind::new()),
            SourceTime::Receive,
        );
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        h.feed.publish(
            "cvi/NDX.Z",
            FakeKind::message("NDX.Z", [100., 200., 300., 400., 500., 600.]),
        );
        // Two keys coalesce independently, so both go at once — in
        // either order.
        let mut first = vec![published(&h.events).0, published(&h.events).0];
        first.sort();
        assert_eq!(first, vec!["NDX.Z".to_string(), "SPX.Z".to_string()]);

        // Two more for SPX.Z inside its window: exactly one publish, and
        // it carries the LAST message's parameters.
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [10., 20., 30., 40., 50., 60.]),
        );
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [11., 21., 31., 41., 51., 61.]),
        );
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        assert_eq!(
            live_params(&h.conn, "SPX.Z"),
            vec![11., 21., 31., 41., 51., 61.],
            "the newest message wins; the one it replaced is never published"
        );
        nothing_more(&h.events, Duration::from_millis(400));
        h.worker.shutdown();
    }

    #[test]
    fn unparseable_bytes_are_reported_under_the_topic_and_the_worker_carries_on() {
        let mut h = plain_harness();
        h.feed.publish("cvi/rubbish", b"not-a-document".to_vec());
        wait_until("the parse failure", || {
            !h.failures.lock().unwrap().is_empty()
        });
        {
            let failures = h.failures.lock().unwrap();
            assert_eq!(
                failures[0].0, "cvi/rubbish",
                "keyed by topic: the bytes never yielded a key"
            );
            assert!(failures[0].1.starts_with("parse: "), "{:?}", failures[0].1);
        }
        // Nothing was published, and the next good message still is.
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        h.worker.shutdown();
    }

    #[test]
    fn a_document_that_fails_validation_is_reported_under_its_joined_key() {
        let mut h = plain_harness();
        // A key part carrying the reserved separator: parses, but
        // `DocumentRows::validate` refuses it — and the failure is filed
        // under the key, which is what the load lane is keyed by.
        let key = format!("SPX{}Z", geode_core::document::KEY_SEPARATOR);
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message(&key, [1., 2., 3., 4., 5., 6.]),
        );
        wait_until("the validation failure", || {
            !h.failures.lock().unwrap().is_empty()
        });
        let failures = h.failures.lock().unwrap();
        assert_eq!(failures[0].0, key);
        assert!(failures[0].1.contains("separator"), "{:?}", failures[0].1);
        drop(failures);
        nothing_more(&h.events, Duration::from_millis(200));
        h.worker.shutdown();
    }

    #[test]
    fn an_unresolvable_source_time_is_reported_under_the_key_and_nothing_publishes() {
        let mut h = harness(
            Duration::ZERO,
            Arc::new(FakeKind::new()),
            SourceTime::Document("nonesuch".into()),
        );
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        wait_until("the source-time failure", || {
            !h.failures.lock().unwrap().is_empty()
        });
        let failures = h.failures.lock().unwrap();
        assert_eq!(failures[0].0, "SPX.Z");
        assert!(failures[0].1.contains("nonesuch"), "{:?}", failures[0].1);
        drop(failures);
        nothing_more(&h.events, Duration::from_millis(200));
        h.worker.shutdown();
    }

    #[test]
    fn connection_state_reaches_the_health_sink_and_shutdown_stops_delivery() {
        let mut h = plain_harness();
        assert_eq!(
            h.states.lock().unwrap().clone(),
            vec![ConnectionState::Connected],
            "subscribing reports the current state, so nothing has to poll for it"
        );
        h.feed.set_state(ConnectionState::Lost {
            reason: "broker gone".into(),
        });
        // `set_state` fans out synchronously on this thread, so there is
        // nothing to wait for.
        assert_eq!(
            h.states.lock().unwrap().clone(),
            vec![
                ConnectionState::Connected,
                ConnectionState::Lost {
                    reason: "broker gone".into()
                }
            ]
        );
        h.worker.shutdown();
        // Unsubscribed: a message published afterwards reaches nothing.
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        nothing_more(&h.events, Duration::from_millis(200));
        h.feed.set_state(ConnectionState::Connected);
        assert_eq!(
            h.states.lock().unwrap().len(),
            2,
            "an unsubscribed worker hears no more state either"
        );
    }

    #[test]
    fn a_panicking_parse_is_contained_and_the_receiver_thread_carries_on() {
        let mut h = harness(
            Duration::ZERO,
            Arc::new(PanickingKind::new()),
            SourceTime::Receive,
        );
        h.feed.publish("cvi/SPX.Z", b"anything".to_vec());
        wait_until("the contained panic to be reported", || {
            !h.failures.lock().unwrap().is_empty()
        });
        {
            let failures = h.failures.lock().unwrap();
            assert_eq!(
                failures[0].0, "cvi/SPX.Z",
                "keyed by topic: a panicking parse yielded no key, exactly as an Err does"
            );
            assert!(
                failures[0].1.contains("panicked") && failures[0].1.contains(PARSE_PANIC),
                "the payload a trader needs to see is the panic's own message: {:?}",
                failures[0].1
            );
        }
        // The thread is still there. A second message handled is the only
        // honest evidence: an unwinding receiver thread ends, and its
        // source then latches at whatever health it last reported with
        // nothing ever arriving again.
        h.feed.publish("cvi/NDX.Z", b"anything".to_vec());
        wait_until("the second message to be handled too", || {
            h.failures.lock().unwrap().len() == 2
        });
        assert_eq!(h.failures.lock().unwrap()[1].0, "cvi/NDX.Z");
        nothing_more(&h.events, Duration::from_millis(100));
        h.worker.shutdown();
    }

    /// A kind that holds the receiver inside `parse` until a test lets it
    /// go, so the subscription's queue can be filled while nothing is
    /// draining it. Everything but `parse` is [`FakeKind`]'s.
    struct GateKind {
        inner: FakeKind,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }

    impl DocumentKind for GateKind {
        fn name(&self) -> &'static str {
            self.inner.name()
        }

        fn columns(&self) -> &[(&'static str, ColumnType)] {
            self.inner.columns()
        }

        fn parse(&self, bytes: &[u8]) -> Result<ParsedDocument, ParseError> {
            let (lock, opened) = &*self.gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = opened.wait(open).unwrap();
            }
            drop(open);
            self.inner.parse(bytes)
        }

        fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, WriteError> {
            self.inner.write(rows)
        }
    }

    #[test]
    fn a_subscription_whose_queue_fills_counts_what_it_could_not_take() {
        // A long coalescing window: this test is about the queue in front
        // of the receiver, and nothing the flood eventually parses should
        // reach the publish path.
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let kind = Arc::new(GateKind {
            inner: FakeKind::new(),
            gate: Arc::clone(&gate),
        });
        let mut h = harness(Duration::from_secs(60), kind, SourceTime::Receive);
        assert_eq!(
            h.worker.refused(),
            0,
            "nothing has been refused before anything was sent"
        );
        let body = FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]);
        // The receiver is parked inside `parse` on the first message, so
        // the MESSAGE_BOUND-deep queue behind it fills and the
        // dispatcher's pushes start being refused. Published in a loop
        // rather than a fixed count because the bus's own inbound queue is
        // the same depth: some publishes are refused a hop earlier and
        // never reach this sink at all.
        wait_until("a message the subscription could not take", || {
            h.feed.publish("cvi/SPX.Z", body.clone());
            h.worker.refused() > 0
        });
        // Let the thread out of `parse` before joining it: `shutdown`
        // joins, and a receiver still waiting on this gate never returns.
        {
            let (lock, opened) = &*gate;
            *lock.lock().unwrap() = true;
            opened.notify_all();
        }
        h.worker.shutdown();
    }
}
