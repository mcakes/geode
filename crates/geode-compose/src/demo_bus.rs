//! Background document publishing for `--demo`.
//!
//! Each [`Producer`] generates documents, serializes them through its
//! registered kind, and publishes to a channel feed. Subscribed sources
//! receive and parse those bytes through the normal ingestion path.
//! The application starts this bus only in demo mode.

use chrono::NaiveDate;
use geode_core::document::{DocumentKind, DocumentRows};
use geode_data::adapter::ChannelFeed;
use geode_demo_data::documents::chain::ChainGenerator;
use geode_demo_data::documents::cvi::CviGenerator;
use geode_demo_data::documents::dividend::DividendGenerator;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Interval for checking shutdown while waiting between publishes.
/// Generation, serialization, and publication run to completion before the
/// next check; this interval bounds the sleep slice, not total stop latency.
const STOP_POLL: Duration = Duration::from_millis(20);

/// A producer's generator: the next document for a key, or `None` to skip.
pub type NextDocument = dyn FnMut(&str) -> Option<DocumentRows> + Send;

/// A document generator and its destination topics.
///
/// `kind` serializes rows into the wire format consumed by the subscribed
/// source. Topics are `topic_prefix`, the key, then `/NOTIFY`, so a prefix
/// such as `"marketdata/cvi/"` includes its separator. `next` owns mutable generator
/// state without coupling the bus to a concrete generator type.
pub struct Producer {
    pub kind: Arc<dyn DocumentKind>,
    pub topic_prefix: &'static str,
    pub keys: Vec<String>,
    /// Publishes of each key in the startup burst. It is 1 for a kind with
    /// one document per key. A producer that rotates through several
    /// documents per key, like the chain's expiries, sets how many it takes
    /// to publish them all.
    pub startup_repeats: usize,
    /// `None` skips this publish: a producer whose input is not ready yet.
    pub next: Box<NextDocument>,
}

/// A running demo bus thread, stopped and joined on drop.
///
/// Shutdown is checked between publishes and in [`STOP_POLL`] sleep slices.
/// A publish already in progress must finish before the thread exits.
pub struct DemoBus {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DemoBus {
    /// Stops the bus thread and waits for it to exit. Idempotent — a
    /// second call finds `thread` already taken and returns at once —
    /// and safe to call from [`Drop`], so a caller that forgets to call
    /// it explicitly still stops the thread rather than leaking it for
    /// the rest of the process's life.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DemoBus {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawns the `geode-demo-bus` thread over `producers`.
///
/// Startup attempts `startup_repeats` publishes per key, in producer order
/// then key order, without waiting for a cadence. Ingestion and delivery
/// remain asynchronous. Subsequent publishes follow [`round_robin_schedule`],
/// with one wait per publish across all producers. Jitter is seeded and
/// capped at `cadence`, giving nonnegative waits from `cadence - jitter`
/// through `cadence + jitter` at millisecond jitter resolution. Adding
/// producers lengthens the schedule without multiplying its steady-state
/// publication rate.
///
/// Shutdown is checked between publishes and during waits. Thread creation
/// failure panics.
pub fn spawn(
    feed: ChannelFeed,
    producers: Vec<Producer>,
    cadence: Duration,
    jitter: Duration,
    seed: u64,
) -> DemoBus {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("geode-demo-bus".to_string())
        .spawn(move || run(feed, producers, cadence, jitter, seed, &thread_stop))
        .expect("failed to spawn the geode-demo-bus thread");
    DemoBus {
        stop,
        thread: Some(thread),
    }
}

/// Sleeps `duration` in [`STOP_POLL`]-sized slices, checking `stop`
/// between each. Returns `true` the moment `stop` is seen (the caller
/// must not publish or sleep again), `false` once the whole duration has
/// elapsed with `stop` never set.
fn sleep_checking_stop(duration: Duration, stop: &AtomicBool) -> bool {
    let mut remaining = duration;
    loop {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        if remaining.is_zero() {
            return false;
        }
        let step = remaining.min(STOP_POLL);
        std::thread::sleep(step);
        remaining -= step;
    }
}

/// Generates and serializes one document, then publishes it to the NOTIFY
/// topic for the key, `<prefix><key>/NOTIFY` — the level the demo sources
/// subscribe to and recover from. A generator that returns
/// `None` (its input is not ready yet) skips this publish silently.
/// Serialization errors log a warning and skip this publish. A full or
/// disconnected inbound queue drops the message; the feed counts refusals
/// and the bus warns only on the first. There is no immediate retry.
/// Generator and serializer panics are not caught.
///
/// Public only so a `geode-app` test can publish through the real bus.
pub fn publish_one(
    feed: &ChannelFeed,
    kind: &Arc<dyn DocumentKind>,
    topic_prefix: &str,
    next: &mut NextDocument,
    key: &str,
    warned_full: &mut bool,
) {
    let Some(rows) = next(key) else {
        return;
    };
    let bytes = match kind.write(&rows) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(
                target: "geode::ingest",
                "demo bus: could not write a {} document for '{key}': {e}",
                kind.name()
            );
            return;
        }
    };
    let topic = format!("{topic_prefix}{key}/NOTIFY");
    if !feed.publish(&topic, bytes) && !*warned_full {
        *warned_full = true;
        tracing::warn!(
            target: "geode::ingest",
            "demo bus: the inbound queue is full; at least one publish was dropped"
        );
    }
}

/// The CVI producer's body: generate the underlying's next CVI document and
/// store a clone under `key` for [`chain_next`] to price off, surviving a
/// poisoned lock. Always publishes.
pub fn cvi_next(
    cvi: &mut CviGenerator,
    latest_cvi: &Mutex<HashMap<String, DocumentRows>>,
    key: &str,
) -> Option<DocumentRows> {
    let doc = cvi.next_document(key);
    latest_cvi
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.to_string(), doc.clone());
    Some(doc)
}

/// The chain producer's body: price the underlying's next expiry off the
/// latest CVI document the CVI producer stored for it, or skip (`None`)
/// when there is none yet — the chain never publishes without the curve
/// it is meant to sit near.
pub fn chain_next(
    latest_cvi: &Mutex<HashMap<String, DocumentRows>>,
    chain: &mut ChainGenerator,
    key: &str,
) -> Option<DocumentRows> {
    let cvi = latest_cvi
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
        .cloned()?;
    Some(chain.next_document(key, &cvi, chrono::Utc::now()))
}

/// The `--demo` producers, in publish order: CVI, dividend, then option
/// chain, each over `underlyings` and seeded with 42.
///
/// The chain producer prices off the CVI producer's latest document for
/// its underlying (the shared store [`cvi_next`] fills and [`chain_next`]
/// reads). The startup burst runs producers in list order, so the chain
/// must stay after the CVI or its burst finds no curve and skips (the
/// cadence recovers, but slowly). One anchor (`today`) serves both CVI
/// and chain: the chain clamps its curve date to the CVI's terms, and a
/// shared anchor keeps its expiries inside them.
pub fn demo_producers(underlyings: Vec<String>, today: NaiveDate) -> Vec<Producer> {
    let mut cvi_generator = CviGenerator::new(42, underlyings.clone(), today);
    let mut dividend_generator = DividendGenerator::new(42, underlyings.clone(), today);
    let mut chain_generator = ChainGenerator::new(42, underlyings.clone(), today);
    let latest_cvi: Arc<Mutex<HashMap<String, DocumentRows>>> = Arc::default();
    vec![
        Producer {
            kind: Arc::new(geode_documents::CviKind),
            topic_prefix: "marketdata/cvi/",
            keys: underlyings.clone(),
            startup_repeats: 1,
            next: Box::new({
                let latest_cvi = Arc::clone(&latest_cvi);
                move |key| cvi_next(&mut cvi_generator, &latest_cvi, key)
            }),
        },
        Producer {
            kind: Arc::new(geode_documents::DividendKind),
            topic_prefix: "marketdata/dividend/",
            keys: underlyings.clone(),
            startup_repeats: 1,
            next: Box::new(move |key| Some(dividend_generator.next_document(key))),
        },
        Producer {
            kind: Arc::new(geode_documents::OptionChainKind),
            topic_prefix: "marketdata/chain/",
            keys: underlyings,
            startup_repeats: geode_demo_data::documents::chain::EXPIRIES,
            next: Box::new(move |key| chain_next(&latest_cvi, &mut chain_generator, key)),
        },
    ]
}

/// The one flat schedule the cadence loop replays forever: every
/// producer's key list read at the same row in lock step, producer
/// order within a row (a "zip-longest") — `[(p0,k0), (p1,k0), (p0,k1),
/// (p1,k1), (p0,k2), …]` for a three-key and a two-key producer, so a
/// shorter producer simply drops out of the later rows rather than
/// padding or repeating early. Each entry names a producer by index
/// (into the same `producers` slice the caller holds) rather than
/// cloning its `kind`/`next`, since those are exactly what the loop
/// needs mutable access to per publish.
fn round_robin_schedule(producers: &[Producer]) -> Vec<(usize, String)> {
    let rows = producers.iter().map(|p| p.keys.len()).max().unwrap_or(0);
    let mut schedule = Vec::new();
    for row in 0..rows {
        for (idx, producer) in producers.iter().enumerate() {
            if let Some(key) = producer.keys.get(row) {
                schedule.push((idx, key.clone()));
            }
        }
    }
    schedule
}

/// The bus thread's whole life. Runs until `stop` is set.
fn run(
    feed: ChannelFeed,
    mut producers: Vec<Producer>,
    cadence: Duration,
    jitter: Duration,
    seed: u64,
    stop: &AtomicBool,
) {
    let mut warned_full = false;

    // Attempt every key `startup_repeats` times before the first cadence
    // wait. Parsing and store publication happen asynchronously after feed
    // admission.
    for producer in producers.iter_mut() {
        let keys = producer.keys.clone();
        for key in &keys {
            for _ in 0..producer.startup_repeats.max(1) {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                publish_one(
                    &feed,
                    &producer.kind,
                    producer.topic_prefix,
                    producer.next.as_mut(),
                    key,
                    &mut warned_full,
                );
            }
        }
    }

    let schedule = round_robin_schedule(&producers);
    if schedule.is_empty() {
        // No producer has a single key: nothing will ever publish again.
        // Wait out `stop` rather than spinning the cadence loop below on
        // an empty schedule, which would otherwise busy-loop with no
        // publish and no sleep in between.
        while !sleep_checking_stop(Duration::from_secs(3600), stop) {}
        return;
    }

    let mut rng = StdRng::seed_from_u64(seed);
    let jitter = jitter.min(cadence);
    let span_ms = (jitter.as_millis() as u64).saturating_mul(2);
    loop {
        for (idx, key) in &schedule {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let offset_ms = if span_ms == 0 {
                0
            } else {
                rng.random_range(0..=span_ms)
            };
            let wait = (cadence - jitter) + Duration::from_millis(offset_ms);
            if sleep_checking_stop(wait, stop) {
                return;
            }
            let producer = &mut producers[*idx];
            publish_one(
                &feed,
                &producer.kind,
                producer.topic_prefix,
                producer.next.as_mut(),
                key,
                &mut warned_full,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_data::adapter::{Adapter, ChannelAdapter, MessageSink};
    use geode_documents::{CviKind, DividendKind};
    use std::collections::HashSet;
    use std::time::Instant;

    fn cvi_producer(underlyings: Vec<String>, anchor: NaiveDate) -> Producer {
        let mut generator = CviGenerator::new(42, underlyings.clone(), anchor);
        Producer {
            kind: Arc::new(CviKind),
            topic_prefix: "marketdata/cvi/",
            keys: underlyings,
            startup_repeats: 1,
            next: Box::new(move |key| Some(generator.next_document(key))),
        }
    }

    fn dividend_producer(underlyings: Vec<String>, today: NaiveDate) -> Producer {
        let mut generator = DividendGenerator::new(43, underlyings.clone(), today);
        Producer {
            kind: Arc::new(DividendKind),
            topic_prefix: "marketdata/dividend/",
            keys: underlyings,
            startup_repeats: 1,
            next: Box::new(move |key| Some(generator.next_document(key))),
        }
    }

    /// The production list must keep CVI ahead of chain: the chain prices
    /// off the latest CVI its producer stored, and the startup burst runs
    /// producers in list order, so a chain placed first finds no curve and
    /// skips its whole burst.
    #[test]
    fn the_demo_producers_run_cvi_then_dividend_then_chain() {
        use geode_demo_data::documents::chain::EXPIRIES;
        let underlyings = vec!["SPX".to_string(), "NDX".to_string()];
        let today = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let producers = demo_producers(underlyings.clone(), today);
        assert_eq!(
            producers.iter().map(|p| p.topic_prefix).collect::<Vec<_>>(),
            vec![
                "marketdata/cvi/",
                "marketdata/dividend/",
                "marketdata/chain/"
            ]
        );
        assert_eq!(
            producers
                .iter()
                .map(|p| p.startup_repeats)
                .collect::<Vec<_>>(),
            vec![1, 1, EXPIRIES]
        );
        assert!(producers.iter().all(|p| p.keys == underlyings));
    }

    #[test]
    fn the_startup_burst_publishes_every_expiry_of_every_chain() {
        use geode_demo_data::documents::chain::EXPIRIES;
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, rx) = MessageSink::bounded(256);
        let mut sub = adapter.subscription().expect("channel adapters subscribe");
        sub.subscribe(
            &["marketdata/chain/*/NOTIFY".to_string()],
            sink,
            Arc::new(|_| {}),
        )
        .unwrap();
        let underlyings = vec!["SPX".to_string(), "NDX".to_string()];
        let anchor = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let _bus = spawn(
            feed,
            demo_producers(underlyings, anchor),
            Duration::from_secs(3600),
            Duration::ZERO,
            42,
        );
        let mut keys = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while keys.len() < 2 * EXPIRIES && Instant::now() < deadline {
            if let Ok(m) = rx.recv_timeout(Duration::from_millis(200)) {
                let parsed = geode_documents::OptionChainKind
                    .parse(&m.bytes)
                    .expect("a well-formed option chain document");
                keys.insert(parsed.rows.key);
            }
        }
        assert_eq!(
            keys.len(),
            2 * EXPIRIES,
            "every (underlying, expiry) at startup"
        );
    }

    #[test]
    fn the_cvi_producer_stores_what_it_publishes() {
        use std::collections::HashMap;
        use std::sync::Mutex;
        let anchor = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let latest: Mutex<HashMap<String, DocumentRows>> = Mutex::default();
        let mut cvi = CviGenerator::new(42, vec!["SPX".to_string()], anchor);
        let published =
            cvi_next(&mut cvi, &latest, "SPX").expect("the CVI producer always publishes");
        let store = latest.lock().unwrap();
        assert_eq!(store.len(), 1, "only the published key is stored");
        assert_eq!(
            store.get("SPX"),
            Some(&published),
            "the store holds exactly what was published"
        );
    }

    #[test]
    fn the_chain_producer_skips_an_underlying_with_no_cvi_yet() {
        use geode_demo_data::documents::chain::ChainGenerator;
        use std::collections::HashMap;
        use std::sync::Mutex;
        let anchor = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let latest: Mutex<HashMap<String, DocumentRows>> = Mutex::default();
        let mut chain = ChainGenerator::new(42, vec!["SPX".to_string()], anchor);
        assert!(
            chain_next(&latest, &mut chain, "SPX").is_none(),
            "no CVI yet: skip, don't panic"
        );
        let cvi = CviGenerator::new(42, vec!["SPX".to_string()], anchor).next_document("SPX");
        latest.lock().unwrap().insert("SPX".to_string(), cvi);
        assert!(
            chain_next(&latest, &mut chain, "SPX").is_some(),
            "with a CVI it publishes"
        );
    }

    /// Two producers (three CVI keys, two dividend keys) — the burst
    /// covers all five, producer order then key order, and the cadence
    /// loop's first two publishes are one of each prefix (the round-robin
    /// schedule's first row).
    #[test]
    fn the_bus_publishes_every_key_once_at_start_then_on_its_cadence() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, rx) = MessageSink::bounded(64);
        let mut sub = adapter.subscription().expect("channel adapters subscribe");
        sub.subscribe(
            &[
                "marketdata/cvi/*/NOTIFY".to_string(),
                "marketdata/dividend/*/NOTIFY".to_string(),
            ],
            sink,
            Arc::new(|_state| {}),
        )
        .expect("subscribing to an open channel bus succeeds");

        let cvi_underlyings = vec!["SPX".to_string(), "NDX".to_string(), "RUT".to_string()];
        let dividend_underlyings = vec!["SPX".to_string(), "AAPL".to_string()];
        let anchor = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let producers = vec![
            cvi_producer(cvi_underlyings.clone(), anchor),
            dividend_producer(dividend_underlyings.clone(), anchor),
        ];
        // Deliberately large next to the burst deadline below: a build
        // that skipped the immediate burst and only ever published on
        // its cadence could not satisfy that deadline by chance.
        let cadence = Duration::from_millis(400);
        let jitter = Duration::from_millis(50);
        let mut bus = spawn(feed, producers, cadence, jitter, 42);

        // The startup burst must cover every key within less than one cadence
        // wait, so cadence-only publication cannot satisfy this deadline.
        let burst_deadline = Instant::now() + Duration::from_millis(200);
        let mut cvi_burst: HashSet<String> = HashSet::new();
        let mut dividend_burst: HashSet<String> = HashSet::new();
        while (cvi_burst.len() < cvi_underlyings.len()
            || dividend_burst.len() < dividend_underlyings.len())
            && Instant::now() < burst_deadline
        {
            if let Ok(m) = rx.recv_timeout(Duration::from_millis(50)) {
                if let Some(key) = m
                    .topic
                    .strip_prefix("marketdata/cvi/")
                    .and_then(|k| k.strip_suffix("/NOTIFY"))
                {
                    assert!(
                        cvi_underlyings.contains(&key.to_string()),
                        "topic names an underlying the CVI generator produces"
                    );
                    CviKind.parse(&m.bytes).expect("a well-formed CVI document");
                    cvi_burst.insert(key.to_string());
                } else if let Some(key) = m
                    .topic
                    .strip_prefix("marketdata/dividend/")
                    .and_then(|k| k.strip_suffix("/NOTIFY"))
                {
                    assert!(
                        dividend_underlyings.contains(&key.to_string()),
                        "topic names an underlying the dividend generator produces"
                    );
                    DividendKind
                        .parse(&m.bytes)
                        .expect("a well-formed dividend document");
                    dividend_burst.insert(key.to_string());
                } else {
                    panic!("unexpected topic {:?}", m.topic);
                }
            }
        }
        assert_eq!(
            cvi_burst,
            cvi_underlyings.iter().cloned().collect::<HashSet<_>>(),
            "every CVI key must publish once immediately at start"
        );
        assert_eq!(
            dividend_burst,
            dividend_underlyings.iter().cloned().collect::<HashSet<_>>(),
            "every dividend key must publish once immediately at start"
        );

        // The first two cadence publishes must cover both producers: the
        // schedule begins with `(cvi, SPX)` then `(dividend, SPX)`.
        let mut prefixes = HashSet::new();
        for _ in 0..2 {
            let m = rx
                .recv_timeout(cadence + jitter + Duration::from_millis(500))
                .expect("the bus keeps publishing on its cadence after the initial burst");
            if m.topic.starts_with("marketdata/cvi/") {
                prefixes.insert("cvi");
            } else if m.topic.starts_with("marketdata/dividend/") {
                prefixes.insert("dividend");
            } else {
                panic!("unexpected topic {:?}", m.topic);
            }
        }
        assert_eq!(
            prefixes,
            HashSet::from(["cvi", "dividend"]),
            "the first two cadence publishes round-robin across both producers"
        );

        bus.stop();
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "nothing arrives once the bus has been stopped"
        );
    }

    /// The generator and document parser must use the same dividend statuses.
    /// They live in independent crates; the composition root can verify both.
    #[test]
    fn the_demo_generators_status_vocabulary_matches_the_dividend_kind() {
        assert_eq!(
            geode_demo_data::documents::dividend::STATUSES,
            geode_documents::dividend::STATUSES
        );
    }
}
