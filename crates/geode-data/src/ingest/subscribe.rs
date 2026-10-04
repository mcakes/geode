//! One receiver thread per subscribed source parses, validates, stamps, and
//! coalesces documents before submitting them to the ingest writer.
//!
//! The adapter's MessageSink is bounded and refuses without waiting. The
//! coalescer keeps one pending snapshot per key; it does not replace jobs
//! already queued at ingest or cap the number of distinct keys. The receive
//! loop waits until the next deadline or MAX_WAIT, whichever comes first.
//! Releases depend on thread scheduling and message-processing time.
//!
//! The service combines adapter connection reports in the discovery lane with
//! receiver/publication reports in the load lane. Parse failures without a
//! key use the raw topic. A later message that parses, validates, and stamps
//! successfully clears that topic entry; publication reports separately under
//! the document key. A clean connection report cannot clear either failure.
//! Messages the bounded queue refused are reported on the load lane under
//! `<source>:queue` (`health::condition_key`): `Degraded "N messages dropped
//! since HH:MM:SS"` while drops continue, `Ok` after `DROP_QUIET` without one.
//! Only this receiver clears that slot, so when the subscription ends
//! (disconnect or stop) it clears an open episode with `Ok`: no receiver, no
//! drops.
//!
//! Recovery. A transport that can answer a GET is asked for the latest
//! document on every concrete topic the receiver knows: the topics the
//! service read from the store at open, plus the topic of each document
//! submitted to the runner this run. It is asked once at start and again
//! after each reconnect (a `Connected` report following a non-`Connected`
//! one). Replies arrive on the same sink marked `recovered` and take the
//! ordinary parse, source-time and coalescer path, judged by a
//! [`RecoveryWindow`] under two rules:
//! - Rule 1: a NOTIFY for a key received at or after the window's start
//!   beats a reply for that key, which is dropped. The window starts just
//!   before `subscribe` at start, and at the disconnect on a reconnect, so a
//!   NOTIFY handled before the receiver noticed the reconnect still counts.
//!   `last_notify` holds one entry per key, like the coalescer's release
//!   times, with no fixed cap.
//! - Rule 2: a reply that survives is submitted marked recovered, and the
//!   store publishes it only if it differs from live. A NOTIFY replacing a
//!   pending reply in the coalescer is unmarked and publishes as usual.
//!
//! A reply on a topic the request did not name, arriving while a window is
//! open, takes the same path (rule 1 included) but does not count as
//! answered. A reply after its window, or with none open, is dropped and
//! counted in the end line of the window that closes next (or the
//! end-of-subscription line). The window closes early on replies only; a
//! NOTIFY on an asked topic received since the start covers that topic in
//! the report. Each window reports on the load lane under
//! `<source>:recovery`: `Degraded` when the request failed, or when no
//! reply arrived and some asked topic was not covered; `Ok` otherwise. An
//! open window at stop reports nothing. Only one window is open at a time:
//! a reconnect during recovery supersedes the open window, which logs one
//! line and makes no health report, and the new window starts at the
//! latest disconnect. Known limit: a `Message` carries no request id, so a
//! reply to the superseded request that lands in the new window is judged
//! by the new start; after two reconnects inside one window a key can stay
//! stale until its next NOTIFY.
//!
//! Topic records. A NOTIFY document carries its topic to the runner for
//! recording on its first document per run, and again once its record here
//! is `RERECORD_AFTER` old. A recovered document always carries its topic
//! and never marks that record: a reply proves the topic alive, so the
//! store records it whether the reply publishes or is unchanged, and the
//! topic's first NOTIFY this run still records it.
//!
//! Parsed columns move into DocumentJob without per-row copies. Shutdown
//! unsubscribes, sets the stop flag, and joins; pending coalesced documents are
//! not flushed. Blocking adapter/parser code can delay shutdown.

use crate::adapter::{
    AdapterError, ConnectionState, HealthSink, MESSAGE_BOUND, Message, MessageSink, Recovery,
    Subscription,
};
use crate::health::Health;
use crate::ingest::coalesce::Coalescer;
use crate::ingest::recover::{RERECORD_AFTER, RecoveryReport, RecoveryWindow, ReplyVerdict};
use crate::ingest::runner::{DocumentJob, IngestHandle, panic_payload_message};
use chrono::{DateTime, NaiveTime, Utc};
use geode_core::clock::Clock;
use geode_core::document::{DocumentKind, DocumentRows, Value, join_key};
use geode_core::schema::{ColumnType, DatasetSpec};
use geode_core::source_config::{SourceSpec, SourceTime};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Maximum idle receive wait when no coalesced document is due sooner.
/// Messages and disconnection wake the receiver. This bounds stop-flag checks
/// while idle, not total shutdown latency or parser execution time.
const MAX_WAIT: Duration = Duration::from_millis(250);

/// How long a subscription must go without a new drop before its
/// `<source>:queue` load lane reports `Ok` and the episode ends.
pub const DROP_QUIET: Duration = Duration::from_secs(60);

/// The shortest interval between two reports of a still-growing drop count:
/// a flood updates its count about once a second, not once per message.
const DROP_REPORT_EVERY: Duration = Duration::from_secs(1);

/// What a receiver says about the messages its bounded queue refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DropReport {
    /// `dropped` messages since `since`: the episode's first drop as this
    /// receiver observed it (within one receive cycle, later if a parser held
    /// the receiver).
    Dropping { dropped: u64, since: DateTime<Utc> },
    /// `DROP_QUIET` passed with no new drop.
    Quiet,
}

#[derive(Debug)]
struct Episode {
    /// The refusal total before the episode's first drop.
    base: u64,
    since: DateTime<Utc>,
    last_drop: Instant,
    reported: u64,
    reported_at: Instant,
}

/// One subscription's drop episodes, fed the sink's lifetime refusal total.
/// Pure: the caller supplies both clocks.
#[derive(Debug, Default)]
pub(crate) struct DropEpisode {
    seen: u64,
    open: Option<Episode>,
}

impl DropEpisode {
    /// A rise in `total` is read before the quiet check, so a drop that lands
    /// as the quiet interval ends keeps the episode open rather than racing
    /// its clear; a drop after a clear opens a new episode counted from zero.
    pub(crate) fn observe(
        &mut self,
        total: u64,
        now: Instant,
        wall: DateTime<Utc>,
    ) -> Option<DropReport> {
        if total > self.seen {
            let before = self.seen;
            self.seen = total;
            match self.open.as_mut() {
                None => {
                    self.open = Some(Episode {
                        base: before,
                        since: wall,
                        last_drop: now,
                        reported: total,
                        reported_at: now,
                    });
                    return Some(DropReport::Dropping {
                        dropped: total - before,
                        since: wall,
                    });
                }
                Some(open) => open.last_drop = now,
            }
        }
        let open = self.open.as_mut()?;
        if now.saturating_duration_since(open.last_drop) >= DROP_QUIET {
            self.open = None;
            return Some(DropReport::Quiet);
        }
        if open.reported < self.seen
            && now.saturating_duration_since(open.reported_at) >= DROP_REPORT_EVERY
        {
            open.reported = self.seen;
            open.reported_at = now;
            return Some(DropReport::Dropping {
                dropped: self.seen - open.base,
                since: open.since,
            });
        }
        None
    }

    /// Close the open episode, if any, when the subscription ends; whether
    /// one was open (its `Degraded` needs clearing).
    pub(crate) fn end(&mut self) -> bool {
        self.open.take().is_some()
    }
}

/// The load-lane slot a drop report fills under `key` (`<source>:queue`).
pub(crate) fn drop_health(key: &str, report: &DropReport, clock: Clock) -> (Health, String) {
    match report {
        DropReport::Dropping { dropped, since } => {
            let reason = format!("{dropped} messages dropped since {}", clock.hms(*since));
            (
                Health::Degraded {
                    reason: reason.clone(),
                },
                format!("{key}: {reason}"),
            )
        }
        DropReport::Quiet => (
            Health::Ok,
            format!("{key}: no drops for {}s", DROP_QUIET.as_secs()),
        ),
    }
}

/// What a receiver thread reports about its own documents: the batch it
/// is filed under (the document's joined key, or the topic when the bytes
/// never yielded one), the health, and the detail line that goes with it.
///
/// The health is a parameter rather than always `Failed` because this
/// module reports BOTH directions: a failure, and the `Ok` that clears a
/// topic-keyed one (see the module doc — no other writer of that lane
/// knows the topic key exists).
///
/// A closure rather than a channel because the service already owns the
/// one door these have to go through — `HealthTracker`'s load lane — and
/// the shape of a `DataEvent` is not this module's business. It must not
/// block: it is called on the receiver thread, between two messages.
pub type LoadReportSink = Arc<dyn Fn(&str, Health, String) + Send + Sync>;

/// One parsed document waiting for its release window, as the coalescer
/// holds it.
///
/// `source_time` is resolved when the message arrives rather than when the
/// document is released, for two reasons: its failure is a report about
/// THIS message (the load report names the key), and resolving it late
/// would mean either doing the work twice or having a fallible step on the
/// release path with nowhere sensible to report from.
struct Pending {
    rows: DocumentRows,
    received: DateTime<Utc>,
    source_time: DateTime<Utc>,
    bytes: u64,
    /// The concrete topic it arrived on, for recording and later recovery.
    topic: String,
    /// A recovery reply. Per message, not per key: a NOTIFY that replaces a
    /// pending reply carries `false` and publishes without comparison.
    recovered: bool,
}

/// Epoch microseconds as a time; the reconnect atomics hold these, with 0
/// meaning unset.
fn micros_to_utc(us: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(us).unwrap_or_default()
}

/// What the receiver needs from connection reports, which arrive on the
/// transport's thread: when the connection went down, and once it is back,
/// that a recovery is due from that instant. Epoch micros; 0 is unset.
#[derive(Clone, Default)]
struct Reconnect {
    down_at: Arc<AtomicI64>,
    reconnected_at: Arc<AtomicI64>,
}

impl Reconnect {
    /// Wrap the service's connection sink: observe, then forward.
    fn watch(&self, forward: HealthSink) -> HealthSink {
        let this = self.clone();
        Arc::new(move |state: ConnectionState| {
            this.observe(&state);
            forward(state)
        })
    }

    /// The first non-`Connected` report of an outage marks its start; the
    /// `Connected` that ends it hands that start to the receiver, replacing
    /// any start the receiver has not taken yet. Rule 1 is sound only when
    /// a window starts no earlier than the most recent disconnect: a NOTIFY
    /// received between two outages predates what the second outage missed,
    /// and an earlier start would let it drop the reply carrying that update.
    fn observe(&self, state: &ConnectionState) {
        match state {
            ConnectionState::Connected => {
                let down = self.down_at.swap(0, Ordering::AcqRel);
                if down != 0 {
                    self.reconnected_at.store(down, Ordering::Release);
                }
            }
            ConnectionState::Reconnecting | ConnectionState::Lost { .. } => {
                let now = Utc::now().timestamp_micros();
                let _ = self
                    .down_at
                    .compare_exchange(0, now, Ordering::AcqRel, Ordering::Relaxed);
            }
        }
    }
}

/// Owns a source's subscription and receiver thread. Keeping the subscription
/// here lets shutdown unsubscribe before joining the receiver.
pub struct SubscriptionWorker {
    subscription: Box<dyn Subscription>,
    stop: Arc<AtomicBool>,
    /// Keep only the refusal counter, not a MessageSink clone. Retaining a sender
    /// here would prevent unsubscribe from disconnecting the receiver.
    refused: Arc<AtomicU64>,
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
    ///
    /// `known_topics` are the concrete topics the receiver asks the
    /// transport to recover at start; it adds every topic a document
    /// arrives on, for recoveries after a reconnect.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        spec: &SourceSpec,
        dataset: DatasetSpec,
        kind: Arc<dyn DocumentKind>,
        mut subscription: Box<dyn Subscription>,
        ingest: Arc<IngestHandle>,
        report_load: LoadReportSink,
        on_connection: HealthSink,
        clock: Clock,
        stopped: crate::service::EventSink,
        known_topics: Vec<String>,
    ) -> Result<SubscriptionWorker, AdapterError> {
        let (sink, rx) = MessageSink::bounded(MESSAGE_BOUND);
        // The counter first, then the sink MOVED into the adapter: after
        // this line nothing on this side holds a sender (see the `refused`
        // field), which is what makes `unsubscribe` disconnect.
        let refused = sink.refused_counter();
        let reconnect = Reconnect::default();
        // Before subscribing: every NOTIFY the subscription delivers is
        // received after this, so each one beats the start's replies for
        // its key under rule 1.
        let subscribed_at = Utc::now();
        subscription.subscribe(&spec.topics, sink, reconnect.watch(on_connection))?;
        let recovery = subscription.recovery();
        let stop = Arc::new(AtomicBool::new(false));
        let mut receiving = Receiving {
            source: spec.name.clone(),
            dataset,
            kind,
            policy: spec.source_time.clone(),
            ingest,
            report_load,
            stop: Arc::clone(&stop),
            unknown: UnknownPaths::new(spec.name.clone()),
            failed_topics: HashSet::new(),
            refused: Arc::clone(&refused),
            drops: DropEpisode::default(),
            queue_key: crate::health::condition_key(&spec.name, crate::health::QUEUE),
            clock,
            recovery,
            recover_timeout: spec.recover_timeout,
            recovery_key: crate::health::condition_key(&spec.name, crate::health::RECOVERY),
            window: None,
            late_replies: 0,
            unsupported_logged: false,
            last_notify: HashMap::new(),
            recorded: RecordedTopics::default(),
            known: known_topics.into_iter().collect(),
            reconnected_at: Arc::clone(&reconnect.reconnected_at),
        };
        let window = spec.coalesce;
        let thread = crate::supervise::spawn_supervised(
            format!("geode-subscribe-{}", spec.name),
            stopped,
            move || receiving.run(rx, window, subscribed_at),
        );
        match thread {
            Ok(thread) => Ok(SubscriptionWorker {
                subscription,
                stop,
                refused,
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
        self.refused.load(Ordering::Relaxed)
    }

    /// Unsubscribe, set the stop flag, and join. Idempotent. Pending coalesced
    /// documents are not flushed. Disconnection wakes an idle receiver; the stop
    /// flag also handles adapters that retain a sender after unsubscribe.
    /// In-progress adapter/parser calls must return before shutdown can complete.
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

/// Maximum number of unknown-element paths retained per receiver. Feed-chosen
/// names must not grow this warning-deduplication set without bound.
const UNKNOWN_PATH_CAP: usize = 256;

/// Per-source deduplication of unknown-element warnings, with a fixed cap.
struct UnknownPaths {
    source: String,
    seen: HashSet<String>,
    /// Whether the one "cap reached" warning has already fired. Separate
    /// from the per-path warning `first_sighting`'s caller makes: this
    /// one is about the SET, not about any particular path, and must
    /// fire exactly once no matter how many further distinct paths
    /// arrive.
    cap_warned: bool,
}

impl UnknownPaths {
    fn new(source: String) -> Self {
        UnknownPaths {
            source,
            seen: HashSet::new(),
            cap_warned: false,
        }
    }

    /// Return false for remembered paths and true for a new path. At the cap,
    /// warn once and stop remembering new paths; unremembered paths return true
    /// on every call, so their per-path warnings can repeat.
    fn first_sighting(&mut self, path: &str) -> bool {
        if self.seen.contains(path) {
            return false;
        }
        if self.seen.len() < UNKNOWN_PATH_CAP {
            self.seen.insert(path.to_string());
            return true;
        }
        if !self.cap_warned {
            tracing::warn!(
                target: "geode::ingest",
                "source {}: unknown-element path cap ({UNKNOWN_PATH_CAP}) \
                 reached; further distinct paths will warn every time \
                 rather than once",
                self.source,
            );
            self.cap_warned = true;
        }
        true
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
    report_load: LoadReportSink,
    stop: Arc<AtomicBool>,
    /// Element paths this source's parser has already complained about,
    /// capped so an adversarial or buggy feed's own element names cannot
    /// grow it without bound — see [`UnknownPaths`].
    unknown: UnknownPaths,
    /// Raw topics with unresolved parse failures. Remove a topic after a message
    /// parses, validates, and stamps successfully. This set has no fixed cap;
    /// wildcard subscriptions can expose arbitrarily many distinct topics.
    failed_topics: HashSet<String>,
    /// The sink's lifetime refusal count, shared with the adapter side.
    refused: Arc<AtomicU64>,
    drops: DropEpisode,
    /// `<source>:queue`, computed once.
    queue_key: String,
    clock: Clock,
    /// The transport's GET side; `None` when it cannot recover.
    recovery: Option<Box<dyn Recovery>>,
    recover_timeout: Duration,
    /// `<source>:recovery`, computed once.
    recovery_key: String,
    /// The open recovery window, which holds the topics it asked for.
    window: Option<RecoveryWindow>,
    /// Replies no window could judge, since the last window's end line.
    late_replies: u64,
    /// "Cannot recover" is logged once per subscription.
    unsupported_logged: bool,
    /// Key → `received` of its newest NOTIFY: rule 1's evidence. One entry
    /// per key, no fixed cap.
    last_notify: HashMap<String, DateTime<Utc>>,
    /// Topic → when this run last carried it to the runner on a NOTIFY.
    recorded: RecordedTopics,
    /// Every topic a recovery asks for; sorted and unique.
    known: BTreeSet<String>,
    /// A reconnect's disconnect instant in epoch micros, set by the
    /// connection callback and taken here; 0 when none is waiting.
    reconnected_at: Arc<AtomicI64>,
}

impl Receiving {
    /// The thread's whole life.
    fn run(&mut self, rx: Receiver<Message>, window: Duration, subscribed_at: DateTime<Utc>) {
        let mut coalescer: Coalescer<Pending> = Coalescer::new(window);
        self.start_recovery(subscribed_at);
        while !self.stop.load(Ordering::Relaxed) {
            self.notice_reconnect();
            let now = Instant::now();
            let mut wait = coalescer.next_deadline().map_or(MAX_WAIT, |due| {
                due.saturating_duration_since(now).min(MAX_WAIT)
            });
            if let Some(open) = &self.window {
                wait = wait.min(open.deadline().saturating_duration_since(now));
            }
            match rx.recv_timeout(wait) {
                Ok(message) => self.handle_message(&message, &mut coalescer),
                Err(RecvTimeoutError::Timeout) => {}
                // Every clone of our sink is gone, so no message can ever
                // arrive again: the subscription was dropped or
                // unsubscribed (`shutdown`, or an adapter that ended it).
                // Nothing to wait for.
                Err(RecvTimeoutError::Disconnected) => break,
            }
            // After a message AND after a timeout: an offer that was held
            // back may be due by now, and a quiet feed is exactly when a
            // release must not wait for the next message to trigger it.
            for (_key, pending) in coalescer.due(Instant::now()) {
                self.submit(pending);
            }
            if self.window.as_ref().is_some_and(|w| w.done(Instant::now())) {
                self.finish_recovery();
            }
            self.watch_drops();
        }
        if self.late_replies > 0 {
            tracing::info!(
                target: "geode::ingest",
                "source {}: {} recovery replies arrived with no window open and were dropped",
                self.source,
                self.late_replies,
            );
        }
        self.end_drops();
    }

    /// Start a recovery if the connection came back since the last turn.
    fn notice_reconnect(&mut self) {
        let down = self.reconnected_at.swap(0, Ordering::AcqRel);
        if down != 0 {
            let started_at = micros_to_utc(down);
            self.start_recovery(started_at);
        }
    }

    /// Ask for every known topic under a window opened at `started_at`,
    /// before the request goes out so no reply arrives unjudged. An open
    /// window is finished first: reported if it was done, otherwise
    /// superseded, which logs and reports nothing (its outcome was cut
    /// short, and the new window reports for the source).
    fn start_recovery(&mut self, started_at: DateTime<Utc>) {
        if self.window.as_ref().is_some_and(|w| w.done(Instant::now())) {
            self.finish_recovery();
        } else if self.window.take().is_some() {
            tracing::info!(
                target: "geode::ingest",
                "source {}: recovery superseded by a reconnect before its window ended",
                self.source,
            );
        }
        if self.known.is_empty() {
            return;
        }
        let Some(recovery) = self.recovery.as_mut() else {
            if !self.unsupported_logged {
                self.unsupported_logged = true;
                tracing::info!(
                    target: "geode::ingest",
                    "source {}: the transport cannot recover; {} known topics wait for their next update",
                    self.source,
                    self.known.len(),
                );
            }
            return;
        };
        let asked: Vec<String> = self.known.iter().cloned().collect();
        self.window = Some(RecoveryWindow::start(
            Instant::now(),
            started_at,
            self.recover_timeout,
            &asked,
        ));
        if let Err(e) = recovery.recover(&asked, self.recover_timeout) {
            self.window = None;
            let reason = format!("recovery failed: {e}");
            self.report_recovery(
                Health::Degraded {
                    reason: reason.clone(),
                },
                reason,
            );
        }
    }

    /// Close the open window and report its outcome under
    /// `<source>:recovery`, with one log line naming what went unanswered
    /// and how many replies came too late.
    fn finish_recovery(&mut self) {
        let Some(window) = self.window.take() else {
            return;
        };
        let late = std::mem::take(&mut self.late_replies);
        let asked = window.asked();
        let (health, summary) = match window.report() {
            RecoveryReport::AllAnswered => (
                Health::Ok,
                format!("all {asked} topics answered or notified"),
            ),
            RecoveryReport::Partial { unanswered, sample } => (
                Health::Ok,
                format!(
                    "{unanswered} of {asked} topics unanswered ({})",
                    sample.join(", ")
                ),
            ),
            RecoveryReport::NoReplies { asked } => (
                Health::Degraded {
                    reason: format!("recovery: no replies for {asked} topics"),
                },
                format!("no replies for {asked} topics"),
            ),
        };
        tracing::info!(
            target: "geode::ingest",
            "source {}: recovery: {summary}; {late} late replies dropped",
            self.source,
        );
        let what = match &health {
            Health::Degraded { reason } => reason.clone(),
            _ => format!("recovery: {summary}"),
        };
        self.report_recovery(health, what);
    }

    fn report_recovery(&self, health: Health, what: String) {
        let detail = format!("{}: {what}", self.recovery_key);
        (self.report_load)(&self.recovery_key, health, detail);
    }

    /// The subscription ended (disconnect or stop): with no receiver there
    /// are no drops, and only this thread clears `<source>:queue`, so an
    /// open episode is cleared here rather than left standing until restart.
    fn end_drops(&mut self) {
        if self.drops.end() {
            (self.report_load)(
                &self.queue_key,
                Health::Ok,
                format!("{}: subscription ended", self.queue_key),
            );
        }
    }

    /// Report a change in this subscription's drops on the load lane under
    /// `<source>:queue`: the count and the episode's start while drops go on,
    /// `Ok` once `DROP_QUIET` passes without one. One atomic load and two
    /// clock reads per receive cycle.
    fn watch_drops(&mut self) {
        let total = self.refused.load(Ordering::Relaxed);
        let Some(report) = self.drops.observe(total, Instant::now(), Utc::now()) else {
            return;
        };
        let (health, detail) = drop_health(&self.queue_key, &report, self.clock);
        (self.report_load)(&self.queue_key, health, detail);
    }

    /// Handle one message under panic containment. Report a panic as a load
    /// failure keyed by raw topic, including the payload, and continue receiving.
    /// The boundary covers all of on_message, including parsing and validation.
    /// Borrow the message so its topic remains available after unwinding without
    /// cloning it for every message.
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
            self.report_topic_failure(&message.topic, format!("parse panicked: {panicked}"));
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
                self.report_topic_failure(&message.topic, format!("parse: {e}"));
                return;
            }
        };
        for path in &parsed.unknown_paths {
            if self.unknown.first_sighting(path) {
                tracing::warn!(
                    target: "geode::ingest",
                    "source {}: unknown element {path} in {} document; skipped",
                    self.source,
                    self.kind.name(),
                );
            }
        }
        let rows = parsed.rows;
        // The key is needed by both failure reports below and by the
        // coalescer, and it is the `batch` the publish will record.
        let key = join_key(&rows.key);
        if let Err(e) = rows.validate(&self.dataset) {
            self.report_failure(&key, e);
            return;
        }
        let source_time = match source_time_of(&self.policy, &rows, message.received) {
            Ok(t) => t,
            Err(e) => {
                self.report_failure(&key, e);
                return;
            }
        };
        // Everything about this message is now good, so a topic-keyed
        // failure standing against it is over. Before the offer rather
        // than after: a coalesced message may not release for another
        // window, and the recovery is news now.
        self.clear_topic(&message.topic);
        if message.recovered {
            // Judged by the window, never by the reply's own receive time:
            // a reply snapshotted before a newer NOTIFY can arrive after it.
            let verdict = match self.window.as_mut() {
                Some(w) => w.on_reply(
                    Instant::now(),
                    &message.topic,
                    self.last_notify.get(&key).copied(),
                ),
                None => ReplyVerdict::DropLate,
            };
            match verdict {
                ReplyVerdict::Publish => {}
                ReplyVerdict::DropNotified => return,
                ReplyVerdict::DropLate => {
                    self.late_replies += 1;
                    return;
                }
            }
        } else {
            if let Some(at) = self.last_notify.get_mut(&key) {
                *at = message.received;
            } else {
                self.last_notify.insert(key.clone(), message.received);
            }
            // A NOTIFY covers its topic in the open window's report, never
            // closes it: the feed is live even if the GET side is silent.
            if let Some(w) = self.window.as_mut() {
                w.on_notify(&message.topic, message.received);
            }
        }
        let pending = Pending {
            rows,
            received: message.received,
            source_time,
            bytes: message.bytes.len() as u64,
            topic: message.topic.clone(),
            recovered: message.recovered,
        };
        if let Some((_key, released)) = coalescer.offer(Instant::now(), key, pending) {
            self.submit(released);
        }
    }

    /// Files a load-lane failure under `batch` — a document's own joined
    /// key, which the ingest sink's next clean publish of that batch
    /// clears.
    fn report_failure(&self, batch: &str, reason: String) {
        (self.report_load)(
            batch,
            Health::Failed {
                reason: reason.clone(),
            },
            format!("{batch}: {reason}"),
        );
    }

    /// The same, for the two failures with no batch to key on (a parse
    /// `Err`, a panicking parse) — filed under the raw TOPIC and
    /// remembered, because [`Receiving::clear_topic`] is the only thing
    /// that will ever clear it.
    fn report_topic_failure(&mut self, topic: &str, reason: String) {
        self.failed_topics.insert(topic.to_string());
        self.report_failure(topic, reason);
    }

    /// Reports `Ok` under `topic` if a failure was ever filed there.
    ///
    /// `remove` answers that question and forgets it in one lookup, so
    /// the ordinary case — a clean message on a topic that never failed —
    /// costs a hash of the topic and no report at all. A repeated
    /// recovery cannot flood the entity either way: the tracker emits on
    /// a real transition only.
    fn clear_topic(&mut self, topic: &str) {
        if self.failed_topics.remove(topic) {
            (self.report_load)(topic, Health::Ok, format!("{topic}: parse ok"));
        }
    }

    /// Queue a released document under the runner's mutex and notify it.
    /// This does not wait for publication or refuse on queue capacity.
    ///
    /// A recovered document always carries its topic and never touches the
    /// NOTIFY record slot: the store records a reply's topic whether it
    /// publishes or is `Unchanged`, and a reply marking the slot would stop
    /// the topic's first NOTIFY this run from recording it.
    fn submit(&mut self, pending: Pending) {
        let topic = if pending.recovered {
            self.know(&pending.topic);
            Some(pending.topic)
        } else {
            self.topic_to_record(pending.topic, Instant::now())
        };
        self.ingest.submit_document(DocumentJob {
            source: self.source.clone(),
            dataset: self.dataset.name.clone(),
            rows: pending.rows,
            source_time: pending.source_time,
            received_at: pending.received,
            bytes: pending.bytes,
            recovered: pending.recovered,
            topic,
        });
    }

    /// The topic a NOTIFY document carries for recording: on its first
    /// document this run, or once its record here is `RERECORD_AFTER` old
    /// so a long run keeps the store's receive time current and the topic
    /// is not pruned. Every other document carries `None` and costs the
    /// publish nothing.
    fn topic_to_record(&mut self, topic: String, now: Instant) -> Option<String> {
        self.know(&topic);
        self.recorded.due(&topic, now).then_some(topic)
    }

    /// `topic` joins the set later recoveries ask for.
    fn know(&mut self, topic: &str) {
        if !self.known.contains(topic) {
            self.known.insert(topic.to_string());
        }
    }
}

/// Topic → when this run last carried it to the runner on a NOTIFY
/// document. Recovery replies never mark it: the slot exists so the NOTIFY
/// hot path records a topic at most once per `RERECORD_AFTER`.
#[derive(Debug, Default)]
struct RecordedTopics(HashMap<String, Instant>);

impl RecordedTopics {
    /// Whether a NOTIFY document on `topic` at `now` carries it, marking
    /// the slot when it does. Pure: the caller supplies the clock.
    fn due(&mut self, topic: &str, now: Instant) -> bool {
        let fresh = self
            .0
            .get(topic)
            .is_some_and(|at| now.saturating_duration_since(*at) < RERECORD_AFTER);
        if fresh {
            return false;
        }
        self.0.insert(topic.to_string(), now);
        true
    }
}

/// Resolve the publication source timestamp used by backfill, as-of reads,
/// and retention. Receive uses message arrival time. Document(field) reads
/// a document attribute: Date becomes midnight UTC; Utf8 parses RFC 3339
/// with its offset. Missing or other-typed values fail this document.
/// Schema validation at source open cannot guarantee the actual message
/// contains an optional attribute.
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
        FakeKind, GateKind, PARSE_PANIC, PanickingKind, cvi_dataset, cvi_doc, d, ts,
    };
    use geode_core::document::{DocumentKind, DocumentRows, Value};
    use geode_core::schema::SchemaSpec;
    use geode_core::source_config::{SourceSpec, SourceTime};
    use std::sync::mpsc::Receiver;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    // ---- DropEpisode (pure) -------------------------------------------

    fn wall(secs: i64) -> DateTime<Utc> {
        ts("2026-10-01T09:00:00Z") + chrono::Duration::seconds(secs)
    }

    #[test]
    fn a_drop_episode_reports_the_first_drop_and_its_time() {
        let t0 = Instant::now();
        let mut e = DropEpisode::default();
        assert_eq!(e.observe(0, t0, wall(0)), None, "no drop, no report");
        assert_eq!(
            e.observe(3, t0 + Duration::from_millis(250), wall(1)),
            Some(DropReport::Dropping {
                dropped: 3,
                since: wall(1)
            })
        );
    }

    #[test]
    fn a_growing_count_is_reported_at_most_once_a_second() {
        let t0 = Instant::now();
        let mut e = DropEpisode::default();
        e.observe(1, t0, wall(0));
        assert_eq!(e.observe(5, t0 + Duration::from_millis(400), wall(0)), None);
        assert_eq!(
            e.observe(9, t0 + Duration::from_millis(1000), wall(1)),
            Some(DropReport::Dropping {
                dropped: 9,
                since: wall(0)
            }),
            "N counts from the episode's first drop, and the time stays the first drop's"
        );
    }

    #[test]
    fn a_drop_episode_clears_after_sixty_quiet_seconds() {
        let t0 = Instant::now();
        let mut e = DropEpisode::default();
        e.observe(2, t0, wall(0));
        assert_eq!(
            e.observe(2, t0 + DROP_QUIET - Duration::from_millis(1), wall(59)),
            None
        );
        assert_eq!(
            e.observe(2, t0 + DROP_QUIET, wall(60)),
            Some(DropReport::Quiet)
        );
        assert_eq!(
            e.observe(2, t0 + DROP_QUIET * 2, wall(120)),
            None,
            "cleared once"
        );
    }

    #[test]
    fn a_drop_observed_as_the_quiet_interval_ends_keeps_the_episode_open() {
        let t0 = Instant::now();
        let mut e = DropEpisode::default();
        e.observe(2, t0, wall(0));
        // The counter rose between two observations; the one that finds the
        // 60 s mark also finds the new drop, so the episode goes on.
        assert_eq!(
            e.observe(3, t0 + DROP_QUIET, wall(60)),
            Some(DropReport::Dropping {
                dropped: 3,
                since: wall(0)
            })
        );
        assert_eq!(
            e.observe(3, t0 + DROP_QUIET + Duration::from_secs(1), wall(61)),
            None
        );
        assert_eq!(
            e.observe(3, t0 + DROP_QUIET * 2, wall(120)),
            Some(DropReport::Quiet)
        );
    }

    #[test]
    fn a_drop_after_a_clear_opens_a_new_episode_counted_from_zero() {
        let t0 = Instant::now();
        let mut e = DropEpisode::default();
        e.observe(4, t0, wall(0));
        assert_eq!(
            e.observe(4, t0 + DROP_QUIET, wall(60)),
            Some(DropReport::Quiet)
        );
        assert_eq!(
            e.observe(6, t0 + DROP_QUIET + Duration::from_secs(5), wall(65)),
            Some(DropReport::Dropping {
                dropped: 2,
                since: wall(65)
            })
        );
        // The second episode grows; its count is still from its own base
        // (4), not from the subscription's lifetime total.
        e.observe(9, t0 + DROP_QUIET + Duration::from_secs(5), wall(65));
        assert_eq!(
            e.observe(9, t0 + DROP_QUIET + Duration::from_secs(6), wall(66)),
            Some(DropReport::Dropping {
                dropped: 5,
                since: wall(65)
            }),
            "a growing count in a later episode counts from that episode's base"
        );
    }

    #[test]
    fn drop_health_formats_the_reason_on_the_display_clock() {
        let (health, detail) = drop_health(
            "cvi:queue",
            &DropReport::Dropping {
                dropped: 12,
                since: ts("2026-10-01T09:30:05Z"),
            },
            geode_core::clock::Clock::in_zone_named("Asia/Tokyo"),
        );
        assert_eq!(
            health,
            Health::Degraded {
                reason: "12 messages dropped since 18:30:05".into()
            }
        );
        assert_eq!(detail, "cvi:queue: 12 messages dropped since 18:30:05");
        assert_eq!(
            drop_health(
                "cvi:queue",
                &DropReport::Quiet,
                geode_core::clock::Clock::utc()
            )
            .0,
            Health::Ok
        );
    }

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

    // ---- UnknownPaths::first_sighting (pure) ----------------------------

    /// Remembered paths warn once; at the cap, the set stops growing and
    /// unremembered paths continue returning true.
    #[test]
    fn first_sighting_dedupes_below_the_cap_then_warns_once_and_stops_growing() {
        let mut u = UnknownPaths::new("test-source".to_string());
        assert!(u.first_sighting("a/b"), "the first sighting of a path");
        assert!(!u.first_sighting("a/b"), "a repeat is not a first sighting");

        // Fill the set to the cap with distinct paths ("a/b" already
        // counts as one).
        for i in 1..UNKNOWN_PATH_CAP {
            assert!(
                u.first_sighting(&format!("p/{i}")),
                "every distinct path up to the cap is a first sighting"
            );
        }
        assert_eq!(u.seen.len(), UNKNOWN_PATH_CAP);

        // Past the cap: still reports true (there's nowhere left to
        // remember it), the set does not grow further, and asking about
        // the very same overflow path again still reports true — it was
        // never actually recorded.
        assert!(
            u.first_sighting("overflow/1"),
            "past the cap every distinct path still reports as a first sighting"
        );
        assert_eq!(
            u.seen.len(),
            UNKNOWN_PATH_CAP,
            "the set stops growing at the cap"
        );
        assert!(
            u.first_sighting("overflow/1"),
            "an overflow path is never actually recorded, so it reports true again too"
        );
        assert!(u.cap_warned, "the one-time cap warning fired");
    }

    /// A topic is carried on its first NOTIFY, not again inside
    /// `RERECORD_AFTER`, and again once that long has passed — the re-record
    /// is what keeps a topic live through a run longer than
    /// `recover_max_age` from being pruned at the next open.
    #[test]
    fn a_recorded_topic_is_carried_again_once_rerecord_after_has_passed() {
        let mut recorded = RecordedTopics::default();
        let t0 = Instant::now();
        assert!(recorded.due("cvi/SPX.Z", t0), "the first NOTIFY carries it");
        assert!(!recorded.due("cvi/SPX.Z", t0 + Duration::from_secs(3600)));
        assert!(
            recorded.due("cvi/NDX.Z", t0 + Duration::from_secs(3600)),
            "slots are per topic"
        );
        let later = t0 + RERECORD_AFTER;
        assert!(recorded.due("cvi/SPX.Z", later), "a day on, carried again");
        assert!(
            !recorded.due("cvi/SPX.Z", later + Duration::from_secs(3600)),
            "and the slot restarts from the re-record"
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
        /// Every load-lane report the receiver made, as
        /// `(batch, health, detail)` — both directions, since the
        /// clearing `Ok` is one of this module's reports too.
        reports: Arc<Mutex<Vec<(String, Health, String)>>>,
        states: Arc<Mutex<Vec<ConnectionState>>>,
    }

    /// What a receiver test varies. `Setup::new()` is the plain harness;
    /// `spawn` builds the store, runner, bus and worker.
    struct Setup {
        coalesce: Duration,
        kind: Arc<dyn DocumentKind>,
        source_time: SourceTime,
        /// The topics the worker is handed at spawn, as the service reads
        /// them from the store.
        known_topics: Vec<String>,
        recover_timeout: Duration,
        /// Wraps the channel subscription so the test scripts recovery.
        script: Option<Arc<Script>>,
        /// A bus the test prepared, e.g. holding a last value already.
        bus: Option<(Arc<ChannelAdapter>, ChannelFeed)>,
        /// Documents published live (as `Published`) before the worker
        /// starts.
        seed: Vec<DocumentRows>,
    }

    impl Setup {
        fn new() -> Self {
            Setup {
                coalesce: Duration::ZERO,
                kind: Arc::new(FakeKind::new()),
                source_time: SourceTime::Receive,
                known_topics: Vec::new(),
                recover_timeout: Duration::from_secs(10),
                script: None,
                bus: None,
                seed: Vec::new(),
            }
        }

        fn spawn(self) -> Harness {
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
            for rows in self.seed {
                let batch = join_key(&rows.key);
                let now = Utc::now();
                ingest.submit_document(DocumentJob {
                    source: "cvi".into(),
                    dataset: ds.name.clone(),
                    rows,
                    source_time: now,
                    received_at: now,
                    bytes: 0,
                    recovered: false,
                    topic: None,
                });
                assert_eq!(published(&events).0, batch, "the seed publishes");
            }

            let (bus, feed) = self.bus.unwrap_or_else(|| ChannelAdapter::new("test_bus"));
            let spec = SourceSpec {
                adapter: "test_bus".into(),
                document: Some("fake_cvi".into()),
                topics: vec!["cvi/>".into()],
                coalesce: self.coalesce,
                source_time: self.source_time,
                recover_timeout: self.recover_timeout,
                ..SourceSpec::directory("cvi", "cvi_params", Vec::new())
            };
            let mut subscription = bus.subscription().expect("the channel adapter subscribes");
            if let Some(script) = &self.script {
                subscription = Box::new(ScriptedSubscription {
                    inner: subscription,
                    script: Arc::clone(script),
                });
            }
            Harness::spawn(
                dir,
                conn,
                events,
                ingest,
                feed,
                &spec,
                ds,
                self.kind,
                subscription,
                self.known_topics,
            )
        }
    }

    fn harness(
        coalesce: Duration,
        kind: Arc<dyn DocumentKind>,
        source_time: SourceTime,
    ) -> Harness {
        Setup {
            coalesce,
            kind,
            source_time,
            ..Setup::new()
        }
        .spawn()
    }

    impl Harness {
        #[allow(clippy::too_many_arguments)]
        fn spawn(
            dir: tempfile::TempDir,
            conn: duckdb::Connection,
            events: Receiver<IngestEvent>,
            ingest: Arc<IngestHandle>,
            feed: ChannelFeed,
            spec: &SourceSpec,
            ds: DatasetSpec,
            kind: Arc<dyn DocumentKind>,
            subscription: Box<dyn Subscription>,
            known_topics: Vec<String>,
        ) -> Harness {
            let reports: Arc<Mutex<Vec<(String, Health, String)>>> =
                Arc::new(Mutex::new(Vec::new()));
            let states: Arc<Mutex<Vec<ConnectionState>>> = Arc::new(Mutex::new(Vec::new()));
            let report_load: LoadReportSink = {
                let reports = Arc::clone(&reports);
                Arc::new(move |batch: &str, health: Health, detail: String| {
                    reports
                        .lock()
                        .unwrap()
                        .push((batch.to_string(), health, detail));
                })
            };
            let on_connection: HealthSink = {
                let states = Arc::clone(&states);
                Arc::new(move |state: ConnectionState| states.lock().unwrap().push(state))
            };
            let worker = SubscriptionWorker::spawn(
                spec,
                ds,
                kind,
                subscription,
                Arc::clone(&ingest),
                report_load,
                on_connection,
                geode_core::clock::Clock::utc(),
                crate::supervise::unwatched(),
                known_topics,
            )
            .expect("the bus is open");
            Harness {
                _dir: dir,
                conn,
                events,
                _ingest: ingest,
                feed,
                worker,
                reports,
                states,
            }
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

    /// Read the next publication, skipping Started and queue-idle announcements
    /// that can occur before submission or between documents.
    fn published(rx: &Receiver<IngestEvent>) -> (String, usize) {
        loop {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(IngestEvent::Published { batch, rows, .. }) => return (batch, rows),
                Ok(IngestEvent::PlanComplete) | Ok(IngestEvent::Started { .. }) => continue,
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
        assert!(h.reports.lock().unwrap().is_empty());
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
            !h.reports.lock().unwrap().is_empty()
        });
        {
            let reports = h.reports.lock().unwrap();
            assert_eq!(
                reports[0].0, "cvi/rubbish",
                "keyed by topic: the bytes never yielded a key"
            );
            assert!(
                matches!(&reports[0].1, Health::Failed { reason } if reason.starts_with("parse: ")),
                "{:?}",
                reports[0].1
            );
            assert!(
                reports[0].2.starts_with("cvi/rubbish: parse: "),
                "{:?}",
                reports[0].2
            );
        }
        // Nothing was published, and the next good message still is.
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        h.worker.shutdown();
    }

    /// The load lane is per-batch and worst-across-batches, and its only
    /// other `Ok` writer is the ingest sink's `Published` arm — keyed by
    /// the document's own batch (`SPX.Z`), never by the topic
    /// (`cvi/SPX.Z`) a failed parse is filed under. So if the receiver did
    /// not clear its own topic entry, a source that recovered from one
    /// malformed message would read `Failed` for the rest of the session
    /// while publishing perfectly good documents.
    #[test]
    fn a_clean_message_clears_the_topic_its_predecessor_failed_under() {
        let mut h = plain_harness();
        h.feed.publish("cvi/SPX.Z", b"not-a-document".to_vec());
        wait_until("the parse failure", || {
            !h.reports.lock().unwrap().is_empty()
        });

        // The same topic, this time parseable.
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        wait_until("the clearing report", || {
            h.reports.lock().unwrap().len() == 2
        });
        {
            let reports = h.reports.lock().unwrap();
            assert!(
                matches!(&reports[0].1, Health::Failed { .. }),
                "{:?}",
                reports[0]
            );
            assert_eq!(
                (reports[1].0.as_str(), &reports[1].1),
                ("cvi/SPX.Z", &Health::Ok),
                "the clearing Ok is filed under the very topic the failure was"
            );
            assert_eq!(reports[1].2, "cvi/SPX.Z: parse ok");
        }
        // The document published too, so the clear is not instead of the
        // ordinary path — and nothing more is reported for it: a topic
        // that has been cleared is forgotten, so a third clean message
        // costs one lookup and no report.
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [7., 7., 7., 7., 7., 7.]),
        );
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        assert_eq!(h.reports.lock().unwrap().len(), 2);
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
            !h.reports.lock().unwrap().is_empty()
        });
        let reports = h.reports.lock().unwrap();
        assert_eq!(reports[0].0, key);
        assert!(reports[0].2.contains("separator"), "{:?}", reports[0].2);
        drop(reports);
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
            !h.reports.lock().unwrap().is_empty()
        });
        let reports = h.reports.lock().unwrap();
        assert_eq!(reports[0].0, "SPX.Z");
        assert!(reports[0].2.contains("nonesuch"), "{:?}", reports[0].2);
        drop(reports);
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

    /// Unsubscribe without setting the stop flag, then wait for the receiver to
    /// exit. This proves no sender clone keeps the channel connected: only the
    /// Disconnected arm can stop this loop. Handling a message first proves the
    /// receiver has started; a bounded wait turns a leaked sender into a failure.
    #[test]
    fn shutting_down_an_idle_worker_does_not_wait_out_max_wait() {
        let mut h = plain_harness();
        // One message handled end to end: evidence the thread is in its
        // loop rather than still starting up, and it restarts the wait.
        h.feed.publish(
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]),
        );
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        h.worker.subscription.unsubscribe();
        wait_until("the receiver thread to end on Disconnected alone", || {
            h.worker.thread.as_ref().is_some_and(|t| t.is_finished())
        });
        h.worker
            .thread
            .take()
            .expect("only `shutdown` ever takes this, and it was not called")
            .join()
            .unwrap();
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
            !h.reports.lock().unwrap().is_empty()
        });
        {
            let reports = h.reports.lock().unwrap();
            assert_eq!(
                reports[0].0, "cvi/SPX.Z",
                "keyed by topic: a panicking parse yielded no key, exactly as an Err does"
            );
            assert!(
                reports[0].2.contains("panicked") && reports[0].2.contains(PARSE_PANIC),
                "the payload a trader needs to see is the panic's own message: {:?}",
                reports[0].2
            );
        }
        // The thread is still there. A second message handled is the only
        // honest evidence: an unwinding receiver thread ends, and its
        // source then latches at whatever health it last reported with
        // nothing ever arriving again.
        h.feed.publish("cvi/NDX.Z", b"anything".to_vec());
        wait_until("the second message to be handled too", || {
            h.reports.lock().unwrap().len() == 2
        });
        assert_eq!(h.reports.lock().unwrap()[1].0, "cvi/NDX.Z");
        nothing_more(&h.events, Duration::from_millis(100));
        h.worker.shutdown();
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
        // joins, and a receiver still waiting on this gate would hold the
        // join for as long as `GATE_CAP` allows.
        {
            let (lock, opened) = &*gate;
            *lock.lock().unwrap() = true;
            opened.notify_all();
        }
        h.worker.shutdown();
    }

    #[test]
    fn a_subscription_whose_queue_fills_reports_its_drops_under_the_queue_key() {
        let (kind, gate) = GateKind::new();
        let mut h = harness(Duration::from_secs(60), kind, SourceTime::Receive);
        let body = FakeKind::message("SPX.Z", [1., 2., 3., 4., 5., 6.]);
        wait_until("a message the subscription could not take", || {
            h.feed.publish("cvi/SPX.Z", body.clone());
            h.worker.refused() > 0
        });
        GateKind::open(&gate);
        wait_until("a drop report", || {
            h.reports
                .lock()
                .unwrap()
                .iter()
                .any(|(batch, _, _)| batch == "cvi:queue")
        });
        let (batch, health, detail) = h
            .reports
            .lock()
            .unwrap()
            .iter()
            .find(|(batch, _, _)| batch == "cvi:queue")
            .cloned()
            .unwrap();
        let Health::Degraded { reason } = health else {
            panic!("a drop is Degraded: {health:?}")
        };
        let (count, time) = reason
            .split_once(" messages dropped since ")
            .unwrap_or_else(|| panic!("{reason}"));
        let count: u64 = count.parse().unwrap();
        assert!(count >= 1 && count <= h.worker.refused(), "{reason}");
        assert_eq!(time.len(), "HH:MM:SS".len(), "{reason}");
        assert_eq!(detail, format!("{batch}: {reason}"));
        h.worker.shutdown();
    }

    #[test]
    fn a_receiver_that_dies_is_declared() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("geode.duckdb")).unwrap();
        let ds = cvi_dataset();
        store.apply_schema(&ds).unwrap();
        Catalog::new(store.writer()).ensure_tables().unwrap();
        let mut schema = SchemaSpec::default();
        schema.datasets.push(ds.clone());
        let (handle, _events) = IngestRunner::spawn_channel(store, schema);
        let (bus, feed) = ChannelAdapter::new("test_bus");
        let spec = SourceSpec {
            adapter: "test_bus".into(),
            document: Some("fake_cvi".into()),
            topics: vec!["cvi/>".into()],
            coalesce: Duration::ZERO,
            ..SourceSpec::directory("cvi", "cvi_params", Vec::new())
        };
        let (stop, stops) = crate::supervise::tests_support::recording();
        let mut worker = SubscriptionWorker::spawn(
            &spec,
            ds,
            Arc::new(PanickingKind::new()),
            bus.subscription().expect("the channel adapter subscribes"),
            Arc::new(handle),
            Arc::new(|_: &str, _: Health, _: String| panic!("the load report fell over")),
            Arc::new(|_: ConnectionState| {}),
            geode_core::clock::Clock::utc(),
            stop,
            Vec::new(),
        )
        .expect("the bus is open");
        feed.publish("cvi/SPX.Z", b"anything".to_vec());
        let (thread, reason) = crate::supervise::tests_support::next_stop(&stops);
        assert_eq!(thread, "geode-subscribe-cvi");
        assert!(reason.contains("the load report fell over"), "{reason}");
        worker.shutdown();
    }

    // ---- recovery, over a real ChannelAdapter ---------------------------

    /// A test transport's GET side: records each request, fails the next
    /// ones the test queued, and sends a reply only when the test calls
    /// `reply` — so a test decides exactly when, and with what, each topic
    /// answers.
    #[derive(Default)]
    struct Script {
        /// The worker's own sink, captured at subscribe and released at
        /// unsubscribe so the receiver can still see its queue disconnect.
        sink: Mutex<Option<MessageSink>>,
        calls: Mutex<Vec<Vec<String>>>,
        failures: Mutex<std::collections::VecDeque<String>>,
    }

    impl Script {
        fn reply(&self, topic: &str, bytes: Vec<u8>) {
            let sink = self.sink.lock().unwrap().clone().expect("subscribed");
            assert!(sink.push(Message {
                topic: topic.to_string(),
                received: Utc::now(),
                bytes,
                recovered: true,
            }));
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    struct ScriptedSubscription {
        inner: Box<dyn Subscription>,
        script: Arc<Script>,
    }

    impl Subscription for ScriptedSubscription {
        fn subscribe(
            &mut self,
            topics: &[String],
            sink: MessageSink,
            health: HealthSink,
        ) -> Result<(), AdapterError> {
            *self.script.sink.lock().unwrap() = Some(sink.clone());
            self.inner.subscribe(topics, sink, health)
        }

        fn unsubscribe(&mut self) {
            self.script.sink.lock().unwrap().take();
            self.inner.unsubscribe();
        }

        fn recovery(&mut self) -> Option<Box<dyn Recovery>> {
            Some(Box::new(ScriptedRecovery(Arc::clone(&self.script))))
        }
    }

    struct ScriptedRecovery(Arc<Script>);

    impl Recovery for ScriptedRecovery {
        fn recover(&mut self, topics: &[String], _timeout: Duration) -> Result<(), AdapterError> {
            self.0.calls.lock().unwrap().push(topics.to_vec());
            match self.0.failures.lock().unwrap().pop_front() {
                Some(message) => Err(AdapterError { message }),
                None => Ok(()),
            }
        }
    }

    fn scripted() -> (Setup, Arc<Script>) {
        let script = Arc::new(Script::default());
        let setup = Setup {
            script: Some(Arc::clone(&script)),
            ..Setup::new()
        };
        (setup, script)
    }

    /// Puts `bytes` on the bus as `topic`'s last value before the worker
    /// exists. A throwaway subscriber starts the dispatcher (which records
    /// last values) and its receipt proves the value is recorded.
    fn publish_before_subscribers(
        bus: &Arc<ChannelAdapter>,
        feed: &ChannelFeed,
        topic: &str,
        bytes: Vec<u8>,
    ) {
        let mut sub = bus.subscription().unwrap();
        let (sink, rx) = MessageSink::bounded(4);
        sub.subscribe(&["cvi/>".into()], sink, Arc::new(|_: ConnectionState| {}))
            .unwrap();
        assert!(feed.publish(topic, bytes));
        rx.recv_timeout(Duration::from_secs(30))
            .expect("the throwaway subscriber hears it");
        sub.unsubscribe();
    }

    /// The next ingest outcome, skipping run announcements.
    fn outcome(rx: &Receiver<IngestEvent>) -> IngestEvent {
        loop {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(IngestEvent::PlanComplete) | Ok(IngestEvent::Started { .. }) => continue,
                Ok(other) => return other,
                Err(e) => panic!("no outcome: {e}"),
            }
        }
    }

    fn recovery_reports(h: &Harness) -> Vec<(Health, String)> {
        h.reports
            .lock()
            .unwrap()
            .iter()
            .filter(|(batch, _, _)| batch == "cvi:recovery")
            .map(|(_, health, detail)| (health.clone(), detail.clone()))
            .collect()
    }

    fn fake_rows(key: &str, params: [f64; 6]) -> DocumentRows {
        FakeKind::new()
            .parse(&FakeKind::message(key, params))
            .unwrap()
            .rows
    }

    /// Drop the connection and bring it back, as a transport reports it.
    fn reconnect(h: &Harness) {
        h.feed.set_state(ConnectionState::Lost {
            reason: "broker gone".into(),
        });
        h.feed.set_state(ConnectionState::Connected);
    }

    #[test]
    fn recovery_publishes_the_last_document_of_a_known_topic_at_start() {
        let (bus, feed) = ChannelAdapter::new("test_bus");
        publish_before_subscribers(
            &bus,
            &feed,
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [0.5; 6]),
        );
        let mut h = Setup {
            bus: Some((bus, feed)),
            known_topics: vec!["cvi/SPX.Z".into()],
            ..Setup::new()
        }
        .spawn();
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        assert_eq!(live_params(&h.conn, "SPX.Z"), vec![0.5; 6]);
        wait_until("the recovery's Ok", || {
            recovery_reports(&h)
                .iter()
                .any(|(health, _)| *health == Health::Ok)
        });
        h.worker.shutdown();
    }

    #[test]
    fn a_reply_equal_to_live_publishes_nothing() {
        let (bus, feed) = ChannelAdapter::new("test_bus");
        publish_before_subscribers(
            &bus,
            &feed,
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [0.5; 6]),
        );
        let mut h = Setup {
            bus: Some((bus, feed)),
            known_topics: vec!["cvi/SPX.Z".into()],
            seed: vec![fake_rows("SPX.Z", [0.5; 6])],
            ..Setup::new()
        }
        .spawn();
        match outcome(&h.events) {
            IngestEvent::Unchanged { source, batch, .. } => {
                assert_eq!((source.as_str(), batch.as_str()), ("cvi", "SPX.Z"));
            }
            other => panic!("expected Unchanged, got {other:?}"),
        }
        nothing_more(&h.events, Duration::from_millis(200));
        assert_eq!(live_params(&h.conn, "SPX.Z"), vec![0.5; 6]);
        h.worker.shutdown();
    }

    /// `topic`'s `last_received_us`, or `None` when it is not recorded.
    fn topic_last_received(conn: &duckdb::Connection, topic: &str) -> Option<i64> {
        conn.query_row(
            "select last_received_us from subscription_topics \
             where source = 'cvi' and topic = ?",
            [topic],
            |r| r.get(0),
        )
        .ok()
    }

    /// An equal reply proves its topic alive: it is recorded although
    /// nothing publishes. It must not spend the run's NOTIFY record slot,
    /// so the topic's first NOTIFY still carries it and moves the record
    /// to that NOTIFY's receive time.
    #[test]
    fn an_equal_reply_records_its_topic_and_leaves_the_first_notify_to_record_it_again() {
        let (bus, feed) = ChannelAdapter::new("test_bus");
        publish_before_subscribers(
            &bus,
            &feed,
            "cvi/SPX.Z",
            FakeKind::message("SPX.Z", [0.5; 6]),
        );
        let mut h = Setup {
            bus: Some((bus, feed)),
            known_topics: vec!["cvi/SPX.Z".into()],
            // Seeded with no topic, so any record comes from the reply.
            seed: vec![fake_rows("SPX.Z", [0.5; 6])],
            ..Setup::new()
        }
        .spawn();
        assert!(matches!(outcome(&h.events), IngestEvent::Unchanged { .. }));
        let by_reply =
            topic_last_received(&h.conn, "cvi/SPX.Z").expect("the equal reply recorded its topic");

        // Later by at least a publish round trip, so a NOTIFY record moves
        // the receive time past the reply's.
        std::thread::sleep(Duration::from_millis(5));
        h.feed
            .publish("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.6; 6]));
        assert_eq!(published(&h.events).0, "SPX.Z");
        let by_notify = topic_last_received(&h.conn, "cvi/SPX.Z").unwrap();
        assert!(
            by_notify > by_reply,
            "the first NOTIFY carried its topic: {by_notify} > {by_reply}"
        );
        h.worker.shutdown();
    }

    /// The NOTIFY for SPX.Z arrives after the connection went down and is
    /// handled before the receiver learns of the reconnect. The window
    /// starts at the disconnect, so that NOTIFY counts against the older
    /// reply; a window started when the receiver noticed would let the
    /// reply replace it.
    #[test]
    fn a_notify_processed_before_the_reconnect_is_noticed_still_beats_the_reply() {
        let (setup, script) = scripted();
        let mut h = setup.spawn();
        assert!(
            script.calls().is_empty(),
            "no known topics, no recovery at start"
        );
        h.feed
            .publish("cvi/NDX.Z", FakeKind::message("NDX.Z", [1.0; 6]));
        assert_eq!(published(&h.events).0, "NDX.Z");

        h.feed.set_state(ConnectionState::Lost {
            reason: "broker gone".into(),
        });
        h.feed
            .publish("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.9; 6]));
        assert_eq!(published(&h.events).0, "SPX.Z");
        h.feed.set_state(ConnectionState::Connected);

        wait_until("the reconnect's recovery", || script.calls().len() == 1);
        assert_eq!(
            script.calls()[0],
            vec!["cvi/NDX.Z".to_string(), "cvi/SPX.Z".to_string()],
            "every topic seen this run, sorted"
        );
        // The older SPX.Z state, then an NDX.Z reply whose publish proves
        // the SPX.Z one was handled first.
        script.reply("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.1; 6]));
        script.reply("cvi/NDX.Z", FakeKind::message("NDX.Z", [2.0; 6]));
        assert_eq!(
            published(&h.events).0,
            "NDX.Z",
            "NDX.Z was last notified before the disconnect, so its reply publishes; SPX.Z's does not"
        );
        assert_eq!(live_params(&h.conn, "SPX.Z"), vec![0.9; 6]);
        assert_eq!(live_params(&h.conn, "NDX.Z"), vec![2.0; 6]);
        nothing_more(&h.events, Duration::from_millis(200));
        h.worker.shutdown();
    }

    /// The replacing NOTIFY repeats the live document. Published without
    /// comparison it is a `Published`; had it inherited the reply's
    /// recovered mark it would compare equal and report `Unchanged`.
    #[test]
    fn a_notify_replacing_a_pending_reply_publishes_without_comparison() {
        let (mut setup, script) = scripted();
        setup.coalesce = Duration::from_secs(1);
        let mut h = setup.spawn();
        h.feed
            .publish("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.1; 6]));
        assert_eq!(published(&h.events).0, "SPX.Z");

        reconnect(&h);
        wait_until("the reconnect's recovery", || script.calls().len() == 1);
        // Inside SPX.Z's spacing window: the reply goes pending, and the
        // NOTIFY replaces it before the release.
        script.reply("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.2; 6]));
        h.feed
            .publish("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.1; 6]));
        assert_eq!(published(&h.events), ("SPX.Z".to_string(), 6));
        assert_eq!(live_params(&h.conn, "SPX.Z"), vec![0.1; 6]);
        nothing_more(&h.events, Duration::from_millis(200));
        h.worker.shutdown();
    }

    #[test]
    fn a_failed_recovery_degrades_the_recovery_key_and_the_next_success_clears_it() {
        let (mut setup, script) = scripted();
        setup.known_topics = vec!["cvi/SPX.Z".into()];
        script
            .failures
            .lock()
            .unwrap()
            .push_back("broker said no".into());
        let mut h = setup.spawn();
        wait_until("the failed recovery's report", || {
            !recovery_reports(&h).is_empty()
        });
        assert_eq!(
            recovery_reports(&h),
            vec![(
                Health::Degraded {
                    reason: "recovery failed: broker said no".into()
                },
                "cvi:recovery: recovery failed: broker said no".into()
            )]
        );

        reconnect(&h);
        wait_until("the second recovery", || script.calls().len() == 2);
        script.reply("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.3; 6]));
        assert_eq!(published(&h.events).0, "SPX.Z");
        wait_until("the clearing report", || recovery_reports(&h).len() == 2);
        assert_eq!(recovery_reports(&h)[1].0, Health::Ok);
        h.worker.shutdown();
    }

    #[test]
    fn no_replies_degrades_but_a_partial_answer_stays_ok() {
        let (mut setup, script) = scripted();
        setup.known_topics = vec!["cvi/NDX.Z".into(), "cvi/SPX.Z".into()];
        // A zero timeout leaves the grace second as the whole window.
        setup.recover_timeout = Duration::ZERO;
        let mut h = setup.spawn();
        wait_until("the unanswered window's report", || {
            !recovery_reports(&h).is_empty()
        });
        assert_eq!(
            recovery_reports(&h)[0].0,
            Health::Degraded {
                reason: "recovery: no replies for 2 topics".into()
            }
        );

        reconnect(&h);
        wait_until("the second recovery", || script.calls().len() == 2);
        script.reply("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.3; 6]));
        assert_eq!(published(&h.events).0, "SPX.Z");
        wait_until("the partial window's report", || {
            recovery_reports(&h).len() == 2
        });
        assert_eq!(
            recovery_reports(&h)[1].0,
            Health::Ok,
            "one of two topics answered: retired instruments must not hold a source degraded"
        );
        h.worker.shutdown();
    }

    /// The demo launch: the bus holds nothing yet, so recovery is never
    /// answered, but the producers' startup burst notifies the known topic
    /// inside the window. The source is live; reporting "no replies" would
    /// hold it Degraded for the session.
    #[test]
    fn a_notify_inside_an_unanswered_window_keeps_recovery_ok() {
        let (mut setup, script) = scripted();
        setup.known_topics = vec!["cvi/SPX.Z".into()];
        // A zero timeout leaves the grace second as the whole window.
        setup.recover_timeout = Duration::ZERO;
        let mut h = setup.spawn();
        wait_until("the recovery request", || script.calls().len() == 1);
        h.feed
            .publish("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.4; 6]));
        assert_eq!(published(&h.events).0, "SPX.Z");
        wait_until("the window's report", || !recovery_reports(&h).is_empty());
        assert_eq!(
            recovery_reports(&h)[0].0,
            Health::Ok,
            "{:?}",
            recovery_reports(&h)
        );
        h.worker.shutdown();
    }

    #[test]
    fn shutdown_during_a_recovery_window_reports_nothing() {
        let (mut setup, script) = scripted();
        setup.known_topics = vec!["cvi/SPX.Z".into()];
        setup.recover_timeout = Duration::from_secs(60);
        let mut h = setup.spawn();
        wait_until("the recovery request", || script.calls().len() == 1);
        let asked = Instant::now();
        h.worker.shutdown();
        assert!(
            asked.elapsed() < Duration::from_secs(1),
            "an open window does not hold shutdown: {:?}",
            asked.elapsed()
        );
        assert!(
            recovery_reports(&h).is_empty(),
            "{:?}",
            recovery_reports(&h)
        );
    }

    #[test]
    fn a_topics_first_document_per_run_is_recorded_once() {
        let mut h = plain_harness();
        h.feed
            .publish("cvi/SPX.Z", FakeKind::message("SPX.Z", [1.0; 6]));
        assert_eq!(published(&h.events).0, "SPX.Z");
        h.feed
            .publish("cvi/NDX.Z", FakeKind::message("NDX.Z", [1.0; 6]));
        assert_eq!(published(&h.events).0, "NDX.Z");
        // Later by at least a publish round trip, so a second record would
        // move SPX.Z's last receive time past its first sighting.
        std::thread::sleep(Duration::from_millis(5));
        h.feed
            .publish("cvi/SPX.Z", FakeKind::message("SPX.Z", [2.0; 6]));
        assert_eq!(published(&h.events).0, "SPX.Z");
        let rows: Vec<(String, i64, i64)> = h
            .conn
            .prepare(
                "select topic, first_seen_us, last_received_us from subscription_topics \
                 where source = 'cvi' order by topic",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            vec!["cvi/NDX.Z", "cvi/SPX.Z"]
        );
        for (topic, first, last) in &rows {
            assert_eq!(
                first, last,
                "{topic} was recorded by its first document only"
            );
        }
        h.worker.shutdown();
    }

    // ---- recovery: reconnect starts, superseded windows, stray replies --

    /// Two outages before the receiver looks: the second outage's start
    /// is the one handed over.
    #[test]
    fn a_second_outage_before_the_receiver_looks_hands_over_the_latest_disconnect() {
        let r = Reconnect::default();
        let lost = || ConnectionState::Lost {
            reason: "broker gone".into(),
        };
        r.observe(&lost());
        let t1 = r.down_at.load(Ordering::Acquire);
        r.observe(&ConnectionState::Connected);
        assert_eq!(r.reconnected_at.load(Ordering::Acquire), t1);
        std::thread::sleep(Duration::from_millis(2));
        r.observe(&ConnectionState::Reconnecting);
        r.observe(&lost());
        let t2 = r.down_at.load(Ordering::Acquire);
        assert!(t2 > t1, "the second outage starts later");
        r.observe(&ConnectionState::Connected);
        assert_eq!(
            r.reconnected_at.load(Ordering::Acquire),
            t2,
            "a window starting at the first outage would let a NOTIFY between the two drop the reply"
        );
        assert_eq!(r.down_at.load(Ordering::Acquire), 0);
    }

    /// A gated kind that says when a parse is parked on its gate, so a test
    /// knows the receiver is busy rather than at its loop top.
    struct HoldKind {
        inner: GateKind,
        entered: std::sync::atomic::AtomicBool,
    }

    impl DocumentKind for HoldKind {
        fn name(&self) -> &'static str {
            self.inner.name()
        }

        fn columns(&self) -> &[(&'static str, ColumnType)] {
            self.inner.columns()
        }

        fn parse(
            &self,
            bytes: &[u8],
        ) -> Result<geode_core::document::ParsedDocument, geode_core::document::ParseError>
        {
            self.entered.store(true, Ordering::Release);
            self.inner.parse(bytes)
        }

        fn write(&self, rows: &DocumentRows) -> Result<Vec<u8>, geode_core::document::WriteError> {
            self.inner.write(rows)
        }
    }

    /// NOTIFY X for SPX.Z arrives between two outages; the update the
    /// second outage missed comes back as a reply. The window starts at the
    /// second disconnect, so X (older) does not beat the reply.
    #[test]
    fn a_reply_to_a_second_outage_beats_a_notify_from_between_the_outages() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let kind = Arc::new(HoldKind {
            inner: GateKind {
                inner: FakeKind::new(),
                gate: Arc::clone(&gate),
            },
            entered: std::sync::atomic::AtomicBool::new(false),
        });
        let (mut setup, script) = scripted();
        setup.kind = kind.clone();
        let mut h = setup.spawn();
        // D parks the receiver inside its parse for the whole sequence.
        h.feed
            .publish("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.1; 6]));
        wait_until("the receiver parked in a parse", || {
            kind.entered.load(Ordering::Acquire)
        });
        h.feed.set_state(ConnectionState::Lost {
            reason: "first".into(),
        });
        h.feed.set_state(ConnectionState::Connected);
        std::thread::sleep(Duration::from_millis(2));
        h.feed
            .publish("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.5; 6]));
        std::thread::sleep(Duration::from_millis(2));
        h.feed.set_state(ConnectionState::Lost {
            reason: "second".into(),
        });
        h.feed.set_state(ConnectionState::Connected);
        GateKind::open(&gate);

        assert_eq!(published(&h.events).0, "SPX.Z", "D");
        wait_until("the reconnect's recovery", || script.calls().len() == 1);
        assert_eq!(published(&h.events).0, "SPX.Z", "X");
        script.reply("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.9; 6]));
        script.reply("cvi/NDX.Z", FakeKind::message("NDX.Z", [2.0; 6]));
        assert_eq!(
            published(&h.events).0,
            "SPX.Z",
            "the reply carrying the missed update publishes before NDX.Z's"
        );
        assert_eq!(live_params(&h.conn, "SPX.Z"), vec![0.9; 6]);
        h.worker.shutdown();
    }

    #[test]
    fn a_superseded_recovery_window_reports_nothing() {
        let (mut setup, script) = scripted();
        setup.known_topics = vec!["cvi/SPX.Z".into()];
        setup.recover_timeout = Duration::from_secs(60);
        let mut h = setup.spawn();
        wait_until("the start's recovery", || script.calls().len() == 1);
        reconnect(&h);
        wait_until("the reconnect's recovery", || script.calls().len() == 2);
        script.reply("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.3; 6]));
        assert_eq!(published(&h.events).0, "SPX.Z");
        wait_until("the second window's report", || {
            !recovery_reports(&h).is_empty()
        });
        h.worker.shutdown();
        let reports = recovery_reports(&h);
        assert_eq!(
            reports.len(),
            1,
            "only the finished window reports: {reports:?}"
        );
        assert_eq!(reports[0].0, Health::Ok);
    }

    #[test]
    fn a_recovered_message_with_no_window_open_never_publishes() {
        // No known topics: no recovery runs and no window opens.
        let (setup, script) = scripted();
        let mut h = setup.spawn();
        script.reply("cvi/SPX.Z", FakeKind::message("SPX.Z", [0.4; 6]));
        h.feed
            .publish("cvi/NDX.Z", FakeKind::message("NDX.Z", [1.0; 6]));
        assert_eq!(
            published(&h.events).0,
            "NDX.Z",
            "the stray reply ahead of it was dropped"
        );
        assert!(live_params(&h.conn, "SPX.Z").is_empty());
        assert!(script.calls().is_empty());
        h.worker.shutdown();
    }
}
