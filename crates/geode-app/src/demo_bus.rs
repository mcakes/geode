//! Background document publishing for `--demo`.
//!
//! Each [`Producer`] generates documents, serializes them through its
//! registered kind, and publishes to a channel feed. Subscribed sources
//! receive and parse those bytes through the normal ingestion path.
//! The application starts this bus only in demo mode.

use geode_core::document::{DocumentKind, DocumentRows};
use geode_data::adapter::ChannelFeed;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Interval for checking shutdown while waiting between publishes.
/// Generation, serialization, and publication run to completion before the
/// next check; this interval bounds the sleep slice, not total stop latency.
const STOP_POLL: Duration = Duration::from_millis(20);

/// A document generator and its destination topics.
///
/// `kind` serializes rows into the wire format consumed by the subscribed
/// source. Topics concatenate `topic_prefix` and the key, so a prefix such as
/// `"marketdata/cvi/"` includes its separator. `next` owns mutable generator
/// state without coupling the bus to a concrete generator type.
pub struct Producer {
    pub kind: Arc<dyn DocumentKind>,
    pub topic_prefix: &'static str,
    pub keys: Vec<String>,
    pub next: Box<dyn FnMut(&str) -> DocumentRows + Send>,
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
/// Startup attempts one publish per key in producer order then key order,
/// without waiting for a cadence. Ingestion and delivery remain asynchronous.
/// Subsequent publishes follow [`round_robin_schedule`], with one wait per
/// publish across all producers. Jitter is seeded and capped at `cadence`,
/// giving nonnegative waits from `cadence - jitter` through `cadence + jitter`
/// at millisecond jitter resolution. Adding producers lengthens the schedule
/// without multiplying its steady-state publication rate.
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

/// Generates and serializes one document, then publishes it to the topic
/// formed by concatenating the prefix and key. Serialization errors log a
/// warning and skip this publish. A full or disconnected inbound queue drops
/// the message; the feed counts refusals and the bus warns only on the first.
/// There is no immediate retry. Generator and serializer panics are not caught.
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

    // Attempt every key once before the first cadence wait. Parsing and
    // store publication happen asynchronously after feed admission.
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

    /// The dividend panel the composition root loads declares its status
    /// choices once, in its TOML; the parser and the demo generator must
    /// offer exactly those. Only the composition root sees the loaded panel
    /// and the parser together, without a feature-to-parser dependency.
    #[test]
    fn the_dividend_panel_specs_status_vocabulary_matches_the_dividend_kind() {
        let dir = tempfile::tempdir().unwrap();
        let config = geode_core::config::Config::load(&geode_core::config::ConfigSources {
            builtin: crate::builtin_layer(Some(dir.path())),
            ..Default::default()
        });
        let setup = crate::bridge::data_setup(
            &config,
            dir.path().join("geode.duckdb"),
            geode_data::adapter::AdapterRegistry::default(),
            geode_data::PricerRegistry::default(),
            geode_data::VolModelRegistry::default(),
        )
        .expect("the demo layer declares datasets and views");
        let dividend = setup
            .panels
            .iter()
            .find(|p| p.kind == "dividend")
            .expect("the builtin dividend panel is accepted");
        let choices = dividend
            .value_column("status")
            .and_then(|c| c.choices.as_deref())
            .expect("status declares its choices");
        assert_eq!(choices, geode_documents::dividend::STATUSES);
        assert_eq!(choices, geode_demo_data::documents::dividend::STATUSES);
    }

    /// Upload bytes must reach the subscribed source and return through an
    /// ordinary document query. The demo `sophis` target routes the upload to
    /// `marketdata/dividend/XYZ`; the dividend subscription parses and stores it.
    /// Each event receive has a timeout so a silent pipeline fails the test.
    #[test]
    fn an_uploaded_dividend_document_echoes_through_the_real_data_service() {
        use geode_core::config::{Config, ConfigSources};
        use geode_core::document::{Column, DocumentRows, Value};
        use geode_core::query::{DocumentParams, QueryKey};
        use geode_data::adapter::AdapterRegistry;
        use geode_data::egress::UploadParams;
        use geode_data::query::as_of::AsOf;
        use geode_data::{DataEvent, DataService, PricerRegistry, VolModelRegistry};

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
        let mut vol_models = VolModelRegistry::default();
        vol_models.register(Arc::new(geode_pricing::DemoVolModel));

        let db_dir = tempfile::tempdir().unwrap();
        let setup = crate::bridge::data_setup(
            &config,
            db_dir.path().join("geode.duckdb"),
            adapters,
            pricers,
            vol_models,
        )
        .expect("the demo layer carries both a datasets and a views document");
        assert!(setup.diagnostics.is_empty(), "{:?}", setup.diagnostics);
        assert_eq!(setup.config.egress.len(), 1);
        assert_eq!(setup.config.egress[0].name, "sophis");

        let (service, rx) = DataService::open_channel(setup.config)
            .expect("the demo schema opens cleanly against a fresh database");

        // Two rows share an ex date to exercise ordinal ID minting through
        // the full pipeline. Placeholder draft labels are omitted by the
        // writer; the parser assigns IDs from ex dates and row order.
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
    /// The store sorts document rows by their axes while uploads use painted
    /// order. An inserted dividend with a later ex date can move on readback;
    /// the panel's echo comparison must still confirm the same contents.
    #[test]
    fn an_out_of_order_insert_echoes_back_as_confirmed_through_the_real_store() {
        use geode_core::config::{Config, ConfigSources};
        use geode_core::document::{Column, DocumentRows, Value};
        use geode_core::query::{DocumentParams, QueryKey};
        use geode_core::snapshot::Snapshot;
        use geode_data::adapter::AdapterRegistry;
        use geode_data::egress::UploadParams;
        use geode_data::query::as_of::AsOf;
        use geode_data::{DataEvent, DataService, PricerRegistry, VolModelRegistry};
        use geode_marketdata::core::upload::{assemble, echo_differs};
        use geode_marketdata::core::{Draft, MatrixModel, builtin_panel};
        use std::sync::mpsc::Receiver;

        let dividend = builtin_panel("dividend");
        let src_dir = tempfile::tempdir().unwrap();
        let config = Config::load(&ConfigSources {
            builtin: crate::demo::layer(src_dir.path()),
            ..ConfigSources::default()
        });
        let mut adapters = AdapterRegistry::default();
        let (bus, _feed) = ChannelAdapter::new("demo_bus");
        adapters.register(bus);
        let mut pricers = PricerRegistry::default();
        pricers.register(Arc::new(geode_pricing::MockPricer::new()));
        let mut vol_models = VolModelRegistry::default();
        vol_models.register(Arc::new(geode_pricing::DemoVolModel));
        let db_dir = tempfile::tempdir().unwrap();
        let setup = crate::bridge::data_setup(
            &config,
            db_dir.path().join("geode.duckdb"),
            adapters,
            pricers,
            vol_models,
        )
        .expect("the demo layer opens");
        let (service, rx) = DataService::open_channel(setup.config).expect("the store opens");

        let timeout = Duration::from_secs(15);
        // Upload, wait for its `Ok` and the publish it echoes as, then
        // read the document back through an ordinary document request.
        let round_trip = |service: &DataService,
                          rx: &Receiver<DataEvent>,
                          tag: u64,
                          rows: DocumentRows|
         -> Arc<Snapshot> {
            service.upload(UploadParams {
                key: QueryKey(1),
                tag,
                target: "sophis".to_string(),
                document: "dividend_schedule".to_string(),
                rows,
            });
            let (mut ok, mut published) = (None, false);
            while ok.is_none() || !published {
                match rx.recv_timeout(timeout).expect("an event arrives") {
                    DataEvent::Upload(o) if o.tag == tag => ok = Some(o.result),
                    DataEvent::Published { dataset, .. } if dataset == "dividend_schedule" => {
                        published = true
                    }
                    _ => {}
                }
            }
            assert_eq!(ok, Some(Ok(())));
            service
                .document(&DocumentParams {
                    key: QueryKey(2),
                    tag,
                    submitted: Instant::now(),
                    dataset: "dividend_schedule".to_string(),
                    document_key: vec!["XYZ".to_string()],
                    as_of: AsOf::Live,
                })
                .expect("the document request is admitted");
            loop {
                match rx.recv_timeout(timeout).expect("a query outcome arrives") {
                    DataEvent::Query(o) if o.key == QueryKey(2) && o.tag == tag => {
                        break o.snapshot.expect("the document reads back");
                    }
                    _ => continue,
                }
            }
        };

        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        let exes = vec![d(10, 1), d(11, 2), d(12, 3)];
        let first = DocumentRows {
            key: vec!["XYZ".to_string()],
            attributes: vec![
                ("currency".to_string(), Value::Utf8("USD".to_string())),
                ("schedule_date".to_string(), Value::Date(d(9, 1))),
            ],
            axes: vec![(
                "dividend_id".to_string(),
                Column::Utf8(vec!["new-1".into(), "new-2".into(), "new-3".into()]),
            )],
            values: vec![
                ("ex_date".to_string(), Column::Date(exes.clone())),
                ("announced_date".to_string(), Column::Date(exes.clone())),
                ("pay_date".to_string(), Column::Date(exes.clone())),
                ("amount".to_string(), Column::F64(vec![1.0, 2.0, 3.0])),
                (
                    "status".to_string(),
                    Column::Utf8(vec!["declared".into(); 3]),
                ),
            ],
        };
        let base = round_trip(&service, &rx, 1, first);
        assert_eq!(base.rows(), 3);

        // The panel's own route: a clean model of the base, an inserted
        // row under the FIRST document row carrying the LATEST ex date,
        // then the painted model and the assembled upload.
        let clean = MatrixModel::build(&base, &dividend, &Draft::default()).unwrap();
        let first_label = clean.rows[0].label.to_string();
        let mut draft = Draft::default();
        let label = draft.mint_label(|l| clean.rows.iter().any(|r| r.label.as_ref() == l));
        // Stamp the insert against the delivered generation the clean model
        // was built from, exactly as the panel does.
        let stamp = clean.base.clone().unwrap_or_default();
        draft.insert_row(label.clone(), Some(first_label), &stamp);
        let late = NaiveDate::from_ymd_opt(2027, 3, 19).unwrap();
        for (column, value) in [
            ("ex", Value::Date(late)),
            ("announced", Value::Date(late)),
            ("pay", Value::Date(late)),
            ("amount", Value::F64(0.75)),
            ("status", Value::Utf8("estimated".into())),
        ] {
            assert!(draft.set_row_cell(&label, column, value), "{column}");
        }
        let painted = MatrixModel::build(&base, &dividend, &draft).unwrap();
        let sent = assemble(&base, &dividend, &painted, &draft).expect("assembles");
        let Column::Date(sent_ex) = &sent.values[0].1 else {
            panic!("ex_date is a date column");
        };
        assert_eq!(
            sent_ex[1], late,
            "the insert sits second, out of date order"
        );

        let echoed = round_trip(&service, &rx, 2, sent.clone());
        let clean = MatrixModel::build(&echoed, &dividend, &Draft::default()).unwrap();
        let delivered =
            assemble(&echoed, &dividend, &clean, &Draft::default()).expect("the echo assembles");
        let Column::Date(echo_ex) = &delivered.values[0].1 else {
            panic!("ex_date is a date column");
        };
        assert_ne!(
            sent_ex, echo_ex,
            "the store reorders the rows — otherwise this test proves nothing"
        );
        assert_eq!(echo_differs(&dividend, &sent, &delivered), 0);

        service.shutdown();
    }
}
