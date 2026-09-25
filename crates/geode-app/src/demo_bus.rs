//! The demo bus (market-data-documents plan, Task 10; extended to
//! several producers by the dividend-schedule plan, Task 11): the
//! background thread `--demo` mode uses for the market-data path. Each
//! [`Producer`] wraps one document generator (CVI, dividend schedules)
//! and publishes onto a `ChannelAdapter`'s feed through exactly the wire
//! format a subscribed source's own receiver thread parses (spec §9.4)
//! — so a demo panel exercises the real subscribed-source path with no
//! broker anywhere. Registered and spawned only in demo mode
//! (`main.rs`); never built outside it.

use geode_core::document::{DocumentKind, DocumentRows};
use geode_data::adapter::ChannelFeed;
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

/// One document source the bus feeds: a kind (which also parses the
/// wire bytes on the subscribed source's receiver thread), the topic
/// prefix its keys publish under (e.g. `"marketdata/cvi/"`, trailing
/// slash included so a topic is just `format!("{prefix}{key}")`), the
/// keys it produces documents for, and the stateful closure that builds
/// the next document for a given key — a generator's own `next_document`
/// moved in, since the generator itself is `FnMut`-shaped (it mutates
/// its held schedules on every call) and the bus must not know which
/// concrete generator type is behind it (design spec §6.4).
pub struct Producer {
    pub kind: Arc<dyn DocumentKind>,
    pub topic_prefix: &'static str,
    pub keys: Vec<String>,
    pub next: Box<dyn FnMut(&str) -> DocumentRows + Send>,
}

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

/// Spawns the demo bus thread (named `geode-demo-bus`) over `producers`.
///
/// On start it publishes every producer's every key once, immediately,
/// in producer order then key order, so a panel opened at startup has
/// something to paint on its very first frame (Task 10 brief) whichever
/// document kind it shows. It then loops forever over one flat,
/// round-robin schedule built once from every producer's key list (see
/// [`round_robin_schedule`]) — one publish per `cadence` plus or minus a
/// seeded `jitter` (`cadence - jitter ..= cadence + jitter`, clamped so
/// a `jitter` larger than `cadence` still waits a non-negative time) —
/// so the overall generation rate stays about one per `cadence`
/// regardless of how many producers are registered (design spec §6.4:
/// adding a second source must not double the archive growth rate).
/// `stop` is checked between every publish and while waiting.
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

/// One publish: generate the next document for `key` through `next`,
/// write it via `kind`, and put it on the bus under
/// `format!("{topic_prefix}{key}")`. Never `unwrap`s — a malformed
/// document (a write refusal the generator itself should never produce,
/// but a future generator bug or a `DocumentKind` swap might) must not
/// take the whole demo bus thread down with it, so a write failure is
/// logged at `warn` under `geode::ingest` and this publish is skipped
/// rather than panicking. A refused send (the inbound queue is full) is
/// counted and logged once, not spun on — the caller carries on to the
/// next key on its own cadence rather than retrying immediately.
fn publish_one(
    feed: &ChannelFeed,
    kind: &Arc<dyn DocumentKind>,
    topic_prefix: &str,
    next: &mut (dyn FnMut(&str) -> DocumentRows + Send),
    key: &str,
    warned_full: &mut bool,
) {
    let rows = next(key);
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
    let topic = format!("{topic_prefix}{key}");
    if !feed.publish(&topic, bytes) && !*warned_full {
        *warned_full = true;
        tracing::warn!(
            target: "geode::ingest",
            "demo bus: the inbound queue is full; at least one publish was dropped"
        );
    }
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

    // Every producer's every key once, immediately, producer order then
    // key order: the first thing a freshly opened panel of any kind
    // sees.
    for producer in producers.iter_mut() {
        let keys = producer.keys.clone();
        for key in &keys {
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
    use chrono::NaiveDate;
    use geode_data::adapter::{Adapter, ChannelAdapter, MessageSink};
    use geode_demo_data::documents::cvi::CviGenerator;
    use geode_demo_data::documents::dividend::DividendGenerator;
    use geode_documents::{CviKind, DividendKind};
    use std::collections::HashSet;
    use std::time::Instant;

    fn cvi_producer(underlyings: Vec<String>, anchor: NaiveDate) -> Producer {
        let mut generator = CviGenerator::new(42, underlyings.clone(), anchor);
        Producer {
            kind: Arc::new(CviKind),
            topic_prefix: "marketdata/cvi/",
            keys: underlyings,
            next: Box::new(move |key| generator.next_document(key)),
        }
    }

    fn dividend_producer(underlyings: Vec<String>, today: NaiveDate) -> Producer {
        let mut generator = DividendGenerator::new(43, underlyings.clone(), today);
        Producer {
            kind: Arc::new(DividendKind),
            topic_prefix: "marketdata/dividend/",
            keys: underlyings,
            next: Box::new(move |key| generator.next_document(key)),
        }
    }

    /// Extended for Task 11: two producers (three CVI keys, two dividend
    /// keys) — the burst covers all five, producer order then key order,
    /// and the cadence loop's first two publishes are one of each prefix
    /// (the round-robin schedule's first row).
    #[test]
    fn the_bus_publishes_every_key_once_at_start_then_on_its_cadence() {
        let (adapter, feed) = ChannelAdapter::new("demo_bus");
        let (sink, rx) = MessageSink::bounded(64);
        let mut sub = adapter.subscription().expect("channel adapters subscribe");
        sub.subscribe(
            &[
                "marketdata/cvi/>".to_string(),
                "marketdata/dividend/>".to_string(),
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

        // Phase 1: every key of every producer published once,
        // immediately — well inside one cadence-minus-jitter window, so
        // this can only pass if the burst really runs before the first
        // cadence wait.
        let burst_deadline = Instant::now() + Duration::from_millis(200);
        let mut cvi_burst: HashSet<String> = HashSet::new();
        let mut dividend_burst: HashSet<String> = HashSet::new();
        while (cvi_burst.len() < cvi_underlyings.len()
            || dividend_burst.len() < dividend_underlyings.len())
            && Instant::now() < burst_deadline
        {
            if let Ok(m) = rx.recv_timeout(Duration::from_millis(50)) {
                if let Some(key) = m.topic.strip_prefix("marketdata/cvi/") {
                    assert!(
                        cvi_underlyings.contains(&key.to_string()),
                        "topic names an underlying the CVI generator produces"
                    );
                    CviKind.parse(&m.bytes).expect("a well-formed CVI document");
                    cvi_burst.insert(key.to_string());
                } else if let Some(key) = m.topic.strip_prefix("marketdata/dividend/") {
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

        // Phase 2: the cadence loop round-robins across producers — the
        // first two publishes after the burst are one of each prefix
        // (the schedule's first row: `(cvi, SPX)` then `(dividend,
        // SPX)`).
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

    /// Task 11: the demo generator's own status vocabulary must agree
    /// with `geode_documents::dividend::STATUSES` — `geode-demo-data`
    /// cannot depend on `geode-documents` (workspace layering), so it
    /// keeps its own copy, and this is the one place both are visible at
    /// once to prove they have not drifted.
    #[test]
    fn the_demo_generators_status_vocabulary_matches_the_dividend_kind() {
        assert_eq!(
            geode_demo_data::documents::dividend::STATUSES,
            geode_documents::dividend::STATUSES
        );
    }

    /// Task 12: the `DIVIDEND` panel spec's own copy — `geode-marketdata`
    /// cannot depend on `geode-documents` either — must agree with
    /// `geode_documents::dividend::STATUSES` the same way the generator's
    /// copy above does; `geode-app` is the one crate where all three are
    /// visible at once.
    #[test]
    fn the_dividend_panel_specs_status_vocabulary_matches_the_dividend_kind() {
        assert_eq!(
            geode_marketdata::core::STATUSES,
            geode_documents::dividend::STATUSES
        );
    }

    /// Task 11 (the market-data egress plan's end-to-end check): a headless
    /// upload-then-echo loop through the real `DataService` built from the
    /// demo config — no internal mutator stands in for any hop. The path
    /// exercised: `service.upload` resolves the `[sophis]` egress target
    /// and hands written bytes to `ChannelEgress`; that publish lands on
    /// `marketdata/dividend/XYZ`, which the `[dividend]` source's own
    /// subscription (topics `marketdata/dividend/>`) receives and parses
    /// exactly as a real broker source would; the resulting publish is
    /// read back through an ordinary document request. Every wait is
    /// bounded so a broken hop fails the test rather than hanging it.
    #[test]
    fn an_uploaded_dividend_document_echoes_through_the_real_data_service() {
        use geode_core::config::{Config, ConfigSources};
        use geode_core::document::{Column, DocumentRows, Value};
        use geode_core::query::{DocumentParams, QueryKey};
        use geode_data::adapter::AdapterRegistry;
        use geode_data::egress::UploadParams;
        use geode_data::query::as_of::AsOf;
        use geode_data::{DataEvent, DataService, PricerRegistry};

        // An empty directory rather than a nonexistent path: the demo
        // layer's own `[demo]` csv_dir source polls it, and this test has
        // no interest in that source's health, only that `DataService::open`
        // does not fail to construct over it.
        let src_dir = tempfile::tempdir().unwrap();
        let config = Config::load(&ConfigSources {
            builtin: crate::demo::layer(src_dir.path()),
            ..ConfigSources::default()
        });
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);

        let mut adapters = AdapterRegistry::default();
        let (bus, _feed) = ChannelAdapter::new("demo_bus");
        adapters.register(bus);
        let mut pricers = PricerRegistry::default();
        pricers.register(Arc::new(geode_pricing::MockPricer::new()));

        let db_dir = tempfile::tempdir().unwrap();
        let setup = crate::bridge::data_setup(
            &config,
            db_dir.path().join("geode.duckdb"),
            adapters,
            pricers,
        )
        .expect("the demo layer carries both a datasets and a views document");
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        assert_eq!(setup.config.egress.len(), 1);
        assert_eq!(setup.config.egress[0].name, "sophis");

        let (service, rx) = DataService::open_channel(setup.config)
            .expect("the demo schema opens cleanly against a fresh database");

        // Two rows share an ex date (to prove ordinal minting through the
        // real pipeline, not only `mint_ids` in isolation) and a third
        // falls on a different date. The axis carries placeholder labels —
        // exactly what a fresh draft's `Inserted` rows would ("new-<n>",
        // spec §10 amendment 3) — since `DividendKind::write` never emits
        // an id: the assembled rows on the wire carry none, and the
        // ordinal position each id occupies is what `mint_ids` reads back.
        let ex1 = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let ex2 = NaiveDate::from_ymd_opt(2027, 1, 5).unwrap();
        let uploaded = DocumentRows {
            key: vec!["XYZ".to_string()],
            attributes: vec![
                ("currency".to_string(), Value::Utf8("USD".to_string())),
                ("schedule_date".to_string(), Value::Date(ex1)),
            ],
            axes: vec![(
                "dividend_id".to_string(),
                Column::Utf8(vec!["new-1".into(), "new-2".into(), "new-3".into()]),
            )],
            values: vec![
                ("ex_date".to_string(), Column::Date(vec![ex1, ex1, ex2])),
                (
                    "announced_date".to_string(),
                    Column::Date(vec![ex1, ex1, ex2]),
                ),
                ("pay_date".to_string(), Column::Date(vec![ex1, ex1, ex2])),
                ("amount".to_string(), Column::F64(vec![1.0, 2.0, 3.0])),
                (
                    "status".to_string(),
                    Column::Utf8(vec![
                        "declared".into(),
                        "declared".into(),
                        "estimated".into(),
                    ]),
                ),
            ],
        };

        service.upload(UploadParams {
            key: QueryKey(1),
            tag: 1,
            target: "sophis".to_string(),
            document: "dividend_schedule".to_string(),
            rows: uploaded,
        });

        let timeout = Duration::from_secs(15);
        let upload_result = loop {
            match rx.recv_timeout(timeout).expect("an upload outcome arrives") {
                DataEvent::Upload(outcome) => break outcome.result,
                _ => continue,
            }
        };
        assert_eq!(upload_result, Ok(()));

        let (dataset, batch) = loop {
            match rx.recv_timeout(timeout).expect("a publish arrives") {
                DataEvent::Published { dataset, batch, .. } if dataset == "dividend_schedule" => {
                    break (dataset, batch);
                }
                _ => continue,
            }
        };
        assert_eq!(
            (dataset.as_str(), batch.as_str()),
            ("dividend_schedule", "XYZ")
        );

        service
            .document(&DocumentParams {
                key: QueryKey(2),
                tag: 1,
                submitted: Instant::now(),
                dataset: "dividend_schedule".to_string(),
                document_key: vec!["XYZ".to_string()],
                as_of: AsOf::Live,
            })
            .expect("the document request is admitted");
        let snap = loop {
            match rx.recv_timeout(timeout).expect("a query outcome arrives") {
                DataEvent::Query(outcome) if outcome.key == QueryKey(2) => {
                    break outcome.snapshot.expect("the document reads back");
                }
                _ => continue,
            }
        };

        let expected_ids = geode_documents::dividend::mint_ids(&[ex1, ex1, ex2]);
        assert!(
            expected_ids.iter().all(|id| !id.starts_with("new-")),
            "{expected_ids:?}"
        );
        let ex_dates = [ex1, ex1, ex2];
        let amounts = [1.0, 2.0, 3.0];
        let statuses = ["declared", "declared", "estimated"];
        assert_eq!(snap.rows(), 3);
        for i in 0..3 {
            assert_eq!(
                snap.text_value("dividend_id", i),
                Some(expected_ids[i].as_str()),
                "row {i}: the id is re-minted from the ex date, not carried from the upload"
            );
            assert_eq!(
                snap.display_value("ex_date", i),
                Some(ex_dates[i].format("%Y-%m-%d").to_string())
            );
            assert_eq!(snap.f64_value("amount", i), Some(amounts[i]));
            assert_eq!(snap.text_value("status", i), Some(statuses[i]));
        }

        service.shutdown();
    }
}
