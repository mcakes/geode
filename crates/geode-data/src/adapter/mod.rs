//! Transport capabilities for subscription, upload, and on-demand history.
//! The application registers adapters by name; the data service resolves the
//! capability each source requires. ChannelAdapter provides an in-process bus.
//!
//! MessageSink uses bounded try_send: refusal drops and counts the message
//! without waiting for queue space. Acceptance means queued, not parsed or
//! stored. Connection callbacks and blocking fetch calls have separate
//! contracts below. See `docs/current/data-path.md`.

pub mod channel;
pub mod topic;

pub use channel::{ChannelAdapter, ChannelFeed};
pub use topic::topic_matches;

use chrono::{DateTime, Utc};
use geode_core::reference::TableRows;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};

/// One message, as it arrived.
///
/// `bytes` is the whole document body, unparsed — the adapter tier knows
/// nothing about formats; the receiver thread hands these to a
/// `DocumentKind`. `received` is stamped at arrival rather than read from
/// the body because it is also a `source_time` policy in its own right
/// (`SourceTime::Receive`), and a body's own timestamp field is the other
/// one.
///
/// `recovered` marks a reply to [`Recovery::recover`]: the transport's
/// latest document on that topic, which may repeat one already delivered.
/// Every ordinary delivery is `false`.
#[derive(Debug, Clone)]
pub struct Message {
    pub topic: String,
    pub received: DateTime<Utc>,
    pub bytes: Vec<u8>,
    pub recovered: bool,
}

/// Capacity of each subscription queue and the ChannelAdapter inbound queue.
/// Each hop can refuse independently. This absorbs bursts but neither bounds
/// the downstream ingest backlog nor guarantees delivery under sustained load.
pub const MESSAGE_BOUND: usize = 256;

/// Bounded message admission. A successful push queues the message; it does
/// not acknowledge parsing or storage. Full and disconnected queues both
/// return false and increment the shared refusal counter. Producers must
/// handle refusal without blocking their callback thread or stopping unrelated
/// subscriptions.
///
/// Clones share both the sender and counter. A retained clone keeps the
/// receiver connected; use refused_counter for monitoring without a sender.
#[derive(Clone)]
pub struct MessageSink {
    tx: SyncSender<Message>,
    /// Shared with every clone. `Relaxed` throughout: this is a
    /// diagnostic counter, and nothing orders any other memory on it.
    refused: Arc<AtomicU64>,
}

impl MessageSink {
    /// A sink and its receiving end. `capacity` is the queue depth —
    /// [`MESSAGE_BOUND`] in the service, a small number in tests that want
    /// to observe a refusal.
    pub fn bounded(capacity: usize) -> (MessageSink, Receiver<Message>) {
        let (tx, rx) = mpsc::sync_channel(capacity);
        (
            MessageSink {
                tx,
                refused: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    /// Offers `m` to the consumer. `true` if it was queued; `false`, with
    /// the refusal counted, if the queue is full or the receiver is gone.
    /// Never blocks.
    pub fn push(&self, m: Message) -> bool {
        match self.tx.try_send(m) {
            Ok(()) => true,
            Err(_) => {
                self.refused.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// How many messages this sink has dropped over its whole life.
    /// Shared across clones, so the count is the subscription's rather
    /// than one holder's.
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    /// Read refusals without retaining a sender. Keeping a MessageSink clone
    /// solely for metrics would prevent unsubscribe from disconnecting an idle
    /// receiver; this counter does not affect channel liveness.
    pub fn refused_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.refused)
    }
}

/// Adapter connection status. DataService maps Connected to Ok, Reconnecting
/// to Pending, and Lost to Failed in the discovery lane. These reports do not
/// establish that every message was delivered or that stored content is clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Reconnecting,
    Lost { reason: String },
}

/// Connection callback on the reporting thread. It must return promptly,
/// avoid reentering the adapter, and not panic. There is no delivery verdict;
/// the service combines reports with load health. Implementations must define
/// their notification ordering rather than relying on a queue here.
pub type HealthSink = Arc<dyn Fn(ConnectionState) + Send + Sync>;

/// Why an adapter refused. A plain message rather than an enum: every
/// caller either logs it or forwards it into a health lane's `reason`, and
/// a vendor library's failures are not a set this crate can enumerate
/// ahead of time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError {
    pub message: String,
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AdapterError {}

/// One source's subscription handle. Send permits transferring ownership;
/// methods require exclusive access, without requiring Sync. The service owns
/// this handle beside the receiver thread so it can unsubscribe before joining.
pub trait Subscription: Send {
    /// Starts delivering messages on any of `topics` to `sink`, and
    /// connection transitions to `health`. An adapter reports its current
    /// state through `health` as part of subscribing, so a caller never
    /// has to poll for "am I connected".
    fn subscribe(
        &mut self,
        topics: &[String],
        sink: MessageSink,
        health: HealthSink,
    ) -> Result<(), AdapterError>;

    /// Stop the subscription. Idempotent and infallible; release retained message
    /// sinks so receivers can observe disconnection. ChannelAdapter can still
    /// finish delivery from a snapshot taken before unsubscribe.
    fn unsubscribe(&mut self);

    /// The recovery side, asked for once after `subscribe` returns `Ok` and
    /// owned by the receiver thread. `None`: this transport cannot recover.
    fn recovery(&mut self) -> Option<Box<dyn Recovery>> {
        None
    }
}

/// Asks the transport for its latest document per topic, at start and
/// after a reconnect, so a document published while nobody listened is not
/// missed until its next update.
pub trait Recovery: Send {
    /// Ask for the latest document on each of `topics` (concrete NOTIFY
    /// topics, never patterns). Replies arrive on the subscription's sink
    /// under the NOTIFY topic with `recovered = true`. `timeout` travels
    /// with each request. Returns promptly: `Ok` means requested, not
    /// answered. A topic with nothing to send is simply not answered.
    fn recover(
        &mut self,
        topics: &[String],
        timeout: std::time::Duration,
    ) -> Result<(), AdapterError>;
}

/// Optional upload capability, independent of subscription and fetch.
/// Transport implementations define what successful upload acknowledges.
pub trait Egress: Send {
    fn upload(&mut self, target: &str, bytes: Vec<u8>) -> Result<(), AdapterError>;
}

/// Optional command capability for the position system, independent of the
/// other capabilities. One worker owns it and runs one command at a time.
/// `Ok` means the position system accepted the command; the grid learns of
/// the move only from a later snapshot. A refusal's message is shown to the
/// user as the reason. The worker sets no deadline: an implementation must
/// bound its own calls, or a hung call holds every later command.
pub trait PositionCommands: Send {
    /// Move every one of `positions` to LHU `lhu`. All or nothing with
    /// respect to validation: a refusal found before writing changes
    /// nothing. A write that fails part-way can leave earlier writes in
    /// place; the `Err` then says so only as far as its message does.
    fn move_lhu(&mut self, positions: &[String], lhu: &str) -> Result<(), AdapterError>;
}

/// History request for one adapter-defined identity over the half-open span
/// `from <= ts < to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub identity: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// Columnar fetch response with equal lengths and strictly ascending times.
/// The worker validates alignment/order before dropping non-finite values;
/// the append path reads matching timestamp/value indexes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SeriesRows {
    pub ts: Vec<DateTime<Utc>>,
    pub value: Vec<f64>,
}

impl SeriesRows {
    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }

    /// Equal lengths and strictly ascending `ts`. A repeated timestamp is
    /// refused rather than resolved: which value wins is the source's
    /// question, not this crate's.
    pub fn validate(&self) -> Result<(), AdapterError> {
        if self.ts.len() != self.value.len() {
            return Err(AdapterError {
                message: format!(
                    "{} values for {} timestamps",
                    self.value.len(),
                    self.ts.len()
                ),
            });
        }
        if let Some(i) = (1..self.ts.len()).find(|&i| self.ts[i] <= self.ts[i - 1]) {
            return Err(AdapterError {
                message: format!(
                    "timestamps must be strictly ascending; row {i} ({}) follows {}",
                    self.ts[i],
                    self.ts[i - 1]
                ),
            });
        }
        Ok(())
    }

    /// Removes rows whose value is NaN or infinite, keeping the two arrays
    /// aligned. Returns how many were dropped, for the warning line.
    pub fn drop_non_finite(&mut self) -> usize {
        let before = self.ts.len();
        let mut keep = self.value.iter().map(|v| v.is_finite());
        self.ts.retain(|_| keep.next().unwrap_or(false));
        self.value.retain(|v| v.is_finite());
        before - self.ts.len()
    }
}

/// Blocking history capability owned by one fetch worker. Methods require
/// exclusive access; Send allows ownership transfer without requiring Sync.
///
/// Implementations must bound both fetch and catalogue calls with their own
/// timeouts. The worker provides no deadline or cancellation and drains its
/// accepted queue before joining at shutdown. A call that never returns can
/// therefore prevent application shutdown.
pub trait Fetch: Send {
    fn fetch(&mut self, req: &FetchRequest) -> Result<SeriesRows, AdapterError>;

    /// Identities this source can name, for typeahead. `None` when the
    /// source cannot enumerate (a REST endpoint). Called once at open and
    /// on `Request::Identities`.
    fn catalogue(&mut self) -> Option<Vec<String>>;
}

/// Blocking whole-table read owned by one snapshot worker. Implementations
/// bound `query` with their own timeout: the worker has no deadline, and a
/// call that never returns stops polling for that source and can block shutdown.
pub trait SnapshotQuery: Send {
    fn query(&mut self, table: &str) -> Result<TableRows, AdapterError>;
}

/// Named transport shared through Arc. Optional capabilities are independent:
/// None means unavailable, while a returned capability may still refuse an
/// operation. Availability can change; for example, a closed ChannelAdapter
/// cannot create new egress handles, and its subscriptions reject subscribe.
pub trait Adapter: Send + Sync {
    /// Name selected by a source's `adapter` setting in sources.toml.
    fn name(&self) -> &'static str;

    /// A FRESH subscription each call — one per subscribed source, with
    /// its own topics, sink and identity, so unsubscribing one never
    /// touches another. `None` if this adapter cannot subscribe at all.
    fn subscription(&self) -> Option<Box<dyn Subscription>>;

    /// The upload side, or `None` if this adapter has none. Each call
    /// returns a FRESH handle, like `subscription`: `resolve` probes one to
    /// check availability and discards it, then `EgressWorkers::spawn` takes
    /// another to own for the life of the worker thread.
    fn egress(&self) -> Option<Box<dyn Egress>>;

    /// On-demand history capability. The default returns None for adapters that
    /// provide no fetch implementation.
    fn fetch(&self) -> Option<Box<dyn Fetch>> {
        None
    }

    /// The position-command side, or `None` (the default) if this adapter
    /// has none. Each call returns a FRESH handle, like `egress`:
    /// `positions::resolve` probes one and discards it, then
    /// `PositionWorker::spawn` takes another for its worker thread.
    fn positions(&self) -> Option<Box<dyn PositionCommands>> {
        None
    }

    /// A FRESH handle per call, like `fetch`. `None` (the default) when the
    /// adapter cannot read tables.
    fn snapshot(&self) -> Option<Box<dyn SnapshotQuery>> {
        None
    }
}

/// Adapters keyed by name, registered by geode-app at startup. A configured
/// source naming an absent adapter is reported as unservable by the service;
/// other sources remain usable.
#[derive(Default, Clone)]
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn Adapter>>,
}

impl AdapterRegistry {
    /// Register by name. A duplicate replaces the previous adapter and warns so
    /// transport selection cannot change silently.
    pub fn register(&mut self, adapter: Arc<dyn Adapter>) {
        let name = adapter.name().to_string();
        if self.adapters.insert(name.clone(), adapter).is_some() {
            tracing::warn!(
                target: "geode::ingest",
                "adapter '{name}' registered twice; the later registration wins"
            );
        }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Adapter>> {
        self.adapters.get(name).cloned()
    }

    /// Sorted, so a diagnostic listing the adapters this build has reads
    /// the same way twice.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.adapters.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::channel::ChannelAdapter;
    use chrono::Utc;

    #[test]
    fn a_full_sink_refuses_and_counts_rather_than_blocking() {
        let (sink, rx) = MessageSink::bounded(2);
        let m = |t: &str| Message {
            topic: t.into(),
            received: Utc::now(),
            bytes: vec![],
            recovered: false,
        };
        assert!(sink.push(m("a")) && sink.push(m("b")));
        assert!(!sink.push(m("c")));
        assert_eq!(sink.refused(), 1);
        drop(rx);
        assert!(!sink.push(m("d")));
        assert_eq!(sink.refused(), 2);
    }

    #[test]
    fn the_registry_finds_adapters_by_name_and_a_replacement_wins() {
        let (cvi_bus, cvi_feed) = ChannelAdapter::new("cvi_bus");
        let (repo_bus, repo_feed) = ChannelAdapter::new("repo_bus");
        let mut registry = AdapterRegistry::default();
        registry.register(cvi_bus.clone());
        registry.register(repo_bus.clone());

        let found = registry.get("cvi_bus").expect("registered");
        assert_eq!(found.name(), "cvi_bus");
        assert!(Arc::ptr_eq(&found, &(cvi_bus.clone() as Arc<dyn Adapter>)));
        assert_eq!(registry.get("repo_bus").unwrap().name(), "repo_bus");
        assert!(registry.get("nonesuch").is_none());
        assert_eq!(
            registry.names(),
            vec!["cvi_bus".to_string(), "repo_bus".to_string()]
        );

        let (replacement, replacement_feed) = ChannelAdapter::new("cvi_bus");
        registry.register(replacement.clone());
        let found = registry.get("cvi_bus").expect("still registered");
        assert!(Arc::ptr_eq(
            &found,
            &(replacement.clone() as Arc<dyn Adapter>)
        ));
        assert!(!Arc::ptr_eq(&found, &(cvi_bus as Arc<dyn Adapter>)));
        assert_eq!(
            registry.names(),
            vec!["cvi_bus".to_string(), "repo_bus".to_string()]
        );
        let _ = (cvi_feed, repo_feed, replacement_feed);
    }

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn series_rows_validate_lengths_and_order() {
        let ok = SeriesRows {
            ts: vec![t("2026-01-01T00:00:00Z"), t("2026-01-01T00:01:00Z")],
            value: vec![1.0, 2.0],
        };
        assert!(ok.validate().is_ok());
        let unequal = SeriesRows {
            ts: vec![t("2026-01-01T00:00:00Z")],
            value: vec![1.0, 2.0],
        };
        assert!(
            unequal
                .validate()
                .unwrap_err()
                .message
                .contains("2 values for 1 timestamp")
        );
        let unsorted = SeriesRows {
            ts: vec![t("2026-01-01T00:01:00Z"), t("2026-01-01T00:00:00Z")],
            value: vec![1.0, 2.0],
        };
        assert!(
            unsorted
                .validate()
                .unwrap_err()
                .message
                .contains("ascending")
        );
        let dup = SeriesRows {
            ts: vec![t("2026-01-01T00:00:00Z"), t("2026-01-01T00:00:00Z")],
            value: vec![1.0, 2.0],
        };
        assert!(
            dup.validate().unwrap_err().message.contains("ascending"),
            "a repeated ts is not strictly ascending"
        );
    }

    #[test]
    fn drop_non_finite_keeps_the_arrays_aligned() {
        let mut rows = SeriesRows {
            ts: vec![
                t("2026-01-01T00:00:00Z"),
                t("2026-01-01T00:01:00Z"),
                t("2026-01-01T00:02:00Z"),
            ],
            value: vec![1.0, f64::NAN, f64::INFINITY],
        };
        assert_eq!(rows.drop_non_finite(), 2);
        assert_eq!(rows.ts, vec![t("2026-01-01T00:00:00Z")]);
        assert_eq!(rows.value, vec![1.0]);
    }

    #[test]
    fn an_adapter_has_no_fetch_side_by_default() {
        let (adapter, _feed) = ChannelAdapter::new("demo_bus");
        assert!(adapter.fetch().is_none());
    }
}
