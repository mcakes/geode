//! Transport capabilities for subscription, upload, on-demand history, and
//! keyed history.
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
use geode_core::query::{KeySet, Span, normalize_key};
use geode_core::reference::TableRows;
use geode_core::schema::ColumnType;
use std::collections::{HashMap, HashSet};
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

/// One keyed history ask: the rows of `table` whose key falls inside `keys`
/// over `span`.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRequest {
    pub table: String,
    pub keys: KeySet,
    pub span: Span,
}

/// Columnar answer: `keys[c][i]` is row i's value for key column c (f64 keys
/// as text), `values[v][i]` row i's value column v.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HistoryRows {
    pub ts: Vec<DateTime<Utc>>,
    pub keys: Vec<Vec<String>>,
    pub values: Vec<Vec<f64>>,
}

impl HistoryRows {
    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }

    /// Row i's key tuple.
    pub fn key_at(&self, i: usize) -> Vec<String> {
        self.keys.iter().map(|c| c[i].clone()).collect()
    }

    /// Rewrites every key value to its canonical spelling, column `c`
    /// through `key_types[c]`. Must run before `validate_against`: a source
    /// spelling `0.50` for an asked `0.5` would otherwise be refused as not
    /// asked for, and a `0.5`/`0.50` pair would slip past the repeat check
    /// and duplicate at append. `Err` names the row and column; on `Err`
    /// the rows are partly rewritten and must be discarded.
    pub fn normalize_keys(&mut self, key_types: &[ColumnType]) -> Result<(), String> {
        if self.keys.len() != key_types.len() {
            return Err(format!(
                "{} key column(s) for a {}-column key",
                self.keys.len(),
                key_types.len()
            ));
        }
        for (c, (column, ty)) in self.keys.iter_mut().zip(key_types).enumerate() {
            for (i, value) in column.iter_mut().enumerate() {
                *value = normalize_key(value, *ty)
                    .map_err(|e| format!("row {i}, key column {c}: {e}"))?;
            }
        }
        Ok(())
    }

    /// Equal lengths, `key_arity` key columns, `value_arity` value columns,
    /// no repeated `(key tuple, ts)`, and every row's key inside `asked`
    /// (equal to an Exact tuple, or starting with a Prefix tuple). A row
    /// outside the ask is refused rather than filed: it would land under a
    /// key whose coverage the request never established.
    pub fn validate_against(
        &self,
        key_arity: usize,
        value_arity: usize,
        asked: &KeySet,
    ) -> Result<(), AdapterError> {
        let err = |message: String| AdapterError { message };
        if self.keys.len() != key_arity {
            return Err(err(format!(
                "{} key column(s) for a {key_arity}-column key",
                self.keys.len()
            )));
        }
        if self.values.len() != value_arity {
            return Err(err(format!(
                "{} value column(s) for {value_arity} declared",
                self.values.len()
            )));
        }
        let n = self.ts.len();
        if let Some(c) = self.keys.iter().find(|c| c.len() != n) {
            return Err(err(format!("{} key values for {n} timestamps", c.len())));
        }
        if let Some(c) = self.values.iter().find(|c| c.len() != n) {
            return Err(err(format!("{} values for {n} timestamps", c.len())));
        }
        let mut seen = HashSet::with_capacity(n);
        for i in 0..n {
            let key = self.key_at(i);
            let inside = asked.tuples().iter().any(|t| match asked {
                KeySet::Exact(_) => *t == key,
                KeySet::Prefix(_) => key.starts_with(t),
            });
            if !inside {
                return Err(err(format!(
                    "row {i} ({}) was not asked for",
                    key.join(" / ")
                )));
            }
            if !seen.insert((key, self.ts[i])) {
                return Err(err(format!(
                    "repeated observation at row {i}: {} at {}; which value wins is the source's question",
                    self.key_at(i).join(" / "),
                    self.ts[i]
                )));
            }
        }
        Ok(())
    }

    /// Removes rows with any NaN or infinite value, keeping every column
    /// aligned. Returns how many were dropped, for the warning line.
    pub fn drop_non_finite(&mut self) -> usize {
        let n = self.ts.len();
        let keep: Vec<bool> = (0..n)
            .map(|i| self.values.iter().all(|c| c[i].is_finite()))
            .collect();
        let dropped = keep.iter().filter(|k| !**k).count();
        if dropped == 0 {
            return 0;
        }
        retain_mask(&mut self.ts, &keep);
        for c in &mut self.keys {
            retain_mask(c, &keep);
        }
        for c in &mut self.values {
            retain_mask(c, &keep);
        }
        dropped
    }
}

/// Keeps `v[i]` where `keep[i]`; an element past the mask is dropped.
fn retain_mask<T>(v: &mut Vec<T>, keep: &[bool]) {
    let mut k = keep.iter();
    v.retain(|_| *k.next().unwrap_or(&false));
}

/// Blocking keyed history read owned by one history worker. Implementations
/// bound both calls with their own timeouts: the worker has no deadline, and
/// a call that never returns stops that source's history and can block
/// shutdown.
pub trait HistoryQuery: Send {
    fn query(&mut self, req: &HistoryRequest) -> Result<HistoryRows, AdapterError>;

    /// Distinct values of one key column, for typeahead and the tenor set.
    /// `None` when the source cannot enumerate.
    fn keys(&mut self, table: &str, column: &str) -> Option<Vec<String>>;
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

    /// A FRESH handle per call, like `fetch`. `None` (the default) when the
    /// adapter cannot answer keyed history queries.
    fn history(&self) -> Option<Box<dyn HistoryQuery>> {
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

    fn rows(ts: &[&str], keys: &[&[&str]], values: &[&[f64]]) -> HistoryRows {
        HistoryRows {
            ts: ts.iter().map(|s| t(s)).collect(),
            keys: keys
                .iter()
                .map(|c| c.iter().map(|s| s.to_string()).collect())
                .collect(),
            values: values.iter().map(|c| c.to_vec()).collect(),
        }
    }

    #[test]
    fn history_rows_validate_shape_duplicates_and_the_asked_keys() {
        let asked = KeySet::Prefix(vec![vec!["SPX".into()]]);
        let ok = rows(
            &["2026-01-05T21:00:00Z", "2026-01-06T21:00:00Z"],
            &[&["SPX", "SPX"], &["3M", "3M"], &["0.5", "0.5"]],
            &[&[15.0, 15.2], &[7650.0, 7660.0]],
        );
        assert!(ok.validate_against(3, 2, &asked).is_ok());
        assert_eq!(ok.key_at(1), vec!["SPX", "3M", "0.5"]);
        let mut bad = ok.clone();
        bad.values.pop();
        assert!(
            bad.validate_against(3, 2, &asked)
                .unwrap_err()
                .message
                .contains("value column")
        );
        let mut bad = ok.clone();
        bad.keys.pop();
        assert!(
            bad.validate_against(3, 2, &asked)
                .unwrap_err()
                .message
                .contains("key column")
        );
        let mut bad = ok.clone();
        bad.ts[1] = bad.ts[0];
        assert!(
            bad.validate_against(3, 2, &asked)
                .unwrap_err()
                .message
                .contains("repeated")
        );
        let mut bad = ok.clone();
        bad.keys[0][1] = "NDX".into();
        assert!(
            bad.validate_against(3, 2, &asked)
                .unwrap_err()
                .message
                .contains("not asked for"),
            "a row for a key outside the request is refused, not filed"
        );
        let exact = KeySet::Exact(vec![vec!["SPX".into(), "3M".into(), "0.5".into()]]);
        assert!(ok.validate_against(3, 2, &exact).is_ok());
        let mut unequal = ok.clone();
        unequal.ts.pop();
        assert!(
            unequal
                .validate_against(3, 2, &asked)
                .unwrap_err()
                .message
                .contains("timestamps")
        );
    }

    #[test]
    fn history_rows_drop_non_finite_values_keeping_every_column_aligned() {
        let mut r = rows(
            &[
                "2026-01-05T21:00:00Z",
                "2026-01-06T21:00:00Z",
                "2026-01-07T21:00:00Z",
            ],
            &[&["SPX", "SPX", "SPX"]],
            &[&[1.0, f64::NAN, 3.0], &[10.0, 20.0, f64::INFINITY]],
        );
        assert_eq!(r.drop_non_finite(), 2);
        assert_eq!(r.len(), 1);
        assert_eq!(r.ts, vec![t("2026-01-05T21:00:00Z")]);
        assert_eq!(r.keys, vec![vec!["SPX".to_string()]]);
        assert_eq!(r.values, vec![vec![1.0], vec![10.0]]);
    }

    const VOL_KEY: [ColumnType; 3] = [ColumnType::Utf8, ColumnType::Utf8, ColumnType::F64];

    #[test]
    fn history_rows_normalize_key_spellings_before_the_asked_keys_are_compared() {
        let mut r = rows(
            &["2026-01-05T21:00:00Z"],
            &[&[" SPX "], &["3M"], &["0.50"]],
            &[&[15.0]],
        );
        let exact = KeySet::Exact(vec![vec!["SPX".into(), "3M".into(), "0.5".into()]]);
        assert!(
            r.validate_against(3, 1, &exact).is_err(),
            "unnormalized spellings differ from the ask"
        );
        r.normalize_keys(&VOL_KEY).unwrap();
        assert_eq!(r.key_at(0), vec!["SPX", "3M", "0.5"]);
        assert!(r.validate_against(3, 1, &exact).is_ok());
    }

    #[test]
    fn history_rows_normalized_spellings_of_one_key_at_one_ts_are_repeats() {
        let mut r = rows(
            &["2026-01-05T21:00:00Z", "2026-01-05T21:00:00Z"],
            &[&["SPX", "SPX"], &["3M", "3M"], &["0.5", "0.50"]],
            &[&[15.0, 15.1]],
        );
        let asked = KeySet::Prefix(vec![vec!["SPX".into()]]);
        assert!(r.validate_against(3, 1, &asked).is_ok());
        r.normalize_keys(&VOL_KEY).unwrap();
        assert!(
            r.validate_against(3, 1, &asked)
                .unwrap_err()
                .message
                .contains("repeated")
        );
    }

    #[test]
    fn history_rows_normalize_names_the_row_and_column_it_refuses() {
        let mut r = rows(
            &["2026-01-05T21:00:00Z", "2026-01-06T21:00:00Z"],
            &[&["SPX", "SPX"], &["3M", "3M"], &["0.5", "half"]],
            &[&[15.0, 15.1]],
        );
        let e = r.normalize_keys(&VOL_KEY).unwrap_err();
        assert!(e.contains("row 1") && e.contains("key column 2"), "{e}");
        let e = r.normalize_keys(&VOL_KEY[..2]).unwrap_err();
        assert!(e.contains("3 key column(s)"), "{e}");
    }

    #[test]
    fn an_adapter_has_no_history_side_by_default() {
        let (adapter, _feed) = ChannelAdapter::new("demo_bus");
        assert!(adapter.history().is_none());
    }
}
