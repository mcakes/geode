//! The demo bus (market-data-documents plan, Task 10): the one producer
//! `--demo` mode has for the market-data path. A background thread
//! generates CVI documents (`geode_demo_data::documents::cvi`) and
//! publishes them onto a `ChannelAdapter`'s feed through exactly the
//! wire format a subscribed source's own receiver thread parses (spec
//! §9.4) — so the demo panel exercises the real subscribed-source path
//! with no broker anywhere. Registered and spawned only in demo mode
//! (`main.rs`); never built outside it.

use geode_core::document::DocumentKind;
use geode_data::adapter::ChannelFeed;
use geode_demo_data::documents::cvi::CviGenerator;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the run loop re-checks `stop` while waiting out a cadence.
/// Bounds how long [`DemoBus::stop`] can take to return regardless of
/// how long `cadence` itself is — a real desk's `cadence` is measured in
/// seconds, and nothing here may block a caller for that long.
const STOP_POLL: Duration = Duration::from_millis(20);

/// A running demo bus thread.
///
/// `stop` is also `AtomicBool`-checked between every publish and while
/// waiting out a cadence, so [`Self::stop`] returns within about
/// [`STOP_POLL`] rather than at the mercy of `cadence`.
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

/// Spawns the demo bus thread (named `geode-demo-bus`).
///
/// On start it publishes every one of `generator.underlyings()` once,
/// immediately, so a panel opened at startup has something to paint on
/// its very first frame (Task 10 brief). It then loops, publishing each
/// key in turn on `cadence` plus or minus a seeded `jitter`
/// (`cadence - jitter ..= cadence + jitter`, clamped so a `jitter`
/// larger than `cadence` still waits a non-negative time), checking
/// `stop` between every publish and while waiting.
pub fn spawn(
    feed: ChannelFeed,
    kind: Arc<dyn DocumentKind>,
    mut generator: CviGenerator,
    cadence: Duration,
    jitter: Duration,
    seed: u64,
) -> DemoBus {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("geode-demo-bus".to_string())
        .spawn(move || {
            run(
                feed,
                &kind,
                &mut generator,
                cadence,
                jitter,
                seed,
                &thread_stop,
            )
        })
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

/// One publish: generate the next document for `key`, write it, and put
/// it on the bus. Never `unwrap`s — a malformed document (a write
/// refusal the generator itself should never produce, but a future
/// generator bug or a `DocumentKind` swap might) must not take the whole
/// demo bus thread down with it, so a write failure is logged at `warn`
/// under `geode::ingest` and this publish is skipped rather than
/// panicking. A refused send (the inbound queue is full) is counted and
/// logged once, not spun on — the caller carries on to the next key on
/// its own cadence rather than retrying immediately.
fn publish_one(
    feed: &ChannelFeed,
    kind: &Arc<dyn DocumentKind>,
    generator: &mut CviGenerator,
    key: &str,
    warned_full: &mut bool,
) {
    let rows = generator.next_document(key);
    let bytes = match kind.write(&rows) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(
                target: "geode::ingest",
                "demo bus: could not write a CVI document for '{key}': {e}"
            );
            return;
        }
    };
    let topic = format!("marketdata/cvi/{key}");
    if !feed.publish(&topic, bytes) && !*warned_full {
        *warned_full = true;
        tracing::warn!(
            target: "geode::ingest",
            "demo bus: the inbound queue is full; at least one publish was dropped"
        );
    }
}

/// The bus thread's whole life. Runs until `stop` is set.
fn run(
    feed: ChannelFeed,
    kind: &Arc<dyn DocumentKind>,
    generator: &mut CviGenerator,
    cadence: Duration,
    jitter: Duration,
    seed: u64,
    stop: &AtomicBool,
) {
    let underlyings = generator.underlyings().to_vec();
    let mut warned_full = false;

    // Every key once, immediately: the first thing a freshly opened
    // panel sees.
    for key in &underlyings {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        publish_one(&feed, kind, generator, key, &mut warned_full);
    }

    let mut rng = StdRng::seed_from_u64(seed);
    let jitter = jitter.min(cadence);
    let span_ms = (jitter.as_millis() as u64).saturating_mul(2);
    loop {
        for key in &underlyings {
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
            publish_one(&feed, kind, generator, key, &mut warned_full);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use geode_data::adapter::{Adapter, ChannelAdapter, MessageSink};
    use geode_documents::CviKind;
    use std::collections::HashSet;
    use std::time::Instant;

    #[test]
    fn the_bus_publishes_every_key_once_at_start_then_on_its_cadence() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, rx) = MessageSink::bounded(64);
        let mut sub = adapter.subscription().expect("channel adapters subscribe");
        sub.subscribe(
            &["marketdata/cvi/>".to_string()],
            sink,
            Arc::new(|_state| {}),
        )
        .expect("subscribing to an open channel bus succeeds");

        let underlyings = vec!["SPX".to_string(), "NDX".to_string(), "RUT".to_string()];
        let anchor = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
        let generator = CviGenerator::new(42, underlyings.clone(), anchor);
        let mut bus = spawn(
            feed,
            Arc::new(CviKind),
            generator,
            Duration::from_millis(50),
            Duration::from_millis(10),
            42,
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut received = Vec::new();
        while received.len() < underlyings.len() * 2 && Instant::now() < deadline {
            if let Ok(m) = rx.recv_timeout(Duration::from_millis(200)) {
                received.push(m);
            }
        }
        assert!(
            received.len() >= underlyings.len() * 2,
            "expected at least {} messages within 2s, got {}",
            underlyings.len() * 2,
            received.len()
        );

        // Every one of the initial burst's keys is represented, and
        // every message parses back into the key its topic names.
        let mut seen_in_first_burst: HashSet<String> = HashSet::new();
        for m in &received {
            let key = m
                .topic
                .strip_prefix("marketdata/cvi/")
                .unwrap_or_else(|| panic!("unexpected topic {:?}", m.topic))
                .to_string();
            assert!(
                underlyings.contains(&key),
                "topic names an underlying this generator produces"
            );
            let parsed = CviKind.parse(&m.bytes).expect("a well-formed CVI document");
            assert_eq!(parsed.rows.key, vec![key.clone()]);
            seen_in_first_burst.insert(key);
        }
        assert_eq!(
            seen_in_first_burst.len(),
            underlyings.len(),
            "the initial burst publishes every key once"
        );

        bus.stop();
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "nothing arrives once the bus has been stopped"
        );
    }
}
