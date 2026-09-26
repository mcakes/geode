//! One pricing thread with its own queue, independent of the DuckDB read pool.
//!
//! At most [`PRICE_BOUND`] distinct keys can wait. A newer queued batch replaces
//! that key's pending batch in place. Submitting while the same key runs queues
//! another batch; it does not cancel the running one.
//!
//! Cancellation removes queued work and stops a running batch at the next line
//! boundary, returning any lines already processed. Overrides are set once per
//! batch; failure makes each processed line an error. Individual pricing panics
//! become line errors and leave the worker available. An unavailable pricer
//! returns the configured missing-pricer reason for each processed line.
//!
//! A refused sink delivery logs once and does not stop the worker. Shutdown
//! cancels running work at a line boundary and drops queued work; it joins the
//! thread, so a blocked pricer can delay shutdown.

use super::PricerConfig;
use geode_core::pricing::{PriceOutcome, PriceParams};
use geode_core::query::QueryKey;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

pub type PriceSink = Arc<dyn Fn(PriceOutcome) -> bool + Send + Sync>;

/// Distinct keys that may wait; one batch per key.
pub const PRICE_BOUND: usize = 64;

#[derive(Default)]
struct Queue {
    order: VecDeque<QueryKey>,
    pending: HashMap<QueryKey, PriceParams>,
    running: Option<QueryKey>,
    cancel_running: bool,
    shutdown: bool,
}

pub struct PricingWorker {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl PricingWorker {
    pub fn spawn(config: PricerConfig, sink: PriceSink) -> PricingWorker {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let thread = {
            let queue = Arc::clone(&queue);
            std::thread::Builder::new()
                .name("geode-pricing".into())
                .spawn(move || run(queue, config, sink))
                .expect("spawn the pricing worker")
        };
        PricingWorker {
            queue,
            thread: Mutex::new(Some(thread)),
        }
    }

    pub fn request(&self, params: PriceParams) -> bool {
        let (lock, cvar) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.shutdown {
            return false;
        }
        let key = params.key;
        if let Some(slot) = q.pending.get_mut(&key) {
            *slot = params;
        } else {
            if q.order.len() >= PRICE_BOUND {
                return false;
            }
            q.order.push_back(key);
            q.pending.insert(key, params);
        }
        cvar.notify_all();
        true
    }

    pub fn cancel(&self, key: QueryKey) {
        let (lock, _) = &*self.queue;
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        if q.pending.remove(&key).is_some() {
            q.order.retain(|k| *k != key);
        }
        if q.running == Some(key) {
            q.cancel_running = true;
        }
    }

    pub fn shutdown(&self) {
        {
            let (lock, cvar) = &*self.queue;
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.shutdown = true;
            q.cancel_running = true;
            cvar.notify_all();
        }
        if let Some(t) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = t.join();
        }
    }
}

impl Drop for PricingWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The one place a `catch_unwind` payload becomes a message, shared by
/// both boundary sites in `run` (`set_overrides` and `price`).
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

fn run(queue: Arc<(Mutex<Queue>, Condvar)>, config: PricerConfig, sink: PriceSink) {
    let (lock, cvar) = &*queue;
    let mut refusal_logged = false;
    loop {
        let params = {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(key) = q.order.pop_front()
                    && let Some(p) = q.pending.remove(&key)
                {
                    q.running = Some(key);
                    q.cancel_running = false;
                    break p;
                }
                q = cvar.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let started = std::time::Instant::now();
        let mut results = Vec::with_capacity(params.lines.len());
        let mut failures = 0usize;
        // Apply overrides once per batch. If refused or panicking, price no lines
        // against the wrong inputs; report the failure for each processed line.
        let overrides_failed: Option<String> = match &config.pricer {
            None => None,
            Some(pricer) => {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    geode_core::panic::contained(|| pricer.set_overrides(&params.overrides))
                }));
                match outcome {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => {
                        tracing::warn!(
                            target: "geode::pricing",
                            "overrides refused for key {} tag {}: {}",
                            params.key.0, params.tag, e.0
                        );
                        Some(format!("overrides refused: {}", e.0))
                    }
                    Err(payload) => {
                        let message = panic_message(&payload);
                        tracing::warn!(
                            target: "geode::pricing",
                            "set_overrides panicked for key {}: {message}",
                            params.key.0
                        );
                        Some(format!("overrides refused: pricer panicked: {message}"))
                    }
                }
            }
        };
        for line in &params.lines {
            {
                let q = lock.lock().unwrap_or_else(|e| e.into_inner());
                if q.cancel_running || q.shutdown {
                    break;
                }
            }
            if let Some(reason) = &overrides_failed {
                failures += 1;
                results.push((line.id, line.revision, Err(reason.clone())));
                continue;
            }
            let result = match &config.pricer {
                None => Err(config.missing_reason()),
                Some(pricer) => {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        geode_core::panic::contained(|| pricer.price(&line.request))
                    }));
                    match outcome {
                        Ok(Ok(r)) => Ok(r),
                        Ok(Err(e)) => Err(e.0),
                        Err(payload) => {
                            let message = panic_message(&payload);
                            tracing::warn!(
                                target: "geode::pricing",
                                "pricer panicked on line {} of key {}: {message}",
                                line.id, params.key.0
                            );
                            Err(format!("pricer panicked: {message}"))
                        }
                    }
                }
            };
            if result.is_err() {
                failures += 1;
            }
            results.push((line.id, line.revision, result));
        }
        {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            q.running = None;
            q.cancel_running = false;
        }
        tracing::debug!(
            target: "geode::pricing",
            "priced {} of {} line(s) for key {} tag {} in {:?} ({failures} failed)",
            results.len(), params.lines.len(), params.key.0, params.tag, started.elapsed()
        );
        let delivered = sink(PriceOutcome {
            key: params.key,
            tag: params.tag,
            submitted: params.submitted,
            results,
        });
        if !delivered && !refusal_logged {
            refusal_logged = true;
            tracing::warn!(
                target: "geode::pricing",
                "a price outcome for key {} was not delivered; further refusals are not logged",
                params.key.0
            );
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use geode_core::pricing::{
        Expiry, Instrument, MarketOverrides, OptionKind, PriceLine, PriceRequest, PriceResult,
        Pricer, PricingError, Shifts, Strike, Vanilla,
    };
    use std::sync::Mutex;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    /// Prices anything but "FAIL" (an error) and "BOOM" (a panic), after
    /// `delay`, and records every underlying it was asked. `set_overrides`
    /// pushes a clone into `overrides_seen`, refuses when the map contains
    /// the key `"REFUSE"`, and panics when it contains `"BOOM"` (mirroring
    /// `price`).
    pub(crate) struct FakePricer {
        pub(crate) asked: Arc<Mutex<Vec<String>>>,
        pub(crate) delay: Duration,
        pub(crate) overrides_seen: Arc<Mutex<Vec<MarketOverrides>>>,
    }
    impl Pricer for FakePricer {
        fn name(&self) -> &str {
            "fake"
        }
        fn set_overrides(&self, overrides: &MarketOverrides) -> Result<(), PricingError> {
            self.overrides_seen.lock().unwrap().push(overrides.clone());
            if overrides.spot.contains_key("BOOM") {
                panic!("the fake pricer's set_overrides exploded");
            }
            if overrides.spot.contains_key("REFUSE") {
                return Err(PricingError("refused overrides".into()));
            }
            Ok(())
        }
        fn price(&self, req: &PriceRequest) -> Result<PriceResult, PricingError> {
            self.asked
                .lock()
                .unwrap()
                .push(req.instrument.underlying().to_string());
            std::thread::sleep(self.delay);
            match req.instrument.underlying() {
                "FAIL" => Err(PricingError("refused".into())),
                "BOOM" => panic!("the fake pricer exploded"),
                _ => Ok(PriceResult {
                    price: 1.0,
                    delta: 0.5,
                    gamma: 0.0,
                    vega: 0.0,
                    theta: 0.0,
                    rho: 0.0,
                }),
            }
        }
    }

    pub(crate) fn line(id: u64, underlying: &str) -> PriceLine {
        PriceLine {
            id,
            revision: 1,
            request: PriceRequest {
                instrument: Instrument::Vanilla(Vanilla {
                    underlying: underlying.into(),
                    expiry: Expiry::Tenor("3m".into()),
                    strike: Strike::Absolute(100.0),
                    kind: OptionKind::Call,
                }),
                shifts: Shifts::default(),
            },
        }
    }

    pub(crate) fn params(key: u64, tag: u64, underlyings: &[&str]) -> PriceParams {
        params_with_overrides(key, tag, underlyings, MarketOverrides::default())
    }

    pub(crate) fn params_with_overrides(
        key: u64,
        tag: u64,
        underlyings: &[&str],
        overrides: MarketOverrides,
    ) -> PriceParams {
        PriceParams {
            key: QueryKey(key),
            tag,
            submitted: Instant::now(),
            overrides,
            lines: underlyings
                .iter()
                .enumerate()
                .map(|(i, u)| line(i as u64 + 1, u))
                .collect(),
        }
    }

    fn worker(
        delay: Duration,
    ) -> (
        PricingWorker,
        Arc<Mutex<Vec<String>>>,
        std::sync::mpsc::Receiver<PriceOutcome>,
    ) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: PriceSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = PricingWorker::spawn(
            PricerConfig::with(Arc::new(FakePricer {
                asked: asked.clone(),
                delay,
                overrides_seen: Default::default(),
            })),
            sink,
        );
        (w, asked, rx)
    }

    fn next(rx: &std::sync::mpsc::Receiver<PriceOutcome>) -> PriceOutcome {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("an outcome")
    }

    #[test]
    fn a_batch_is_priced_line_by_line_and_answered_under_its_key_and_tag() {
        let (w, _, rx) = worker(Duration::ZERO);
        let p = params(7, 3, &["SPX", "FAIL", "NDX"]);
        let submitted = p.submitted;
        assert!(w.request(p));
        let o = next(&rx);
        assert_eq!((o.key, o.tag), (QueryKey(7), 3));
        assert_eq!(o.submitted, submitted);
        assert_eq!(o.results.len(), 3);
        assert_eq!(o.results[0].0, 1);
        assert_eq!(o.results[0].1, 1);
        assert!(o.results[0].2.is_ok());
        assert_eq!(o.results[1].2.as_ref().unwrap_err(), "refused");
        assert!(o.results[2].2.is_ok());
        w.shutdown();
    }

    #[test]
    fn latest_wins_per_key_while_queued() {
        // The first batch holds the worker; the second and third for key 9
        // queue behind it and only the third runs.
        let (w, asked, rx) = worker(Duration::from_millis(50));
        assert!(w.request(params(1, 1, &["A"])));
        assert!(w.request(params(9, 1, &["OLD"])));
        assert!(w.request(params(9, 2, &["NEW"])));
        let first = next(&rx);
        assert_eq!(first.key, QueryKey(1));
        let second = next(&rx);
        assert_eq!((second.key, second.tag), (QueryKey(9), 2));
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "only two outcomes"
        );
        assert!(
            !asked.lock().unwrap().iter().any(|u| u == "OLD"),
            "{:?}",
            asked.lock().unwrap()
        );
        w.shutdown();
    }

    #[test]
    fn cancel_drops_a_queued_batch_and_stops_a_running_one_at_the_line_boundary() {
        let (w, asked, rx) = worker(Duration::from_millis(40));
        assert!(w.request(params(1, 1, &["A", "B", "C", "D", "E"])));
        assert!(w.request(params(2, 1, &["Q"])));
        std::thread::sleep(Duration::from_millis(60)); // inside line A or B of key 1
        w.cancel(QueryKey(2));
        w.cancel(QueryKey(1));
        let o = next(&rx);
        assert_eq!(o.key, QueryKey(1));
        assert!(o.results.len() < 5, "stopped early: {}", o.results.len());
        assert!(
            !o.results.is_empty(),
            "the lines already priced are delivered"
        );
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "key 2 never ran"
        );
        assert!(!asked.lock().unwrap().iter().any(|u| u == "Q"));
        w.shutdown();
    }

    #[test]
    fn a_replaced_batch_keeps_its_queue_position() {
        // Key 2's replacement (tag 2, "NEW2") must not move to the back of
        // the queue behind key 3 — it keeps key 2's original slot, so the
        // arrival order is 1, 2, 3, not 1, 3, 2.
        let (w, asked, rx) = worker(Duration::from_millis(50));
        assert!(w.request(params(1, 1, &["A"])));
        assert!(w.request(params(2, 1, &["OLD2"])));
        assert!(w.request(params(3, 1, &["C"])));
        assert!(w.request(params(2, 2, &["NEW2"])));
        let first = next(&rx);
        assert_eq!(first.key, QueryKey(1));
        let second = next(&rx);
        assert_eq!((second.key, second.tag), (QueryKey(2), 2));
        let third = next(&rx);
        assert_eq!(third.key, QueryKey(3));
        assert!(!asked.lock().unwrap().iter().any(|u| u == "OLD2"));
        w.shutdown();
    }

    /// Pins that a cancel's effect does not outlive the batch it stopped:
    /// `cancel_running` is cleared both when a batch is picked up and after
    /// it finishes, and a later, un-cancelled batch must run to completion
    /// rather than being stopped by a stale flag left over from key 1's
    /// cancel.
    #[test]
    fn a_cancel_of_a_running_key_does_not_stop_the_next_batch() {
        let (w, _, rx) = worker(Duration::from_millis(40));
        assert!(w.request(params(1, 1, &["A", "B", "C", "D", "E"])));
        std::thread::sleep(Duration::from_millis(60)); // inside line B or C of key 1
        w.cancel(QueryKey(1));
        assert!(w.request(params(2, 1, &["X", "Y", "Z"])));
        let first = next(&rx);
        assert_eq!(first.key, QueryKey(1));
        assert!(
            first.results.len() < 5,
            "stopped early: {}",
            first.results.len()
        );
        let second = next(&rx);
        assert_eq!(second.key, QueryKey(2));
        assert_eq!(
            second.results.len(),
            3,
            "key 2 was not stopped by key 1's cancel"
        );
        assert!(second.results.iter().all(|(_, _, r)| r.is_ok()));
        w.shutdown();
    }

    #[test]
    fn a_panicking_line_is_that_lines_error_and_the_next_line_prices() {
        let (w, _, rx) = worker(Duration::ZERO);
        assert!(w.request(params(3, 1, &["BOOM", "SPX"])));
        let o = next(&rx);
        assert!(
            o.results[0].2.as_ref().unwrap_err().contains("panicked"),
            "{:?}",
            o.results[0]
        );
        assert!(o.results[1].2.is_ok());
        assert!(
            w.request(params(3, 2, &["SPX"])),
            "the worker is still alive"
        );
        assert_eq!(next(&rx).tag, 2);
        w.shutdown();
    }

    #[test]
    fn no_pricer_answers_every_line_with_the_configured_name() {
        let (tx, rx) = channel();
        let sink: PriceSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = PricingWorker::spawn(PricerConfig::missing("vendor"), sink);
        assert!(w.request(params(4, 1, &["SPX", "NDX"])));
        let o = next(&rx);
        for (_, _, r) in &o.results {
            assert_eq!(
                r.as_ref().unwrap_err(),
                "pricer \"vendor\" is not built into this binary"
            );
        }
        w.shutdown();
    }

    #[test]
    fn the_queue_is_bounded_by_distinct_keys_and_a_stopped_worker_refuses() {
        let (w, _, _rx) = worker(Duration::from_millis(200));
        assert!(w.request(params(0, 1, &["A"]))); // running
        std::thread::sleep(Duration::from_millis(20));
        for k in 1..=PRICE_BOUND as u64 {
            assert!(w.request(params(k, 1, &["A"])), "key {k} fits");
        }
        assert!(
            !w.request(params(PRICE_BOUND as u64 + 1, 1, &["A"])),
            "one over the bound is refused"
        );
        assert!(
            w.request(params(1, 2, &["A"])),
            "a replacement for a queued key always fits"
        );
        w.shutdown();
        assert!(!w.request(params(99, 1, &["A"])));
    }

    #[test]
    fn overrides_are_set_once_per_batch_before_its_first_line() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: PriceSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = PricingWorker::spawn(
            PricerConfig::with(Arc::new(FakePricer {
                asked: asked.clone(),
                delay: Duration::ZERO,
                overrides_seen: seen.clone(),
            })),
            sink,
        );
        let mut o = MarketOverrides::default();
        o.spot.insert("SPX".into(), 5000.0);
        assert!(w.request(params_with_overrides(1, 1, &["SPX", "NDX"], o.clone())));
        assert!(w.request(params_with_overrides(
            2,
            1,
            &["SPX"],
            MarketOverrides::default()
        )));
        next(&rx);
        next(&rx);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "once per batch, not per line: {seen:?}");
        assert_eq!(seen[0], o);
        assert_eq!(seen[1], MarketOverrides::default());
        w.shutdown();
    }

    #[test]
    fn refused_overrides_fail_every_line_of_the_batch_and_price_none() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: PriceSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = PricingWorker::spawn(
            PricerConfig::with(Arc::new(FakePricer {
                asked: asked.clone(),
                delay: Duration::ZERO,
                overrides_seen: Default::default(),
            })),
            sink,
        );
        let mut bad = MarketOverrides::default();
        bad.spot.insert("REFUSE".into(), 1.0);
        assert!(w.request(params_with_overrides(3, 1, &["SPX", "NDX"], bad)));
        let o = next(&rx);
        assert_eq!(o.results.len(), 2);
        for (_, _, r) in &o.results {
            assert_eq!(
                r.as_ref().unwrap_err(),
                "overrides refused: refused overrides"
            );
        }
        assert!(
            asked.lock().unwrap().is_empty(),
            "no line was priced against the wrong data source"
        );
        assert!(
            w.request(params(3, 2, &["SPX"])),
            "the worker is still alive"
        );
        assert!(next(&rx).results[0].2.is_ok());
        w.shutdown();
    }

    #[test]
    fn a_panicking_set_overrides_fails_the_batch_and_the_worker_survives() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let sink: PriceSink = Arc::new(move |o| tx.send(o).is_ok());
        let w = PricingWorker::spawn(
            PricerConfig::with(Arc::new(FakePricer {
                asked: asked.clone(),
                delay: Duration::ZERO,
                overrides_seen: Default::default(),
            })),
            sink,
        );
        let mut boom = MarketOverrides::default();
        boom.spot.insert("BOOM".into(), 1.0);
        assert!(w.request(params_with_overrides(5, 1, &["SPX", "NDX"], boom)));
        let o = next(&rx);
        assert_eq!(o.results.len(), 2);
        for (_, _, r) in &o.results {
            assert!(
                r.as_ref()
                    .unwrap_err()
                    .starts_with("overrides refused: pricer panicked"),
                "{:?}",
                r
            );
        }
        assert!(
            asked.lock().unwrap().is_empty(),
            "no line was priced against the wrong data source"
        );
        assert!(
            w.request(params(5, 2, &["SPX"])),
            "the worker is still alive"
        );
        assert!(next(&rx).results[0].2.is_ok());
        w.shutdown();
    }
}
