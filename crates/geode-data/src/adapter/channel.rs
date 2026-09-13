//! `ChannelAdapter`: an [`Adapter`](super::Adapter) that is a channel and
//! nothing else — no socket, no file, no vendor library.
//!
//! It exists twice over. It is the fixture every subscribed-source path in
//! this crate is tested against (a real thread, a real bounded queue, real
//! topic matching — the only thing that is not real is the wire), and it
//! is the demo bus: `--demo` has no broker to talk to, so the generator
//! publishes onto one of these and the service subscribes to it through
//! exactly the code path a Solace source would take (spec §9.4).
//!
//! Shape:
//!
//! * [`ChannelAdapter::new`] makes a bounded inbound channel and hands
//!   back the adapter and one [`ChannelFeed`] — the producer side.
//! * One dispatcher thread drains that channel and fans each message out
//!   to every registration whose topic list matches. It starts on the
//!   FIRST `subscribe` rather than at construction, so an adapter that is
//!   registered but never used by any source costs no thread.
//! * `ChannelFeed::set_state` fans a [`ConnectionState`] out synchronously
//!   on the CALLER's thread. That is the point: a test asserts on what the
//!   health sinks saw on the line after the call, with nothing to wait
//!   for, and the state a trader sees never queues behind a backlog of
//!   messages.
//!
//! **Lock discipline.** The registrations live behind a `Mutex` because
//! subscribing, unsubscribing and dispatching happen on different threads.
//! No sink — message or health — is ever called with that lock held: both
//! fan-out paths clone the handles they need into a small `Vec`, drop the
//! guard, and then call out. A sink is foreign code (Task 9's receiver
//! thread, a test's closure, later the app's), so calling it under the
//! lock would make a `subscribe` from inside a sink a deadlock rather than
//! a merely surprising thing to write. No two of this module's locks are
//! ever held at once, in any order.
//!
//! **Lifetime.** The adapter holds the inbound sender only WEAKLY, so
//! dropping the last strong holder — every `ChannelFeed` and every
//! outstanding egress — closes the channel, ends the dispatcher, and frees
//! the adapter even if the registry still holds its `Arc`. That is why
//! [`ChannelAdapter::egress`] returns `None` once every feed is gone: with
//! nothing left to keep the bus open, there is no honest way to publish on
//! it.

use super::{
    Adapter, AdapterError, ConnectionState, Egress, HealthSink, MESSAGE_BOUND, Message,
    MessageSink, Subscription, topic::topic_matches,
};
use chrono::Utc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;

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

/// Everything a feed, a subscription and the dispatcher share.
///
/// Split out of [`ChannelAdapter`] because `Adapter::subscription` and
/// `Adapter::egress` take `&self`: there is no `Arc<Self>` to clone from
/// inside a trait method, and both the subscription and the egress side
/// have to outlive the borrow. An `Arc<Bus>` the adapter merely wraps is
/// the plainest way to say that, and it keeps the strong/weak story in one
/// struct.
struct Bus {
    name: &'static str,
    /// The inbound sender, held weakly — see the module doc's *Lifetime*.
    /// `Mutex` only because `Weak` is not `Sync` on its own; it is taken
    /// for the length of an upgrade and nothing else.
    feed: Mutex<Weak<SyncSender<Message>>>,
    /// The receiving end, waiting for the dispatcher to take it. Left here
    /// rather than moved into the thread closure so a failed `spawn` loses
    /// nothing: the receiver is still here for the next `subscribe` to try
    /// again.
    inbound: Mutex<Option<Receiver<Message>>>,
    registrations: Mutex<Vec<Registration>>,
    next_id: AtomicU64,
    /// `Some` once the dispatcher is running. Never joined: nothing in the
    /// app has a thread it may block on, and the dispatcher ends by itself
    /// when the channel closes. Holding the handle is what makes "started"
    /// a single check-and-set under one lock, so two concurrent
    /// `subscribe`s cannot start two dispatchers.
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
    /// The adapter and its producer side.
    ///
    /// `name` is what a `[sources.<name>] adapter = "…"` key must say, so
    /// it is `&'static str` like every other adapter's name — `"channel"`
    /// for a plain one, `"demo_bus"` for the demo's. The inbound channel is
    /// bounded at [`MESSAGE_BOUND`]: a publisher that outruns the
    /// dispatcher is refused (`publish` answers `false`) rather than
    /// allowed to grow the queue, the same rule every sink in this tier
    /// follows.
    pub fn new(name: &'static str) -> (Arc<ChannelAdapter>, ChannelFeed) {
        let (tx, inbound) = mpsc::sync_channel(MESSAGE_BOUND);
        let tx = Arc::new(tx);
        let bus = Arc::new(Bus {
            name,
            feed: Mutex::new(Arc::downgrade(&tx)),
            inbound: Mutex::new(Some(inbound)),
            registrations: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
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
        // `None` once every feed is gone: the bus is closed and publishing
        // on it could only fail. Holding this strong sender is also what
        // keeps the bus open for as long as the egress lives.
        let tx = self.bus.feed.lock().unwrap().upgrade()?;
        Some(Box::new(ChannelEgress {
            feed: ChannelFeed {
                bus: Arc::clone(&self.bus),
                tx,
            },
        }))
    }
}

/// The producer side of a [`ChannelAdapter`]: what a test or the demo bus
/// holds to put messages on it. `Clone` so several producers can share one
/// bus; the bus lives until the last of them is dropped.
#[derive(Clone)]
pub struct ChannelFeed {
    bus: Arc<Bus>,
    /// Strong, unlike the adapter's own `Weak` — this is the handle whose
    /// existence keeps the channel open.
    tx: Arc<SyncSender<Message>>,
}

impl ChannelFeed {
    /// Puts one message on the bus. `false` if the inbound queue is full
    /// (the dispatcher is behind) — never blocks, so a demo generator or a
    /// test cannot be stalled by a slow subscriber.
    pub fn publish(&self, topic: &str, bytes: Vec<u8>) -> bool {
        self.tx
            .try_send(Message {
                topic: topic.to_string(),
                received: Utc::now(),
                bytes,
            })
            .is_ok()
    }

    /// Reports a connection state to every registered subscription,
    /// synchronously on this thread. Not queued behind the messages on
    /// purpose: a "lost" a trader sees only after the backlog drains is a
    /// "lost" reported at the wrong time.
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
        // Before the registration, so a message can never be dispatched to
        // a half-built one, and so a failure leaves nothing registered.
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
        // Outside the lock, and synchronous: the caller knows it is
        // connected by the time `subscribe` returns, with nothing to poll.
        // An in-process channel is connected as soon as it exists, so this
        // is the whole of this adapter's connection lifecycle unless a feed
        // says otherwise through `set_state`.
        health(ConnectionState::Connected);
        Ok(())
    }

    fn unsubscribe(&mut self) {
        self.remove();
    }
}

impl Drop for ChannelSubscription {
    /// A dropped subscription is an unsubscribed one. Without this, a
    /// receiver thread that ended without calling `unsubscribe` (a panic,
    /// an early return) would leave the dispatcher cloning messages into a
    /// sink nobody reads for the life of the process.
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
    /// Publishes `bytes` on `target` — the echo of spec §5.5: an uploaded
    /// document comes straight back to whichever subscriptions cover the
    /// topic it was written to, which is what makes the write path
    /// exercisable with no broker at all.
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
}
