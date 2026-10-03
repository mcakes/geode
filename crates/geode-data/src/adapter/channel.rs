//! In-process message bus for demos and adapter integration tests. It uses the
//! same subscription pipeline as external transports without network I/O.
//!
//! The bounded inbound channel feeds one dispatcher, started by the first
//! subscription. Each matching registration receives one message, even if
//! several of its patterns match. Full subscriber queues refuse independently.
//! Message and health fan-out snapshot registrations, release the mutex, then
//! deliver. Unsubscribe removes future interest but cannot retract a snapshot
//! already taken. No callback runs under a bus lock; locks are not nested.
//!
//! The bus also keeps the newest message per concrete topic, the simulator's
//! answer to a broker GET. Recovery snapshots the asking registration's sink
//! under the registration lock, then the asked topics' last messages under
//! the last-value lock, releases both, and pushes copies marked `recovered`
//! into that one sink: a reply reaches the subscription that asked, never
//! every subscriber on the topic. The dispatcher records a message's last
//! value under its own lock, taken apart from the registration lock and
//! released before delivery.
//!
//! Feeds and egress handles own strong senders; the bus keeps only a weak one.
//! Dropping the last sender lets the dispatcher drain and exit. Its handle is
//! not joined. The channel cannot reopen: egress returns None and subscribe
//! returns an error once no strong sender remains.
//!
//! Connection reports are synchronous notifications via ChannelFeed::set_state,
//! not retained state or delivery gates. New subscriptions report Connected.
//! Closing the inbound channel does not emit Lost or remove registrations.

use super::{
    Adapter, AdapterError, ConnectionState, Egress, HealthSink, MESSAGE_BOUND, Message,
    MessageSink, Recovery, Subscription, topic::topic_matches,
};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

/// One subscribe call's standing interest in the bus.
struct Registration {
    /// Identity, so unsubscribing one of several subscriptions that happen
    /// to share a topic list removes exactly the right one. A counter
    /// rather than the sink's or the closure's address: two clones of the
    /// same sink are the same address and different registrations.
    id: u64,
    topics: Vec<String>,
    sink: MessageSink,
    health: HealthSink,
}

/// Shared bus state owned by adapter, feed, subscription, and dispatcher
/// handles. Capabilities retain an Arc independently of their adapter borrow.
struct Bus {
    name: &'static str,
    /// Weak inbound sender. Upgrade under this mutex without retaining a lock
    /// while delivering callbacks; the bus itself must not keep producers alive.
    feed: Mutex<Weak<SyncSender<Message>>>,
    /// The receiving end, waiting for the dispatcher to take it. Left here
    /// rather than moved into the thread closure so a failed `spawn` loses
    /// nothing: the receiver is still here for the next `subscribe` to try
    /// again.
    inbound: Mutex<Option<Receiver<Message>>>,
    registrations: Mutex<Vec<Registration>>,
    /// The newest message per concrete topic, the simulator's answer to a
    /// GET; grows with distinct topics, which is bounded in the demo.
    last: Mutex<HashMap<String, Message>>,
    next_id: AtomicU64,
    /// Messages [`ChannelFeed::publish`] could not put on the bus, over the
    /// bus's whole life. Shared by every feed and read through
    /// [`ChannelFeed::refused`], for the same reason
    /// [`MessageSink::refused`] exists: a dropped message must be countable
    /// somewhere, or a producer outrunning the dispatcher is invisible.
    refused: AtomicU64,
    /// Dispatcher handle, set once under a lock and never joined or cleared.
    /// The lock serializes concurrent startup attempts. Bus liveness is checked
    /// through the weak sender, not by whether this handle remains present.
    dispatcher: Mutex<Option<JoinHandle<()>>>,
}

impl Bus {
    /// Starts the dispatcher if it is not already running.
    ///
    /// The `dispatcher` guard is held across the spawn so the check and the
    /// set are one step; nothing else is locked inside, so there is no
    /// ordering to get wrong.
    fn ensure_dispatcher(self: &Arc<Self>) -> Result<(), AdapterError> {
        let mut running = self.dispatcher.lock().unwrap();
        if running.is_some() {
            return Ok(());
        }
        let bus = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("geode-channel-{}", self.name))
            .spawn(move || bus.dispatch());
        match spawned {
            Ok(handle) => {
                *running = Some(handle);
                Ok(())
            }
            Err(e) => Err(AdapterError {
                message: format!(
                    "channel adapter '{}': could not start the dispatcher thread: {e}",
                    self.name
                ),
            }),
        }
    }

    /// The dispatcher thread's whole life: drain the inbound channel, fan
    /// each message to the registrations that asked for it, and end when
    /// the channel closes (every `ChannelFeed` and egress dropped).
    fn dispatch(&self) {
        let Some(inbound) = self.inbound.lock().unwrap().take() else {
            // Only reachable if a previous dispatcher took the receiver,
            // which `ensure_dispatcher`'s check-and-set rules out.
            return;
        };
        while let Ok(message) = inbound.recv() {
            // The guard is dropped with this block, before any `push`: see
            // the module doc's lock discipline.
            let targets: Vec<MessageSink> = {
                let registrations = self.registrations.lock().unwrap();
                registrations
                    .iter()
                    .filter(|r| {
                        r.topics
                            .iter()
                            .any(|pattern| topic_matches(pattern, &message.topic))
                    })
                    .map(|r| r.sink.clone())
                    .collect()
            };
            // Recorded whether or not anyone is subscribed now, so a later
            // subscriber can recover a document published before it arrived.
            // Its own lock, released before any `push`.
            self.last
                .lock()
                .unwrap()
                .insert(message.topic.clone(), message.clone());
            // One clone of the body per extra subscriber and none for the
            // last — each sink owns its bytes, and the ordinary case is a
            // single subscription per topic space, which then costs none.
            let Some((last, rest)) = targets.split_last() else {
                continue;
            };
            for sink in rest {
                sink.push(message.clone());
            }
            // The refusal is the sink's own business to count; a full
            // subscriber must never stop this thread, which is feeding
            // every other one.
            last.push(message);
        }
    }

    /// Hands `state` to every registered health sink, on the caller's
    /// thread. Snapshot-then-call, for the lock discipline above.
    fn fan_state(&self, state: ConnectionState) {
        let sinks: Vec<HealthSink> = {
            let registrations = self.registrations.lock().unwrap();
            registrations
                .iter()
                .map(|r| Arc::clone(&r.health))
                .collect()
        };
        for sink in sinks {
            sink(state.clone());
        }
    }
}

/// The in-process adapter. Register it like any other
/// ([`AdapterRegistry::register`](super::AdapterRegistry::register)); a
/// source naming it subscribes through the same code path a broker source
/// would.
pub struct ChannelAdapter {
    bus: Arc<Bus>,
}

impl ChannelAdapter {
    /// Create an adapter and producer feed. A source selects the adapter by
    /// `name`. The inbound queue holds up to MESSAGE_BOUND messages; publish
    /// returns false and counts refusal if the queue cannot accept another.
    pub fn new(name: &'static str) -> (Arc<ChannelAdapter>, ChannelFeed) {
        let (tx, inbound) = mpsc::sync_channel(MESSAGE_BOUND);
        let tx = Arc::new(tx);
        let bus = Arc::new(Bus {
            name,
            feed: Mutex::new(Arc::downgrade(&tx)),
            inbound: Mutex::new(Some(inbound)),
            registrations: Mutex::new(Vec::new()),
            last: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            dispatcher: Mutex::new(None),
        });
        let feed = ChannelFeed {
            bus: Arc::clone(&bus),
            tx,
        };
        (Arc::new(ChannelAdapter { bus }), feed)
    }
}

impl Adapter for ChannelAdapter {
    fn name(&self) -> &'static str {
        self.bus.name
    }

    fn subscription(&self) -> Option<Box<dyn Subscription>> {
        Some(Box::new(ChannelSubscription {
            bus: Arc::clone(&self.bus),
            id: self.bus.next_id.fetch_add(1, Ordering::Relaxed),
        }))
    }

    fn egress(&self) -> Option<Box<dyn Egress>> {
        // No new egress can be created after the last strong sender is dropped.
        // A returned egress itself keeps the inbound channel open.
        let tx = self.bus.feed.lock().unwrap().upgrade()?;
        Some(Box::new(ChannelEgress {
            feed: ChannelFeed {
                bus: Arc::clone(&self.bus),
                tx,
            },
        }))
    }
}

/// Cloneable producer handle. Feeds and egress handles keep the inbound
/// channel open until the last strong sender is dropped.
#[derive(Clone)]
pub struct ChannelFeed {
    bus: Arc<Bus>,
    /// Strong, unlike the adapter's own `Weak` — this is the handle whose
    /// existence keeps the channel open.
    tx: Arc<SyncSender<Message>>,
}

impl ChannelFeed {
    /// Try to enqueue a message stamped with the current UTC arrival time.
    /// False counts a full or disconnected inbound queue. True acknowledges
    /// only bus admission, not receipt by any subscriber or publication to storage.
    pub fn publish(&self, topic: &str, bytes: Vec<u8>) -> bool {
        let queued = self
            .tx
            .try_send(Message {
                topic: topic.to_string(),
                received: Utc::now(),
                bytes,
                recovered: false,
            })
            .is_ok();
        if !queued {
            self.bus.refused.fetch_add(1, Ordering::Relaxed);
        }
        queued
    }

    /// How many messages this bus's inbound queue refused, over its whole
    /// life and across every feed sharing it. These refusals happen before
    /// topic routing, so no source owns them and no health lane reports them;
    /// the demo publisher warns on the first. A subscription's own drops are
    /// [`MessageSink::refused`], which its receiver reports as `<source>:queue`.
    pub fn refused(&self) -> u64 {
        self.bus.refused.load(Ordering::Relaxed)
    }

    /// Notify current health subscribers synchronously on the caller's thread,
    /// outside the registration lock. State is not retained for new subscribers
    /// and does not pause or reject messages. Concurrent calls have no ordering
    /// guarantee; callbacks must return promptly and must not panic.
    pub fn set_state(&self, state: ConnectionState) {
        self.bus.fan_state(state);
    }
}

/// One registration's handle, as handed out by
/// [`ChannelAdapter::subscription`]. Holds the bus and its own id, so a
/// second subscription from the same adapter is genuinely independent.
struct ChannelSubscription {
    bus: Arc<Bus>,
    id: u64,
}

impl ChannelSubscription {
    /// Drops this subscription's registration, if it has one. The one
    /// remover: `subscribe` (replacing), `unsubscribe` and `Drop` all come
    /// here, so there is no second copy of the identity check to fall out
    /// of step.
    fn remove(&self) {
        self.bus
            .registrations
            .lock()
            .unwrap()
            .retain(|r| r.id != self.id);
    }
}

impl Subscription for ChannelSubscription {
    /// Register interest and synchronously report Connected. Hold a strong sender
    /// until the call returns so the bus remains open through registration and
    /// notification. This does not guarantee future messages or a Lost notification
    /// when the last producer disappears.
    fn subscribe(
        &mut self,
        topics: &[String],
        sink: MessageSink,
        health: HealthSink,
    ) -> Result<(), AdapterError> {
        if topics.is_empty() {
            return Err(AdapterError {
                message: format!(
                    "channel adapter '{}': a subscription needs at least one topic",
                    self.bus.name
                ),
            });
        }
        // Retain a strong sender across registration and Connected delivery. Merely
        // checking the weak sender would allow the last producer to disappear
        // between those steps. The feed mutex is released after this upgrade.
        let _open = self
            .bus
            .feed
            .lock()
            .unwrap()
            .upgrade()
            .ok_or_else(|| AdapterError {
                message: format!(
                    "channel adapter '{}': every feed has been dropped; the bus is closed",
                    self.bus.name
                ),
            })?;
        // Start the dispatcher before registering. A spawn failure then leaves no
        // new registration. Messages can be drained before registration takes effect.
        self.bus.ensure_dispatcher()?;
        {
            let mut registrations = self.bus.registrations.lock().unwrap();
            // A second `subscribe` on the same handle REPLACES its topics
            // rather than adding a second registration under one id —
            // otherwise `unsubscribe` would remove both and the subscriber
            // would have no way to say which it meant.
            registrations.retain(|r| r.id != self.id);
            registrations.push(Registration {
                id: self.id,
                topics: topics.to_vec(),
                sink,
                health: Arc::clone(&health),
            });
        }
        // Notify outside the registration lock. New subscriptions report Connected
        // regardless of any earlier set_state notification.
        health(ConnectionState::Connected);
        Ok(())
    }

    fn unsubscribe(&mut self) {
        self.remove();
    }

    fn recovery(&mut self) -> Option<Box<dyn Recovery>> {
        Some(Box::new(ChannelRecovery {
            bus: Arc::clone(&self.bus),
            id: self.id,
        }))
    }
}

/// One subscription's GET side. Holds the registration id rather than its
/// sink, so recovering after `unsubscribe` is refused instead of feeding a
/// receiver its owner has stopped.
struct ChannelRecovery {
    bus: Arc<Bus>,
    id: u64,
}

impl Recovery for ChannelRecovery {
    /// Push the last message on each asked topic into this registration's
    /// own sink, marked `recovered`. The in-process bus answers at once, so
    /// `timeout` is unused. A full sink refuses and counts like any delivery.
    fn recover(&mut self, topics: &[String], _timeout: Duration) -> Result<(), AdapterError> {
        let sink = {
            let registrations = self.bus.registrations.lock().unwrap();
            registrations
                .iter()
                .find(|r| r.id == self.id)
                .map(|r| r.sink.clone())
        };
        let Some(sink) = sink else {
            return Err(AdapterError {
                message: format!("channel adapter '{}': not subscribed", self.bus.name),
            });
        };
        let replies: Vec<Message> = {
            let last = self.bus.last.lock().unwrap();
            topics.iter().filter_map(|t| last.get(t).cloned()).collect()
        };
        for message in replies {
            sink.push(Message {
                received: Utc::now(),
                recovered: true,
                ..message
            });
        }
        Ok(())
    }
}

impl Drop for ChannelSubscription {
    /// Remove this subscription on drop as well as explicit unsubscribe. Delivery
    /// snapshots already taken by a fan-out can still hold its sink briefly.
    fn drop(&mut self) {
        self.remove();
    }
}

/// The upload side. Holds a full [`ChannelFeed`], so an upload goes onto
/// the bus by exactly the door a producer uses — there is no second way to
/// build a `Message` in this module.
struct ChannelEgress {
    feed: ChannelFeed,
}

impl Egress for ChannelEgress {
    /// Enqueue bytes on the target topic through the normal feed path. Success
    /// acknowledges inbound admission; matching subscriptions can still refuse
    /// the message independently.
    fn upload(&mut self, target: &str, bytes: Vec<u8>) -> Result<(), AdapterError> {
        if self.feed.publish(target, bytes) {
            Ok(())
        } else {
            Err(AdapterError {
                message: format!(
                    "channel adapter '{}': the bus is full or closed; nothing was published on '{target}'",
                    self.feed.bus.name
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{ConnectionState, HealthSink, MessageSink};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Polls `done` to a 5 s deadline. Every wait in these tests is on a
    /// condition, never a bare sleep: the dispatcher is a real thread, so
    /// "the message arrived" and "the refusals have happened" are the only
    /// things worth waiting for, and both are observable.
    fn wait_until(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(
                Instant::now() < deadline,
                "the condition was not reached within 5s"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn recorder(seen: &Arc<Mutex<Vec<ConnectionState>>>) -> HealthSink {
        let seen = Arc::clone(seen);
        Arc::new(move |state| seen.lock().unwrap().push(state))
    }

    #[test]
    fn subscribing_to_no_topic_at_all_is_refused_rather_than_reported_connected() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, _rx) = MessageSink::bounded(8);
        let states: Arc<Mutex<Vec<ConnectionState>>> = Default::default();
        let mut sub = adapter.subscription().unwrap();
        let refused = sub
            .subscribe(&[], sink, recorder(&states))
            .expect_err("a subscription with no topics matches nothing");
        assert!(refused.message.contains("at least one topic"), "{refused}");
        // The refusal is what keeps the caller from believing it is live:
        // nothing was registered and no `Connected` was reported, so a
        // source misconfigured this way reads as broken rather than as a
        // healthy feed that never delivers.
        assert!(states.lock().unwrap().is_empty());
        assert!(feed.publish("marketdata/cvi/SPX.Z", b"<x/>".to_vec()));
    }

    #[test]
    fn a_published_message_reaches_every_matching_subscription_and_no_other() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (cvi_sink, cvi_rx) = MessageSink::bounded(8);
        let (repo_sink, repo_rx) = MessageSink::bounded(8);
        let states: Arc<Mutex<Vec<ConnectionState>>> = Default::default();
        let health = recorder(&states);
        let mut sub_a = adapter.subscription().unwrap();
        sub_a
            .subscribe(&["marketdata/cvi/>".into()], cvi_sink, health.clone())
            .unwrap();
        let mut sub_b = adapter.subscription().unwrap();
        sub_b
            .subscribe(&["marketdata/repo/>".into()], repo_sink, health.clone())
            .unwrap();
        assert_eq!(
            states.lock().unwrap().as_slice(),
            &[ConnectionState::Connected, ConnectionState::Connected]
        );
        assert!(feed.publish("marketdata/cvi/SPX.Z", b"<x/>".to_vec()));
        let m = cvi_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            (m.topic.as_str(), m.bytes.as_slice()),
            ("marketdata/cvi/SPX.Z", &b"<x/>"[..])
        );
        assert!(
            repo_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "repo did not get cvi's message"
        );
        sub_a.unsubscribe();
        assert!(feed.publish("marketdata/cvi/SPX.Z", b"<y/>".to_vec()));
        assert!(
            cvi_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "unsubscribed"
        );
    }

    #[test]
    fn an_upload_echoes_on_the_target_topic() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, rx) = MessageSink::bounded(8);
        let mut sub = adapter.subscription().unwrap();
        sub.subscribe(&["marketdata/upload/>".into()], sink, Arc::new(|_| {}))
            .unwrap();
        let mut egress = adapter.egress().unwrap();
        egress
            .upload("marketdata/upload/cvi", b"<doc/>".to_vec())
            .unwrap();
        let m = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            (m.topic.as_str(), m.bytes.as_slice()),
            ("marketdata/upload/cvi", &b"<doc/>"[..])
        );
        let _ = feed;
    }

    #[test]
    fn set_state_reaches_every_health_sink() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let seen_a: Arc<Mutex<Vec<ConnectionState>>> = Default::default();
        let seen_b: Arc<Mutex<Vec<ConnectionState>>> = Default::default();
        let (sink_a, _rx_a) = MessageSink::bounded(1);
        let (sink_b, _rx_b) = MessageSink::bounded(1);
        let mut sub_a = adapter.subscription().unwrap();
        sub_a
            .subscribe(&["marketdata/cvi/>".into()], sink_a, recorder(&seen_a))
            .unwrap();
        let mut sub_b = adapter.subscription().unwrap();
        sub_b
            .subscribe(&["marketdata/repo/>".into()], sink_b, recorder(&seen_b))
            .unwrap();

        let lost = ConnectionState::Lost {
            reason: "the bus went away".into(),
        };
        feed.set_state(lost.clone());
        assert_eq!(
            seen_a.lock().unwrap().as_slice(),
            &[ConnectionState::Connected, lost.clone()]
        );
        assert_eq!(
            seen_b.lock().unwrap().as_slice(),
            &[ConnectionState::Connected, lost]
        );
    }

    #[test]
    fn dropping_every_feed_ends_the_dispatcher_and_closes_the_egress_door() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, _rx) = MessageSink::bounded(8);
        let mut sub = adapter.subscription().unwrap();
        sub.subscribe(&["marketdata/cvi/>".into()], sink, Arc::new(|_| {}))
            .unwrap();
        assert!(
            adapter.egress().is_some(),
            "a live feed keeps the upload door open"
        );

        // The dispatcher holds one `Arc<Bus>` of its own, so the count
        // falling to the adapter's single reference is the observable proof
        // that the thread ended — which it can only do by the inbound
        // channel closing, i.e. by the last strong sender going away.
        drop(sub);
        drop(feed);
        wait_until(|| Arc::strong_count(&adapter.bus) == 1);
        assert!(
            adapter.egress().is_none(),
            "with every feed gone there is nothing to publish onto"
        );
    }

    #[test]
    fn subscribing_after_every_feed_is_dropped_is_refused() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, _rx) = MessageSink::bounded(8);
        let mut first = adapter.subscription().unwrap();
        first
            .subscribe(&["marketdata/cvi/>".into()], sink, Arc::new(|_| {}))
            .unwrap();
        drop(first);
        drop(feed);
        // The dispatcher has exited (its `Arc<Bus>` is gone), which is
        // exactly the state in which the started-flag alone would say "all
        // fine, already running".
        wait_until(|| Arc::strong_count(&adapter.bus) == 1);

        let (late_sink, late_rx) = MessageSink::bounded(8);
        let states: Arc<Mutex<Vec<ConnectionState>>> = Default::default();
        let mut late = adapter.subscription().unwrap();
        let refused = late
            .subscribe(&["marketdata/cvi/>".into()], late_sink, recorder(&states))
            .expect_err("a closed bus cannot be subscribed to");
        assert!(refused.message.contains("the bus is closed"), "{refused}");
        // Nothing was registered and nothing was reported: a caller that
        // was told `Connected` here would wait forever for rows that can
        // never come.
        assert!(states.lock().unwrap().is_empty());
        assert!(late_rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn a_publish_onto_a_full_bus_is_refused_and_counted() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        // No subscription, so no dispatcher is draining: the inbound queue
        // fills at exactly its bound, which makes the refusal deterministic
        // rather than a race with a consumer.
        for i in 0..MESSAGE_BOUND {
            assert!(
                feed.publish("marketdata/cvi/SPX.Z", vec![i as u8]),
                "message {i} fits within the bound"
            );
        }
        assert_eq!(feed.refused(), 0);
        assert!(!feed.publish("marketdata/cvi/SPX.Z", vec![0]));
        assert!(!feed.publish("marketdata/cvi/SPX.Z", vec![1]));
        assert_eq!(feed.refused(), 2);
        // The count is the bus's, not one holder's.
        assert_eq!(feed.clone().refused(), 2);
        let _ = adapter;
    }

    #[test]
    fn a_subscription_whose_sink_is_full_loses_the_message_and_the_dispatcher_lives() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, rx) = MessageSink::bounded(1);
        // Kept beside the subscription's own copy: `refused` is shared
        // through the sink's `Arc`, and polling it is what makes this test
        // deterministic. Asserting "only the first arrived" without it
        // would race the dispatcher, which need not have tried the other
        // two by the time the assert runs.
        let counter = sink.clone();
        let mut sub = adapter.subscription().unwrap();
        sub.subscribe(&["marketdata/cvi/>".into()], sink, Arc::new(|_| {}))
            .unwrap();

        for body in [&b"1"[..], b"2", b"3"] {
            assert!(feed.publish("marketdata/cvi/SPX.Z", body.to_vec()));
        }
        wait_until(|| counter.refused() == 2);

        let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.bytes.as_slice(), b"1");
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a refused message is dropped, not queued behind the first"
        );

        // The dispatcher survived both refusals: a later publish still
        // flows now that the sink has room again.
        assert!(feed.publish("marketdata/cvi/SPX.Z", b"4".to_vec()));
        let next = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next.bytes.as_slice(), b"4");
    }

    #[test]
    fn recovery_resends_the_last_message_on_each_asked_topic_marked_recovered() {
        let (adapter, feed) = ChannelAdapter::new("t");
        let mut sub = adapter.subscription().unwrap();
        let (sink, rx) = MessageSink::bounded(16);
        sub.subscribe(&["md/*/NOTIFY".into()], sink, Arc::new(|_| {}))
            .unwrap();
        feed.publish("md/SPX/NOTIFY", b"one".to_vec());
        feed.publish("md/SPX/NOTIFY", b"two".to_vec());
        for _ in 0..2 {
            rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        let mut rec = sub.recovery().expect("the channel adapter recovers");
        rec.recover(
            &["md/SPX/NOTIFY".into(), "md/NONE/NOTIFY".into()],
            Duration::from_secs(1),
        )
        .unwrap();
        let m = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(
            (m.topic.as_str(), m.bytes.as_slice(), m.recovered),
            ("md/SPX/NOTIFY", &b"two"[..], true)
        );
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "an unknown topic is not answered"
        );
    }

    #[test]
    fn recovery_after_unsubscribe_is_an_error() {
        let (adapter, _feed) = ChannelAdapter::new("t");
        let mut sub = adapter.subscription().unwrap();
        let (sink, _rx) = MessageSink::bounded(4);
        sub.subscribe(&["md/>".into()], sink, Arc::new(|_| {}))
            .unwrap();
        let mut rec = sub.recovery().unwrap();
        sub.unsubscribe();
        assert!(
            rec.recover(&["md/SPX/NOTIFY".into()], Duration::from_secs(1))
                .is_err()
        );
    }

    #[test]
    fn ordinary_messages_are_not_marked_recovered() {
        let (adapter, feed) = ChannelAdapter::new("t");
        let mut sub = adapter.subscription().unwrap();
        let (sink, rx) = MessageSink::bounded(4);
        sub.subscribe(&["md/>".into()], sink, Arc::new(|_| {}))
            .unwrap();
        feed.publish("md/SPX/NOTIFY", b"x".to_vec());
        assert!(!rx.recv_timeout(Duration::from_secs(1)).unwrap().recovered);
    }

    #[test]
    fn recovery_replies_reach_only_the_subscription_that_asked() {
        let (adapter, feed) = ChannelAdapter::new("t");
        let (asking_sink, asking_rx) = MessageSink::bounded(8);
        let (other_sink, other_rx) = MessageSink::bounded(8);
        let mut asking = adapter.subscription().unwrap();
        asking
            .subscribe(&["md/*/NOTIFY".into()], asking_sink, Arc::new(|_| {}))
            .unwrap();
        let mut other = adapter.subscription().unwrap();
        other
            .subscribe(&["md/>".into()], other_sink, Arc::new(|_| {}))
            .unwrap();
        feed.publish("md/SPX/NOTIFY", b"doc".to_vec());
        asking_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        other_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let mut rec = asking.recovery().unwrap();
        rec.recover(&["md/SPX/NOTIFY".into()], Duration::from_secs(1))
            .unwrap();
        let m = asking_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(m.recovered);
        assert!(
            other_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "a GET reply answers the asker, not every subscriber on the topic"
        );
    }
}
