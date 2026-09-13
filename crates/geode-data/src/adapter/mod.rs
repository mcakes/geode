//! The adapter tier: how a market-data feed reaches this crate (market-
//! data spec §5.2).
//!
//! Everything a vendor adapter has to provide is a trait here, and the
//! only implementation in the workspace is [`channel::ChannelAdapter`],
//! which is in-process. That is deliberate (roadmap ruling 5): the real
//! Solace adapter is built on the desk's own machine against a crate this
//! repo cannot compile, so what lives here is its CONTRACT — three small
//! object-safe traits, a registry the app fills at startup exactly as it
//! fills [`crate::documents::DocumentRegistry`], and a bounded sink. No
//! file and no socket is opened anywhere in this module.
//!
//! The one rule the whole tier is shaped around: **nothing blocks a
//! producer.** A broker's callback thread is not ours to stall — it feeds
//! every other subscriber in that process too — so [`MessageSink::push`]
//! is a `try_send` on a bounded channel plus a counter, never a `send`,
//! in exactly the mould of [`crate::service::EventSink`] and
//! [`crate::ingest::IngestSink`]. A message that cannot be delivered is
//! DROPPED and counted: market data is a stream of latest-value snapshots
//! (the coalescer downstream keeps at most one pending document per key),
//! so a queue that grows until it is authoritative is worse than a gap.

pub mod channel;
pub mod topic;

pub use channel::{ChannelAdapter, ChannelFeed};
pub use topic::topic_matches;

use chrono::{DateTime, Utc};
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
#[derive(Debug, Clone)]
pub struct Message {
    pub topic: String,
    pub received: DateTime<Utc>,
    pub bytes: Vec<u8>,
}

/// The depth of one subscription's message queue — and, in
/// [`ChannelAdapter`], of the bus's own inbound queue as well, since a
/// producer outrunning the dispatcher and a dispatcher outrunning a
/// receiver are the same problem one hop apart and there is no reason for
/// the two to disagree.
///
/// Sized for a burst, not a backlog: a receiver thread that has fallen 256
/// messages behind is not going to catch up by being given 4,096 — the
/// coalescer would collapse them to one document per key anyway. What the
/// depth buys is tolerance of a momentary stall (a publish transaction, a
/// slow parse) without dropping anything.
pub const MESSAGE_BOUND: usize = 256;

/// Where an adapter puts the messages it received: bounded, and never
/// blocking.
///
/// `push` answers whether the message was DELIVERED. `false` means the
/// consumer's queue was full or its receiver is gone, and the two are the
/// same answer on purpose, for the same reason [`crate::service::EventSink`]
/// gives: the rule for both is identical — the message is dropped, the
/// refusal is counted, and **the producer carries on**. An adapter must
/// never treat `false` as "stop publishing": the thread it is running on
/// belongs to the vendor library, and stalling it or unwinding out of it
/// is a far worse failure than a missed snapshot.
///
/// Clone is by design and shares the counter: the dispatcher (or the
/// vendor's callback) holds one clone per registration while the test or
/// the service that created it reads [`MessageSink::refused`] from another
/// thread.
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

    /// The shared refusal counter on its own, for a reader that wants the
    /// count WITHOUT holding a sender.
    ///
    /// That distinction is the whole reason this exists
    /// (`SubscriptionWorker`'s own `refused` field): keeping a
    /// `MessageSink` clone alive to read [`MessageSink::refused`] also
    /// keeps the receiver's channel connected, so unsubscribing no longer
    /// disconnects it and every join has to wait out the receiver's
    /// timeout instead of returning at once. An `Arc<AtomicU64>` carries
    /// the number and nothing else.
    pub fn refused_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.refused)
    }
}

/// What an adapter says about its connection to the feed.
///
/// Three states rather than a bool because the middle one is the whole
/// point: a vendor client that is reconnecting has not lost anything yet,
/// and a trader should see "waiting", not "broken". `Lost` carries the
/// reason because that string is what reaches the diagnostics tile;
/// `DataService` maps the three onto the discovery health lane
/// (`Connected -> Ok`, `Reconnecting -> Pending`, `Lost -> Failed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Reconnecting,
    Lost { reason: String },
}

/// Where connection state goes. Called on whatever thread noticed the
/// transition — the vendor's, or [`ChannelFeed::set_state`]'s caller — so
/// it must not block and must not call back into the adapter. It returns
/// nothing: unlike a message, a state is idempotent and the newest one
/// wins, so there is nothing useful to say about "delivered".
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

/// One source's live subscription. `Send` and not `Sync`: each is owned by
/// exactly one receiver thread, which is also the only thread allowed to
/// unsubscribe it — hence `&mut self` on both methods, so no lock is
/// needed at this level.
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

    /// Stops delivery. Idempotent, and infallible on purpose: it is what a
    /// shutting-down worker calls, and a failure there has nowhere to go.
    fn unsubscribe(&mut self);
}

/// The write side: publishing a document back onto the bus (spec §5.5).
/// Separate from [`Subscription`] because the two capabilities are
/// independent — a read-only feed has no egress, and an adapter that can
/// only upload has no subscription.
pub trait Egress: Send {
    fn upload(&mut self, target: &str, bytes: Vec<u8>) -> Result<(), AdapterError>;
}

/// A named feed transport. `Send + Sync` because one adapter is shared
/// (behind an `Arc`) by the service thread that hands out subscriptions
/// and by whatever thread the transport itself runs on.
///
/// Both capability doors return `Option` rather than `Result` because
/// `None` is not the failure of a call: it says this adapter cannot do
/// that, and the caller's response is to report a source it cannot serve
/// rather than to treat it as an error to surface verbatim.
///
/// `None` is usually a static fact — a read-only feed has no egress side in
/// any build. It need not be: an adapter may also LOSE a capability at
/// runtime, as [`ChannelAdapter`] does once every producer of its bus is
/// gone. So a caller must not cache the answer as a property of the
/// adapter; asking again on a later request is legitimate, and the same
/// goes for a capability that is present but refuses — `subscribe` on a
/// closed [`ChannelAdapter`] answers `Err` rather than a silent
/// `Connected`.
pub trait Adapter: Send + Sync {
    /// The name a `[sources.<name>] adapter = "…"` key refers to. A
    /// `&'static str` because an adapter is compiled in, never named at
    /// runtime.
    fn name(&self) -> &'static str;

    /// A FRESH subscription each call — one per subscribed source, with
    /// its own topics, sink and identity, so unsubscribing one never
    /// touches another. `None` if this adapter cannot subscribe at all.
    fn subscription(&self) -> Option<Box<dyn Subscription>>;

    /// The upload side, or `None` if this adapter has none.
    fn egress(&self) -> Option<Box<dyn Egress>>;
}

/// The adapters this build has, keyed by [`Adapter::name`].
///
/// Shaped exactly like [`crate::documents::DocumentRegistry`] and for the
/// same reason: `geode-app` is the one crate that knows which transports
/// were compiled in (the vendor adapter is not in this repo), so it fills
/// the registry at startup and `DataService` only ever looks a name up. A
/// source naming an adapter that is not here is a configured source this
/// build cannot serve — reported as unhealthy, never a panic.
#[derive(Default, Clone)]
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn Adapter>>,
}

impl AdapterRegistry {
    /// Registers an adapter, keyed by `adapter.name()`. A second
    /// registration under the same name replaces the first and warns — the
    /// same ruling `DocumentRegistry::register` records: there is no
    /// ordering guarantee across the app's startup wiring that would make
    /// "first wins" safer, but which transport answers a name from then on
    /// must not be silent.
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
}
